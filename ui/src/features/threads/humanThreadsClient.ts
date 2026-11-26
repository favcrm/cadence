import { ApiError } from "../../lib/api";
import { errorMessage } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation, type PrivateOperation } from "../../lib/privateSession";
import type { MentionOutcome } from "../mentions/mentions";

/**
 * CAD-1412 human rooms wire client (`/api/human-threads/*`, same origin).
 * Every call carries the Threads headers, DELETE included. The daemon derives
 * the actor from the board sign-in and checks the live roster itself; this
 * client never sends an identity, and a role is presentation only.
 */

export interface HumanMe {
  id: string;
  name: string;
}

export type HumanRoomKind = "dm" | "human_group" | "mixed_group";

export interface HumanRoomSummary {
  id: string;
  kind: HumanRoomKind;
  title: string;
  creator: string;
  created: number;
  unread: number;
}

export interface HumanMember {
  kind: "human" | "agent";
  id: string;
  label: string;
  look?: string | null;
}

export type HumanAttachmentKind = "image" | "video" | "pdf" | "text" | "office";

export interface HumanPostAttachment {
  id: string;
  name: string;
  mime: string;
  size: number;
  kind: HumanAttachmentKind;
  url: string;
}

export interface HumanPost {
  id: string;
  /** Opaque, per-human marker for this visible post. */
  read_cursor: string;
  sender: { id: string | null; label: string; kind?: "human" | "agent" };
  /** Plain or safe inline Markdown body; empty when the post only carries attachments. */
  body: string;
  created: number;
  silent: boolean;
  outcomes: MentionOutcome[];
  attachments?: HumanPostAttachment[];
}

export interface HumanRoomEvent {
  id: string;
  text: string;
  created: number;
}

export interface HumanRoomPage {
  room: string;
  scope_id: string;
  members: HumanMember[];
  posts: HumanPost[];
  events: HumanRoomEvent[];
  before: string | null;
  more_before: boolean;
  /** Fresh permission hint for an explicit Ask. */
  can_ask: boolean;
  /** True when the caller is the channel creator or workspace owner. */
  can_manage: boolean;
  /** Consecutive agent turns since the last human message. */
  agent_chain_count: number;
  agent_chain_limit: number;
}

/** A current agent on the trusted catalog (`uid` is the durable id, never an alias hash). */
export interface HumanAgent {
  uid: string;
  alias: string;
  look?: string | null;
}

export interface HumanAskReceipt {
  request: string;
  post: HumanPost;
  target: HumanAgent;
  /** True when the same client id was already queued: nothing new was sent. */
  duplicate: boolean;
}

async function call<T>(method: "GET" | "POST" | "DELETE", path: string, body?: object, operation?: PrivateOperation, signal?: AbortSignal): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(path, {
      method,
      credentials: "same-origin",
      cache: "no-store",
      headers: {
        ...(method === "GET" ? {} : { "Content-Type": "application/json" }),
        "X-Cadence-Board": "1",
        ...sessionHeaders(),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    }, op);
    const parsed = (await resp.json().catch(() => null)) as Record<string, unknown> | null;
    op.assertCurrent();
    if (!resp.ok) {
      throw new ApiError(
        errorMessage(parsed, `${resp.status} ${resp.statusText}`.trim()),
        resp.status,
        parsed ?? undefined,
      );
    }
    if (parsed === null || typeof parsed !== "object") {
      throw new ApiError("The server returned an invalid human rooms response", 502);
    }
    return parsed as T;
  }, signal);
}

const ROOMS = "/api/human-threads/rooms";
const roomPath = (id: string) => `${ROOMS}/${encodeURIComponent(id)}`;

/** `GET /api/human-threads/me` — the signed-in person (`{human:{id,name}}`). */
export function humanMe(): Promise<HumanMe> {
  return call<{ human: HumanMe }>("GET", "/api/human-threads/me").then((r) => r.human);
}

/** `GET /api/human-threads/rooms` — the rooms this person belongs to. */
export function listHumanRooms(): Promise<HumanRoomSummary[]> {
  return call<{ rooms: HumanRoomSummary[] }>("GET", ROOMS).then((r) => r.rooms);
}

/** `POST /api/human-threads/dm` — the same pair reopens its room. Returns the room id. */
export function openHumanDm(peerSubject: string): Promise<string> {
  return call<{ room: string }>("POST", "/api/human-threads/dm", { peer_subject: peerSubject }).then((r) => r.room);
}

export interface HumanGroupInput {
  kind: "human_group" | "mixed_group";
  title?: string;
  /** One id per logical envelope (`crypto.randomUUID()`), reused on retry. */
  client_id: string;
  humans: string[];
  agents: string[];
}

/** `POST /api/human-threads/groups` — a new group, never a copy of a DM. */
export function createHumanGroup(input: HumanGroupInput): Promise<string> {
  return call<{ room: string }>("POST", "/api/human-threads/groups", input).then((r) => r.room);
}

function opaqueCursor(value: unknown): value is string {
  return typeof value === "string" && value.startsWith("csc1_") && value.length > 5;
}

function mentionOutcome(value: unknown): value is MentionOutcome {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const o = value as Record<string, unknown>;
  return typeof o.kind === "string"
    && typeof o.handle === "string"
    && typeof o.label === "string"
    && (o.reason === undefined || typeof o.reason === "string");
}

const ATTACHMENT_KINDS = new Set<HumanAttachmentKind>(["image", "video", "pdf", "text", "office"]);

function humanPostAttachment(value: unknown): value is HumanPostAttachment {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const att = value as Record<string, unknown>;
  return typeof att.id === "string" && att.id.length > 0
    && typeof att.name === "string"
    && typeof att.mime === "string" && att.mime.length > 0
    && typeof att.size === "number" && Number.isFinite(att.size) && att.size >= 0
    && typeof att.kind === "string" && ATTACHMENT_KINDS.has(att.kind as HumanAttachmentKind)
    && typeof att.url === "string" && att.url.startsWith("/api/human-threads/rooms/");
}

function humanPost(value: unknown): value is HumanPost {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const post = value as Record<string, unknown>;
  const sender = post.sender;
  if (!sender || typeof sender !== "object" || Array.isArray(sender)) return false;
  const who = sender as Record<string, unknown>;
  return !("seq" in post)
    && typeof post.id === "string" && post.id.length > 0
    && opaqueCursor(post.read_cursor)
    && typeof post.body === "string"
    && typeof post.created === "number"
    && typeof post.silent === "boolean"
    && Array.isArray(post.outcomes) && post.outcomes.every(mentionOutcome)
    && (post.attachments === undefined || (Array.isArray(post.attachments) && post.attachments.every(humanPostAttachment)))
    && (who.id === null || typeof who.id === "string")
    && typeof who.label === "string"
    && (who.kind === undefined || who.kind === "human" || who.kind === "agent");
}

function roomEvent(value: unknown): value is HumanRoomEvent {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const ev = value as Record<string, unknown>;
  return typeof ev.id === "string" && ev.id.length > 0
    && typeof ev.text === "string"
    && typeof ev.created === "number";
}

function invalidRoomResponse(): ApiError {
  return new ApiError("The server returned an invalid human room response", 502);
}

/** `GET /api/human-threads/rooms/<id>` — members and one page of posts. */
export function readHumanRoom(id: string, at: { before?: string; limit?: number } = {}): Promise<HumanRoomPage> {
  const q = new URLSearchParams();
  if (at.before !== undefined) q.set("before", at.before);
  if (at.limit !== undefined) q.set("limit", String(at.limit));
  const query = q.toString();
  return call<HumanRoomPage>("GET", `${roomPath(id)}${query ? `?${query}` : ""}`).then((page) => {
    if (page.room !== id
      || typeof page.scope_id !== "string" || !page.scope_id.startsWith("csrid_") || page.scope_id.length <= 6
      || !Array.isArray(page.members) || !page.members.every((member) => member
        && (member.kind === "human" || member.kind === "agent")
        && typeof member.id === "string" && typeof member.label === "string"
        && (member.look === undefined || member.look === null || typeof member.look === "string"))
      || !Array.isArray(page.posts) || !page.posts.every(humanPost)
      || !Array.isArray(page.events) || !page.events.every(roomEvent)
      || typeof page.can_ask !== "boolean"
      || typeof page.can_manage !== "boolean"
      || typeof page.agent_chain_count !== "number"
      || typeof page.agent_chain_limit !== "number"
      || typeof page.more_before !== "boolean"
      || !(page.before === null || opaqueCursor(page.before))
      || (page.posts.length > 0 && page.before === null)) {
      throw invalidRoomResponse();
    }
    return page;
  });
}

/** `POST /api/human-threads/rooms/<id>/attachments` — upload one room-scoped file or image. */
export function uploadHumanRoomAttachment(
  roomId: string,
  file: File,
  signal?: AbortSignal,
): Promise<HumanPostAttachment> {
  return usingPrivateOperation(undefined, async (op) => {
    const form = new FormData();
    form.set("file", file);
    const resp = await privateFetch(`${roomPath(roomId)}/attachments`, {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      headers: {
        "X-Cadence-Board": "1",
        ...sessionHeaders(),
      },
      body: form,
    }, op);
    const parsed = (await resp.json().catch(() => null)) as Record<string, unknown> | null;
    op.assertCurrent();
    if (!resp.ok) {
      throw new ApiError(
        errorMessage(parsed, `${resp.status} ${resp.statusText}`.trim()),
        resp.status,
        parsed ?? undefined,
      );
    }
    if (!parsed || typeof parsed !== "object" || !humanPostAttachment(parsed.attachment)) {
      throw new ApiError("The server returned an invalid human room attachment response", 502);
    }
    return parsed.attachment;
  }, signal);
}

/** `POST /api/human-threads/rooms/<id>/posts` — post and dispatch active `@mentions` (unless `silent`). */
export function postHumanRoom(
  id: string,
  clientId: string,
  body: string,
  silent = false,
  attachmentIds?: readonly string[],
): Promise<{ post: HumanPost; created: boolean }> {
  const payload = {
    client_id: clientId,
    body,
    ...(silent ? { silent: true } : {}),
    ...(attachmentIds && attachmentIds.length > 0 ? { attachment_ids: [...attachmentIds] } : {}),
  };
  return call<{ post: HumanPost; created: boolean }>(
    "POST",
    `${roomPath(id)}/posts`,
    payload,
  ).then((receipt) => {
    if (!humanPost(receipt.post) || typeof receipt.created !== "boolean") throw invalidRoomResponse();
    return receipt;
  });
}

/** `GET /api/human-threads/agents` — the current agents this person may ask (`{agents:[{uid,alias}]}`). */
export function humanAgents(): Promise<HumanAgent[]> {
  return call<{ agents: HumanAgent[] }>("GET", "/api/human-threads/agents").then((r) => r.agents);
}

/** `POST /api/human-threads/rooms/<id>/ask` — one explicit Ask to one current agent. Queued, not done. */
export function humanAskRoom(
  id: string,
  input: { agent_uid: string; client_id: string; body: string },
): Promise<HumanAskReceipt> {
  return call<HumanAskReceipt>("POST", `${roomPath(id)}/ask`, input).then((receipt) => {
    if (!humanPost(receipt.post) || typeof receipt.duplicate !== "boolean"
      || typeof receipt.request !== "string"
      || typeof receipt.target?.uid !== "string" || typeof receipt.target?.alias !== "string") {
      throw invalidRoomResponse();
    }
    return receipt;
  });
}

/** `POST /api/human-threads/rooms/<id>/read` — the newest post actually shown. */
export function markHumanRead(id: string, cursor: string): Promise<{ unread: number }> {
  return call("POST", `${roomPath(id)}/read`, { cursor });
}

export interface HumanPerson {
  /** Presentation only: `human-` + first 12 bytes of SHA256 over the verified tuple. */
  id: string;
  subject: string;
  name: string;
}

export interface HumanPeoplePage {
  actor: string;
  people: HumanPerson[];
  has_more: boolean;
}

/** The complete verified workspace is searched before the server's 20-result cap. */
export function searchHumanPeople(query: string, signal: AbortSignal): Promise<HumanPeoplePage> {
  const params = new URLSearchParams({ query });
  return call("GET", `/api/human-threads/people?${params}`, undefined, undefined, signal);
}

interface RosterResponse {
  issuer: string;
  organization_id: string;
  members: { subject_id: string; name: string; role: string }[];
}

/**
 * The human id the daemon derives for a verified tuple (store `VerifiedHuman`):
 * SHA256 over UTF-8 `issuer\norg\nsubject`, first 12 bytes as hex. Used only to
 * match a roster person to a room member; the id is never sent as authority.
 */
export async function humanIdFor(issuer: string, org: string, subject: string): Promise<string> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) throw new Error("This origin has no secure crypto, so people cannot be matched here");
  const digest = new Uint8Array(await subtle.digest("SHA-256", new TextEncoder().encode(`${issuer}\n${org}\n${subject}`)));
  return `human-${Array.from(digest.slice(0, 12), (b) => b.toString(16).padStart(2, "0")).join("")}`;
}

/** `GET /__platform/threads/roster` — the current people, same origin. */
export async function humanPeople(): Promise<HumanPerson[]> {
  const roster = await call<RosterResponse>("GET", "/__platform/threads/roster");
  return Promise.all(
    roster.members.map(async (m) => ({
      id: await humanIdFor(roster.issuer, roster.organization_id, m.subject_id),
      subject: m.subject_id,
      name: m.name,
    })),
  );
}

/** `POST /api/human-threads/rooms/<id>/members` — creator only, groups only. */
export function addHumanMember(id: string, target: { human_subject: string } | { agent_uid: string }): Promise<unknown> {
  return call("POST", `${roomPath(id)}/members`, target);
}

/** `DELETE /api/human-threads/rooms/<id>/members/<kind>/<member>` — creator only. */
export function removeHumanMember(id: string, kind: "human" | "agent", member: string): Promise<unknown> {
  return call("DELETE", `${roomPath(id)}/members/${kind}/${encodeURIComponent(member)}`);
}

/** CAD-1423: one authorized received-message history group for the viewer. */
export interface NotificationGroup {
  room: string;
  kind: HumanRoomKind;
  title: string;
  unread: number;
  muted: boolean;
  mention: boolean;
  alert_seq: number;
  latest_sender?: string | null;
  latest_sender_kind?: "human" | "agent" | null;
  latest_preview?: string | null;
  latest_at?: number | null;
}

export interface NotificationPreferences {
  dm: boolean;
  mention: boolean;
  sound: boolean;
  preview: boolean;
  /** `off` or `22-08`: quiet from 22:00 to 08:00 in `zone`. */
  quiet: "off" | "22-08";
  /** The user's IANA time zone. */
  zone: string;
}

export interface NotificationInbox {
  /** The verified current human, returned atomically with the feed. */
  human: HumanMe;
  unread_total: number;
  groups: NotificationGroup[];
  preferences: NotificationPreferences;
  push: { available: boolean; public_key: string | null };
}

/** `GET /api/human-threads/notifications` — the caller's inbox. Private state only. */
export function humanNotifications(): Promise<NotificationInbox> {
  return call("GET", "/api/human-threads/notifications");
}

/** `POST /api/human-threads/notifications/read` — dismiss the inbox; never a thread read. */
export function markNotificationsRead(): Promise<{ unread_total: number }> {
  return call("POST", "/api/human-threads/notifications/read", {});
}

/** `POST /api/human-threads/notifications/preferences` — the verified caller's own preferences. */
export function setNotificationPreferences(prefs: NotificationPreferences): Promise<{ preferences: NotificationPreferences }> {
  return call("POST", "/api/human-threads/notifications/preferences", prefs);
}

/** `POST /api/human-threads/rooms/<id>/mute` — mute or unmute one room; unread is kept. */
export function setRoomMuted(id: string, muted: boolean): Promise<{ muted: boolean }> {
  return call("POST", `${roomPath(id)}/mute`, { muted });
}

/** Refresh this verified device's visible room; presence expires after 30 seconds. */
export function setPushPresence(deviceId: string, tabId: string, room: string | null): Promise<{ accepted: boolean }> {
  return call("POST", "/api/human-threads/push/presence", { device_id: deviceId, tab_id: tabId, room });
}

/** `POST /api/human-threads/push/subscriptions` — enroll this browser endpoint. */
export function enrollPush(sub: { endpoint: string; keys: { p256dh: string; auth: string } }): Promise<{ device_id: string }> {
  return call("POST", "/api/human-threads/push/subscriptions", sub);
}

/** Rebind only an existing consent for this verified human and endpoint. */
export function rebindPush(endpoint: string): Promise<{ device_id: string | null }> {
  return call("POST", "/api/human-threads/push/subscriptions/rebind", { endpoint });
}

/** `DELETE /api/human-threads/push/subscriptions/<device_id>` — `removed` is the explicit result. */
export function unenrollPush(deviceId: string): Promise<{ removed: boolean }> {
  return call("DELETE", `/api/human-threads/push/subscriptions/${encodeURIComponent(deviceId)}`);
}

/**
 * `GET /api/human-threads/agents/:uid/rooms` — non-DM channels that both this
 * hosted agent profile and the signed-in viewer belong to. Older boards or
 * single-user native daemons without this route answer 404/501 and render empty.
 */
export async function humanAgentRooms(uid: string, signal?: AbortSignal): Promise<HumanRoomSummary[]> {
  try {
    const data = await call<{ rooms?: HumanRoomSummary[] }>(
      "GET",
      `/api/human-threads/agents/${encodeURIComponent(uid)}/rooms`,
      undefined,
      undefined,
      signal,
    );
    return Array.isArray(data.rooms) ? data.rooms : [];
  } catch (error) {
    if (error instanceof ApiError && (error.status === 404 || error.status === 501)) {
      return [];
    }
    throw error;
  }
}
