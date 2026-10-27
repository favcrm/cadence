// The keyless native-model provider bridge imported by the protected Pi guest.
//
// The guest's Pi 1.4 agent loop keeps owning validation, tool execution and the
// transcript. This module exposes fixed, shared-schema Connection tools and
// turns each model request into strict private operations frames, reconstructing
// a real `AssistantMessageEventStream` from the sanitized terminal receipt. It
// performs no network I/O, reads no environment/auth file, runs no command and
// imports no agent/tool runtime.
//
// Wire: one newline JSON `{v:1,method,args}` per call over one Unix socket
// connection, one newline JSON reply, 10s per frame and bounded bytes.

import { connect } from 'node:net';
import { readFileSync } from 'node:fs';

export const NATIVE_MODEL_PROVIDER = 'devin';

const FRAME_LIMIT = 1024 * 1024 + 8 * 1024;
const FRAME_DEADLINE_MS = 10_000;
const MAX_OUTPUT = 8192;
const MAX_MESSAGES = 100;
const MAX_TOOLS = 16;

const EXECUTION_WAIT_MS = 65_000;

const PREPARE = 'native_model_turn_prepare_v1';
const INVOKE = 'native_model_turn_v1';
const RESULT = 'native_model_turn_result_v1';

const safeInt = value => Number.isSafeInteger(value) && value >= 0;

const closed = (code, message) => {
  const error = new Error(message);
  error.code = code;
  return error;
};

const textPart = part => {
  if (!part || typeof part !== 'object' || typeof part.text !== 'string') {
    throw closed('unsupported_content', 'the model content is not supported');
  }
  const out = { type: 'text', text: part.text };
  if (typeof part.textSignature === 'string') out.textSignature = part.textSignature;
  return out;
};

const toolDeclaration = tool => {
  if (!tool || typeof tool.name !== 'string' || typeof tool.description !== 'string') {
    throw closed('unsupported_content', 'the tool declaration is not supported');
  }
  const out = {
    name: tool.name,
    description: tool.description,
    parameters: tool.parameters ?? {},
  };
  if (tool.constrainedSampling && typeof tool.constrainedSampling === 'object') {
    out.constrainedSampling = tool.constrainedSampling;
  }
  return out;
};

const usageOf = message => {
  const usage = message?.usage;
  if (!usage || !safeInt(usage.input) || !safeInt(usage.output)) {
    throw closed('unsupported_content', 'the model history usage is not supported');
  }
  const cacheRead = safeInt(usage.cacheRead) ? usage.cacheRead : 0;
  const cacheWrite = safeInt(usage.cacheWrite) ? usage.cacheWrite : 0;
  return {
    input: usage.input,
    output: usage.output,
    cacheRead,
    cacheWrite,
    totalTokens: usage.input + usage.output + cacheRead + cacheWrite,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
  };
};

const projectMessage = message => {
  if (!message || typeof message !== 'object') {
    throw closed('invalid_request', 'the model message is not supported');
  }
  switch (message.role) {
    case 'system': {
      const content = Array.isArray(message.content)
        ? message.content.map(textPart)
        : typeof message.content === 'string'
          ? message.content
          : (() => {
              throw closed('unsupported_content', 'the system content is not supported');
            })();
      const out = { role: 'system', content, timestamp: message.timestamp };
      if (!safeInt(out.timestamp)) throw closed('unsupported_content', 'the message timestamp is malformed');
      if (message.sections && typeof message.sections === 'object') {
        out.sections = message.sections;
      }
      if (Array.isArray(message.toolsAdded) && message.toolsAdded.length > 0) {
        out.toolsAdded = message.toolsAdded.map(toolDeclaration);
      }
      if (Array.isArray(message.toolsRemoved) && message.toolsRemoved.length > 0) {
        out.toolsRemoved = message.toolsRemoved.map(tool => ({ name: String(tool.name) }));
      }
      return out;
    }
    case 'user': {
      if (typeof message.content === 'string') {
        return { role: 'user', content: message.content, timestamp: message.timestamp };
      }
      if (!Array.isArray(message.content)) {
        throw closed('unsupported_content', 'the user content is not supported');
      }
      for (const part of message.content) {
        if (part?.type !== 'text') {
          throw closed('unsupported_content', 'images are not supported by this model route');
        }
      }
      const out = {
        role: 'user',
        content: message.content.map(textPart),
        timestamp: message.timestamp,
      };
      if (!safeInt(out.timestamp)) throw closed('unsupported_content', 'the message timestamp is malformed');
      return out;
    }
    case 'toolResult': {
      if (!Array.isArray(message.content)) {
        throw closed('unsupported_content', 'the tool result content is not supported');
      }
      for (const part of message.content) {
        if (part?.type !== 'text') {
          throw closed('unsupported_content', 'image tool results are not supported');
        }
      }
      const out = {
        role: 'toolResult',
        toolCallId: message.toolCallId,
        toolName: message.toolName,
        content: message.content.map(textPart),
        isError: Boolean(message.isError),
        timestamp: message.timestamp,
      };
      if (!safeInt(out.timestamp)) throw closed('unsupported_content', 'the message timestamp is malformed');
      if (safeInt(message.durationMs)) out.durationMs = message.durationMs;
      return out;
    }
    case 'assistant': {
      if (!['stop', 'length', 'toolUse'].includes(message.stopReason)) {
        throw closed('unsupported_content', 'the assistant history is not a successful turn');
      }
      const content = [];
      for (const part of Array.isArray(message.content) ? message.content : []) {
        if (part?.type === 'text') content.push(textPart(part));
        else if (part?.type === 'thinking') {
          const block = { type: 'thinking', thinking: String(part.thinking ?? '') };
          if (typeof part.thinkingSignature === 'string') block.thinkingSignature = part.thinkingSignature;
          if (part.redacted === true) block.redacted = true;
          content.push(block);
        } else if (part?.type === 'toolCall') {
          const block = {
            type: 'toolCall',
            id: String(part.id),
            name: String(part.name),
            arguments: part.arguments ?? {},
          };
          if (typeof part.thoughtSignature === 'string') block.thoughtSignature = part.thoughtSignature;
          if (typeof part.namespace === 'string') block.namespace = part.namespace;
          if (typeof part.argumentsError === 'string') block.argumentsError = part.argumentsError;
          content.push(block);
        } else {
          throw closed('unsupported_content', 'the assistant content is not supported');
        }
      }
      const out = {
        role: 'assistant',
        content,
        api: String(message.api),
        provider: String(message.provider),
        model: String(message.model),
        usage: usageOf(message),
        stopReason: message.stopReason,
        timestamp: message.timestamp,
      };
      if (!safeInt(out.timestamp)) throw closed('unsupported_content', 'the message timestamp is malformed');
      if (typeof message.responseModel === 'string') out.responseModel = message.responseModel;
      if (typeof message.responseId === 'string') out.responseId = message.responseId;
      if (typeof message.providerThinkingLevel === 'string') {
        out.providerThinkingLevel = message.providerThinkingLevel;
      }
      if (typeof message.thinkingLevel === 'string') out.thinkingLevel = message.thinkingLevel;
      if (safeInt(message.durationMs)) out.durationMs = message.durationMs;
      return out;
    }
    default:
      throw closed('unsupported_content', 'the model message role is not supported');
  }
};

const projectMessages = context => {
  const messages = Array.isArray(context?.messages) ? context.messages : [];
  if (messages.length === 0 || messages.length > MAX_MESSAGES) {
    throw closed('payload_too_large', 'the model message count is out of bounds');
  }
  return messages.map(projectMessage);
};

/** The contiguous request index of this model call inside the current turn. */
const requestIndex = context => {
  const messages = Array.isArray(context?.messages) ? context.messages : [];
  let lastUser = -1;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    if (messages[index]?.role === 'user') {
      lastUser = index;
      break;
    }
  }
  let assistants = 0;
  for (let index = lastUser + 1; index < messages.length; index += 1) {
    if (messages[index]?.role === 'assistant') assistants += 1;
  }
  return String(assistants);
};

// The root operations server consumes exactly one frame per connection.
// Never retry a submitted mutation when its acknowledgement is uncertain.
const callData = (socketPath, method, args, deadlineMs = FRAME_DEADLINE_MS, signal) => {
  let encoded;
  try {
    encoded = JSON.stringify({ v: 1, method, args });
  } catch {
    return Promise.reject(closed('invalid_request', 'the operations request could not be encoded'));
  }
  if (Buffer.byteLength(encoded) > FRAME_LIMIT) {
    return Promise.reject(closed('payload_too_large', 'the operations request exceeds its byte bound'));
  }
  if (signal?.aborted) {
    return Promise.reject(closed('operation_cancelled', 'the operation was cancelled before submission'));
  }
  return new Promise((resolve, reject) => {
    const socket = connect({ path: socketPath });
    const chunks = [];
    let bytes = 0;
    let connected = false;
    let settled = false;
    let timer;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
      socket.destroy();
      if (error) reject(error);
      else resolve(value);
    };
    const lost = () => finish(closed(
      connected ? 'outcome_unknown' : 'runtime_unqualified',
      'the operations connection was lost',
    ));
    const abort = () => finish(closed(
      connected ? 'outcome_unknown' : 'operation_cancelled',
      'the operations wait was cancelled',
    ));
    timer = setTimeout(() => finish(closed(
      'runtime_unqualified', 'the operations socket did not open',
    )), FRAME_DEADLINE_MS);
    signal?.addEventListener('abort', abort, { once: true });
    socket.once('connect', () => {
      if (settled) return;
      connected = true;
      clearTimeout(timer);
      timer = setTimeout(() => finish(closed(
        'outcome_unknown', 'the operations frame deadline elapsed',
      )), deadlineMs);
      socket.write(encoded + '\n');
    });
    socket.on('error', lost);
    socket.on('end', lost);
    socket.on('close', lost);
    socket.on('data', chunk => {
      if (settled) return;
      bytes += chunk.length;
      if (bytes > FRAME_LIMIT + 1) {
        finish(closed('outcome_unknown', 'the operations reply exceeds its byte bound'));
        return;
      }
      chunks.push(chunk);
      const newline = chunk.indexOf(0x0a);
      if (newline < 0) return;
      if (newline !== chunk.length - 1) {
        finish(closed('outcome_unknown', 'the operations reply has trailing bytes'));
        return;
      }
      let reply;
      try {
        reply = JSON.parse(Buffer.concat(chunks, bytes).subarray(0, bytes - 1).toString('utf8'));
      } catch {
        finish(closed('outcome_unknown', 'the operations reply is malformed'));
        return;
      }
      if (reply?.ok === true && Object.hasOwn(reply, 'data')) {
        finish(null, reply.data);
      } else {
        const code = typeof reply?.error?.code === 'string' ? reply.error.code : 'outcome_unknown';
        // The daemon's `message` is a fixed authored classification, never a
        // prompt, provider body or credential. Carry it (bounded) so an
        // operator can tell which check refused instead of only the code.
        const detail =
          typeof reply?.error?.message === 'string' && reply.error.message.length > 0
            ? reply.error.message.slice(0, 200)
            : '';
        finish(closed(
          code,
          detail
            ? `the protected operation was refused: ${detail}`
            : 'the protected operation was refused',
        ));
      }
    });
  });
};

const CONNECTION_TOOLS = [
  ['connection_snapshot', 'Read the authorized Connection capability snapshot.', 'connection-snapshot-args.schema.json'],
  ['connection_stage_send', 'Stage an SMTP message for explicit human approval; never sends it.', 'connection-stage-send-args.schema.json'],
  ['connection_quote', 'Quote a bounded model operation without executing it.', 'connection-quote-args.schema.json'],
  ['connection_invoke', 'Execute an authorized frozen Connection operation with its approval or quote.', 'connection-invoke.schema.json'],
  ['connection_result', 'Read an existing dispatch result without executing or retrying it.', 'connection-result-args.schema.json'],
  ['connection_request', 'Ask the human for trusted credential entry using metadata only; never request a secret in chat.', 'connection-request-args.schema.json'],
];

/** Public Pi ToolDefinitions, using the exact generated shared wire schemas. */
export function createNativeConnectionTools({
  socketPath = '/opt/cadence/run/ops.sock',
  compileSchema,
} = {}) {
  if (typeof compileSchema !== 'function') {
    throw closed('unsupported_transport', 'the pinned SDK schema compiler is not supplied');
  }
  return CONNECTION_TOOLS.map(([name, description, filename]) => {
    const parameters = JSON.parse(readFileSync(
      new URL(`./connection-schemas/${filename}`, import.meta.url), 'utf8',
    ));
    const validator = compileSchema(parameters);
    // Pi's later validation may coerce types. Reject raw nonconforming args
    // first, so decimal-string revisions never round-trip through JS numbers.
    const prepareArguments = args => {
      if (!validator.Check(args)) {
        throw closed('invalid_request', 'the tool arguments do not match the shared contract');
      }
      return args;
    };
    return {
      name,
      label: name,
      description,
      promptSnippet: description,
      parameters,
      prepareArguments,
      executionMode: 'sequential',
      execute: async (_toolCallId, args, signal) => {
        const reply = await callData(
          socketPath, `${name}_v1`, prepareArguments(args), EXECUTION_WAIT_MS, signal,
        );
        return {
          content: [{ type: 'text', text: JSON.stringify(reply) }],
          details: reply,
        };
      },
    };
  });
}

const emptyUsage = () => ({
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheWrite: 0,
  totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
});

const assistantMessage = (stream, configured, receipt) => {
  const message = receipt.assistant;
  if (!message || typeof message !== 'object') {
    throw closed('result_refused', 'the model result is unavailable');
  }
  return {
    role: 'assistant',
    content: Array.isArray(message.content) ? message.content : [],
    api: message.api,
    provider: message.provider,
    model: message.model ?? configured.model,
    usage: message.usage ?? emptyUsage(),
    stopReason: message.stopReason,
    timestamp: safeInt(message.timestamp) ? message.timestamp : Date.now(),
    ...(typeof message.responseModel === 'string' ? { responseModel: message.responseModel } : {}),
    ...(typeof message.responseId === 'string' ? { responseId: message.responseId } : {}),
    ...(typeof message.providerThinkingLevel === 'string'
      ? { providerThinkingLevel: message.providerThinkingLevel }
      : {}),
    ...(typeof message.thinkingLevel === 'string' ? { thinkingLevel: message.thinkingLevel } : {}),
    ...(safeInt(message.durationMs) ? { durationMs: message.durationMs } : {}),
  };
};

const emitStream = (stream, configured, receipt) => {
  const message = assistantMessage(stream, configured, receipt);
  const partial = { ...message, content: [], usage: emptyUsage(), stopReason: 'stop' };
  stream.push({ type: 'start', partial });
  message.content.forEach((block, index) => {
    if (block.type === 'text') {
      partial.content.push({ type: 'text', text: '' });
      stream.push({ type: 'text_start', contentIndex: index, partial });
      partial.content[index].text = block.text;
      if (block.text) {
        stream.push({ type: 'text_delta', contentIndex: index, delta: block.text, partial });
      }
      stream.push({ type: 'text_end', contentIndex: index, content: block.text, partial });
      return;
    }
    if (block.type === 'thinking') {
      partial.content.push({ type: 'thinking', thinking: '' });
      stream.push({ type: 'thinking_start', contentIndex: index, partial });
      partial.content[index].thinking = block.thinking;
      if (block.thinking) {
        stream.push({
          type: 'thinking_delta',
          contentIndex: index,
          delta: block.thinking,
          partial,
        });
      }
      stream.push({
        type: 'thinking_end',
        contentIndex: index,
        content: block.thinking,
        partial,
      });
      return;
    }
    if (block.type === 'toolCall') {
      partial.content.push({ type: 'toolCall', id: block.id, name: block.name, arguments: {} });
      stream.push({ type: 'toolcall_start', contentIndex: index, partial });
      const encoded = JSON.stringify(block.arguments ?? {});
      stream.push({ type: 'toolcall_delta', contentIndex: index, delta: encoded, partial });
      partial.content[index] = {
        ...block,
        arguments: block.arguments ?? {},
      };
      stream.push({
        type: 'toolcall_end',
        contentIndex: index,
        toolCall: { type: 'toolCall', id: block.id, name: block.name, arguments: block.arguments ?? {} },
        partial,
      });
    }
  });
  stream.push({
    type: 'done',
    reason: message.stopReason,
    message,
  });
  stream.end(message);
};

const emitError = (stream, configured, error) => {
  // `closed()` messages are bridge-authored fixed text (never prompt, provider
  // body or credential material). Carry the bounded classification so an
  // operator can distinguish an encoding/projection refusal from a daemon
  // code instead of seeing only `invalid_request`.
  const detail =
    typeof error?.message === 'string' && error.message.length > 0
      ? error.message.slice(0, 200)
      : '';
  const message = {
    role: 'assistant',
    content: [],
    api: 'devin',
    provider: 'devin',
    model: configured.model,
    usage: emptyUsage(),
    stopReason: 'error',
    timestamp: Date.now(),
    errorMessage:
      typeof error?.code === 'string'
        ? detail
          ? `${error.code}: ${detail}`
          : error.code
        : 'result_refused',
  };
  stream.push({ type: 'start', partial: message });
  stream.push({ type: 'error', reason: 'error', error: message });
  stream.end(message);
};

/**
 * Create the keyless Pi provider for the protected native route.
 *
 * `provider`/the actual `model`/`effort` are the frozen guest configuration;
 * the broker independently derives them from durable state. The public SDK
 * auth marker below is NOT a credential or an authority: only the root-owned
 * socket's live admission can execute the configured upstream route.
 */
export function createNativeModelProvider({
  socketPath = '/opt/cadence/run/ops.sock',
  provider = NATIVE_MODEL_PROVIDER,
  model,
  effort,
  piAi,
} = {}) {
  if (provider !== NATIVE_MODEL_PROVIDER || !model || model.provider !== provider
      || typeof model.id !== 'string' || !model.id || typeof model.api !== 'string') {
    throw closed('unsupported_transport', 'the native model provider is not qualified');
  }
  // The bootstrap passes its own public Pi 1.4 module namespace; the bridge
  // never resolves a second ambient Pi graph.
  const createAssistantMessageEventStream = piAi?.createAssistantMessageEventStream;
  if (typeof createAssistantMessageEventStream !== 'function' || typeof piAi?.createProvider !== 'function') {
    throw closed('unsupported_transport', 'the guest Pi model runtime is not supplied');
  }
  const runTurn = (requestedModel, context, options) => {
    const stream = createAssistantMessageEventStream();
    const configured = {
      model: model.id,
      effort: effort ?? '',
    };
    void (async () => {
      try {
        if (requestedModel?.id !== model.id || requestedModel?.provider !== provider
            || requestedModel?.api !== model.api
            || (options?.reasoning !== undefined && options.reasoning !== effort)) {
          throw closed('configuration_changed', 'the requested model or effort differs from the pinned route');
        }
        const messages = projectMessages(context);
        const maxOutput = Math.min(
          MAX_OUTPUT,
          safeInt(requestedModel?.maxTokens) && requestedModel.maxTokens > 0
            ? requestedModel.maxTokens
            : MAX_OUTPUT,
        );
        const toolChoice = options?.toolChoice === 'none' ? 'none' : 'auto';
        const prepared = await callData(socketPath, PREPARE, {
          v: 1,
          request_index: requestIndex(context),
        });
        const invokeArgs = {
          v: 1,
          request_id: prepared.request_id,
          configuration_id: prepared.configuration_id,
          expected_configuration_revision: prepared.configuration_revision,
          snapshot_id: prepared.snapshot_id,
          loaded_receipt_id: prepared.loaded_receipt_id,
          messages,
          max_output_tokens: maxOutput,
          tool_choice: toolChoice,
        };
        let receipt = await callData(socketPath, INVOKE, invokeArgs, EXECUTION_WAIT_MS, options?.signal);
        if (receipt?.state === 'prepared' || receipt?.state === 'committed') {
          receipt = await callData(socketPath, RESULT, {
            v: 1,
            request_id: prepared.request_id,
          });
        }
        if (!receipt || typeof receipt.state !== 'string') {
          throw closed('outcome_unknown', 'the model receipt is malformed');
        }
        if (receipt.state === 'done' && receipt.assistant) {
          emitStream(stream, configured, receipt);
          return;
        }
        const code = receipt.error?.code ?? (receipt.state === 'unknown' ? 'outcome_unknown' : 'result_refused');
        emitError(stream, configured, closed(code, 'the model turn was not completed'));
      } catch (error) {
        emitError(stream, configured, error);
      }
    })();
    return stream;
  };
  return piAi.createProvider({
    id: NATIVE_MODEL_PROVIDER,
    name: 'Cadence protected native model',
    auth: {
      apiKey: {
        name: 'Root-owned native operations socket (keyless)',
        check: async ({ signal }) => signal.aborted
          ? undefined : { type: 'api_key', source: 'cadence-native-ops' },
        resolve: async ({ signal }) => signal.aborted ? undefined : {
          auth: { apiKey: 'cadence-native-ops-nonauthorizing-v1' },
          source: 'cadence-native-ops',
        },
      },
    },
    models: [model],
    api: { stream: runTurn, streamSimple: runTurn },
  });
}
