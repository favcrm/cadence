import { ApiError } from "../../lib/api";
import { errorMessage } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation, type PrivateOperation } from "../../lib/privateSession";

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
}

export interface HumanPost {
  id: string;
  seq: number;
  sender: { id: string | null; label: string };
  /** Already sanitized by the daemon (`conversations::body`). Plain text. */
  body: string;
  created: number;
}

export interface HumanRoomPage {
  room: string;
  members: HumanMember[];
  posts: HumanPost[];
  /** Fresh permission hint for an explicit Ask. Missing or false offers no Ask. */
  can_ask?: boolean;
}

/** A current agent on the trusted catalog (`uid` is the durable id, never an alias hash). */
export interface HumanAgent {
  uid: string;
  alias: string;
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

/** `GET /api/human-threads/rooms/<id>` — members and one page of posts. */
export function readHumanRoom(id: string, at: { before?: number; limit?: number } = {}): Promise<HumanRoomPage> {
  const q = new URLSearchParams();
  if (at.before !== undefined) q.set("before", String(at.before));
  if (at.limit !== undefined) q.set("limit", String(at.limit));
  const query = q.toString();
  return call<HumanRoomPage>("GET", `${roomPath(id)}${query ? `?${query}` : ""}`).then((p) => ({
    ...p,
    can_ask: p.can_ask === true,
  }));
}

/** `POST /api/human-threads/rooms/<id>/posts` — a turnless post. */
export function postHumanRoom(id: string, clientId: string, body: string): Promise<{ post: HumanPost; created: boolean }> {
  return call("POST", `${roomPath(id)}/posts`, { client_id: clientId, body });
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
  return call<HumanAskReceipt>("POST", `${roomPath(id)}/ask`, input);
}

/** `POST /api/human-threads/rooms/<id>/read` — the newest seq actually shown. */
export function markHumanRead(id: string, seq: number): Promise<{ unread: number }> {
  return call("POST", `${roomPath(id)}/read`, { seq });
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
