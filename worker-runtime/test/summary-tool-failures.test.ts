import { describe, expect, it } from "bun:test";
import type { GenerationIdentity } from "../src/contract.ts";
import { meteredBinding } from "../src/model.ts";
import { TOOL_ERROR_REDACTED } from "../src/summary-tool-errors.ts";
import { callNames, settleToolFailures, TRANSIENT_FAILURE_NOTE } from "../src/summary-tool-failures.ts";

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

  it("matches the tool name as a whole word on both sides", () => {
    for (const keep of ["[Assistant]: The canvas_opener plugin is broken.", "[Assistant]: my_canvas_open is broken right now."])
      expect(settleToolFailures(source(call("canvas_open"), failed, keep))).toContain(keep);
  });

  it("clears a failure once a later call of the tool succeeds", () => {
    const keep = "[Assistant]: canvas_open opened board 42; don't share its URL until launch. I used canvas_open and it works now, so you never need to retry manually.";
    const out = settleToolFailures(source(call("canvas_open"), failed, "[Assistant]: Retrying.", call("canvas_open"), "[Tool result]: opened board 42", keep));
    expect(out).toContain(keep);
  });

  it("keeps a restated user rule and a by-design limit", () => {
    const rule = "[Assistant]: I will never call canvas_open on the prod board.";
    const design = "[Assistant]: canvas_open isn't available for archived boards by design.";
    const out = settleToolFailures(source("[User]: Never call canvas_open on the prod board.", call("canvas_open"), failed, rule, design));
    expect(out).toContain(rule);
    expect(out).toContain(design);
    expect(settleToolFailures(source(call("canvas_open"), failed, "[Assistant]: canvas_open is unavailable right now.")))
      .toContain(`[Assistant]: ${AGAIN("canvas_open")}`);
  });

  it("reads call names outside JSON strings only", () => {
    expect(callNames(String.raw`write_file(content="a(); helper(1) \" x; y("); canvas_open(board="b")`)).toEqual(["write_file", "canvas_open"]);
    const out = settleToolFailures(source(
      `[Assistant tool calls]: write_file(content="a(); helper(1)"); canvas_open(board="b")`, ok, failed,
      "[Assistant]: canvas_open does not work, avoid it. Don't run helper on prod, helper is broken.",
    ));
    expect(out).toContain(`[Assistant]: ${AGAIN("canvas_open")} Don't run helper on prod, helper is broken.`);
  });

  it("adds the retry note without blaming either tool when a result is missing", () => {
    const keep = "[Assistant]: canvas_open does not work. Avoid calling screenshot until it is fixed.";
    const text = source(call("screenshot", "canvas_open"), failed, keep);
    expect(settleToolFailures(text)).toBe(text.replace("\n</conversation>", `\n\n${TRANSIENT_FAILURE_NOTE}\n</conversation>`));
  });

  it("keeps corrections, operational facts, and scoped instructions after a failure", () => {
    const keep = [
      "Stop using the old slug q9 and pass slug q10 to canvas_open.",
      "Don't run the migration yet; canvas_open will show the draft board 7 once you approve.",
      "Don't open the archived board 12 with canvas_open, it would unarchive it.",
      "Never try to call canvas_open with a board id from another workspace, ids are scoped.",
      "canvas_open refuses archived boards by design, so I picked board 8.",
      "I'll keep the draft in board 7 until the import is working; canvas_open can show it.",
      "canvas_open is unavailable for archived boards.",
      "canvas_open does not work right now; the draft is saved in board 7.",
    ].join("\n");
    const out = settleToolFailures(source(call("canvas_open"), failed, `[Assistant]: ${keep}`));
    expect(out).toContain(`[Assistant]: ${keep}\n\n${TRANSIENT_FAILURE_NOTE}`);
  });

  it("counts only CAD-2744's failure line, not an [error] log line beside a warning", () => {
    const text = source(call("bash"), "[Tool result]: build log\n[error] lint warning in a.ts\n<harness>\n[warn] output truncated\n</harness>",
      "[Assistant]: bash finished; don't worry about the lint line.");
    expect(settleToolFailures(text)).toBe(text);
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
