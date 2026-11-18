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
import type * as NavModule from "../screen-ui/src/navigation";
import type * as SkillsModule from "../src/features/agents/workspaceSkills";
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
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof NavModule;
  const { allowPrivateReads } = require("../src/lib/privateSession") as { allowPrivateReads: () => void };
  const { useInstructionsDoc } = require("../src/features/agents/useInstructionsDoc") as typeof HookModule;

  // ---- a fake board: one profile and a three-skill library ----
  let holdWrite: Promise<void> | null = null;
  let stored: StoredAgentProfile = {
    id: "ap_1", name: "ads-agent", role: "Ads", responsibilities: "# Ads", skills: ["weekly-report", "copywriting"], status: "active",
    revision: 4, created_at: 1, updated_at: 2, created_by: "owner", catalogue: null, model: "inherit",
  };
  const profileWrites: Record<string, unknown>[] = [];
  // CAD-2438: another agent's profile, for "Turn off for this agent".
  let copy: StoredAgentProfile = { ...stored, id: "ap_2", name: "copy-agent", skills: ["weekly-report", "tone-guide"], revision: 7 };
  let theirWriteFails: (() => Error) | null = null;
  let theirReadHook: (() => void) | null = null;
  const keepUsedBy = (saved: StoredAgentProfile) => {
    library = library.map((s) =>
      s.used_by === undefined ? s : { ...s, used_by: [...s.used_by.filter((u) => u.profile_id !== saved.id), ...(saved.skills.includes(s.name) ? [{ profile_id: saved.id, name: saved.name, status: saved.status }] : [])] },
    );
  };
  const docDeps: DocDeps = {
    readProfile: async (id) => {
      if (id === "ap_2") {
        theirReadHook?.();
        return copy;
      }
      return stored;
    },
    writeProfile: async (id, body) => {
      profileWrites.push({ ...(body as Record<string, unknown>), id });
      if (holdWrite) await holdWrite;
      const b = body as { expectedRevision: number; skills: string[] };
      if (id === "ap_2") {
        if (theirWriteFails) throw theirWriteFails();
        if (b.expectedRevision !== copy.revision) throw new ApiError("Reload the agent profile.", 409, { code: "revision_conflict" });
        copy = { ...copy, skills: b.skills, revision: copy.revision + 1 };
        keepUsedBy(copy);
        return copy;
      }
      if (b.expectedRevision !== stored.revision) throw new ApiError("Reload the agent profile.", 409, { code: "revision_conflict" });
      stored = { ...stored, skills: b.skills, revision: stored.revision + 1 };
      keepUsedBy(stored);
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
    // pricing-check has no `used_by`: a board that predates the field.
    library[0]!.used_by = [
      { profile_id: "ap_1", name: "ads-agent", status: "active" },
      { profile_id: "ap_2", name: "copy-agent", status: "active" },
      { profile_id: "ap_3", name: "old-agent", status: "archived" },
      ...["f", "e", "d", "c", "b", "a"].map((x) => ({ profile_id: `ap_x${x}`, name: `agent-${x}`, status: "active" as const })),
    ];
    copy = { ...copy, skills: ["weekly-report", "tone-guide"], revision: 7 };
    theirWriteFails = null;
    theirReadHook = null;
    library[1]!.used_by = [{ profile_id: "ap_2", name: "copy-agent", status: "active" }];
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
    return React.createElement(
      NavigationProvider,
      {
        adapter: { navigate: () => {}, href: (h: string) => h, modifiedClicks: "browser" },
        children: React.createElement(WorkspaceSkills, { doc, writable, toast: toast as never, deps: skillDeps, profiles: docDeps }),
      },
    );
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
    // A real click focuses what it presses; a dialog's opener is whatever has focus.
    (el as HTMLElement).focus();
    await React.act(async () => { (el as HTMLElement).click(); });
    await flush();
  };
  const focusedId = (): string => document.activeElement?.id ?? "";
  const row = (name: string) => host.querySelector(`[data-record-id="wskill-${name}"]`);
  const toggle = (name: string) => host.querySelector<HTMLButtonElement>(`button[role=switch][aria-label="On for this agent: ${name}"]`);
  const switches = () => host.querySelectorAll("button[role=switch]");
  const drawer = () => document.body.querySelector("[data-drawer=skill]");
  const tabButton = (label: RegExp) => [...document.body.querySelectorAll<HTMLButtonElement>("[role=tab]")].find((b) => label.test(b.textContent ?? ""));
  const closeDrawer = async () => {
    await React.act(async () => { window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })); });
    // The drawer finishes closing on its transition, or 320 ms without one.
    await React.act(async () => { await new Promise((r) => setTimeout(r, 360)); });
    assert(!drawer(), "the drawer closed");
  };
  const key = async (el: Element | null | undefined, k: string) => {
    assert(el, `an element to press ${k} on`);
    await React.act(async () => { el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true })); });
    await flush();
  };
  const dialog = () => document.body.querySelector<HTMLElement>(".s-confirm[role=dialog]");
  const dialogButton = (label: string) => [...(dialog()?.querySelectorAll("button") ?? [])].find((b) => b.textContent === label) as HTMLButtonElement | undefined;

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

  // ---- CAD-2429: pure reads of used_by and the counts ----
  const W = require("../src/features/agents/workspaceSkills") as typeof SkillsModule;
  const user = { profile_id: "ap_1", name: "a", status: "active" };
  assert(W.readUsedBy(undefined) === undefined && W.readUsedBy("x") === undefined, "a missing or non-list used_by is absent");
  assert(JSON.stringify(W.readUsedBy([user])) === JSON.stringify([user]) && W.readUsedBy([])?.length === 0, "a well-formed list is kept, an empty one too");
  assert(W.readUsedBy([user, { ...user, status: "gone" }]) === undefined, "an unknown status makes the whole field absent");
  assert(W.approvedWhen(Date.UTC(2026, 10, 18, 6, 27)) === "18 Nov 06:27", "an approval time (epoch ms) reads as the board's time");
  assert(W.approvedWhen(undefined) === null && W.approvedWhen(0) === null && W.approvedWhen(Number.NaN) === null, "a missing or invalid approval time shows nothing");
  assert(W.readUsedBy([user, { name: "b", status: "active" }]) === undefined && W.readUsedBy([user, null]) === undefined, "an entry without an id, or not an object, makes it absent");
  assert(W.withUsedBy({ ...summary("x", 1, 1), used_by: [{ nope: 1 }] as never }).used_by === undefined, "a malformed used_by is dropped from the skill");
  assert(W.usedByNote(summary("x", 1, 1)) === null, "no note when the board did not say");
  assert(W.usedByNote({ ...summary("x", 1, 1), used_by: [] })?.n === 0, "zero agents is still said");
  assert(W.usedByNote({ ...summary("x", 1, 1), used_by: [user as never] })?.text === "Used by {n} agent", "one agent is singular");
  assert(W.usedByNote({ ...summary("x", 1, 1), used_by: [user as never, user as never] })?.text === "Used by {n} agents", "two agents are plural");
  assert(W.usedByNote({ ...summary("x", 1, 1), used_by: [user as never, { ...user, profile_id: "p_old", status: "archived" } as never] })?.n === 1, "archived agents are not counted on the row");
  assert(W.isBuiltIn("cadence") && !W.isBuiltIn("weekly-report"), "a bundled skill is built in, a workspace one is not");
  const counted = W.assignedCount(["weekly-report", "copywriting", "pricing-check"], library);
  assert(counted.assigned === 1 && counted.approved === 2, "N counts approved assigned skills, M approved ones in the workspace");

  // ---- CAD-2438: pure helpers ----
  const pills = (x: WorkspaceSkillSummary) => W.skillPills(x).map((p) => `${p.text.replace("{n}", String(p.n))}/${p.tone}`);
  assert(JSON.stringify(pills(summary("x", 3, 3))) === JSON.stringify(["Approved · rev 3/ok"]), "an approved skill has one pill");
  assert(JSON.stringify(pills(summary("x", 4, 3))) === JSON.stringify(["Approved · rev 3/ok", "Draft rev 4 waiting/warn"]), "an approved skill with a newer draft has two");
  assert(JSON.stringify(pills(summary("x", 2, null))) === JSON.stringify(["Draft · rev 2/warn"]), "a draft-only skill has one");
  assert(W.otherRevision(summary("x", 3, 3), 3) === null && W.otherRevision(summary("x", 2, null), 2) === null, "no other revision when approved is latest or nothing is approved");
  assert(W.otherRevision(summary("x", 4, 3), 4)?.kind === "approved" && W.otherRevision(summary("x", 4, 3), 4)?.revision === 3, "showing the draft points to the approved revision");
  assert(W.otherRevision(summary("x", 4, 3), 3)?.kind === "draft" && W.otherRevision(summary("x", 4, 3), 3)?.revision === 4, "showing the approved points to the draft");
  const crowd = [
    { profile_id: "p9", name: "zed", status: "active" }, { profile_id: "p1", name: "Bob", status: "active" }, { profile_id: "p2", name: "amy", status: "active" },
    { profile_id: "p3", name: "old", status: "archived" }, { profile_id: "me", name: "mmm", status: "active" }, { profile_id: "p4", name: "cat", status: "active" },
    { profile_id: "p5", name: "dog", status: "active" }, { profile_id: "p6", name: "eel", status: "active" },
  ] as const;
  const crowdUsers = crowd.map((u) => ({ ...u })) as Parameters<typeof W.filterUsedBy>[0];
  const names = (users: { name: string }[]) => users.map((u) => u.name).join(",");
  assert(JSON.stringify(W.usedByCounts(crowdUsers)) === JSON.stringify({ active: 7, archived: 1, total: 8 }), "counts split active and archived");
  assert(names(W.filterUsedBy(crowdUsers, { query: "", filter: "active", profileId: "me" })) === "mmm,amy,Bob,cat,dog,eel,zed", "active: this agent first, the rest by name");
  assert(names(W.filterUsedBy(crowdUsers, { query: "", filter: "archived", profileId: "me" })) === "old", "archived shows only archived");
  assert(W.filterUsedBy(crowdUsers, { query: "", filter: "all", profileId: "me" }).length === 8, "all shows everyone");
  assert(names(W.filterUsedBy(crowdUsers, { query: " B", filter: "all", profileId: null })) === "Bob", "search ignores case and surrounding space");
  assert(W.filterUsedBy(crowdUsers, { query: "nobody", filter: "all", profileId: null }).length === 0, "no match is empty");
  const summ = W.usedBySummary(crowdUsers, "me");
  assert(summ.shown.length === 5 && summ.shown[0]!.name === "mmm" && summ.more === 2, "the summary shows five active agents, this one first, and how many more");
  assert(W.usedBySummary([], null).more === 0, "no users, nothing more");
  assert(W.stripFrontmatter("---\nname: x\n---\n\n# Title\n") === "# Title\n", "frontmatter and its blank line are dropped");
  assert(W.stripFrontmatter("---\r\nname: x\r\n---\r\nBody") === "Body", "CRLF frontmatter is dropped");
  assert(W.stripFrontmatter("---\n---\nBody") === "Body", "empty frontmatter is dropped");
  assert(W.stripFrontmatter("# No front\n---\nrule") === "# No front\n---\nrule" && W.stripFrontmatter("---\nunclosed\n# x") === "---\nunclosed\n# x", "text without a closed opening block is kept");
  assert(JSON.stringify(W.referenceFiles({ "z.md": "", "SKILL.md": "", "a.md": "" }).map(([p]) => p)) === JSON.stringify(["a.md", "z.md"]), "reference files leave SKILL.md out");
  const fakeProfile = (skills: string[], revision = 2) => ({ ...stored, id: "ap_9", skills, revision });
  let wrote: { skills: string[]; expectedRevision: number } | null = null;
  const offOk = await W.setProfileSkill({ readProfile: async () => fakeProfile(["a", "b"]), writeProfile: async (_i, b) => { wrote = b as never; return fakeProfile([]); } }, "ap_9", "a", false);
  assert(offOk.kind === "saved" && JSON.stringify(wrote) === JSON.stringify({ name: stored.name, role: stored.role, responsibilities: stored.responsibilities, skills: ["b"], status: "active", expectedRevision: 2 }), "turn off saves the other skills at the revision read");
  wrote = null;
  const same = await W.setProfileSkill({ readProfile: async () => fakeProfile(["b"]), writeProfile: async () => { throw new Error("must not write"); } }, "ap_9", "a", false);
  assert(same.kind === "unchanged", "a profile that no longer lists the skill is left alone");
  const stale = await W.setProfileSkill({ readProfile: async () => fakeProfile(["a"]), writeProfile: async () => { throw new ApiError("Reload.", 409, { code: "revision_conflict" }); } }, "ap_9", "a", false);
  assert(stale.kind === "conflict", "a stale revision is a conflict, not retried");
  let writes = 0;
  const lost = await W.setProfileSkill({ readProfile: async () => fakeProfile(["a"], 5), writeProfile: async () => { writes += 1; throw new ApiError("down", 503); } }, "ap_9", "a", false);
  assert(lost.kind === "unconfirmed" && writes === 1, "a dropped answer is unconfirmed");
  let reads = 0;
  const landed = await W.setProfileSkill({ readProfile: async () => (reads++ === 0 ? fakeProfile(["a"], 5) : fakeProfile([], 6)), writeProfile: async () => { throw new ApiError("down", 503); } }, "ap_9", "a", false);
  assert(landed.kind === "saved", "a re-read showing exactly this change confirms a dropped answer");
  const refused = await W.setProfileSkill({ readProfile: async () => fakeProfile(["a"]), writeProfile: async () => { throw new ApiError("Owners only.", 403); } }, "ap_9", "a", false);
  assert(refused.kind === "refused" && refused.message === "Owners only.", "a refusal carries the board's words");

  // ---- member: everything visible, nothing changeable ----
  await mount(false);
  assert(/1 of 2 on/.test(host.querySelector("h3")?.parentElement?.textContent ?? ""), "the header counts skills that are on, of approved");
  assert(/Used by 8 agents/.test(row("weekly-report")?.textContent ?? "") && /Used by 1 agent(?!s)/.test(row("tone-guide")?.textContent ?? ""), "rows say how many agents use them");
  assert(!/Used by/.test(row("pricing-check")?.textContent ?? ""), "a skill without used_by has no note");
  await click(row("weekly-report"), "the weekly-report row");
  assert(drawer()?.getAttribute("aria-label") === "weekly-report" && /Approved · rev 1/.test(drawer()?.querySelector(".crm-drawer-pills")?.textContent ?? ""), "the panel's header names the skill and its status");
  assert(["Overview", "Instructions", "Used by (8)", "Files (2)"].every((label, i) => document.body.querySelectorAll("[role=tab]")[i]?.textContent === label), "the panel has four tabs with counts");
  const summaryBox = document.body.querySelector("[data-work-panel=used-by-summary]");
  assert(/Used by · 8 active/.test(summaryBox?.textContent ?? "") && /and 3 more/.test(summaryBox?.textContent ?? "") && /See all 8/.test(summaryBox?.textContent ?? ""), "the Overview shows five names, how many more and See all");
  assert(!document.body.querySelector("[role=switch]") && /Details/.test(all()) && /Approved revision1 · weekly-repor/.test(all()) && !/usr_owner/.test(all()) && /Approved\d{1,2} [A-Z][a-z]{2} \d{2}:\d{2}/.test(all()), "a member has On/Off text and sees the details");
  assert(/Only the workspace owner can approve, revoke or delete skills\./.test(drawer()?.querySelector("footer")?.textContent ?? "") && !drawer()?.querySelector("footer button"), "a member's footer says who can act and has no buttons");
  await click(button("See all 8"), "See all");
  assert(tabButton(/Used by/)?.getAttribute("aria-selected") === "true", "See all opens the Used by tab");
  const usedBy = document.body.querySelector("[data-work-panel=used-by]");
  assert(/8 active, 1 archived/.test(usedBy?.textContent ?? ""), "the tab counts active and archived");
  assert([...usedBy!.querySelectorAll(".sk-user-name")].map((n) => n.textContent).join(",") === "ads-agent,agent-a,agent-b,agent-c,agent-d,agent-e,agent-f,copy-agent", "this agent is pinned first, the rest by name, archived hidden");
  assert(/ads-agentthis agent/.test(usedBy?.textContent ?? "") && !usedBy?.querySelector("a[href*=ads-agent]"), "this agent is marked, not linked");
  assert(usedBy?.querySelector("a[href='/agents/copy-agent?tab=skills']"), "another agent links to its Skills tab");
  assert(!usedBy?.querySelector("[aria-haspopup=menu]"), "a member has no row menus");
  const search = usedBy!.querySelector<HTMLInputElement>("input[type=search]")!;
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(search, "AGENT-C");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  assert([...document.body.querySelectorAll(".sk-user-name")].map((n) => n.textContent).join(",") === "agent-c", "search narrows the list");
  await click(button("Archived"), "the Archived filter");
  assert(/No agent matches\./.test(document.body.textContent ?? ""), "search plus filter can match nothing");
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(search, "");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  assert(/old-agentarchived/.test(document.body.querySelector("[data-work-panel=used-by]")?.textContent ?? "") && !document.body.querySelector("[data-work-panel=used-by] a[href*=old-agent]"), "an archived agent is noted, not linked");
  await click(button("All"), "the All filter");
  assert(document.body.querySelectorAll(".sk-user").length === 9, "All lists archived agents too");
  // The panel's tabs move with the arrow keys.
  await key(tabButton(/Used by/), "ArrowRight");
  assert(tabButton(/Files/)?.getAttribute("aria-selected") === "true" && document.activeElement === tabButton(/Files/), "ArrowRight moves to the next tab and focuses it");
  await closeDrawer();
  await click(row("pricing-check"), "the pricing-check row, no used_by");
  assert(!document.body.querySelector("[data-work-panel=used-by]") && !tabButton(/Used by/) && !document.body.querySelector("[data-work-panel=used-by-summary]"), "Used by is hidden everywhere when the board omits used_by");
  await closeDrawer();
  // An empty list is said, not hidden.
  library = library.map((s) => (s.name === "tone-guide" ? { ...s, used_by: [] } : s));
  await mount(false);
  await click(row("tone-guide"), "the tone-guide row, empty used_by");
  assert(/Used by · 0 active/.test(document.body.textContent ?? "") && /No agent lists this skill yet/.test(document.body.textContent ?? ""), "an empty used_by says so");
  await closeDrawer();
  reset();
  // A bundled name on the profile reads Built-in; an unknown one keeps its words.
  const keep = stored;
  stored = { ...stored, skills: [...stored.skills, "cadence"] };
  await mount(false);
  assert(/cadence\s*Built-in/.test(all()) && /copywriting\s*Not approved in this workspace/.test(all()), "a bundled name is Built-in, an unknown one is not approved");
  assert(/1 of 2 on/.test(all()), "a built-in name does not count as an assigned workspace skill");
  stored = keep;
  await mount(false);
  assert(/Approved · rev 1/.test(row("weekly-report")?.textContent ?? ""), "an approved skill says so");
  assert(/Approved · rev 1Draft rev 2 waiting/.test(row("tone-guide")?.textContent ?? ""), "a newer draft of an approved skill says both");
  assert(/Draft · rev 1/.test(row("pricing-check")?.textContent ?? ""), "a draft says so");
  assert(row("weekly-report")?.textContent?.includes("Synthetic weekly-report guidance."), "a card carries the description");
  assert(switches().length === 0, "a member gets no switches");
  const states = [...host.querySelectorAll(".sk-switch")].map((n) => n.textContent);
  assert(JSON.stringify(states) === JSON.stringify(["On", "Off", "Off"]), `a member sees On and Off as text: ${states}`);
  assert(/copywriting\s*Not approved in this workspace/.test(all()), "a listed name with no approved skill is named as such");
  await click(row("pricing-check"), "the pricing-check row");
  assert(!button("Approve revision 1") && !button("Delete") && !button("Revoke"), "a member's drawer has no owner actions");
  assert(/Authoragent:ads-agent/.test(all()), "the Overview names the author");
  await click(tabButton(/Instructions/), "the Instructions tab");
  assert(/Workspace guidance/.test(all()) && /SKILL\.md/.test(all()), "the file is framed as workspace guidance");
  const preview = document.body.querySelector("[data-view=preview]");
  assert(preview && !document.body.querySelector("[data-view=source]"), "Preview shows alone");
  assert(!/description: x/.test(preview!.textContent ?? "") && !(preview!.textContent ?? "").startsWith("---"), "the frontmatter is not in Preview");
  assert(!document.body.querySelector(".crm-drawer-body img") && !document.body.querySelector(".crm-drawer-body [onerror]") && preview!.querySelector("strong")?.textContent === "not bold", "Preview renders Markdown, never HTML");
  await click(button("Source"), "Source");
  assert(!document.body.querySelector("[data-view=preview]") && document.body.querySelector("[data-view=source]")?.textContent === `---\ndescription: x\n---\n${HOSTILE}`, "Source replaces Preview and is the exact text");
  assert(button("Source")?.getAttribute("aria-pressed") === "true" && button("Preview")?.getAttribute("aria-pressed") === "false", "the segment says which is shown");
  await click(button("Preview"), "Preview");
  assert(document.body.querySelector("[data-view=preview]") && !document.body.querySelector("[data-view=source]"), "Preview replaces Source");
  await click(tabButton(/Files/), "the Files tab");
  const cards = [...document.body.querySelectorAll("details.sk-file-card")];
  assert(cards.length === 2 && /references\/b\.md/.test(cards[0]!.textContent ?? "") && /skill\.json/.test(cards[1]!.textContent ?? ""), "Files lists each reference, then skill.json");
  assert(/Reference text/.test(cards[0]!.textContent ?? "") && cards[1]!.querySelector("pre") && /generated/.test(cards[1]!.textContent ?? ""), "a reference is rendered, skill.json is plain");
  await closeDrawer();

  // ---- owner: the toggle saves the profile's skills at its revision ----
  await mount(true);
  const before = stored.revision;
  const checked = (name: string) => toggle(name)?.getAttribute("aria-checked");
  assert(checked("weekly-report") === "true" && checked("tone-guide") === "false", "the switches show the profile's assignments");
  assert(toggle("pricing-check")?.disabled === true && /Approve first/.test(row("pricing-check")?.parentElement?.textContent ?? ""), "a draft has a disabled switch and says why");
  assert(/On/.test(row("weekly-report")?.parentElement?.querySelector(".sk-switch")?.textContent ?? ""), "a row says On beside its switch");
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
  assert(checked("tone-guide") === "true" && /Turned on tone-guide\./.test(lastToast()), "the switch shows the saved state");
  await click(toggle("weekly-report"), "the weekly-report toggle");
  const unassign = profileWrites[profileWrites.length - 1] as { skills: string[]; expectedRevision: number };
  assert(unassign.expectedRevision === before + 1 && !unassign.skills.includes("weekly-report"), "the next save uses the advanced revision");

  // Someone else's save: a reload prompt, never a retry that overwrites.
  stored = { ...stored, revision: stored.revision + 1, skills: ["copywriting"] };
  const writesBefore = profileWrites.length;
  await click(toggle("tone-guide"), "the tone-guide toggle again");
  assert(profileWrites.length === writesBefore + 1, "a stale revision is written once and not retried");
  assert(/Someone else changed this agent’s profile\./.test(all()) && button("Reload"), "a stale revision asks for a reload");
  assert([...switches()].every((b) => (b as HTMLButtonElement).disabled), "switches wait for the reload");
  await click(button("Reload"), "the Reload button");
  assert(document.activeElement?.textContent === "Workspace skills", "Reload hands focus to the section");
  assert(checked("tone-guide") === "false" && checked("weekly-report") === "false", "the reload shows the board's assignments");

  // ---- owner decisions ----
  await click(row("pricing-check"), "the pricing-check row");
  await click(button("Approve revision 1"), "an Approve button on a draft");
  assert(JSON.stringify(calls[calls.length - 1]) === JSON.stringify(["approve", "pricing-check", 1, "pricing-check-d1"]), "Approve sends the shown revision and its digest");
  assert(/Approved revision 1 of pricing-check\./.test(lastToast()) && !button("Delete") && button("Revoke"), "an approved skill offers Revoke, not Delete");
  assert(focusedId() === "wskill-revoke", "focus moves from Approve to the Revoke that replaced it");
  await click(button("Revoke"), "Revoke on pricing-check");
  assert(dialog() && /Revoke pricing-check\?/.test(dialog()!.textContent ?? "") && /Every agent that lists this skill loses it/.test(dialog()!.textContent ?? ""), "Revoke asks first, and without a used_by says every agent");
  assert(calls[calls.length - 1]![0] !== "revoke", "nothing is revoked before the confirm");
  await click(dialogButton("Revoke"), "the confirm Revoke button");
  assert(calls[calls.length - 1]![0] === "revoke" && !dialog(), "the confirm revokes and closes");
  assert(focusedId() === "wskill-approve", "focus moves from Revoke to the Approve that replaced it");
  await closeDrawer();

  // A draft newer than the approval: Approve names and sends that revision.
  await click(row("tone-guide"), "the tone-guide row");
  assert(/Showing draft revision/.test(drawer()?.querySelector(".crm-drawer-pills")?.textContent ?? "") && button("see approved") && button("Revoke") && !button("Delete"), "a newer draft is flagged in the header and an approved skill cannot be deleted from the drawer");
  await click(button("see approved"), "see approved");
  assert(/Showing approved revision/.test(drawer()?.querySelector(".crm-drawer-pills")?.textContent ?? "") && button("see draft") && !button("Approve revision 1"), "the approved revision shows with a way back to the draft");
  await click(button("see draft"), "see draft");
  decisionFails = () => new ApiError("Reload the skill and review it again.", 409, { code: "digest_mismatch" });
  await click(button("Approve revision 2"), "Approve on the newer draft");
  assert(JSON.stringify(calls[calls.length - 1]) === JSON.stringify(["approve", "tone-guide", 2, "tone-guide-d2"]), "the newer revision's digest is sent");
  assert(/This skill changed after it was shown\. Review it again before approving\./.test(lastToast()), "digest_mismatch reads plainly");
  decisionFails = () => new ApiError("Revoke an approved skill first.", 409, { code: "approved_limit" });
  await click(button("Approve revision 2"), "Approve again");
  assert(/as many approved skills as it can hold\. Revoke one first\./.test(lastToast()), "approved_limit reads plainly");
  decisionFails = () => new ApiError("Only owners decide workspace skills.", 403, { code: "owner_required" });
  await click(button("Revoke"), "Revoke");
  await click(dialogButton("Revoke"), "the confirm Revoke button");
  assert(/Only a workspace owner can decide workspace skills\./.test(lastToast()) && !dialog(), "owner_required reads plainly");
  decisionFails = null;
  await closeDrawer();

  // Delete asks first; a skill approved meanwhile is refused in plain words.
  library = [...library, summary("draft-only", 3, null)];
  await mount(true);
  await click(row("draft-only"), "the draft-only row");
  await click(button("Delete"), "Delete on a draft");
  assert(dialog() && calls[calls.length - 1]![0] !== "delete", "Delete asks before anything is sent");
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

  // ---- CAD-2438: the panel's own switch, Revoke's count, and Turn off for one agent ----
  reset();
  stored = { ...stored, skills: ["weekly-report"], revision: 20 };
  await mount(true);
  await click(row("pricing-check"), "the pricing-check row");
  assert(/Approve first/.test(drawer()?.textContent ?? "") && !drawer()?.querySelector("[role=switch]"), "a draft's panel says Approve first and has no switch");
  await closeDrawer();
  await click(row("weekly-report"), "the weekly-report row");
  const panelSwitch = () => drawer()?.querySelector<HTMLButtonElement>("button[role=switch]");
  assert(panelSwitch()?.getAttribute("aria-label") === "On for ads-agent" && panelSwitch()?.getAttribute("aria-checked") === "true", "the panel has an On for <agent> switch");
  await click(panelSwitch(), "the panel switch");
  assert((profileWrites[profileWrites.length - 1] as { skills: string[] }).skills.length === 0 && panelSwitch()?.getAttribute("aria-checked") === "false", "the panel's switch saves the profile");
  await click(panelSwitch(), "the panel switch again");
  assert(panelSwitch()?.getAttribute("aria-checked") === "true", "and turns it back on");
  // Revoke says how many active agents lose the skill, and Escape cancels it.
  const revokesBefore = calls.filter((c) => c[0] === "revoke").length;
  await click(button("Revoke"), "Revoke on weekly-report");
  assert(/8 active agents lose this skill/.test(dialog()?.textContent ?? ""), "the Revoke confirm shows the active count");
  await key(dialog(), "Escape");
  assert(!dialog() && drawer() && calls.filter((c) => c[0] === "revoke").length === revokesBefore, "Escape cancels the confirm and leaves the panel open");
  assert(document.activeElement === button("Revoke"), "focus returns to the Revoke button");

  // The row menu: arrows, Escape returns focus, and the drawer stays open.
  await click(tabButton(/Used by/), "the Used by tab");
  const menuTrigger = (agent: string) => document.body.querySelector<HTMLButtonElement>(`button[aria-label="Actions for ${agent}"]`);
  const items = () => [...document.body.querySelectorAll<HTMLElement>("[role=menuitem]")];
  assert(menuTrigger("agent-a")?.getAttribute("aria-expanded") === "false" && items().length === 0, "a row menu starts closed");
  await click(menuTrigger("agent-a"), "the agent-a menu");
  assert(menuTrigger("agent-a")?.getAttribute("aria-expanded") === "true" && items().map((i) => i.textContent).join("|").startsWith("Open agent") && /Turn off for this agent/.test(items()[1]?.textContent ?? ""), "the menu offers Open agent, then Turn off");
  assert(document.activeElement === items()[0], "the menu opens on its first item");
  await key(items()[0], "ArrowDown");
  assert(document.activeElement === items()[1], "ArrowDown moves to Turn off");
  await key(items()[1], "ArrowDown");
  assert(document.activeElement === items()[0], "ArrowDown wraps");
  await key(items()[0], "ArrowUp");
  assert(document.activeElement === items()[1], "ArrowUp wraps back");
  await key(items()[1], "Escape");
  assert(items().length === 0 && document.activeElement === menuTrigger("agent-a") && drawer(), "Escape closes the menu, returns focus to its trigger and keeps the panel");
  await click(menuTrigger("agent-a"), "the agent-a menu again");
  await click(items()[1], "Turn off for this agent");
  assert(dialog() && /Turn off weekly-report for agent-a\?/.test(dialog()!.textContent ?? "") && /profile, not the skill/.test(dialog()!.textContent ?? ""), "Turn off asks first and says what changes");
  assert(document.activeElement === dialog(), "the dialog takes focus");
  const lastBtn = dialogButton("Turn off")!;
  lastBtn.focus();
  await key(lastBtn, "Tab");
  assert(document.activeElement === dialogButton("Cancel"), "Tab wraps inside the dialog");
  await key(dialogButton("Cancel"), "Escape");
  assert(!dialog() && drawer() && document.activeElement === menuTrigger("agent-a"), "Escape cancels the dialog and returns focus to the menu's trigger");
  const writesBeforeOff = profileWrites.length;
  assert(profileWrites.length === writesBeforeOff, "a cancelled dialog writes nothing");
  // The owner's own row has no Open agent.
  await click(menuTrigger("ads-agent"), "the ads-agent menu");
  assert(items().length === 1 && /Turn off for this agent/.test(items()[0]!.textContent ?? ""), "this agent's menu only turns the skill off");
  await key(items()[0], "Escape");

  // Turn off for copy-agent: its profile is read, then saved at its revision.
  await click(menuTrigger("copy-agent"), "the copy-agent menu");
  await click(items()[1], "Turn off for this agent");
  await click(dialogButton("Turn off"), "the confirm Turn off button");
  const off = profileWrites[profileWrites.length - 1] as { id: string; skills: string[]; expectedRevision: number };
  assert(off.id === "ap_2" && JSON.stringify(off.skills) === JSON.stringify(["tone-guide"]) && off.expectedRevision === 7, "it saves copy-agent's own skills without the name, at its own revision");
  assert(!dialog() && /Turned off weekly-report for copy-agent\./.test(host.querySelector("[role=status].fixed")?.textContent ?? "") && button("Undo"), "a toast confirms it and offers Undo");
  assert(![...document.body.querySelectorAll(".sk-user-name")].some((n) => n.textContent === "copy-agent"), "the agent leaves the Used by list");
  assert(document.activeElement === tabButton(/Used by/), "focus lands on the tab the row left");
  await click(button("Undo"), "Undo");
  const back = profileWrites[profileWrites.length - 1] as { id: string; skills: string[]; expectedRevision: number };
  assert(back.id === "ap_2" && back.skills.includes("weekly-report") && back.expectedRevision === 8, "Undo turns it back on at the revision now stored");
  assert([...document.body.querySelectorAll(".sk-user-name")].some((n) => n.textContent === "copy-agent") && !host.querySelector("[role=status].fixed"), "the agent is listed again and the toast is gone");
  // A stale revision is reported, never overwritten.
  theirWriteFails = () => new ApiError("Reload the agent profile.", 409, { code: "revision_conflict" });
  const writesBeforeStale = profileWrites.length;
  await click(menuTrigger("copy-agent"), "the copy-agent menu");
  await click(items()[1], "Turn off for this agent");
  await click(dialogButton("Turn off"), "the confirm Turn off button");
  assert(profileWrites.length === writesBeforeStale + 1, "a stale revision is written once, not retried");
  assert(/Someone else changed copy-agent’s profile, so nothing was saved\./.test(drawer()?.textContent ?? "") && !dialog(), "the panel says someone else changed it");
  assert([...document.body.querySelectorAll(".sk-user-name")].some((n) => n.textContent === "copy-agent"), "nothing changed in the list");
  theirWriteFails = null;
  await click([...(drawer()?.querySelectorAll("button") ?? [])].find((b) => b.textContent === "Reload"), "the panel's Reload");
  assert(!/Someone else changed copy-agent/.test(drawer()?.textContent ?? ""), "Reload clears the notice");
  theirWriteFails = () => new ApiError("Owners only.", 403);
  await click(menuTrigger("copy-agent"), "the copy-agent menu");
  await click(items()[1], "Turn off for this agent");
  await click(dialogButton("Turn off"), "the confirm Turn off button");
  assert(toasts[toasts.length - 1]![0] === "err" && /Owners only\./.test(lastToast()), "a refusal reads as the board's words");
  theirWriteFails = null;
  // This agent's own row saves through the page's profile.
  await click(menuTrigger("ads-agent"), "the ads-agent menu");
  await click(items()[0], "Turn off for this agent");
  await click(dialogButton("Turn off"), "the confirm Turn off button");
  const own = profileWrites[profileWrites.length - 1] as { id: string; skills: string[]; expectedRevision: number };
  assert(own.id === "ap_1" && own.skills.length === 0 && own.expectedRevision === stored.revision - 1, "this agent's skills are saved at the page's revision");
  await click(button("Undo"), "Undo on this agent");
  assert((profileWrites[profileWrites.length - 1] as { skills: string[] }).skills.includes("weekly-report"), "Undo turns it back on for this agent");
  await closeDrawer();

  await React.act(async () => { root.unmount(); });
  console.log("workspaceSkills checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
