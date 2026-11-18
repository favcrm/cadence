import { api } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { ApiError } from "../../lib/apiError";
import type { WorkspaceSkillRead, WorkspaceSkillSummary, WorkspaceSkillUser } from "../../lib/types";
import { BUNDLED_SKILLS } from "./agentTabModel";
import { classifyFailure, profileBody, type DocDeps } from "./instructionsDoc";

/**
 * CAD-2419: the hosted workspace skill library on an agent's Skills tab
 * (agenticos-v2 AOS-626). Any member reads it; approving one revision by its
 * digest, revoking and deleting are the owner's. Which agents read an approved
 * skill is the profile's own `skills` list, saved through the profile edit.
 * The calls are `SkillDeps`, so a test drives every answer without a board.
 */
export interface SkillDeps {
  list: () => Promise<WorkspaceSkillSummary[]>;
  read: (name: string, revision?: number) => Promise<WorkspaceSkillRead>;
  approve: (name: string, revision: number, digest: string) => Promise<WorkspaceSkillSummary>;
  revoke: (name: string, digest: string) => Promise<WorkspaceSkillSummary>;
  remove: (name: string, expectedRevision: number) => Promise<void>;
}

/**
 * CAD-2429: `used_by` is read strictly. A board that predates it omits it; a
 * malformed one (not a list, or any entry without a string id and name and a
 * known status) is treated as absent, never repaired into a count.
 */
export function readUsedBy(value: unknown): WorkspaceSkillUser[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const users: WorkspaceSkillUser[] = [];
  for (const entry of value as unknown[]) {
    if (!entry || typeof entry !== "object") return undefined;
    const { profile_id, name, status } = entry as Record<string, unknown>;
    if (typeof profile_id !== "string" || typeof name !== "string" || (status !== "active" && status !== "archived")) return undefined;
    users.push({ profile_id, name, status });
  }
  return users;
}

/** A skill as the board answered it, with `used_by` kept only when it is well-formed. */
export function withUsedBy(skill: WorkspaceSkillSummary): WorkspaceSkillSummary {
  const { used_by, ...rest } = skill;
  const users = readUsedBy(used_by);
  return users === undefined ? rest : { ...rest, used_by: users };
}

export const liveSkillDeps: SkillDeps = {
  list: async () => (await api.workspaceSkills()).skills.map(withUsedBy),
  read: async (name, revision) => {
    const read = await api.workspaceSkill(name, revision);
    return { ...read, skill: withUsedBy(read.skill) };
  },
  approve: async (name, revision, digest) => (await api.approveWorkspaceSkill(name, revision, digest)).skill,
  revoke: async (name, digest) => (await api.revokeWorkspaceSkill(name, digest)).skill,
  remove: async (name, expectedRevision) => {
    await api.deleteWorkspaceSkill(name, expectedRevision);
  },
};

/** A board that predates the library answers the list with 404 or 501. */
export function libraryAbsent(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 404 || error.status === 501);
}

/** A status pill of a skill: English text with `{n}`, and its tone. */
export interface SkillPill {
  text: string;
  n: number;
  tone: "ok" | "warn";
}

/** "Approved · rev N", "Draft rev N waiting" beside it, or "Draft · rev N" when nothing is approved. */
export function skillPills(skill: WorkspaceSkillSummary): SkillPill[] {
  if (!skill.approved) return [{ text: "Draft · rev {n}", n: skill.latest_revision, tone: "warn" }];
  const pills: SkillPill[] = [{ text: "Approved · rev {n}", n: skill.approved.revision, tone: "ok" }];
  if (skill.latest_revision > skill.approved.revision) pills.push({ text: "Draft rev {n} waiting", n: skill.latest_revision, tone: "warn" });
  return pills;
}

/** The revision to offer instead of the one shown, when the approved and the latest differ; null otherwise. */
export function otherRevision(skill: WorkspaceSkillSummary, shown: number): { kind: "approved" | "draft"; revision: number } | null {
  const approved = skill.approved;
  if (!approved || approved.revision === skill.latest_revision) return null;
  return shown === approved.revision ? { kind: "draft", revision: skill.latest_revision } : { kind: "approved", revision: approved.revision };
}

export type UsedByFilter = "active" | "all" | "archived";

/** Active, archived and total agents of a skill's `used_by`. */
export function usedByCounts(users: WorkspaceSkillUser[]): { active: number; archived: number; total: number } {
  const archived = users.filter((user) => user.status === "archived").length;
  return { active: users.length - archived, archived, total: users.length };
}

/** The Used by list: the filter and a case-insensitive name search, this agent first, the rest by name. */
export function filterUsedBy(users: WorkspaceSkillUser[], opts: { query: string; filter: UsedByFilter; profileId: string | null }): WorkspaceSkillUser[] {
  const query = opts.query.trim().toLowerCase();
  const kept = users.filter(
    (user) => (opts.filter === "all" || user.status === opts.filter) && user.name.toLowerCase().includes(query),
  );
  const mine = (user: WorkspaceSkillUser) => user.profile_id === opts.profileId;
  return [...kept.filter(mine), ...kept.filter((user) => !mine(user)).sort((a, b) => a.name.localeCompare(b.name))];
}

/** The Overview's Used by: the first five active agents (this one first) and how many more there are. */
export function usedBySummary(users: WorkspaceSkillUser[], profileId: string | null): { shown: WorkspaceSkillUser[]; more: number } {
  const active = filterUsedBy(users, { query: "", filter: "active", profileId });
  return { shown: active.slice(0, 5), more: Math.max(0, active.length - 5) };
}

/** SKILL.md without its YAML frontmatter, for the rendered view only. Text that
 *  does not open with a closed `---` block is returned as it is. */
export function stripFrontmatter(text: string): string {
  const match = /^---[ \t]*\r?\n(?:[\s\S]*?\r?\n)?---[ \t]*(?:\r?\n|$)/.exec(text);
  return match ? text.slice(match[0].length).replace(/^(?:[ \t]*\r?\n)+/, "") : text;
}

/** Files shown on the Files tab: everything but SKILL.md. */
export function referenceFiles(files: Record<string, string>): [string, string][] {
  return skillFiles(files).filter(([path]) => path !== "SKILL.md");
}

/** What turning a skill off or on in one agent's profile came to. */
export type ProfileSkillOutcome =
  | { kind: "saved" | "unchanged" | "conflict" | "unconfirmed" }
  | { kind: "refused"; message: string };

/**
 * Turn `name` off (or on) in another agent's profile: read it, then save its
 * `skills` at the revision just read. A stale revision (409) is reported, never
 * retried, so nothing is overwritten. A lost answer counts as saved only when a
 * re-read shows exactly this change on top of the revision written at.
 */
export async function setProfileSkill(deps: Pick<DocDeps, "readProfile" | "writeProfile">, profileId: string, name: string, on: boolean): Promise<ProfileSkillOutcome> {
  let base;
  try {
    base = await deps.readProfile(profileId);
  } catch (error) {
    const out = classifyFailure(error, "hosted");
    return out === "refused" ? { kind: "refused", message: error instanceof Error ? error.message : String(error) } : { kind: out };
  }
  if (base.skills.includes(name) === on) return { kind: "unchanged" };
  const skills = on ? [...base.skills, name] : base.skills.filter((n) => n !== name);
  try {
    await deps.writeProfile(base.id, profileBody(base, base.responsibilities, base.revision, skills));
    return { kind: "saved" };
  } catch (error) {
    const out = classifyFailure(error, "hosted");
    if (out === "refused") return { kind: "refused", message: error instanceof Error ? error.message : String(error) };
    if (out === "unconfirmed") {
      const latest = await deps.readProfile(base.id).catch(() => null);
      if (latest && latest.revision === base.revision + 1 && latest.skills.join("\n") === skills.join("\n")) return { kind: "saved" };
    }
    return { kind: out };
  }
}

/** A skill Cadence bundles with every agent; it needs no workspace approval. */
export function isBuiltIn(name: string): boolean {
  return BUNDLED_SKILLS.some((skill) => skill.name === name);
}

/** "N of M": approved workspace skills this profile lists, of all approved ones. */
export function assignedCount(profileSkills: string[], library: WorkspaceSkillSummary[]): { assigned: number; approved: number } {
  const approved = library.filter((skill) => skill.approved);
  return { assigned: approved.filter((skill) => profileSkills.includes(skill.name)).length, approved: approved.length };
}

/** "Used by N agents" for a row, in English for `t`; null when the board did not say. */
export function usedByNote(skill: WorkspaceSkillSummary): { text: string; n: number } | null {
  if (skill.used_by === undefined) return null;
  return { text: skill.used_by.length === 1 ? "Used by {n} agent" : "Used by {n} agents", n: skill.used_by.length };
}

/** Names the profile lists that no approved workspace skill answers. */
export function unapprovedNames(profileSkills: string[], library: WorkspaceSkillSummary[]): string[] {
  const approved = new Set(library.filter((skill) => skill.approved).map((skill) => skill.name));
  return profileSkills.filter((name) => !approved.has(name));
}

const REFUSALS: Record<string, string> = {
  owner_required: "Only a workspace owner can decide workspace skills.",
  digest_mismatch: "This skill changed after it was shown. Review it again before approving.",
  revision_conflict: "This skill has a newer revision. Review it again, then try once more.",
  approved_limit: "The workspace already has as many approved skills as it can hold. Revoke one first.",
  approved_skill: "An approved skill can’t be deleted. Revoke it first.",
  not_found: "This skill no longer exists.",
};

/** How a refused or failed decision reads, in English for `t`. `unconfirmed`:
 *  no answer arrived, so the decision may have landed. */
export function refusal(error: unknown): { text: string; unconfirmed: boolean; code: string | null } {
  if (!(error instanceof ApiError) || error.status === 0 || error.status >= 500) {
    return { text: "Not confirmed: the connection dropped. Check the skill before trying again.", unconfirmed: true, code: null };
  }
  const code = error.code ?? null;
  return { text: (code && REFUSALS[code]) || error.message, unconfirmed: false, code };
}

/** SKILL.md first, then the other files by path. */
export function skillFiles(files: Record<string, string>): [string, string][] {
  return Object.entries(files).sort(([a], [b]) => (a === "SKILL.md" ? -1 : b === "SKILL.md" ? 1 : a.localeCompare(b)));
}

/** When a revision was approved (`at` is epoch milliseconds), as the board shows times; null when unknown. */
export function approvedWhen(at: number | undefined): string | null {
  if (typeof at !== "number" || !Number.isFinite(at) || at <= 0) return null;
  return fmtTime(new Date(at).toISOString());
}
