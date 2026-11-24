import { describe, expect, it } from "bun:test";
import type { GenerationIdentity } from "../src/contract.ts";
import { meteredBinding } from "../src/model.ts";
import { TOOL_ERROR_REDACTED } from "../src/summary-tool-errors.ts";
import { settleToolFailures, TRANSIENT_FAILURE_NOTE } from "../src/summary-tool-failures.ts";

const PROMPT = "The messages above are a conversation to summarize.";
const source = (...parts: string[]) => `<conversation>\n${parts.join("\n\n")}\n</conversation>\n\n${PROMPT}`;
const failed = `[Tool result]: <harness>\n[error] ${TOOL_ERROR_REDACTED}\n</harness>`;
const ok = "[Tool result]: opened board 7";
const call = (...names: string[]) => `[Assistant tool calls]: ${names.map((n) => `${n}(board="b1")`).join("; ")}`;
const AGAIN = (tool: string) => `An earlier ${tool} call failed; try it again normally.`;

describe("CAD-2745: failed tool calls stay transient in the summary source", () => {
  it("rewrites give-up sentences that name the failed tool, and closes with the runtime note", () => {
    const out = settleToolFailures(source(
      "[User]: Open the canvas.",
      call("canvas_open"), failed,
      "[Assistant]: canvas_open does not work right now. Avoid calling canvas_open until it is fixed.",
      "[User]: Then draft the copy.",
      "[Assistant]: Here is the copy. The headline uses no emoji.",
    ));
    expect(out).toContain(`[Assistant]: ${AGAIN("canvas_open")} ${AGAIN("canvas_open")}`);
    expect(out).toContain("[Assistant]: Here is the copy. The headline uses no emoji.");
    expect(out).toContain(`\n\n${TRANSIENT_FAILURE_NOTE}\n</conversation>\n\n${PROMPT}`);
    expect(out.match(/\[Runtime note\]/g)).toHaveLength(1);
  });

  it("keeps a sentence that names the tool without a standing claim, and one about another tool", () => {
    const keep = "[Assistant]: canvas_open failed once with a slot clash. I can't open the archive_read tool either.";
    const out = settleToolFailures(source(call("canvas_open"), failed, keep));
    expect(out).toContain(keep);
  });

  it("never changes user text, and only rewrites after the tool has failed", () => {
    const user = "[User]: Do not use canvas_open until Odessa signs off.";
    const before = "[Assistant]: I won't open canvas_open until you confirm.";
    const out = settleToolFailures(source(user, before, call("canvas_open"), failed, "[Assistant]: OK."));
    expect(out).toContain(user);
    expect(out).toContain(before);
  });

  it("pairs results with calls in order, so only the failing one of several counts", () => {
    const out = settleToolFailures(source(
      call("board_list", "canvas_open"), ok, failed,
      "[Assistant]: board_list keeps failing? No. canvas_open keeps failing though.",
    ));
    expect(out).toContain(`[Assistant]: board_list keeps failing? No. ${AGAIN("canvas_open")}`);
  });

  it("matches the tool name as a whole word", () => {
    const keep = "[Assistant]: The canvas_opener plugin is broken.";
    expect(settleToolFailures(source(call("canvas_open"), failed, keep))).toContain(keep);
  });

  it("returns a source with no failed call unchanged", () => {
    const text = source(call("canvas_open"), ok, "[Assistant]: Do not worry, canvas_open worked.");
    expect(settleToolFailures(text)).toBe(text);
  });

  it("stays fast on a long assistant turn", () => {
    const text = source(call("canvas_open"), failed, `[Assistant]: ${"canvas_open is broken until fixed ".repeat(20_000)}`);
    const started = performance.now();
    settleToolFailures(text);
    expect(performance.now() - started).toBeLessThan(500);
  });
});

describe("CAD-2745: the attested summary request", () => {
  const identity: GenerationIdentity = { conversation: "1", task: "2", cutoff: "3" };
  it("redacts the error, then marks the failure transient, before any split", async () => {
    let received: Record<string, unknown> | undefined;
    const binding = meteredBinding(
      { async requestModel(request) { received = request; return new Response("ok"); } },
      () => identity, undefined, 1, undefined, () => true,
    );
    const text = source(call("canvas_open"), "[Tool result]: <harness>\n[error] slot_held_9: Slot q9 is held\n</harness>",
      "[Assistant]: canvas_open keeps refusing with slot_held_9: Slot q9 is held.");
    await binding.run("default", { messages: [{ role: "system", content: "Summarize." }, { role: "user", content: text }] });
    const sent = (received!.messages as Array<{ content: string }>).slice(1).map((m) => m.content).join("");
    expect(sent).not.toContain("slot_held_9");
    expect(sent).toContain(`[Assistant]: ${AGAIN("canvas_open")}`);
    expect(sent).toContain(TRANSIENT_FAILURE_NOTE);
  });
});
