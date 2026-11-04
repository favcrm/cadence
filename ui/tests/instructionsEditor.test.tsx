/**
 * CAD-2091: the Instructions editor on a hosted profile, through the real hook
 * and component against a fake board. The things a person feels: Save only on a
 * change, a stale revision keeps the draft and asks, a dropped connection never
 * says saved, the caret survives Preview, and the document cannot inject HTML.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type { ApiError as ApiErrorType } from "../src/lib/apiError";
import type { StoredAgentProfile } from "../src/lib/types";
import type { DocDeps } from "../src/features/agents/instructionsDoc";
import type InstructionsEditorType from "../src/features/agents/InstructionsEditor";
import type * as HookModule from "../src/features/agents/useInstructionsDoc";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(`instructionsEditor: ${why}`);
}
type ModuleLoader = { prototype: { require: (this: unknown, id: string) => unknown } };

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module") as ModuleLoader;
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, [id]);
  };
  const React = require("react") as typeof ReactNs;
  const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
  const { ApiError } = require("../src/lib/apiError") as { ApiError: typeof ApiErrorType };
  const Editor = (require("../src/features/agents/InstructionsEditor") as { default: typeof InstructionsEditorType }).default;
  const { useInstructionsDoc } = require("../src/features/agents/useInstructionsDoc") as typeof HookModule;

  const profile = (over: Partial<StoredAgentProfile> = {}): StoredAgentProfile => ({
    id: "ap_1", name: "ads-agent", role: "Ads", responsibilities: "# Ads\n\nOld text.", skills: ["cadence"], status: "active",
    revision: 3, created_at: 1, updated_at: 2, created_by: "owner", catalogue: "ads", model: "inherit", ...over,
  });

  let stored = profile();
  const writes: Record<string, unknown>[] = [];
  let writeFails: (() => Error) | null = null;
  const deps: DocDeps = {
    readProfile: async () => stored,
    writeProfile: async (_id, body) => {
      writes.push(body as Record<string, unknown>);
      if (writeFails) throw writeFails();
      stored = profile({ revision: stored.revision + 1, responsibilities: String((body as { responsibilities: string }).responsibilities).trim() });
      return stored;
    },
    readNative: async () => null,
    writeNative: async () => ({ applies: "next_turn" }),
  };
  const toasts: [string, string][] = [];
  const toast = (kind: string, text: string) => toasts.push([kind, text]);

  function Harness({ writable, tick }: { writable: boolean; tick: number }) {
    const doc = useInstructionsDoc({
      alias: "ads-agent", kind: "hosted", profileId: "ap_1", nativeServed: null, writable, onSaved: () => {},
      toast: toast as never, deps,
    });
    if (doc.read !== "ready") return React.createElement("p", null, "loading");
    return React.createElement("div", { "data-tick": tick },
      React.createElement(Editor, { doc, writable, resetTo: null, contextLines: [], contextNote: null }));
  }

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const flush = async () => { await React.act(async () => { await new Promise((r) => setTimeout(r, 20)); }); };
  const render = async (writable: boolean, tick = 0) => {
    await React.act(async () => { root.render(React.createElement(Harness, { writable, tick })); });
    await flush();
  };
  const area = () => host.querySelector("textarea") as HTMLTextAreaElement;
  const button = (label: string) => [...host.querySelectorAll("button")].find((b) => b.textContent === label) as HTMLButtonElement | undefined;
  const type = async (value: string) => {
    const el = area();
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
    await React.act(async () => { setter.call(el, value); el.dispatchEvent(new Event("input", { bubbles: true })); });
  };
  const click = async (label: string) => {
    const b = button(label);
    assert(b, `a ${label} button`);
    await React.act(async () => { b.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  };
  const primaries = () => host.querySelectorAll(".btn-primary").length;

  await render(true);
  assert(area().value === stored.responsibilities, "the stored text is loaded");

  // Save appears only on a change, and is the one primary.
  assert(!button("Save"), "no Save without a change");
  await type("# Ads\n\nMy text.");
  assert(button("Save") && primaries() === 1, "Save appears on a change, as the only primary");
  await type(stored.responsibilities);
  assert(!button("Save"), "typing back to the stored text removes Save");

  // Caret and draft survive a re-render and Preview; Preview cannot inject HTML.
  const hostile = "# Hi\n\n<img src=x onerror=alert(1)> <script>boom()</script>\n\n**bold** line";
  await type(hostile);
  const before = area();
  before.setSelectionRange(5, 5);
  before.dispatchEvent(new Event("select", { bubbles: true }));
  await render(true, 1);
  assert(area() === before && area().selectionStart === 5, "a re-render keeps the same textarea and caret");
  await click("Preview");
  const preview = host.querySelector(".ap-doc-preview")!;
  assert(preview, "Preview shows");
  assert(!preview.querySelector("img") && !preview.querySelector("script"), "Preview renders no raw HTML element");
  assert(preview.querySelector("strong"), "Preview renders the Markdown");
  assert(area() === before && area().hidden, "the textarea stays mounted, hidden");
  await click("Edit");
  assert(area().value === hostile && area().selectionStart === 5, "the draft and caret are back on Edit");

  // A stale revision keeps the draft and asks; nothing is overwritten.
  await type("# Ads\n\nMine.");
  writeFails = () => new ApiError("Reload the agent profile.", 409);
  stored = profile({ revision: 4, responsibilities: "# Ads\n\nTheirs." });
  await click("Save");
  assert(writes.length === 1 && writes[0]!.expectedRevision === 3, "one write, at the revision the draft started from");
  assert(area().value === "# Ads\n\nMine.", "the draft is kept");
  assert(button("Use theirs") && button("Save mine") && !button("Save"), "use theirs or save mine");
  assert(primaries() === 1, "still one primary");
  assert(toasts.some(([, text]) => /changed this/.test(text)), "a toast says so");
  writeFails = null;
  await click("Save mine");
  assert((writes.length as number) === 2 && writes[1]!.expectedRevision === 4 && writes[1]!.responsibilities === "# Ads\n\nMine.", "save mine rides their revision with my text");
  assert(stored.responsibilities === "# Ads\n\nMine." && !button("Save mine"), "saved, and the bar is gone");
  assert(toasts.some(([kind, text]) => kind === "ok" && /running turn/.test(text)), "the toast says when it applies");

  // Use theirs replaces the draft.
  await type("# Ads\n\nDraft two.");
  writeFails = () => new ApiError("Reload the agent profile.", 409);
  stored = profile({ revision: 9, responsibilities: "# Ads\n\nTheirs again." });
  await click("Save");
  await click("Use theirs");
  assert(area().value === "# Ads\n\nTheirs again." && !button("Save mine"), "use theirs loads their text");

  // A dropped connection is not confirmed, never success; Check settles it.
  writeFails = () => new TypeError("Failed to fetch");
  toasts.length = 0;
  await type("# Ads\n\nOver a bad line.");
  await click("Save");
  assert(toasts.length === 1 && toasts[0]![0] === "warn" && /Not confirmed/.test(toasts[0]![1]), "not confirmed, as a warning");
  assert(!toasts.some(([, text]) => /^Saved|did save/.test(text)), "never says saved");
  assert(button("Check now") && primaries() === 1 && !button("Save"), "offers a check as the one primary");
  assert(area().value === "# Ads\n\nOver a bad line.", "the draft is kept");
  await click("Check now");
  assert(toasts.some(([, text]) => /did not save/.test(text)) && button("Save"), "a check that finds nothing keeps the draft and Save");
  writeFails = () => new TypeError("Failed to fetch");
  await click("Save");
  // The write actually landed before the line dropped.
  stored = profile({ revision: stored.revision + 1, responsibilities: "# Ads\n\nOver a bad line." });
  await click("Check now");
  assert(toasts.some(([kind, text]) => kind === "ok" && /did save/.test(text)), "a check that finds it says it did save");
  assert(!button("Save") && !button("Check now"), "and the bar is gone");

  // A viewer reads it and cannot change it.
  await render(false);
  assert(area().readOnly && !button("Save") && !button("Discard"), "a viewer reads, and has no write action");

  await React.act(async () => { root.unmount(); });
  console.log("instructionsEditor checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
