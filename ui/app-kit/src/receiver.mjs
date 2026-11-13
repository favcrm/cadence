/**
 * The app side of the `app-screens` bridge, implemented once.
 *
 * Two zero-dependency layers:
 *  1. `createScreenReceiver` — transport and lifecycle: boot validation, single
 *     window `init` port transfer, `ready` capability negotiation, `call`/`slot`/
 *     `tool`/`asset` bookkeeping, drawer surfaces + context, scoped `leave`
 *     ownership, bounded `state` snapshots, and teardown.
 *  2. `createScreenBridge` — reactive `screen.v2` store (`subscribe` + `getState`),
 *     closed `ScreenPush` validation, host drawer verb separation, per-verb
 *     argument allowlists and byte budgets, concurrency limits, reply size and
 *     secret-key guards, write vs read retirement, and route patching.
 *
 * Wire shapes follow `ui/src/features/workspace-apps/screen/screenProtocol.ts`.
 */
import { ScreenBridgeError, ScreenClosedError, ScreenRefusalError, ScreenTimeoutError, readRefusal } from "./refusal.mjs";

export const MAX_STATE_BYTES = 32 * 1024;
/** The encoded allowance for a 96 KiB image (base64 + prefix), matching
 *  `ASSET_BYTES_MAX` and the reference receiver's `MAX_DATA_URL`. */
export const MAX_ASSET_BYTES = Math.ceil((96 * 1024 * 4) / 3) + 32;
export const DEFAULT_TIMEOUT_MS = 15_000;
export const DRAWER_CONTEXT_BYTES = 1024;
export const MAX_PUSH_BYTES = 128 * 1024;
export const MAX_REPLY_DATA_BYTES = 1024 * 1024;
export const MAX_AUDIO_TRANSCRIPT_CHARS = 200_000;
export const MAX_AUDIO_UPLOAD_BYTES = 50 * 1024 * 1024;
export const SCREEN_AUDIO_V1 = "screen-audio.v1";
export const SCREEN_AUDIO_V2 = "screen-audio.v2";
export const HOST_DRAWER_VERBS = Object.freeze(["screen.drawer.open", "screen.drawer.close", "screen.drawer.context"]);
export const ROUTABLE_ID = /^[a-z0-9_-]{1,64}$/;
export const ROUTE_RECORD_ID = /^[A-Za-z0-9_-]{1,128}$/;

const BOOT_ID = "cadence-screen-boot";
const BOOT_KEYS = ["bridge_nonce", "generation", "parent_origin", "tag"];
const TAG = /^[a-z0-9][a-z0-9-]{0,31}$/;
const NONCE = /^[0-9a-f]{64}$/;
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const SAFE_ID = /^[A-Za-z0-9_.:-]{1,128}$/;
const DNS_LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
// Wire bounds from `screenProtocol.ts`: ACTION_ID, TOOL_ALIAS,
// TOOL_REQUEST_ID, ACTION_ARGS_BYTES and COORD_MAX. The receiver enforces the
// shape; which verbs an app may send stays the app's closed set.
const ACTION_ID = /^[A-Za-z0-9_-]{1,64}$/;
const PUSH_ACTION_TOKEN = /^[a-z][a-z0-9_.-]{0,127}$/;
const DRAWER_SURFACE = /^[a-z0-9_-]{1,64}$/;
const TOOL_ALIAS = /^[a-z][a-z0-9]{0,31}(\.[a-z][a-z0-9]{0,31}){0,3}$/;
const TOOL_REQUEST_ID = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/;
const ACTION_ARGS_BYTES = 16 * 1024;
const OBSERVATION_ROW_BYTES = 64 * 1024;
const ASSET_DATA_URL = /^data:image\/(jpeg|png|webp);base64,[A-Za-z0-9+/]+=*$/;
const COORD_MAX = 16384;
const SECRET_KEYS = Object.freeze(["confirmation_nonce", "confirmation_token", "confirm_token", "action_token"]);
const DEFAULT_PUSH_KEYS = Object.freeze([
  "v", "op", "install_id", "context_id", "digest", "tag", "installation", "contexts",
  "runs", "outbox", "publish_intents", "sources", "defaults", "publish_settings",
  "readiness", "actions", "run_receipts", "original_downloads", "media_previews",
  "media_chat_references", "audio_capture", "now", "resume", "route", "read_observations", "presentation_locale",
]);

function fail(message) {
  throw new TypeError(message);
}

/** The one http parent the host serves: `http://<name>.localhost:<port>`.
 *  No IP literals, no bare host without a port, no credentials (checked by
 *  the caller), and the origin must round-trip exactly. */
function loopbackHttp(origin) {
  if (origin.protocol !== "http:" || origin.port === "" || Number(origin.port) < 1) return false;
  const labels = origin.hostname.split(".");
  return labels.at(-1) === "localhost" && labels.every((label) => DNS_LABEL.test(label)) && origin.hostname.length <= 253;
}

export function validateBoot(value, expectedTag) {
  if (expectedTag !== undefined && (typeof expectedTag !== "string" || !TAG.test(expectedTag))) {
    fail("expected tag must be 1-32 lowercase letters, digits or dashes, not leading with a dash");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) fail("boot must be an object");
  const keys = Object.keys(value);
  if (keys.length !== BOOT_KEYS.length || keys.some((key) => !BOOT_KEYS.includes(key))) {
    fail("boot keys must be exactly bridge_nonce, generation, parent_origin, tag");
  }
  if (!TAG.test(value.tag ?? "")) fail("boot tag must be 1-32 lowercase letters, digits or dashes, not leading with a dash");
  if (expectedTag !== undefined && value.tag !== expectedTag) fail(`boot tag mismatch: expected ${expectedTag}, got ${value.tag}`);
  if (!NONCE.test(value.bridge_nonce ?? "")) fail("boot bridge_nonce must be 64 lowercase hex");
  if (typeof value.generation !== "number" || !Number.isSafeInteger(value.generation) || value.generation < 0) {
    fail("boot generation must be a non-negative safe integer");
  }
  let origin;
  try {
    origin = new URL(value.parent_origin);
  } catch {
    fail("boot parent_origin must be an absolute origin");
  }
  if (origin.username || origin.password) fail("boot parent_origin must not carry credentials");
  if (origin.origin !== value.parent_origin) fail("boot parent_origin must be an origin, not a path");
  if (!(origin.protocol === "https:" || loopbackHttp(origin))) {
    fail("boot parent_origin must be https or the host's loopback http origin");
  }
  return Object.freeze({
    tag: value.tag,
    bridge_nonce: value.bridge_nonce,
    generation: value.generation,
    parent_origin: origin.origin,
  });
}

export function readBoot(documentRef = globalThis.document, expectedTag) {
  const node = documentRef?.getElementById?.(BOOT_ID);
  if (!node) fail(`no #${BOOT_ID} element`);
  const raw = node.textContent ?? "";
  if (raw.length > 2048) fail(`#${BOOT_ID} exceeds 2 KiB`);
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    fail(`#${BOOT_ID} is not JSON`);
  }
  return validateBoot(parsed, expectedTag);
}

const isPlain = (value) => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
};
const exactKeys = (value, allowed) => Object.keys(value).every((key) => allowed.includes(key));

/** A plain-object args bag within the host's byte bound, or `null`. */
function validArgs(args, limit = ACTION_ARGS_BYTES) {
  if (!isPlain(args)) return null;
  let text;
  try {
    text = JSON.stringify(args);
  } catch {
    return null;
  }
  return text !== undefined && byteLength(text) <= limit ? args : null;
}

/** SOC-55: bounded plain-JSON check matching `drawerContextBytes` in `screenProtocol.ts`. */
function validDrawerContext(value) {
  const ancestors = new Set();
  let remaining = DRAWER_CONTEXT_BYTES;
  const check = (item) => {
    if (--remaining < 0) return false;
    if (item === null || typeof item === "boolean") return true;
    if (typeof item === "string") return item.length <= DRAWER_CONTEXT_BYTES;
    if (typeof item === "number") return Number.isFinite(item);
    if (typeof item !== "object" || ancestors.has(item)) return false;
    const array = Array.isArray(item);
    if ((!array && !isPlain(item)) || (array && item.length > DRAWER_CONTEXT_BYTES)) return false;
    ancestors.add(item);
    const ok = array
      ? item.every(check)
      : Object.keys(item).every((key) => key.length <= DRAWER_CONTEXT_BYTES && check(item[key]));
    ancestors.delete(item);
    return ok;
  };
  try {
    if (!check(value)) return false;
    const text = JSON.stringify(value);
    return typeof text === "string" && byteLength(text) <= DRAWER_CONTEXT_BYTES;
  } catch {
    return false;
  }
}

/** CAD-1365: the optional revision the host binds a generation tool to.
 *  Exact keys; `draft_id` is an ACTION_ID, `revision` a positive safe int. */
function validGenerationScope(scope) {
  if (!isPlain(scope) || Object.keys(scope).sort().join() !== "draft_id,operation,revision") return null;
  if (!["caption", "image"].includes(scope.operation)) return null;
  if (typeof scope.draft_id !== "string" || !ACTION_ID.test(scope.draft_id)) return null;
  if (!Number.isSafeInteger(scope.revision) || scope.revision < 1) return null;
  return scope;
}

/** `"footer"` or an exact `{x,y,w,h}` rect inside the frame coordinates. */
function validAnchor(anchor) {
  if (anchor === "footer") return anchor;
  if (!isPlain(anchor) || Object.keys(anchor).sort().join() !== "h,w,x,y") return null;
  const ok = [anchor.x, anchor.y, anchor.w, anchor.h].every(
    (n) => typeof n === "number" && Number.isFinite(n) && n >= 0 && n <= COORD_MAX,
  );
  return ok && anchor.w > 0 && anchor.h > 0 ? anchor : null;
}

/** Closed `audio-reply` payload matching `parseScreenAudioCapture` in `screenProtocol.ts`. */
function validAudioCaptureData(value) {
  if (!isPlain(value) || Object.keys(value).sort().join() !== "duration_seconds,held_at,source,transcript") return null;
  if (typeof value.transcript !== "string" || value.transcript.length > MAX_AUDIO_TRANSCRIPT_CHARS) return null;
  if (
    typeof value.duration_seconds !== "number" ||
    !Number.isSafeInteger(value.duration_seconds) ||
    value.duration_seconds < 0 ||
    value.duration_seconds > 86_400
  ) {
    return null;
  }
  if (typeof value.held_at !== "string" || value.held_at.length === 0 || value.held_at.length > 64) return null;
  if (value.source !== "recording" && value.source !== "upload") return null;
  return Object.freeze({
    transcript: value.transcript,
    duration_seconds: value.duration_seconds,
    held_at: value.held_at,
    source: value.source,
  });
}

const AUDIO_SESSION_PHASES = new Set([
  "idle", "starting", "recording", "paused", "stopping", "transcribing", "ready", "error",
]);
const AUDIO_SESSION_SNAPSHOT_KEYS =
  "elapsed_seconds,error,held_at,level,mode,no_speech_segments,peak,phase,segments_done,segments_failed,segments_in_flight,segments_total,session_id,transcript";

function isSafeBoundedInt(value, min, max) {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= min && value <= max;
}

function isCrossRealmArrayBuffer(value) {
  return (
    typeof value === "object" &&
    value !== null &&
    Object.prototype.toString.call(value) === "[object ArrayBuffer]" &&
    typeof value.byteLength === "number"
  );
}

/** Closed `audio-session-reply` / `audio-session-event` payload matching `parseScreenAudioSessionSnapshot`. */
function validAudioSessionSnapshot(value) {
  if (!isPlain(value) || Object.keys(value).sort().join() !== AUDIO_SESSION_SNAPSHOT_KEYS) return null;
  if (typeof value.session_id !== "string" || !/^[A-Za-z0-9_-]{0,64}$/.test(value.session_id)) return null;
  if (value.mode !== "record" && value.mode !== "upload") return null;
  if (typeof value.phase !== "string" || !AUDIO_SESSION_PHASES.has(value.phase)) return null;
  if (typeof value.held_at !== "string" || value.held_at.length > 64 || (value.phase !== "idle" && value.held_at.length === 0)) {
    return null;
  }
  if (!isSafeBoundedInt(value.elapsed_seconds, 0, 86_400)) return null;
  if (!isSafeBoundedInt(value.level, 0, 100) || !isSafeBoundedInt(value.peak, 0, 100)) return null;
  if (
    !isSafeBoundedInt(value.segments_total, 0, 10_000) ||
    !isSafeBoundedInt(value.segments_done, 0, 10_000) ||
    !isSafeBoundedInt(value.segments_in_flight, 0, 10_000) ||
    !isSafeBoundedInt(value.segments_failed, 0, 10_000) ||
    !isSafeBoundedInt(value.no_speech_segments, 0, 10_000)
  ) {
    return null;
  }
  if (typeof value.transcript !== "string" || value.transcript.length > MAX_AUDIO_TRANSCRIPT_CHARS) return null;
  let parsedError = null;
  if (value.error !== null) {
    const refusal = readRefusal(value.error);
    if (!refusal || Object.keys(value.error).sort().join() !== "code,text") return null;
    parsedError = Object.freeze({ code: refusal.code, text: refusal.text });
  }
  return Object.freeze({
    session_id: value.session_id,
    mode: value.mode,
    phase: value.phase,
    held_at: value.held_at,
    elapsed_seconds: value.elapsed_seconds,
    level: value.level,
    peak: value.peak,
    segments_total: value.segments_total,
    segments_done: value.segments_done,
    segments_in_flight: value.segments_in_flight,
    segments_failed: value.segments_failed,
    no_speech_segments: value.no_speech_segments,
    transcript: value.transcript,
    error: parsedError,
  });
}

function byteLength(value) {
  try {
    return new TextEncoder().encode(value).length;
  } catch {
    return value.length * 3;
  }
}

function inert(reason) {
  const refuse = () => Promise.reject(new ScreenClosedError(`screen bridge is closed: ${reason}`));
  return Object.freeze({
    boot: null,
    accepted: () => false,
    call: refuse,
    slot: refuse,
    tool: refuse,
    audioCapture: refuse,
    audioSession: refuse,
    asset: refuse,
    assetAdmitted: () => false,
    state: () => false,
    ownLeaveState: () => null,
    presentation: () => "outlet",
    surfaceRole: () => "primary",
    surface: () => undefined,
    openDrawer: refuse,
    closeDrawer: refuse,
    drawerContext: refuse,
    teardown() {},
  });
}

export function createScreenReceiver(options = {}) {
  const {
    tag: expectedTag,
    onPush,
    onError,
    onPresentation,
    onSurface,
    accepts = ["screen.v2"],
    maxArgsBytes = ACTION_ARGS_BYTES,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    onSlotState,
    onAudioSessionEvent,
    document: documentRef = globalThis.document,
    parent = globalThis.parent,
    window: windowRef = globalThis.window,
    MessageChannel: Channel = globalThis.MessageChannel,
  } = options;
  if (typeof onPush !== "function") throw new TypeError("onPush is required");
  if (expectedTag !== undefined && (typeof expectedTag !== "string" || !TAG.test(expectedTag))) {
    throw new TypeError("tag must be 1-32 lowercase letters, digits or dashes, not leading with a dash");
  }
  const ACCEPTED = new Set(["screen.v1", "screen.v2", "screen-actions.v1", "screen-media.v1", "screen-presentation.v1", "screen-surfaces.v1", "screen-records.v1", "screen-locale.v1", SCREEN_AUDIO_V1, SCREEN_AUDIO_V2]);
  if (
    !Array.isArray(accepts) ||
    accepts.length === 0 ||
    accepts.some((value) => !ACCEPTED.has(value)) ||
    !accepts.some((value) => value === "screen.v1" || value === "screen.v2")
  ) {
    throw new TypeError("accepts must name screen.v1 or screen.v2, optionally screen-actions.v1, screen-media.v1, screen-presentation.v1, screen-surfaces.v1, screen-records.v1, screen-locale.v1, screen-audio.v1 or screen-audio.v2");
  }
  if (accepts.includes(SCREEN_AUDIO_V1) && accepts.includes(SCREEN_AUDIO_V2)) {
    throw new TypeError("accepts cannot combine screen-audio.v1 and screen-audio.v2");
  }
  const actionsAccepted = accepts.includes("screen-actions.v1");
  if (actionsAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-actions.v1 needs screen.v2");
  }
  const mediaAccepted = accepts.includes("screen-media.v1");
  if (mediaAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-media.v1 needs screen.v2");
  }
  const presentationAccepted = accepts.includes("screen-presentation.v1");
  if (presentationAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-presentation.v1 needs screen.v2");
  }
  // CAD-1777 — the host mounts this frame as its own overlay surface: a
  // `primary` stays in its layout box, a `drawer` lives in the shell's fixed
  // panel. The role is host-derived; a frame never claims one.
  const surfacesAccepted = accepts.includes("screen-surfaces.v1");
  if (surfacesAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-surfaces.v1 needs screen.v2");
  }
  const recordsAccepted = accepts.includes("screen-records.v1");
  if (recordsAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-records.v1 needs screen.v2");
  }
  const localeAccepted = accepts.includes("screen-locale.v1");
  if (localeAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-locale.v1 needs screen.v2");
  }
  const audioAccepted = accepts.includes(SCREEN_AUDIO_V1);
  if (audioAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-audio.v1 needs screen.v2");
  }
  const audioV2Accepted = accepts.includes(SCREEN_AUDIO_V2);
  if (audioV2Accepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-audio.v2 needs screen.v2");
  }
  // `screen.v1` is a local PUSH filter. The host's ready envelope admits only
  // v2 contract names, and a v1-only mount sends the bare ready. The host
  // accepts at most six ready capabilities; refuse here rather than let the
  // host close a mount for an envelope it never admits.
  const readyAccepts = ["screen.v2",
    ...(actionsAccepted ? ["screen-actions.v1"] : []),
    ...(mediaAccepted ? ["screen-media.v1"] : []),
    ...(presentationAccepted ? ["screen-presentation.v1"] : []),
    ...(surfacesAccepted ? ["screen-surfaces.v1"] : []),
    ...(recordsAccepted ? ["screen-records.v1"] : []),
    ...(localeAccepted ? ["screen-locale.v1"] : []),
    ...(audioAccepted ? [SCREEN_AUDIO_V1] : []),
    ...(audioV2Accepted ? [SCREEN_AUDIO_V2] : [])];
  if (readyAccepts.length > 6) throw new TypeError("the host admits at most six ready capabilities");
  const ready = accepts.includes("screen.v2") ? { v: 1, op: "ready", accepts: readyAccepts } : { v: 1, op: "ready" };
  if (!Number.isSafeInteger(maxArgsBytes) || maxArgsBytes <= 0) throw new TypeError("maxArgsBytes must be a positive safe integer");
  const argsLabel = maxArgsBytes === ACTION_ARGS_BYTES ? "16 KiB" : `${maxArgsBytes} bytes`;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new TypeError("timeoutMs must be positive");
  if (!parent || typeof parent.postMessage !== "function") throw new TypeError("a parent window is required");
  if (typeof Channel !== "function") throw new TypeError("MessageChannel is unavailable");

  if (options.parent === undefined && windowRef && parent === windowRef) {
    const reason = "boot refused: screen must run inside a child frame";
    onError?.(reason);
    return inert(reason);
  }

  let boot;
  try {
    boot = validateBoot(options.boot ?? readBoot(documentRef), expectedTag);
  } catch (error) {
    const reason = `boot refused: ${error.message}`;
    onError?.(reason);
    return inert(reason);
  }

  const channel = new Channel();
  const port = channel.port1;
  const pending = new Map(); // request id -> { resolve, reject, timer }
  const sent = new Map(); // id -> kind, kept for late/replayed replies
  const assets = new Map(); // ref -> resolve
  // CAD-1921: refs whose request timed out. The host may still answer one late;
  // that answer is dropped once instead of tearing the bridge down. Bounded by the
  // host's own per-frame queue (ASSET_QUEUE_MAX, 256): it closes a frame past that.
  const abandoned = new Set();
  const MAX_ABANDONED = 256;
  // CAD-1921: asset refs a call reply said the host now serves (`asset_refs`). Bounded like
  // the host's own reply-learned refs (128); it never forgets a ref a later reply dropped.
  const admitted = new Set();
  const MAX_ADMITTED = 128;
  const identityKeys = ["v", "install_id", "digest", "context_id"];
  let tornDown = false;
  let accepted = false;
  let locked = null;
  let liveV2 = false;
  let presentation = "outlet";
  // The opt-in says the app can be presented; only a host message proves it
  // actually is. The drawer helpers wait for that proof so an opted-in app on
  // a non-presenting host keeps its own drawer (CAD-1785 fallback).
  let presented = false;
  /** CAD-1777 — the host-derived role of this mount and the surface it names. */
  let surfaceRole = "primary";
  let surfaceName = undefined;
  let leaveOwner = null;
  let seq = 0;
  // The host closes a mount that starts a second tool while one is running.
  // A local timeout does not cancel the host invocation, so busy survives it
  // until the host's reply (even a late one) or teardown.
  let toolBusy = false;
  let busyToolId = null;
  let audioBusy = false;
  let busyAudioId = null;

  const close = (reason, notify) => {
    if (tornDown) return;
    tornDown = true;
    accepted = false;
    leaveOwner = null;
    try {
      port.onmessage = null;
      port.onmessageerror = null;
      port.close();
    } catch {
      // a closed port is already closed
    }
    toolBusy = false;
    busyToolId = null;
    audioBusy = false;
    busyAudioId = null;
    for (const entry of pending.values()) {
      if (entry.timer !== null) clearTimeout(entry.timer);
      entry.reject(new ScreenClosedError(`screen bridge closed: ${reason}`));
    }
    pending.clear();
    for (const entry of assets.values()) {
      clearTimeout(entry.timer);
      entry.fail?.(new ScreenClosedError(`screen bridge closed: ${reason}`));
    }
    assets.clear();
    if (notify) onError?.(reason);
  };

  const post = (message) => {
    if (tornDown) throw new Error("screen bridge is closed");
    port.postMessage(message);
  };

  const request = (kind, body, onSent, customTimeoutMs = timeoutMs) => {
    if (tornDown) return Promise.reject(new ScreenClosedError("screen bridge is closed"));
    if (!liveV2) {
      return Promise.reject(new ScreenClosedError("the action channel opens only after a screen.v2 PUSH"));
    }
    const id = `${kind}-${++seq}`;
    if (!ACTION_ID.test(id)) return Promise.reject(new TypeError("request id exceeds the wire bound"));
    return new Promise((resolve, reject) => {
      const timer = customTimeoutMs === null ? null : setTimeout(() => {
        pending.delete(id);
        reject(new ScreenTimeoutError(id, customTimeoutMs));
      }, customTimeoutMs);
      pending.set(id, { resolve, reject, timer, kind });
      sent.set(id, kind);
      onSent?.(id);
      try {
        post({ v: 2, op: kind, id, ...body });
      } catch (error) {
        if (timer !== null) clearTimeout(timer);
        pending.delete(id);
        reject(error);
      }
    });
  };

  const expectedOp = {
    call: "reply",
    slot: "slot-state",
    tool: "tool-reply",
    audio: "audio-reply",
    "audio-session": "audio-session-reply",
  };

  /** A reply whose outcome is well-formed for its op. A late/replayed reply
   *  is validated exactly like a waiting one before it can release a gate. */
  const validOutcome = (message) => {
    if (message.op === "slot-state") {
      return ["pending", "done", "refused"].includes(message.state) &&
        (message.refusal === undefined || readRefusal(message.refusal) !== null);
    }
    if (message.op === "audio-reply") {
      return (message.ok === true && validAudioCaptureData(message.data) !== null) ||
        (message.ok === false && readRefusal(message.refusal) !== null);
    }
    if (message.op === "audio-session-reply") {
      return (message.ok === true && validAudioSessionSnapshot(message.data) !== null) ||
        (message.ok === false && readRefusal(message.refusal) !== null);
    }
    return message.ok === true || (message.ok === false && readRefusal(message.refusal) !== null);
  };

  const settle = (message) => {
    const entry = pending.get(message.id);
    if (!entry) {
      if (message.v !== 2) throw new Error(`${message.op} must be v2`);
      const kind = sent.get(message.id);
      if (!kind) return false;
      // A retired request still validates its kind; a replayed reply never
      // releases a different invocation's gate.
      if (message.op !== expectedOp[kind]) throw new Error(`${kind} answered with ${message.op}`);
      if (!validOutcome(message)) throw new Error(`malformed ${message.op} reply`);
      if (kind === "tool" && message.id === busyToolId) {
        toolBusy = false;
        busyToolId = null;
      }
      if (kind === "audio" && message.id === busyAudioId) {
        audioBusy = false;
        busyAudioId = null;
      }
      return true; // a late reply is ignored, not fatal
    }
    if (message.v !== 2) throw new Error(`${message.op} must be v2`);
    if (message.op !== expectedOp[entry.kind]) {
      throw new Error(`${entry.kind} answered with ${message.op}`);
    }
    if (message.op === "tool-reply" && message.id === busyToolId) {
      toolBusy = false;
      busyToolId = null;
    }
    if (message.op === "audio-reply" && message.id === busyAudioId) {
      audioBusy = false;
      busyAudioId = null;
    }
    if (message.op === "slot-state") {
      // A slot is a host-drawn control: `pending` is progress, not a result.
      // Keep the request open (and stop its clock) until `done`/`refused` so
      // a later receipt is never discarded.
      if (!["pending", "done", "refused"].includes(message.state)) throw new Error("malformed slot-state");
      if (message.refusal !== undefined && !readRefusal(message.refusal)) {
        throw new Error("malformed slot-state refusal");
      }
      const { id: _id, op: _op, v: _v, ...event } = message;
      if (message.state === "pending") {
        if (entry.timer !== null) clearTimeout(entry.timer);
        onSlotState?.(event);
        return true;
      }
      pending.delete(message.id);
      if (entry.timer !== null) clearTimeout(entry.timer);
      entry.resolve(event);
      return true;
    }
    const refusal = message.ok === false ? readRefusal(message.refusal) : null;
    const audioData = message.op === "audio-reply" && message.ok === true ? validAudioCaptureData(message.data) : null;
    const audioSessionData = message.op === "audio-session-reply" && message.ok === true
      ? validAudioSessionSnapshot(message.data)
      : null;
    if (
      (message.ok !== true && !(message.ok === false && refusal)) ||
      (message.op === "audio-reply" && message.ok === true && !audioData) ||
      (message.op === "audio-session-reply" && message.ok === true && !audioSessionData)
    ) {
      // Throw before removal: teardown then rejects this entry with the rest.
      throw new Error(`malformed ${message.op} reply`);
    }
    pending.delete(message.id);
    if (entry.timer !== null) clearTimeout(entry.timer);
    if (message.ok === true && Array.isArray(message.asset_refs)) {
      for (const ref of message.asset_refs.slice(0, MAX_ADMITTED)) {
        if (typeof ref !== "string" || ref.length === 0 || ref.length > 512) continue;
        if (admitted.size >= MAX_ADMITTED) admitted.delete(admitted.values().next().value);
        admitted.add(ref);
      }
    }
    if (message.ok === true) {
      entry.resolve(
        message.op === "audio-reply"
          ? audioData
          : message.op === "audio-session-reply"
            ? audioSessionData
            : message.data,
      );
    }
    else entry.reject(new ScreenRefusalError(refusal));
    return true;
  };

  port.onmessageerror = () => close("port messageerror", true);
  port.onmessage = (event) => {
    if (tornDown) return;
    const message = event?.data;
    try {
      if (!message || typeof message !== "object" || Array.isArray(message)) fail("non-object message");
      if (message.op === "asset") {
        if (message.v !== 2) fail("asset reply must be v2");
        if (Object.keys(message).sort().join() !== "data_url,op,ref,v") fail("asset reply has extra or missing keys");
        const entry = assets.get(message.ref);
        if (!entry && abandoned.delete(message.ref)) return;
        if (!entry) fail("unsolicited asset reply");
        if (typeof message.data_url !== "string" || byteLength(message.data_url) > MAX_ASSET_BYTES) {
          fail("asset reply is over the bound");
        }
        if (!ASSET_DATA_URL.test(message.data_url)) fail("asset reply is not an image data URL");
        assets.delete(message.ref);
        clearTimeout(entry.timer);
        entry.resolve(message.data_url);
        return;
      }
      if (
        message.op === "reply" ||
        message.op === "tool-reply" ||
        message.op === "audio-reply" ||
        message.op === "audio-session-reply" ||
        message.op === "slot-state"
      ) {
        if (!settle(message)) fail(`reply for an id this mount never sent: ${message.id}`);
        return;
      }
      if (message.op === "audio-session-event") {
        if (!audioV2Accepted) fail("audio-session-event without the opt-in");
        if (message.v !== 2 || Object.keys(message).sort().join() !== "op,state,v") {
          fail("invalid audio-session-event envelope");
        }
        const stateSnapshot = validAudioSessionSnapshot(message.state);
        if (!stateSnapshot) fail("invalid audio-session-event state");
        onAudioSessionEvent?.(stateSnapshot);
        return;
      }
      if (message.op === "surface") {
        // CAD-1777 (host): the role this mount plays and, for a drawer, the
        // closed surface idiom the primary asked for. Closed shape only.
        if (!surfacesAccepted) fail("surface without the opt-in");
        // Field set, not an exact key string: the host sends `modal` on every
        // surface and `surface` only for a drawer, and a strict key comparison
        // closed the channel on the first mount (SOC-52).
        const FIELDS = new Set(["v", "op", "role", "modal", "surface"]);
        if (message.v !== 1 || Object.keys(message).some((key) => !FIELDS.has(key)) ||
            (message.role !== "primary" && message.role !== "drawer") ||
            (message.modal !== undefined && typeof message.modal !== "boolean") ||
            (message.surface !== undefined &&
              (typeof message.surface !== "string" || !DRAWER_SURFACE.test(message.surface)))) {
          fail("invalid surface message");
        }
        surfaceRole = message.role;
        surfaceName = message.role === "drawer" ? message.surface : undefined;
        onSurface?.(surfaceRole, surfaceName, message.modal === true);
        return;
      }
      if (message.op === "presentation") {
        // CAD-1615 (host): the surface this frame is presented in. Only a host
        // that saw the opt-in may send it; the mode is validated like any
        // other closed message.
        if (!presentationAccepted) fail("presentation without the opt-in");
        if (message.v !== 1 || Object.keys(message).sort().join() !== "mode,op,v" ||
            (message.mode !== "outlet" && message.mode !== "drawer")) fail("invalid presentation message");
        presented = true;
        presentation = message.mode;
        onPresentation?.(presentation);
        return;
      }
      if (message.op !== "screen") fail(`unknown op ${JSON.stringify(message.op)}`);
      if (message.tag !== boot.tag) fail("PUSH tag mismatch");
      if (message.v !== 1 && message.v !== 2) fail("unsupported PUSH version");
      if (!accepts.includes(`screen.v${message.v}`)) fail(`PUSH v${message.v} is not accepted`);
      liveV2 = message.v === 2;
      const identity = Object.fromEntries(identityKeys.map((key) => [key, message[key]]));
      if (locked && identityKeys.some((key) => identity[key] !== locked[key])) {
        fail("identity/scope mismatch on an accepted port");
      }
      locked = identity;
      accepted = true;
      onPush(message);
    } catch (error) {
      close(`screen push refused: ${error.message}`, true);
    }
  };

  try {
    parent.postMessage(
      { v: 1, op: "init", tag: boot.tag, bridge_nonce: boot.bridge_nonce, generation: boot.generation },
      boot.parent_origin,
      [channel.port2],
    );
    port.postMessage(ready);
  } catch (error) {
    close(`init postMessage: ${error.message}`, true);
    return inert(`init postMessage: ${error.message}`);
  }

  windowRef?.addEventListener?.("pagehide", () => close("pagehide", false), { once: true });

  return Object.freeze({
    boot,
    accepted: () => accepted,
    call(verb, args = {}) {
      if (typeof verb !== "string" || verb.length === 0 || verb.length > 128) {
        return Promise.reject(new TypeError("verb must be a non-empty string of at most 128 characters"));
      }
      if (validArgs(args, maxArgsBytes) === null) {
        return Promise.reject(new TypeError(`args must be a plain object within ${argsLabel}`));
      }
      return request("call", { verb, args });
    },
    /** The surface the host currently presents this frame in. */
    presentation: () => presentation,
    /** CAD-1777 — the host-derived role of this mount ("primary" | "drawer"). */
    surfaceRole: () => surfaceRole,
    /** The closed surface idiom this drawer mounts, when the host named one. */
    surface: () => surfaceName,
    /** Ask the host to present the app's surface in the board-wide drawer.
     *  `options.size` (CAD-1932) is `"narrow"` (the default) or `"wide"`;
     *  `options.context` (SOC-55) is bounded plain JSON on `screen-surfaces.v1`. */
    openDrawer(surface, drawerOptions = {}) {
      if (!presentationAccepted && !surfacesAccepted) {
        return Promise.reject(new ScreenClosedError("the host did not opt into a drawer"));
      }
      if (presentationAccepted && !surfacesAccepted && !presented) {
        return Promise.reject(new ScreenClosedError("the host has not presented this screen"));
      }
      if (surfaceRole !== "primary") {
        return Promise.reject(new ScreenClosedError("a drawer surface opens no further drawer"));
      }
      if (typeof surface !== "string" || !DRAWER_SURFACE.test(surface)) {
        return Promise.reject(new TypeError("surface must be 1-64 of a-z, 0-9, _ or -"));
      }
      const size = drawerOptions?.size;
      if (size !== undefined && size !== "narrow" && size !== "wide") {
        return Promise.reject(new TypeError('size must be "narrow" or "wide"'));
      }
      const hasContext = drawerOptions !== null && typeof drawerOptions === "object" && drawerOptions.context !== undefined;
      if (hasContext) {
        if (!surfacesAccepted) return Promise.reject(new ScreenClosedError("drawer context requires screen-surfaces.v1"));
        if (!validDrawerContext(drawerOptions.context)) {
          return Promise.reject(new TypeError(`context must be plain JSON within ${DRAWER_CONTEXT_BYTES} bytes`));
        }
      }
      const args = {
        surface,
        ...(size !== undefined ? { size } : {}),
        ...(hasContext ? { context: drawerOptions.context } : {}),
      };
      return request("call", { verb: "screen.drawer.open", args });
    },
    /** Ask the host to close the board-wide drawer it opened. */
    closeDrawer() {
      if (!presentationAccepted && !surfacesAccepted) {
        return Promise.reject(new ScreenClosedError("the host did not opt into a drawer"));
      }
      if (presentationAccepted && !surfacesAccepted && !presented) {
        return Promise.reject(new ScreenClosedError("the host has not presented this screen"));
      }
      return request("call", { verb: "screen.drawer.close", args: {} });
    },
    drawerContext() {
      if (!surfacesAccepted) return Promise.reject(new ScreenClosedError("drawer context requires screen-surfaces.v1"));
      if (surfaceRole !== "drawer") return Promise.reject(new ScreenClosedError("only a drawer surface has open context"));
      return request("call", { verb: "screen.drawer.context", args: {} }).then((data) => {
        if (!isPlain(data) || !("context" in data)) throw new ScreenClosedError("malformed drawer context reply");
        return data.context ?? null;
      });
    },
    slot(verb, args = {}, anchor = "footer") {
      if (typeof verb !== "string" || verb.length === 0 || verb.length > 128) {
        return Promise.reject(new TypeError("verb must be a non-empty string of at most 128 characters"));
      }
      if (validArgs(args, maxArgsBytes) === null) {
        return Promise.reject(new TypeError(`args must be a plain object within ${argsLabel}`));
      }
      if (validAnchor(anchor) === null) {
        return Promise.reject(new TypeError('anchor must be "footer" or exactly {x,y,w,h}'));
      }
      return request("slot", { verb, args, anchor });
    },
    tool(alias, input = {}, requestId, generationScope) {
      if (!actionsAccepted) {
        return Promise.reject(new ScreenClosedError('the tool channel needs accepts to include "screen-actions.v1"'));
      }
      if (typeof alias !== "string" || !TOOL_ALIAS.test(alias)) {
        return Promise.reject(new TypeError("tool alias is malformed"));
      }
      if (validArgs(input, maxArgsBytes) === null) {
        return Promise.reject(new TypeError(`input must be a plain object within ${argsLabel}`));
      }
      if (typeof requestId !== "string" || !TOOL_REQUEST_ID.test(requestId)) {
        return Promise.reject(new TypeError("tool request_id is required and bounded"));
      }
      if (generationScope !== undefined && validGenerationScope(generationScope) === null) {
        return Promise.reject(new TypeError("tool generation_scope must be exactly {draft_id,operation,revision}"));
      }
      // The host closes the mount on a second concurrent tool call; refuse
      // locally instead of destroying the channel.
      if (toolBusy) {
        return Promise.reject(new ScreenRefusalError({ code: "conflict", text: "Another tool call is already running." }));
      }
      toolBusy = true;
      const pendingTool = request("tool", {
        alias,
        input,
        request_id: requestId,
        ...(generationScope ? { generation_scope: generationScope } : {}),
      }, (id) => {
        busyToolId = id;
      });
      // A host reply settles busy in `settle`. A local timeout must NOT:
      // the host may still be running the tool. Any other local rejection
      // means nothing was posted, so release the gate.
      pendingTool.catch((error) => {
        if (!(error instanceof ScreenTimeoutError)) toolBusy = false;
      });
      return pendingTool;
    },
    audioCapture(audioOptions = {}) {
      if (!audioAccepted) {
        return Promise.reject(new ScreenClosedError('the audio channel needs accepts to include "screen-audio.v1"'));
      }
      if (!isPlain(audioOptions) || !exactKeys(audioOptions, ["mode", "timeoutMs"])) {
        return Promise.reject(new TypeError("audioCapture options must be a plain object with optional mode and timeoutMs"));
      }
      const mode = audioOptions.mode ?? "record";
      if (mode !== "record" && mode !== "upload") {
        return Promise.reject(new TypeError('audioCapture mode must be "record" or "upload"'));
      }
      const wait = audioOptions.timeoutMs;
      if (wait !== undefined && !(Number.isSafeInteger(wait) && wait > 0 && wait <= 2147483647)) {
        return Promise.reject(new TypeError("audioCapture timeoutMs must be a positive integer"));
      }
      if (tornDown) return Promise.reject(new ScreenClosedError("screen bridge is closed"));
      if (!liveV2) return Promise.reject(new ScreenClosedError("the action channel opens only after a screen.v2 PUSH"));
      if (audioBusy) {
        return Promise.reject(new ScreenRefusalError({ code: "conflict", text: "Audio capture is already open." }));
      }
      audioBusy = true;
      const pendingAudio = request("audio", { mode }, (id) => {
        busyAudioId = id;
      }, wait ?? null);
      pendingAudio.catch((error) => {
        if (!(error instanceof ScreenTimeoutError)) audioBusy = false;
      });
      return pendingAudio;
    },
    audioSession(command = {}) {
      if (!audioV2Accepted) {
        return Promise.reject(new ScreenClosedError('the inline audio channel needs accepts to include "screen-audio.v2"'));
      }
      if (!isPlain(command)) {
        return Promise.reject(new TypeError("audioSession command must be a plain object"));
      }
      const action = command.action;
      const wait = command.timeoutMs;
      if (wait !== undefined && !(Number.isSafeInteger(wait) && wait > 0 && wait <= 2147483647)) {
        return Promise.reject(new TypeError("audioSession timeoutMs must be a positive integer"));
      }
      const validLanguage = (lang) => lang === "" || lang === "en" || lang === "zh";
      let body = null;
      if (action === "start") {
        if (!exactKeys(command, ["action", "language", "timeoutMs"])) {
          return Promise.reject(new TypeError("audioSession start accepts only action, language and timeoutMs"));
        }
        if (command.language !== undefined && !validLanguage(command.language)) {
          return Promise.reject(new TypeError('audioSession language must be "", "en" or "zh"'));
        }
        body = {
          action: "start",
          ...(command.language !== undefined ? { language: command.language } : {}),
        };
      } else if (
        action === "pause" ||
        action === "resume" ||
        action === "stop" ||
        action === "retry" ||
        action === "cancel"
      ) {
        if (!exactKeys(command, ["action", "timeoutMs"])) {
          return Promise.reject(new TypeError(`audioSession ${action} accepts only action and timeoutMs`));
        }
        body = { action };
      } else if (action === "upload") {
        if (!exactKeys(command, ["action", "bytes", "mimeType", "fileName", "lastModified", "language", "timeoutMs"])) {
          return Promise.reject(new TypeError("audioSession upload has unknown keys"));
        }
        if (!isCrossRealmArrayBuffer(command.bytes) || command.bytes.byteLength <= 0 || command.bytes.byteLength > MAX_AUDIO_UPLOAD_BYTES) {
          return Promise.reject(new TypeError("audioSession upload bytes must be a non-empty ArrayBuffer up to 50 MiB"));
        }
        if (typeof command.mimeType !== "string" || command.mimeType.length > 128) {
          return Promise.reject(new TypeError("audioSession upload mimeType must be a string of at most 128 characters"));
        }
        if (command.fileName !== undefined && (typeof command.fileName !== "string" || command.fileName.length > 255)) {
          return Promise.reject(new TypeError("audioSession upload fileName must be a string of at most 255 characters"));
        }
        if (command.lastModified !== undefined && !isSafeBoundedInt(command.lastModified, 0, 8_640_000_000_000_000)) {
          return Promise.reject(new TypeError("audioSession upload lastModified must be a non-negative safe integer"));
        }
        if (command.language !== undefined && !validLanguage(command.language)) {
          return Promise.reject(new TypeError('audioSession language must be "", "en" or "zh"'));
        }
        body = {
          action: "upload",
          bytes: command.bytes,
          mime_type: command.mimeType,
          ...(command.fileName !== undefined ? { file_name: command.fileName } : {}),
          ...(command.lastModified !== undefined ? { last_modified: command.lastModified } : {}),
          ...(command.language !== undefined ? { language: command.language } : {}),
        };
      } else {
        return Promise.reject(new TypeError('audioSession action must be "start", "pause", "resume", "stop", "upload", "retry" or "cancel"'));
      }
      if (tornDown) return Promise.reject(new ScreenClosedError("screen bridge is closed"));
      if (!liveV2) return Promise.reject(new ScreenClosedError("the action channel opens only after a screen.v2 PUSH"));
      return request("audio-session", body, undefined, wait ?? null);
    },
    /** True once a call reply named `ref` as served now; an older host never says so. It
     *  stays true: decide from the latest reply's own data (e.g. a draft still has an image). */
    assetAdmitted(ref) {
      return admitted.has(ref);
    },
    asset(ref, options = {}) {
      if (typeof ref !== "string" || ref.length === 0 || ref.length > 512) {
        return Promise.reject(new TypeError("asset ref must be a non-empty string"));
      }
      const wait = options?.timeoutMs;
      if (wait !== undefined && !(Number.isSafeInteger(wait) && wait > 0 && wait <= 2147483647)) {
        return Promise.reject(new TypeError("asset timeoutMs must be a positive integer"));
      }
      if (tornDown) return Promise.reject(new ScreenClosedError("screen bridge is closed"));
      if (!liveV2) return Promise.reject(new ScreenClosedError("assets require a screen.v2 PUSH"));
      if (assets.has(ref)) return Promise.reject(new Error(`asset ${ref} is already pending`));
      return new Promise((resolve, reject) => {
        const entry = { resolve, fail: reject, timer: null };
        assets.set(ref, entry);
        try {
          post({ v: 2, op: "asset", ref });
        } catch (error) {
          assets.delete(ref);
          reject(error);
          return;
        }
        // A host that cannot serve a ref sends nothing: free the ref so it can be asked for again.
        if (wait) entry.timer = setTimeout(() => {
          if (assets.get(ref) !== entry) return;
          assets.delete(ref);
          if (abandoned.size >= MAX_ABANDONED) abandoned.delete(abandoned.values().next().value);
          abandoned.add(ref);
          reject(new ScreenTimeoutError(ref, wait));
        }, wait);
      });
    },
    state(data) {
      if (tornDown || typeof data !== "string" || byteLength(data) > MAX_STATE_BYTES) return false;
      try {
        post({ v: 1, op: "state", data });
        return true;
      } catch {
        return false;
      }
    },
    ownLeaveState(leaveOptions = {}) {
      const isLive = leaveOptions?.isLive;
      if (isLive !== undefined && typeof isLive !== "function") throw new TypeError("isLive must be a function when provided");
      if (tornDown || !accepted || !liveV2 || (isLive && !isLive())) return null;
      const owner = Symbol();
      leaveOwner = owner;
      const active = () => !tornDown && accepted && liveV2 && leaveOwner === owner && (!isLive || isLive());
      const publish = (unsaved) => {
        if (typeof unsaved !== "boolean" || !active()) return false;
        try {
          post({ v: 2, op: "leave", unsaved });
          return true;
        } catch {
          return false;
        }
      };
      return Object.freeze({
        publish,
        release() {
          if (!active()) return;
          publish(false);
          leaveOwner = null;
        },
      });
    },
    teardown: () => close("teardown", false),
  });
}

/** Domain-agnostic reactive `screen.v2` bridge (`subscribe` + `getState`). */
export function createScreenBridge(options = {}) {
  const {
    tag, appName, records = false, actions = false, media = false, surfaces = false,
    presentation = false, locale = false, audio = false, calls = {}, slots = {}, argBytes = ACTION_ARGS_BYTES,
    singleFlight = [], writeVerbs = [], route: routeConfig, observations: observationVerbs = [],
    pushKeys = DEFAULT_PUSH_KEYS, maxInFlight = 8, timeoutMs = 65_000, createError,
  } = options;
  if (typeof tag !== "string" || !TAG.test(tag)) fail("tag must be 1-32 lowercase letters, digits or dashes, not leading with a dash");
  if (appName !== undefined && (typeof appName !== "string" || appName.length === 0)) fail("appName must be a non-empty string when provided");
  if (!isPlain(calls) || !isPlain(slots)) fail("calls and slots must be plain verb -> field-list maps");
  if (!Number.isSafeInteger(maxInFlight) || maxInFlight < 1) fail("maxInFlight must be a positive safe integer");
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) fail("timeoutMs must be positive");
  if (createError !== undefined && typeof createError !== "function") fail("createError must be a function when provided");

  let defaultArgBytes = ACTION_ARGS_BYTES;
  const perVerbBytes = new Map();
  if (typeof argBytes === "number") {
    if (!Number.isSafeInteger(argBytes) || argBytes <= 0) fail("argBytes must be a positive safe integer");
    defaultArgBytes = argBytes;
  } else if (isPlain(argBytes)) {
    for (const [verb, limit] of Object.entries(argBytes)) {
      if (!Number.isSafeInteger(limit) || limit <= 0) fail(`argBytes[${verb}] must be a positive safe integer`);
      perVerbBytes.set(verb, limit);
    }
  } else {
    fail("argBytes must be a positive safe integer or a plain verb -> byte-limit map");
  }
  const receiverMaxArgsBytes = Math.max(defaultArgBytes, ...perVerbBytes.values());

  let parsedRouteConfig = null;
  if (routeConfig !== undefined) {
    if (!isPlain(routeConfig) || !Array.isArray(routeConfig.sections) || routeConfig.sections.length === 0 ||
        routeConfig.sections.some((s) => typeof s !== "string" || s.length === 0)) {
      fail("route.sections must be a non-empty string array");
    }
    const views = routeConfig.views ?? ["list", "new"];
    if (!Array.isArray(views) || views.length === 0 || views.some((v) => typeof v !== "string" || v.length === 0)) {
      fail("route.views must be a non-empty string array");
    }
    const defaultSection = routeConfig.defaultSection ?? routeConfig.sections[0];
    if (!routeConfig.sections.includes(defaultSection)) fail("route.defaultSection must be listed in route.sections");
    parsedRouteConfig = { sections: [...routeConfig.sections], defaultSection, views: [...views], defaultView: views[0] };
  }

  const declaredOperations = Object.freeze([...new Set([...Object.keys(calls), ...Object.keys(slots)])]);
  const declaredSet = new Set(declaredOperations);
  const writeVerbSet = new Set(writeVerbs);
  const observationSet = new Set(observationVerbs);
  const allowedPushKeys = [...pushKeys];
  const singleFlightRules = [...singleFlight];
  const negotiatedAudio =
    audio === "v2" ? [SCREEN_AUDIO_V2]
      : audio === true || audio === "v1" ? [SCREEN_AUDIO_V1]
        : audio === false ? []
          : null;
  if (negotiatedAudio === null) fail('audio must be a boolean, "v1", or "v2"');
  const negotiatedAccepts = [
    "screen.v2",
    ...(actions ? ["screen-actions.v1"] : []),
    ...(media ? ["screen-media.v1"] : []),
    ...(presentation ? ["screen-presentation.v1"] : []),
    ...(surfaces ? ["screen-surfaces.v1"] : []),
    ...(records ? ["screen-records.v1"] : []),
    ...(locale ? ["screen-locale.v1"] : []),
    ...negotiatedAudio,
  ];
  if (negotiatedAccepts.length > 6) fail("the host admits at most six ready capabilities");

  const makeError = (message, status = 502, extra) =>
    createError ? createError(message, status, extra) : new ScreenBridgeError(message, status, extra);
  const invalidBridge = (detail = "The screen bridge refused an invalid or retired message") => makeError(detail, 502);
  const initialSurface = Object.freeze({ role: "primary", name: undefined, modal: false });
  const makeState = (patch) => Object.freeze({
    status: patch.status, snapshot: patch.snapshot, error: patch.error, pushes: patch.pushes, surface: patch.surface,
  });

  let currentState = makeState({ status: "loading", snapshot: null, error: null, pushes: 0, surface: initialSurface });
  let latestAudioSession = null;
  let receiver = null;
  let retired = false;
  let retiredCause = null;
  let inFlight = 0;
  const activeSingleFlight = new Set();
  const listeners = new Set();
  const writeListeners = new Set();
  const audioSessionListeners = new Set();
  const notify = () => { for (const listener of [...listeners]) listener(); };
  const notifyWritten = () => { for (const listener of [...writeListeners]) listener(); };
  const publishAudioSession = (snap) => {
    latestAudioSession = snap;
    for (const listener of [...audioSessionListeners]) listener(snap);
  };

  const setState = (next) => {
    currentState = makeState({
      status: next.status ?? currentState.status,
      snapshot: "snapshot" in next ? next.snapshot : currentState.snapshot,
      error: "error" in next ? next.error : currentState.error,
      pushes: next.pushes ?? currentState.pushes,
      surface: next.surface ?? currentState.surface,
    });
    notify();
  };

  const parseRoute = (value) => {
    if (!parsedRouteConfig) return null;
    const fallback = Object.freeze({
      section: parsedRouteConfig.defaultSection, view: parsedRouteConfig.defaultView, record_id: null, segment_id: null,
    });
    if (value === undefined) return fallback;
    if (!isPlain(value) || !exactKeys(value, ["section", "view", "record_id", "segment_id"])) {
      throw invalidBridge("Invalid screen route projection");
    }
    return Object.freeze({
      section: parsedRouteConfig.sections.includes(value.section) ? value.section : parsedRouteConfig.defaultSection,
      view: parsedRouteConfig.views.includes(value.view) ? value.view : parsedRouteConfig.defaultView,
      record_id: typeof value.record_id === "string" && ROUTE_RECORD_ID.test(value.record_id) ? value.record_id : null,
      segment_id: typeof value.segment_id === "string" && ROUTE_RECORD_ID.test(value.segment_id) ? value.segment_id : null,
    });
  };

  const can = (verb, actionList) => {
    const list = actionList ?? currentState.snapshot?.actions;
    return Array.isArray(list) && declaredSet.has(verb) && list.includes(verb);
  };
  const canAudioCapture = (snap = currentState.snapshot) => Boolean(audio && snap?.audioCapture === true);

  const acceptPush = (push) => {
    let serialized;
    try { serialized = JSON.stringify(push); } catch { throw invalidBridge(); }
    if (!serialized || byteLength(serialized) > MAX_PUSH_BYTES) throw invalidBridge();
    if (!isPlain(push) || !exactKeys(push, allowedPushKeys)) throw invalidBridge("Invalid screen scope projection");
    if (push.v !== 2 || push.op !== "screen" || push.tag !== tag) throw invalidBridge("Invalid screen scope projection");
    if (typeof push.install_id !== "string" || !SAFE_ID.test(push.install_id)) throw invalidBridge("Invalid screen scope projection");
    const contextId = push.context_id === undefined ? "" : push.context_id;
    if (typeof contextId !== "string" || (contextId !== "" && !SAFE_ID.test(contextId))) {
      throw invalidBridge("Invalid screen scope projection");
    }
    if (typeof push.digest !== "string" || !DIGEST.test(push.digest)) throw invalidBridge("Invalid screen scope projection");
    if ("presentation_locale" in push && push.presentation_locale !== "en" && push.presentation_locale !== "zh-TW") {
      throw invalidBridge("Invalid screen scope projection");
    }
    if ("audio_capture" in push && push.audio_capture !== undefined && push.audio_capture !== 1) {
      throw invalidBridge("Invalid screen scope projection");
    }
    if (!isPlain(push.installation) ||
        !exactKeys(push.installation, ["install_id", "name", "title", "version", "digest", "summary"]) ||
        push.installation.install_id !== push.install_id || push.installation.digest !== push.digest ||
        (appName !== undefined && push.installation.name !== appName) ||
        typeof push.installation.name !== "string" || push.installation.name.length === 0 ||
        typeof push.installation.title !== "string" || typeof push.installation.version !== "string" ||
        typeof push.installation.summary !== "string") {
      throw invalidBridge("Invalid screen scope projection");
    }
    let contexts = [];
    if ("contexts" in push && push.contexts !== undefined) {
      if (!Array.isArray(push.contexts) || !push.contexts.every(
        (c) => isPlain(c) && exactKeys(c, ["id", "label"]) && typeof c.id === "string" && SAFE_ID.test(c.id) && typeof c.label === "string",
      )) {
        throw invalidBridge("Invalid screen scope projection");
      }
      contexts = push.contexts.map((c) => Object.freeze({ id: c.id, label: c.label }));
    }
    const rawActions = push.actions === undefined ? [] : push.actions;
    if (!Array.isArray(rawActions) || rawActions.length > 256 || new Set(rawActions).size !== rawActions.length ||
        !rawActions.every((a) => typeof a === "string" && PUSH_ACTION_TOKEN.test(a))) {
      throw invalidBridge("Invalid screen scope projection");
    }
    const prev = currentState.snapshot;
    if (prev && (prev.scope.installId !== push.install_id || prev.digest !== push.digest || prev.scope.contextId !== contextId)) {
      throw invalidBridge("Invalid screen scope projection");
    }
    const pushActions = Object.freeze(
      rawActions.filter((a) => !HOST_DRAWER_VERBS.includes(a) && (actions || declaredSet.size === 0 || declaredSet.has(a))),
    );
    const parsedObservations = [];
    if (push.read_observations !== undefined) {
      if (!Array.isArray(push.read_observations) || push.read_observations.length > 16) {
        throw invalidBridge("Invalid screen scope projection");
      }
      for (const row of push.read_observations) {
        if (!isPlain(row) || !exactKeys(row, ["operation", "args", "data"]) || typeof row.operation !== "string" ||
            !observationSet.has(row.operation) || !can(row.operation, pushActions) || !isPlain(row.args) ||
            !exactKeys(row.args, calls[row.operation] ?? []) || byteLength(JSON.stringify(row)) > OBSERVATION_ROW_BYTES) {
          throw invalidBridge("Invalid screen scope projection");
        }
        parsedObservations.push(Object.freeze({ operation: row.operation, args: row.args, data: row.data }));
      }
    }
    const scope = Object.freeze({ installId: push.install_id, digest: push.digest, contextId });
    const snapshot = Object.freeze({
      scope, digest: push.digest, actions: pushActions,
      audioCapture: Boolean(audio && push.audio_capture === 1),
      presentationLocale: push.presentation_locale === "zh-TW" ? "zh-TW" : "en",
      route: parseRoute(push.route), observations: Object.freeze(parsedObservations),
      contexts: Object.freeze(contexts), installation: Object.freeze({ ...push.installation }), raw: push,
    });
    setState({ status: "ready", snapshot, error: null, pushes: currentState.pushes + 1 });
  };

  const retire = (cause) => {
    if (retired) return;
    const err = cause instanceof Error ? cause : makeError(typeof cause === "string" ? cause : "Screen bridge is unavailable", 502);
    retired = true;
    retiredCause = err;
    const live = receiver;
    receiver = null;
    latestAudioSession = null;
    live?.teardown();
    setState({ status: "error", snapshot: null, error: err });
  };

  const mapError = (error) => {
    if (createError && error instanceof Error && "status" in error && typeof error.status === "number" &&
        !(error instanceof ScreenRefusalError) && !(error instanceof ScreenTimeoutError) && !(error instanceof ScreenClosedError)) {
      return error;
    }
    if (error instanceof ScreenBridgeError) return error;
    if (error instanceof ScreenRefusalError) return makeError(error.text || error.code, error.status, { code: error.code });
    if (error instanceof ScreenTimeoutError) return makeError("Request did not answer. Inspect fresh status before retrying.", 504, { code: "timeout" });
    if (error instanceof ScreenClosedError) return retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "gone" });
    if (error instanceof TypeError) return makeError(error.message, 400, { code: "invalid" });
    if (error instanceof Error && typeof error.status === "number") return error;
    return invalidBridge();
  };

  const assertScope = (scope) => {
    const snap = currentState.snapshot;
    if (!snap || retired || !scope || scope.installId !== snap.scope.installId || scope.digest !== snap.digest ||
        (scope.contextId !== undefined && scope.contextId !== snap.scope.contextId)) {
      throw makeError("Screen scope is absent or retired", 403, { code: "forbidden" });
    }
  };

  const cleanArgs = (verb, rawArgs, fieldsMap) => {
    const allowed = fieldsMap[verb];
    if (!allowed) throw makeError("This workspace doesn't offer that action.", 403, { code: "forbidden" });
    const source = rawArgs === undefined ? {} : rawArgs;
    if (!isPlain(source)) throw makeError(`Invalid arguments for ${verb}`, 400, { code: "invalid" });
    const cleaned = {};
    for (const [key, value] of Object.entries(source)) {
      if (value === undefined) continue;
      if (!allowed.includes(key)) throw makeError(`Unexpected arguments for ${verb}`, 400, { code: "invalid" });
      cleaned[key] = value;
    }
    let encoded;
    try { encoded = JSON.stringify(cleaned); } catch { throw makeError(`Invalid arguments for ${verb}`, 400, { code: "invalid" }); }
    if (!encoded || byteLength(encoded) > (perVerbBytes.get(verb) ?? defaultArgBytes)) {
      throw makeError("Request arguments exceed size limit", 400, { code: "invalid" });
    }
    return cleaned;
  };

  const hasSecretKey = (node) => {
    if (!node || typeof node !== "object") return false;
    if (Array.isArray(node)) return node.some(hasSecretKey);
    if (!isPlain(node)) return false;
    return Object.keys(node).some((key) => SECRET_KEYS.includes(key) || hasSecretKey(node[key]));
  };

  const guardData = (value) => {
    let encoded;
    try { encoded = JSON.stringify(value ?? null); } catch { throw invalidBridge(); }
    if (byteLength(encoded) > MAX_REPLY_DATA_BYTES || hasSecretKey(value)) throw invalidBridge();
    return value;
  };

  const matchSingleFlight = (verb) =>
    singleFlightRules.find((rule) => (rule.endsWith(".") ? verb.startsWith(rule) : verb === rule)) ?? null;

  const dispatch = (kind, scope, verb, rawArgs, extra = {}) => {
    const live = receiver;
    if (!live || retired) return Promise.reject(retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "unavailable" }));
    try {
      assertScope(scope);
      const fieldsMap = kind === "call" ? calls : slots;
      if (!(verb in fieldsMap) || !can(verb)) throw makeError("This workspace doesn't offer that action.", 403, { code: "forbidden" });
      const args = cleanArgs(verb, rawArgs, fieldsMap);
      if (inFlight >= maxInFlight) return Promise.reject(makeError("Screen bridge is busy; wait for the current request", 429, { code: "conflict" }));
      const lockKey = matchSingleFlight(verb);
      if (lockKey && activeSingleFlight.has(lockKey)) {
        return Promise.reject(makeError("A conflicting request is already pending", 429, { code: "conflict" }));
      }
      inFlight += 1;
      if (lockKey) activeSingleFlight.add(lockKey);
      const release = () => { inFlight -= 1; if (lockKey) activeSingleFlight.delete(lockKey); };
      const pending = kind === "call"
        ? live.call(verb, args)
        : live.slot(verb, args, extra.anchor ?? "footer").then((event) => {
            if (event.state !== "done") {
              const parsed = readRefusal(event.refusal);
              if (!parsed) throw invalidBridge();
              throw new ScreenRefusalError(parsed);
            }
            return "receipt" in event && event.receipt !== undefined ? event.receipt : event.data;
          });
      return pending.then(
        (value) => {
          release();
          let guarded;
          try { guarded = guardData(value); } catch (err) { const mapped = mapError(err); retire(mapped); throw mapped; }
          if (writeVerbSet.has(verb) && !extra.quiet) notifyWritten();
          return guarded;
        },
        (err) => {
          release();
          const mapped = mapError(err);
          const status = mapped.status;
          const keepOnForbidden = status === 403 && (writeVerbSet.has(verb) || extra.keepAliveOnForbidden === true);
          if (status === 401 || status === 502 || status === 504 || (status === 403 && !keepOnForbidden)) retire(mapped);
          throw mapped;
        },
      );
    } catch (syncError) {
      return Promise.reject(syncError);
    }
  };

  return Object.freeze({
    tag,
    operations: declaredOperations,
    subscribe(listener) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    getState: () => currentState,
    getSnapshot: () => currentState.snapshot,
    can,
    canAudioCapture,
    assertScope,
    start(runtime = {}) {
      if (receiver || retired) return;
      const created = createScreenReceiver({
        tag, accepts: negotiatedAccepts, maxArgsBytes: receiverMaxArgsBytes, timeoutMs,
        ...(runtime.boot !== undefined ? { boot: runtime.boot } : {}),
        ...(runtime.document !== undefined ? { document: runtime.document } : {}),
        ...(runtime.parent !== undefined ? { parent: runtime.parent } : {}),
        ...(runtime.window !== undefined ? { window: runtime.window } : {}),
        ...(runtime.MessageChannel !== undefined ? { MessageChannel: runtime.MessageChannel } : {}),
        ...(runtime.onSlotState !== undefined ? { onSlotState: runtime.onSlotState } : {}),
        onAudioSessionEvent(snap) {
          if (!retired) publishAudioSession(snap);
        },
        onPresentation(mode) {
          if (!retired && !surfaces) {
            setState({ surface: Object.freeze({ role: mode === "drawer" ? "drawer" : "primary", name: undefined, modal: false }) });
          }
        },
        onSurface(role, name, modal) { if (!retired) setState({ surface: Object.freeze({ role, name, modal }) }); },
        onPush(push) { acceptPush(push); },
        onError(reason) { retire(makeError(reason, 502)); },
      });
      if (!created.boot || created.boot.tag !== tag) {
        created.teardown();
        if (!retired) retire(invalidBridge());
        return;
      }
      receiver = created;
    },
    call: (scope, verb, args = {}, callOptions = {}) => dispatch("call", scope, verb, args, callOptions),
    slot: (scope, verb, args = {}, anchor = "footer", slotOptions = {}) => dispatch("slot", scope, verb, args, { ...slotOptions, anchor }),
    tool(scope, alias, input = {}, requestId, generationScope) {
      const live = receiver;
      if (!live || retired) return Promise.reject(retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "unavailable" }));
      try { assertScope(scope); } catch (syncError) { return Promise.reject(syncError); }
      return live.tool(alias, input, requestId, generationScope).then(
        (value) => { try { return guardData(value); } catch (err) { const mapped = mapError(err); retire(mapped); throw mapped; } },
        (err) => {
          const mapped = mapError(err);
          if (mapped.status === 401 || mapped.status === 502 || mapped.status === 504) retire(mapped);
          throw mapped;
        },
      );
    },
    audioCapture(scope, audioOptions = {}) {
      const live = receiver;
      if (!live || retired) return Promise.reject(retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "unavailable" }));
      try {
        assertScope(scope);
        if (!canAudioCapture()) throw makeError("This workspace doesn't offer audio capture.", 403, { code: "forbidden" });
      } catch (syncError) {
        return Promise.reject(syncError);
      }
      return live.audioCapture(audioOptions).then(
        (value) => {
          try { return guardData(value); } catch (err) { const mapped = mapError(err); retire(mapped); throw mapped; }
        },
        (err) => {
          const mapped = mapError(err);
          if (mapped.status === 401 || mapped.status === 502) retire(mapped);
          throw mapped;
        },
      );
    },
    audioSession(scope, command = {}) {
      const live = receiver;
      if (!live || retired) return Promise.reject(retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "unavailable" }));
      try {
        assertScope(scope);
        if (!canAudioCapture()) throw makeError("This workspace doesn't offer audio capture.", 403, { code: "forbidden" });
      } catch (syncError) {
        return Promise.reject(syncError);
      }
      return live.audioSession(command).then(
        (value) => {
          try {
            const guarded = guardData(value);
            publishAudioSession(guarded);
            return guarded;
          } catch (err) {
            const mapped = mapError(err);
            retire(mapped);
            throw mapped;
          }
        },
        (err) => {
          const mapped = mapError(err);
          if (mapped.status === 401 || mapped.status === 502) retire(mapped);
          throw mapped;
        },
      );
    },
    getAudioSession: () => latestAudioSession,
    onAudioSession(listener) {
      audioSessionListeners.add(listener);
      return () => { audioSessionListeners.delete(listener); };
    },
    asset(scope, ref, assetOptions) {
      const live = receiver;
      if (!live || retired) return Promise.reject(retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "unavailable" }));
      try { assertScope(scope); } catch (syncError) { return Promise.reject(syncError); }
      return live.asset(ref, assetOptions).catch((err) => Promise.reject(mapError(err)));
    },
    assetAdmitted: (ref) => Boolean(receiver && !retired && receiver.assetAdmitted(ref)),
    state: (data) => (receiver && !retired && currentState.snapshot ? receiver.state(data) : false),
    patchRoute(patch = {}) {
      const snap = currentState.snapshot;
      if (!parsedRouteConfig || !snap || !("screen.route.patch" in calls) || !can("screen.route.patch", snap.actions) || !isPlain(patch)) return;
      if (patch.record_id !== undefined && patch.record_id !== null && (typeof patch.record_id !== "string" || !ROUTABLE_ID.test(patch.record_id))) return;
      if (patch.segment_id !== undefined && patch.segment_id !== null && (typeof patch.segment_id !== "string" || !ROUTABLE_ID.test(patch.segment_id))) return;
      void dispatch("call", snap.scope, "screen.route.patch", patch, { quiet: true, keepAliveOnForbidden: true }).catch(() => undefined);
    },
    ownLeaveState() {
      const snap = currentState.snapshot;
      if (!receiver || retired || !snap) return null;
      const pinnedInstallId = snap.scope.installId;
      const pinnedDigest = snap.digest;
      return receiver.ownLeaveState({
        isLive: () => {
          const cur = currentState.snapshot;
          return !retired && !!cur && cur.scope.installId === pinnedInstallId && cur.digest === pinnedDigest;
        },
      });
    },
    openDrawer(surface, drawerOptions) {
      if (!receiver || retired) return Promise.reject(retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "unavailable" }));
      return receiver.openDrawer(surface, drawerOptions).catch((err) => Promise.reject(mapError(err)));
    },
    closeDrawer() {
      if (!receiver || retired) return Promise.reject(retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "unavailable" }));
      return receiver.closeDrawer().catch((err) => Promise.reject(mapError(err)));
    },
    drawerContext() {
      if (!receiver || retired) return Promise.reject(retiredCause ?? makeError("Screen bridge is unavailable", 503, { code: "unavailable" }));
      return receiver.drawerContext().catch((err) => Promise.reject(mapError(err)));
    },
    onWrite(listener) { writeListeners.add(listener); return () => { writeListeners.delete(listener); }; },
    notifyWritten,
    markUnreachable(message = "The host is unreachable. Reopen this screen from the board.") {
      if (currentState.status === "loading" && !retired) setState({ status: "error", error: makeError(message, 504, { code: "timeout" }) });
    },
    retire,
    reset() {
      const live = receiver;
      receiver = null;
      latestAudioSession = null;
      live?.teardown();
      retired = false;
      retiredCause = null;
      inFlight = 0;
      activeSingleFlight.clear();
      currentState = makeState({ status: "loading", snapshot: null, error: null, pushes: 0, surface: initialSurface });
      notify();
    },
  });
}
