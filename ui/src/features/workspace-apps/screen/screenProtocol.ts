/**
 * CAD-1006 screen bridge protocol — the closed typed v1 wire between the
 * trusted host (`ScreenOutlet`) and an opaque sandboxed app frame.
 *
 * Two nonce concepts are kept strictly distinct:
 *
 *  - **FrameCap** — the 256-bit bearer in the frame's URL
 *    (`/api/app-screen/<nonce>`), minted operator-only by the daemon,
 *    one-use, 60 s TTL, bound to the live session + digest. It *is* the
 *    authority for the document fetch. It is never sent to the child and
 *    never appears in any message.
 *  - **Bridge nonce** — a separate non-authorizing random 64-hex token the
 *    host stamps into the served document's bootstrap JSON. The child
 *    echoes it in its init to prove *which mount* the port belongs to. It
 *    grants nothing by itself; single-init + source + generation are the
 *    real guards.
 *
 * The child is opaque-origin (`sandbox="allow-scripts"`): `event.origin`
 * on its init is the literal string `"null"`. The child posts its init to
 * the EXACT parent origin (`boot.parent_origin`) — never `"*"`.
 */

/** The bootstrap JSON the host stamps into the served document, read by
 *  the child from the `application/json` block `id="cadence-screen-boot"`.
 *  `generation` is a non-negative safe-integer JSON number — the host's
 *  current mount identity for this `(install_id, tag)`, bound to the cap
 *  at mint time. `parent_origin` is the server-derived exact board origin
 *  the child must postMessage to. */
export interface ScreenBoot {
  tag: string;
  bridge_nonce: string;
  generation: number;
  parent_origin: string;
}

/** The child's one init envelope — a window `message` event whose ports[0]
 *  is the fresh `MessagePort`. Field names are exact; the host matches
 *  `tag`, `bridge_nonce` and `generation` against the live mount. */
export interface ScreenInit {
  v: 1;
  op: "init";
  tag: string;
  bridge_nonce: string;
  generation: number;
}

/** The only two messages the child may ever send over the port. Anything
 *  else closes the port. `state` is the child's opaque draft snapshot
 *  (≤ 32 KiB string), held host-side in memory only. */
export type ChildToHost =
  | { v: 1; op: "ready" }
  | { v: 1; op: "state"; data: string };

/** The host's only PUSH: the closed read-only projection of the verified
 *  scope. Every identity field is host-stamped — the child can never
 *  choose or widen scope. Fields that are unavailable are omitted, never
 *  fabricated: a run with no real timestamps omits them, and artifact
 *  bytes are absent in MVP rather than filled with placeholders. */
export interface ScreenPush {
  v: 1;
  op: "screen";
  /** Host-stamped install id — the verified route's install. */
  install_id: string;
  /** Live bundle digest the mount was proved against. */
  digest: string;
  /** The mounted screen tag. */
  tag: string;
  /** Host-stamped context id; `""` = no context scope. */
  context_id: string;
  /** Verified installation receipt fields, minimally. */
  installation: {
    install_id: string;
    name: string;
    title: string;
    version: string;
    digest: string;
    summary: string;
  };
  /** Active contexts as `{id,label}` only. */
  contexts: { id: string; label: string }[];
  /** Runs inside the pushed scope — real ids/states/titles only. A run's
   *  `created`/`closed` are real epoch seconds or omitted when unknown. */
  runs: {
    id: string;
    state: string;
    title: string;
    snapshot_digest: string;
    workflow: { title: string };
    created?: number;
    closed?: number;
  }[];
  /** Scoped effects/outbox/calendar-intent receipts — real rows only. */
  outbox: {
    effect_id: string;
    state: string;
    title: string;
    scheduled_at?: number;
  }[];
  /** Server epoch seconds for day-row alignment. */
  now: number;
  /** The child's own last `state` snapshot, replayed on remount. */
  resume?: string;
}

export type HostToChild = ScreenPush;

/** Parse a window message into a `ScreenInit`, or `null`. The port is a
 *  `MessagePort` in `event.ports[0]` — the caller checks `event.origin`
 *  and `event.source` first; this only checks the envelope shape. */
export function parseInit(data: unknown): ScreenInit | null {
  if (typeof data !== "object" || data === null || Array.isArray(data)) return null;
  const d = data as Record<string, unknown>;
  if (
    Object.keys(d).sort().join() === "bridge_nonce,generation,op,tag,v" &&
    d.v === 1 &&
    d.op === "init" &&
    typeof d.tag === "string" && /^[a-z0-9][a-z0-9-]{0,31}$/.test(d.tag) &&
    typeof d.bridge_nonce === "string" && /^[a-f0-9]{64}$/.test(d.bridge_nonce) &&
    typeof d.generation === "number" &&
    Number.isSafeInteger(d.generation) &&
    d.generation >= 0
  ) {
    return {
      v: 1,
      op: "init",
      tag: d.tag,
      bridge_nonce: d.bridge_nonce,
      generation: d.generation,
    };
  }
  return null;
}

/** Parse a port message into a `ChildToHost`, or `null` (caller closes
 *  the port). `state.data` is bounded at ≤ 32 KiB here so a hostile or
 *  buggy child cannot push an unbounded snapshot. */
export function parseChild(data: unknown): ChildToHost | null {
  if (typeof data !== "object" || data === null || Array.isArray(data)) return null;
  const d = data as Record<string, unknown>;
  if (Object.keys(d).sort().join() === "op,v" && d.v === 1 && d.op === "ready") {
    return { v: 1, op: "ready" };
  }
  if (
    Object.keys(d).sort().join() === "data,op,v" &&
    d.v === 1 &&
    d.op === "state" &&
    typeof d.data === "string" &&
    d.data.length <= 32 * 1024 &&
    new TextEncoder().encode(d.data).byteLength <= 32 * 1024
  ) {
    return { v: 1, op: "state", data: d.data };
  }
  return null;
}
