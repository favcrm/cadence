import { api, ApiError, planDecision } from "../src/lib/api";
import { planGoal, planView, rejectReasonError } from "../src/features/home/plan";
import type { IssueDetail } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const detail = (plan: unknown): IssueDetail =>
  ({
    id: "D-2",
    title: "Onboarding",
    body: "Onboarding\n\n## Goal\n\nFirst chat in five minutes\n\n## Non-goals\n\n- billing\n",
    plan,
  }) as unknown as IssueDetail;

const PLAN = {
  state: "proposed",
  proposed_by: "master",
  tickets: [
    { id: "D-3", title: "Wizard", status: "backlog", size: "L", acceptance: 2 },
    { id: "D-4", title: "Docs", status: "done", size: "S", acceptance: 1, blocked_by: ["D-3"] },
    { id: "D-5", title: "Polish", status: "dropped", size: null, acceptance: 1 },
  ],
  progress: { done_weight: 1, total_weight: 9, ratio: 0.11 },
};

type Call = { url: string; init: RequestInit };

function stubFetch(status: number, body: unknown): Call[] {
  const calls: Call[] = [];
  (globalThis as { fetch: unknown }).fetch = async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return {
      ok: status >= 200 && status < 300,
      status,
      statusText: "",
      json: async () => body,
    } as Response;
  };
  return calls;
}

async function main() {
  // The card's view: goal, tickets with size and acceptance, progress.
  {
    equal(planGoal(detail(PLAN).body), "First chat in five minutes", "goal section");
    equal(planGoal("no goal here"), null, "no goal");
    const v = planView(detail(PLAN), null)!;
    equal([v.epic, v.state, v.goal, v.percent, v.done, v.total], ["D-2", "proposed", "First chat in five minutes", 11, 1, 2], "view");
    equal(v.tickets.map((t) => [t.id, t.size, t.acceptance]), [["D-3", "L", 2], ["D-4", "S", 1], ["D-5", null, 1]], "tickets");
    equal([v.canDecide, v.blockedReason], [true, null], "proposed → decide");
    const ro = planView(detail(PLAN), "Sign in with `cadence ui login` to act as the operator.")!;
    equal(ro.canDecide, false, "blocked → no buttons");
    equal(ro.blockedReason?.includes("cadence ui login"), true, "with the reason");
    equal(ro.blockedReason?.includes("cadence plan approve"), true, "and the CLI path");
    equal(planView(detail({ ...PLAN, state: "approved" }), null)!.canDecide, false, "decided plans have no buttons");
    equal(planView(detail(null), null), null, "not a plan");
    const bare = planView(detail({ state: "proposed" }), null)!;
    equal([bare.tickets, bare.percent], [[], null], "missing fields degrade");
  }

  // Actions: approve posts {} to the board endpoint with the write
  // guards; reject needs a reason and never sends without one.
  {
    equal(planDecision("D-2", "approve"), { path: "/api/plans/D-2/approve", body: {} }, "approve request");
    equal(planDecision("D-2", "reject", "  too big  "), { path: "/api/plans/D-2/reject", body: { reason: "too big" } }, "reject request");
    let refused: ApiError | null = null;
    try {
      planDecision("D-2", "reject", "   ");
    } catch (e) {
      refused = e as ApiError;
    }
    equal([refused?.status, refused?.code], [400, "reason_required"], "reason required");
    equal(rejectReasonError(" ") !== null, true, "the card says why");
    equal(rejectReasonError("scope"), null, "a reason passes");

    let calls = stubFetch(200, { state: "approved", ready: ["D-3"] });
    const out = await api.decidePlan("D-2", "approve");
    equal(out, { state: "approved", ready: ["D-3"] }, "approve answer");
    equal(calls[0].url, "/api/plans/D-2/approve", "approve url");
    equal(calls[0].init.method, "POST", "POST");
    equal(calls[0].init.headers, { "Content-Type": "application/json", "X-Cadence-Board": "1" }, "write guards");
    equal(calls[0].init.body, "{}", "no identity fields");

    calls = stubFetch(200, {});
    let err: ApiError | null = null;
    await api.decidePlan("D-2", "reject", "").catch((e: ApiError) => {
      err = e;
    });
    equal([calls.length, (err as ApiError | null)?.code], [0, "reason_required"], "no request without a reason");

    calls = stubFetch(403, { error: "plan approve is the operator's decision — agent 'wk'", check: "operator_only" });
    err = null;
    await api.decidePlan("D-2", "approve").catch((e: ApiError) => {
      err = e;
    });
    const refusal = err as ApiError | null;
    equal([refusal?.status, refusal?.check], [403, "operator_only"], "403 surfaces");
    equal(refusal?.message.includes("operator"), true, "with the server's reason");
    equal(JSON.parse(String(calls[0].init.body)), {}, "sent once");

    // The answer endpoint files through the board with the same guards.
    calls = stubFetch(201, { issue: {}, card: {}, warnings: [] });
    await api.answer("D-2", "q.md", "hourly");
    equal(calls[0].url, "/api/issues/D-2/answers", "answer url");
    equal(JSON.parse(String(calls[0].init.body)), { question: "q.md", text: "hourly" }, "answer body");
  }
  console.log("plan card checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
