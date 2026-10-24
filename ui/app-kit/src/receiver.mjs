/**
 * The app side of the `app-screens` bridge, implemented once.
 *
 * This is the transport and lifecycle only: boot validation, the single
 * window `init` that transfers a port, `ready`, the closed PUSH union,
 * request/reply bookkeeping for `call`/`slot`/`tool`, asset requests, the
 * bounded local-state post, and teardown. What a PUSH *means* — the app's
 * projection and its own payload validation — stays in the app through
 * `onPush`.
 *
 * Wire shapes are documented in
 * `ui/src/features/workspace-apps/screen/screenProtocol.ts`; this module
 * follows that document, it does not extend it.
 */
import { ScreenClosedError, ScreenRefusalError, ScreenTimeoutError, readRefusal } from "./refusal.mjs";

export const MAX_STATE_BYTES = 32 * 1024;
/** The encoded allowance for a 96 KiB image (base64 + prefix), matching
 *  `ASSET_BYTES_MAX` and the reference receiver's `MAX_DATA_URL`. */
export const MAX_ASSET_BYTES = Math.ceil((96 * 1024 * 4) / 3) + 32;
export const DEFAULT_TIMEOUT_MS = 15_000;

const BOOT_ID = "cadence-screen-boot";
const BOOT_KEYS = ["bridge_nonce", "generation", "parent_origin", "tag"];
const TAG = /^[a-z0-9][a-z0-9-]{0,31}$/;
const NONCE = /^[0-9a-f]{64}$/;
const DNS_LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
// Wire bounds from `screenProtocol.ts`: ACTION_ID, TOOL_ALIAS,
// TOOL_REQUEST_ID, ACTION_ARGS_BYTES and COORD_MAX. The receiver enforces the
// shape; which verbs an app may send stays the app's closed set.
const ACTION_ID = /^[A-Za-z0-9_-]{1,64}$/;
const TOOL_ALIAS = /^[a-z][a-z0-9]{0,31}(\.[a-z][a-z0-9]{0,31}){0,3}$/;
const TOOL_REQUEST_ID = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/;
const ACTION_ARGS_BYTES = 16 * 1024;
const GEN_SCOPE_KEYS = ["draft_id", "operation", "revision"];
const ASSET_DATA_URL = /^data:image\/(jpeg|png|webp);base64,[A-Za-z0-9+/]+=*$/;
const COORD_MAX = 16384;

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

export function validateBoot(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail("boot must be an object");
  const keys = Object.keys(value);
  if (keys.length !== BOOT_KEYS.length || keys.some((key) => !BOOT_KEYS.includes(key))) {
    fail("boot keys must be exactly bridge_nonce, generation, parent_origin, tag");
  }
  if (!TAG.test(value.tag ?? "")) fail("boot tag must be 1-32 lowercase letters, digits or dashes, not leading with a dash");
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

export function readBoot(documentRef = globalThis.document) {
  const node = documentRef?.getElementById?.(BOOT_ID);
  if (!node) fail(`no #${BOOT_ID} element`);
  let parsed;
  try {
    parsed = JSON.parse(node.textContent ?? "");
  } catch {
    fail(`#${BOOT_ID} is not JSON`);
  }
  return validateBoot(parsed);
}

const isPlain = (value) => !!value && typeof value === "object" && !Array.isArray(value);

/** A plain-object args bag within the host's byte bound, or `null`. */
function validArgs(args) {
  if (!isPlain(args)) return null;
  let text;
  try {
    text = JSON.stringify(args);
  } catch {
    return null;
  }
  return text !== undefined && byteLength(text) <= ACTION_ARGS_BYTES ? args : null;
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
    asset: refuse,
    state: () => false,
    teardown() {},
  });
}

/**
 * @param {object} options
 * @param {object} [options.boot] parsed boot (omit to read `#cadence-screen-boot`)
 * @param {(push: object) => void} options.onPush
 * @param {(reason: string) => void} [options.onError]
 * @param {readonly string[]} [options.accepts] advertised PUSH versions
 * @param {number} [options.timeoutMs]
 * @param {{ getElementById(id: string): { textContent: string } | null }} [options.document]
 * @param {{ postMessage(message: unknown, origin: string, transfer?: unknown[]): void }} [options.parent]
 * @param {{ addEventListener?(type: string, fn: () => void, options?: object): void }} [options.window]
 * @param {typeof MessageChannel} [options.MessageChannel]
 */
export function createScreenReceiver(options = {}) {
  const {
    onPush,
    onError,
    onPresentation,
    accepts = ["screen.v2"],
    timeoutMs = DEFAULT_TIMEOUT_MS,
    onSlotState,
    document: documentRef = globalThis.document,
    parent = globalThis.parent,
    window: windowRef = globalThis.window,
    MessageChannel: Channel = globalThis.MessageChannel,
  } = options;
  if (typeof onPush !== "function") throw new TypeError("onPush is required");
  const ACCEPTED = new Set(["screen.v1", "screen.v2", "screen-actions.v1", "screen-presentation.v1"]);
  if (
    !Array.isArray(accepts) ||
    accepts.length === 0 ||
    accepts.some((value) => !ACCEPTED.has(value)) ||
    !accepts.some((value) => value === "screen.v1" || value === "screen.v2")
  ) {
    throw new TypeError("accepts must name screen.v1 or screen.v2, optionally screen-actions.v1 or screen-presentation.v1");
  }
  const actionsAccepted = accepts.includes("screen-actions.v1");
  if (actionsAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-actions.v1 needs screen.v2");
  }
  const presentationAccepted = accepts.includes("screen-presentation.v1");
  if (presentationAccepted && !accepts.includes("screen.v2")) {
    throw new TypeError("screen-presentation.v1 needs screen.v2");
  }
  // `screen.v1` is a local PUSH filter. The host's ready envelope admits only
  // v2 contract names, and a v1-only mount sends the bare ready.
  const ready = accepts.includes("screen.v2")
    ? { v: 1, op: "ready", accepts: ["screen.v2", ...(actionsAccepted ? ["screen-actions.v1"] : []), ...(presentationAccepted ? ["screen-presentation.v1"] : [])] }
    : { v: 1, op: "ready" };
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new TypeError("timeoutMs must be positive");
  if (!parent || typeof parent.postMessage !== "function") throw new TypeError("a parent window is required");
  if (typeof Channel !== "function") throw new TypeError("MessageChannel is unavailable");

  let boot;
  try {
    boot = validateBoot(options.boot ?? readBoot(documentRef));
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
  const identityKeys = ["v", "install_id", "digest", "context_id"];
  let tornDown = false;
  let accepted = false;
  let locked = null;
  let liveV2 = false;
  let presentation = "outlet";
  let seq = 0;
  // The host closes a mount that starts a second tool while one is running.
  // A local timeout does not cancel the host invocation, so busy survives it
  // until the host's reply (even a late one) or teardown.
  let toolBusy = false;
  let busyToolId = null;

  const close = (reason, notify) => {
    if (tornDown) return;
    tornDown = true;
    try {
      port.onmessage = null;
      port.onmessageerror = null;
      port.close();
    } catch {
      // a closed port is already closed
    }
    toolBusy = false;
    busyToolId = null;
    for (const entry of pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(new ScreenClosedError(`screen bridge closed: ${reason}`));
    }
    pending.clear();
    for (const entry of assets.values()) entry.fail?.(new ScreenClosedError(`screen bridge closed: ${reason}`));
    assets.clear();
    if (notify) onError?.(reason);
  };

  const post = (message) => {
    if (tornDown) throw new Error("screen bridge is closed");
    port.postMessage(message);
  };

  const request = (kind, body, onSent) => {
    if (tornDown) return Promise.reject(new ScreenClosedError("screen bridge is closed"));
    if (!liveV2) {
      return Promise.reject(new ScreenClosedError("the action channel opens only after a screen.v2 PUSH"));
    }
    const id = `${kind}-${++seq}`;
    if (!ACTION_ID.test(id)) return Promise.reject(new TypeError("request id exceeds the wire bound"));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new ScreenTimeoutError(id, timeoutMs));
      }, timeoutMs);
      pending.set(id, { resolve, reject, timer, kind });
      sent.set(id, kind);
      onSent?.(id);
      try {
        post({ v: 2, op: kind, id, ...body });
      } catch (error) {
        clearTimeout(timer);
        pending.delete(id);
        reject(error);
      }
    });
  };

  const expectedOp = { call: "reply", slot: "slot-state", tool: "tool-reply" };

  /** A reply whose outcome is well-formed for its op. A late/replayed reply
   *  is validated exactly like a waiting one before it can release a gate. */
  const validOutcome = (message) => {
    if (message.op === "slot-state") {
      return ["pending", "done", "refused"].includes(message.state) &&
        (message.refusal === undefined || readRefusal(message.refusal) !== null);
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
        clearTimeout(entry.timer);
        onSlotState?.(event);
        return true;
      }
      pending.delete(message.id);
      clearTimeout(entry.timer);
      entry.resolve(event);
      return true;
    }
    const refusal = message.ok === false ? readRefusal(message.refusal) : null;
    if (message.ok !== true && !(message.ok === false && refusal)) {
      // Throw before removal: teardown then rejects this entry with the rest.
      throw new Error(`malformed ${message.op} reply`);
    }
    pending.delete(message.id);
    clearTimeout(entry.timer);
    if (message.ok === true) entry.resolve(message.data);
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
        if (!entry) fail("unsolicited asset reply");
        if (typeof message.data_url !== "string" || byteLength(message.data_url) > MAX_ASSET_BYTES) {
          fail("asset reply is over the bound");
        }
        if (!ASSET_DATA_URL.test(message.data_url)) fail("asset reply is not an image data URL");
        assets.delete(message.ref);
        entry.resolve(message.data_url);
        return;
      }
      if (message.op === "reply" || message.op === "tool-reply" || message.op === "slot-state") {
        if (!settle(message)) fail(`reply for an id this mount never sent: ${message.id}`);
        return;
      }
      if (message.op === "presentation") {
        // CAD-1615 (host): the surface this frame is presented in. Only a host
        // that saw the opt-in may send it; the mode is validated like any
        // other closed message.
        if (!presentationAccepted) fail("presentation without the opt-in");
        if (message.v !== 1 || Object.keys(message).sort().join() !== "mode,op,v" ||
            (message.mode !== "outlet" && message.mode !== "drawer")) fail("invalid presentation message");
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
      onPush(message);
      accepted = true;
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
      if (validArgs(args) === null) {
        return Promise.reject(new TypeError("args must be a plain object within 16 KiB"));
      }
      return request("call", { verb, args });
    },
    /** The surface the host currently presents this frame in. */
    presentation: () => presentation,
    /** Ask the host to present the app's surface in the board-wide drawer. */
    openDrawer(surface) {
      if (!presentationAccepted) {
        return Promise.reject(new ScreenClosedError("the host did not opt into screen-presentation.v1"));
      }
      if (typeof surface !== "string" || !/^[a-z0-9_-]{1,64}$/.test(surface)) {
        return Promise.reject(new TypeError("surface must be 1-64 of a-z, 0-9, _ or -"));
      }
      return request("call", { verb: "screen.drawer.open", args: { surface } });
    },
    /** Ask the host to close the board-wide drawer it opened. */
    closeDrawer() {
      if (!presentationAccepted) {
        return Promise.reject(new ScreenClosedError("the host did not opt into screen-presentation.v1"));
      }
      return request("call", { verb: "screen.drawer.close", args: {} });
    },
    slot(verb, args = {}, anchor = "footer") {
      if (typeof verb !== "string" || verb.length === 0 || verb.length > 128) {
        return Promise.reject(new TypeError("verb must be a non-empty string of at most 128 characters"));
      }
      if (validArgs(args) === null) {
        return Promise.reject(new TypeError("args must be a plain object within 16 KiB"));
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
      if (validArgs(input) === null) {
        return Promise.reject(new TypeError("input must be a plain object within 16 KiB"));
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
    asset(ref) {
      if (typeof ref !== "string" || ref.length === 0 || ref.length > 512) {
        return Promise.reject(new TypeError("asset ref must be a non-empty string"));
      }
      if (tornDown) return Promise.reject(new ScreenClosedError("screen bridge is closed"));
      if (!liveV2) return Promise.reject(new ScreenClosedError("assets require a screen.v2 PUSH"));
      if (assets.has(ref)) return Promise.reject(new Error(`asset ${ref} is already pending`));
      return new Promise((resolve, reject) => {
        assets.set(ref, { resolve, fail: reject });
        try {
          post({ v: 2, op: "asset", ref });
        } catch (error) {
          assets.delete(ref);
          reject(error);
        }
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
    teardown: () => close("teardown", false),
  });
}
