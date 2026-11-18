import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import type { WorkspaceSkillRead, WorkspaceSkillSummary } from "../../lib/types";

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

export const liveSkillDeps: SkillDeps = {
  list: async () => (await api.workspaceSkills()).skills,
  read: (name, revision) => api.workspaceSkill(name, revision),
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
