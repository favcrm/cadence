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
  let nativeFails: (() => Error) | null = null;
  let nativeStored: string | null = "native text";
  let nativeMode = false;
  let reloads = 0;
  const deps: DocDeps = {
    readProfile: async () => stored,
    writeProfile: async (_id, body) => {
      writes.push(body as Record<string, unknown>);
      if (writeFails) throw writeFails();
      if ((body as { expectedRevision: number }).expectedRevision !== stored.revision) throw new ApiError("Reload the agent profile.", 409);
      stored = profile({ revision: stored.revision + 1, responsibilities: String((body as { responsibilities: string }).responsibilities).trim() });
      return stored;
    },
    readNative: async () => nativeStored,
    writeNative: async (_alias, text) => {
      if (nativeFails) throw nativeFails();
      nativeStored = text;
      return { applies: "next_turn" };
    },
  };
  const toasts: [string, string][] = [];
  const toast = (kind: string, text: string) => toasts.push([kind, text]);

  function Harness({ writable, tick }: { writable: boolean; tick: number }) {
    const doc = useInstructionsDoc({
      alias: "ads-agent", kind: nativeMode ? "native" : "hosted", profileId: nativeMode ? null : "ap_1", nativeServed: nativeMode ? "native text" : null, writable, onSaved: () => { reloads += 1; },
      toast: toast as never, deps,
    });
    if (doc.read !== "ready") return React.createElement("p", null, "loading");
    return React.createElement("div", { "data-tick": tick },
      React.createElement(Editor, { doc, writable, resetTo: null, contextHeading: "x", contextLines: [], contextNote: null }));
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
  assert(button("Discard mine, use theirs") && button("Save mine") && !button("Save"), "use theirs or save mine");
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
  await click("Discard mine, use theirs");
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

  // Save mine writes at the revision the conflict was raised with: a newer one
  // gets another conflict, and nothing is written over it.
  await type("# Ads\n\nMine three.");
  writeFails = () => new ApiError("Reload the agent profile.", 409);
  stored = profile({ revision: 20, responsibilities: "# Ads\n\nTheirs 20." });
  await click("Save");
  writeFails = null;
  stored = profile({ revision: 21, responsibilities: "# Ads\n\nTheirs 21, unseen." });
  const writesBefore = writes.length;
  await click("Save mine");
  assert(writes.length === writesBefore + 1 && writes[writes.length - 1]!.expectedRevision === 20, "save mine writes at the revision the conflict named");
  assert(stored.responsibilities === "# Ads\n\nTheirs 21, unseen.", "a newer revision is not overwritten");
  assert(button("Save mine") && area().value === "# Ads\n\nMine three.", "another conflict, the draft still kept");
  await click("Discard mine, use theirs");
  assert(area().value === "# Ads\n\nTheirs 21, unseen.", "use theirs loads the newest");

  // A lost answer locks the text; Discard reads first and says what the board holds.
  writeFails = () => new TypeError("Failed to fetch");
  await type("# Ads\n\nLost answer.");
  await click("Save");
  assert(area().readOnly, "the text is locked while the outcome is unknown");
  stored = profile({ revision: stored.revision + 1, responsibilities: "# Ads\n\nLost answer." });
  toasts.length = 0;
  await click("Discard");
  assert(toasts.some(([kind, text]) => kind === "ok" && /did save/.test(text)), "Discard reports that the write landed");
  assert(area().value === "# Ads\n\nLost answer." && !button("Check now") && !button("Discard"), "and the board's text is what is shown");
  await type("# Ads\n\nNever landed.");
  await click("Save");
  toasts.length = 0;
  await click("Discard");
  assert(toasts.some(([, text]) => /did not save/.test(text)), "Discard says when the write did not land");
  assert(area().value === "# Ads\n\nLost answer.", "and shows what the board holds");
  writeFails = null;

  // Unsaved work stays on screen, disabled, when the board stops taking writes.
  await type("# Ads\n\nStale roster.");
  await render(false, 3);
  assert(area().value === "# Ads\n\nStale roster." && area().readOnly, "the draft stays, read-only");
  assert(/Not saved/.test(host.querySelector(".ap-doc-save")?.textContent ?? "") && button("Save")?.disabled, "Save is shown disabled, with the reason");
  await render(true, 4);
  assert(!button("Save")?.disabled, "and comes back when the board does");
  await click("Discard");

  // Preview drops a javascript: link; a reader gets no inert Edit.
  await type("[click](javascript:alert(1)) and [ok](https://example.com)");
  await click("Preview");
  assert(![...host.querySelectorAll(".ap-doc-preview a")].some((a) => (a.getAttribute("href") ?? "").startsWith("javascript:")), "a javascript: link is not a link");
  await click("Discard");

  // A viewer reads it and cannot change it.
  await click("Edit");
  await render(false);
  assert(area().readOnly && !button("Save") && !button("Discard"), "a viewer reads, and has no write action");
  assert(!button("Edit"), "a reader has no inert Edit toggle");
  assert(button("Preview") && !button("Preview")!.hidden && area().value.length > 0 && !area().hidden, "a reader still reads the text by default, and can preview it");
  await click("Preview");
  assert(host.querySelector(".ap-doc-preview"), "a reader gets Preview");

  // Native has no revision: a 409 there is a refusal, not use-theirs or save-mine.
  nativeMode = true;
  await React.act(async () => { root.unmount(); });
  const root2 = createRoot(host);
  await React.act(async () => { root2.render(React.createElement(Harness, { writable: true, tick: 9 })); });
  await flush();
  await type("native edit");
  nativeFails = () => new ApiError("conflict", 409);
  await click("Save");
  assert(!button("Save mine") && !button("Discard mine, use theirs") && button("Save"), "a native 409 offers neither use theirs nor save mine");
  assert(toasts.some(([kind]) => kind === "err"), "it says why");
  // A lost native answer with an unreadable board is unconfirmed, never "did not save".
  nativeFails = () => new TypeError("Failed to fetch");
  await click("Save");
  nativeStored = null;
  toasts.length = 0;
  await click("Check now");
  assert(button("Check now") && !toasts.some(([, text]) => /did not save/.test(text)), "a null native read stays unconfirmed");
  // A native read that finds other text shows it: Check says so and keeps the
  // draft, Discard shows the board's text; neither says "did not save" over it.
  await React.act(async () => { root2.unmount(); });
  const root3 = createRoot(host);
  await React.act(async () => { root3.render(React.createElement(Harness, { writable: true, tick: 10 })); });
  await flush();
  nativeFails = null;
  nativeStored = "native text";
  reloads = 0;
  await type("saved ok");
  await click("Save");
  assert(reloads === 1 && nativeStored === "saved ok" && !button("Save"), "a native save reloads the page's stored text");
  await type("lost A");
  nativeFails = () => new TypeError("Failed to fetch");
  await click("Save");
  nativeStored = "SOMEONE ELSE";
  reloads = 0;
  toasts.length = 0;
  await click("Check now");
  assert(toasts.some(([, text]) => /different version\. Your draft is kept\. Discard shows theirs/.test(text)) && !toasts.some(([, text]) => text.startsWith("It did not save")), "native Check says the draft is kept and theirs is a Discard away");
  assert(reloads === 1 && area().value === "lost A", "and reloads the page's stored text, keeping the draft");
  await click("Discard");
  nativeFails = () => new TypeError("Failed to fetch");
  await type("lost B");
  await click("Save");
  nativeStored = "ELSE AGAIN";
  reloads = 0;
  toasts.length = 0;
  await click("Discard");
  assert(toasts.some(([, text]) => /different version, now shown/.test(text)), "native Discard says the board holds a different version");
  assert(reloads === 1 && area().value === "ELSE AGAIN", "and shows what the board holds");
  // Same text as the page holds stored: then, and only then, "did not save".
  await type("lost C");
  await click("Save");
  nativeStored = "ELSE AGAIN";
  reloads = 0;
  toasts.length = 0;
  await click("Discard");
  assert(toasts.some(([, text]) => /did not save/.test(text)) && reloads === 0, "an unchanged board is the only time it says did not save");
  await React.act(async () => { root3.unmount(); });
  console.log("instructionsEditor checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
