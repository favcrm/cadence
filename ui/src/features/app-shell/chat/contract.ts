import { FORBIDDEN_DESCRIPTOR_KEYS } from "../app-views/contract";
import { HOST_ACTION_IDS, HOST_ATTACHMENT_IDS, type HostActionId, type HostAttachmentId } from "./registry";

/**
 * App chat contract v1 (CAD-1109/1110): strict runtime validation of the
 * data-only descriptor an app package ships as `app-chat.json`. This file
 * mirrors contracts/app-chat/v1/app-chat.schema.json — the same grammar in
 * two notations (and the daemon's install validator, a third that reads the
 * schema file) — a change must land in all of them.
 *
 * A descriptor is *data*: it describes how the host's one Conversation
 * renders an app's chat. It cannot carry code, markup, URLs, actor or scope
 * claims, tokens or effects: the forbidden keys are the SAME constant
 * app-views/v1 refuses (imported, not copied), checked at every depth, and
 * `parseAppChat` fails closed on the first violation. Every string it
 * carries is rendered as React text, never as markdown, an anchor or a
 * style.
 */

export const APP_CHAT_CONTRACT = "app-chat/v1" as const;

export interface ChatContext {
  /** The shell's own screen id this entry answers to (D5). */
  id: string;
  label: string;
  prompts: string[];
  record: { label: string; prompts: string[] } | null;
}
export interface ChatAttachment {
  id: HostAttachmentId;
  label: string | null;
}
export interface CardField {
  label: string;
  from: string;
}
export interface CardButton {
  label: string;
  run: HostActionId;
  view: string;
}
export interface DirectiveCardSpec {
  title: string;
  text: string | null;
  fields: CardField[];
  buttons: CardButton[];
}
export type FrameSize = "small" | "medium" | "large";
export type ChatDirectiveSpec =
  | { match: string; card: DirectiveCardSpec; render: null }
  | { match: string; card: null; render: string; size: FrameSize };
export interface ChatSubject {
  kind: string;
  label: string;
}
export interface AppChat {
  contract: typeof APP_CHAT_CONTRACT;
  app: string;
  contexts: ChatContext[];
  attachments: ChatAttachment[];
  directives: ChatDirectiveSpec[];
  subjects: ChatSubject[];
  presentation: { showContext: boolean };
}

/** One bounded refusal; `path` is a dotted location in the value. */
export class AppChatContractError extends Error {
  readonly path: string;
  constructor(path: string, message: string) {
    super(`${path}: ${message}`);
    this.name = "AppChatContractError";
    this.path = path;
  }
}

const MAX_BYTES = 16 * 1024;
const MAX_NODES = 1024;
const MAX_DEPTH = 12;
const IDENT = /^[a-z][a-z0-9_-]{0,63}$/;
const MATCH = /^cadence_[a-z][a-z0-9_]{0,47}$/;
const SCREEN = /^screen:([a-z0-9][a-z0-9-]{0,31})$/;
/** Control and line-separator characters (code points 0-31, 127, 8232, 8233),
 *  matched by code so no separator ever sits in this source. */
function hasControl(s: string): boolean {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c <= 0x1f || c === 0x7f || c === 0x2028 || c === 0x2029) return true;
  }
  return false;
}
const FORBIDDEN = new Set<string>(FORBIDDEN_DESCRIPTOR_KEYS);

type Obj = Record<string, unknown>;
function isObj(v: unknown): v is Obj {
  return typeof v === "object" && v !== null && !Array.isArray(v)
    && (Object.getPrototypeOf(v) === Object.prototype || Object.getPrototypeOf(v) === null);
}
function fail(path: string, message: string): never {
  throw new AppChatContractError(path, message);
}

/** Bounds first, as app-views does: depth, node count, accessors and
 *  forbidden keys are checked before any shape check runs, and before
 *  serialization (so a cycle or a `toJSON` never runs). */
function scan(value: unknown, path: string, budget: { nodes: number }, depth: number): void {
  if (depth > MAX_DEPTH) fail(path, "input is nested too deeply");
  if (++budget.nodes > MAX_NODES) fail(path, "input has too many nodes");
  if (value === null || typeof value === "boolean" || typeof value === "string") return;
  if (typeof value === "number" && Number.isFinite(value)) return;
  if (Array.isArray(value)) {
    for (let i = 0; i < value.length; i++) scan(value[i], `${path}.${i}`, budget, depth + 1);
    return;
  }
  if (!isObj(value)) fail(path, "expected plain JSON data");
  for (const [key, property] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
    if (FORBIDDEN.has(key)) fail(path, `forbidden descriptor key: ${key}`);
    if (property.get || property.set) fail(path, "accessors are not JSON data");
    scan(property.value, `${path}.${key}`, budget, depth + 1);
  }
}

function text(v: unknown, path: string, max: number): string {
  if (typeof v !== "string" || v.length === 0) fail(path, "expected a non-empty string");
  if (v.length > max) fail(path, `string is longer than ${max} characters`);
  if (hasControl(v)) fail(path, "control characters are not allowed");
  return v;
}
function ident(v: unknown, path: string): string {
  const s = text(v, path, 64);
  if (!IDENT.test(s)) fail(path, `unsafe identifier: ${JSON.stringify(s)}`);
  return s;
}
function object(v: unknown, path: string, allowed: readonly string[]): Obj {
  if (!isObj(v)) fail(path, "expected an object");
  for (const key of Object.keys(v)) if (!allowed.includes(key)) fail(path, `unknown key: ${key}`);
  return v;
}
function list(v: unknown, path: string, max: number): unknown[] {
  if (v === undefined) return [];
  if (!Array.isArray(v)) fail(path, "expected an array");
  if (v.length > max) fail(path, `more than ${max} entries`);
  return v;
}
function prompts(v: unknown, path: string): string[] {
  return list(v, path, 3).map((p, i) => text(p, `${path}.${i}`, 80));
}
function unique(seen: Set<string>, key: string, path: string, what: string): void {
  if (seen.has(key)) fail(path, `duplicate ${what}: ${key}`);
  seen.add(key);
}

function parseCard(raw: unknown, path: string): DirectiveCardSpec {
  const c = object(raw, path, ["title", "text", "fields", "buttons"]);
  const fields = list(c.fields, `${path}.fields`, 4).map((f, i) => {
    const p = `${path}.fields.${i}`;
    const o = object(f, p, ["label", "from"]);
    return { label: text(o.label, `${p}.label`, 40), from: ident(o.from, `${p}.from`) };
  });
  const buttons = list(c.buttons, `${path}.buttons`, 2).map((b, i) => {
    const p = `${path}.buttons.${i}`;
    const o = object(b, p, ["label", "run", "view"]);
    const run = ident(o.run, `${p}.run`);
    if (!(HOST_ACTION_IDS as readonly string[]).includes(run)) fail(`${p}.run`, `not a host action: ${run}`);
    // open-view is the one v1 action and takes exactly a view id.
    return { label: text(o.label, `${p}.label`, 40), run: run as HostActionId, view: ident(o.view, `${p}.view`) };
  });
  return {
    title: text(c.title, `${path}.title`, 80),
    text: c.text === undefined ? null : text(c.text, `${path}.text`, 240),
    fields,
    buttons,
  };
}

/** Parse and validate; throws `AppChatContractError` on the first violation. */
export function parseAppChat(raw: unknown): AppChat {
  scan(raw, "$", { nodes: 0 }, 0);
  if (new TextEncoder().encode(JSON.stringify(raw)).byteLength > MAX_BYTES) {
    fail("$", `input exceeds ${MAX_BYTES} bytes`);
  }
  const root = object(raw, "$", ["contract", "app", "contexts", "attachments", "directives", "subjects", "presentation"]);
  if (root.contract !== APP_CHAT_CONTRACT) fail("contract", `expected ${APP_CHAT_CONTRACT}`);
  const app = ident(root.app, "app");

  const seenContexts = new Set<string>();
  const contexts = list(root.contexts, "contexts", 12).map((raw, i): ChatContext => {
    const p = `contexts.${i}`;
    const c = object(raw, p, ["id", "label", "prompts", "record"]);
    const id = ident(c.id, `${p}.id`);
    unique(seenContexts, id, p, "context id");
    let record: ChatContext["record"] = null;
    if (c.record !== undefined) {
      const r = object(c.record, `${p}.record`, ["label", "prompts"]);
      record = { label: text(r.label, `${p}.record.label`, 40), prompts: prompts(r.prompts, `${p}.record.prompts`) };
    }
    return { id, label: text(c.label, `${p}.label`, 40), prompts: prompts(c.prompts, `${p}.prompts`), record };
  });

  const seenAttachments = new Set<string>();
  const attachments = list(root.attachments, "attachments", 4).map((raw, i): ChatAttachment => {
    const p = `attachments.${i}`;
    const a = object(raw, p, ["id", "label"]);
    const id = ident(a.id, `${p}.id`);
    if (!(HOST_ATTACHMENT_IDS as readonly string[]).includes(id)) fail(`${p}.id`, `not a host capability: ${id}`);
    unique(seenAttachments, id, p, "attachment");
    return { id: id as HostAttachmentId, label: a.label === undefined ? null : text(a.label, `${p}.label`, 40) };
  });

  const seenMatches = new Set<string>();
  const directives = list(root.directives, "directives", 8).map((raw, i): ChatDirectiveSpec => {
    const p = `directives.${i}`;
    const d = object(raw, p, ["match", "card", "render", "size"]);
    const match = text(d.match, `${p}.match`, 64);
    if (!MATCH.test(match)) fail(`${p}.match`, "expected cadence_<snake_case>");
    unique(seenMatches, match, p, "directive match");
    if (d.card !== undefined && d.render === undefined) {
      if (d.size !== undefined) fail(`${p}.size`, "size belongs to a render directive");
      return { match, card: parseCard(d.card, `${p}.card`), render: null };
    }
    if (d.card === undefined && d.render !== undefined) {
      const render = SCREEN.exec(text(d.render, `${p}.render`, 40));
      if (!render) fail(`${p}.render`, "expected screen:<tag>");
      const size = d.size === undefined ? "medium" : d.size;
      if (size !== "small" && size !== "medium" && size !== "large") fail(`${p}.size`, "expected small, medium or large");
      return { match, card: null, render: render[1], size };
    }
    return fail(p, "a directive has exactly one of card or render");
  });

  const seenKinds = new Set<string>();
  const subjects = list(root.subjects, "subjects", 8).map((raw, i): ChatSubject => {
    const p = `subjects.${i}`;
    const s = object(raw, p, ["kind", "label"]);
    const kind = ident(s.kind, `${p}.kind`);
    unique(seenKinds, kind, p, "subject kind");
    return { kind, label: text(s.label, `${p}.label`, 40) };
  });

  let showContext = false;
  if (root.presentation !== undefined) {
    const pres = object(root.presentation, "presentation", ["showContext"]);
    if (pres.showContext !== undefined) {
      if (typeof pres.showContext !== "boolean") fail("presentation.showContext", "expected a boolean");
      showContext = pres.showContext;
    }
  }
  return { contract: APP_CHAT_CONTRACT, app, contexts, attachments, directives, subjects, presentation: { showContext } };
}
