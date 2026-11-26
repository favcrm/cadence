/**
 * CAD-2441 (card system phase 1, CAD-2437): what a finished turn read and
 * what it staged, derived only from the turn's own recorded tool calls and
 * results — never from the model's prose, so an answer cannot invent a
 * source or a write. No React, so the UI tests load it directly.
 */
import type { StagedOutcome, StagedWrite } from "../decisions/stagedWrite";
import { toolApprovalDecisionId, toolSteps, type ThreadEntry } from "./thread";

export type TurnSourceKind = "wiki" | "issue" | "attachment" | "artifact" | "web" | "app";

export interface TurnSource {
  kind: TurnSourceKind;
  /** Stable identity for deduplication. */
  key: string;
  /** What the chip names: a path, an id, a host or an app. */
  label: string;
  /** The board identity the chip opens (for an app record, the record id). */
  target: string;
  /** An app record's app key. */
  app?: string;
}

/** At most this many chips; the rest are "+N more". */
export const SOURCES_SHOWN = 8;

/**
 * A recorded call. Two forms exist: the v2 runtime records `name {json}`;
 * the native runtime records `store::threads::tool_summary`'s one line,
 * `name: <detail>`. The detail is the first of command, file_path, path,
 * pattern, url, query, description and prompt, or `{sorted keys}`
 * (CAD-2456). A summary's detail arrives as `summary`; a keys-only summary
 * carries none. Null when the text is neither form.
 */
export function parseToolCall(text: string): { name: string; args: Record<string, unknown>; summary?: string } | null {
  const trimmed = text.trim();
  const summary = /^([a-z][a-z0-9_]*): (.+)$/s.exec(trimmed);
  if (summary) {
    const detail = summary[2]!.trim();
    // `{a, b}` names keys only, never values.
    return { name: summary[1]!, args: {}, ...(detail.startsWith("{") ? {} : { summary: detail }) };
  }
  const space = trimmed.indexOf(" ");
  const name = space < 0 ? trimmed : trimmed.slice(0, space);
  if (!/^[a-z][a-z0-9_]*$/.test(name)) return null;
  if (space < 0) return { name, args: {} };
  try {
    const value: unknown = JSON.parse(trimmed.slice(space + 1));
    return value && typeof value === "object" && !Array.isArray(value)
      ? { name, args: value as Record<string, unknown> }
      : null;
  } catch {
    return null;
  }
}

/** A long opaque id cut for a chip: `art-4cff6ebb…`. */
export function shortId(id: string): string {
  const dash = id.indexOf("-");
  const head = dash > 0 && dash <= 8 ? dash + 1 : 0;
  return id.length > head + 10 ? `${id.slice(0, head + 8)}…` : id;
}

const str = (v: unknown): string | null => (typeof v === "string" && v.trim() !== "" ? v.trim() : null);

/** An app operation that reads one record by id. */
const APP_RECORD_READ = /^records\.(show|get|read)$/;

/** `app_call` args arrive as a JSON string or an object. */
function callArgs(value: unknown): Record<string, unknown> {
  if (value && typeof value === "object" && !Array.isArray(value)) return value as Record<string, unknown>;
  if (typeof value !== "string") return {};
  try {
    const parsed: unknown = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

function sourceOf(name: string, args: Record<string, unknown>, summary?: string): TurnSource | null {
  switch (name) {
    case "wiki_read": {
      // A summary's detail is the path (tool_summary's `path` field).
      const path = str(args.path) ?? (summary !== undefined ? str(summary) : null);
      return path ? { kind: "wiki", key: `wiki:${path}`, label: path, target: path } : null;
    }
    case "board_read_issue": {
      const id = str(args.id) ?? str(args.issue);
      return id ? { kind: "issue", key: `issue:${id}`, label: id, target: id } : null;
    }
    case "chat_attachment_read": {
      const id = str(args.id);
      return id ? { kind: "attachment", key: `attachment:${id}`, label: id, target: id } : null;
    }
    case "artifact_read":
    case "artifact_show": {
      // The chip names the artifact by its title when the turn holds it
      // (TurnRichCards); otherwise by this short id.
      const id = str(args.id) ?? str(args.artifact);
      return id ? { kind: "artifact", key: `artifact:${id}`, label: shortId(id), target: id } : null;
    }
    case "web_read": {
      // A summary's detail is the url (tool_summary's `url` field).
      const url = str(args.url) ?? (summary !== undefined ? str(summary) : null);
      if (!url) return null;
      let parsed: URL;
      try {
        parsed = new URL(url);
      } catch {
        return null;
      }
      if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return null;
      return { kind: "web", key: `web:${parsed.href}`, label: parsed.host, target: parsed.href };
    }
    case "app_call": {
      // An app record, named by its board identity (app and record id).
      // Lists, searches and writes name no single record, so they are not
      // sources.
      const app = str(args.app);
      const operation = str(args.operation);
      if (!app || !operation || !APP_RECORD_READ.test(operation)) return null;
      const inner = callArgs(args.args);
      const record = str(inner.record_id) ?? str(inner.id);
      if (!record) return null;
      return { kind: "app", key: `app:${app}:${record}`, label: `${app} · ${record}`, target: record, app };
    }
    default:
      return null;
  }
}

/**
 * The sources a turn read, in the order it first read them, one chip per
 * source. Only recorded `tool_call` entries count.
 */
export function turnSources(entries: readonly ThreadEntry[], names: ReadonlyMap<string, string> = new Map()): TurnSource[] {
  const seen = new Set<string>();
  const out: TurnSource[] = [];
  for (const entry of entries) {
    if (entry.kind !== "tool_call") continue;
    const call = parseToolCall(entry.text ?? "");
    if (!call) continue;
    const source = sourceOf(call.name, call.args, call.summary);
    if (!source || seen.has(source.key)) continue;
    seen.add(source.key);
    out.push(source);
  }
  return nameSources(out, names);
}

/** A web chip's label when its host alone is ambiguous: host plus the path, cut. */
const WEB_LABEL_CHARS = 40;

/**
 * CAD-2463: labels a person can tell apart. Web sources that share a host add
 * their path; an attachment the thread holds is named by its file name.
 */
function nameSources(sources: TurnSource[], names: ReadonlyMap<string, string>): TurnSource[] {
  const hosts = new Map<string, number>();
  for (const source of sources) if (source.kind === "web") hosts.set(source.label, (hosts.get(source.label) ?? 0) + 1);
  return sources.map((source) => {
    if (source.kind === "web" && (hosts.get(source.label) ?? 0) > 1) {
      const url = new URL(source.target);
      const path = url.pathname.replace(/\/$/, "");
      const label = `${url.host}${path || "/"}${url.search ? "?…" : ""}`;
      return { ...source, label: label.length > WEB_LABEL_CHARS ? `${label.slice(0, WEB_LABEL_CHARS - 1)}…` : label };
    }
    if (source.kind === "attachment") {
      const name = names.get(source.target);
      return { ...source, label: name ?? shortId(source.target) };
    }
    return source;
  });
}

/**
 * The file names of the attachments a thread's own messages carry
 * (`payload.attachments`, CAD-1168), by attachment id. A malformed row is
 * skipped.
 */
export function attachmentNames(entries: readonly ThreadEntry[]): Map<string, string> {
  const names = new Map<string, string>();
  for (const entry of entries) {
    const list = (entry.payload as { attachments?: unknown } | null | undefined)?.attachments;
    if (!Array.isArray(list)) continue;
    for (const item of list) {
      if (!item || typeof item !== "object") continue;
      const { id, name } = item as { id?: unknown; name?: unknown };
      if (typeof id === "string" && typeof name === "string" && name.trim() !== "") names.set(id, name.trim());
    }
  }
  return names;
}

const norm = (s: string): string => s.toLowerCase().replace(/-/g, "");

function resultObjects(entry: ThreadEntry): Record<string, unknown>[] {
  const candidates: unknown[] = [entry.payload];
  try {
    candidates.push(JSON.parse((entry.text ?? "").trim()));
  } catch {
    // Not JSON: not a staging result.
  }
  return candidates.filter(
    (value): value is Record<string, unknown> =>
      Boolean(value) && typeof value === "object" && !Array.isArray(value),
  );
}

/**
 * The decision id a staging result names: `{"staged": true, "actionId": …}`
 * (or `action_id`), from the result's JSON text or payload. A result that
 * only lists, reads or mentions an id names none (CAD-2450).
 */
export function stagedActionId(entry: ThreadEntry): string | null {
  for (const result of resultObjects(entry)) {
    if (result.staged === true) {
      const id = str(result.actionId) ?? str(result.action_id);
      if (id) return norm(id);
    }
    if (result.sent === false && typeof result.note === "string") {
      const id = str(result.action) ?? str(result.actionId) ?? str(result.action_id);
      if (id) return norm(id);
    }
  }
  return null;
}

/**
 * All decision ids a turn's tool result surfaced: single direct staging
 * results (`stagedActionId`) plus `staged_actions` from settled
 * `agent_message` results. Inbox listings (`items`) and unsettled messages are
 * excluded.
 */
export function stagedActionIds(entry: ThreadEntry): string[] {
  const ids: string[] = [];
  const seen = new Set<string>();
  const push = (id: string | null) => {
    if (id && !seen.has(id)) {
      seen.add(id);
      ids.push(id);
    }
  };
  push(stagedActionId(entry));
  for (const result of resultObjects(entry)) {
    if (
      typeof result.message !== "string" ||
      typeof result.alias !== "string" ||
      (result.state !== "answered" && result.state !== "failed") ||
      !Array.isArray(result.staged_actions) ||
      Array.isArray(result.items)
    ) {
      continue;
    }
    for (const item of result.staged_actions) {
      if (typeof item === "string") {
        const raw = str(item);
        if (raw) push(norm(raw));
      } else if (item && typeof item === "object" && !Array.isArray(item)) {
        const rec = item as Record<string, unknown>;
        const raw = str(rec.action_id) ?? str(rec.actionId);
        if (raw) push(norm(raw));
      }
    }
  }
  return ids;
}

/**
 * The staged writes this turn made: a decision named by one of the turn's
 * staging results. Only rows the surface already holds can match, so a write
 * the viewer cannot see never shows, and a later turn that lists or reads the
 * decision does not repeat its card.
 */
export function stagedInTurn(entries: readonly ThreadEntry[], rows: readonly StagedWrite[]): StagedWrite[] {
  const ids = new Set(entries.filter((entry) => entry.kind === "tool_result").flatMap(stagedActionIds));
  if (ids.size === 0) return [];
  return rows.filter((row) => ids.has(norm(row.id)));
}

/** A staged action or its receipt does not start work or stop an open sibling. */
export function activityAfterStagedTool(entries: readonly ThreadEntry[], actions: readonly StagedWrite[]): {
  continuing: boolean;
  continuation: string | null;
} {
  let newerWork = false;
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i];
    const decision = toolApprovalDecisionId(entry);
    const isReceipt = decision !== null && actions.some((action) => norm(action.id) === decision);
    const isStage = entry.role === "agent" && entry.kind === "tool_result" &&
      stagedActionIds(entry).some((id) => actions.some((action) => norm(action.id) === id));
    if (isReceipt || isStage) {
      // Same-round calls can finish separately. Keep any sibling still open.
      return {
        continuing: newerWork || toolSteps(entries).some((step) => step.id !== undefined && !step.done),
        continuation: isReceipt ? entry.message ?? null : null,
      };
    }
    if (entry.role === "agent" && (entry.kind === "tool_call" || entry.kind === "thinking" || entry.kind === "assistant_text")) newerWork = true;
  }
  return { continuing: false, continuation: null };
}

export interface StagedField {
  label: string;
  value: string;
  /** Empty or not provided: shown as flagged. */
  missing: boolean;
  /** The write removes this field; `value` is what it removes. */
  removed: boolean;
}

/** At most this many fields on the card; the review card has the rest. */
export const STAGED_FIELDS_SHOWN = 6;
/** Each value is cut to this many characters on the card. */
export const STAGED_VALUE_CHARS = 280;

const cut = (s: string): string => (s.length > STAGED_VALUE_CHARS ? `${s.slice(0, STAGED_VALUE_CHARS - 1)}…` : s);

/**
 * The key fields the staged card shows: a connected effect's `preview` when
 * the row carries one (display only — what runs is the stored call), else
 * the row's own changes. Every value is plain text.
 */
/** Preview roles that hold the message itself, shown as the card's body. */
const BODY_ROLES = new Set(["body", "text", "message"]);

export function stagedFields(write: StagedWrite): { fields: StagedField[]; more: number; body: string | null } {
  const preview = write.connected?.preview ?? write.customMcp?.preview;
  // CAD-2463: a connected effect's message is the body, in full (the preview
  // already caps it at 2,000 characters); the card clamps it, not cuts it.
  const bodyField = preview?.fields.find((field) => BODY_ROLES.has(field.role)) ?? null;
  const waTextChange = !preview && write.whatsapp != null
    ? write.changes.find((change) => change.field === "text" && change.kind !== "removed" && (change.after ?? "").trim() !== "") ?? null
    : null;
  const body =
    bodyField && bodyField.value.trim() !== ""
      ? bodyField.value
      : waTextChange && waTextChange.after
        ? waTextChange.after
        : null;
  const orderedChanges = write.whatsapp != null
    ? [
        ...write.changes.filter((change) => change !== waTextChange && change.field !== "digest"),
        ...write.changes.filter((change) => change !== waTextChange && change.field === "digest"),
      ]
    : write.changes;
  const all: StagedField[] = preview
    ? preview.fields
        .filter((field) => field !== bodyField || body === null)
        .map((field) => ({ label: field.label, value: cut(field.value), missing: field.value.trim() === "", removed: false }))
    : orderedChanges.map((change) => {
        if (change.kind === "removed") return { label: change.label, value: cut(change.before ?? ""), missing: false, removed: true };
        const value = change.after ?? "";
        return { label: change.label, value: cut(value), missing: change.after == null || value.trim() === "", removed: false };
      });
  return { fields: all.slice(0, STAGED_FIELDS_SHOWN), more: Math.max(0, all.length - STAGED_FIELDS_SHOWN), body };
}

/** The body clamps at this many lines; about this many characters fill them. */
export const BODY_CLAMP_LINES = 6;
const BODY_CLAMP_CHARS = 400;

/**
 * Whether a body is long enough to clamp, so "Show full message" only
 * appears when it would reveal something (CAD-2463 QA).
 */
export function bodyNeedsClamp(body: string): boolean {
  return body.split("\n").length > BODY_CLAMP_LINES || body.length > BODY_CLAMP_CHARS;
}

/** The sending account or Custom MCP endpoint a staged effect names, if any. */
export function stagedAccount(write: StagedWrite): string | null {
  const account =
    write.connected?.preview?.account ??
    write.connected?.account ??
    write.customMcp?.preview?.account ??
    write.customMcp?.endpointHost ??
    write.whatsapp?.providerLabel ??
    null;
  return account && account.trim() !== "" ? account.trim() : null;
}

const domainOf = (address: string): string | null => {
  const at = address.lastIndexOf("@");
  if (at < 1 || at === address.length - 1) return null;
  const domain = address.slice(at + 1).toLowerCase().replace(/[>\s.]+$/, "");
  return /^[a-z0-9.-]+\.[a-z]{2,}$/.test(domain) ? domain : null;
};

/** Recipient roles an outside-domain check reads. */
const RECIPIENT_ROLES = new Set(["to", "cc", "bcc"]);

/**
 * CAD-2463: how many email recipients sit outside the sending account's
 * domain (a subdomain counts as inside). Zero when the effect is not an email
 * or the account is not an address, so the card never guesses.
 */
export function outsideDomainRecipients(write: StagedWrite): number {
  const preview = write.connected?.preview;
  if (!preview || preview.kind !== "email") return 0;
  const account = stagedAccount(write);
  const home = account ? domainOf(account) : null;
  if (!home) return 0;
  let outside = 0;
  for (const field of preview.fields) {
    if (!RECIPIENT_ROLES.has(field.role)) continue;
    for (const part of field.value.split(/[,;]/)) {
      const address = (/<([^>]+)>/.exec(part)?.[1] ?? part).trim();
      const domain = domainOf(address);
      if (domain && domain !== home && !domain.endsWith(`.${home}`)) outside++;
    }
  }
  return outside;
}

/**
 * CAD-2463: the outcome in a connected effect's own words, only once the row
 * says so: an applied send is "Sent", an applied draft "Draft saved", a
 * failed one "Couldn't send". Anything else keeps the rail's word, so an
 * uncertain result never reads as success.
 */
export function stagedStatus(write: StagedWrite, outcome: StagedOutcome, railWord: string): string {
  if (write.customMcp != null) {
    if (outcome === "applied") return "Ran";
    if (outcome === "failed") return "Couldn't run";
    return railWord;
  }
  if (write.whatsapp != null) {
    if (write.whatsapp.category === "message" || write.whatsapp.category === "broadcast") {
      if (write.whatsapp.operation === "whatsapp.broadcast.stop.stage") {
        if (outcome === "applied") return "Stopped";
        if (outcome === "failed") return "Couldn't stop";
        return railWord;
      }
      if (outcome === "applied") return "Sent";
      if (outcome === "failed") return "Couldn't send";
      return railWord;
    }
    if (outcome === "applied") return "Applied";
    if (outcome === "failed") return "Couldn't apply";
    return railWord;
  }
  const effect = write.connected?.effect;
  if (!effect) return railWord;
  if (outcome === "applied") return effect === "send" ? "Sent" : "Draft saved";
  if (outcome === "failed") return effect === "send" ? "Couldn't send" : "Couldn't save the draft";
  return railWord;
}

export interface TurnSkillDraft {
  name: string;
  revision: number;
  digest: string;
  description: string;
  created: boolean;
  approvedRevision: number | null;
}

const SKILL_TAG = /^[a-z0-9][a-z0-9-]{0,31}$/;

function parseSkillDraftResult(entry: ThreadEntry): TurnSkillDraft | null {
  for (const result of resultObjects(entry)) {
    if (typeof result.created !== "boolean") continue;
    const skill = result.skill;
    const rev = result.revision;
    if (!skill || typeof skill !== "object" || Array.isArray(skill)) continue;
    if (!rev || typeof rev !== "object" || Array.isArray(rev)) continue;
    const s = skill as Record<string, unknown>;
    const r = rev as Record<string, unknown>;
    const name = str(s.name);
    const digest = str(r.digest);
    const revNum = r.revision;
    if (!name || !SKILL_TAG.test(name) || !digest) continue;
    if (typeof s.description !== "string") continue;
    if (typeof revNum !== "number" || !Number.isInteger(revNum) || revNum < 1) continue;
    const approvedRevision =
      typeof s.approved_revision === "number" && Number.isInteger(s.approved_revision) && s.approved_revision >= 1
        ? s.approved_revision
        : null;
    return {
      name,
      revision: revNum,
      digest,
      description: s.description.trim(),
      created: result.created,
      approvedRevision,
    };
  }
  return null;
}

/**
 * Workspace skill drafts saved by `workspace_skill_draft` tool calls in this
 * turn. Deduplicated by skill `name` in order of first appearance, keeping the
 * latest draft per skill if a turn drafted the same skill more than once.
 */
export function skillDraftsInTurn(entries: readonly ThreadEntry[]): TurnSkillDraft[] {
  const byName = new Map<string, TurnSkillDraft>();
  const order: string[] = [];
  for (const step of toolSteps([...entries])) {
    if (step.call.kind !== "tool_call" || !step.result || step.error || step.refused) continue;
    const call = parseToolCall(step.call.text ?? "");
    if (!call || call.name !== "workspace_skill_draft") continue;
    const draft = parseSkillDraftResult(step.result);
    if (!draft) continue;
    if (!byName.has(draft.name)) order.push(draft.name);
    byName.set(draft.name, draft);
  }
  return order.map((name) => byName.get(name)!);
}
