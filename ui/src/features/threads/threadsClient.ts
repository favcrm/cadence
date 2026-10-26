import { ApiError } from "../../lib/api";
import { errorMessage } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation, type PrivateOperation } from "../../lib/privateSession";

/**
 * CAD-818 Threads wire client. Shapes follow the IMPLEMENTED daemon
 * responses (backend contract checkpoint, /tmp/cad818-backend-contract.md),
 * not the earlier draft. Authority is the daemon's: the operator is derived
 * from the connection, never from a field sent here. Ids (`conv-…`,
 * `topic-…`, `post-…`) are opaque strings and are never built client-side.
 * The app conversation namespace (`/api/app-installations/…`) is separate
 * and not touched.
 */

/**
 * The server's `threads` stream frame (CAD-818) names no data; it means the
 * thread list and the open timeline are stale. A revision counter lets the
 * Threads page subscribe without new polling or a second stream.
 */
let threadsRevision = 0;
const threadsListeners = new Set<() => void>();
export function noteThreadsChanged(): void {
  threadsRevision++;
  for (const listener of threadsListeners) listener();
}
export function subscribeThreadsChanged(listener: () => void): () => void {
  threadsListeners.add(listener);
  return () => {
    threadsListeners.delete(listener);
  };
}
export function threadsRevisionNow(): number {
  return threadsRevision;
}

export type ParticipantKind = "agent" | "operator" | "daemon" | "unknown";

export interface Participant {
  /** Durable identity; null for a legacy row with no recorded sender. */
  uid: string | null;
  /** Display label only. */
  alias: string | null;
  kind: ParticipantKind;
  removed: boolean;
}

/** A message's delivery stage. `queued` and `available` are not seen; `available`
 *  means the notice is readable in the inbox, not that it was seen. A topic
 *  delivery is `pending` until its message is queued. */
export type DeliveryState =
  | "pending"
  | "queued"
  | "available"
  | "refused"
  | "claimed"
  | "running"
  | "completed"
  | "failed"
  | "unknown"
  | "interrupted";

/** `unknown` is the legacy bucket: rows with no recorded sender. */
export type ConversationKind = "pair" | "operator" | "group" | "unknown";

export interface ConversationCounts {
  queued: number;
  claimed: number;
  running: number;
  completed: number;
  failed: number;
  unknown: number;
}

/** One list row from `work_conversation_list`. Operator and group rows carry
 *  no `last` or `counts`; the legacy `unknown` row carries counts only. */
export interface ConversationRow {
  id: string;
  kind: ConversationKind;
  participants: Participant[];
  last?: { preview: string; at: number } | null;
  counts?: ConversationCounts;
  last_activity: number;
  /** Group rows: the topic type, its source and its post count. */
  type?: string;
  source?: string;
  posts?: number;
}

export interface ConversationListFilter {
  project?: string;
  agent?: string;
  include_system?: boolean;
}

/** One routed copy nested under the message it answers (`origin`). */
export interface RoutedCopy {
  id: string;
  seq: number;
  origin: string | null;
  /** The recorded send path (`worker_result`, `worker_notice`, `job_event`). */
  source: string;
  state: DeliveryState | string;
  error: string | null;
  created: number;
}

/** One message from `work_conversation_read` (FINAL). `issue` and
 *  `task_id` are the existing refs when present, else null. `origin`
 *  marks a routed copy: the id of the message it answers. `routed`
 *  lists the copies that answer this message. */
export interface ThreadMessage {
  id: string;
  seq: number;
  conversation: string;
  sender: Participant;
  origin: string | null;
  /** The recorded recipient; `kind: "unknown"` when none was recorded. */
  recipient?: Participant | null;
  /** The recorded send path (`user`, `worker_result`, …) — evidence,
   *  never a claim of intent. */
  source?: string;
  /** Routed copies that answered this message, when the daemon sent any. */
  routed?: RoutedCopy[];
  /** The daemon's stored refusal/failure text, when one was recorded. */
  error?: string | null;
  body: string;
  state: DeliveryState;
  issue: string | null;
  task_id: string | null;
  created: number;
  started: number | null;
  completed: number | null;
}

/** `work_conversation_read` page. `conversation` is the pair object. */
export interface ConversationPage {
  conversation: { id: string; kind: "pair"; participants: Participant[] };
  messages: ThreadMessage[];
  cursor: number;
  more_before: boolean;
}

/** One subscriber's delivery of a post. `message` is null while pending;
 *  `state` is the queued message row's live state, never shown as seen. */
export interface PostDelivery {
  uid: string;
  alias: string;
  removed: boolean;
  message: string | null;
  state: DeliveryState;
}

/** One transcript row of a topic, with its per-subscriber deliveries. */
export interface TopicPost {
  id: string;
  seq: number;
  sender: { uid: string | null; alias: string };
  body: string;
  created: number;
  /** The queued request this post is bound to (an Ask), or null. */
  request: string | null;
  deliveries: PostDelivery[];
}

/** `topic_read` page. `topic` is the topic object, not its id. */
export interface TopicPage {
  topic: { id: string; type: string; source: string; created: number };
  posts: TopicPost[];
  cursor: number;
  more_before: boolean;
}

export interface ReadPageOptions {
  tail?: boolean;
  before?: number;
  after?: number;
  limit?: number;
}

/** `topic_publish` receipt. `deliveries` is set on a fresh post only. */
export interface TopicPostReceipt {
  topic: string;
  message: string;
  duplicate: boolean;
  deliveries: { alias: string; uid: string; message: string | null; state: DeliveryState }[];
}

/** `topic_request` answers the target and the queued request id only. */
export interface TopicRequestReceipt {
  topic: string;
  message: string;
  request: string;
  target: { alias: string; uid: string };
  duplicate: boolean;
}

/** Client-side refusal of a malformed request, before any byte is sent. */
function invalid(message: string): never {
  throw new ApiError(message, 400, { code: "invalid_request" });
}

const part = encodeURIComponent;

async function send<T>(method: "GET" | "POST", path: string, body?: object, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(path, {
      method,
      credentials: "same-origin",
      cache: "no-store",
      headers: {
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
        "X-Cadence-Board": "1",
        ...sessionHeaders(),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    }, op);
    const parsed = await resp.json().catch(() => null);
    op.assertCurrent();
    if (!resp.ok) {
      throw new ApiError(
        errorMessage(parsed, `${resp.status} ${resp.statusText}`.trim()),
        resp.status,
        (parsed as Record<string, unknown> | null) ?? undefined,
      );
    }
    if (parsed === null || typeof parsed !== "object") {
      throw new ApiError("The server returned an invalid Threads response", 502);
    }
    return parsed as T;
  });
}

/** The daemon's cursor: `before` or `after` a seq, else the newest page. */
function pageQuery(at: ReadPageOptions): string {
  const q = new URLSearchParams();
  if (at.before !== undefined) q.set("before", String(at.before));
  else if (at.after !== undefined) q.set("after", String(at.after));
  else q.set("tail", "1");
  q.set("limit", String(at.limit ?? 50));
  return q.toString();
}

/** What a backward page says about its own scan: its continuation boundary
 *  and whether raw rows remain below it. */
export interface BackwardPage {
  /** The LOWEST seq the page scanned — older history continues below it. */
  scanned_before?: number | null;
  /** Raw rows remain below the boundary (a page can keep nothing). */
  more_before?: boolean | null;
}

/**
 * Where a backward window continues, and whether more history exists below.
 *
 * The continuation is the boundary the page SCANNED — `scanned_before` — and
 * never the oldest row it KEPT: a projection (or a redaction) can filter
 * every row of a page, and a kept-row cursor would then re-fetch that same
 * hidden page forever, so "Load earlier" would never reach the history below
 * it. `more_before` is the daemon's exhaustion answer and is only withheld
 * when the boundary cannot advance (a daemon that predates `scanned_before`,
 * or a page that scanned nothing) — an end-of-history verdict is never
 * inferred from visible rows alone.
 */
export function earlierBoundary(
  page: BackwardPage,
  oldestKept: number | null,
): { before: number | null; more: boolean } {
  const scanned =
    typeof page.scanned_before === "number" &&
    Number.isSafeInteger(page.scanned_before) &&
    page.scanned_before > 0
      ? page.scanned_before
      : null;
  const before = scanned ?? oldestKept;
  return { before, more: page.more_before === true && before !== null };
}

/** `GET /api/work-conversations` — read-only. */
export function listWorkConversations(filter: ConversationListFilter = {}): Promise<{ conversations: ConversationRow[] }> {
  const q = new URLSearchParams();
  if (filter.project) q.set("project", filter.project);
  if (filter.agent) q.set("agent", filter.agent);
  if (filter.include_system) q.set("include_system", "1");
  const query = q.toString();
  return send("GET", `/api/work-conversations${query ? `?${query}` : ""}`);
}

/** `GET /api/work-conversations/<id>` — read-only; never drains a queue.
 *  The list's filter is forwarded so the timeline matches the list's scope. */
export function readWorkConversation(
  id: string,
  at: ReadPageOptions = {},
  filter: ConversationListFilter = {},
): Promise<ConversationPage> {
  if (!id) invalid("a conversation id is required");
  const q = new URLSearchParams(pageQuery(at));
  if (filter.project) q.set("project", filter.project);
  if (filter.agent) q.set("agent", filter.agent);
  if (filter.include_system) q.set("include_system", "1");
  return send("GET", `/api/work-conversations/${part(id)}?${q.toString()}`);
}

/** `GET /api/topics/<id>` — the shared transcript page. */
export function readTopic(id: string, at: ReadPageOptions = {}): Promise<TopicPage> {
  if (!id) invalid("a topic id is required");
  return send("GET", `/api/topics/${part(id)}?${pageQuery(at)}`);
}

/** `POST /api/topics` — the operator creates a `fleet:` or `project:` topic.
 *  `topic` is a plain id string; `created` is false on a repeat. */
export function createTopic(input: { type: "fleet" | "project"; source: string }): Promise<{ topic: string; type: string; source: string; created: boolean }> {
  const source = input.source.trim();
  if (!source) invalid("a topic needs a source");
  return send("POST", "/api/topics", { type: input.type, source });
}

/** `POST /api/topics/<id>/subscribers` — add or remove one member by alias. */
export function changeSubscriber(topic: string, action: "subscribe" | "unsubscribe", alias: string): Promise<unknown> {
  return send("POST", `/api/topics/${part(topic)}/subscribers`, { action, alias });
}

/** `POST /api/topics/<id>/posts` — a turnless notice. `id` is the idempotence key. */
export function postToTopic(topic: string, text: string, id: string): Promise<TopicPostReceipt> {
  const body = text.trim();
  if (!body) invalid("nothing to post");
  return send("POST", `/api/topics/${part(topic)}/posts`, { text: body, id });
}

/** `POST /api/topics/<id>/requests` — one explicit target; the server fans out nothing. */
export function requestFromTopic(topic: string, target: string, text: string, id: string): Promise<TopicRequestReceipt> {
  const body = text.trim();
  if (!body) invalid("nothing to ask");
  return send("POST", `/api/topics/${part(topic)}/requests`, { target, text: body, id });
}

/** HTTP 501 means the daemon predates these verbs (the `/api/threads` convention). */
export function predatesThreads(error: unknown): boolean {
  return error instanceof ApiError && error.status === 501;
}
