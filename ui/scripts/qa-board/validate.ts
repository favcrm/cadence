/**
 * CAD-2291: the QA board's fixture payloads, checked with the UI's own client
 * code — the parsers and loaders the board UI runs on these routes — so a
 * payload the UI would refuse stops `qa-board up` (and fails the ui test)
 * instead of being served as a drifted fake.
 *
 * `qa-board up` compiles this file against the served checkout's `ui/src`
 * with the ui tests' compiler options, then runs it as
 * `node validate.js <fixtures-dir> <viewer-url>`.
 */
import { kickoffBlock, writeBlock } from "../../src/features/auth/gate";
import { mutatePersonalProfile, readPersonalBootstrap, readPersonalIdentity } from "../../src/features/assistant/identity";
import { parseBrowserRead, readBrowser, stopBrowser, viewBrowser } from "../../src/features/canvas/browserClient";
import { canApprove, documentsViewer, typeFor } from "../../src/features/documents/documentsAccess";
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
import { settingsRedirect } from "../../src/features/settings/settingsNav";
import { parseMount } from "../../src/features/workspace-apps/screen/screenLifecycle";
import { screenProjection, screenTag } from "../../src/features/workspace-apps/screen/screenProjection";
import { shapeFor } from "../../src/features/workspace-apps/screen/screenProtocol";
import type { AppContext, AppEffect, HomeInstallation, Installation, WorkspaceRun } from "../../src/features/workspace-apps/workspaceApps";
import { api } from "../../src/lib/api";
import { allowPrivateReads } from "../../src/lib/privateSession";
import { readTeamMembers } from "../../src/lib/teamRoster";
import type { Meta } from "../../src/lib/types";

declare const require: { (name: string): any; main: unknown };
declare const module: unknown;
declare const process: { argv: string[]; exit(code: number): never; stdout: { write(text: string): void } };

/** Fixture file name (without `.json`) → payload. */
export type Fixtures = Record<string, unknown>;

/** The browser session presets the control endpoint can load. */
export const BROWSER_PRESETS = ["handoff", "handoff-signed-in", "active", "ended"] as const;

// ---- CAD-2286: the document lifecycle, as the hosted board answers a member ----

/** The capability probe presets: the lifecycle on, or a board that does not
 *  serve it (the flag off: every document route answers 404). */
export const DOCUMENT_PROBES = ["available", "not-served"] as const;
export type DocumentProbePreset = (typeof DOCUMENT_PROBES)[number];

/** The write routes; each has one refusal preset in `document-refusals.json`. */
export const DOCUMENT_WRITES = ["approve", "archive", "restore", "set-type", "confirm-type"] as const;
export type DocumentWrite = (typeof DOCUMENT_WRITES)[number];

export interface DocumentsControl {
  probe: DocumentProbePreset;
  /** The write routes answered with their refusal fixture. */
  refusals: readonly DocumentWrite[];
}

/** One answer from the documents responder, and the route it counts under. */
export interface DocumentsAnswer {
  route: string;
  status: number;
  body: unknown;
}

type RawRow = {
  file: { id: string; version: number };
  document: { type: string; stage: string; submittedVersion: number | null; approvedVersion: number | null; selfApproved: boolean; rev: number };
};
type RawType = { label: string; approver: string | null; state: string };
type Refusal = { status: number; code: string; message: string };

const ART_ID = /^art-[A-Za-z0-9]{1,40}$/;
const refused = (route: string, status: number, code: string, message: string): DocumentsAnswer => ({
  route,
  status,
  body: { ok: false, error: { code, message } },
});

/** `/api/meta` as the hosted board sends it for this probe preset: with the
 *  lifecycle off, `documents` leaves the settings sections. */
export function documentsMeta(meta: unknown, probe: DocumentProbePreset): unknown {
  const row = meta as { settings_sections?: string[] };
  if (probe === "available" || !Array.isArray(row.settings_sections)) return meta;
  return { ...row, settings_sections: row.settings_sections.filter((section) => section !== "documents") };
}

/** The paths the documents responder answers; every other path is not its own. */
export function isDocumentsRoute(pathname: string): boolean {
  return /^\/api\/(documents|document-types)(\/|$)/.test(pathname);
}

/**
 * The document routes, answered from the fixtures as the hosted board answers
 * the fixture member (`viewerSub`). The QA board's proxy and this validator
 * both use it, so the UI's own client checks the answers the board serves.
 * Approve is the only write a member can reach; it moves the row to approved
 * in this responder's copy. Archive, restore and the type writes are the
 * owner's: they answer their refusal preset, else `qa_board_unserved`, since
 * this board has no owner session to answer them as. Returns null for a path
 * that is not a document route.
 */
export function documentsResponder(fx: Fixtures, viewerSub: string, control: () => DocumentsControl) {
  const copy = <T>(value: unknown): T => JSON.parse(JSON.stringify(value)) as T;
  const active = copy<{ documents: RawRow[] }>(fx["documents-active"]).documents;
  const archived = copy<{ documents: RawRow[] }>(fx["documents-archived"]).documents;
  const types = copy<{ types: RawType[] }>(fx["document-types"]).types;
  const refusals = fx["document-refusals"] as Record<DocumentWrite, Refusal>;

  function approve(id: string, body: unknown): DocumentsAnswer {
    const route = "POST /api/documents/:id/approve";
    if (!ART_ID.test(id)) return refused(route, 404, "not_found", "No such file.");
    const request = body as Record<string, unknown> | null;
    const version = request?.version;
    if (request === null || typeof request !== "object" || Object.keys(request).join() !== "version"
      || typeof version !== "number" || !Number.isSafeInteger(version) || version < 1) {
      return refused(route, 400, "invalid_request", "Give the version you reviewed.");
    }
    const row = active.find((entry) => entry.file.id === id) ?? archived.find((entry) => entry.file.id === id);
    if (!row) return refused(route, 404, "not_found", "No such file.");
    if (row.document.stage !== "in_review") return refused(route, 409, "not_in_review", "The document is not in review.");
    if (row.document.submittedVersion !== version) return refused(route, 409, "conflict", "The document changed; read it again.");
    // The approver in force: a confirmed type's, else the owner role, which this member is not.
    const type = types.find((entry) => entry.label === row.document.type.trim().toLowerCase());
    const approver = type?.state === "confirmed" ? type.approver : null;
    if (approver !== viewerSub) return refused(route, 403, "not_approver", "You are not this document's approver.");
    row.document = { ...row.document, stage: "approved", approvedVersion: version, selfApproved: false, rev: row.document.rev + 1 };
    return { route, status: 200, body: { stage: "approved", version, selfApproved: false } };
  }

  return function answer(method: string, url: URL, body: unknown): DocumentsAnswer | null {
    const { probe, refusals: refusing } = control();
    const off = (route: string) => (probe === "not-served" ? refused(route, 404, "not_found", "Not found.") : null);
    if (method === "GET" && url.pathname === "/api/document-types") {
      const route = "GET /api/document-types";
      return off(route) ?? { route, status: 200, body: { ...(fx["document-types"] as object), types } };
    }
    if (method === "GET" && url.pathname === "/api/documents") {
      const route = "GET /api/documents";
      const flag = url.searchParams.get("archived");
      const gone = off(route);
      if (gone) return gone;
      // As the hosted route reads it: absent or 0 is active, 1 archived.
      if (flag !== null && flag !== "0" && flag !== "1") return refused(route, 400, "invalid_request", "archived is 0 or 1.");
      return { route, status: 200, body: { documents: flag === "1" ? archived : active } };
    }
    if (!isDocumentsRoute(url.pathname)) return null;
    if (method !== "POST") return refused(`${method} ${url.pathname}`, 404, "not_found", "Not found.");
    const doc = /^\/api\/documents\/([^/]+)\/(approve|archive|restore)$/.exec(url.pathname);
    const type = /^\/api\/document-types\/([^/]+?)(\/confirm)?$/.exec(url.pathname);
    const write: DocumentWrite | null = doc ? (doc[2] as DocumentWrite) : type ? (type[2] ? "confirm-type" : "set-type") : null;
    if (write === null) return isDocumentsRoute(url.pathname) ? refused(`${method} ${url.pathname}`, 404, "not_found", "Not found.") : null;
    const route = doc ? `POST /api/documents/:id/${write}` : `POST /api/document-types/:label${write === "confirm-type" ? "/confirm" : ""}`;
    const gone = off(route);
    if (gone) return gone;
    if (refusing.includes(write)) {
      const refusal = refusals[write];
      return refused(route, refusal.status, refusal.code, refusal.message);
    }
    if (write === "approve") return approve(decodeURIComponent(doc![1]!), body);
    return refused(route, 404, "qa_board_unserved", "The QA board's member view does not serve this owner write; set its refusal preset to exercise it.");
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
function fixtureFetch(fx: Fixtures, browser: () => unknown, viewUrl: string, documents: ReturnType<typeof documentsResponder>): typeof fetch {
  return (async (input: unknown, init?: { method?: string; body?: unknown }) => {
    const url = new URL(String(input), "http://qa-board.invalid");
    const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
    const request = typeof init?.body === "string" ? JSON.parse(init.body) as Record<string, unknown> : {};
    const document = documents(init?.method ?? "GET", url, typeof init?.body === "string" ? request : null);
    if (document) return json(document.body, document.status);
    switch (`${init?.method ?? "GET"} ${url.pathname}`) {
      case "GET /api/team/members": return json(fx["team-members"]);
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

/** CAD-2286: the document fixtures through `documentsClient`, as the
 *  Documents screen and Settings → Documents read them for this member. */
async function validateDocuments(fx: Fixtures, viewerSub: string, control: DocumentsControl, fail: (payload: string, why: string) => void) {
  const meta = fx.meta as Meta;
  const viewer = documentsViewer({ sub: viewerSub, operator: meta.operator ?? null, readOnly: meta.read_only === true });

  try {
    const roster = await readTeamMembers();
    if (!roster.some((member) => member.userId === viewerSub)) fail("team-members", "the roster does not list the fixture member");
  } catch (error) { fail("team-members", `readTeamMembers refused it: ${failed(error)}`); }

  const answer = await probe();
  if (answer.kind !== "available") {
    fail("document-types", `probe read it as ${answer.kind === "error" ? `an error (${answer.failure.message})` : `unavailable (${answer.reason})`}`);
    return;
  }
  if (answer.capability !== DOCUMENT_CAPABILITY) fail("document-types", `capability is not ${DOCUMENT_CAPABILITY}`);
  if (!answer.types.some((type) => type.state === "pending")) fail("document-types", "no pending type: the waiting-for-the-owner state never shows");
  if (!answer.types.some((type) => type.state === "confirmed" && type.approver === viewerSub)) {
    fail("document-types", "no confirmed type names the fixture member as approver: Approve never shows");
  }

  const read = async (archived: boolean, payload: string): Promise<DocumentRow[]> => {
    const result = await list(archived, undefined);
    if (!result.ok) {
      fail(payload, `list refused it: ${failureText(result)}`);
      return [];
    }
    return result.value;
  };
  const active = await read(false, "documents-active");
  const archived = await read(true, "documents-archived");
  if (active.some((row) => row.document.stage === "archived")) fail("documents-active", "an archived row is in the active list");
  for (const stage of ["draft", "in_review", "approved", "delivered"] as const) {
    if (!active.some((row) => row.document.stage === stage)) fail("documents-active", `no ${stage} row: that stage filter shows nothing`);
  }
  if (archived.length === 0 || archived.some((row) => row.document.stage !== "archived")) fail("documents-archived", "the archived list is empty or holds an unarchived row");
  const ids = [...active, ...archived].map((row) => row.file.id);
  if (new Set(ids).size !== ids.length) fail("documents-active", "a file id appears twice across the lists");
  for (const id of ids) if (!/^art-[A-Za-z0-9]{1,40}$/.test(id)) fail("documents-active", `${id} is not a hosted artifact id`);

  const inReview = active.filter((row) => row.document.stage === "in_review");
  const mine = inReview.find((row) => canApprove(viewer, row.document, typeFor(answer.types, row.document)));
  if (!inReview.some((row) => !canApprove(viewer, row.document, typeFor(answer.types, row.document)))) {
    fail("documents-active", "every in-review row is the member's to approve: no row shows without Approve");
  }
  if (!mine) fail("documents-active", "no in-review row is the member's to approve");
  else {
    const approved = await approve(mine.file.id, mine.document.submittedVersion ?? mine.file.version);
    if (!approved.ok || approved.value.stage !== "approved") fail("documents-active", `approve refused ${mine.file.id}: ${failureText(approved)}`);
    else {
      const after = await list(false);
      if (!after.ok || after.value.find((row) => row.file.id === mine.file.id)?.document.stage !== "approved") {
        fail("documents-active", "an approved row does not read back as approved");
      }
    }
  }

  // One refusal per write route, each the code the screen's copy is keyed on.
  const refusals = fx["document-refusals"] as Record<string, { status: number; code: string }>;
  const known = (DOCUMENT_REFUSAL_CODES as readonly string[]);
  const calls: Record<DocumentWrite, () => Promise<DocumentsResult<unknown>>> = {
    approve: () => approve(mine?.file.id ?? "art-missing", 1),
    archive: () => archive(active[0]?.file.id ?? "art-missing"),
    restore: () => restore(archived[0]?.file.id ?? "art-missing"),
    "set-type": () => setType(answer.types[0]?.label ?? "qa", { requireApproval: true, approver: null, recipientNote: "", archiveAfterDays: 30 }),
    "confirm-type": () => confirmType(answer.types[0]?.label ?? "qa", 1),
  };
  for (const write of DOCUMENT_WRITES) {
    const refusal = refusals[write];
    if (!refusal || !known.includes(refusal.code)) {
      fail("document-refusals", `${write} has no refusal with a lifecycle code`);
      continue;
    }
    control.refusals = [write];
    const result = await calls[write]();
    if (refusalCode(result) !== refusal.code || (!result.ok && result.failure.status !== refusal.status)) {
      fail("document-refusals", `${write} read back as ${failureText(result)}, not ${refusal.status} ${refusal.code}`);
    }
  }
  control.refusals = [];

  // The flag off: the probe hides the lifecycle, and the list is gone too.
  control.probe = "not-served";
  const off = await probe();
  if (off.kind !== "unavailable" || off.reason !== "not_served") fail("document-types", "the not-served preset does not read as not served");
  const offList = await list(false);
  if (refusalCode(offList) !== "not_found" || (!offList.ok && offList.failure.status !== 404)) fail("documents-active", "the not-served preset still lists documents");
  control.probe = "available";
}

/** Every failure, each naming its payload; empty when the UI accepts them all. */
export async function validateFixtures(fx: Fixtures, viewUrl: string): Promise<string[]> {
  const failures: string[] = [];
  const fail = (payload: string, why: string) => failures.push(`${payload}.json: ${why}`);
  for (const name of ["meta", "locale", "browser-stop", "member-assistant-state", "member-assistant-history", "member-assistant-profile", "member-assistant-send", ...BROWSER_PRESETS.map((p) => `browser-${p}`),
    "app-home", "app-installation", "app-contexts", "app-runs", "app-effects", "app-screen-mount",
    "team-members", "document-types", "documents-active", "documents-archived", "document-refusals"]) {
    if (!(name in fx)) fail(name, "missing");
  }
  if (failures.length > 0) return failures;

  // `/api/meta`: the sign-in admission itself is inline in AuthBoundary (not
  // importable); these are the UI's exported readers of the same answer.
  const meta = fx.meta as Meta;
  if (writeBlock(meta) !== null) fail("meta", `writeBlock refused it: ${writeBlock(meta)}`);
  if (kickoffBlock(meta) === null) fail("meta", "kickoffBlock reads it as the operator, not a member");
  // Settings → Documents is mounted only when the board lists the section, and
  // a board without the lifecycle drops it (the not-served preset).
  if (settingsRedirect("documents", meta) !== null) fail("meta", "settings_sections does not offer documents");
  if (settingsRedirect("documents", documentsMeta(meta, "not-served") as Meta) === null) {
    fail("meta", "the not-served preset still offers Settings → Documents");
  }
  const viewerSub = meta.session?.user?.sub ?? null;
  if (viewerSub === null) fail("meta", "no session subject: the member could never be a document's approver");

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
  const control: DocumentsControl = { probe: "available", refusals: [] };
  globalThis.fetch = fixtureFetch(fx, () => browser, viewUrl, documentsResponder(fx, viewerSub ?? "", () => control));
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

    await validateDocuments(fx, viewerSub ?? "", control, fail);
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
