import { api } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { ApiError } from "../../lib/apiError";
import type { WorkspaceSkillDraftResult, WorkspaceSkillRead, WorkspaceSkillSummary, WorkspaceSkillUser } from "../../lib/types";
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
  draft?: (name: string, files: Record<string, string>, expectedRevision: number) => Promise<WorkspaceSkillDraftResult>;
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
  draft: async (name, files, expectedRevision) => {
    const saved = await api.draftWorkspaceSkill(name, files, expectedRevision);
    return { ...saved, skill: withUsedBy(saved.skill) };
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
  // Active agents only, as the panel counts them; archived ones are in its Used by tab.
  const n = skill.used_by.filter((user) => user.status !== "archived").length;
  return { text: n === 1 ? "Used by {n} agent" : "Used by {n} agents", n };
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
  skill_limit: "The workspace already has as many skills as it can hold. Delete an unused skill first.",
  skill_refused: "This skill package does not meet the workspace skill rules.",
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

export const SKILL_TAG_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
export const SKILL_REF_LEAF_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
export const SKILL_DESCRIPTION_MAX = 240;
export const SKILL_FILE_BYTES_MAX = 16 * 1024;
export const SKILL_PACKAGE_BYTES_MAX = 32 * 1024;
export const SKILL_REFERENCES_MAX = 15;

export interface DraftReference {
  name: string;
  content: string;
}

const utf8Encoder = new TextEncoder();
export const utf8ByteLength = (text: string): number => utf8Encoder.encode(text).byteLength;

function hasControlChar(text: string): boolean {
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i);
    if (code <= 0x1f || (code >= 0x7f && code <= 0x9f)) return true;
  }
  return false;
}

export function validateSkillName(raw: string): string | null {
  const name = raw.trim();
  if (name === "") return "Enter a skill name.";
  if (!SKILL_TAG_RE.test(name)) return "Use lowercase letters, digits and hyphens (max 32 characters).";
  return null;
}

/** Read the single-line `description:` from `SKILL.md` frontmatter (matching `app-skills.ts`). */
export function extractDescription(skillMd: string): string {
  const lines = skillMd.replace(/\r\n?/g, "\n").split("\n");
  if (lines[0]?.trim() !== "---") return "";
  const end = lines.indexOf("---", 1);
  if (end < 0) return "";
  const front = lines.slice(1, end);
  const at = front.findIndex((line) => /^description\s*:/.test(line));
  if (at < 0) return "";
  let value = front[at]!.replace(/^description\s*:/, "").trim();
  if (/^[>|][+-]?$/.test(value)) {
    const folded: string[] = [];
    for (const line of front.slice(at + 1)) {
      if (line.length > 0 && !/^\s/.test(line)) break;
      folded.push(line.trim());
    }
    value = folded.join(" ");
  }
  const quoted = /^(["'])(.*)\1$/.exec(value);
  if (quoted) value = quoted[2]!;
  return value.replace(/\s+/g, " ").trim();
}

export function composeSkillMd(description: string, body: string): string {
  return `---\ndescription: ${description.trim()}\n---\n\n${body.trim()}\n`;
}

/** Reference members of a revision for editing (`references/<leaf>` only; excludes `SKILL.md` and `skill.json`). */
export function editableReferences(files: Record<string, string>): DraftReference[] {
  return Object.entries(files)
    .filter(([path]) => path.startsWith("references/"))
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([path, content]) => ({ name: path.slice("references/".length), content }));
}

export function buildDraftFiles(input: {
  description: string;
  body: string;
  references: DraftReference[];
}): { ok: true; files: Record<string, string>; bytes: number } | { ok: false; error: string } {
  const description = input.description.trim();
  if (description === "") return { ok: false, error: "Enter a one-line description." };
  if (hasControlChar(description)) return { ok: false, error: "Description must be a single plain line." };
  if (description.length > SKILL_DESCRIPTION_MAX) return { ok: false, error: "Description must be 240 characters or fewer." };
  const body = input.body.trim();
  if (body === "") return { ok: false, error: "Enter skill instructions." };
  const skillMd = composeSkillMd(description, body);
  const skillBytes = utf8ByteLength(skillMd);
  if (skillBytes > SKILL_FILE_BYTES_MAX) return { ok: false, error: "SKILL.md exceeds the 16 KiB file limit." };
  if (input.references.length > SKILL_REFERENCES_MAX) return { ok: false, error: "A skill can have at most 15 reference files." };

  const files: Record<string, string> = { "SKILL.md": skillMd };
  const seen = new Set<string>();
  let totalBytes = skillBytes;
  for (const ref of input.references) {
    const leaf = ref.name.trim();
    if (!SKILL_REF_LEAF_RE.test(leaf)) {
      return { ok: false, error: "Reference names must be plain filenames (letters, digits, ., _, -)." };
    }
    if (seen.has(leaf)) {
      return { ok: false, error: "Reference filenames must be unique." };
    }
    seen.add(leaf);
    const refBytes = utf8ByteLength(ref.content);
    if (refBytes > SKILL_FILE_BYTES_MAX) {
      return { ok: false, error: "Each reference file must be 16 KiB or smaller." };
    }
    totalBytes += refBytes;
    files[`references/${leaf}`] = ref.content;
  }
  if (totalBytes > SKILL_PACKAGE_BYTES_MAX) {
    return { ok: false, error: "Total skill package exceeds the 32 KiB limit." };
  }
  return { ok: true, files, bytes: totalBytes };
}

export function parseUploadedSkillFiles(uploads: readonly { name: string; content: string }[]): {
  description?: string;
  body?: string;
  references: DraftReference[];
  errors: string[];
} {
  let description: string | undefined;
  let body: string | undefined;
  const references: DraftReference[] = [];
  const errors: string[] = [];
  for (const file of uploads) {
    const normalized = file.name.replace(/\\/g, "/").replace(/^\/+/, "");
    const parts = normalized.split("/").filter(Boolean);
    const leaf = parts[parts.length - 1] ?? "";
    if (parts.some((p) => p === "scripts")) {
      errors.push("Workspace skills cannot include scripts.");
      continue;
    }
    if (leaf.toLowerCase() === "skill.json") {
      errors.push("skill.json is generated automatically and does not need to be uploaded.");
      continue;
    }
    if (leaf.toLowerCase() === "skill.md") {
      const desc = extractDescription(file.content);
      if (desc !== "") description = desc;
      body = stripFrontmatter(file.content).trim();
      continue;
    }
    if (!SKILL_REF_LEAF_RE.test(leaf)) {
      errors.push("Reference names must be plain filenames (letters, digits, ., _, -).");
      continue;
    }
    references.push({ name: leaf, content: file.content });
  }
  return {
    ...(description !== undefined ? { description } : {}),
    ...(body !== undefined ? { body } : {}),
    references,
    errors,
  };
}
