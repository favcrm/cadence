/**
 * CAD-2419: the workspace skill library on a hosted profile's Skills tab,
 * through the real section, drawer and profile hook against a fake board. What
 * a person feels: a board without the library shows nothing, a member sees the
 * library and assignments but can change neither, the owner's toggle saves the
 * profile at its revision (a stale one asks for a reload), Approve approves the
 * revision shown by its digest, and refusals read as plain words.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type { ApiError as ApiErrorType } from "../src/lib/apiError";
import type { StoredAgentProfile, WorkspaceSkillRead, WorkspaceSkillSummary } from "../src/lib/types";
import type { DocDeps } from "../src/features/agents/instructionsDoc";
import type { SkillDeps } from "../src/features/agents/workspaceSkills";
import type * as TabsModule from "../src/features/agents/AgentTabs";
import type { TabContext } from "../src/features/agents/AgentTabs";
import type * as HookModule from "../src/features/agents/useInstructionsDoc";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(`workspaceSkills: ${why}`);
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
  const { WorkspaceSkills, default: AgentTabBody } = require("../src/features/agents/AgentTabs") as typeof TabsModule;
  const { allowPrivateReads } = require("../src/lib/privateSession") as { allowPrivateReads: () => void };
  const { useInstructionsDoc } = require("../src/features/agents/useInstructionsDoc") as typeof HookModule;

  // ---- a fake board: one profile and a three-skill library ----
  let holdWrite: Promise<void> | null = null;
  let stored: StoredAgentProfile = {
    id: "ap_1", name: "ads-agent", role: "Ads", responsibilities: "# Ads", skills: ["weekly-report", "copywriting"], status: "active",
    revision: 4, created_at: 1, updated_at: 2, created_by: "owner", catalogue: null, model: "inherit",
  };
  const profileWrites: Record<string, unknown>[] = [];
  const docDeps: DocDeps = {
    readProfile: async () => stored,
    writeProfile: async (_id, body) => {
      profileWrites.push(body as Record<string, unknown>);
      if (holdWrite) await holdWrite;
      const b = body as { expectedRevision: number; skills: string[] };
      if (b.expectedRevision !== stored.revision) throw new ApiError("Reload the agent profile.", 409, { code: "revision_conflict" });
      stored = { ...stored, skills: b.skills, revision: stored.revision + 1 };
      return stored;
    },
    readNative: async () => null,
    writeNative: async () => ({ applies: "next_turn" }),
  };
  const summary = (name: string, latest: number, approved: number | null): WorkspaceSkillSummary => ({
    name, latest_revision: latest, latest_digest: `${name}-d${latest}`, description: `Synthetic ${name} guidance.`,
    approved: approved === null ? null : { revision: approved, digest: `${name}-d${approved}`, by: "usr_owner", at: 1 },
    created_by: "usr_member", created_at: 1, updated_at: 2,
  });
  let library: WorkspaceSkillSummary[] = [];
  const reset = () => {
    library = [summary("weekly-report", 1, 1), summary("tone-guide", 2, 1), summary("pricing-check", 1, null)];
  };
  reset();
  let listFails: (() => Error) | null = null;
  const calls: [string, ...unknown[]][] = [];
  let decisionFails: (() => Error) | null = null;
  const HOSTILE = "<img src=x onerror=alert(1)> **not bold**";
  const skillDeps: SkillDeps = {
    list: async () => {
      if (listFails) throw listFails();
      return library;
    },
    read: async (name, revision): Promise<WorkspaceSkillRead> => {
      const skill = library.find((s) => s.name === name);
      if (!skill) throw new ApiError("Unknown skill or revision.", 404, { code: "not_found" });
      const n = revision ?? skill.latest_revision;
      return {
        notice: "Workspace guidance.",
        skill,
        revision: {
          name, revision: n, digest: `${name}-d${n}`, description: skill.description,
          files: { "references/b.md": "Reference text", "SKILL.md": `---\ndescription: x\n---\n${HOSTILE}`, "skill.json": "{}" },
          created_by: "usr_member", author: "agent:ads-agent", created_at: 1,
        },
        approved: skill.approved?.revision === n,
      };
    },
    approve: async (name, revision, digest) => {
      calls.push(["approve", name, revision, digest]);
      if (decisionFails) throw decisionFails();
      library = library.map((s) => (s.name === name ? { ...s, approved: { revision, digest, by: "usr_owner", at: 3 } } : s));
      return library.find((s) => s.name === name)!;
    },
    revoke: async (name, digest) => {
      calls.push(["revoke", name, digest]);
      if (decisionFails) throw decisionFails();
      library = library.map((s) => (s.name === name ? { ...s, approved: null } : s));
      return library.find((s) => s.name === name)!;
    },
    remove: async (name, expectedRevision) => {
      calls.push(["delete", name, expectedRevision]);
      if (decisionFails) throw decisionFails();
      library = library.filter((s) => s.name !== name);
    },
  };
  const toasts: [string, string][] = [];
  const toast = (kind: string, text: string) => toasts.push([kind, text]);
  const lastToast = () => toasts[toasts.length - 1]?.[1] ?? "";

  function Harness({ writable }: { writable: boolean }) {
    const doc = useInstructionsDoc({
      alias: "ads-agent", kind: "hosted", profileId: "ap_1", nativeServed: null, writable, onSaved: () => {}, toast: toast as never, deps: docDeps,
    });
    return React.createElement(WorkspaceSkills, { doc, writable, toast: toast as never, deps: skillDeps });
  }

  const host = document.createElement("div");
  document.body.appendChild(host);
  let root = createRoot(host);
  const flush = async () => { await React.act(async () => { await new Promise((r) => setTimeout(r, 20)); }); };
  const mount = async (writable: boolean) => {
    await React.act(async () => { root.unmount(); });
    root = createRoot(host);
    await React.act(async () => { root.render(React.createElement(Harness, { writable })); });
    await flush();
  };
  const all = () => document.body.textContent ?? "";
  const button = (label: string) => [...document.body.querySelectorAll("button")].find((b) => b.textContent === label) as HTMLButtonElement | undefined;
  const click = async (el: Element | undefined | null, why: string) => {
    assert(el, why);
    await React.act(async () => { (el as HTMLElement).click(); });
    await flush();
  };
  const focusedId = (): string => document.activeElement?.id ?? "";
  const row = (name: string) => host.querySelector(`[data-record-id="wskill-${name}"]`);
  const toggle = (name: string) => host.querySelector<HTMLInputElement>(`input[aria-label="Assigned: ${name}"]`);
  const closeDrawer = async () => {
    await React.act(async () => { window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })); });
    // The drawer finishes closing on its transition, or 320 ms without one.
    await React.act(async () => { await new Promise((r) => setTimeout(r, 360)); });
    assert(!document.body.querySelector(".ap-body pre"), "the drawer closed");
  };

  // A board that predates the library: the section is not shown at all.
  for (const status of [404, 501]) {
    listFails = () => new ApiError("not here", status);
    await mount(true);
    assert(!/Workspace skills/.test(host.textContent ?? "") && host.textContent === "", `a ${status} list hides the section`);
  }
  // The whole tab on such a board: a profile whose role names nothing still
  // says so, as before the library, instead of rendering nothing.
  allowPrivateReads();
  let served: { status: number; body: unknown } = { status: 404, body: { error: { code: "not_found", message: "Not found." } } };
  globalThis.fetch = (async (input: unknown) => {
    assert(String(input).includes("/api/workspace-skills"), `the tab read only the library, not ${String(input)}`);
    return new Response(JSON.stringify(served.body), { status: served.status, headers: { "Content-Type": "application/json" } });
  }) as typeof fetch;
  const tab = async () => {
    const c = {
      hired: { id: "ap_1", alias: "ads-agent" }, role: null, roleRead: "read", writable: false, hostedBoard: true, toast,
      doc: { kind: "hosted", read: "ready", profile: stored, draft: null, retry: () => {}, saveSkills: async () => ({ kind: "busy" }) },
    } as unknown as TabContext;
    await React.act(async () => { root.unmount(); });
    root = createRoot(host);
    await React.act(async () => { root.render(React.createElement(AgentTabBody, { tab: "skills", c })); });
    await flush();
    return host.textContent ?? "";
  };
  for (const status of [404, 501]) {
    served = { status, body: { error: { code: status === 404 ? "not_found" : "not_implemented", message: "Not here." } } };
    const text = await tab();
    assert(text === "No skills are recorded for this agent.", `a ${status} library leaves the tab's own empty note: ${text}`);
  }
  served = { status: 200, body: { skills: [] } };
  const listed = await tab();
  assert(/Workspace skills/.test(listed) && /No workspace skills yet/.test(listed), "a served library shows the section");

  // A read failure says so, and Retry reads again.
  listFails = () => new ApiError("unavailable", 503);
  await mount(false);
  assert(/Workspace skills couldn’t be read/.test(all()), "a failed read says so");
  listFails = null;
  await click(button("Retry"), "a Retry button");
  assert(row("weekly-report"), "Retry reads the library");
  // An empty library.
  library = [];
  await mount(false);
  assert(/No workspace skills yet\. Agents draft them; an owner approves them\./.test(all()), "the empty library explains itself");
  reset();

  // ---- member: everything visible, nothing changeable ----
  await mount(false);
  assert(/Approved rev 1/.test(row("weekly-report")?.textContent ?? ""), "an approved skill says so");
  assert(/Approved rev 1 · Draft rev 2 awaiting approval/.test(row("tone-guide")?.textContent ?? ""), "a newer draft of an approved skill says both");
  assert(/Draft rev 1 awaiting approval/.test(row("pricing-check")?.textContent ?? ""), "a draft says it awaits approval");
  assert(host.querySelectorAll("input[type=checkbox]").length === 0, "a member gets no toggles");
  assert(/Assigned/.test(host.querySelector(".ap-skill-assign")?.textContent ?? ""), "a member sees the assignment read-only");
  assert(/copywriting\s*Not approved in this workspace/.test(all()), "a listed name with no approved skill is named as such");
  await click(row("pricing-check"), "the pricing-check row");
  assert(!button("Approve revision 1") && !button("Delete") && !button("Revoke"), "a member's drawer has no owner actions");
  assert(/Workspace guidance, shown as plain text/.test(all()), "the files are framed as workspace guidance");
  const pre = [...document.body.querySelectorAll("pre.ap-code")];
  assert(pre.length === 3 && /SKILL\.md/.test(pre[0]!.previousElementSibling?.textContent ?? ""), "SKILL.md comes first, then the other files");
  assert(pre[0]!.textContent!.includes(HOSTILE) && !document.body.querySelector(".ap-body img") && !document.body.querySelector(".ap-body strong"), "skill text renders as plain text, never HTML or Markdown");
  assert(/Authoragent:ads-agent/.test(all()) && /Saved byusr_member/.test(all()), "the drawer names the author and who saved it");
  await closeDrawer();

  // ---- owner: the toggle saves the profile's skills at its revision ----
  await mount(true);
  const before = stored.revision;
  assert(toggle("weekly-report")?.checked === true && toggle("tone-guide")?.checked === false, "the toggles show the profile's assignments");
  assert(!toggle("pricing-check"), "a draft has no toggle");
  let release = () => {};
  holdWrite = new Promise<void>((resolve) => { release = resolve; });
  const box = toggle("tone-guide")!;
  box.focus();
  await click(box, "the tone-guide toggle");
  assert(!box.disabled && box.getAttribute("aria-disabled") === "true" && document.activeElement === box, "a saving toggle stays focusable and focused");
  holdWrite = null;
  release();
  await flush();
  assert(document.activeElement === toggle("tone-guide"), "the toggle keeps focus after the save");
  const assign = profileWrites[profileWrites.length - 1] as { skills: string[]; expectedRevision: number; responsibilities: string };
  assert(JSON.stringify(assign.skills) === JSON.stringify(["weekly-report", "copywriting", "tone-guide"]), "assigning appends the skill");
  assert(assign.expectedRevision === before && assign.responsibilities === "# Ads", "it saves at the page's revision, keeping the stored text");
  assert(toggle("tone-guide")?.checked === true && /Assigned tone-guide\./.test(lastToast()), "the toggle shows the saved state");
  await click(toggle("weekly-report"), "the weekly-report toggle");
  const unassign = profileWrites[profileWrites.length - 1] as { skills: string[]; expectedRevision: number };
  assert(unassign.expectedRevision === before + 1 && !unassign.skills.includes("weekly-report"), "the next save uses the advanced revision");

  // Someone else's save: a reload prompt, never a retry that overwrites.
  stored = { ...stored, revision: stored.revision + 1, skills: ["copywriting"] };
  const writesBefore = profileWrites.length;
  await click(toggle("tone-guide"), "the tone-guide toggle again");
  assert(profileWrites.length === writesBefore + 1, "a stale revision is written once and not retried");
  assert(/Someone else changed this agent’s profile\./.test(all()) && button("Reload"), "a stale revision asks for a reload");
  assert(host.querySelectorAll("input[type=checkbox]").length === 0, "toggles wait for the reload");
  await click(button("Reload"), "the Reload button");
  assert(document.activeElement?.textContent === "Workspace skills", "Reload hands focus to the section");
  assert(toggle("tone-guide")?.checked === false && toggle("weekly-report")?.checked === false, "the reload shows the board's assignments");

  // ---- owner decisions ----
  await click(row("pricing-check"), "the pricing-check row");
  await click(button("Approve revision 1"), "an Approve button on a draft");
  assert(JSON.stringify(calls[calls.length - 1]) === JSON.stringify(["approve", "pricing-check", 1, "pricing-check-d1"]), "Approve sends the shown revision and its digest");
  assert(/Approved revision 1 of pricing-check\./.test(lastToast()) && !button("Delete") && button("Revoke"), "an approved skill offers Revoke, not Delete");
  assert(focusedId() === "wskill-revoke", "focus moves from Approve to the Revoke that replaced it");
  await click(button("Revoke"), "Revoke on pricing-check");
  assert(focusedId() === "wskill-approve", "focus moves from Revoke to the Approve that replaced it");
  await closeDrawer();

  // A draft newer than the approval: Approve names and sends that revision.
  await click(row("tone-guide"), "the tone-guide row");
  assert(button("Show the approved revision") && button("Revoke") && !button("Delete"), "an approved skill cannot be deleted from the drawer");
  decisionFails = () => new ApiError("Reload the skill and review it again.", 409, { code: "digest_mismatch" });
  await click(button("Approve revision 2"), "Approve on the newer draft");
  assert(JSON.stringify(calls[calls.length - 1]) === JSON.stringify(["approve", "tone-guide", 2, "tone-guide-d2"]), "the newer revision's digest is sent");
  assert(/This skill changed after it was shown\. Review it again before approving\./.test(lastToast()), "digest_mismatch reads plainly");
  decisionFails = () => new ApiError("Revoke an approved skill first.", 409, { code: "approved_limit" });
  await click(button("Approve revision 2"), "Approve again");
  assert(/as many approved skills as it can hold\. Revoke one first\./.test(lastToast()), "approved_limit reads plainly");
  decisionFails = () => new ApiError("Only owners decide workspace skills.", 403, { code: "owner_required" });
  await click(button("Revoke"), "Revoke");
  assert(/Only a workspace owner can decide workspace skills\./.test(lastToast()), "owner_required reads plainly");
  decisionFails = null;
  await closeDrawer();

  // Delete asks first; a skill approved meanwhile is refused in plain words.
  library = [...library, summary("draft-only", 3, null)];
  await mount(true);
  await click(row("draft-only"), "the draft-only row");
  await click(button("Delete"), "Delete on a draft");
  assert(document.body.querySelector(".s-confirm[role=dialog]") && calls[calls.length - 1]![0] !== "delete", "Delete asks before anything is sent");
  decisionFails = () => new ApiError("Revoke the skill before deleting it.", 409, { code: "approved_skill" });
  const confirm = [...document.body.querySelectorAll(".s-confirm button")].find((b) => b.textContent === "Delete");
  await click(confirm, "the confirm Delete button");
  assert(JSON.stringify(calls[calls.length - 1]) === JSON.stringify(["delete", "draft-only", 3]), "Delete sends the latest revision");
  assert(/An approved skill can’t be deleted\. Revoke it first\./.test(lastToast()), "approved_skill reads plainly");
  decisionFails = null;
  await click(button("Delete"), "Delete again");
  await click([...document.body.querySelectorAll(".s-confirm button")].find((b) => b.textContent === "Delete"), "confirm again");
  assert(!row("draft-only") && /Deleted draft-only\./.test(lastToast()), "a deleted skill leaves the list");
  assert(document.activeElement?.textContent === "Workspace skills", "focus lands on the section, not the page");

  await React.act(async () => { root.unmount(); });
  console.log("workspaceSkills checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
