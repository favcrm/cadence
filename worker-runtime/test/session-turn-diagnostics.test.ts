import { afterEach, expect, it } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import fixture from "./fixtures/conversation-turn-diagnostics.json";
import { answer, chunk, cleanups, session } from "./support/hosted-session.ts";

afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

it("distinguishes a failed tool from a failed model across a reset without returning content or running anything", async () => {
  let executed = 0;
  const run = await session((requests) => {
    if (requests.length === 1) return new Response(
      chunk({ role: "assistant", content: "" }) + chunk({ tool_calls: [{
        index: 0, id: "lookup:1", type: "function",
        function: { name: "lookup_audit_catalog", arguments: '{"query":"private-tool-argument"}' },
      }] }) + chunk({}, "tool_calls") + "data: [DONE]\n\n",
      { headers: { "content-type": "text/event-stream" } },
    );
    if (requests.length === 2) return answer("private-answer-text");
    return Response.json({ error: { message: "Forbidden: private-model-error-body" } }, { status: 403 });
  }, { tools: [{ name: "test", tools: [{
    name: "lookup_audit_catalog", description: "Read test catalog",
    parameters: { type: "object", properties: { query: { type: "string" } }, required: ["query"] },
    replay: "safe",
    async execute() {
      executed++;
      return { isError: true, output: [{ type: "text", text: "private-tool-result" }] };
    },
  }] }] });
  await (await run.submit("private-first-prompt", null)).wait(BACKGROUND_CONTEXT);
  const first = run.hosted.sessionSummary();
  if (first.state !== "observed") throw new Error("missing first generation");
  await run.reset();
  await (await run.submit("private-second-prompt")).wait(BACKGROUND_CONTEXT);
  const second = run.hosted.sessionSummary();
  if (second.state !== "observed") throw new Error("missing second generation");

  const old = run.hosted.sessionTurnDiagnostics(first.session.sessionId);
  const fresh = run.hosted.sessionTurnDiagnostics(second.session.sessionId);
  expect(old?.events.every(({ at }) => typeof at === "number" && at > 0)).toBe(true);
  expect(fresh?.events.every(({ at }) => typeof at === "number" && at > 0)).toBe(true);
  expect({ ...old, events: old?.events.map((event) => ({ ...event, at: null })) }).toEqual(fixture.toolFailure);
  expect({ ...fresh, events: fresh?.events.map((event) => ({ ...event, at: null })) }).toEqual(fixture.modelFailure);
  expect(run.hosted.sessionTurnDiagnostics("foreign-object:0")).toBeNull();
  expect(JSON.stringify([old, fresh])).not.toMatch(/private-/);
  expect(run.requests).toHaveLength(3);
  expect(executed).toBe(1);
});

it("pages older evidence without skipping turns and rejects a cursor from a different generation", async () => {
  const run = await session(() => answer("private-answer-text"));
  for (let i = 0; i < 14; i++) await (await run.submit("private-user-text")).wait(BACKGROUND_CONTEXT);
  const summary = run.hosted.sessionSummary();
  if (summary.state !== "observed") throw new Error("missing generation");
  const head = run.hosted.sessionTurnDiagnostics(summary.session.sessionId);
  if (head?.nextBefore == null) throw new Error("expected an older page");
  const tail = run.hosted.sessionTurnDiagnostics(summary.session.sessionId, head.nextBefore);
  if (tail === null) throw new Error("missing older page");
  expect(tail.nextBefore).toBeNull();
  expect([...tail.events, ...head.events].flatMap((event) => event.kind === "input" ? [event.messageId] : []))
    .toEqual(Array.from({ length: 14 }, (_, index) => `operation-${index + 1}`));
  await run.reset();
  await (await run.submit("private-new-generation")).wait(BACKGROUND_CONTEXT);
  const after = run.hosted.sessionSummary();
  if (after.state !== "observed") throw new Error("missing newer generation");
  expect(run.hosted.sessionTurnDiagnostics(after.session.sessionId, head.nextBefore)).toBeNull();
});

it("bounds sparse pages even when no diagnostic events appear in the newest stored records", async () => {
  const run = await session(() => answer("private-answer-text"));
  await (await run.submit("private-user-text")).wait(BACKGROUND_CONTEXT);
  const summary = run.hosted.sessionSummary();
  if (summary.state !== "observed") throw new Error("missing generation");
  for (let i = 0; i < 25; i++) await run.writeRecord({
    kind: "private.note",
    data: { text: "private sparse record" },
  });
  const newest = run.hosted.sessionTurnDiagnostics(summary.session.sessionId);
  expect(newest?.events).toEqual([]);
  if (newest?.nextBefore == null) throw new Error("the empty window must still advance");
  expect(run.hosted.sessionTurnDiagnostics(summary.session.sessionId, newest.nextBefore)?.events)
    .toContainEqual({ kind: "input", at: expect.any(Number), messageId: "operation-1", settlement: { outcome: "answered" } });
});

it("reports a bounded evidence gap without discarding other turns", async () => {
  const run = await session(() => answer("private-answer-text"));
  await (await run.submit("private-user-text")).wait(BACKGROUND_CONTEXT);
  const summary = run.hosted.sessionSummary();
  if (summary.state !== "observed") throw new Error("missing generation");
  await run.writeRecord({
    kind: "private.note",
    data: { text: "private oversize content ".repeat(10_000) },
  });
  const page = run.hosted.sessionTurnDiagnostics(summary.session.sessionId);
  expect(page?.events.map(({ kind }) => kind)).toEqual(["input", "model_attempt", "evidence_gap"]);
  expect(page?.events.at(-1)).toEqual({ kind: "evidence_gap", at: null, code: "record_too_large" });
  expect(JSON.stringify(page)).not.toContain("private oversize content");
});

it("pages through compaction heads without mistaking them for a reset boundary", async () => {
  const run = await session(() => answer("private-answer"));
  const first = await (await run.submit("private-before-reset")).wait(BACKGROUND_CONTEXT);
  if (first.type !== "input" || first.status !== "done") throw new Error("expected a retained input");
  const summary = run.hosted.sessionSummary();
  if (summary.state !== "observed") throw new Error("expected the first generation");
  for (let i = 0; i < 68; i++)
    await run.writeRecord({ kind: "pi.compaction", head: first.entry, data: { text: "private-summary" } });
  await run.reset();
  await (await run.submit("private-after-reset")).wait(BACKGROUND_CONTEXT);
  const events = [];
  let before: number | undefined;
  for (let i = 0; i < 4; i++) {
    const page = run.hosted.sessionTurnDiagnostics(summary.session.sessionId, before);
    if (!page) throw new Error("expected retained evidence");
    events.unshift(...page.events);
    if (page.nextBefore === null) break;
    before = page.nextBefore;
  }
  expect(events.filter((event) => event.kind === "input").map((event) => event.messageId))
    .toEqual(["operation-1"]);
  expect(JSON.stringify(events)).not.toMatch(/private-/);
});
