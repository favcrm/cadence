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
export const MAX_ASSET_BYTES = 2 * 1024 * 1024;
export const DEFAULT_TIMEOUT_MS = 15_000;

const BOOT_ID = "cadence-screen-boot";
const BOOT_KEYS = ["bridge_nonce", "generation", "parent_origin", "tag"];
const TAG = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const NONCE = /^[0-9a-f]{64}$/;
// Wire bounds from `screenProtocol.ts`: ACTION_ID, TOOL_ALIAS,
// TOOL_REQUEST_ID, ACTION_ARGS_BYTES and COORD_MAX. The receiver enforces the
// shape; which verbs an app may send stays the app's closed set.
const ACTION_ID = /^[A-Za-z0-9_-]{1,64}$/;
const TOOL_ALIAS = /^[a-z][a-z0-9]{0,31}(\.[a-z][a-z0-9]{0,31}){0,3}$/;
const TOOL_REQUEST_ID = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/;
const ACTION_ARGS_BYTES = 16 * 1024;
const COORD_MAX = 16384;
const LOOPBACK = new Set(["localhost", "127.0.0.1", "::1", "[::1]"]);

function fail(message) {
  throw new TypeError(message);
}

export function validateBoot(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail("boot must be an object");
  const keys = Object.keys(value);
  if (keys.length !== BOOT_KEYS.length || keys.some((key) => !BOOT_KEYS.includes(key))) {
    fail("boot keys must be exactly bridge_nonce, generation, parent_origin, tag");
  }
  if (!TAG.test(value.tag ?? "")) fail("boot tag is malformed");
  if (!NONCE.test(value.bridge_nonce ?? "")) fail("boot bridge_nonce must be 64 lowercase hex");
  if (typeof value.generation !== "string" || value.generation.length === 0 || value.generation.length > 128) {
    fail("boot generation must be a non-empty string of at most 128 characters");
  }
  let origin;
  try {
    origin = new URL(value.parent_origin);
  } catch {
    fail("boot parent_origin must be an absolute origin");
  }
  const loopback = LOOPBACK.has(origin.hostname) || origin.hostname.endsWith(".localhost");
  if (!(origin.protocol === "https:" || (origin.protocol === "http:" && loopback))) {
    fail("boot parent_origin must be https or loopback http");
  }
  if (origin.origin !== value.parent_origin) fail("boot parent_origin must be an origin, not a path");
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
  const refuse = () => Promise.reject(new Error(`screen bridge is closed: ${reason}`));
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
    accepts = ["screen.v2"],
    timeoutMs = DEFAULT_TIMEOUT_MS,
    document: documentRef = globalThis.document,
    parent = globalThis.parent,
    window: windowRef = globalThis.window,
    MessageChannel: Channel = globalThis.MessageChannel,
  } = options;
  if (typeof onPush !== "function") throw new TypeError("onPush is required");
  if (!Array.isArray(accepts) || accepts.length === 0 || accepts.some((v) => !/^screen\.v[12]$/.test(v))) {
    throw new TypeError("accepts must name screen.v1 and/or screen.v2");
  }
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
  const sent = new Set(); // ids this mount sent, kept for late replies
  const assets = new Map(); // ref -> resolve
  const identityKeys = ["v", "install_id", "digest", "context_id"];
  let tornDown = false;
  let accepted = false;
  let locked = null;
  let liveV2 = false;
  let seq = 0;

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
    for (const entry of pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(new ScreenClosedError(`screen bridge closed: ${reason}`));
    }
    pending.clear();
    for (const resolve of assets.values()) resolve.fail?.(new Error(`screen bridge closed: ${reason}`));
    assets.clear();
    if (notify) onError?.(reason);
  };

  const post = (message) => {
    if (tornDown) throw new Error("screen bridge is closed");
    port.postMessage(message);
  };

  const request = (kind, body) => {
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
      pending.set(id, { resolve, reject, timer });
      sent.add(id);
      try {
        post({ v: 2, op: kind, id, ...body });
      } catch (error) {
        clearTimeout(timer);
        pending.delete(id);
        reject(error);
      }
    });
  };

  const settle = (message) => {
    const entry = pending.get(message.id);
    if (!entry) return sent.has(message.id); // a late reply is ignored, not fatal
    pending.delete(message.id);
    clearTimeout(entry.timer);
    if (message.op === "slot-state") {
      const refusal = readRefusal(message.refusal);
      if (message.refusal !== undefined && !refusal) entry.reject(new Error("malformed slot-state refusal"));
      else if (refusal) entry.reject(new ScreenRefusalError(refusal));
      else entry.resolve({ state: message.state });
      return true;
    }
    if (message.ok === true) {
      entry.resolve(message.data);
      return true;
    }
    const refusal = readRefusal(message.refusal);
    if (message.ok === false && refusal) entry.reject(new ScreenRefusalError(refusal));
    else entry.reject(new Error(`malformed ${message.op} reply`));
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
        const entry = assets.get(message.ref);
        if (!entry) fail("unsolicited asset reply");
        if (typeof message.data_url !== "string" || byteLength(message.data_url) > MAX_ASSET_BYTES) {
          fail("asset reply is malformed or over the bound");
        }
        assets.delete(message.ref);
        entry.resolve(message.data_url);
        return;
      }
      if (message.op === "reply" || message.op === "tool-reply" || message.op === "slot-state") {
        if (!settle(message)) fail(`reply for an id this mount never sent: ${message.id}`);
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
    port.postMessage({ v: 1, op: "ready", accepts: [...accepts] });
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
    slot(verb, args = {}, anchor = {}) {
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
    tool(alias, input = {}, requestId) {
      if (typeof alias !== "string" || !TOOL_ALIAS.test(alias)) {
        return Promise.reject(new TypeError("tool alias is malformed"));
      }
      if (validArgs(input) === null) {
        return Promise.reject(new TypeError("input must be a plain object within 16 KiB"));
      }
      if (typeof requestId !== "string" || !TOOL_REQUEST_ID.test(requestId)) {
        return Promise.reject(new TypeError("tool request_id is required and bounded"));
      }
      return request("tool", { alias, input, request_id: requestId });
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
