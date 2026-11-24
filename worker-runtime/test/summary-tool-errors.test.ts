import { describe, expect, it } from "bun:test";
import type { GenerationIdentity } from "../src/contract.ts";
import { meteredBinding } from "../src/model.ts";
import {
  redactToolErrors, TOOL_ERROR_CODE_OMITTED, TOOL_ERROR_REDACTED, TOOL_ERROR_TEXT_OMITTED,
} from "../src/summary-tool-errors.ts";

const PROMPT = "The messages above are a conversation to summarize.\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";
const source = (...parts: string[]) => `<conversation>\n${parts.join("\n\n")}\n</conversation>\n\n${PROMPT}`;
const harness = (...lines: string[]) => `<harness>\n${lines.join("\n")}\n</harness>`;
const EARLIER = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";

const MESSAGE = "page_missing_7q: no such wiki page at that path";
const CALL = `[Assistant tool calls]: wiki_read(path="projects/demo/plan.md")`;

describe("CAD-2744: tool error text leaves the summary source", () => {
  it("rewrites an [error] diagnostic and removes its message from the whole source", () => {
    const text = source(
      "[User]: Read the plan page.",
      CALL,
      `[Tool result]: ${harness(`[error] ${MESSAGE}`)}`,
      "[Assistant]: Done.",
    );
    const out = redactToolErrors(text);
    expect(out).toContain(`[Tool result]: ${harness(`[error] ${TOOL_ERROR_REDACTED}`)}`);
    expect(out).not.toContain("no such wiki page");
    expect(out).not.toContain("page_missing_7q");
    expect(out).toContain(CALL);
    expect(out.endsWith(PROMPT)).toBe(true);
  });

  it("keeps [warn] lines and a result's other output", () => {
    const result = `[Tool result]: 12 rows\n${harness("[warn] output was shortened", `[error] ${MESSAGE}`)}`;
    const out = redactToolErrors(source("[User]: Count rows.", result));
    expect(out).toContain(`[Tool result]: 12 rows\n${harness("[warn] output was shortened", `[error] ${TOOL_ERROR_REDACTED}`)}`);
  });

  it("removes a multi-line error message whole", () => {
    const out = redactToolErrors(source(`[Tool result]: ${harness("[error] quota_burst_9: limit hit", "retry after 30 s on lane q7")}`));
    expect(out).toContain(harness(`[error] ${TOOL_ERROR_REDACTED}`));
    expect(out).not.toContain("lane q7");
  });

  it("replaces an assistant echo of the message and of its identifier code", () => {
    const out = redactToolErrors(source(
      `[Tool result]: ${harness(`[error] ${MESSAGE}`)}`,
      `[Assistant]: It failed: ${MESSAGE}. Still page_missing_7q on retry.`,
    ));
    expect(out).toContain(`[Assistant]: It failed: ${TOOL_ERROR_TEXT_OMITTED}. Still ${TOOL_ERROR_CODE_OMITTED} on retry.`);
  });

  it("never changes a [User] segment, even when it carries the same text", () => {
    const user = `[User]: Our till shows ${MESSAGE} too, and error E-4417 on sync.`;
    const out = redactToolErrors(source(user, `[Tool result]: ${harness(`[error] ${MESSAGE}`)}`));
    expect(out).toContain(user);
  });

  it("cleans an earlier summary's one-line harness quote and its echo", () => {
    const summary = `[User]: ${EARLIER}## Blocked\n- wiki_read returned \`<harness>[error] ${MESSAGE}</harness>\`.\n\n## Critical Context\n- Error: \`${MESSAGE}\`\n- Till error E-4417 blocks launch.\n</summary>`;
    const out = redactToolErrors(source(summary, "[User]: Continue."));
    expect(out).toContain(`\`<harness>[error] ${TOOL_ERROR_REDACTED}</harness>\``);
    expect(out).toContain(`- Error: \`${TOOL_ERROR_TEXT_OMITTED}\``);
    expect(out).toContain("Till error E-4417 blocks launch.");
    expect(out).not.toContain("no such wiki page");
  });

  it("rewrites a short message only in its harness line", () => {
    const out = redactToolErrors(source(
      `[Tool result]: ${harness("[error] timeout")}`,
      "[Assistant]: The search hit a timeout; I'll try once more.",
    ));
    expect(out).toContain(harness(`[error] ${TOOL_ERROR_REDACTED}`));
    expect(out).toContain("[Assistant]: The search hit a timeout; I'll try once more.");
  });

  it("leaves a result truncated inside its harness block unchanged, and the next segment intact", () => {
    const truncated = `[Tool result]: ${"x".repeat(40)}\n<harness>\n[error] ${MESSAGE.slice(0, 20)}\n\n[... 812 more characters truncated]`;
    const next = `[Assistant]: Retrying ${MESSAGE.slice(0, 20)}.`;
    const text = source("[User]: Go.", truncated, next);
    expect(redactToolErrors(text)).toBe(text);
  });

  it("does not let an unclosed block swallow a later closed one", () => {
    const open = `[Tool result]: <harness>\n[error] cut off\n\n[... 9 more characters truncated]`;
    const closed = `[Tool result]: ${harness(`[error] ${MESSAGE}`)}`;
    const out = redactToolErrors(source(open, "[Assistant]: Next.", closed));
    expect(out).toContain(open);
    expect(out).toContain("[Assistant]: Next.");
    expect(out).toContain(`[Tool result]: ${harness(`[error] ${TOOL_ERROR_REDACTED}`)}`);
  });

  it("shapes a long unclosed quote in an earlier summary in linear time", () => {
    const summary = `[User]: ${EARLIER}<harness>[error]${" ".repeat(64_000)}\n</summary>`;
    const text = source(summary, "[User]: Go.");
    const started = performance.now();
    expect(redactToolErrors(text)).toBe(text);
    expect(performance.now() - started).toBeLessThan(500);
  });

  it("cleans a multi-diagnostic quote in an earlier summary, keeping its warning", () => {
    const summary = `[User]: ${EARLIER}- Result: <harness>\n[warn] output shortened\n[error] ${MESSAGE}\n</harness>\n</summary>`;
    const out = redactToolErrors(source(summary));
    expect(out).toContain(`<harness>\n[warn] output shortened\n[error] ${TOOL_ERROR_REDACTED}\n</harness>`);
    expect(out).not.toContain("no such wiki page");
  });

  it("never lets an unclosed quote in an earlier summary swallow later facts", () => {
    const summary = `[User]: ${EARLIER}- Tried <harness>[error] cut off\n- Budget HK$18,450.\n- Quote: \`<harness>[error] ${MESSAGE}</harness>\`\n</summary>`;
    const out = redactToolErrors(source(summary));
    expect(out).toContain("- Budget HK$18,450.");
    expect(out).toContain(`\`<harness>[error] ${TOOL_ERROR_REDACTED}</harness>\``);
  });

  it("treats only the engine's severities as diagnostics, so a continuation line goes with its message", () => {
    const out = redactToolErrors(source(`[Tool result]: ${harness("[error] build failed", "[stderr] token=abc123 rejected")}`));
    expect(out).toContain(harness(`[error] ${TOOL_ERROR_REDACTED}`));
    expect(out).not.toContain("token=abc123");
  });

  it("leaves its own placeholder alone on the next compaction", () => {
    const once = redactToolErrors(source(
      `[Tool result]: ${harness(`[error] ${MESSAGE}`)}`,
      `[Assistant]: Saw ${MESSAGE}.`,
    ));
    const summary = `[User]: ${EARLIER}- \`<harness>[error] ${TOOL_ERROR_REDACTED}</harness>\`\n- ${TOOL_ERROR_REDACTED}\n</summary>`;
    expect(redactToolErrors(source(summary))).toBe(source(summary));
    expect(once).toContain(TOOL_ERROR_TEXT_OMITTED);
  });

  it("returns a source with no tool error unchanged", () => {
    const text = source("[User]: Hi.", "[Assistant]: Hello.", `[Tool result]: ${harness("[warn] note")}`);
    expect(redactToolErrors(text)).toBe(text);
  });
});

describe("CAD-2744: the attested summary request", () => {
  const identity: GenerationIdentity = { conversation: "1", task: "2", cutoff: "3" };
  const send = async (input: Record<string, unknown>, compacting: boolean) => {
    let received: Record<string, unknown> | undefined;
    const binding = meteredBinding(
      { async requestModel(request) { received = request; return new Response("ok"); } },
      () => identity, undefined, 1, undefined, () => compacting,
    );
    await binding.run("default", input);
    return received!;
  };

  it("redacts before splitting a source over 64,000 characters", async () => {
    const filler = `[User]: ${"word ".repeat(14_000)}`;
    const text = source(filler, `[Tool result]: ${harness(`[error] ${MESSAGE}`)}`, filler, `[Assistant]: Saw ${MESSAGE}.`);
    const request = await send({
      messages: [{ role: "system", content: "Summarize." }, { role: "user", content: [{ type: "text", text }] }],
    }, true);
    const parts = (request.messages as Array<{ role: string; content: string }>).filter((m) => m.role === "user");
    expect(parts.length).toBeGreaterThan(1);
    expect(parts.every((part) => part.content.length <= 64_000)).toBe(true);
    const joined = parts.map((part) => part.content).join("");
    expect(joined).toBe(redactToolErrors(text));
    expect(joined).not.toContain("no such wiki page");
  });

  it("leaves a turn request untouched", async () => {
    const text = source(`[Tool result]: ${harness(`[error] ${MESSAGE}`)}`);
    const input = { messages: [{ role: "system", content: "You help." }, { role: "user", content: text }] };
    const request = await send(input, false);
    expect(JSON.stringify(request.messages)).toContain(MESSAGE);
  });
});
