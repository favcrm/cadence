/**
 * CAD-2291: the QA board's fixture payloads, checked with the UI's own client
 * code — the parsers and loaders the board UI runs on these routes — so a
 * payload the UI would refuse stops `qa-board up` instead of being served as
 * a drifted fake. `up` is its only runner: no ui test runs it (CAD-2291
 * dropped that test), and `ui/tsconfig.json` typechecks it.
 *
 * `qa-board up` compiles this file against the served checkout's `ui/src`
 * with the ui tests' compiler options, then runs it as
 * `node validate.js <fixtures-dir> <viewer-url>`.
 */
import { kickoffBlock, sessionAdmissible, writeBlock } from "../../src/features/auth/gate";
import { mutatePersonalProfile, readPersonalBootstrap, readPersonalIdentity } from "../../src/features/assistant/identity";
import { parseBrowserRead, readBrowser, stopBrowser, viewBrowser } from "../../src/features/canvas/browserClient";
import { canApprove, canArchive, canEditTypes, canRestore, documentsViewer, typeFor } from "../../src/features/documents/documentsAccess";
import {
  approve,
  archive,
  confirmType,
  DOCUMENT_CAPABILITY,
  DOCUMENT_REFUSAL_CODES,
  list,
  probe,
  refusalCode,
  restore,
  setType,
  type DocumentRow,
  type DocumentsResult,
} from "../../src/features/documents/documentsClient";
import { memberHistory, memberThreadStore, sendToAssistant } from "../../src/features/home/memberThread";
import { classifyFailure } from "../../src/features/agents/instructionsDoc";
import { libraryAbsent, liveSkillDeps, refusal, skillFiles, statusParts, unapprovedNames } from "../../src/features/agents/workspaceSkills";
import { settingsRedirect } from "../../src/features/settings/settingsNav";
import { parseMount } from "../../src/features/workspace-apps/screen/screenLifecycle";
import { screenProjection, screenTag } from "../../src/features/workspace-apps/screen/screenProjection";
import { shapeFor } from "../../src/features/workspace-apps/screen/screenProtocol";
import { workspaceApps, type AppContext, type AppEffect, type HomeInstallation, type Installation, type WorkspaceRun } from "../../src/features/workspace-apps/workspaceApps";
import { api } from "../../src/lib/api";
import { allowPrivateReads } from "../../src/lib/privateSession";
import { readTeamMembers } from "../../src/lib/teamRoster";
import type { Meta, StoredAgentProfile, WorkspaceSkillRead, WorkspaceSkillSummary } from "../../src/lib/types";

declare const require: { (name: string): any; main: unknown };
declare const module: unknown;
declare const process: { argv: string[]; exit(code: number): never; stdout: { write(text: string): void } };

/** Fixture file name (without `.json`) → payload. */
export type Fixtures = Record<string, unknown>;

/** The browser session presets the control endpoint can load. */
export const BROWSER_PRESETS = ["handoff", "handoff-signed-in", "active", "ended"] as const;

// ---- CAD-2286: the document lifecycle, as the hosted board answers ----

/** The capability probe presets: the lifecycle on, or a board that does not
 *  serve it (the flag off: every document route answers 404). */
export const DOCUMENT_PROBES = ["available", "not-served"] as const;
export type DocumentProbePreset = (typeof DOCUMENT_PROBES)[number];

/** The write routes; each has one refusal preset in `document-refusals.json`. */
export const DOCUMENT_WRITES = ["approve", "archive", "restore", "set-type", "confirm-type"] as const;
export type DocumentWrite = (typeof DOCUMENT_WRITES)[number];

/**
 * Who the fixture session is. `member` (the default) serves meta exactly as
 * the fixture has it. `owner` serves the roster's owner as the hosted board
 * serves a workspace owner (`operator: true`, session role `operator`), for
 * the owner's document flows.
 */
export const VIEWER_PRESETS = ["member", "owner"] as const;
export type ViewerPreset = (typeof VIEWER_PRESETS)[number];

/**
 * The Settings list. `native` (the default) sends no `settings_sections`, so
 * Settings keeps the full native list every other QA run relies on. `hosted`
 * sends the list the hosted board sends this viewer (agenticos-v2
 * `tenantSettingsSections`), with `documents` while the lifecycle is on.
 */
export const SETTINGS_PRESETS = ["native", "hosted"] as const;
export type SettingsPreset = (typeof SETTINGS_PRESETS)[number];
const HOSTED_SECTIONS: Record<ViewerPreset, readonly string[]> = {
  member: ["preferences", "members"],
  owner: ["preferences", "general", "members", "connections", "email", "status"],
};

export interface BoardPresets {
  viewer: ViewerPreset;
  settings: SettingsPreset;
  probe: DocumentProbePreset;
  /** The write routes answered with their refusal fixture. */
  refusals: readonly DocumentWrite[];
}
export const DEFAULT_PRESETS: BoardPresets = { viewer: "member", settings: "native", probe: "available", refusals: [] };

type RosterRow = { userId: string; name: string; email?: string; role: "owner" | "member" };
const roster = (fx: Fixtures) => (fx["team-members"] as { members: RosterRow[] }).members;

/** The session subject the documents routes answer, and whether it is the owner. */
export function boardViewer(fx: Fixtures, viewer: ViewerPreset): { sub: string; owner: boolean } {
  if (viewer === "owner") return { sub: roster(fx).find((row) => row.role === "owner")?.userId ?? "", owner: true };
  const meta = fx.meta as { session?: { user?: { sub?: string } } };
  return { sub: meta.session?.user?.sub ?? "", owner: false };
}

/** `GET /api/app-installations`: the operator's installed-app list, which the
 *  sidebar's app menu reads (CAD-2314). The one installed app is the list. */
export function appInstallations(fx: Fixtures): unknown {
  return [fx["app-installation"]];
}

/** `/api/meta` for these presets. The default presets return the fixture itself. */
export function boardMeta(fx: Fixtures, presets: BoardPresets): unknown {
  const meta = fx.meta as Record<string, unknown> & { session: { user: Record<string, unknown> } };
  let out: Record<string, unknown> = meta;
  if (presets.viewer === "owner") {
    const owner = roster(fx).find((row) => row.role === "owner");
    // As read-api.ts answers a workspace owner: the session role is `operator`
    // (the wire has only operator and member) and the actor is `users/<handle>`.
    const handle = owner?.email?.split("@")[0];
    const user = { ...meta.session.user, sub: owner?.userId, name: owner?.name, email: owner?.email, handle, role: "operator" };
    out = { ...meta, actor: `users/${handle}`, operator: true, session: { ...meta.session, user } };
  }
  if (presets.settings === "native") return out;
  const sections = HOSTED_SECTIONS[presets.viewer];
  return { ...out, settings_sections: presets.probe === "available" ? [...sections, "documents"] : [...sections] };
}

/** `/api/team/members` is answered only where the hosted board would be asked
 *  for it here; otherwise it goes to the sandbox, as before CAD-2286. */
export function servesRoster(presets: BoardPresets): boolean {
  return presets.settings === "hosted" || presets.viewer === "owner";
}

/** One answer from the documents responder, and the route it counts under. */
export interface DocumentsAnswer {
  route: string;
  status: number;
  body: unknown;
}

type WireFile = { id: string; version: number; created: string };
type WireDocument = {
  type: string;
  stage: string;
  submittedVersion: number | null;
  approvedVersion: number | null;
  selfApproved: boolean;
  deliveredVersion: number | null;
  deliveredAt: number | null;
  archivedAt: number | null;
  rev: number;
};
type WireType = { label: string; requireApproval: boolean; approver: string | null; recipientNote: string; archiveAfterDays: number; state: "pending" | "confirmed"; rev: number };
/** A served row, with what the store keeps that the wire does not carry. */
type Row = { file: WireFile; document: WireDocument; createdBy: string; approvedBy: string | null; preArchiveStage: string | null };
type Refusal = { status: number; code: string; message: string };

const ART_ID = /^art-[A-Za-z0-9]{1,40}$/;
const CONTROL = /[\u0000-\u001f\u007f]/;
const refused = (route: string, status: number, code: string, message: string): DocumentsAnswer => ({
  route,
  status,
  body: { ok: false, error: { code, message } },
});
const decode = (part: string): string | null => {
  try {
    return decodeURIComponent(part);
  } catch {
    return null;
  }
};

/** The paths the documents responder answers; every other path is not its own. */
export function isDocumentsRoute(pathname: string): boolean {
  return /^\/api\/(documents|document-types)(\/|$)/.test(pathname);
}

/**
 * The document routes, answered from the fixtures as the hosted board answers
 * the preset viewer (agenticos-v2 `document-routes.ts` and the store's
 * `document-store.ts`). The QA board's proxy and this validator both run it,
 * so the UI's own client checks the answers the board serves. Writes change
 * this responder's own copy of the rows and types; a new responder starts
 * again from the fixtures. A refusal preset answers its route first.
 * Returns null for a path that is not a document route.
 */
export function documentsResponder(fx: Fixtures, presets: () => BoardPresets) {
  const copy = <T>(value: unknown): T => JSON.parse(JSON.stringify(value)) as T;
  const authors = fx["document-authors"] as Record<string, string>;
  const rows: Row[] = [
    ...copy<{ documents: { file: WireFile; document: WireDocument }[] }>(fx["documents-active"]).documents,
    ...copy<{ documents: { file: WireFile; document: WireDocument }[] }>(fx["documents-archived"]).documents,
  ].map((row) => ({ ...row, createdBy: authors[row.file.id] ?? "", approvedBy: null, preArchiveStage: null }));
  const types = copy<{ types: WireType[] }>(fx["document-types"]).types;
  const refusals = fx["document-refusals"] as Record<DocumentWrite, Refusal>;
  const members = new Set(roster(fx).map((row) => row.userId));
  const changed = { approved: new Set<string>(), archived: new Set<string>(), restored: new Set<string>(), types: new Set<string>() };

  const key = (label: string) => label.trim().toLowerCase();
  /** The store's `rules()`: a confirmed type's approver, else the owner role (`null`). */
  const approverOf = (row: Row) => {
    const type = types.find((entry) => entry.label === key(row.document.type));
    return type?.state === "confirmed" ? type.approver : null;
  };
  const bump = (row: Row, patch: Partial<WireDocument>) => {
    row.document = { ...row.document, ...patch, rev: row.document.rev + 1 };
  };
  /** An exact JSON object with exactly `keys`, as the route's `readJsonBody` wants. */
  const exact = (body: unknown, keys: readonly string[]): Record<string, unknown> | null => {
    if (!body || typeof body !== "object" || Array.isArray(body)) return null;
    const found = Object.keys(body);
    return found.length === keys.length && keys.every((name) => found.includes(name)) ? (body as Record<string, unknown>) : null;
  };
  const shape = (route: string) => refused(route, 400, "invalid_request", "The request body has an unexpected shape.");
  const approverUnavailable = (route: string) =>
    refused(route, 409, "approver_unavailable", "This type's approver is no longer a member; the owner must update it.");

  function approve(route: string, id: string, body: unknown): DocumentsAnswer {
    const { sub, owner } = boardViewer(fx, presets().viewer);
    const request = exact(body, ["version"]);
    if (!request) return shape(route);
    const expected = request.version;
    if (typeof expected !== "number" || !Number.isSafeInteger(expected) || expected < 1) {
      return refused(route, 400, "invalid_request", "Give the version you reviewed.");
    }
    const row = rows.find((entry) => entry.file.id === id);
    if (!row) return refused(route, 404, "not_found", "submit this document for review first");
    if (row.document.stage === "archived") return refused(route, 409, "document_archived", "this document is archived");
    const approver = approverOf(row);
    if (!(approver === null ? owner : sub === approver)) {
      // The route: for the owner, an approver who has left is the owner's to fix.
      if (owner && approver !== null && !members.has(approver)) return approverUnavailable(route);
      return refused(route, 403, "not_approver", "only this type's approver can approve it");
    }
    const self = row.createdBy === sub;
    if (self && !owner) return refused(route, 403, "self_approval", "the author of a version cannot approve it");
    const newest = row.file.version;
    const repeat = row.document.stage === "approved" && row.document.approvedVersion === newest && row.approvedBy === sub;
    if (!repeat && (row.document.stage !== "in_review" || row.document.submittedVersion !== newest)) {
      return refused(route, 409, "not_in_review", "submit the current version for review first");
    }
    if (expected !== newest) return refused(route, 409, "conflict", "the document changed; open it again");
    if (!repeat) {
      bump(row, { stage: "approved", approvedVersion: newest, selfApproved: self });
      row.approvedBy = sub;
      changed.approved.add(id);
    }
    return { route, status: 200, body: { stage: "approved", version: row.document.approvedVersion, selfApproved: row.document.selfApproved } };
  }

  function archiveOrRestore(route: string, id: string, restore: boolean): DocumentsAnswer {
    if (!boardViewer(fx, presets().viewer).owner) {
      return refused(route, 403, "owner_only", restore ? "only the workspace owner can restore" : "only the workspace owner can archive");
    }
    const row = rows.find((entry) => entry.file.id === id);
    if (!row) return refused(route, 404, "not_found", "no such document");
    if (!restore) {
      if (row.document.stage !== "archived") {
        row.preArchiveStage = row.document.stage;
        bump(row, { stage: "archived", archivedAt: Date.now() });
        changed.archived.add(id);
        changed.restored.delete(id);
      }
      return { route, status: 200, body: { stage: "archived" } };
    }
    if (row.document.stage === "archived") {
      const back = row.preArchiveStage ?? (row.document.deliveredAt === null ? "draft" : "delivered");
      row.preArchiveStage = null;
      bump(row, { stage: back, archivedAt: null });
      changed.restored.add(id);
      changed.archived.delete(id);
    }
    return { route, status: 200, body: { stage: row.document.stage } };
  }

  function setType(route: string, label: string, body: unknown): DocumentsAnswer {
    const { owner } = boardViewer(fx, presets().viewer);
    const fields = exact(body, ["requireApproval", "approver", "recipientNote", "archiveAfterDays"]);
    if (!fields) return shape(route);
    // validTypeInput, with the route's empty approver read as the owner role.
    const { requireApproval, recipientNote, archiveAfterDays } = fields;
    const raw = fields.approver === "" ? null : fields.approver;
    const trimmed = label.trim();
    const valid = typeof requireApproval === "boolean" && (raw === null || typeof raw === "string") && typeof recipientNote === "string"
      && typeof archiveAfterDays === "number" && !CONTROL.test(label) && trimmed.length >= 1 && trimmed.length <= 40
      && (raw === null || (!CONTROL.test(raw) && raw.trim().length <= 200)) && recipientNote.length <= 200 && !CONTROL.test(recipientNote)
      && Number.isSafeInteger(archiveAfterDays) && archiveAfterDays >= 1 && archiveAfterDays <= 3650;
    if (!valid) return refused(route, 400, "invalid_request", "The type preference is not valid.");
    const approver = raw === null || raw.trim() === "" ? null : raw.trim();
    if (owner && approver !== null && !members.has(approver)) return approverUnavailable(route);
    const existing = types.find((entry) => entry.label === key(label));
    if (!owner && existing?.state === "confirmed") {
      return refused(route, 403, "owner_only", "a confirmed document type can only be changed by the workspace owner on the board");
    }
    const state = owner ? "confirmed" : "pending";
    const next: WireType = { label: key(label), requireApproval, approver, recipientNote, archiveAfterDays, state, rev: 1 };
    const same = existing && existing.state === state && existing.requireApproval === requireApproval && existing.approver === approver
      && existing.recipientNote === recipientNote && existing.archiveAfterDays === archiveAfterDays;
    if (existing && !same) Object.assign(existing, next, { rev: existing.rev + 1 });
    if (!existing) types.push(next);
    if (!same) changed.types.add(next.label);
    types.sort((a, b) => (a.label < b.label ? -1 : a.label > b.label ? 1 : 0));
    return { route, status: 200, body: { type: { ...(existing ?? next) } } };
  }

  function confirmType(route: string, label: string, body: unknown): DocumentsAnswer {
    const request = exact(body, ["rev"]);
    if (!request) return shape(route);
    const rev = request.rev;
    if (typeof rev !== "number" || !Number.isSafeInteger(rev) || rev < 1) return refused(route, 400, "invalid_request", "Give the type's rev as last read.");
    if (!boardViewer(fx, presets().viewer).owner) return refused(route, 403, "owner_only", "Only the workspace owner can confirm a type.");
    const row = types.find((entry) => entry.label === key(label));
    if (!row) return refused(route, 404, "not_found", "No such document type.");
    if (row.rev !== rev) return refused(route, 409, "conflict", "The type changed; read it again.");
    if (row.approver !== null && !members.has(row.approver)) return approverUnavailable(route);
    if (row.state !== "confirmed") {
      row.state = "confirmed";
      row.rev += 1;
      changed.types.add(row.label);
    }
    return { route, status: 200, body: { type: { ...row } } };
  }

  const wire = (row: Row) => ({ file: row.file, document: row.document });
  const newestFirst = (a: Row, b: Row) => (a.file.created < b.file.created ? 1 : a.file.created > b.file.created ? -1 : a.file.id.localeCompare(b.file.id));

  function answer(method: string, url: URL, body: unknown): DocumentsAnswer | null {
    const { pathname } = url;
    if (!isDocumentsRoute(pathname)) return null;
    const { probe, refusals: refusing } = presets();
    const off = (route: string) => (probe === "not-served" ? refused(route, 404, "not_found", "Not found.") : null);
    if (method === "GET" && pathname === "/api/document-types") {
      const route = "GET /api/document-types";
      return off(route) ?? { route, status: 200, body: { ...(fx["document-types"] as object), types: types.map((type) => ({ ...type })) } };
    }
    if (method === "GET" && pathname === "/api/documents") {
      const flag = url.searchParams.get("archived");
      // Counted apart, so a QA run can see the Archived tab asked for its own list.
      const route = flag === "1" ? "GET /api/documents?archived=1" : "GET /api/documents";
      const gone = off(route);
      if (gone) return gone;
      // As the hosted route reads it: absent or 0 is active, 1 archived.
      if (flag !== null && flag !== "0" && flag !== "1") return refused(route, 400, "invalid_request", "archived is 0 or 1.");
      const listed = rows.filter((row) => (row.document.stage === "archived") === (flag === "1")).sort(newestFirst);
      return { route, status: 200, body: { documents: listed.map(wire) } };
    }
    const doc = /^\/api\/documents\/([^/]+)\/(approve|archive|restore)$/.exec(pathname);
    const type = /^\/api\/document-types\/([^/]+?)(\/confirm)?$/.exec(pathname);
    if (method !== "POST" || (!doc && !type)) return refused(`${method} ${pathname}`, 404, "not_found", "Not found.");
    const write: DocumentWrite = doc ? (doc[2] as DocumentWrite) : type![2] ? "confirm-type" : "set-type";
    const route = doc ? `POST /api/documents/:id/${write}` : `POST /api/document-types/:label${write === "confirm-type" ? "/confirm" : ""}`;
    const gone = off(route);
    if (gone) return gone;
    if (refusing.includes(write)) {
      const refusal = refusals[write];
      return refused(route, refusal.status, refusal.code, refusal.message);
    }
    const target = decode((doc ?? type)![1]!);
    if (target === null) return refused(route, 404, "not_found", "Not found.");
    if (doc && !ART_ID.test(target)) return refused(route, 404, "not_found", "No such file.");
    if (write === "approve") return approve(route, target, body);
    if (write === "archive" || write === "restore") return archiveOrRestore(route, target, write === "restore");
    return write === "set-type" ? setType(route, target, body) : confirmType(route, target, body);
  }

  /** The rows and types this responder has changed since it started. */
  const changes = () => ({
    approved: [...changed.approved].sort(),
    archived: [...changed.archived].sort(),
    restored: [...changed.restored].sort(),
    types: [...changed.types].sort(),
  });
  return { answer, changes };
}

/** CAD-2419: the workspace skill library presets. `not-served` is a board
 *  that predates it (every library route answers 404); `failing` answers its
 *  list with 503, so the read failure and its Retry can be seen. */
export const SKILL_PROBES = ["available", "not-served", "failing"] as const;
export type SkillProbe = (typeof SKILL_PROBES)[number];

type SkillRevision = WorkspaceSkillRead["revision"];
type FixtureSkill = Omit<WorkspaceSkillSummary, "latest_revision" | "latest_digest" | "description" | "approved" | "updated_at"> & {
  approved: { revision: number; by: string; at: number } | null;
  revisions: Omit<SkillRevision, "name">[];
};

/** The paths the skills responder answers: the library and the profile it assigns through. */
export function isSkillsRoute(pathname: string): boolean {
  return /^\/api\/(workspace-skills|agent-profiles\/[^/]+$)/.test(pathname);
}

/**
 * CAD-2419: the workspace skill library and the one hosted profile the agent
 * page assigns its skills through, answered as agenticos-v2's
 * `workspace-skill-routes.ts` and the profile route answer the preset viewer.
 * The owner decides (approve, revoke, delete) and saves the profile; a member
 * is answered `403 owner_required`. The QA board refuses an approval past
 * `approved_max`, so `approved_limit` can be seen. Writes change this
 * responder's own copy; a new responder starts again from the fixtures.
 */
export function skillsResponder(fx: Fixtures, viewer: () => ViewerPreset) {
  const fixture = JSON.parse(JSON.stringify(fx["workspace-skills"])) as { notice: string; approved_max: number; skills: FixtureSkill[] };
  const library = fixture.skills;
  const seed = JSON.parse(JSON.stringify(fx["skills-profile"])) as { profile: StoredAgentProfile };
  let profile = seed.profile;
  let probe: SkillProbe = "available";
  const answer = (route: string, status: number, body: unknown): DocumentsAnswer => ({ route, status, body });
  const no = (route: string, status: number, code: string, message: string) => answer(route, status, { error: { code, message } });
  const latest = (skill: FixtureSkill) => skill.revisions[skill.revisions.length - 1]!;
  const summary = (skill: FixtureSkill): WorkspaceSkillSummary => {
    const last = latest(skill);
    const approved = skill.approved && skill.revisions.find((r) => r.revision === skill.approved!.revision);
    return {
      name: skill.name,
      latest_revision: last.revision,
      latest_digest: last.digest,
      description: last.description,
      approved: skill.approved && approved ? { ...skill.approved, digest: approved.digest } : null,
      created_by: skill.created_by,
      created_at: skill.created_at,
      updated_at: last.created_at,
    };
  };
  const exact = (body: unknown, keys: readonly string[]): Record<string, unknown> | null => {
    if (!body || typeof body !== "object" || Array.isArray(body)) return null;
    const found = Object.keys(body);
    return found.length === keys.length && keys.every((name) => found.includes(name)) ? (body as Record<string, unknown>) : null;
  };

  function respond(method: string, url: URL, body: unknown): DocumentsAnswer {
    const { pathname } = url;
    const owner = viewer() === "owner";
    const profileRoute = /^\/api\/agent-profiles\/([^/]+)$/.exec(pathname);
    if (profileRoute) {
      const route = `${method} /api/agent-profiles/:id`;
      if (decodeURIComponent(profileRoute[1]!) !== profile.id) return no(route, 404, "not_found", "Unknown agent profile.");
      if (method === "GET") return answer(route, 200, { profile });
      if (!owner) return no(route, 403, "owner_required", "Only owners edit agent profiles.");
      const fields = exact(body, ["name", "role", "responsibilities", "skills", "status", "expectedRevision"]);
      if (!fields || !Array.isArray(fields.skills) || fields.skills.some((name) => typeof name !== "string")) {
        return no(route, 400, "invalid_request", "Invalid agent profile.");
      }
      if (fields.expectedRevision !== profile.revision) return no(route, 409, "revision_conflict", "Reload the agent profile.");
      profile = {
        ...profile,
        name: String(fields.name),
        role: String(fields.role),
        responsibilities: String(fields.responsibilities).trim(),
        skills: [...new Set(fields.skills as string[])],
        revision: profile.revision + 1,
        updated_at: Date.now(),
      };
      return answer(route, 200, { profile });
    }
    if (probe === "not-served") return no(`${method} ${pathname}`, 404, "not_found", "Not found.");
    if (method === "GET" && pathname === "/api/workspace-skills") {
      const route = "GET /api/workspace-skills";
      if (probe === "failing") return no(route, 503, "unavailable", "The workspace is unavailable.");
      return answer(route, 200, { skills: library.map(summary) });
    }
    const one = /^\/api\/workspace-skills\/([^/]+)(?:\/(approve|revoke|delete))?$/.exec(pathname);
    if (!one || (method === "GET") === Boolean(one[2])) return no(`${method} ${pathname}`, 404, "not_found", "Not found.");
    const name = decodeURIComponent(one[1]!);
    const skill = library.find((entry) => entry.name === name);
    if (method === "GET") {
      const route = "GET /api/workspace-skills/:name";
      const asked = url.searchParams.get("revision");
      const revision = skill && (asked === null ? latest(skill) : skill.revisions.find((r) => String(r.revision) === asked));
      if (!skill || !revision) return no(route, 404, "not_found", "Unknown skill or revision.");
      return answer(route, 200, { notice: fixture.notice, skill: summary(skill), revision: { name, ...revision }, approved: skill.approved?.revision === revision.revision });
    }
    const verb = one[2]!;
    const route = `POST /api/workspace-skills/:name/${verb}`;
    if (!owner) return no(route, 403, "owner_required", "Only owners decide workspace skills.");
    const fields = exact(body, verb === "approve" ? ["revision", "digest"] : verb === "revoke" ? ["digest"] : ["expectedRevision"]);
    if (!fields) return no(route, 400, "invalid_request", "Invalid request.");
    if (!skill) return no(route, 404, "not_found", "Unknown skill or revision.");
    if (verb === "approve") {
      const revision = skill.revisions.find((r) => r.revision === fields.revision);
      if (!revision) return no(route, 404, "not_found", "Unknown skill or revision.");
      if (revision.digest !== fields.digest) return no(route, 409, "digest_mismatch", "Reload the skill and review it again.");
      if (!skill.approved && library.filter((entry) => entry.approved).length >= fixture.approved_max) {
        return no(route, 409, "approved_limit", "Revoke an approved skill first.");
      }
      skill.approved = { revision: revision.revision, by: boardViewer(fx, "owner").sub, at: Date.now() };
      return answer(route, 200, { skill: summary(skill) });
    }
    if (verb === "revoke") {
      const current = summary(skill).approved;
      if (!current || current.digest !== fields.digest) return no(route, 409, "digest_mismatch", "Reload the skill and review it again.");
      skill.approved = null;
      return answer(route, 200, { skill: summary(skill) });
    }
    if (skill.approved) return no(route, 409, "approved_skill", "Revoke the skill before deleting it.");
    if (fields.expectedRevision !== latest(skill).revision) return no(route, 409, "revision_conflict", "Reload the skill.");
    library.splice(library.indexOf(skill), 1);
    return answer(route, 200, { name, deleted: true });
  }

  return {
    answer: respond,
    probe: () => probe,
    setProbe: (next: SkillProbe) => {
      probe = next;
    },
    /** Someone else saved the profile: its revision moves on, so the page's next save is stale. */
    bumpProfile: () => {
      profile = { ...profile, revision: profile.revision + 1, updated_at: Date.now() };
    },
    profile: () => profile,
    /** `GET /api/agents` with the fixture profile on the roster, on top of what the sandbox listed. */
    roster: (sandbox: unknown) => {
      const row = (fx["skills-profile"] as { row: { id: string } }).row;
      const base = sandbox && typeof sandbox === "object" && Array.isArray((sandbox as { agents?: unknown }).agents)
        ? (sandbox as Record<string, unknown>)
        : { agents: [], by_issue: {}, daemon: "reachable", profiles: [], refresh_error: false, snapshot_age_secs: 0, stale: false, totals: null };
      const profiles = Array.isArray(base.profiles) ? (base.profiles as { id: string }[]).filter((p) => p.id !== row.id) : [];
      return { ...base, profiles: [...profiles, row] };
    },
  };
}

const CONVERSATION = "qa-board-conversation";

export function loadFixtures(dir: string): Fixtures {
  const fs = require("fs");
  const path = require("path");
  const out: Fixtures = {};
  for (const name of fs.readdirSync(dir).sort()) {
    if (name.endsWith(".json")) out[name.slice(0, -5)] = JSON.parse(fs.readFileSync(path.join(dir, name), "utf8"));
  }
  return out;
}

/** A browser body the UI accepts as a session, or why not. The control
 *  endpoint uses this too, so a state set mid-run is checked the same way. */
export function checkBrowser(body: unknown): string | null {
  const read = parseBrowserRead(body);
  if (read === null) return "parseBrowserRead refused it";
  if (read.kind !== "session") return "parseBrowserRead read it as no browser";
  const raw = body as Record<string, unknown>;
  // `""` reads back as null: a fixture must say what it means.
  if (read.session.state !== raw.state || read.session.signedIn !== (raw.signedIn === true)
    || read.session.url !== (raw.url ?? null) || read.session.instructions !== (raw.instructions ?? null)) {
    return "parseBrowserRead did not read it back unchanged";
  }
  return null;
}

/** Answers the board routes from `fx`, as the QA board's proxy does. */
function fixtureFetch(
  fx: Fixtures,
  browser: () => unknown,
  viewUrl: string,
  board: () => Board,
): typeof fetch {
  return (async (input: unknown, init?: { method?: string; body?: unknown }) => {
    const url = new URL(String(input), "http://qa-board.invalid");
    const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
    const request = typeof init?.body === "string" ? JSON.parse(init.body) as Record<string, unknown> : {};
    if (isSkillsRoute(url.pathname)) {
      const skill = board().skills.answer(init?.method ?? "GET", url, request);
      return json(skill.body, skill.status);
    }
    const document = board().documents.answer(init?.method ?? "GET", url, typeof init?.body === "string" ? request : null);
    if (document) return json(document.body, document.status);
    if (url.pathname === "/api/team/members" && servesRoster(board().presets)) return json(fx["team-members"]);
    switch (`${init?.method ?? "GET"} ${url.pathname}`) {
      case "GET /api/app-installations": return json(appInstallations(fx));
      case "GET /api/browser": return json(browser());
      case "POST /api/browser/view": return json({ url: viewUrl });
      case "POST /api/browser/stop": return json(fx["browser-stop"]);
      case "POST /api/member-assistant/state": return json(fx["member-assistant-state"]);
      case "POST /api/member-assistant/history": return json(fx["member-assistant-history"]);
      case "POST /api/member-assistant/profile": return json(fx["member-assistant-profile"]);
      case "POST /api/member-assistant/send": return json({ ...(fx["member-assistant-send"] as object), message: request.message });
      default: return new Response("{}", { status: 404 });
    }
  }) as typeof fetch;
}

const failed = (error: unknown) => (error instanceof Error ? error.message : String(error));
const failureText = (result: DocumentsResult<unknown>) => (result.ok ? "ok" : `${result.failure.kind} ${result.failure.status}: ${result.failure.message}`);

type Board = { presets: BoardPresets; documents: ReturnType<typeof documentsResponder>; skills: ReturnType<typeof skillsResponder> };
type Fail = (payload: string, why: string) => void;

/**
 * CAD-2286: the document fixtures through `documentsClient` and the UI's own
 * access rules, for each viewer preset, each on a fresh responder. `board` is
 * what the fetch shim answers from; this swaps it per scenario.
 */
/**
 * CAD-2419: the skill library and the profile through the agent page's own
 * calls (`liveSkillDeps`, `api.agentProfile`/`saveAgentProfile`) and its
 * readers, for the member and the owner, each on a fresh responder.
 */
async function validateSkills(fx: Fixtures, board: Board, fail: Fail) {
  const fresh = (viewer: ViewerPreset) => {
    board.presets = { ...DEFAULT_PRESETS, viewer };
    board.skills = skillsResponder(fx, () => board.presets.viewer);
  };
  const code = (error: unknown) => refusal(error).code;
  const row = (fx["skills-profile"] as { row: { id: string } }).row;
  try {
    fresh("member");
    const skills = await liveSkillDeps.list();
    if (!skills.some((s) => s.approved) || !skills.some((s) => !s.approved) || !skills.some((s) => s.approved && s.latest_revision > s.approved.revision)) {
      fail("workspace-skills", "the library needs an approved skill, a draft and an approved skill with a newer draft");
    }
    for (const skill of skills) {
      if (statusParts(skill).length === 0) fail("workspace-skills", `${skill.name}: no status line`);
      const read = await liveSkillDeps.read(skill.name);
      if (skillFiles(read.revision.files)[0]?.[0] !== "SKILL.md") fail("workspace-skills", `${skill.name}: no SKILL.md`);
      if (skill.approved) {
        const approved = await liveSkillDeps.read(skill.name, skill.approved.revision);
        if (!approved.approved || approved.revision.digest !== skill.approved.digest) fail("workspace-skills", `${skill.name}: the approved revision does not read back as approved`);
      }
    }
    const { profile } = await api.agentProfile(row.id);
    if (profile.id !== row.id) fail("skills-profile", "the stored profile is not the roster row's");
    if (unapprovedNames(profile.skills, skills).length === 0 || unapprovedNames(profile.skills, skills).length === profile.skills.length) {
      fail("skills-profile", "the profile should list one approved skill and one name with no approved skill");
    }
    const draft = skills.find((s) => !s.approved)!;
    await liveSkillDeps.approve(draft.name, draft.latest_revision, draft.latest_digest).then(
      () => fail("workspace-skills", "a member's approve was accepted"),
      (error) => code(error) !== "owner_required" && fail("workspace-skills", `a member's approve answered ${code(error)}`),
    );

    fresh("owner");
    const drafts = skills.filter((s) => !s.approved);
    const approvedCount = skills.filter((s) => s.approved).length;
    const max = (fx["workspace-skills"] as { approved_max: number }).approved_max;
    // Approving drafts up to the QA board's limit, then one more is approved_limit.
    for (const [i, s] of drafts.entries()) {
      const over = approvedCount + i >= max;
      await liveSkillDeps.approve(s.name, s.latest_revision, s.latest_digest).then(
        () => over && fail("workspace-skills", `approving ${s.name} past approved_max was accepted`),
        (error) => (!over || code(error) !== "approved_limit") && fail("workspace-skills", `approving ${s.name} answered ${code(error)}`),
      );
    }
    if (approvedCount + drafts.length <= max) fail("workspace-skills", "approved_max is never reached, so approved_limit cannot be seen");
    const approved = skills.find((s) => s.approved)!;
    await liveSkillDeps.remove(approved.name, approved.latest_revision).then(
      () => fail("workspace-skills", "deleting an approved skill was accepted"),
      (error) => code(error) !== "approved_skill" && fail("workspace-skills", `deleting an approved skill answered ${code(error)}`),
    );
    await liveSkillDeps.approve(approved.name, approved.latest_revision, "0".repeat(64)).then(
      () => fail("workspace-skills", "approving with a wrong digest was accepted"),
      (error) => code(error) !== "digest_mismatch" && fail("workspace-skills", `a wrong digest answered ${code(error)}`),
    );
    await liveSkillDeps.revoke(approved.name, approved.approved!.digest);
    await liveSkillDeps.remove(approved.name, approved.latest_revision);
    const saved = (await api.saveAgentProfile(row.id, { name: profile.name, role: profile.role, responsibilities: profile.responsibilities, skills: [], status: profile.status, expectedRevision: profile.revision })).profile;
    if (saved.revision !== profile.revision + 1 || saved.skills.length !== 0) fail("skills-profile", "a profile save did not advance the revision");
    await api.saveAgentProfile(row.id, { name: profile.name, role: profile.role, responsibilities: profile.responsibilities, skills: [], status: profile.status, expectedRevision: profile.revision }).then(
      () => fail("skills-profile", "a stale profile revision was accepted"),
      (error) => classifyFailure(error, "hosted") !== "conflict" && fail("skills-profile", "a stale profile revision is not read as a conflict"),
    );

    board.skills.setProbe("not-served");
    await liveSkillDeps.list().then(() => fail("workspace-skills", "not-served still lists"), (error) => !libraryAbsent(error) && fail("workspace-skills", "not-served is not read as absent"));
    board.skills.setProbe("failing");
    await liveSkillDeps.list().then(() => fail("workspace-skills", "failing still lists"), (error) => libraryAbsent(error) && fail("workspace-skills", "failing is read as absent, which hides the section"));
  } catch (error) {
    fail("workspace-skills", `the agent page's calls refused it: ${failed(error)}`);
  } finally {
    board.presets = { ...DEFAULT_PRESETS };
    board.skills = skillsResponder(fx, () => board.presets.viewer);
  }
}

async function validateDocuments(fx: Fixtures, board: Board, fail: Fail) {
  const start = (viewer: ViewerPreset) => {
    board.presets = { ...DEFAULT_PRESETS, viewer };
    board.documents = documentsResponder(fx, () => board.presets);
    const meta = boardMeta(fx, board.presets) as Meta;
    return documentsViewer({ sub: meta.session?.user?.sub ?? null, operator: meta.operator ?? null, readOnly: meta.read_only === true });
  };
  /** A write's answer must be `code` at `status` (or a success when `code` is null). */
  const expect = (payload: string, what: string, result: DocumentsResult<unknown>, code: string | null, status = 200) => {
    const ok = code === null ? result.ok : refusalCode(result) === code && !result.ok && result.failure.status === status;
    if (!ok) fail(payload, `${what}: read back as ${failureText(result)}${result.ok ? "" : ` (${refusalCode(result) ?? "no code"})`}, not ${code === null ? "a success" : `${status} ${code}`}`);
    return ok;
  };
  const rowsOf = async (archived: boolean, payload: string): Promise<DocumentRow[]> => {
    const result = await list(archived);
    if (!result.ok) {
      fail(payload, `list refused it: ${failureText(result)}`);
      return [];
    }
    return result.value;
  };
  const row = (rows: DocumentRow[], id: string) => rows.find((entry) => entry.file.id === id);

  // ---- the member ----
  let viewer = start("member");
  if (viewer.isOwner) fail("meta", "documentsViewer reads the member preset as the owner");
  // The roster is served only where the hosted board would be asked for it.
  if (await readTeamMembers().then(() => true, () => false)) fail("team-members", "the default presets serve the roster: it should go to the sandbox, as before");
  board.presets = { ...board.presets, settings: "hosted" };
  try {
    const members = await readTeamMembers();
    if (!members.some((member) => member.userId === viewer.viewerSub)) fail("team-members", "the roster does not list the fixture member");
  } catch (error) { fail("team-members", `readTeamMembers refused it: ${failed(error)}`); }
  board.presets = { ...board.presets, settings: "native" };

  const answer = await probe();
  if (answer.kind !== "available") {
    fail("document-types", `probe read it as ${answer.kind === "error" ? `an error (${answer.failure.message})` : `unavailable (${answer.reason})`}`);
    return;
  }
  if (answer.capability !== DOCUMENT_CAPABILITY) fail("document-types", `capability is not ${DOCUMENT_CAPABILITY}`);
  if (!answer.types.some((type) => type.state === "pending")) fail("document-types", "no pending type: the waiting-for-the-owner state never shows");
  if (!answer.types.some((type) => type.state === "confirmed" && type.approver === viewer.viewerSub)) {
    fail("document-types", "no confirmed type names the fixture member as approver: Approve never shows");
  }

  const active = await rowsOf(false, "documents-active");
  const archived = await rowsOf(true, "documents-archived");
  if (active.length === 0 || archived.length === 0) return;
  if (active.some((entry) => entry.document.stage === "archived")) fail("documents-active", "an archived row is in the active list");
  for (const stage of ["draft", "in_review", "approved", "delivered"] as const) {
    if (!active.some((entry) => entry.document.stage === stage)) fail("documents-active", `no ${stage} row: that stage filter shows nothing`);
  }
  if (archived.some((entry) => entry.document.stage !== "archived")) fail("documents-archived", "the archived list holds an unarchived row");
  const all = [...active, ...archived];
  const ids = all.map((entry) => entry.file.id);
  if (new Set(ids).size !== ids.length) fail("documents-active", "a file id appears twice across the lists");
  for (const id of ids) if (!/^art-[A-Za-z0-9]{1,40}$/.test(id)) fail("documents-active", `${id} is not a hosted artifact id`);
  const authors = fx["document-authors"] as Record<string, string>;
  const known = new Set((fx["team-members"] as { members: { userId: string }[] }).members.map((member) => member.userId));
  for (const id of ids) if (!known.has(authors[id] ?? "")) fail("document-authors", `${id} has no author on the roster`);

  const inReview = active.filter((entry) => entry.document.stage === "in_review");
  const typeOf = (entry: DocumentRow) => typeFor(answer.types, entry.document);
  const mine = inReview.filter((entry) => canApprove(viewer, entry.document, typeOf(entry)));
  const notMine = inReview.filter((entry) => !canApprove(viewer, entry.document, typeOf(entry)));
  if (mine.length < 2) fail("documents-active", "fewer than two in-review rows are the member's to approve");
  if (notMine.length === 0) fail("documents-active", "every in-review row is the member's to approve: no row shows without Approve");
  for (const entry of mine) {
    const version = entry.document.submittedVersion ?? entry.file.version;
    if (version > 1) expect("documents-active", `approve ${entry.file.id} at an older version`, await approve(entry.file.id, version - 1), "conflict", 409);
    expect("documents-active", `the member approves ${entry.file.id}`, await approve(entry.file.id, version), null);
    expect("documents-active", `the member approves ${entry.file.id} again`, await approve(entry.file.id, version), null);
  }
  const afterApprove = await rowsOf(false, "documents-active");
  for (const entry of mine) if (row(afterApprove, entry.file.id)?.document.stage !== "approved") fail("documents-active", `${entry.file.id} does not read back as approved`);
  for (const entry of notMine) expect("documents-active", `the member approves ${entry.file.id}`, await approve(entry.file.id, entry.file.version), "not_approver", 403);
  expect("documents-archived", `approve archived ${archived[0]!.file.id}`, await approve(archived[0]!.file.id, archived[0]!.file.version), "document_archived", 409);
  const changedAfterMember = board.documents.changes();
  if (changedAfterMember.approved.join() !== mine.map((entry) => entry.file.id).sort().join()) fail("documents-active", `changes() reports ${changedAfterMember.approved.join()} approved`);

  // The member's owner writes: hidden by the access rules, refused by the board.
  if (canArchive(viewer, active[0]!.document) || canRestore(viewer, archived[0]!.document) || canEditTypes(viewer)) {
    fail("meta", "the member preset reads as able to archive, restore or edit types");
  }
  expect("documents-active", "the member archives", await archive(active[0]!.file.id), "owner_only", 403);
  expect("documents-archived", "the member restores", await restore(archived[0]!.file.id), "owner_only", 403);
  const pending = answer.types.find((type) => type.state === "pending");
  const confirmed = answer.types.find((type) => type.state === "confirmed");
  if (pending) {
    expect("document-types", "the member confirms", await confirmType(pending.label, pending.rev), "owner_only", 403);
    const proposal = { requireApproval: !pending.requireApproval, approver: pending.approver, recipientNote: pending.recipientNote, archiveAfterDays: pending.archiveAfterDays };
    const proposed = await setType(pending.label, proposal);
    if (expect("document-types", "the member proposes a pending type", proposed, null) && proposed.ok
      && (proposed.value.state !== "pending" || proposed.value.rev !== pending.rev + 1)) {
      fail("document-types", "a member's proposal is not pending at the next rev");
    }
    const reread = await probe();
    const shown = reread.kind === "available" ? reread.types.find((type) => type.label === pending.label) : undefined;
    if (shown?.state !== "pending" || shown.requireApproval !== proposal.requireApproval) fail("document-types", "the type list does not show the member's pending proposal");
  }
  if (confirmed) {
    const input = { requireApproval: confirmed.requireApproval, approver: confirmed.approver, recipientNote: "", archiveAfterDays: 7 };
    expect("document-types", "the member changes a confirmed type", await setType(confirmed.label, input), "owner_only", 403);
  }

  // One refusal per write route, each the code the screens' copy is keyed on.
  const refusals = fx["document-refusals"] as Record<string, { status: number; code: string }>;
  const lifecycle = DOCUMENT_REFUSAL_CODES as readonly string[];
  const calls: Record<DocumentWrite, () => Promise<DocumentsResult<unknown>>> = {
    approve: () => approve(mine[0]?.file.id ?? "art-missing", 1),
    archive: () => archive(active[0]!.file.id),
    restore: () => restore(archived[0]!.file.id),
    "set-type": () => setType(answer.types[0]?.label ?? "qa", { requireApproval: true, approver: null, recipientNote: "", archiveAfterDays: 30 }),
    "confirm-type": () => confirmType(answer.types[0]?.label ?? "qa", 1),
  };
  for (const write of DOCUMENT_WRITES) {
    const refusal = refusals[write];
    if (!refusal || !lifecycle.includes(refusal.code)) {
      fail("document-refusals", `${write} has no refusal with a lifecycle code`);
      continue;
    }
    board.presets = { ...board.presets, refusals: [write] };
    expect("document-refusals", `the ${write} preset`, await calls[write](), refusal.code, refusal.status);
  }

  // The flag off: the probe hides the lifecycle, and the list is gone too.
  board.presets = { ...board.presets, refusals: [], probe: "not-served" };
  const off = await probe();
  if (off.kind !== "unavailable" || off.reason !== "not_served") fail("document-types", "the not-served preset does not read as not served");
  expect("documents-active", "the not-served list", await list(false), "not_found", 404);

  // ---- the owner, on a fresh responder ----
  viewer = start("owner");
  if (!viewer.isOwner || viewer.isOperatorSession) fail("meta", "documentsViewer does not read the owner preset as the workspace owner");
  const ownerTypes = await probe();
  if (ownerTypes.kind !== "available") return;
  const ownerActive = await rowsOf(false, "documents-active");
  const ownerArchived = await rowsOf(true, "documents-archived");
  const ownerTypeOf = (entry: DocumentRow) => typeFor(ownerTypes.types, entry.document);
  // Approve: the owner approves where no confirmed approver is in force, and may
  // approve their own version, which the receipt records as self-approved.
  const baseline = ownerActive.filter((entry) => entry.document.stage === "in_review" && canApprove(viewer, entry.document, ownerTypeOf(entry)));
  if (baseline.length === 0) fail("documents-active", "no in-review row is the owner's to approve");
  for (const entry of baseline) {
    const approved = await approve(entry.file.id, entry.file.version);
    if (expect("documents-active", `the owner approves ${entry.file.id}`, approved, null) && approved.ok
      && approved.value.selfApproved !== (authors[entry.file.id] === viewer.viewerSub)) {
      fail("documents-active", `${entry.file.id}: selfApproved does not say whether the owner wrote it`);
    }
  }
  if (!baseline.some((entry) => authors[entry.file.id] === viewer.viewerSub)) fail("document-authors", "no row the owner approves is the owner's own: self-approval never shows");
  for (const entry of ownerActive.filter((item) => item.document.stage === "in_review" && !canApprove(viewer, item.document, ownerTypeOf(item)))) {
    expect("documents-active", `the owner approves ${entry.file.id}, a named approver's`, await approve(entry.file.id, entry.file.version), "not_approver", 403);
  }
  // Archive and restore: restore returns the stage the row was archived from.
  const live = ownerActive.find((entry) => entry.document.stage === "approved");
  if (!live || !canArchive(viewer, live.document)) fail("documents-active", "the owner has no approved row to archive");
  else {
    const archivedNow = await archive(live.file.id);
    if (expect("documents-active", `the owner archives ${live.file.id}`, archivedNow, null) && archivedNow.ok && archivedNow.value.stage !== "archived") fail("documents-active", "archive does not answer archived");
    if (!row(await rowsOf(true, "documents-archived"), live.file.id)) fail("documents-archived", `${live.file.id} is not on the archived list`);
    const back = await restore(live.file.id);
    if (expect("documents-active", `the owner restores ${live.file.id}`, back, null) && back.ok && back.value.stage !== "approved") fail("documents-active", `restore answers ${back.value.stage}, not the stage it was archived from`);
    if (row(await rowsOf(false, "documents-active"), live.file.id)?.document.stage !== "approved") fail("documents-active", `${live.file.id} is not back on the active list as approved`);
  }
  for (const entry of ownerArchived) {
    if (!canRestore(viewer, entry.document)) fail("documents-archived", `the owner cannot restore ${entry.file.id}`);
    const back = await restore(entry.file.id);
    const expected = entry.document.deliveredAt === null ? "draft" : "delivered";
    if (expect("documents-archived", `the owner restores ${entry.file.id}`, back, null) && back.ok && back.value.stage !== expected) fail("documents-archived", `${entry.file.id} restores to ${back.value.stage}, not ${expected}`);
  }
  const owned = board.documents.changes();
  if (owned.restored.length === 0 || owned.approved.length === 0) fail("documents-active", "changes() does not report the owner's approvals and restores");

  // Types: the owner's answer is confirmed at once; confirm checks the rev.
  if (!canEditTypes(viewer)) fail("meta", "the owner preset reads as unable to edit types");
  const added = await setType("qa-notice", { requireApproval: false, approver: null, recipientNote: "QA fixture note.", archiveAfterDays: 21 });
  if (expect("document-types", "the owner adds a type", added, null) && added.ok && (added.value.state !== "confirmed" || added.value.rev !== 1)) fail("document-types", "the owner's new type is not confirmed at rev 1");
  expect("document-types", "the owner names a departed approver", await setType("qa-notice", { requireApproval: true, approver: "usr_qa_departed", recipientNote: "", archiveAfterDays: 21 }), "approver_unavailable", 409);
  const waiting = ownerTypes.types.find((type) => type.state === "pending");
  if (waiting) {
    expect("document-types", "the owner confirms at an old rev", await confirmType(waiting.label, waiting.rev + 1), "conflict", 409);
    const done = await confirmType(waiting.label, waiting.rev);
    if (expect("document-types", "the owner confirms", done, null) && done.ok && (done.value.state !== "confirmed" || done.value.rev !== waiting.rev + 1)) fail("document-types", "confirm does not move the type to confirmed at the next rev");
  }
  const after = await probe();
  if (after.kind !== "available" || !after.types.some((type) => type.label === "qa-notice" && type.state === "confirmed")) fail("document-types", "the type list does not show the owner's new type");
  board.presets = { ...DEFAULT_PRESETS };
}

/** Every failure, each naming its payload; empty when the UI accepts them all. */
export async function validateFixtures(fx: Fixtures, viewUrl: string): Promise<string[]> {
  const failures: string[] = [];
  const fail = (payload: string, why: string) => failures.push(`${payload}.json: ${why}`);
  for (const name of ["meta", "locale", "browser-stop", "member-assistant-state", "member-assistant-history", "member-assistant-profile", "member-assistant-send", ...BROWSER_PRESETS.map((p) => `browser-${p}`),
    "app-home", "app-installation", "app-contexts", "app-runs", "app-effects", "app-screen-mount",
    "team-members", "document-types", "documents-active", "documents-archived", "document-authors", "document-refusals",
    "workspace-skills", "skills-profile"]) {
    if (!(name in fx)) fail(name, "missing");
  }
  if (failures.length > 0) return failures;

  // `/api/meta`: the sign-in admission itself is inline in AuthBoundary (not
  // importable); these are the UI's exported readers of the same answer.
  const meta = fx.meta as Meta;
  if (writeBlock(meta) !== null) fail("meta", `writeBlock refused it: ${writeBlock(meta)}`);
  if (kickoffBlock(meta) === null) fail("meta", "kickoffBlock reads it as the operator, not a member");
  // The default presets serve the fixture meta itself, which keeps the native
  // Settings list every other QA run relies on: a `settings_sections` here
  // would redirect Connections, Email and Status away for everyone.
  if ("settings_sections" in (fx.meta as object)) fail("meta", "the default meta lists settings_sections: the native Settings pages would redirect for every QA run");
  if (boardMeta(fx, DEFAULT_PRESETS) !== fx.meta) fail("meta", "the default presets do not serve the fixture meta unchanged");
  for (const viewer of VIEWER_PRESETS) {
    for (const settings of SETTINGS_PRESETS) {
      for (const probe of DOCUMENT_PROBES) {
        const served = boardMeta(fx, { viewer, settings, probe, refusals: [] }) as Meta;
        const named = `viewer ${viewer}, settings ${settings}, documents ${probe}`;
        if (writeBlock(served) !== null) fail("meta", `${named}: writeBlock refused it`);
        // The shell's own admission: a hosted session's role is exactly operator or member.
        if (served.signed_in === true && !sessionAdmissible(served.session)) fail("meta", `${named}: AuthBoundary would not admit the session`);
        // The member reads as a member (member chat stays); the owner as the operator.
        if ((kickoffBlock(served) === null) !== (viewer === "owner")) fail("meta", `${named}: kickoffBlock reads the wrong viewer`);
        const offered = settingsRedirect("documents", served) === null;
        // Settings → Documents is mounted only for a hosted list that names it,
        // and a board without the lifecycle drops it.
        if (offered !== (settings === "hosted" && probe === "available")) {
          fail("meta", `${named}: Settings → Documents is ${offered ? "offered" : "not offered"}`);
        }
        if (settings === "native") {
          for (const section of ["connections", "email", "update"] as const) {
            if (settingsRedirect(section, served) !== null) fail("meta", `${named}: ${section} redirects away`);
          }
        }
      }
    }
  }
  if (!boardViewer(fx, "member").sub) fail("meta", "no session subject: the member could never be a document's approver");
  if (!boardViewer(fx, "owner").sub) fail("team-members", "no owner row: the owner preset has no subject");

  // The member app screen, as the board's WorkspaceApp builds it: the screen
  // tag, the projection and its v2 push (as ui/scripts/app-fixtures/producer.ts
  // does), and the mount receipt.
  const installation = fx["app-installation"] as Installation;
  const tag = screenTag(installation);
  if (tag === null) fail("app-installation", "screenTag finds no mountable screen");
  else {
    const contexts = (fx["app-contexts"] as { contexts: AppContext[] }).contexts;
    const runs = (fx["app-runs"] as { runs: WorkspaceRun[] }).runs;
    const effects = (fx["app-effects"] as { effects: AppEffect[] }).effects;
    const contextId = contexts[0]?.id ?? "";
    try {
      const projection = screenProjection(installation, tag, contextId, contexts, runs, effects,
        { installId: installation.install_id, contextId, status: "ok", intents: [] },
        { installId: installation.install_id, contextId, bindings: [], workers: null, texts: new Map() });
      if (!shapeFor(projection, "v2")) fail("app-runs", "shapeFor refused the screen's v2 push");
    } catch (error) { fail("app-contexts", `screenProjection refused it: ${failed(error)}`); }
    if (parseMount(fx["app-screen-mount"], tag) === null) fail("app-screen-mount", "parseMount refused it");
  }
  const home = (fx["app-home"] as { installations?: HomeInstallation[] }).installations ?? [];
  if (!home.some((row) => row.install_id === installation.install_id && row.name === installation.name)) {
    fail("app-home", "no row names app-installation.json, so /apps/<key> would not resolve");
  }

  for (const preset of BROWSER_PRESETS) {
    const why = checkBrowser(fx[`browser-${preset}`]);
    if (why) fail(`browser-${preset}`, why);
  }

  const original = globalThis.fetch;
  let browser: unknown = fx["browser-handoff"];
  const board: Board = {
    presets: { ...DEFAULT_PRESETS },
    documents: documentsResponder(fx, () => board.presets),
    skills: skillsResponder(fx, () => board.presets.viewer),
  };
  globalThis.fetch = fixtureFetch(fx, () => browser, viewUrl, () => board);
  allowPrivateReads();
  try {
    for (const preset of BROWSER_PRESETS) {
      browser = fx[`browser-${preset}`];
      try {
        const read = await readBrowser(CONVERSATION);
        if (read.kind !== "session") fail(`browser-${preset}`, "readBrowser read no browser");
      } catch (error) { fail(`browser-${preset}`, `readBrowser refused it: ${failed(error)}`); }
    }
    try {
      const view = await viewBrowser(CONVERSATION);
      if (view.kind !== "url" || view.url !== viewUrl) fail("browser-view", "viewBrowser did not return the viewer URL");
    } catch (error) { fail("browser-view", `viewBrowser refused ${viewUrl}: ${failed(error)}`); }
    try { await stopBrowser(CONVERSATION); } catch (error) { fail("browser-stop", `stopBrowser refused it: ${failed(error)}`); }

    try {
      const state = await api.memberAssistState();
      if (readPersonalIdentity(state).state !== "ready") fail("member-assistant-state", "readPersonalIdentity read no identity");
      if (readPersonalBootstrap(state) === "pending") fail("member-assistant-state", "bootstrap is pending: member chat would open on first-time setup");
      if (typeof state.conversation_id !== "string" || state.conversation_id === "") fail("member-assistant-state", "no conversation: the Browser card would never load");
    } catch (error) { fail("member-assistant-state", `memberAssistState refused it: ${failed(error)}`); }

    const outcome = await mutatePersonalProfile((body) => api.memberAssistProfileUpdate(body), { expected_revision: 1, action: "update", name: "QA" });
    if (!outcome.ok) fail("member-assistant-profile", `mutatePersonalProfile refused it: ${outcome.error}`);

    const store = memberThreadStore("qa-board:validate");
    await store.refresh();
    const history = memberHistory(store.get());
    if (!history.loaded || history.error) fail("member-assistant-history", `memberThreadStore refused it: ${history.error?.text ?? "not loaded"}`);
    else if ((store.get().data?.entries.length ?? 0) === 0) fail("member-assistant-history", "no entries: member chat would show an empty thread");

    const sent = await sendToAssistant("qa-board:validate", "hello", "qa-board-check");
    if (!sent.ok) fail("member-assistant-send", `sendToAssistant refused it: ${sent.error}`);

    // The sidebar's app menu, as App.tsx builds it from the operator's list.
    try {
      const rows = await workspaceApps.installations();
      const row = Array.isArray(rows) ? rows.find((i) => i.install_id === installation.install_id && i.name === installation.name) : undefined;
      if (!row) fail("app-installation", "the installed-app list has no row for it: the sidebar would list no app");
      else if (row.removed != null || (row.state ?? "active") !== "active") fail("app-installation", "the sidebar's menu drops it as removed or inactive");
    } catch (error) { fail("app-installation", `workspaceApps.installations refused the list: ${failed(error)}`); }

    await validateDocuments(fx, board, fail);
    await validateSkills(fx, board, fail);
  } finally {
    globalThis.fetch = original;
  }
  return failures;
}

if (typeof require === "function" && require.main === module) {
  const [dir, viewUrl] = process.argv.slice(2);
  validateFixtures(loadFixtures(dir), viewUrl).then((failures) => {
    process.stdout.write(`${JSON.stringify({ ok: failures.length === 0, failures })}\n`);
    process.exit(failures.length === 0 ? 0 : 1);
  }, (error: unknown) => {
    process.stdout.write(`${JSON.stringify({ ok: false, failures: [`validator crashed: ${failed(error)}`] })}\n`);
    process.exit(1);
  });
}
