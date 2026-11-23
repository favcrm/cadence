// The sealed `fetch` wrapper of the private native-model SDK child.
//
// Installed only inside this dedicated child, before the pinned published
// provider module is imported. It permits exactly two POST targets on the one
// pinned origin — the JWT mint and the chat stream — once each, validates the
// assembled credential-free payload against the same commitment the broker
// admitted, bounds both request and response bodies before the provider decoder
// allocates, and records the independent usage/EOS evidence the broker
// re-checks.
//
// It never passes upstream status text, headers, trailers or error bodies back
// to the provider module or the guest: failures are fixed local errors.

import { boundedGunzip, iterFields } from './framing.mjs';

const AUTH_PATH = '/exa.auth_pb.AuthService/GetUserJwt';
const CHAT_PATH = '/exa.api_server_pb.ApiServerService/GetChatMessage';
const MAX_AUTH_REQUEST = 96 * 1024;
const MAX_AUTH_RESPONSE = 64 * 1024;
const MAX_INFERENCE_REQUEST = 2 * 1024 * 1024;
const MAX_INFERENCE_RESPONSE = 2 * 1024 * 1024;
const MAX_FRAME = 1024 * 1024;
const MAX_FRAMES = 32768;
const MAX_MESSAGES = 100;
const MAX_TOOLS = 16;
const INFERENCE_DEADLINE_MS = 65_000;

const TO_TOP_LEVEL = new Set([1, 2, 3, 4, 5, 6, 7, 8, 10, 16, 21, 22]);

const fail = message => {
  throw new Error(message);
};

const combineSignals = signals => {
  const live = signals.filter(Boolean);
  if (live.length === 0) return undefined;
  if (typeof AbortSignal.any === 'function') return AbortSignal.any(live);
  const controller = new AbortController();
  for (const signal of live) {
    if (signal.aborted) {
      controller.abort(signal.reason);
      return controller.signal;
    }
    signal.addEventListener('abort', () => controller.abort(signal.reason), { once: true });
  }
  return controller.signal;
};

const bodyBuffer = body => {
  if (Buffer.isBuffer(body)) return body;
  if (body instanceof Uint8Array) return Buffer.from(body.buffer, body.byteOffset, body.byteLength);
  return null;
};

const readBounded = async (response, limit) => {
  if (!response.body) fail('provider response has no body');
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      if (value) {
        size += value.length;
        if (size > limit) fail('provider response exceeds its bound');
        chunks.push(Buffer.from(value));
      }
    }
  } finally {
    try {
      await reader.cancel();
    } catch {
      // best effort
    }
  }
  return Buffer.concat(chunks, size);
};

const validateMetadata = metadata => {
  let key = null;
  let jwt = null;
  for (const field of iterFields(metadata)) {
    if (field.num === 3 && field.wire === 2 && field.value.length > 0) key = field.value;
    if (field.num === 21 && field.wire === 2 && field.value.length > 0) jwt = field.value;
  }
  if (!key) fail('provider authentication carrier is missing');
  if (!jwt) fail('provider authentication token is missing');
  return Buffer.from(jwt).toString('utf8');
};

const validateAuthBody = body => {
  let metadata = null;
  let fields = 0;
  for (const field of iterFields(body)) {
    fields += 1;
    if (field.num !== 1 || field.wire !== 2) fail('authentication request is malformed');
    metadata = field.value;
  }
  if (fields !== 1 || !metadata) fail('authentication request is malformed');
  let hasKey = false;
  let hasJwt = false;
  for (const field of iterFields(metadata)) {
    if (field.num === 3 && field.wire === 2) hasKey = field.value.length > 0;
    if (field.num === 21 && field.wire === 2) hasJwt = true;
  }
  if (!hasKey || hasJwt) fail('authentication request is malformed');
};

const validateInferenceWire = (proto, route) => {
  if (proto.length > MAX_INFERENCE_REQUEST) fail('inference request exceeds its bound');
  let modelUid = null;
  let maxOutput = null;
  let messages = 0;
  let tools = 0;
  let token = null;
  for (const field of iterFields(proto)) {
    if (!TO_TOP_LEVEL.has(field.num)) fail('inference request carries an unexpected field');
    if (field.num === 21 && field.wire === 2) {
      modelUid = field.value.toString('utf8');
    } else if (field.num === 3) {
      messages += 1;
    } else if (field.num === 10) {
      tools += 1;
    } else if (field.num === 8 && field.wire === 2) {
      maxOutput = completionMaxOutput(field.value);
    } else if (field.num === 1 && field.wire === 2) {
      token = validateMetadata(field.value);
    }
  }
  if (messages === 0 || messages > MAX_MESSAGES) fail('inference prompt count is out of bounds');
  if (tools > MAX_TOOLS) fail('inference tool count is out of bounds');
  if (modelUid !== route.model_uid) fail('inference model uid differs from the commitment');
  if (maxOutput !== route.max_output_tokens) {
    fail('inference output bound differs from the commitment');
  }
  if (!token) fail('inference metadata is missing');
  return token;
};

const completionMaxOutput = buffer => {
  for (const field of iterFields(buffer)) {
    if (field.num !== 3 || field.wire !== 0) continue;
    if (typeof field.value !== 'bigint') return null;
    if (field.value > BigInt(Number.MAX_SAFE_INTEGER)) fail('completion field is out of range');
    return Number(field.value);
  }
  return null;
};

/**
 * Install the sealed wrapper. Returns the independent evidence state.
 */
export const installSealedFetch = route => {
  const originalFetch = globalThis.fetch;
  const state = {
    authRequests: 0,
    inferenceRequests: 0,
    eos: false,
    usageSeen: false,
    finishSeen: false,
    error: null,
  };
  // The minted token never leaves this closure: it screens the provider's own
  // output and is never part of the returned evidence state.
  let mintedToken = null;
  const origin = route.origin.replace(/\/$/, '');
  const authUrl = `${origin}${AUTH_PATH}`;
  const chatUrl = `${origin}${CHAT_PATH}`;

  const sealed = async (input, init = {}) => {
    const url = typeof input === 'string' ? input : input?.url;
    if (typeof url !== 'string') fail('unsupported provider destination');
    const body = bodyBuffer(init.body);
    if (!body) fail('provider request body is unsupported');
    if (init.method && String(init.method).toUpperCase() !== 'POST') {
      fail('provider request method is unsupported');
    }
    if (url === authUrl) {
      state.authRequests += 1;
      if (state.authRequests > 1) fail('second provider authentication is refused');
      if (body.length > MAX_AUTH_REQUEST) fail('authentication request exceeds its bound');
      validateAuthBody(body);
      const response = await originalFetch(authUrl, {
        method: 'POST',
        headers: { 'Content-Type': 'application/proto', 'Connect-Protocol-Version': '1' },
        body,
        redirect: 'error',
        credentials: 'omit',
        cache: 'no-store',
        signal: combineSignals([init.signal, AbortSignal.timeout(30_000)]),
      });
      const payload = await readBounded(response, MAX_AUTH_RESPONSE);
      if (!response.ok) fail('provider authentication failed');
      return new Response(payload, {
        status: response.status,
        headers: { 'Content-Type': 'application/proto' },
      });
    }
    if (url === chatUrl) {
      state.inferenceRequests += 1;
      if (state.inferenceRequests > 1) fail('second inference request is refused');
      if (body.length > MAX_INFERENCE_REQUEST) fail('inference request exceeds its bound');
      // Connect stream framing: 5-byte header, gzip flag, one payload.
      if (body.length < 5 || !(body[0] & 0x01) || body[0] & 0x02) {
        fail('inference request framing is unexpected');
      }
      const length = body.readUInt32BE(1);
      if (length !== body.length - 5) fail('inference request framing is truncated');
      const proto = boundedGunzip(body.subarray(5), MAX_INFERENCE_REQUEST);
      mintedToken = validateInferenceWire(proto, route);
      // Freeze exactly the admitted bytes for this one request; the buffer is
      // dropped when this call returns.
      const wire = Buffer.from(body);
      const response = await originalFetch(chatUrl, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/connect+proto',
          'Connect-Protocol-Version': '1',
          'Connect-Content-Encoding': 'gzip',
          'Connect-Accept-Encoding': 'gzip',
        },
        body: wire,
        redirect: 'error',
        credentials: 'omit',
        cache: 'no-store',
        signal: combineSignals([
          init.signal,
          AbortSignal.timeout(INFERENCE_DEADLINE_MS),
        ]),
      });
      if (!response.ok) fail('provider inference failed');
      const framed = await readBounded(response, MAX_INFERENCE_RESPONSE);
      inspectInferenceResponse(framed, state);
      return new Response(framed, {
        status: response.status,
        headers: { 'Content-Type': 'application/connect+proto' },
      });
    }
    fail('unsupported provider destination');
  };

  // Closure-bound screening only. A match on the complete token or any
  // eight-character fragment refuses the result; the check never throws, so a
  // screening edge cannot turn a completed turn into an unknown outcome.
  state.refusesCredentialEcho = value => {
    try {
      if (typeof mintedToken !== 'string' || mintedToken.length < 8) return false;
      const fragments = [];
      for (let i = 0; i + 8 <= mintedToken.length; i += 1) {
        fragments.push(mintedToken.slice(i, i + 8));
      }
      const hits = text =>
        typeof text === 'string' &&
        text.length >= 8 &&
        (text.includes(mintedToken) || fragments.some(fragment => text.includes(fragment)));
      const walk = node => {
        if (typeof node === 'string') return hits(node);
        if (Array.isArray(node)) return node.some(walk);
        if (node && typeof node === 'object') {
          for (const [key, child] of Object.entries(node)) {
            if (hits(key) || walk(child)) return true;
          }
        }
        return false;
      };
      return walk(value);
    } catch {
      return false;
    }
  };

  globalThis.fetch = sealed;
  return state;
};

const inspectInferenceResponse = (framed, state) => {
  let offset = 0;
  let frames = 0;
  let expandedTotal = 0;
  let sawTrailer = false;
  while (offset < framed.length) {
    if (offset + 5 > framed.length) fail('inference response framing is truncated');
    const flags = framed[offset];
    const length = framed.readUInt32BE(offset + 1);
    offset += 5;
    if (length > MAX_FRAME || offset + length > framed.length) {
      fail('inference response frame exceeds its bound');
    }
    frames += 1;
    if (frames > MAX_FRAMES) fail('inference response frame count exceeds its bound');
    const raw = framed.subarray(offset, offset + length);
    offset += length;
    let payload = raw;
    if (flags & 0x01) payload = boundedGunzip(raw, MAX_FRAME);
    expandedTotal += payload.length;
    if (expandedTotal > MAX_INFERENCE_RESPONSE) {
      fail('inference response exceeds its expanded bound');
    }
    if (flags & 0x02) {
      sawTrailer = true;
      const text = payload.toString('utf8');
      state.eos = true;
      if (text.includes('"error"')) {
        state.error = 'trailer_error';
        fail('provider stream reported an error');
      }
      continue;
    }
    for (const field of iterFields(payload)) {
      if (field.num === 28 && field.wire === 2) state.usageSeen = true;
      if (field.num === 5 && field.wire === 0) state.finishSeen = true;
    }
  }
  if (!sawTrailer) {
    state.eos = false;
    fail('provider stream ended without an EOS trailer');
  }
};
