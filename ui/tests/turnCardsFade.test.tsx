/**
 * CAD-2548: data cards wait for the answer, then fade in. The real ThreadTurn
 * is rendered statically for the streaming and history states and mounted in
 * happy-dom for the live transition.
 */
declare function require(name: string): any;
declare const process: any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame", "getSelection"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
const { ThreadTurn } = require("../src/features/home/ThreadView") as typeof import("../src/features/home/ThreadView");
const { threadItems, threadRuns } = require("../src/features/home/thread") as typeof import("../src/features/home/thread");
const fs = require("fs");
const path = require("path");

type Entry = import("../src/features/home/thread").ThreadEntry;
const at = (seq: number) => `2026-10-02T10:00:${String(seq).padStart(2, "0")}Z`;
const agent = (seq: number, kind: string, text: string, payload: Record<string, unknown> | null = null): Entry =>
  ({ seq, role: "agent", kind, text, payload, message: "m1", created: at(seq) }) as Entry;
const art = (seq: number, id: string) => agent(seq, "artifact", "", { id, version: 1, kind: "csv", title: id, bytes: 10 });
const image = agent(2, "image_job", "", { job_id: "med_7b693b44d928437491a2c13e549f6e0b", title: "An image", aspect: "1:1" });
const items = (entries: Entry[]) => threadItems({ exists: true, entries, pending: [] });
const turn = (entries: Entry[], running: boolean) =>
  renderToStaticMarkup(React.createElement(ThreadTurn, { items: items(entries), running, onOpenIssue: () => undefined }));

const cardsAndImage = [agent(1, "assistant_text", "Looking"), image, art(3, "art-a"), art(4, "art-b")];
const answered = [...cardsAndImage, agent(5, "turn_result", "Done.")];

// Streaming: the data cards are not painted; the image job card still is.
const streaming = turn(cardsAndImage, true);
assert(!streaming.includes('data-item-key="art-a@') && !streaming.includes("art-a"), "no data card while streaming");
assert(streaming.includes("turn-image"), "the image job card stays live while streaming");

// History (never streamed): cards present, no animation class.
const history = turn(answered, false);
assert(history.includes("art-a") && history.includes("art-b"), "history shows its cards");
assert(!history.includes("turn-card-in"), "history carries no animation class");

// Live: streaming then the answer commits. The cards fade in, staggered; the image card does not.
const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);
const mount = async (entries: Entry[], running: boolean) =>
  React.act(async () => { root.render(React.createElement(ThreadTurn, { items: items(entries), running, onOpenIssue: () => undefined })); });
await mount(cardsAndImage, true);
assert(!host.innerHTML.includes("art-a"), "live: no card while streaming");
await mount(answered, true);
const faded = [...host.querySelectorAll(".turn-card-in")] as HTMLElement[];
assert(faded.length === 2, `live: both data cards fade in, got ${faded.length}`);
assert(faded[0].style.getPropertyValue("--i") === "0" && faded[1].style.getPropertyValue("--i") === "1", "live: cards are staggered by index");
assert(!host.querySelector(".turn-image.turn-card-in"), "live: the image card is not faded");
assert(host.innerHTML.indexOf("Done.") < host.innerHTML.indexOf("turn-card-in"), "live: the answer paints before the cards");
await React.act(async () => root.unmount());

// CAD-2552: a run of cards with no agent text is one turn, so it takes the same
// rules: data cards wait while its turn runs, then fade in; history does not.
const cardsOnly = [art(1, "art-a"), art(2, "art-b")];
const grouped = threadRuns(items(cardsOnly));
assert(grouped.length === 1 && grouped[0].turn, "a cards-only run is grouped into a turn");
assert(!turn(cardsOnly, true).includes("art-a"), "cards-only: no data card while its turn runs");
assert(!turn(cardsOnly, false).includes("turn-card-in") && turn(cardsOnly, false).includes("art-a"), "cards-only history: cards, no animation");
const host2 = document.createElement("div");
document.body.appendChild(host2);
const root2 = createRoot(host2);
const mount2 = async (running: boolean) =>
  React.act(async () => { root2.render(React.createElement(ThreadTurn, { items: items(cardsOnly), running, onOpenIssue: () => undefined })); });
await mount2(true);
assert(!host2.innerHTML.includes("art-a"), "cards-only live: nothing while the turn runs");
await mount2(false);
assert(host2.querySelectorAll(".turn-card-in").length === 2, "cards-only live: both cards fade in when the turn ends");
await React.act(async () => root2.unmount());

// The stylesheet: 4px rise over 180ms, and reduced motion turns it off.
const css: string = fs.readFileSync(path.join(process.cwd(), "src", "styles.css"), "utf8");
assert(/\.turn-card-in\s*{[^}]*animation:\s*card-in \.18s ease-out/.test(css), "the card animation is 180ms ease-out");
assert(/@keyframes card-in\s*{\s*from\s*{[^}]*opacity:\s*0;[^}]*translateY\(4px\)/.test(css), "the keyframe fades in with a 4px rise");
assert(/@media \(prefers-reduced-motion: reduce\)\s*{\s*\.turn-card-in\s*{\s*animation:\s*none;/.test(css), "reduced motion disables the card animation");
loader.prototype.require = originalRequire;
}
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
