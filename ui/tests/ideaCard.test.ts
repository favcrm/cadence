import { api, ApiError } from "../src/lib/api";
import {
  ideaDecisionBody,
  parkDateError,
  rejectReasonError,
} from "../src/features/home/idea";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

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
  // Validation first: approve sends nothing, reject needs a reason,
  // park needs a real UTC date — nothing sends until these pass.
  {
    equal(ideaDecisionBody("approve", "", ""), { body: {}, error: null }, "approve");
    equal(
      ideaDecisionBody("reject", "  too vague  ", ""),
      { body: { reason: "too vague" }, error: null },
      "reject trims its reason",
    );
    equal(ideaDecisionBody("reject", "   ", "").error !== null, true, "reject needs a reason");
    equal(rejectReasonError("scope"), null, "a reason passes");
    equal(
      ideaDecisionBody("park", "", " 2026-10-15 "),
      { body: { park_until: "2026-10-15" }, error: null },
      "park trims its date",
    );
    equal(ideaDecisionBody("park", "", "").error !== null, true, "park needs a date");
    equal(ideaDecisionBody("park", "", "15.10.2026").error !== null, true, "park reads YYYY-MM-DD");
    equal(ideaDecisionBody("park", "", "2026-13-01").error !== null, true, "park reads a real date");
    equal(parkDateError("1969-12-31") !== null, true, "park stays on the UTC calendar");
    equal(parkDateError("2026-10-15"), null, "a date passes");
  }

  // The request: one POST per decision, no identity fields — the
  // daemon attributes the board's proven connection, never the body.
  {
    let calls = stubFetch(200, { decision: { action: "approve" } });
    const out = await api.ideaDecide("D-9", "approve");
    equal(out, { decision: { action: "approve" } }, "approve answer");
    equal(calls[0].url, "/api/ideas/D-9/decide", "decide url");
    equal(calls[0].init.method, "POST", "POST");
    equal(calls[0].init.headers, { "Content-Type": "application/json", "X-Cadence-Board": "1" }, "write guards");
    equal(JSON.parse(String(calls[0].init.body)), { action: "approve" }, "no identity fields");

    calls = stubFetch(200, { decision: { action: "reject" } });
    await api.ideaDecide("D-9", "reject", { reason: "too big" });
    equal(JSON.parse(String(calls[0].init.body)), { action: "reject", reason: "too big" }, "reject body");

    calls = stubFetch(200, { decision: { action: "park" } });
    await api.ideaDecide("D-9", "park", { park_until: "2026-10-15" });
    equal(
      JSON.parse(String(calls[0].init.body)),
      { action: "park", park_until: "2026-10-15" },
      "park body",
    );

    calls = stubFetch(403, { error: "idea decide is the operator's decision — agent 'wk'", check: "operator_only" });
    let err: ApiError | null = null;
    await api.ideaDecide("D-9", "approve").catch((e: ApiError) => {
      err = e;
    });
    const refusal = err as ApiError | null;
    equal([refusal?.status, refusal?.check], [403, "operator_only"], "403 surfaces");
    equal(refusal?.message.includes("operator"), true, "with the server's reason");
  }

  // Filing: a report goes through the intake endpoint — kind plus the
  // board filed from, never bare tags. Routing is the server's.
  {
    const calls = stubFetch(201, { issue: {}, card: {}, report: {} });
    await api.report({ kind: "idea", project: "demo", title: "Dark mode", priority: "P3", body: "Why it matters" });
    equal(calls[0].url, "/api/reports", "report url");
    equal(calls[0].init.method, "POST", "POST");
    equal(
      JSON.parse(String(calls[0].init.body)),
      { kind: "idea", project: "demo", title: "Dark mode", priority: "P3", body: "Why it matters" },
      "report kind, board, title and body",
    );
  }

  // Decisions carry what they were made against: the idea binds its
  // shown rev, the merge its shown head; decline states its reason.
  {
    let calls = stubFetch(200, { decision: { action: "approve" } });
    await api.ideaDecide("D-9", "approve", { expect_rev: "fnv1a:abc" });
    equal(
      JSON.parse(String(calls[0].init.body)),
      { action: "approve", expect_rev: "fnv1a:abc" },
      "idea binds its shown rev",
    );

    calls = stubFetch(200, { state: "enqueued" });
    await api.mergeDelivery("D-2", "c".repeat(40));
    equal(calls[0].url, "/api/delivery/D-2/merge", "merge url");
    equal(JSON.parse(String(calls[0].init.body)), { sha: "c".repeat(40) }, "merge binds its shown head");

    calls = stubFetch(200, { state: "declined" });
    await api.declineDelivery("D-2", "not now");
    equal(calls[0].url, "/api/delivery/D-2/decline", "decline url");
    equal(JSON.parse(String(calls[0].init.body)), { reason: "not now" }, "decline states its reason");
  }
  console.log("idea card checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
