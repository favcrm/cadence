import { api, ApiError } from "../src/lib/api";
import {
  childrenOf,
  epicsOf,
  healthView,
  noMoveReason,
  noteBytes,
  NOTE_MAX_BYTES,
  progressView,
  stageEvents,
  stageLabel,
  TONE_CHIP,
  visibleMoves,
  worseHealth,
  type Viewer,
} from "../src/features/projects/work";
import type { IssueCard, IssueHistoryEntry, WorkStage } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const STAGES = ["shape", "build", "verify", "release", "done"];
const stage = (id: string, moves: WorkStage["moves"], source = "field"): WorkStage => ({
  id,
  source,
  since: "2026-09-20T00:00:00Z",
  exit: null,
  next: null,
  next_needs_operator: false,
  terminal: id === "done",
  stages: STAGES,
  moves,
});
const OPERATOR: Viewer = { readOnly: false, operator: true };
const AGENT: Viewer = { readOnly: false, operator: false };
const READ_ONLY: Viewer = { readOnly: true, operator: true };

type Call = { url: string; init: RequestInit };
function stubFetch(status: number, body: unknown): Call[] {
  const calls: Call[] = [];
  globalThis.fetch = async (url: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(url), init: init ?? {} });
    return {
      ok: status >= 200 && status < 300,
      status,
      statusText: String(status),
      json: async () => body,
    } as Response;
  };
  return calls;
}

async function main() {
  // Each health state renders its own label and tone, with every reason
  // and its next action; an unknown one never reads as on track.
  {
    const onTrack = healthView(
      { state: "on_track", days_in_stage: 1, limit_days: 5, reasons: [] },
      "build",
    );
    equal(
      [onTrack.state, onTrack.label, onTrack.tone, onTrack.timing, onTrack.reasons],
      ["on_track", "on track", "ok", "1 day in build (limit 5)", []],
      "on track",
    );
    const atRisk = healthView(
      {
        state: "at_risk",
        days_in_stage: 2,
        limit_days: 5,
        reasons: [
          {
            cause: "blocked",
            issue: "D-3",
            owner: "dev-1",
            detail: "D-3 waits on D-2",
            next: "unblock D-3 or re-plan around it",
          },
        ],
      },
      "build",
    );
    equal(
      [atRisk.label, atRisk.tone, atRisk.reasons],
      [
        "at risk",
        "warn",
        [{ detail: "D-3 waits on D-2", next: "unblock D-3 or re-plan around it", owner: "dev-1" }],
      ],
      "at risk shows the reason, owner and next action",
    );
    const stalled = healthView(
      {
        state: "stalled",
        days_in_stage: 12,
        limit_days: 5,
        reasons: [
          {
            cause: "stalled",
            issue: "D-1",
            owner: null,
            detail: "12 days in 'build' (limit 5)",
            next: "meet the 'build' exit criterion and move the stage, or record why it waits",
          },
        ],
      },
      "build",
    );
    equal(
      [stalled.label, stalled.tone, stalled.timing, stalled.reasons[0].owner, stalled.reasons[0].next.startsWith("meet")],
      ["stalled", "fail", "12 days in build (limit 5)", null, true],
      "stalled",
    );
    for (const h of [null, undefined, { state: "exploded", reasons: [] }]) {
      const v = healthView(h);
      equal([v.state, v.label, v.tone, v.timing], ["unknown", "health unknown", "muted", null], `unknown ${JSON.stringify(h)}`);
    }
    equal(healthView({ state: "on_track", days_in_stage: null, reasons: [] }, "shape").timing, null, "unknown entry time");
    equal(
      [TONE_CHIP.ok, TONE_CHIP.warn, TONE_CHIP.fail].every((c) => /^bg-(ok|warn|fail)\/\d+ text-(ok|warn|fail)$/.test(c)),
      true,
      "tones are theme tokens",
    );
    equal(
      [worseHealth("on_track", "at_risk"), worseHealth("stalled", "at_risk"), worseHealth("at_risk", "nope")],
      ["at_risk", "stalled", "at_risk"],
      "worst health",
    );
  }

  // Progress is the weights, never a count; nothing weighted is no percent.
  {
    equal(
      progressView({ done_weight: 8, total_weight: 12, ratio: 0.67, counts: {} }),
      { done: 8, total: 12, percent: 67, label: "8/12 weight" },
      "weighted",
    );
    equal(progressView(null), { done: 0, total: 0, percent: null, label: "no tasks yet" }, "empty");
  }

  // Move buttons: the operator sees the legal moves (operator-only ones
  // included, flagged); anyone else — an agent, an unproven caller, a
  // read-only board — sees none; an illegal move in the payload (a skip,
  // the current stage, an unknown stage) is never offered.
  {
    const build = stage("build", [
      { to: "shape", forward: false, needs_operator: false },
      { to: "verify", forward: true, needs_operator: false },
    ]);
    equal(visibleMoves(build, OPERATOR).map((m) => m.to), ["shape", "verify"], "operator: back and one forward");
    equal(visibleMoves(build, AGENT), [], "agent: none on the board");
    equal(visibleMoves(build, READ_ONLY), [], "read-only: none");
    const shape = stage("shape", [{ to: "build", forward: true, needs_operator: true }], "default");
    equal(visibleMoves(shape, OPERATOR), [{ to: "build", forward: true, needs_operator: true }], "operator-only move for the operator");
    equal(visibleMoves(shape, AGENT), [], "operator-only move hidden from an agent");
    const forged = stage("build", [
      { to: "release", forward: true, needs_operator: true },
      { to: "build", forward: false, needs_operator: false },
      { to: "ship", forward: true, needs_operator: false },
      { to: "done", forward: false, needs_operator: false },
      { to: "verify", forward: true, needs_operator: false },
    ]);
    equal(visibleMoves(forged, OPERATOR).map((m) => m.to), ["verify"], "skips, no-ops and unknowns dropped");
    const limbo = stage("limbo", [{ to: "shape", forward: false, needs_operator: false }]);
    equal(visibleMoves(limbo, OPERATOR).map((m) => m.to), ["shape"], "unknown stage → back to the floor");
    equal(visibleMoves(stage("build", undefined), OPERATOR), [], "an older server lists no moves");
    equal(visibleMoves(null, OPERATOR), [], "not an epic");

    equal(noMoveReason(build, OPERATOR), null, "operator: buttons, no reason");
    equal(noMoveReason(build, AGENT)?.includes("cadence issue epic stage"), true, "agent is told its own path");
    equal(noMoveReason(build, READ_ONLY), "The board is read-only.", "read-only reason");
    equal(noMoveReason(stage("shape", [], "plan"), OPERATOR)?.includes("plan"), true, "plan owns the stage");
    equal(noMoveReason(stage("build", undefined), OPERATOR)?.includes("does not list"), true, "old server");
  }

  // The note cap is the server's: bytes, not characters.
  {
    equal(noteBytes("  done  "), 4, "trimmed ascii");
    equal(noteBytes("é"), 2, "two bytes");
    equal(noteBytes("✓".repeat(167)) > NOTE_MAX_BYTES, true, "167 three-byte chars exceed 500 bytes");
    equal("✓".repeat(167).length <= NOTE_MAX_BYTES, true, "though under 500 characters");
  }

  // Stage history: who moved each stage and when, newest first; plan
  // decisions ride along; other commits do not.
  {
    const h: IssueHistoryEntry[] = [
      { sha: "c", at: "2026-09-22T10:00:00Z", by: "operator", kind: "stage", summary: "stage build → verify", from: "build", to: "verify", note: "tasks done" },
      { sha: "b", at: "2026-09-21T10:00:00Z", by: "operator", kind: "set", summary: "set size=L" },
      { sha: "a", at: "2026-09-20T10:00:00Z", by: "operator", kind: "plan", summary: "plan approved by operator (2 tickets ready)" },
    ];
    equal(
      stageEvents(h),
      [
        { at: "2026-09-22T10:00:00Z", by: "operator", what: "build → verify", note: "tasks done" },
        { at: "2026-09-20T10:00:00Z", by: "operator", what: "plan approved by operator (2 tickets ready)", note: null },
      ],
      "stage events",
    );
  }

  // Rows: a project's epics by effective type; children by parent.
  {
    const card = (id: string, project: string, type: string, parent?: string) =>
      ({ id, project, parent, work: { type } }) as unknown as IssueCard;
    const cards = [card("D-1", "demo", "epic"), card("D-2", "demo", "task", "D-1"), card("C-1", "cad", "epic"), card("D-3", "demo", "bug")];
    equal(epicsOf(cards, "demo").map((c) => c.id), ["D-1"], "epics of demo");
    equal(childrenOf(cards, "D-1").map((c) => c.id), ["D-2"], "children");
    equal(stageLabel(stage("build", [])), "build 2/5", "stage label");
    equal(stageLabel(stage("limbo", [])), "limbo", "unknown stage label");
    equal(stageLabel(null), "no stage", "no stage");
  }

  // The move request: the board route with the write guards, no identity
  // field, a blank note dropped; a refusal surfaces its check.
  {
    let calls = stubFetch(200, { epic: "D-1", from: "build", to: "verify", by: "operator", at: "x" });
    const out = await api.moveStage("D-1", "verify", "  ");
    equal(out.to, "verify", "answer");
    equal(calls[0].url, "/api/epics/D-1/stage", "url");
    equal(calls[0].init.method, "POST", "POST");
    equal(calls[0].init.headers, { "Content-Type": "application/json", "X-Cadence-Board": "1" }, "write guards");
    equal(JSON.parse(String(calls[0].init.body)), { stage: "verify" }, "no identity, blank note dropped");
    calls = stubFetch(200, {});
    await api.moveStage("D-1", "verify", " done ");
    equal(JSON.parse(String(calls[0].init.body)), { stage: "verify", note: "done" }, "note trimmed");
    stubFetch(403, { error: "a stage move is the operator's decision — agent 'wk'", check: "operator_only" });
    let err: ApiError | null = null;
    await api.moveStage("D-1", "verify").catch((e: ApiError) => {
      err = e;
    });
    equal([(err as ApiError | null)?.status, (err as ApiError | null)?.check], [403, "operator_only"], "403 surfaces");
    calls = stubFetch(200, { milestones: [] });
    await api.milestones("demo");
    equal(calls[0].url, "/api/milestones?project=demo", "milestones url");
  }
  console.log("projects work checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
