/** CAD-2744: scrub rendered tool diagnostics, historical quotes and their echoes. */

export const TOOL_ERROR_REDACTED = "the call failed; its error text is left out of this summary";
export const TOOL_ERROR_TEXT_OMITTED = "(tool error text omitted)";
export const TOOL_ERROR_CODE_OMITTED = "(tool error code omitted)";

// Short messages are common words ("timeout"), not safe to scrub outside diagnostics.
const MIN_ECHO_CHARS = 16;
const ERROR_CODE = /^[A-Za-z][A-Za-z0-9_.-]{5,}$/;

const HARNESS_OPEN = "<harness>";
const HARNESS_CLOSE = "</harness>";
/**
 * Bound the search for an older summary's quoted diagnostic. Pi's serializer
 * kept at most 2,000 characters of the original tool result.
 */
const MAX_BLOCK = 2_000;

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

/**
 * An earlier summary's quoted harness block, in any layout (often one line,
 * sometimes in backticks): each `[error]` diagnostic in it becomes the fixed
 * line.
 */
export function redactQuotedHarness(body: string, removed: Set<string>): string {
  return redactHarnessBlocks(body, removed, HARNESS_OPEN, HARNESS_CLOSE, /(\[(?:info|warn|error)\])/);
}

/** Explicitly failed tools can return rendered diagnostics without a separate metadata array. */
export function redactFailedOutput(body: string, removed: Set<string>): string {
  return redactHarnessBlocks(body, removed, `${HARNESS_OPEN}\n`, `\n${HARNESS_CLOSE}`, /^(\[(?:info|warn|error)\])/m);
}

function redactHarnessBlocks(body: string, removed: Set<string>, open: string, close: string, diagnostic: RegExp): string {
  return eachBlock(body, open, close, (inner) => {
    const parts = inner.split(diagnostic);
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
    return `${open}${changed ? parts.join("") : inner}${close}`;
  });
}

function escape(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Replace echoes of removed messages, longest first, then of their codes. */
export function errorEchoScrubber(removed: ReadonlySet<string>): (body: string) => string {
  const messages = [...removed].filter((message) => message.length >= MIN_ECHO_CHARS && message !== TOOL_ERROR_REDACTED)
    .sort((a, b) => b.length - a.length);
  const codes = new Set<string>();
  for (const message of removed) {
    const colon = message.indexOf(": ");
    const code = colon < 0 ? "" : message.slice(0, colon);
    if (ERROR_CODE.test(code) && /[_\d]/.test(code)) codes.add(code);
  }
  const patterns = [...codes].map((code) => new RegExp(String.raw`(?<![\w-])${escape(code)}(?![\w-])`, "g"));
  return (body) => {
    for (const message of messages) body = body.split(message).join(TOOL_ERROR_TEXT_OMITTED);
    for (const pattern of patterns) body = body.replace(pattern, TOOL_ERROR_CODE_OMITTED);
    return body;
  };
}
