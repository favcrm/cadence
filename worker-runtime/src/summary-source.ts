import type { Message, ToolResultMessage, UserMessage } from "@earendil-works/pi-ai";
import { CompactionEntry, ToolResultEntry, type CompactionHooks } from "@earendil-works/pi-durable";
import { errorEchoScrubber, redactQuotedHarness, TOOL_ERROR_REDACTED } from "./summary-tool-errors.ts";
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
      const failed = message.isError || diagnostics.some((item) => item.severity === "error");
      let shaped = message;
      if (failed && diagnostics.length > 0) {
        const last = message.content.at(-1);
        const render = (scrub: boolean) => `<harness>\n${diagnostics.map(({ severity, message }) =>
          `[${severity}] ${scrub && severity === "error" ? TOOL_ERROR_REDACTED : message}`).join("\n")}\n</harness>`;
        if (last?.type === "text" && last.text === render(false)) {
          for (const item of diagnostics) if (item.severity === "error") errors.add(item.message.trim());
          shaped = { ...message, content: [...message.content.slice(0, -1), { type: "text", text: render(true) }] };
        }
      }
      outcomes.set(message, { failed, shaped });
    }
  }

  const userRules = new Set<string>();
  for (const message of messages) {
    if (message.role === "user" && !priorSummaries.has(message)) {
      if (typeof message.content === "string") collectUserToolRestrictions(message.content, userRules);
      else for (const block of message.content) {
        if (block.type === "text") collectUserToolRestrictions(block.text, userRules);
      }
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
      const text: string[] = [];
      for (const part of message.content) if (part.type === "text") text.push(part.text);
      const before = text.join("\n");
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
