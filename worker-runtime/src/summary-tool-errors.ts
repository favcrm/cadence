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
 */

export const TOOL_ERROR_REDACTED = "the call failed; its error text is left out of this summary";
export const TOOL_ERROR_TEXT_OMITTED = "(tool error text omitted)";
export const TOOL_ERROR_CODE_OMITTED = "(tool error code omitted)";

/** How the engine introduces an earlier summary in the summarized turns. */
const SUMMARY_PREFIX = "The conversation history before this point was compacted into the following summary:";

/** A message shorter than this is common words ("timeout"), not worth chasing outside its harness line. */
const MIN_ECHO_CHARS = 16;
/** A leading error code worth chasing: an identifier with an underscore or a digit, at least 6 characters. */
const ERROR_CODE = /^[A-Za-z][A-Za-z0-9_.-]{5,}$/;

const ROLE = String.raw`\[(?:User|Assistant|Assistant tool calls|Tool result)\]: `;
const SEGMENT_START = new RegExp(String.raw`(?:^|\n\n|<conversation>\n)(?=${ROLE})`, "g");
const CONVERSATION_END = "\n</conversation>";

/** A closed diagnostics block as the engine renders it in a tool result. */
const HARNESS_BLOCK = /<harness>\n([\s\S]*?)\n<\/harness>/g;
/** A quoted block inside a summary: often on one line, sometimes in backticks. */
const QUOTED_HARNESS_ERROR = /<harness>\s*\[error\]\s*([\s\S]*?)\s*<\/harness>/g;

type Segment = { role: string; start: number; end: number };

function segments(text: string): Segment[] {
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

/** Rewrite each `[error]` diagnostic of a closed harness block; collect what it said. */
function redactHarness(body: string, removed: Set<string>): string {
  return body.replace(HARNESS_BLOCK, (block, inner: string) => {
    const lines = inner.split("\n");
    const out: string[] = [];
    let inError = false;
    let message: string[] = [];
    const flush = () => {
      if (inError) removed.add(message.join("\n").trim());
      message = [];
    };
    for (const line of lines) {
      const diagnostic = /^\[([a-z]+)\] ?(.*)$/.exec(line);
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

/** An earlier summary's quoted `<harness>[error] …</harness>`, in any layout. */
function redactQuotedHarness(body: string, removed: Set<string>): string {
  return body.replace(QUOTED_HARNESS_ERROR, (_quote, message: string) => {
    removed.add(message.trim());
    return `<harness>[error] ${TOOL_ERROR_REDACTED}</harness>`;
  });
}

function escape(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Replace echoes of removed messages, longest first, then of their codes. */
function scrubEchoes(body: string, removed: ReadonlySet<string>): string {
  const messages = [...removed].filter((message) => message.length >= MIN_ECHO_CHARS)
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
