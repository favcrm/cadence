/**
 * CAD-2744: keep tool error text out of a compaction summary in code.
 *
 * CAD-2658 and CAD-2729 asked the summarizer not to quote tool errors. A
 * hosted model ignored that request and quoted the tool's error verbatim, so
 * the summary source is shaped before the model sees it, whatever the model.
 *
 * The engine serializes the summarized turns as `[User]: `, `[Assistant]: `,
 * `[Assistant tool calls]: ` and `[Tool result]: ` segments, and renders a
 * tool's diagnostics at the end of its result as
 * `<harness>\n[<severity>] <message>\n</harness>`: thrown errors, refusals
 * and loop-guard blocks all arrive this way. Here:
 *
 * - each `[error]` diagnostic in a tool result becomes a fixed line, so the
 *   summary still records that the call failed;
 * - an earlier summary (a user segment the engine wrote) sheds a quoted
 *   `<harness>[error] …</harness>` the same way;
 * - an assistant or earlier-summary echo of a removed message, or of its
 *   identifier-like code, is replaced;
 * - other user text is never changed: an error the user reports is a fact
 *   about their business.
 *
 * The engine's format is plain text, so a message that itself contains
 * "\n\n[User]: " or a harness block can shift a segment boundary; the rules
 * above then apply to what the text looks like. Scans stay linear in the
 * text: a summary is shaped on every compaction attempt.
 */

export const TOOL_ERROR_REDACTED = "the call failed; its error text is left out of this summary";
export const TOOL_ERROR_TEXT_OMITTED = "(tool error text omitted)";
export const TOOL_ERROR_CODE_OMITTED = "(tool error code omitted)";

/** How the engine introduces an earlier summary in the summarized turns. */
export const SUMMARY_PREFIX = "The conversation history before this point was compacted into the following summary:";

/** A message shorter than this is common words ("timeout"), not worth chasing outside its harness line. */
const MIN_ECHO_CHARS = 16;
/** A leading error code worth chasing: an identifier with an underscore or a digit, at least 6 characters. */
const ERROR_CODE = /^[A-Za-z][A-Za-z0-9_.-]{5,}$/;

const ROLE = String.raw`\[(?:User|Assistant|Assistant tool calls|Tool result)\]: `;
const SEGMENT_START = new RegExp(String.raw`(?:^|\n\n|<conversation>\n)(?=${ROLE})`, "g");
export const CONVERSATION_END = "\n</conversation>";

const HARNESS_OPEN = "<harness>";
const HARNESS_CLOSE = "</harness>";
/**
 * The engine cuts a tool result to 2,000 characters, so a longer block is not
 * one: it is never rewritten, and the search for its end never looks further.
 */
const MAX_BLOCK = 2_000;
/** The engine's diagnostic severities (pi-durable `Diagnostic`). */
const DIAGNOSTIC = /^\[(info|warn|error)\] ?(.*)$/;

export type Segment = { role: string; start: number; end: number };

/** The engine's serialized segments, found by role prefix; the closing tag and prompt are not part of any. */
export function segments(text: string): Segment[] {
  const end = text.lastIndexOf(CONVERSATION_END);
  const limit = end < 0 ? text.length : end;
  const starts: number[] = [];
  for (const match of text.matchAll(SEGMENT_START)) {
    const at = match.index + match[0].length;
    if (at < limit && (starts.length === 0 || starts.at(-1)! < at)) starts.push(at);
  }
  return starts.map((start, index) => {
    const next = starts[index + 1];
    const stop = next === undefined ? limit : next - 2; // the "\n\n" before the next segment
    const role = /^\[([^\]]+)\]: /.exec(text.slice(start))![1]!;
    return { role, start, end: Math.max(start, stop) };
  });
}

/**
 * Each closed `open … close` block of at most MAX_BLOCK characters, passed to
 * `rewrite`. Every search looks at most MAX_BLOCK characters ahead, so a text
 * full of unclosed openings is still scanned in linear time; a block with
 * another opening before its end is skipped, so it cannot swallow later text.
 */
function eachBlock(body: string, open: string, close: string, rewrite: (inner: string) => string): string {
  let out = "";
  let from = 0;
  let at = 0;
  for (;;) {
    const start = body.indexOf(open, at);
    if (start < 0) break;
    const inner = start + open.length;
    const window = body.slice(inner, inner + MAX_BLOCK + close.length);
    const end = window.indexOf(close);
    const nested = window.indexOf(HARNESS_OPEN);
    if (end < 0 || (nested >= 0 && nested < end)) {
      at = inner;
      continue;
    }
    out += body.slice(from, start) + rewrite(window.slice(0, end));
    from = at = inner + end + close.length;
  }
  return out + body.slice(from);
}

/** Rewrite each `[error]` diagnostic of a closed harness block in a tool result; collect what it said. */
function redactHarness(body: string, removed: Set<string>): string {
  return eachBlock(body, `${HARNESS_OPEN}\n`, `\n${HARNESS_CLOSE}`, (inner) => {
    const block = `${HARNESS_OPEN}\n${inner}\n${HARNESS_CLOSE}`;
    const lines = inner.split("\n");
    const out: string[] = [];
    let inError = false;
    let message: string[] = [];
    const flush = () => {
      if (inError) removed.add(message.join("\n").trim());
      message = [];
    };
    for (const line of lines) {
      const diagnostic = DIAGNOSTIC.exec(line);
      if (diagnostic) {
        flush();
        inError = diagnostic[1] === "error";
        if (inError) {
          message = [diagnostic[2]!];
          out.push(`[error] ${TOOL_ERROR_REDACTED}`);
        } else out.push(line);
      } else if (inError) {
        message.push(line); // a multi-line message continues its diagnostic
      } else out.push(line);
    }
    flush();
    const shaped = `<harness>\n${out.join("\n")}\n</harness>`;
    return shaped === block ? block : shaped;
  });
}

/**
 * An earlier summary's quoted harness block, in any layout (often one line,
 * sometimes in backticks): each `[error]` diagnostic in it becomes the fixed
 * line.
 */
function redactQuotedHarness(body: string, removed: Set<string>): string {
  return eachBlock(body, HARNESS_OPEN, HARNESS_CLOSE, (inner) => quotedBlock(inner, removed));
}

function quotedBlock(inner: string, removed: Set<string>): string {
  // ["before", "[warn]", " w ", "[error]", " msg ", …]: split on the tags is linear.
  const parts = inner.split(/(\[(?:info|warn|error)\])/);
  let changed = false;
  for (let index = 1; index < parts.length; index += 2) {
    if (parts[index] !== "[error]") continue;
    const text = parts[index + 1] ?? "";
    const message = text.trim();
    if (message === "" || message === TOOL_ERROR_REDACTED) continue;
    removed.add(message);
    const trailing = text.slice(text.trimEnd().length);
    parts[index + 1] = ` ${TOOL_ERROR_REDACTED}${trailing}`;
    changed = true;
  }
  return `${HARNESS_OPEN}${changed ? parts.join("") : inner}${HARNESS_CLOSE}`;
}

function escape(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Replace echoes of removed messages, longest first, then of their codes. */
function scrubEchoes(body: string, removed: ReadonlySet<string>): string {
  const messages = [...removed].filter((message) => message.length >= MIN_ECHO_CHARS && message !== TOOL_ERROR_REDACTED)
    .sort((a, b) => b.length - a.length);
  let out = body;
  for (const message of messages) out = out.split(message).join(TOOL_ERROR_TEXT_OMITTED);
  const codes = new Set<string>();
  for (const message of removed) {
    const colon = message.indexOf(": ");
    const code = colon < 0 ? "" : message.slice(0, colon);
    if (ERROR_CODE.test(code) && /[_\d]/.test(code)) codes.add(code);
  }
  for (const code of codes)
    out = out.replace(new RegExp(String.raw`(?<![\w-])${escape(code)}(?![\w-])`, "g"), TOOL_ERROR_CODE_OMITTED);
  return out;
}

/** The summary source with tool error text removed; unchanged when there is none. */
export function redactToolErrors(text: string): string {
  if (!text.includes("<harness>")) return text;
  const parts = segments(text);
  if (parts.length === 0) return text;
  const removed = new Set<string>();
  const isEarlierSummary = (part: Segment) =>
    part.role === "User" && text.startsWith(SUMMARY_PREFIX, part.start + "[User]: ".length);
  const bodies = parts.map((part) => {
    const body = text.slice(part.start, part.end);
    if (part.role === "Tool result") return redactHarness(body, removed);
    if (isEarlierSummary(part)) return redactQuotedHarness(body, removed);
    return body;
  });
  if (removed.size === 0) return text;
  const shaped = bodies.map((body, index) => {
    const part = parts[index]!;
    return part.role === "Assistant" || isEarlierSummary(part) ? scrubEchoes(body, removed) : body;
  });
  let out = text.slice(0, parts[0]!.start);
  parts.forEach((part, index) => {
    out += shaped[index];
    const next = parts[index + 1];
    out += text.slice(part.end, next === undefined ? part.end : next.start);
  });
  return out + text.slice(parts.at(-1)!.end);
}
