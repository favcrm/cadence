import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import type { WorkspaceSkillRead, WorkspaceSkillSummary, WorkspaceSkillUser } from "../../lib/types";
import { BUNDLED_SKILLS } from "./agentTabModel";

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

/** The status line, in English parts that are each translated. */
export function statusParts(skill: WorkspaceSkillSummary): { text: string; n: number }[] {
  const parts: { text: string; n: number }[] = [];
  if (skill.approved) parts.push({ text: "Approved rev {n}", n: skill.approved.revision });
  if (!skill.approved || skill.latest_revision > skill.approved.revision) {
    parts.push({ text: "Draft rev {n} awaiting approval", n: skill.latest_revision });
  }
  return parts;
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
