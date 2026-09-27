export {};
/** Publication navigation/lifetime and preserved operator proof; Markdown itself is covered in browser verification. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/outbox" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
  "history",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons")
    return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  if (id.endsWith("/ui/Md"))
    return {
      __esModule: true,
      default: ({ text }: { text: string }) =>
        React.createElement("p", {}, text),
    };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } =
  require("react-dom/client") as typeof import("react-dom/client");
const Outbox = (
  require("../src/features/outbox/Outbox") as typeof import("../src/features/outbox/Outbox")
).default;
const { resources } =
  require("../src/lib/resources") as typeof import("../src/lib/resources");
const { navigate } =
  require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
type Post = import("../src/lib/types").OutboxItem & { post?: string | null };
const a: Post = {
  effect_id: "a",
  project: "site",
  title: "Autumn launch",
  preview: "A reviewed campaign",
  published_at: "2026-09-27T08:00:00Z",
  post: "Saved autumn content",
  path: "/outbox/site/a",
  attachments: [{ name: "creative.png", bytes: 0, sha256: "sha" }],
};
const b: Post = {
  effect_id: "b",
  project: "website",
  title: "Site update",
  published_at: "2026-09-26T08:00:00Z",
  post: "Other saved content",
};
const reads: string[] = [];
const waits: { id: string; resolve: (value: Response) => void }[] = [];
let holdDetails = false;
let failDetails = false;
let failList = false;
let list: Post[] = [b, a];
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
globalThis.fetch = async (input, init) => {
  if (init?.method && init.method !== "GET")
    throw new Error("Outbox must remain read-only");
  const url = new URL(String(input), "http://localhost");
  reads.push(url.pathname + url.search);
  if (url.pathname !== "/api/outbox")
    throw new Error(`Unexpected request ${url.pathname}`);
  const id = url.searchParams.get("effect_id");
  if (id) {
    if (holdDetails)
      return new Promise<Response>((resolve) => waits.push({ id, resolve }));
    if (failDetails) return json({ error: "Post unavailable" }, 503);
    return json({ item: id === "a" ? a : b });
  }
  return failList
    ? json({ error: "History unavailable" }, 503)
    : json({ items: list });
};
function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = async () => {
  await React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
};
const render = async (operator: boolean) => {
  await React.act(async () =>
    root.render(React.createElement(Outbox, { operator })),
  );
  await flush();
};
const move = async (href: string) => {
  await React.act(async () => navigate(href));
  await flush();
};
const click = async (el: Element | null) => {
  assert(el, "click target exists");
  await React.act(async () =>
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, button: 0 })),
  );
  await flush();
};
const button = (label: string) =>
  Array.from(host.querySelectorAll("button")).find(
    (el) => el.textContent?.trim() === label,
  );

async function main() {
  await move("/outbox?item=a");
  await render(false);
  assert(
    reads.length === 0 &&
      host.textContent?.includes("Sign in to view") &&
      !host.textContent?.includes("No published posts"),
    "unproven viewer never requests or claims empty, even on a direct link",
  );
  failList = true;
  await render(true);
  assert(
    Number(reads.length) === 1 && reads[0].includes("effect_id=a"),
    "direct detail does not read its failing collection dependency",
  );
  assert(
    host.textContent?.includes("Autumn launch") &&
      host.textContent?.includes("Saved autumn content"),
    "direct detail includes title and saved content",
  );
  assert(
    host.textContent?.includes("creative.png") &&
      host.textContent?.includes("0 B"),
    "zero-byte attachment metadata retained",
  );
  assert(
    !host.querySelector("details")?.hasAttribute("open") &&
      host.textContent?.includes("/outbox/site/a"),
    "storage identifiers retained inside closed disclosure",
  );
  failDetails = true;
  await click(host.querySelector('[aria-label="Refresh post"]'));
  assert(
    host.textContent?.includes("Could not load this post") && button("Retry"),
    "item errors are recoverable",
  );
  failDetails = false;
  await click(button("Retry")!);
  assert(
    host.textContent?.includes("Saved autumn content"),
    "retry reads item again",
  );
  a.post = null;
  await click(host.querySelector('[aria-label="Refresh post"]'));
  assert(
    host.textContent?.includes("Autumn launch") &&
      host.textContent?.includes("creative.png") &&
      button("Retry content"),
    "missing content retains publication and attachment evidence",
  );
  a.post = "Recovered content";
  await click(button("Retry content")!);
  assert(
    host.textContent?.includes("Recovered content"),
    "missing content can recover",
  );

  await move("/outbox?outbox_q=autumn&outbox_project=site&source=app");
  assert(
    host.textContent?.includes("Could not load published posts") &&
      button("Retry"),
    "failed list does not claim empty",
  );
  failList = false;
  await click(button("Retry")!);
  const links = Array.from(host.querySelectorAll(".outbox-post-link"));
  assert(
    links.length === 1 && links[0].textContent?.includes("Autumn launch"),
    "exact project and all-term query applied to collection",
  );
  assert(
    host.querySelector("label")?.getAttribute("for") ===
      host.querySelector("input")?.id,
    "native search has a visible associated label",
  );
  await click(links[0]);
  const back = host.querySelector('a[href*="outbox_q="]');
  assert(
    back?.getAttribute("href")?.includes("source=app") &&
      back.getAttribute("href")?.includes("outbox_project=site"),
    "detail back preserves filter and unrelated parameters",
  );
  await click(back);
  assert(
    host.querySelectorAll(".outbox-post-link").length === 1 &&
      host.querySelector("input")?.value === "autumn",
    "back restores filtered list",
  );
  failList = true;
  await React.act(async () => {
    await resources.outbox.refresh();
  });
  assert(
    host.textContent?.includes("Showing the last loaded posts") &&
      host.querySelectorAll(".outbox-post-link").length === 1,
    "failed refresh retains and labels stale posts",
  );
  failList = false;
  await click(button("Retry")!);
  await click(button("Clear filters")!);
  assert(
    host.querySelectorAll(".outbox-post-link").length === 2 &&
      location.search === "?source=app",
    "clear filters leaves unrelated parameters and all posts ordered",
  );
  assert(
    host
      .querySelector(".outbox-post-link")
      ?.textContent?.includes("Autumn launch"),
    "newest post first",
  );
  await move("/outbox?outbox_q=missing");
  assert(
    host.textContent?.includes("No matching posts") &&
      !host.textContent?.includes("No published posts yet"),
    "filtered empty is distinct from no publications",
  );
  list = [];
  await React.act(async () => {
    await resources.outbox.refresh();
  });
  await move("/outbox");
  assert(
    host.textContent?.includes("No published posts yet"),
    "successful empty history has publication guidance",
  );

  holdDetails = true;
  await move("/outbox?item=a");
  assert(waits.length === 1, "first held detail request");
  await move("/outbox?item=b");
  await move("/outbox?item=a");
  assert(
    Number(waits.length) === 3,
    "A to B to A creates independent request lifetimes",
  );
  await React.act(async () => {
    waits[2].resolve(json({ item: { ...a, post: "Current A" } }));
  });
  await flush();
  await React.act(async () => {
    waits[0].resolve(json({ item: { ...a, post: "Obsolete A" } }));
    waits[1].resolve(json({ item: b }));
  });
  await flush();
  assert(
    host.textContent?.includes("Current A") &&
      !host.textContent?.includes("Obsolete A") &&
      !host.textContent?.includes("Other saved content"),
    "obsolete same-alias and other-item responses cannot replace current content",
  );
  await click(host.querySelector('[aria-label="Refresh post"]'));
  const count = reads.length;
  await render(false);
  await React.act(async () => {
    waits[3].resolve(json({ item: a }));
  });
  await flush();
  assert(
    reads.length === count &&
      host.textContent?.includes("Sign in to view") &&
      !host.textContent?.includes("Recovered content"),
    "access revocation unmounts protected content and ignores pending response",
  );
  await React.act(async () => root.unmount());
  console.log(
    "outbox view: operator proof, independent detail, retry, filters, stale data and navigation lifetimes passed",
  );
}
main().catch((error) => {
  console.error(error);
  throw error;
});
