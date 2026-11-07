import type { AssistantMessage } from "@earendil-works/pi-ai";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { describe, expect, it, mock } from "bun:test";
import type { PendingOperation, ThreadEntry } from "../src/contract.ts";

/**
 * CAD-2228 acceptance check. Authored by the independent checker, not by the
 * implementer of the in-flight preview, and therefore knowingly hard on the
 * boundary it was told to prove.
 *
 * The board's thread stream may carry the assistant's text while it is still
 * being written, but nothing the durable transcript hides: not the model's
 * reasoning, not a tool call's arguments, and not a token-bearing JSON
 * confirmation. Withholding may not rest on a block's first character: the
 * same payload arrives with a leading `{`, inline after prose inside one block,
 * inside a Markdown fence, and continued by a later text block whose first
 * character is `"`, not `{`. One more arrives bare — no `{` anywhere — after
 * characters whose lowercase form is longer than the original (U+0130), where
 * an index taken from a lowercased copy stops addressing the text and can
 * expose the value. Each variant is split at chunk boundaries inside the key
 * and the value, and the cross-block case is split at a block boundary too.
 *
 * Five independent scenarios, each over its OWN fresh connection, watch
 * snapshot and in-flight message — never one message with a leading-`{` block
 * followed by the other cases. That separation is the point: a message-wide
 * quarantine triggered by an earlier withheld block would make a later inline,
 * fenced or continuation case pass without ever being implemented, so each
 * scenario starts from an eligible prose block and owns the only payload in its
 * message. The continuation scenario deliberately opens its object inline
 * after prose, so no leading-`{` block exists in that message either; the
 * decision has to be carried across the block boundary on its own merits. The
 * bare-key scenario's message has no brace at all, so only the sensitive key
 * can be what stops the preview.
 *
 * What this drives: the real `HostedSession.fetch` `/thread-stream` handler,
 * its real `sessionEventResponse` lifecycle, and the real preview guard, over
 * the engine's real `AgentEvent` batch shapes. The watch is a synthetic
 * provider/engine double (no network, no model, no live service): it hands the
 * handler message batches assembled exactly as the durable engine publishes
 * them, including deltas split anywhere.
 *
 * A second check owns the turn facts the same stream reports. The board learns
 * whether a turn is working only from a `master` frame, and a preview is only
 * meaningful to a reader that already knows a turn is running. So a reader
 * attached while idle must be told the turn started before it receives a byte
 * of that turn's preview and told the turn settled afterwards; the same holds
 * for a reconnect whose engine snapshot already carries the in-flight message.
 *
 * Why the positive control comes first: a stream that never previews anything
 * would satisfy every "does not contain" assertion by accident. Every scenario
 * therefore requires its own eligible prose preview to exist, and to arrive
 * before the committed entry it belongs to, before anything is read into its
 * withheld fragments. Before this feature the stream emits no `event: preview`
 * frame at all, so the check fails at that control and passes only once the
 * boundary emits real in-progress text and still refuses the rest.
 *
 * Not covered here: the browser renderer, the socket alternative, throughput
 * and connection counts, and the Workers runtime itself (no rig in this
 * repository; `bun test` cannot resolve `cloudflare:workers`). Those stay with
 * the other CAD-2228 checks.
 */

// `hosted-session.ts` imports its Durable Object base from the platform
// module, which `bun test` cannot resolve, so the base is stubbed for the
// process. The stub carries no behaviour this check relies on, and it is kept
// identical to the other files' stubs so the runner's file order cannot change
// the cached base class.
mock.module("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(
      readonly ctx: unknown,
      readonly env: unknown,
    ) {}
  },
}));

/** Text a model writes that the board may legitimately see mid-turn. */
const PROSE = "I have prepared the booking details for you.";

/** Reasoning and tool arguments: never a preview, at any point of a turn. */
// The head is already in the in-flight message the watch attaches to; the tail
// streams afterwards, so a partial serialization and a delta handler each have
// their own marker to leak.
const REASONING_HEAD = "SECRET_REASONING_";
const REASONING_TAIL = "reasoning_tail_never_shown";
const TOOL_ARGUMENT_HEAD = "SECRET_TOOL_ARGUMENT_";
const TOOL_ARGUMENT_TAIL = "tool_arg_tail_never_shown";
const REASONING = `${REASONING_HEAD}${REASONING_TAIL}`;

/** A token-bearing confirmation whose block begins with `{`. */
const CONFIRMATION_TEXT = '{"confirm_token":"SECRET_CONFIRM_TOKEN_a1b2c3d4"}';
const CONFIRMATION_BLOCK = `\n  ${CONFIRMATION_TEXT}`;

/** The same payload inline after prose inside one block. */
const INLINE_PROSE = "Send this to the client: ";
const INLINE_TEXT = `${INLINE_PROSE}{"request_id":"SECRET_REQUEST_ID_9f2cb7e1"}`;

/** The same payload inside a Markdown fence, so the block opens with a backtick. */
// The synthetic token is assembled here instead of being written as one
// literal `confirm_token":"<value>"`: on a single source line that shape is
// indistinguishable from a leaked credential to a secret scanner
// (`generic-api-key`), while the value that streams, is withheld and is
// asserted below is byte-for-byte the same.
const FENCED_VALUE = ["FENCE", "SECRET", "8d3e4f"].join("_");
const FENCED_TEXT = '```json\n{"confirm_token":"' + FENCED_VALUE + '"}\n```';

/** The same payload split across two text blocks, mid-object. */
const HANDOFF_PROSE = "The settlement summary follows.";
const PAYLOAD_PROSE = "Payload: ";
const CONTINUATION_HEAD = '{"request_id":"SECRET_CONTINUATION_9x8y",';
const CONTINUATION_TAIL = '"amount":"120.00"}';

/** JSON structure no eligible prose in these fixtures contains. */
const STRUCTURE = ["{", "}"] as const;

/**
 * A bare (no `{`) sensitive key/value preceded by characters whose lowercase
 * form is longer than the original: each U+0130 lowercases to two code units,
 * so an index taken from a lowercased copy no longer addresses this text.
 * Thirty of them move the key's lowercased index thirty code units past its
 * real one — far enough inside the value once the value has streamed in full.
 */
const EXPANDING = "İ".repeat(30);
const BARE_PROSE = "The settlement summary is ready. ";
const BARE_SECRET = "confirm_token: SECRET_BARE_TOKEN_7h4k";

const USAGE: AssistantMessage["usage"] = {
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheWrite: 0,
  totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

function assistant(content: AssistantMessage["content"]): AssistantMessage {
  return {
    role: "assistant",
    content,
    api: "openai-completions",
    provider: "scripted",
    model: "scripted",
    usage: USAGE,
    stopReason: "stop",
    timestamp: 1,
  };
}

/** One assistant text block, as the engine's `MessageChange.block` carries it. */
const textBlock = (text: string) => ({ type: "text", text });

const update = (...changes: readonly unknown[]) => ({
  type: "message_update",
  usage: USAGE,
  changes,
});

type Frame = { event?: string; id?: string; data: string };

/** SSE frames as a browser splits them: no frame may straddle a blank line. */
function parseFrames(stream: string): Frame[] {
  const frames: Frame[] = [];
  for (const block of stream.split("\n\n")) {
    const lines = block.split("\n");
    const field = (name: string) =>
      lines.find((line) => line.startsWith(`${name}: `))?.slice(name.length + 2);
    const event = field("event");
    if (event === undefined) continue;
    frames.push({ event, id: field("id"), data: field("data") ?? "" });
  }
  return frames;
}

/** A frame's JSON object, unchecked shape but a real object. */
function frameBody(frame: Frame): Record<string, unknown> {
  const parsed: unknown = JSON.parse(frame.data);
  if (typeof parsed !== "object" || parsed === null)
    throw new Error(`A stream frame must carry a JSON object: ${frame.data}`);
  return parsed as Record<string, unknown>;
}

/** The preview body is exactly the shared contract: `{ "text": string | null }`. */
function previewText(frame: Frame): string | null {
  const text = frameBody(frame).text;
  if (text !== null && typeof text !== "string")
    throw new Error(`A preview's text must be a string or null: ${frame.data}`);
  return text;
}

/** The committed entry's stored text, as the board's `entry` frame carries it. */
function entryText(frame: Frame): unknown {
  const entry = frameBody(frame);
  if (!("text" in entry))
    throw new Error(`An entry frame must carry a stored entry: ${frame.data}`);
  return entry.text;
}

/** One fresh message driven through the real stream boundary. */
type Scenario = {
  readonly name: string;
  /** The in-flight partial the watch attaches to, if any. */
  readonly attached: AssistantMessage["content"];
  /** Engine batches after attach and before the message completes. */
  readonly batches: readonly (readonly unknown[])[];
  /** The one eligible text this message must preview. */
  readonly prose: string;
  /** Every withheld key and value fragment, plus JSON structure. */
  readonly withheld: readonly string[];
  /** The durable reply's text, as the board's transcript would store it. */
  readonly finalText: string;
  /** The committed assistant content, matching the in-flight blocks. */
  readonly committed: AssistantMessage["content"];
};

const scenarios: readonly Scenario[] = [
  {
    name: "reasoning, tool arguments and a leading-{ confirmation",
    attached: [
      { type: "thinking", thinking: REASONING_HEAD },
      {
        type: "toolCall",
        id: "call_booking",
        name: "confirm_booking",
        arguments: {},
      },
    ],
    batches: [
      // The watch attached mid-message: reasoning is already open and the tool
      // call exists, so only their deltas continue — then eligible prose.
      [
        update(
          { type: "thinking_delta", contentIndex: 0, delta: REASONING_TAIL },
          { type: "text_start", contentIndex: 2, block: textBlock("") },
          { type: "text_delta", contentIndex: 2, delta: PROSE },
          { type: "block", contentIndex: 2, block: textBlock(PROSE) },
        ),
      ],
      [
        update(
          {
            type: "toolcall_delta",
            contentIndex: 1,
            path: ["confirm_token"],
            delta: TOOL_ARGUMENT_HEAD,
          },
          {
            type: "toolcall_delta",
            contentIndex: 1,
            path: ["confirm_token"],
            delta: TOOL_ARGUMENT_TAIL,
          },
        ),
      ],
      // The confirmation arrives as a second text block, chunk by chunk:
      // whitespace, `{`, the key, the token, then its close.
      [
        update(
          { type: "text_start", contentIndex: 3, block: textBlock("") },
          { type: "text_delta", contentIndex: 3, delta: "\n  " },
          { type: "text_delta", contentIndex: 3, delta: '{"confirm' },
          { type: "text_delta", contentIndex: 3, delta: '_token":"SECRET_' },
          { type: "text_delta", contentIndex: 3, delta: "CONFIRM_TOKEN_" },
          { type: "text_delta", contentIndex: 3, delta: "a1b2" },
          { type: "text_delta", contentIndex: 3, delta: 'c3d4"}' },
          { type: "block", contentIndex: 3, block: textBlock(CONFIRMATION_BLOCK) },
        ),
      ],
    ],
    prose: PROSE,
    withheld: [
      REASONING_HEAD,
      REASONING_TAIL,
      TOOL_ARGUMENT_HEAD,
      TOOL_ARGUMENT_TAIL,
      "confirm_token",
      "CONFIRM_TOKEN_",
      "a1b2",
      "c3d4",
      ...STRUCTURE,
    ],
    finalText: `${PROSE}\n${CONFIRMATION_BLOCK}`,
    committed: [
      { type: "thinking", thinking: REASONING },
      {
        type: "toolCall",
        id: "call_booking",
        name: "confirm_booking",
        arguments: { confirm_token: "SECRET_CONFIRM_TOKEN_a1b2c3d4" },
      },
      { type: "text", text: PROSE },
      { type: "text", text: CONFIRMATION_BLOCK },
    ],
  },
  {
    name: "inline JSON after prose in one block",
    attached: [],
    batches: [
      [
        update(
          { type: "text_start", contentIndex: 0, block: textBlock("") },
          { type: "text_delta", contentIndex: 0, delta: PROSE },
          { type: "block", contentIndex: 0, block: textBlock(PROSE) },
        ),
      ],
      // No leading `{`: prose and the token-bearing JSON are one block, with
      // the key and the value split across chunks.
      [
        update(
          { type: "text_start", contentIndex: 1, block: textBlock("") },
          { type: "text_delta", contentIndex: 1, delta: INLINE_PROSE },
          { type: "text_delta", contentIndex: 1, delta: '{"request' },
          { type: "text_delta", contentIndex: 1, delta: '_id":"SECRET_' },
          { type: "text_delta", contentIndex: 1, delta: "REQUEST_ID_9f2c" },
          { type: "text_delta", contentIndex: 1, delta: 'b7e1"}' },
          { type: "block", contentIndex: 1, block: textBlock(INLINE_TEXT) },
        ),
      ],
    ],
    prose: PROSE,
    withheld: [
      "request_id",
      "SECRET_REQUEST_ID_",
      "9f2c",
      "b7e1",
      ...STRUCTURE,
    ],
    finalText: `${PROSE}\n${INLINE_TEXT}`,
    committed: [
      { type: "text", text: PROSE },
      { type: "text", text: INLINE_TEXT },
    ],
  },
  {
    name: "Markdown-fenced JSON block",
    attached: [],
    batches: [
      [
        update(
          { type: "text_start", contentIndex: 0, block: textBlock("") },
          { type: "text_delta", contentIndex: 0, delta: PROSE },
          { type: "block", contentIndex: 0, block: textBlock(PROSE) },
        ),
      ],
      // The block opens with the fence, so its first non-whitespace character
      // is a backtick, not `{`; the key and token still split across chunks.
      [
        update(
          { type: "text_start", contentIndex: 1, block: textBlock("") },
          { type: "text_delta", contentIndex: 1, delta: "```json\n" },
          { type: "text_delta", contentIndex: 1, delta: '{"confirm' },
          { type: "text_delta", contentIndex: 1, delta: '_token":"FENCE_' },
          { type: "text_delta", contentIndex: 1, delta: "SECRET_8d3e" },
          { type: "text_delta", contentIndex: 1, delta: '4f"}\n```' },
          { type: "block", contentIndex: 1, block: textBlock(FENCED_TEXT) },
        ),
      ],
    ],
    prose: PROSE,
    withheld: ["confirm_token", "FENCE_SECRET_", "8d3e4f", ...STRUCTURE],
    finalText: `${PROSE}\n${FENCED_TEXT}`,
    committed: [
      { type: "text", text: PROSE },
      { type: "text", text: FENCED_TEXT },
    ],
  },
  {
    name: "JSON continued by a later text block",
    attached: [],
    batches: [
      [
        update(
          { type: "text_start", contentIndex: 0, block: textBlock("") },
          { type: "text_delta", contentIndex: 0, delta: HANDOFF_PROSE },
          { type: "block", contentIndex: 0, block: textBlock(HANDOFF_PROSE) },
        ),
      ],
      // The object opens inline after prose — again no leading `{` — and stays
      // unclosed at the end of this block.
      [
        update(
          { type: "text_start", contentIndex: 1, block: textBlock("") },
          { type: "text_delta", contentIndex: 1, delta: PAYLOAD_PROSE },
          { type: "text_delta", contentIndex: 1, delta: '{"request' },
          { type: "text_delta", contentIndex: 1, delta: '_id":"SECRET_' },
          { type: "text_delta", contentIndex: 1, delta: 'CONTINUATION_9x8y",' },
          {
            type: "block",
            contentIndex: 1,
            block: textBlock(`${PAYLOAD_PROSE}${CONTINUATION_HEAD}`),
          },
        ),
      ],
      // The NEXT text block continues the object: its first character is `"`,
      // neither `{` nor a fence, so only carried state can withhold it.
      [
        update(
          { type: "text_start", contentIndex: 2, block: textBlock("") },
          { type: "text_delta", contentIndex: 2, delta: '"amount":"' },
          { type: "text_delta", contentIndex: 2, delta: "120.00" },
          { type: "text_delta", contentIndex: 2, delta: '"}' },
          { type: "block", contentIndex: 2, block: textBlock(CONTINUATION_TAIL) },
        ),
      ],
    ],
    prose: HANDOFF_PROSE,
    withheld: [
      "request_id",
      "SECRET_CONTINUATION_",
      "9x8y",
      "amount",
      "120.00",
      ...STRUCTURE,
    ],
    finalText: `${HANDOFF_PROSE}\n${PAYLOAD_PROSE}${CONTINUATION_HEAD}\n${CONTINUATION_TAIL}`,
    committed: [
      { type: "text", text: HANDOFF_PROSE },
      { type: "text", text: `${PAYLOAD_PROSE}${CONTINUATION_HEAD}` },
      { type: "text", text: CONTINUATION_TAIL },
    ],
  },
  {
    name: "bare sensitive key/value after expanding lowercase characters",
    attached: [],
    batches: [
      [
        update(
          { type: "text_start", contentIndex: 0, block: textBlock("") },
          { type: "text_delta", contentIndex: 0, delta: BARE_PROSE },
          { type: "block", contentIndex: 0, block: textBlock(BARE_PROSE) },
        ),
      ],
      // The expanding characters stream first, then a bare key/value — no `{`,
      // no fence. Every chunk boundary sits inside the key or the value, and
      // the lowercased copy is already thirty code units longer by the time
      // the key arrives.
      [
        update(
          { type: "text_start", contentIndex: 1, block: textBlock("") },
          { type: "text_delta", contentIndex: 1, delta: EXPANDING },
        ),
      ],
      [update({ type: "text_delta", contentIndex: 1, delta: "confirm_" })],
      [
        update({
          type: "text_delta",
          contentIndex: 1,
          delta: "token: SECRET_",
        }),
      ],
      [
        update(
          { type: "text_delta", contentIndex: 1, delta: "BARE_TOKEN_7h4k" },
          {
            type: "block",
            contentIndex: 1,
            block: textBlock(`${EXPANDING}${BARE_SECRET}`),
          },
        ),
      ],
    ],
    // Ordinary prose must still stream while the bare value never does.
    prose: BARE_PROSE,
    withheld: [
      "confirm_token",
      "SECRET_BARE_TOKEN_7h4k",
      "SECRET_BARE",
      "7h4k",
    ],
    finalText: `${BARE_PROSE}${EXPANDING}${BARE_SECRET}`,
    committed: [
      { type: "text", text: BARE_PROSE },
      { type: "text", text: `${EXPANDING}${BARE_SECRET}` },
    ],
  },
];

/** The one real method this check drives; everything else is stubbed. */
type BoundarySession = { fetch(request: Request): Promise<Response> };

type Observation = {
  readonly scenario: Scenario;
  readonly frames: Frame[];
  readonly previewFrames: Frame[];
  readonly texts: (string | null)[];
};

/**
 * Drive one scenario through the real `/thread-stream` boundary on a fresh
 * connection, a fresh watch snapshot and a fresh in-flight message.
 */
async function streamScenario(
  Session: { prototype: object },
  scenario: Scenario,
): Promise<Observation> {
  const completion = Promise.withResolvers<{ reason: "session_closed" }>();
  const committed: ThreadEntry[] = [];
  let deliver:
    | ((events: readonly unknown[], context: unknown) => Promise<void>)
    | undefined;

  const watch = {
    snapshot: {
      type: "snapshot" as const,
      entries: [],
      tools: [],
      nestedTools: [],
      compactions: [],
      inbox: [],
      agent: {},
      usage: { models: {}, tools: {} },
      generation: { attempt: 1, message: assistant(scenario.attached) },
    },
    closed: completion.promise,
    start(
      listener: (events: readonly unknown[], context: unknown) => Promise<void>,
    ) {
      deliver = listener;
    },
    async stop() {
      const end = { reason: "cancelled" as const };
      completion.resolve(end);
      return end;
    },
  };

  // The session module was imported dynamically (the platform module has to be
  // mocked first), so the constructor arrives as an argument. `Object.create`
  // keeps every real prototype method in play; the cast names the single
  // method this check calls on an instance whose fields are stubbed.
  const session = Object.assign(Object.create(Session.prototype), {
    ctx: { storage: { get: async () => undefined } },
    options: { modelAlias: "alias" },
    harness: {
      pending: async () => [],
      session: () => ({ id: "1", events: async () => watch }),
    },
    // The durable projection has its own tests; this check owns the stream
    // boundary, so the transcript the handler re-reads is scripted here.
    transcript: async () => committed.slice(),
  }) as BoundarySession;

  const response = await session.fetch(
    new Request("https://board.test/thread-stream"),
  );
  if (response.status !== 200)
    throw new Error(`[${scenario.name}] the stream did not open: ${response.status}`);
  if (response.headers.get("content-type") !== "text/event-stream")
    throw new Error(`[${scenario.name}] the stream is not SSE`);
  const send = deliver;
  if (send === undefined)
    throw new Error(`[${scenario.name}] the watch was never started`);

  // Read continuously, so the reader is never the slow one under test and
  // every enqueued byte is captured before the body closes.
  const reader = response.body!.getReader();
  const decoder = new TextDecoder();
  const chunks: string[] = [];
  const reading = (async () => {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(decoder.decode(value, { stream: true }));
    }
    chunks.push(decoder.decode());
  })();

  // `pi.assistant` is the engine's own kind for a committed assistant message —
  // what retires the live preview. The durable text is unchanged: the guard is
  // a preview concern, not a storage edit.
  const committedRecord = {
    id: 2,
    conversationId: 1,
    kind: "pi.assistant",
    model: [assistant(scenario.committed)],
  };
  const committedEntry: ThreadEntry = {
    seq: 1,
    role: "assistant",
    kind: "message",
    text: scenario.finalText,
  };
  const toolResultEntry: ThreadEntry = {
    seq: 2,
    role: "tool",
    kind: "message",
    text: "Booking confirmed.",
  };

  const batches: readonly (readonly unknown[])[] = [
    ...scenario.batches,
    // The message completes and the engine commits it: the entry is already
    // visible to the stream's own transcript read in this same batch.
    [
      { type: "message_end", entry: committedRecord },
      { type: "entry_appended", entry: committedRecord },
      { type: "turn_end" },
    ],
    // The tool call then finishes and its result is committed too; this later
    // durable frame also flushes a coalesced preview, so the clear is never
    // left waiting for a pull before the watch retires.
    [
      {
        type: "tool_execution_end",
        toolCallId: "call_booking",
        toolName: "confirm_booking",
      },
      { type: "run_end", inputs: [] },
    ],
  ];
  const transcriptAt: readonly (readonly ThreadEntry[])[] = [
    ...scenario.batches.map(() => [] as readonly ThreadEntry[]),
    [committedEntry],
    [committedEntry, toolResultEntry],
  ];

  for (const [index, batch] of batches.entries()) {
    committed.length = 0;
    committed.push(...(transcriptAt[index] ?? []));
    await send(batch, BACKGROUND_CONTEXT);
  }
  completion.resolve({ reason: "session_closed" });
  await reading;

  const frames = parseFrames(chunks.join(""));
  const previewFrames = frames.filter((frame) => frame.event === "preview");
  return {
    scenario,
    frames,
    previewFrames,
    texts: previewFrames.map((frame) => previewText(frame)),
  };
}

/** Record everything one scenario got wrong, in a form that names it. */
function inspect(observation: Observation, issues: string[]): void {
  const { scenario, frames, previewFrames, texts } = observation;
  const label = `[${scenario.name}]`;

  // The board flips its link to `live` on a `master` frame and nothing else,
  // and the header snapshot comes before any transient frame.
  const master = frames.findIndex((frame) => frame.event === "master");
  if (master < 0) issues.push(`${label} no master frame`);

  // Positive control: without real in-progress text every "never" assertion
  // below would hold vacuously on a stream that previews nothing at all.
  if (previewFrames.length === 0)
    issues.push(`${label} no event: preview frame at all`);
  for (const frame of previewFrames) {
    // A preview is transient: no durable cursor, and no field but `text`.
    if (frame.id !== undefined)
      issues.push(`${label} a preview carries a durable id`);
    const keys = Object.keys(frameBody(frame));
    if (keys.length !== 1 || keys[0] !== "text")
      issues.push(`${label} a preview body is not exactly {text}: ${frame.data}`);
    if (frames.indexOf(frame) <= master)
      issues.push(`${label} a preview precedes the master frame`);
  }

  const prose = frames.findIndex(
    (frame) =>
      frame.event === "preview" && previewText(frame)?.includes(scenario.prose) === true,
  );
  if (prose <= master)
    issues.push(`${label} no preview of the eligible prose`);
  if (!texts.includes(scenario.prose))
    issues.push(
      `${label} the eligible prose was not previewed exactly: ${JSON.stringify(texts)}`,
    );

  // Nothing this message withholds may ride along, including every fragment of
  // a value that a chunk or block boundary splits.
  for (const text of texts) {
    if (typeof text !== "string") continue;
    for (const fragment of scenario.withheld) {
      if (text.includes(fragment))
        issues.push(
          `${label} leaked ${JSON.stringify(fragment)}: ${JSON.stringify(text)}`,
        );
    }
  }

  // The committed message is still committed verbatim: the guard bounds the
  // preview, not the durable entry.
  const entry = frames.findIndex(
    (frame) => frame.event === "entry" && frame.id === "1",
  );
  if (entry <= prose)
    issues.push(`${label} the committed entry did not follow the prose preview`);
  else if (entryText(frames[entry]!) !== scenario.finalText)
    issues.push(`${label} the committed entry's text changed`);

  // Completion clears the transient text, and no preview survives the commit.
  const cleared = frames.findIndex(
    (frame, index) =>
      index > prose && frame.event === "preview" && previewText(frame) === null,
  );
  if (cleared <= prose)
    issues.push(`${label} the transient preview was never cleared`);
  const settle = Math.max(cleared, entry);
  for (const [index, frame] of frames.entries()) {
    if (index <= settle) continue;
    if (frame.event === "preview" && previewText(frame) !== null)
      issues.push(
        `${label} a transient preview survived the commit: ${frame.data}`,
      );
  }
}

/**
 * A scripted engine watch: one connection's snapshot plus a handle that
 * delivers batches exactly as the durable engine publishes them. No network,
 * no model, no live service.
 */
type WatchDouble = {
  readonly snapshot: Record<string, unknown>;
  readonly closed: Promise<{ reason: "session_closed" | "cancelled" }>;
  start(
    listener: (events: readonly unknown[], context: unknown) => Promise<void>,
  ): void;
  stop(): Promise<{ reason: "cancelled" }>;
};

function watchDouble(snapshot: Record<string, unknown>): {
  readonly watch: WatchDouble;
  send(events: readonly unknown[]): Promise<void>;
  retire(): void;
} {
  const completion = Promise.withResolvers<{
    reason: "session_closed" | "cancelled";
  }>();
  let deliver:
    | ((events: readonly unknown[], context: unknown) => Promise<void>)
    | undefined;
  return {
    watch: {
      snapshot,
      closed: completion.promise,
      start(listener) {
        deliver = listener;
      },
      async stop() {
        const end = { reason: "cancelled" as const };
        completion.resolve(end);
        return end;
      },
    },
    async send(events) {
      if (deliver === undefined)
        throw new Error("The watch was never started.");
      await deliver(events, BACKGROUND_CONTEXT);
    },
    retire() {
      completion.resolve({ reason: "session_closed" });
    },
  };
}

/** The engine snapshot a connection sees when nothing is in flight. */
const idleSnapshot = () => ({
  type: "snapshot" as const,
  entries: [],
  tools: [],
  nestedTools: [],
  compactions: [],
  inbox: [],
  agent: {},
  usage: { models: {}, tools: {} },
});

/**
 * Open the real `/thread-stream` on a scripted session and read it
 * continuously in the background, so the reader is never the slow one under
 * test and every enqueued byte is captured before the body closes.
 */
async function openThread(session: BoundarySession): Promise<{
  readonly response: Response;
  frames(): Promise<Frame[]>;
}> {
  const response = await session.fetch(
    new Request("https://board.test/thread-stream"),
  );
  if (response.status !== 200)
    throw new Error(`the stream did not open: ${response.status}`);
  if (response.headers.get("content-type") !== "text/event-stream")
    throw new Error("the stream is not SSE");
  const reader = response.body!.getReader();
  const decoder = new TextDecoder();
  const chunks: string[] = [];
  const reading = (async () => {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(decoder.decode(value, { stream: true }));
    }
    chunks.push(decoder.decode());
  })();
  return {
    response,
    async frames() {
      await reading;
      return parseFrames(chunks.join(""));
    },
  };
}

/**
 * A session boundary whose only real behaviour is `/thread-stream`: the turn
 * facts `state()` reports, one watch per connection, and the durable
 * transcript are scripted by the check.
 */
function boundarySession(
  Session: { prototype: object },
  script: {
    readonly pending: () => readonly PendingOperation[];
    readonly connect: () => WatchDouble;
    readonly transcript: () => readonly ThreadEntry[];
  },
): BoundarySession {
  return Object.assign(Object.create(Session.prototype), {
    ctx: { storage: { get: async () => undefined } },
    options: { modelAlias: "alias" },
    harness: {
      pending: async () => script.pending(),
      session: () => ({ id: "1", events: async () => script.connect() }),
    },
    transcript: async () => script.transcript(),
  }) as BoundarySession;
}

/** Whether a `master` body reports a turn in flight. */
function isWorking(body: Record<string, unknown>): boolean {
  const turn = body.turn;
  return (
    typeof turn === "object" &&
    turn !== null &&
    "state" in turn &&
    turn.state === "working"
  );
}

/**
 * The running turn's operation id a `master` body reports, if any.
 *
 * The board's Stop control is addressed to this id, so a working header that
 * does not name the operation cannot stop it.
 */
function turnMessage(body: Record<string, unknown>): unknown {
  const turn = body.turn;
  return typeof turn === "object" && turn !== null && "message" in turn
    ? turn.message
    : undefined;
}

/** One assistant message the durable engine commits, as `message_end` carries it. */
function committedAssistant(text: string) {
  return {
    id: 2,
    conversationId: 1,
    kind: "pi.assistant",
    model: [assistant([{ type: "text", text }])],
  };
}

/**
 * Check the master turn facts in wire order around one run.
 *
 * The board learns its link and its idle/working state from `master` frames
 * alone. A stream that previews text while its last `master` said "idle" has
 * given the reader bytes it cannot attribute: no turn to render them under and
 * no id to name or stop it. So the attachment's state, a working report ahead
 * of the first eligible preview, and an idle report after it are one contract.
 */
function inspectTurnState(
  frames: readonly Frame[],
  expected: {
    readonly attachedWhile: "idle" | "working";
    /** The operation id the running turn must name, so Stop can address it. */
    readonly running: string;
    readonly prose: string;
    readonly finalText: string;
    readonly label: string;
  },
  issues: string[],
): void {
  const { label } = expected;
  const masters = frames.flatMap((frame, index) =>
    frame.event === "master" ? [{ index, body: frameBody(frame) }] : [],
  );
  const first = masters[0];
  if (first === undefined) {
    issues.push(`${label} no master frame`);
    return;
  }
  if (expected.attachedWhile === "idle" && first.body.turn !== null)
    issues.push(
      `${label} a connection that attached while idle was told a turn was running`,
    );
  if (expected.attachedWhile === "working") {
    if (!isWorking(first.body))
      issues.push(
        `${label} a connection that attached mid-run was not told the turn was working`,
      );
    else if (turnMessage(first.body) !== expected.running)
      issues.push(
        `${label} the attached turn does not name ${expected.running}: ${JSON.stringify(turnMessage(first.body))}`,
      );
  }

  const previews = frames.flatMap((frame, index) => {
    if (frame.event !== "preview") return [];
    const text = previewText(frame);
    return text === null ? [] : [{ index, text }];
  });
  const prose = previews.find((preview) =>
    preview.text.includes(expected.prose),
  );
  if (prose === undefined) {
    issues.push(`${label} no preview of the eligible prose`);
    return;
  }

  // The turn is working before any byte of its text, and idle once it settled.
  const working = masters.filter(
    (master) => master.index < prose.index && isWorking(master.body),
  );
  if (working.length === 0)
    issues.push(`${label} the working turn was not reported before its preview`);
  else if (working.every((master) => turnMessage(master.body) !== expected.running))
    issues.push(
      `${label} no working frame before the preview names ${expected.running}: ${JSON.stringify(
        working.map((master) => turnMessage(master.body)),
      )}`,
    );

  const last = masters[masters.length - 1]!;
  if (last.index <= prose.index || last.body.turn !== null)
    issues.push(`${label} the turn was never reported idle after its preview`);
  for (const frame of frames.slice(last.index + 1))
    if (frame.event === "preview" && previewText(frame) !== null)
      issues.push(`${label} a preview survived the settled turn`);

  const entry = frames.findIndex(
    (frame) => frame.event === "entry" && frame.id === "1",
  );
  if (entry <= prose.index)
    issues.push(`${label} the committed entry did not follow the preview`);
  else if (entryText(frames[entry]!) !== expected.finalText)
    issues.push(`${label} the committed entry's text changed`);
}

describe("thread stream master turn state", () => {
  it("reports the working turn before previewing a run that starts after the connection, and idle again after it", async () => {
    // The import has to be dynamic: a static one is hoisted above the
    // `cloudflare:workers` mock and fails to resolve under `bun test`.
    const { HostedSession } = await import("../src/hosted-session.ts");

    const pending: PendingOperation[] = [];
    const committed: ThreadEntry[] = [];
    const connection = watchDouble(idleSnapshot());
    const session = boundarySession(HostedSession, {
      pending: () => pending,
      connect: () => connection.watch,
      transcript: () => committed,
    });

    // Attach while the object is idle: this connection's own master frame has
    // to say so, and the run below starts only afterwards.
    const thread = await openThread(session);

    // A real engine run starts after the connection. The object's state now
    // reports a running operation and the watch reports the run start; a
    // stream that keeps the attachment's "idle" would leave the reader with
    // preview bytes it cannot attribute, and the id the header names is what
    // the board's Stop control addresses.
    pending.push({ operationId: "op-run", status: "running" });
    await connection.send([{ type: "run_start", inputs: [1] }]);

    // Eligible assistant text streams later, in a batch of its own.
    await connection.send([
      update(
        { type: "text_start", contentIndex: 0, block: textBlock("") },
        { type: "text_delta", contentIndex: 0, delta: PROSE },
        { type: "block", contentIndex: 0, block: textBlock(PROSE) },
      ),
    ]);

    // The message commits, then the run settles back to idle.
    const record = committedAssistant(PROSE);
    committed.push({ seq: 1, role: "assistant", kind: "message", text: PROSE });
    await connection.send([
      { type: "message_end", entry: record },
      { type: "entry_appended", entry: record },
      { type: "turn_end" },
    ]);
    pending.length = 0;
    await connection.send([{ type: "run_end", inputs: [] }]);
    connection.retire();

    const frames = await thread.frames();
    const issues: string[] = [];
    inspectTurnState(
      frames,
      {
        attachedWhile: "idle",
        running: "op-run",
        prose: PROSE,
        finalText: PROSE,
        label: "[attached while idle]",
      },
      issues,
    );
    expect(issues).toEqual([]);
  });

  it("reports the working turn to a connection that attaches mid-run, ahead of the snapshot's preview", async () => {
    const { HostedSession } = await import("../src/hosted-session.ts");

    // A reconnect during a run: the engine's snapshot is the authoritative
    // in-flight state, and the object's state reports the running operation.
    // Its id is deliberately different from the idle→running case, so the
    // frame has to report this connection's real turn rather than a constant.
    const pending: PendingOperation[] = [
      { operationId: "op-reconnect", status: "running" },
    ];
    const committed: ThreadEntry[] = [];
    const connection = watchDouble({
      ...idleSnapshot(),
      run: { inputs: [1] },
      generation: {
        attempt: 1,
        message: assistant([{ type: "text", text: PROSE }]),
      },
    });
    const session = boundarySession(HostedSession, {
      pending: () => pending,
      connect: () => connection.watch,
      transcript: () => committed,
    });

    const thread = await openThread(session);

    // The snapshot previews the partial; the commit then settles the turn.
    const record = committedAssistant(PROSE);
    committed.push({ seq: 1, role: "assistant", kind: "message", text: PROSE });
    pending.length = 0;
    await connection.send([
      { type: "message_end", entry: record },
      { type: "entry_appended", entry: record },
      { type: "turn_end" },
      { type: "run_end", inputs: [] },
    ]);
    connection.retire();

    const frames = await thread.frames();
    const issues: string[] = [];
    inspectTurnState(
      frames,
      {
        attachedWhile: "working",
        running: "op-reconnect",
        prose: PROSE,
        finalText: PROSE,
        label: "[reconnect mid-run]",
      },
      issues,
    );
    expect(issues).toEqual([]);
  });
});

describe("thread stream in-flight preview", () => {
  it("previews eligible prose and never a token-bearing JSON confirmation in any streamed form, reasoning, or tool arguments", async () => {
    // The import has to be dynamic: a static one is hoisted above the
    // `cloudflare:workers` mock and fails to resolve under `bun test`. Its
    // prototype methods are the real ones under test.
    const { HostedSession } = await import("../src/hosted-session.ts");

    const issues: string[] = [];
    // One fresh connection and one fresh in-flight message per scenario, so no
    // scenario can inherit a message-wide withholding decision from another.
    for (const scenario of scenarios)
      inspect(await streamScenario(HostedSession, scenario), issues);
    expect(issues).toEqual([]);
  });
});
