// The broker-owned private SDK child for one native model turn.
//
// Started by the trusted broker with a cleared environment, `/` as cwd, one
// bounded request on stdin and one bounded reply on stdout. It verifies the
// pinned module graph by digest, installs the sealed `fetch` wrapper, imports
// ONLY the pinned published provider module and consumes one real
// `AssistantMessageEventStream`. It never imports an agent/tool/extension
// runtime, never reads environment credentials, never runs a command and never
// writes the key, the JWT or any provider body anywhere except the private
// request/response bytes.

import { readFileSync, lstatSync, realpathSync } from 'node:fs';
import { register } from 'node:module';
import { pathToFileURL } from 'node:url';

import { materialDigest, parseStrictJson, readOneLine, sha256 } from './framing.mjs';
import { installSealedFetch } from './fetch-seal.mjs';

const MAX_REQUEST = 1024 * 1024 + 256 * 1024;
const MAX_REPLY = 1024 * 1024 + 32 * 1024;
const MAX_MODULES = 4096;
const MAX_MODULE_BYTES = 64 * 1024 * 1024;
const MAX_ASSISTANT = 1024 * 1024;
const AUTH_PATH = '/exa.auth_pb.AuthService/GetUserJwt';
const CHAT_PATH = '/exa.api_server_pb.ApiServerService/GetChatMessage';
const ALLOWED_STOP = new Set(['stop', 'length', 'toolUse']);

/** The sealed wrapper's evidence for the final classification. */
let sealState = null;

// The provider module must not be able to print into the reply channel or
// scatter diagnostics; anything it writes to stderr is drained and discarded.
for (const method of ['log', 'info', 'warn', 'error', 'debug', 'trace']) {
  console[method] = () => {};
}

const fail = message => {
  throw new Error(message);
};

const isSafeInt = value => Number.isSafeInteger(value) && value >= 0;
const isAbsolute = value => typeof value === 'string' && value.startsWith('/') && !value.includes('\u0000');

const verifyModules = pins => {
  if (!pins || typeof pins !== 'object') fail('sdk pins are missing');
  if (!isAbsolute(pins.sdk_root) || !isAbsolute(pins.pi_devin_entry)) fail('sdk root is malformed');
  const root = pins.sdk_root.replace(/\/$/, '');
  const realRoot = realpathSync(root);
  if (!pins.pi_devin_entry.startsWith(root + '/')) fail('provider entry escapes the sdk root');
  if (!Array.isArray(pins.modules) || pins.modules.length === 0 || pins.modules.length > MAX_MODULES) {
    fail('module manifest is out of bounds');
  }
  const seen = new Set();
  let total = 0;
  let sawEntry = false;
  for (const module of pins.modules) {
    if (!module || typeof module.path !== 'string' || typeof module.digest !== 'string') {
      fail('module manifest entry is malformed');
    }
    if (!module.path.startsWith(root + '/') || module.path.includes('/../')) {
      fail('module manifest escapes the sdk root');
    }
    if (!/^sha256:[0-9a-f]{64}$/.test(module.digest)) fail('module digest is malformed');
    if (seen.has(module.path)) continue;
    seen.add(module.path);
    if (module.path === pins.pi_devin_entry) sawEntry = true;
    const stat = lstatSync(module.path);
    if (!stat.isFile() || stat.isSymbolicLink()) fail('module is not a regular file');
    total += stat.size;
    if (total > MAX_MODULE_BYTES) fail('module graph exceeds its bound');
    const real = realpathSync(module.path);
    if (!real.startsWith(realRoot + '/')) fail('module resolves outside the sdk root');
    const bytes = readFileSync(module.path);
    if (sha256(bytes) !== module.digest) fail('module digest does not match');
  }
  if (!sawEntry) fail('provider entry is not pinned');
};

const commitmentMaterial = route => ({
  v: 1,
  input_digest: route.input_digest,
  source_id: route.source_id,
  source_revision: route.source_revision,
  account: route.account,
  origin: route.origin,
  destination: `${route.origin.replace(/\/$/, '')}${CHAT_PATH}`,
  model_uid: route.model_uid,
  model: route.configured_model,
  effort: route.effort,
  max_output_tokens: route.max_output_tokens,
  tool_choice: route.tool_choice,
  sdk_graph_digest: route.sdk_graph_digest,
  configuration_id: route.configuration_id,
  configuration_revision: route.configuration_revision,
  snapshot_id: route.snapshot_id,
  loaded_receipt_id: route.loaded_receipt_id,
  runtime_generation: route.runtime_generation,
  owner_epoch: route.owner_epoch,
});

const verifyCommitment = route => {
  if (!route || typeof route !== 'object') fail('route is missing');
  if (route.origin !== route.origin.replace(/\/$/, '')) fail('origin is not canonical');
  if (route.paths?.length !== 2 || route.paths[0] !== AUTH_PATH || route.paths[1] !== CHAT_PATH) {
    fail('route paths are not the two fixed provider methods');
  }
  if (route.provider !== 'devin') fail('provider is not the qualified route');
  if (typeof route.configured_model !== 'string' || !route.configured_model ||
      typeof route.model_id !== 'string' || !route.model_id) fail('model identities are missing');
  if (!isSafeInt(route.max_output_tokens) || route.max_output_tokens <= 0 || route.max_output_tokens > 8192) {
    fail('output bound is out of range');
  }
  if (route.tool_choice !== 'auto' && route.tool_choice !== 'none') fail('tool choice is malformed');
  const expected = materialDigest('cadence-native-model-commitment-v1', commitmentMaterial(route));
  if (expected !== route.commitment) fail('the assembled commitment does not match the admission');
};

const contentPart = part => {
  if (!part || typeof part !== 'object') return null;
  if (part.type === 'text' && typeof part.text === 'string') {
    const out = { type: 'text', text: part.text };
    if (typeof part.textSignature === 'string') out.textSignature = part.textSignature;
    return out;
  }
  if (part.type === 'thinking' && typeof part.thinking === 'string') {
    const out = { type: 'thinking', thinking: part.thinking };
    if (typeof part.thinkingSignature === 'string') out.thinkingSignature = part.thinkingSignature;
    if (part.redacted === true) out.redacted = true;
    return out;
  }
  if (part.type === 'toolCall') {
    if (typeof part.id !== 'string' || part.id.length === 0 || part.id.length > 200) return null;
    if (typeof part.name !== 'string' || part.name.length === 0 || part.name.length > 64) return null;
    if (!part.arguments || typeof part.arguments !== 'object' || Array.isArray(part.arguments)) {
      return null;
    }
    const out = {
      type: 'toolCall',
      id: part.id,
      name: part.name,
      arguments: part.arguments,
    };
    if (typeof part.thoughtSignature === 'string') out.thoughtSignature = part.thoughtSignature;
    if (typeof part.namespace === 'string') out.namespace = part.namespace;
    if (typeof part.argumentsError === 'string') out.argumentsError = part.argumentsError;
    return out;
  }
  return null;
};

const usageOf = message => {
  const usage = message?.usage;
  if (!usage || typeof usage !== 'object') fail('provider returned no usage observation');
  for (const key of ['input', 'output', 'cacheRead', 'cacheWrite', 'totalTokens']) {
    if (!isSafeInt(usage[key])) fail('provider usage is not a nonnegative integer');
  }
  const sum = usage.input + usage.output + usage.cacheRead + usage.cacheWrite;
  if (!Number.isSafeInteger(sum) || sum !== usage.totalTokens) {
    fail('provider usage does not reconcile');
  }
  return {
    input: usage.input,
    output: usage.output,
    cacheRead: usage.cacheRead,
    cacheWrite: usage.cacheWrite,
    totalTokens: usage.totalTokens,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
  };
};

const sanitizeAssistant = (message, route) => {
  if (!message || typeof message !== 'object') fail('provider returned no terminal message');
  if (message.role !== 'assistant') fail('provider result is not an assistant message');
  if (message.api !== 'devin' || message.provider !== 'devin') {
    fail('provider result identity is not the qualified route');
  }
  if (message.model !== route.model_id) fail('provider result model is not the configured model');
  if (!ALLOWED_STOP.has(message.stopReason)) fail('provider result is not a terminal turn');
  if (!isSafeInt(message.timestamp)) fail('provider result timestamp is malformed');
  const content = [];
  for (const part of Array.isArray(message.content) ? message.content : []) {
    const mapped = contentPart(part);
    if (mapped) content.push(mapped);
  }
  const assistant = {
    role: 'assistant',
    content,
    api: 'devin',
    provider: 'devin',
    model: route.configured_model,
    usage: usageOf(message),
    stopReason: message.stopReason,
    timestamp: message.timestamp,
  };
  if (typeof message.responseModel === 'string') assistant.responseModel = message.responseModel;
  if (typeof message.responseId === 'string') assistant.responseId = message.responseId;
  if (typeof message.providerThinkingLevel === 'string') {
    assistant.providerThinkingLevel = message.providerThinkingLevel;
  }
  if (typeof message.thinkingLevel === 'string') assistant.thinkingLevel = message.thinkingLevel;
  if (isSafeInt(message.durationMs)) assistant.durationMs = message.durationMs;
  if (Buffer.byteLength(JSON.stringify(assistant), 'utf8') > MAX_ASSISTANT) {
    fail('provider result exceeds its projected bound');
  }
  return assistant;
};

const writeFailure = (seal, code) => {
  // Once the seal has seen an inference attempt, the outcome is unknown: a
  // local failure cannot prove the provider saw nothing.
  if (seal && seal.inferenceRequests > 0) {
    process.stdout.write(
      JSON.stringify(reply('unknown', { error: 'outcome_unknown' }, seal)) + '\n',
    );
    return;
  }
  const effective = seal && seal.authRequests > 0 ? 'auth_unavailable' : code;
  process.stdout.write(
    JSON.stringify(reply('refused', { error: effective }, seal)) + '\n',
  );
};

const reply = (outcome, { error = null, assistant = null } = {}, seal) => ({
  v: 1,
  outcome,
  error: error ? { code: error } : null,
  assistant,
  usage_present: Boolean(seal?.usageSeen),
  eos: Boolean(seal?.eos),
  auth_requests: seal?.authRequests ?? 0,
  inference_requests: seal?.inferenceRequests ?? 0,
});

const main = async () => {
  if (process.argv.length > 3 || (process.argv.length === 3 && process.argv[2] !== '--pinned-child')) {
    process.stdout.write(JSON.stringify(reply('refused', { error: 'unsupported_transport' })) + '\n');
    return;
  }
  const raw = await readOneLine(process.stdin, MAX_REQUEST);
  const request = parseStrictJson(raw, MAX_REQUEST);
  if (!request || typeof request !== 'object') fail('request is malformed');
  const keys = Object.keys(request).sort();
  if (keys.length !== 3 || keys[0] !== 'api_key' || keys[1] !== 'plan' || keys[2] !== 'v') {
    fail('request fields are not the expected set');
  }
  if (request.v !== 1) fail('request version is unsupported');
  if (typeof request.api_key !== 'string' || request.api_key.length === 0 || request.api_key.length > 8192) {
    fail('request key is unusable');
  }
  const plan = request.plan;
  if (!plan || typeof plan !== 'object' || plan.v !== 1) fail('plan is malformed');
  const planKeys = Object.keys(plan).sort();
  if (
    planKeys.length !== 5 ||
    planKeys[0] !== 'messages' ||
    planKeys[1] !== 'pins' ||
    planKeys[2] !== 'route' ||
    planKeys[3] !== 'tools' ||
    planKeys[4] !== 'v'
  ) {
    fail('plan fields are not the expected set');
  }
  if (!Array.isArray(plan.messages) || plan.messages.length === 0 || plan.messages.length > 100) {
    fail('plan messages are out of bounds');
  }
  if (!Array.isArray(plan.tools) || plan.tools.length > 16) fail('plan tools are out of bounds');
  verifyModuleGraph(plan.pins);
  verifyCommitment(plan.route);
  const route = plan.route;

  const seal = installSealedFetch(route);
  sealState = seal;

  let provider;
  try {
    // The published provider is TypeScript with `.js` relative specifiers;
    // this hook maps only inside the pinned root and only after the manifest
    // has hashed every file.
    register(
      pathToFileURL(`${plan.pins.sdk_root.replace(/\/$/, '')}/ts-resolve.mjs`).href,
      import.meta.url,
      { data: { sdkRoot: plan.pins.sdk_root.replace(/\/$/, '') } },
    );
    provider = await import(pathToFileURL(plan.pins.pi_devin_entry).href);
  } catch {
    writeFailure(seal, 'unsupported_transport');
    return;
  }
  if (typeof provider.streamDevin !== 'function') {
    writeFailure(seal, 'unsupported_transport');
    return;
  }

  const model = {
    id: route.model_id,
    name: route.model_id,
    api: 'devin',
    provider: 'devin',
    baseUrl: route.origin,
    input: ['text'],
    reasoning: Boolean(route.reasoning),
    thinkingLevelMap: route.reasoning ? route.thinking_level_map : undefined,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: 262144,
    maxTokens: route.max_output_tokens,
  };
  const context = { messages: plan.messages };
  const options = {
    apiKey: request.api_key,
    maxTokens: route.max_output_tokens,
    signal: AbortSignal.timeout(60_000),
    env: { DEVIN_API_SERVER_URL: route.origin },
    reasoning: route.reasoning ? route.effort : undefined,
  };

  let terminal = null;
  let errorEvent = false;
  try {
    const stream = provider.streamDevin(model, context, options);
    for await (const event of stream) {
      if (!event || typeof event !== 'object') continue;
      if (event.type === 'done') {
        terminal = event.message;
      } else if (event.type === 'error') {
        errorEvent = true;
      }
    }
    if (!terminal && !errorEvent) {
      const result = await stream.result?.();
      terminal = result ?? null;
    }
  } catch {
    terminal = null;
  }

  request.api_key = '';
  if (!terminal) {
    if (seal.inferenceRequests === 0) {
      const code = seal.authRequests > 0 ? 'auth_unavailable' : 'unsupported_transport';
      process.stdout.write(JSON.stringify(reply('refused', { error: code }, seal)) + '\n');
      return;
    }
    process.stdout.write(JSON.stringify(reply('unknown', { error: 'outcome_unknown' }, seal)) + '\n');
    return;
  }

  let assistant;
  try {
    if (
      seal.inferenceRequests !== 1 ||
      !seal.eos ||
      !seal.finishSeen ||
      !seal.usageSeen
    ) {
      fail('stream evidence is incomplete');
    }
    assistant = sanitizeAssistant(terminal, route);
  } catch {
    process.stdout.write(JSON.stringify(reply('unknown', { error: 'outcome_unknown' }, seal)) + '\n');
    return;
  }
  const output = reply('done', { assistant }, seal);
  const encoded = JSON.stringify(output);
  if (Buffer.byteLength(encoded, 'utf8') > MAX_REPLY) fail('reply exceeds its bound');
  process.stdout.write(encoded + '\n');
};

// Local alias to keep the module-verification entry point explicit.
const verifyModuleGraph = pins => verifyModules(pins);

main().catch(() => {
  // A failure after admission is never reported as "not dispatched": if the
  // seal saw an inference attempt, the outcome is unknown and keeps its hold.
  const seal = sealState;
  const outcome = seal && seal.inferenceRequests > 0 ? 'unknown' : 'refused';
  const code =
    outcome === 'unknown'
      ? 'outcome_unknown'
      : seal && seal.authRequests > 0
        ? 'auth_unavailable'
        : 'unsupported_transport';
  try {
    process.stdout.write(
      JSON.stringify(reply(outcome, { error: code }, seal ?? undefined)) + '\n',
    );
  } catch {
    // nothing else can be done
  }
});
