/**
 * CAD-2354: the Master's (owner) and the member's own instructions, through the
 * real hook and editor against a fake board: edit, save, reload; a stale
 * revision keeps the draft; an older board keeps the read-only note; the
 * Context drawer never carries built-in text.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type { ApiError as ApiErrorType } from "../src/lib/apiError";
import type { OwnDeps, OwnDoc, OwnKind } from "../src/features/agents/instructionsDoc";
import type * as DocModule from "../src/features/agents/instructionsDoc";
import type * as PersonalModule from "../src/features/agents/personalAssistant";
import type InstructionsEditorType from "../src/features/agents/InstructionsEditor";
import type * as HookModule from "../src/features/agents/useInstructionsDoc";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(`ownInstructions: ${why}`);
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

  const docs = require("../src/features/agents/instructionsDoc") as typeof DocModule;
  const people = require("../src/features/agents/personalAssistant") as typeof PersonalModule;
  const { api } = require("../src/lib/api") as { api: Record<string, (...a: unknown[]) => Promise<unknown>> };
  const hook = require("../src/features/agents/useInstructionsDoc") as typeof HookModule & { liveOwnDeps: OwnDeps };

  // Kinds: only an owner's Master page and the member's own page edit these.
  assert(docs.instructionsKind({ hired: false, hostedBoard: true, served: null, master: true, profileBound: false, own: "workspace" }) === "workspace", "workspace kind");
  assert(docs.instructionsKind({ hired: false, hostedBoard: true, served: null, master: false, profileBound: false, own: "personal" }) === "personal", "personal kind");
  assert(docs.instructionsKind({ hired: false, hostedBoard: false, served: "x", master: true, profileBound: false, own: "workspace" }) === null, "native master stays refused");
  assert(docs.instructionsKind({ hired: false, hostedBoard: true, served: null, master: true, profileBound: false }) === null, "no own page, no kind");
  const masterRow = { alias: "master", scope: "workspace" } as never;
  assert(!people.masterShownTo(masterRow, { personal_assistant: { scope: "personal" } } as never, true, false), "a member never reaches the Master page");
  assert(people.masterShownTo(masterRow, { personal_assistant: { scope: "personal" } } as never, true, true), "an owner does");

  // Context never includes the built-in text; only a valid server estimate.
  for (const kind of ["workspace", "personal"] as const) {
    const ctx = docs.contextOf("a".repeat(40), kind, null);
    const names = ctx.lines.map((l) => l.name);
    assert(names[0] === "Built-in instructions" && ctx.lines[0]!.size === "Not counted" && ctx.lines[0]!.count === undefined, "built-in is named, never sized");
    assert(ctx.lines[1]!.count === 10 && /Owner-managed|Your instructions/.test(names[1]!), "then the managed section, estimated");
    assert(!JSON.stringify(ctx).includes("aaaa"), "the draft text is not a context line");
    for (const invalid of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1, NaN, "4", null]) {
      assert(docs.contextOf("text", kind, null, invalid as number).lines[0]!.size === "Not counted", `${kind}: malformed count is not shown`);
    }
    assert(docs.contextOf("text", kind, null, 0).lines[0]!.count === 0, `${kind}: zero is a known estimate`);
  }

  // The seam: each route's own revision type and body.
  const fs = require("fs") as { readFileSync(path: string, encoding: string): string };
  const nodeProcess = require("process") as { cwd(): string };
  const fixture = JSON.parse(fs.readFileSync(
    `${nodeProcess.cwd()}/../tests/fixtures/agenticos/assistant-instruction-estimates.json`, "utf8",
  )) as {
    workspace: { instructions: string; revision: string; built_in_instruction_tokens: number };
    personal: { instructions: string; revision: number; built_in_instruction_tokens: number };
    legacyWorkspace: { instructions: string; revision: string };
    legacyPersonal: { instructions: string; revision: number };
  };
  api.masterInstructions = async () => fixture.workspace;
  api.memberAssistInstructions = async () => fixture.personal;
  const currentWorkspace = await hook.liveOwnDeps.read("workspace");
  const currentPersonal = await hook.liveOwnDeps.read("personal");
  assert(currentWorkspace.text === "Be brief.\nAnswer in English." && currentWorkspace.revision === "41" && currentWorkspace.builtInInstructionTokens === 127, "producer workspace read preserves string revision and estimate");
  assert(currentPersonal.text === "Call me Sam." && currentPersonal.revision === 7 && currentPersonal.builtInInstructionTokens === 127, "producer personal read preserves number revision and estimate");
  api.masterInstructions = async () => fixture.legacyWorkspace;
  api.memberAssistInstructions = async () => fixture.legacyPersonal;
  const legacyWorkspace = await hook.liveOwnDeps.read("workspace");
  const legacyPersonal = await hook.liveOwnDeps.read("personal");
  assert(legacyWorkspace.text === "Be brief.\nAnswer in English." && legacyWorkspace.revision === "41" && legacyWorkspace.builtInInstructionTokens === undefined, "legacy workspace omits estimate");
  assert(legacyPersonal.text === "Call me Sam." && legacyPersonal.revision === 7 && legacyPersonal.builtInInstructionTokens === undefined, "legacy personal omits estimate");

  const calls: [string, unknown][] = [];
  api.masterInstructions = async () => ({ instructions: "Company text", revision: "41", built_in_instruction_tokens: 123 });
  api.masterState = async () => { throw new Error("instructions do not require a Master-state read"); };
  api.masterIdentityUpdate = async (body) => { calls.push(["master", body]); return { identity: { name: "Ada", tone: null, look: "robot", revision: "42" }, applies: "next_turn" }; };
  api.memberAssistInstructions = async () => ({ instructions: null, revision: 7, built_in_instruction_tokens: 0 });
  api.memberAssistProfileUpdate = async (body) => { calls.push(["personal", body]); return { assistant: { identity: { revision: 8 } } }; };
  const m = await hook.liveOwnDeps.read("workspace");
  assert(m.text === "Company text" && m.revision === "41" && m.builtInInstructionTokens === 123, "master read, string revision and estimate");
  const instructionText = "    New company text\n";
  const mw = await hook.liveOwnDeps.write("workspace", m, instructionText);
  assert(mw.revision === "42" && mw.text === instructionText, "master write confirms the exact text and revision");
  const sent = calls[0]![1] as Record<string, unknown>;
  assert(JSON.stringify(sent) === JSON.stringify({ expected_revision: "41", instructions: instructionText }), "the instructions write cannot overwrite concurrent identity edits");
  const p = await hook.liveOwnDeps.read("personal");
  assert(p.text === "" && p.revision === 7 && p.builtInInstructionTokens === 0, "personal read, number revision, unset is empty and zero is known");
  await hook.liveOwnDeps.write("personal", p, "  Mine\n");
  assert(JSON.stringify(calls[1]![1]) === JSON.stringify({ expected_revision: 7, action: "update", instructions: "  Mine\n" }), "personal write preserves the exact instructions");
  for (const invalid of [undefined, null, "3", -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
    api.masterInstructions = async () => ({ instructions: "", revision: "1", built_in_instruction_tokens: invalid });
    api.memberAssistInstructions = async () => ({ instructions: "", revision: 1, built_in_instruction_tokens: invalid });
    for (const kind of ["workspace", "personal"] as const) {
      assert((await hook.liveOwnDeps.read(kind)).builtInInstructionTokens === undefined, `${kind}: invalid response estimate is ignored`);
    }
  }

  // A fake board for each kind, with its own revision type.
  const boards: Record<OwnKind, { doc: OwnDoc; next: (r: string | number) => string | number }> = {
    workspace: { doc: { text: "Company text", revision: "41", builtInInstructionTokens: 123 }, next: (r) => String(Number(r) + 1) },
    personal: { doc: { text: "My text", revision: 7, builtInInstructionTokens: 0 }, next: (r) => Number(r) + 1 },
  };
  let kindUnderTest: OwnKind = "workspace";
  let unsupported = false;
  let writeFails: (() => Error) | null = null;
  const writes: { revision: string | number; text: string }[] = [];
  const ownDeps: OwnDeps = {
    read: async (k) => { if (unsupported) throw new ApiError("not found", 404); return { ...boards[k].doc }; },
    write: async (k, base, text) => {
      writes.push({ revision: base.revision, text });
      if (writeFails) throw writeFails();
      const b = boards[k];
      if (String(base.revision) !== String(b.doc.revision)) throw new ApiError("stale", 409);
      b.doc = { ...b.doc, text: text.trim(), revision: b.next(b.doc.revision) };
      return { text: b.doc.text, revision: b.doc.revision };
    },
  };
  const toasts: [string, string][] = [];
  const toast = (kind: string, text: string) => toasts.push([kind, text]);
  function Harness({ writable }: { writable: boolean }) {
    const doc = useInstructionsDoc({ alias: "x", kind: kindUnderTest, profileId: null, nativeServed: null, writable, onSaved: () => undefined, toast: toast as never, ownDeps });
    if (doc.read === "unsupported") return React.createElement("p", null, "read-only note");
    if (doc.read !== "ready") return React.createElement("p", null, "loading");
    const context = docs.contextOf(doc.draft ?? doc.served ?? "", doc.kind, doc.applies, doc.builtInInstructionTokens);
    return React.createElement(React.Fragment, null,
      React.createElement("output", null, JSON.stringify(context.lines.slice(0, 2))),
      React.createElement(Editor, { doc, writable, resetTo: null, contextHeading: context.heading, contextLines: context.lines, contextNote: null }));
  }
  const host = document.createElement("div");
  document.body.appendChild(host);
  const flush = async () => { await React.act(async () => { await new Promise((r) => setTimeout(r, 20)); }); };
  const area = () => host.querySelector("textarea") as HTMLTextAreaElement;
  const button = (label: string) => [...host.querySelectorAll("button")].find((b) => b.textContent === label) as HTMLButtonElement | undefined;
  const lines = () => JSON.parse(host.querySelector("output")!.textContent!) as { size: string; count?: number }[];
  const type = async (value: string) => {
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
    await React.act(async () => { setter.call(area(), value); area().dispatchEvent(new Event("input", { bubbles: true })); });
  };
  const click = async (label: string) => {
    const b = button(label);
    assert(b, `a ${label} button`);
    await React.act(async () => { b.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  };
  const mount = async (writable = true) => {
    const root = createRoot(host);
    await React.act(async () => { root.render(React.createElement(Harness, { writable })); });
    await flush();
    return root;
  };

  for (const kind of ["workspace", "personal"] as const) {
    kindUnderTest = kind;
    writes.length = 0;
    toasts.length = 0;
    const start = boards[kind].doc;
    let root = await mount();
    assert(area().value === start.text, `${kind}: the stored text loads`);
    const estimate = kind === "workspace" ? 123 : 0;
    assert(lines()[0]!.count === estimate, `${kind}: read estimate reaches the presenter`);
    assert(!button("Save"), `${kind}: no Save without a change`);
    // Whitespace-only is allowed (it clears); a control character is not.
    await type("\u0001");
    assert(button("Save")!.disabled, `${kind}: a control character cannot be saved`);
    await type("Edited text");
    assert(lines()[0]!.count === estimate && lines()[1]!.count === 3, `${kind}: editing keeps built-in and updates managed estimate`);
    await click("Save");
    assert(writes.length === 1 && writes[0]!.revision === start.revision, `${kind}: one write at the revision read`);
    assert(boards[kind].doc.text === "Edited text" && !button("Save"), `${kind}: saved, bar gone`);
    assert(lines()[0]!.count === estimate, `${kind}: save response without metadata preserves estimate`);
    assert(toasts.some(([k, t]) => k === "ok" && t === "Saved. A running turn isn’t changed."), `${kind}: the toast promises nothing about when`);
    // Reload shows it.
    await React.act(async () => { root.unmount(); });
    root = await mount();
    assert(area().value === "Edited text", `${kind}: reload shows the saved text`);

    // Stale revision: the draft is kept and the conflict choices appear.
    await type("My newer draft");
    const theirs = boards[kind].doc;
    const newestEstimate = estimate + 5;
    boards[kind].doc = { text: "Their text", revision: boards[kind].next(theirs.revision), builtInInstructionTokens: newestEstimate };
    toasts.length = 0;
    await click("Save");
    assert(area().value === "My newer draft", `${kind}: the draft is kept`);
    assert(button("Save mine") && button("Discard mine, use theirs") && !button("Save"), `${kind}: conflict choices`);
    assert(host.querySelector("[role=alert]")?.textContent === "Someone else changed this. Your draft is kept." && toasts.length === 0, `${kind}: announce conflicts inline without an overlay blocking the choices`);
    assert(lines()[0]!.count === newestEstimate, `${kind}: conflict reread refreshes the estimate while keeping the draft`);
    assert((boards[kind].doc.text as string) === "Their text", `${kind}: nothing overwritten`);
    await click("Save mine");
    assert(String(writes[writes.length - 1]!.revision) === String(boards[kind].doc.revision) || (boards[kind].doc.text as string) === "My newer draft", `${kind}: save mine rides their revision`);
    assert((boards[kind].doc.text as string) === "My newer draft", `${kind}: save mine wrote`);
    assert(lines()[0]!.count === newestEstimate, `${kind}: conflict save preserves the reread estimate`);

    // A dropped answer is not confirmed, never saved; a check finds it did not land.
    await type("Over a bad line");
    const { text, revision } = boards[kind].doc;
    boards[kind].doc = { text, revision };
    writeFails = () => new TypeError("Failed to fetch");
    toasts.length = 0;
    await click("Save");
    assert(toasts.some(([k, t]) => k === "warn" && /Not confirmed/.test(t)) && button("Check now"), `${kind}: not confirmed`);
    writeFails = null;
    await click("Check now");
    assert(toasts.some(([, t]) => /did not save/.test(t)) && area().value === "Over a bad line", `${kind}: the check keeps the draft`);
    assert(lines()[0]!.size === "Not counted", `${kind}: authoritative check without metadata drops stale estimate`);
    await React.act(async () => { root.unmount(); });
  }

  // An older board: no routes, so the read-only note, never an editor.
  unsupported = true;
  kindUnderTest = "workspace";
  const old = await mount();
  assert(host.textContent === "read-only note" && !area(), "an older board keeps the read-only note");
  await React.act(async () => { old.unmount(); });
  console.log("ownInstructions checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
