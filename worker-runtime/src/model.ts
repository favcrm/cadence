import type { Api, Model, Provider } from "@earendil-works/pi-ai";
import { CLOUDFLARE_AI_API, CLOUDFLARE_PROVIDER_ID } from "agents/models/pi-ai";
import type { GenerationIdentity } from "./contract.ts";
import { MAX_IMAGE_BASE64_CHARS, MAX_IMAGE_PARTS } from "./image-limits.ts";

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
const MODEL_CONTEXT_WINDOW = 128_000;
const MODEL_MAX_OUTPUT_TOKENS = 8192;

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
 * Retain the newest image parts that fit one gateway request. Durable session
 * history is untouched; each omitted image is named in the model input.
 */
function boundImageHistory(input: Record<string, unknown>): Record<string, unknown> {
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
      if (count >= MAX_IMAGE_PARTS || bytes + size > MAX_IMAGE_BASE64_CHARS) {
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
            ? {
                type: "text",
                text: "[An image was omitted from this request to fit the image limit.]",
              }
            : part,
        ),
      };
    }),
  };
}

/**
 * The private model endpoint.
 *
 * `identity` reads the engine's current durable generation checkpoint and must
 * fail closed when absent. `alias` is deliberately not passed: the engine's
 * transport hands over the model id its session stores, and a conversation
 * created before the platform default still stores its old pinned id. The
 * runtime never selects a provider model — the application's proxy resolves
 * the alias.
 */
export function meteredBinding(
  transport: MeteredTransport,
  identity: () => GenerationIdentity | null,
  onModelResolved?: (model: string) => void,
): MeteredBinding {
  return {
    async run(_model, input, options) {
      const current = identity();
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
      const response = await transport.requestModel(
        boundImageHistory(input),
        current,
        options?.signal,
      );
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
