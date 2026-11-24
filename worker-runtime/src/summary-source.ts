import type { Message, ToolResultMessage, UserMessage } from "@earendil-works/pi-ai";
import { CompactionEntry, ToolResultEntry, type CompactionHooks } from "@earendil-works/pi-durable";
import { errorEchoScrubber, redactFailedOutput, redactQuotedHarness, TOOL_ERROR_REDACTED } from "./summary-tool-errors.ts";
import { collectUserToolRestrictions, rewriteGiveUps, TRANSIENT_FAILURE_NOTE } from "./summary-tool-failures.ts";

/** Shape only Pi's pinned compaction source, never the transcript or its retained tail. */
export const renderSummarySource: CompactionHooks["renderCompactionSource"] = ({ entries, messages, serialize }) => {
  const included = new Set(messages);
  const errors = new Set<string>();
  const priorSummaries = new Map<UserMessage, UserMessage>();
  const outcomes = new Map<ToolResultMessage, { failed: boolean; shaped: ToolResultMessage }>();

  // Match contributions by identity. An edited contribution or a synthetic
  // missing result must not borrow diagnostics from a stored entry.
  for (const entry of entries) {
    const message = entry.model?.[0];
    if (!message || !included.has(message)) continue;
    if (CompactionEntry.is(entry) && message.role === "user") {
      priorSummaries.set(message, mapUserText(message, (text) => redactQuotedHarness(text, errors)));
    } else if (ToolResultEntry.is(entry) && message.role === "toolResult") {
      const diagnostics = entry.data?.diagnostics ?? [];
      const hasDiagnosticError = diagnostics.some((item) => item.severity === "error");
      const failed = message.isError || hasDiagnosticError;
      let shaped = message;
      if (hasDiagnosticError) {
        const last = message.content.at(-1);
        const render = (scrub: boolean) => `<harness>\n${diagnostics.map(({ severity, message }) =>
          `[${severity}] ${scrub && severity === "error" ? TOOL_ERROR_REDACTED : message}`).join("\n")}\n</harness>`;
        // Pi appends this exact block even when isError is false. If the pinned
        // record disagrees, do not send unverified diagnostic text to the model.
        if (last?.type !== "text" || last.text !== render(false)) {
          throw new Error("Compaction source has an inconsistent tool diagnostic.");
        }
        for (const item of diagnostics) if (item.severity === "error") errors.add(item.message.trim());
        shaped = { ...message, content: [...message.content.slice(0, -1), { type: "text", text: render(true) }] };
      }
      if (message.isError) {
        const before = visibleText(shaped.content);
        const redacted = redactFailedOutput(before, errors);
        if (redacted !== before) shaped = { ...shaped, content: [{ type: "text", text: redacted }] };
      }
      outcomes.set(message, { failed, shaped });
    }
  }

  const userRules = new Map<string, Set<string>>();
  for (const message of messages) {
    if (message.role === "user" && !priorSummaries.has(message)) {
      const text = typeof message.content === "string" ? message.content : visibleText(message.content);
      collectUserToolRestrictions(text, userRules);
    }
  }

  const scrubEchoes = errorEchoScrubber(errors);
  const failed = new Set<string>();
  let anyFailed = false;
  const shaped = messages.map((message): Message => {
    if (message.role === "toolResult") {
      // Pi orders these by call ID, including results without visible text.
      // Synthetic missing results have no stored entry and prove no failure.
      const outcome = outcomes.get(message);
      if (outcome?.failed) {
        failed.add(message.toolName);
        anyFailed = true;
      } else failed.delete(message.toolName);
      return outcome?.shaped ?? message;
    } else if (message.role === "assistant") {
      const before = visibleText(message.content);
      const redacted = scrubEchoes(before);
      const after = failed.size === 0 ? redacted : rewriteGiveUps(redacted, failed, userRules);
      // A fresh call supersedes older evidence about that tool. Its typed
      // result (if any) establishes the next outcome before the next answer.
      for (const part of message.content) if (part.type === "toolCall") failed.delete(part.name);
      if (after !== before) {
        return { ...message, content: [{ type: "text", text: after }, ...message.content.filter((part) => part.type !== "text")] };
      }
    } else if (message.role === "user") {
      const prior = priorSummaries.get(message);
      if (prior !== undefined) return mapUserText(prior, scrubEchoes);
    }
    return message;
  });
  const source = serialize(shaped, shortenArgument);
  return anyFailed ? `${source}\n\n${TRANSIENT_FAILURE_NOTE}` : source;
};

function visibleText(blocks: readonly { type: string; text?: string }[]): string {
  let text: string | undefined;
  for (const block of blocks) {
    if (block.type !== "text" || block.text === undefined) continue;
    text = text === undefined ? block.text : `${text}\n${block.text}`;
  }
  return text ?? "";
}

function mapUserText(message: UserMessage, transform: (text: string) => string): UserMessage {
  const content = typeof message.content === "string" ? transform(message.content)
    : message.content.map((block) => block.type === "text" ? { ...block, text: transform(block.text) } : block);
  return { ...message, content };
}

/** CAD-2480: bound large serialized argument strings on real tool calls only. */
function shortenArgument(json: string): string {
  if (json.length <= 2_202) return json;
  return json.replace(/"(?:[^"\\]|\\[\s\S])*"/g, (literal) => {
    const body = literal.slice(1, -1);
    if (body.length <= 2_200) return literal;
    let head = 1_000;
    if (/\\*$/.exec(body.slice(0, head))![0].length % 2 === 1) head--;
    if (splitsPair(body, head)) head--;
    let tail = body.length - 1_000;
    if (body[tail - 1] === "\\" && /\\*$/.exec(body.slice(0, tail))![0].length % 2 === 1) tail++;
    if (splitsPair(body, tail)) tail++;
    return `"${body.slice(0, head)}[... ${tail - head} more characters omitted]${body.slice(tail)}"`;
  });
}

function splitsPair(text: string, at: number): boolean {
  const high = text.charCodeAt(at - 1);
  const low = text.charCodeAt(at);
  return high >= 0xd800 && high <= 0xdbff && low >= 0xdc00 && low <= 0xdfff;
}
