import type { Api, AssistantMessage, Model, Provider } from "@earendil-works/pi-ai";
import { CLOUDFLARE_AI_API, CLOUDFLARE_PROVIDER_ID } from "agents/models/pi-ai";
import type { GenerationIdentity } from "./contract.ts";
import type { ContextBudgetProofs } from "./context-budget-proofs.ts";
import {
  IMAGE_INPUT_UNAVAILABLE,
  MAX_IMAGE_BASE64_CHARS,
  MAX_IMAGE_PARTS,
} from "./image-limits.ts";

/**
 * Model transport for one hosted conversation.
 *
 * The runtime never pins a provider model. It sends the deployment's platform
 * default alias and the application's `requestModel` hands the call to the
 * platform gateway, which resolves, meters and settles it. The descriptor
 * below exists only so the engine's transport has an id and metadata; it is
 * never a claim about which model serves.
 */

/** Context budget handed to the engine; the resolved model's real window is not exposed. */
export const MODEL_CONTEXT_WINDOW = 128_000;

/**
 * CAD-2414: Pi's compaction thresholds for hosted sessions. The binding limit
 * is the gateway's 256 KiB request body, not the model's window. The engine
 * counts tokens from the last reply's reported usage, and estimates the rest
 * (the first request, the first request after a compaction, and the newest
 * step's results) at 3.5 characters a token.
 *
 * - Measured on the hosted default model (deepseek-v4.1-flash, 2026-11-17)
 *   with requests shaped like hosted traffic: English JSON, HTML and prose
 *   at 3.45 bytes a reported token, Chinese JSON and prose at 3.85. So
 *   256 KiB holds about 68–76k reported tokens, and the default reserve
 *   (window − 16,384 ≈ 112k) never fired before the gateway refused.
 * - Where the engine estimates instead, Chinese (3 bytes a character, about
 *   2 characters a real token) costs about 10.5 bytes an estimated token.
 *
 * - Blocking compaction at 128,000 − 80,000 = 48k tokens (about 185 KB at
 *   3.85 bytes a token), background 16,384 tokens earlier (about 31.6k,
 *   about 122 KB).
 * - The kept tail is 8,000 tokens, so the first request after a compaction
 *   stays far under the budget even when the tail is estimated Chinese text
 *   (8,000 × 10.5 ≈ 84 KB).
 * - Estimated Chinese tool results in one operation are kept in budget by
 *   `boundActiveToolResults`, not by these thresholds.
 * - The summary's output has its own budget, `SUMMARY_MAX_OUTPUT_TOKENS`;
 *   a turn's answer keeps the model's 8,192 output tokens.
 */
export const HOSTED_COMPACTION_POLICY = {
  reserveTokens: 80_000,
  keepRecentTokens: 8_000,
  backgroundTokens: 16_384,
} as const;

const MODEL_MAX_OUTPUT_TOKENS = 8192;

/**
 * CAD-2480: the request size at which hosted blocking compaction starts,
 * whatever the token count says. The token rule reads the last reply's
 * reported usage and estimates the newest step at 3.5 characters a token, so a
 * thread of large HTML tool-call arguments (each quote is escaped twice in
 * the request body, the estimate counts it once) reaches the gateway's 256 KiB
 * cap while the estimate is still under its threshold. 160 KiB is the token
 * rule's own design point: 48k tokens at 3.41 bytes a token, just under the
 * 3.45 bytes a token measured for English JSON and HTML (see above). It leaves
 * 96 KiB (37.5%) for the step in flight, the tool results the engine only
 * estimates, and the summary's own prompt.
 */
export const BYTE_COMPACTION_THRESHOLD = 160 * 1024;

/**
 * CAD-2430: the output budget of the engine's compaction summary, which the
 * engine otherwise caps at the model's 8,192 tokens. A summary that stops at
 * the cap is discarded and the blocking compaction fails ("The conversation
 * could not be compacted"). A model may reason before it writes, and those
 * tokens count against the same budget. Measured on 2026-11-18, replaying
 * demo-company's failed summary (a 362-entry conversation, a 222 KB request)
 * on `tencent/hy4-flash`: 6,330 output tokens, 3,629 of them reasoning, so
 * the cap left about 1,900 tokens of room. 32,768 leaves room for a longer
 * history and for reasoning, and applies only to the attested summary request.
 */
export const SUMMARY_MAX_OUTPUT_TOKENS = 32_768;

/** Raise, never lower, the summary request's output budget. */
function withSummaryBudget(input: Record<string, unknown>): Record<string, unknown> {
  const field = "max_completion_tokens" in input ? "max_completion_tokens" : "max_tokens";
  const current = input[field];
  if (typeof current === "number" && current >= SUMMARY_MAX_OUTPUT_TOKENS) return input;
  return { ...input, [field]: SUMMARY_MAX_OUTPUT_TOKENS };
}

/**
 * CAD-2658: tools change with every deploy, so a summarized tool failure must
 * not steer later turns. Appended to the system prompt of the attested
 * summarization request only; a turn request never carries it.
 *
 * CAD-2729: Pi's base prompt asks to "preserve exact ... error messages", and
 * without an explicit override the summary quoted every tool error verbatim
 * (CAD-2725 eval). The override covers tool output only: an error the user
 * reports is a fact about their business and is kept.
 */
export const SUMMARY_INSTRUCTION =
  "Keep the user's goals, decisions, preferences, facts about their business and open tasks. " +
  "Do not record tool errors, refusals, retries or \"X doesn't work\" as lasting facts or as instructions; " +
  "at most say that an earlier attempt to do X failed and to try again normally. " +
  "For tool calls this replaces the request to preserve exact error messages: do not quote a tool's error code, " +
  "error text or loop-guard message, and do not copy tool-call argument shapes into the summary.";

function withSummaryInstruction(input: Record<string, unknown>): Record<string, unknown> {
  const source = summarySource(input);
  if (source === null) return input;
  const system = source.system as Record<string, unknown>;
  const content = system.content;
  const extra = "\n\n" + SUMMARY_INSTRUCTION;
  const next =
    typeof content === "string" ? content + extra
    : Array.isArray(content) ? [...content, { type: "text", text: extra.trimStart() }]
    : null;
  if (next === null) return input;
  return { ...input, messages: [{ ...system, content: next }, (input.messages as unknown[])[1]] };
}

/**
 * Models stored in sessions created before the alias landed. The engine
 * resolves a durable session by `(provider, id)` through the provider
 * catalog, so an old session whose stored id is one of these otherwise
 * refuses to start against the new provider. The transport still ignores the
 * id and sends the alias; these entries only let the old session wake and
 * reach that transport.
 */
const LEGACY_MODEL_IDS = ["@cf/zai-org/glm-5.3-flash", "z-ai/glm-5.3-flash"] as const;

/**
 * The descriptor for the platform default.
 *
 * The engine needs a model object (id, api, provider, cost, windows); the id
 * here is the alias the platform resolves, never a provider model id.
 */
export function defaultModel(alias: string, acceptsImages = false): Model<Api> {
  return {
    id: alias,
    name: "Platform default",
    api: CLOUDFLARE_AI_API,
    provider: CLOUDFLARE_PROVIDER_ID,
    baseUrl: "",
    input: acceptsImages ? ["text", "image"] : ["text"],
    cost: { cacheRead: 0, cacheWrite: 0, input: 0, output: 0 },
    reasoning: false,
    contextWindow: MODEL_CONTEXT_WINDOW,
    maxTokens: MODEL_MAX_OUTPUT_TOKENS,
  };
}

/** Legacy descriptors use the same transport; their concrete id is never sent. */
function legacyModels(model: Model<Api>): Model<Api>[] {
  return LEGACY_MODEL_IDS.map((id) => ({
    ...model,
    id,
    name: "Legacy tenant model",
  }));
}

/**
 * Register the alias model on the provider the engine resolves sessions
 * through.
 *
 * The engine stores a session's model as `(provider, id)` and looks it up in
 * the `Models` registry, so a model object alone is not enough: the alias must
 * appear in the provider's catalog. The provider's streaming implementation is
 * unchanged — every model on it runs through the private transport.
 */
export function withDefaultModel(provider: Provider, model: Model<Api>): Provider {
  const models = [model, ...legacyModels(model)];
  return {
    ...provider,
    getModels: () => [...provider.getModels(), ...models],
    getAllModels: () => [...(provider.getAllModels?.() ?? provider.getModels()), ...models],
  };
}

/** The served model id the gateway reported in the relayed response bytes. */
const MAX_MODEL_SCAN_BYTES = 64 * 1024;
const MODEL_FIELD = /"model"\s*:\s*"([^"]{1,200})"/;

/**
 * Observe the model the gateway actually served, then relay the same bytes.
 *
 * The gateway names the resolved model in every chat-completion frame (and in
 * the non-stream body); the runtime only reads it to label the board, it never
 * sends it back as a request field.
 */
export function captureResolvedModel(
  response: Response,
  onModelResolved: (model: string) => void,
): Response {
  if (!response.ok || response.body === null) return response;
  const contentType = response.headers.get("content-type") ?? "";
  if (!contentType.includes("text/event-stream") && !contentType.includes("application/json")) {
    return response;
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let scanned = 0;
  let seen = false;
  let buffer = "";
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      const { done, value } = await reader.read();
      if (done) {
        controller.close();
        return;
      }
      if (!seen && scanned < MAX_MODEL_SCAN_BYTES) {
        scanned += value.byteLength;
        buffer += decoder.decode(value, { stream: true });
        const match = MODEL_FIELD.exec(buffer);
        if (match?.[1]) {
          seen = true;
          buffer = "";
          try {
            onModelResolved(match[1]);
          } catch {
            // The label is advisory; a failed write never breaks the stream.
          }
        } else if (buffer.length > MAX_MODEL_SCAN_BYTES) {
          buffer = "";
        }
      }
      controller.enqueue(value);
    },
    async cancel(reason) {
      return reader.cancel(reason);
    },
  });
  return new Response(body, {
    status: response.status,
    headers: response.headers,
  });
}

/** The model call surfaces the engine's transport uses. */
export interface MeteredTransport {
  requestModel(
    input: Record<string, unknown>,
    generation: GenerationIdentity,
    signal?: AbortSignal,
  ): Promise<Response>;
  resumeModel?(url: string, signal?: AbortSignal): Promise<Response>;
}

/**
 * The shape the engine's Workers AI transport expects from a binding:
 * `run(model, input, { returnRawResponse: true })` resolves to the upstream
 * `Response`, and `fetch` resumes a provider-native stream.
 */
export interface MeteredBinding {
  run(
    model: string,
    input: Record<string, unknown>,
    options?: { returnRawResponse?: boolean; signal?: AbortSignal },
  ): Promise<Response | unknown>;
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
  aiGatewayLogId: null;
}

/**
 * Pi supplies one user message containing the whole compaction source. The
 * hosted gateway limits each message to 64,000 characters, independently of
 * the overall byte budget. Preserve the source in consecutive messages, only
 * for the engine's attested summary. Pi's render hook already shaped the
 * structured messages, so the split never decides what stays.
 */
function splitSummaryMessages(input: Record<string, unknown>): Record<string, unknown> {
  const source = summarySource(input);
  if (source === null) return input;
  const { system, user } = source;
  const text = source.text;
  const limit = 64_000;
  if (text.length <= limit) return input;
  const messages: unknown[] = [system];
  for (let start = 0; start < text.length;) {
    let end = Math.min(start + limit, text.length);
    const last = text.charCodeAt(end - 1);
    const next = text.charCodeAt(end);
    if (last >= 0xd800 && last <= 0xdbff && next >= 0xdc00 && next <= 0xdfff) end--;
    messages.push({ ...user, content: text.slice(start, end) });
    start = end;
  }
  return { ...input, messages };
}

/** The engine's summarization request: its system prompt and one user message of text. */
function summarySource(
  input: Record<string, unknown>,
): { system: unknown; user: Record<string, unknown>; text: string } | null {
  if (!Array.isArray(input.messages) || input.messages.length !== 2) return null;
  const [system, user] = input.messages;
  if (
    typeof system !== "object" || system === null || system.role !== "system" ||
    typeof user !== "object" || user === null || user.role !== "user"
  ) return null;
  if (typeof user.content === "string") return { system, user, text: user.content };
  if (
    Array.isArray(user.content) && user.content.length === 1 &&
    user.content[0]?.type === "text" && typeof user.content[0].text === "string"
  ) return { system, user, text: user.content[0].text };
  return null;
}

/**
 * The gateway refuses a body over 256 KiB before dispatch, and its per-message
 * limit is separate. A summary is assembled from stored history, so splitting
 * one oversized message is not enough: the assembled request can still exceed
 * the total. Refuse it here rather than spend a call the gateway must reject.
 */
const SUMMARY_BODY_BYTES = 256 * 1024;

/** The JSON body the transport sends, measured the way the gateway reads it. */
function requestBytes(input: Record<string, unknown>): number {
  return new TextEncoder().encode(JSON.stringify(input)).length;
}

const HISTORICAL_TOOL_RESULT_CHARS = 12_000;
const HISTORICAL_TOOL_RESULT_NOTICE =
  "\n\n[Earlier tool result shortened to save context. This excerpt is incomplete; read the source again before relying on or editing its full contents.]";

/**
 * Bound old tool results only in the model request. A completed assistant
 * answer proves the preceding results belong to earlier work; everything
 * after the latest such answer stays whole here, including all pages of a
 * file the active operation is editing. Only a request over the gateway
 * budget shortens more (`boundActiveToolResults`). The durable transcript is
 * never changed.
 */
function boundToolHistory(input: Record<string, unknown>): Record<string, unknown> {
  if (!Array.isArray(input.messages)) return input;
  const original: unknown[] = input.messages;
  let completed = -1;
  for (let index = original.length - 1; index >= 0; index--) {
    const message = original[index];
    if (
      typeof message === "object" &&
      message !== null &&
      !Array.isArray(message) &&
      "role" in message &&
      message.role === "assistant" &&
      "content" in message &&
      typeof message.content === "string" &&
      message.content.length > 0 &&
      (!("tool_calls" in message) ||
        message.tool_calls === null ||
        (Array.isArray(message.tool_calls) && message.tool_calls.length === 0))
    ) {
      completed = index;
      break;
    }
  }
  if (completed < 0) return input;
  let messages: unknown[] | undefined;
  for (let index = 0; index < completed; index++) {
    const message = original[index];
    if (
      typeof message !== "object" ||
      message === null ||
      Array.isArray(message) ||
      !("role" in message) ||
      message.role !== "tool" ||
      !("content" in message) ||
      typeof message.content !== "string" ||
      message.content.length <= HISTORICAL_TOOL_RESULT_CHARS
    )
      continue;
    let end = HISTORICAL_TOOL_RESULT_CHARS;
    const previous = message.content.charCodeAt(end - 1);
    const next = message.content.charCodeAt(end);
    if (previous >= 0xd800 && previous <= 0xdbff && next >= 0xdc00 && next <= 0xdfff)
      end--;
    messages ??= original.slice();
    messages[index] = {
      ...message,
      content: message.content.slice(0, end) + HISTORICAL_TOOL_RESULT_NOTICE,
    };
  }
  return messages === undefined ? input : { ...input, messages };
}

/**
 * CAD-2414: the gateway's text budget for one request (prompt, host context,
 * tool schemas). Image data has its own headroom and is not counted here.
 * The target leaves room for the gateway's own framing.
 */
const REQUEST_TEXT_BYTES = 256 * 1024;
const REQUEST_TEXT_TARGET = REQUEST_TEXT_BYTES - 16 * 1024;
/** At least this many newest tool results stay whole, and the newest round. */
const ACTIVE_TOOL_RESULTS_KEPT = 2;
/** Each pass shortens the oldest results first, to this many characters. */
const ACTIVE_TOOL_RESULT_PASSES = [HISTORICAL_TOOL_RESULT_CHARS, 2_000] as const;

const encoder = new TextEncoder();

/** The request's bytes against the gateway's text budget, images excluded. */
function textBytes(input: unknown): number {
  return encoder.encode(
    JSON.stringify(input, (key, value: unknown) => (key === "image_url" ? null : value)),
  ).length;
}

/** `text` cut to at most `end` characters, never inside a surrogate pair. */
function cutText(text: string, end: number): string {
  const previous = text.charCodeAt(end - 1);
  const next = text.charCodeAt(end);
  if (previous >= 0xd800 && previous <= 0xdbff && next >= 0xdc00 && next <= 0xdfff) end--;
  return text.slice(0, end);
}

/**
 * Keep one long operation's request inside the gateway budget. The engine
 * cannot compact the operation it is running (`hosted-preserve-active-
 * operation`), so once its own tool results pass 256 KiB every request would
 * be refused. Only then, and only in the model request, shorten tool results
 * oldest first, earlier history included, keeping the newest round and at
 * least `ACTIVE_TOOL_RESULTS_KEPT` results whole, until the request fits.
 * The durable transcript is never changed. A request that still does not fit
 * is sent as it is, and the gateway's refusal takes the existing
 * context-limit path.
 */
function boundActiveToolResults(input: Record<string, unknown>): Record<string, unknown> {
  if (!Array.isArray(input.messages)) return input;
  let bytes = textBytes(input);
  if (bytes <= REQUEST_TEXT_TARGET) return input;
  const messages: unknown[] = input.messages.slice();
  const results: number[] = [];
  let lastCall = -1;
  for (let index = 0; index < messages.length; index++) {
    const message = messages[index];
    if (typeof message !== "object" || message === null || Array.isArray(message) || !("role" in message))
      continue;
    if (message.role === "assistant") lastCall = index;
    else if (message.role === "tool" && "content" in message && typeof message.content === "string")
      results.push(index);
  }
  // The newest round's results (parallel calls answer one assistant turn)
  // have not been seen whole by the model yet; never cut them.
  const newestRound = results.findIndex((index) => index > lastCall);
  const keepFrom = Math.min(
    results.length - ACTIVE_TOOL_RESULTS_KEPT,
    newestRound < 0 ? results.length : newestRound,
  );
  const shortenable = results.slice(0, Math.max(0, keepFrom));
  for (const limit of ACTIVE_TOOL_RESULT_PASSES) {
    for (const index of shortenable) {
      const message = messages[index] as { content: string };
      if (message.content.length <= limit) continue;
      const content = cutText(message.content, limit) + HISTORICAL_TOOL_RESULT_NOTICE;
      bytes -= textBytes(message.content) - textBytes(content);
      messages[index] = { ...message, content };
      if (bytes <= REQUEST_TEXT_TARGET) return { ...input, messages };
    }
  }
  return { ...input, messages };
}

/**
 * Retain the newest image parts that fit one gateway request. Durable session
 * history is untouched; each omitted image is named in the model input.
 */
function boundImageHistory(
  input: Record<string, unknown>,
  maxImages: number,
  notice = "[An image was omitted from this request to fit the image limit.]",
): Record<string, unknown> {
  if (!Array.isArray(input.messages)) return input;
  const omitted = new Map<number, Set<number>>();
  let count = 0;
  let bytes = 0;
  for (let i = input.messages.length - 1; i >= 0; i--) {
    const message: unknown = input.messages[i];
    if (
      typeof message !== "object" ||
      message === null ||
      !("role" in message) ||
      message.role !== "user" ||
      !("content" in message) ||
      !Array.isArray(message.content)
    )
      continue;
    for (let j = message.content.length - 1; j >= 0; j--) {
      const part: unknown = message.content[j];
      if (
        typeof part !== "object" ||
        part === null ||
        !("type" in part) ||
        part.type !== "image_url" ||
        !("image_url" in part) ||
        typeof part.image_url !== "object" ||
        part.image_url === null ||
        !("url" in part.image_url)
      )
        continue;
      const url = part.image_url.url;
      if (typeof url !== "string" || !/^data:image\/(?:png|jpeg|webp);base64,/.test(url)) continue;
      const size = url.length - url.indexOf(",") - 1;
      if (count >= maxImages || bytes + size > MAX_IMAGE_BASE64_CHARS) {
        const indexes = omitted.get(i) ?? new Set<number>();
        indexes.add(j);
        omitted.set(i, indexes);
      } else {
        count++;
        bytes += size;
      }
    }
  }
  if (omitted.size === 0) return input;
  return {
    ...input,
    messages: input.messages.map((message: unknown, messageIndex: number) => {
      const indexes = omitted.get(messageIndex);
      if (
        !indexes ||
        typeof message !== "object" ||
        message === null ||
        !("content" in message) ||
        !Array.isArray(message.content)
      )
        return message;
      return {
        ...message,
        content: message.content.map((part: unknown, partIndex: number) =>
          indexes.has(partIndex)
            ? { type: "text", text: notice }
            : part,
        ),
      };
    }),
  };
}

const IMAGE_REFUSED_NOTICE =
  "[An image was omitted because the selected model cannot read images.]";

/**
 * The gateway's structured size refusal and its public, actionable projection.
 * The engine receives a task-bound marker that requests its bounded overflow
 * recovery, not this public sentence or the gateway's private details.
 */
export const CONTEXT_BUDGET_EXCEEDED = "context_budget_exceeded";
export const CONTEXT_BUDGET_EXCEEDED_TEXT =
  "The context limit was reached. Start a fresh conversation or split the task and retry.";

/**
 * Gateway refusal codes the runtime may name. Anything else stays private: the
 * engine reports a generic failure rather than provider or gateway text. The
 * context budget is the only one that must also arrive as the gateway's own
 * structured 400 (`error.type` `invalid_request_error`); the rest name a
 * provider-side refusal however it arrives.
 */
const PUBLIC_MODEL_REFUSALS: ReadonlySet<string> = new Set([
  IMAGE_INPUT_UNAVAILABLE,
  CONTEXT_BUDGET_EXCEEDED,
]);

/** One JSON gateway error's own `error` object, when it has one. */
function gatewayError(
  body: unknown,
): { code?: unknown; type?: unknown } | null {
  if (typeof body !== "object" || body === null || !("error" in body))
    return null;
  const failure: unknown = body.error;
  if (typeof failure !== "object" || failure === null || Array.isArray(failure))
    return null;
  return {
    code: "code" in failure ? failure.code : undefined,
    type: "type" in failure ? failure.type : undefined,
  };
}

/** The refusal code of a JSON gateway error, when it is an allowlisted one. */
async function publicRefusal(response: Response): Promise<string | null> {
  if (response.ok || !response.headers.get("content-type")?.includes("json"))
    return null;
  try {
    const failure = gatewayError(await response.clone().json());
    const code = failure?.code;
    if (typeof code !== "string" || !PUBLIC_MODEL_REFUSALS.has(code))
      return null;
    // The context budget is the gateway's own policy refusal, not a provider's:
    // its exact status and error type keep a look-alike code from a provider
    // private. Any other status (such as the 512 KiB read limit) is a different
    // refusal, so it is left untouched and stays generic.
    return code === CONTEXT_BUDGET_EXCEEDED &&
      (response.status !== 400 || failure?.type !== "invalid_request_error")
      ? null
      : code;
  } catch {
    return null;
  }
}

/**
 * Carry trusted refusals into the engine without forwarding gateway prose.
 * Budget refusals need a task-bound proof; absent evidence remains generic.
 */
function withRefusalCode(
  response: Response,
  code: string | null,
  budgetMarker?: string,
): Response {
  if (code === null) return response;
  return Response.json(
    {
      error: {
        code,
        message:
          code === CONTEXT_BUDGET_EXCEEDED
            ? (budgetMarker ?? "The model request failed.")
            : code,
      },
    },
    { status: response.status },
  );
}

function hasImage(content: unknown): boolean {
  return (
    Array.isArray(content) &&
    content.some(
      (part: unknown) =>
        typeof part === "object" &&
        part !== null &&
        "type" in part &&
        part.type === "image_url",
    )
  );
}

/**
 * Whether a refused request can go without its images: only when they are all
 * older history. A screenshot the model was just handed stays a visible
 * refusal, because answering without it would read as having seen it.
 */
function imagesOnlyInHistory(input: Record<string, unknown>): boolean {
  if (!Array.isArray(input.messages)) return false;
  const messages = input.messages as { content?: unknown }[];
  return (
    !hasImage(messages.at(-1)?.content) &&
    messages.some((message) => hasImage(message?.content))
  );
}

/**
 * CAD-2480: the text bytes of each generation's latest request, so the reply
 * can carry them into the engine's context estimate (`withByteFloor`). The
 * engine decides blocking compaction from tokens only; this is the one seam
 * that gives it a byte rule without a second compaction path. In memory: a
 * restart only skips one floor, and the token rule still applies.
 */
export class RequestSizes {
  private readonly bytes = new Map<string, number>();

  record(task: string, bytes: number): void {
    this.bytes.delete(task);
    this.bytes.set(task, bytes);
    // A task that never answered (aborted) must not hold an entry forever.
    if (this.bytes.size > 32) this.bytes.delete(this.bytes.keys().next().value!);
  }

  /**
   * Raise a finished reply's reported context tokens to the engine's blocking
   * threshold when the next request, this request plus the reply as it will be
   * sent back, passes `BYTE_COMPACTION_THRESHOLD`. Only raises, and only a
   * reply that answered.
   */
  withByteFloor(task: string, message: AssistantMessage): void {
    const sent = this.bytes.get(task);
    this.bytes.delete(task);
    if (sent === undefined || message.stopReason === "error" || message.stopReason === "aborted")
      return;
    if (sent + replyBytes(message) <= BYTE_COMPACTION_THRESHOLD) return;
    const blocking = MODEL_CONTEXT_WINDOW - HOSTED_COMPACTION_POLICY.reserveTokens + 1;
    const usage = message.usage as { totalTokens: number };
    usage.totalTokens = Math.max(usage.totalTokens, blocking);
  }
}

/** The reply's bytes in the next request: text, and each call's arguments as the JSON string they are sent as. */
function replyBytes(message: AssistantMessage): number {
  let text = "";
  for (const part of message.content) {
    if (part.type === "text") text += part.text;
    else if (part.type === "toolCall")
      text += JSON.stringify({
        id: part.id,
        type: "function",
        function: { name: part.name, arguments: JSON.stringify(part.arguments) },
      });
  }
  return encoder.encode(text).length;
}

/**
 * The private model endpoint.
 *
 * `identity` resolves the calling engine task's durable checkpoint and must
 * fail closed when absent. `alias` is deliberately not passed: the engine's
 * transport hands over the model id its session stores, and a conversation
 * created before the platform default still stores its old pinned id. The
 * runtime never selects a provider model — the application's proxy resolves
 * the alias.
 */
export function meteredBinding(
  transport: MeteredTransport,
  identity: (signal: AbortSignal | undefined) => GenerationIdentity | null,
  onModelResolved?: (model: string) => void,
  maxImagesPerRequest = 1,
  budgetProofs?: Pick<ContextBudgetProofs, "clear" | "issue">,
  isCompaction?: (signal: AbortSignal | undefined) => boolean,
  sizes?: RequestSizes,
): MeteredBinding {
  if (
    !Number.isSafeInteger(maxImagesPerRequest) ||
    maxImagesPerRequest < 1 ||
    maxImagesPerRequest > MAX_IMAGE_PARTS
  )
    throw new Error("Invalid model image limit.");
  return {
    async run(_model, input, options) {
      const current = identity(options?.signal);
      if (!current) {
        return Response.json(
          {
            success: false,
            error: {
              code: "funding_identity_unavailable",
              message: "The durable generation identity is unavailable.",
            },
          },
          { status: 409 },
        );
      }
      const compacting = isCompaction?.(options?.signal) === true;
      const summary = compacting ? withSummaryBudget(splitSummaryMessages(withSummaryInstruction(input))) : input;
      const shaped = boundImageHistory(boundToolHistory(summary), maxImagesPerRequest);
      const bounded = compacting ? shaped : boundActiveToolResults(shaped);
      if (compacting && requestBytes(bounded) > SUMMARY_BODY_BYTES) {
        // A refused summary is terminal, and it must not leave an earlier
        // attempt's evidence behind: nothing was dispatched under it.
        budgetProofs?.clear(current);
        return Response.json(
          {
            error: {
              code: CONTEXT_BUDGET_EXCEEDED,
              message: CONTEXT_BUDGET_EXCEEDED_TEXT,
            },
          },
          { status: 400 },
        );
      }
      const request = (value: Record<string, unknown>) => {
        // Recovery can repeat this task after a crash before the error entry
        // was committed. Never let evidence from the earlier attempt survive.
        budgetProofs?.clear(current);
        if (!compacting) sizes?.record(current.task, textBytes(value));
        return transport.requestModel(value, current, options?.signal);
      };
      let response = await request(bounded);
      let refusal = await publicRefusal(response);
      // A stored screenshot would otherwise be refused on every later turn,
      // leaving a conversation that can only be reset. History is untouched.
      if (refusal === IMAGE_INPUT_UNAVAILABLE && imagesOnlyInHistory(bounded)) {
        response = await request(
          boundImageHistory(bounded, 0, IMAGE_REFUSED_NOTICE),
        );
        refusal = await publicRefusal(response);
      }
      const marker =
        refusal === CONTEXT_BUDGET_EXCEEDED
          ? budgetProofs?.issue(current)
          : undefined;
      response = withRefusalCode(response, refusal, marker);
      const metered = onModelResolved ? captureResolvedModel(response, onModelResolved) : response;
      if (options?.returnRawResponse !== false) return metered;
      return metered.json();
    },
    async fetch(input, init) {
      const url = input instanceof Request ? input.url : new URL(String(input)).href;
      const signal = input instanceof Request ? input.signal : init?.signal;
      if (!transport.resumeModel) {
        return Response.json(
          {
            success: false,
            error: {
              code: "resume_unavailable",
              message: "This runtime cannot resume a model response.",
            },
          },
          { status: 409 },
        );
      }
      return transport.resumeModel(url, signal ?? undefined);
    },
    aiGatewayLogId: null,
  };
}
