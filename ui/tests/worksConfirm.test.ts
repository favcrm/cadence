import { recordServices } from "../src/features/workspace-apps/screen/screenServices";
import type { SlotPlan } from "../src/features/workspace-apps/screen/screenSlot";
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";

// CAD-1662: Works review/export confirmation goes through the real host
// service. Only the fetch input is controlled; the service code under test is real.
const assert = {
  equal(actual: unknown, expected: unknown, why = "values must match"): void {
    if (actual !== expected) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
  deepEqual(actual: unknown, expected: unknown, why = "structures must match"): void {
    if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
  ok(value: unknown, why = "condition must hold"): asserts value {
    if (!value) throw new Error(why);
  },
};

type Seen = { path: string; body: Record<string, unknown> };

const savedFetch = globalThis.fetch;
const token = "a".repeat(64);
const confirmToken = "b".repeat(64);
const receipt: MountReceipt = {
  mount: `/api/app-screen/${"c".repeat(64)}`, bridge_nonce: "d".repeat(64), generation: 1,
  tag: "works", action_token: token, tools: [], host_contract: "screen-records.v1",
  operations: ["works.review", "works.export"], navigation: null, install_id: "own-install", context_id: "own-context",
  digest: `sha256:${"e".repeat(64)}`, display_scope_key: "f".repeat(64),
  display_session_key: "1".repeat(64), remote_images: [],
};

const cardFor = (operation: string) => ({
  confirmation_token: confirmToken,
  card: { operation, facts: { id: "doc-1", revision: 3, title: "Plan", kind: "doc", digest: `sha256:${"2".repeat(64)}` } },
});

function install(replies: { path: string; body: unknown }[], seen: Seen[]): void {
  globalThis.fetch = async (url, init) => {
    const path = String(url).split("/").pop() ?? "";
    const body: Record<string, unknown> = JSON.parse(String(init?.body));
    seen.push({ path, body });
    const next = replies.shift();
    if (!next) throw new Error(`unexpected request ${path}`);
    assert.equal(path, next.path, "request path");
    return new Response(JSON.stringify(next.body), { status: 200, headers: { "Content-Type": "application/json" } });
  };
}

async function prepare(operation: "works.review" | "works.export"): Promise<{ plan: Extract<SlotPlan, { records: true }>; seen: Seen[] }> {
  const seen: Seen[] = [];
  install([{ path: "slot-prepare", body: cardFor(operation) }, { path: "slot-confirm", body: { id: "doc-1", revision: 3 } }], seen);
  const services = recordServices(receipt, "own-domain", { current: () => true, refused: () => {}, command: () => {} });
  const plan = await services.planner(operation, { id: "doc-1", revision: 3 });
  if (!("run" in plan) || plan.records !== true) throw new Error("expected a records confirmation plan");
  return { plan, seen };
}

async function main(): Promise<void> {
  // 1. review: the card exposes a bounded choice and the confirm sends only that choice.
  {
    const { plan, seen } = await prepare("works.review");
    assert.equal(plan.recordCard?.input?.field, "choice", "review card input field");
    assert.deepEqual(plan.recordCard?.input?.choices?.map(c => c.value), ["approve", "changes"], "review choices are the host vocabulary");
    const result = await plan.run("approve");
    assert.equal(result.ok, true, "approve confirm succeeds");
    assert.equal(seen[1].path, "slot-confirm");
    assert.equal(seen[1].body.confirmation_token, confirmToken);
    assert.deepEqual(seen[1].body.confirmation, { confirmed: true, choice: "approve" }, "confirm carries only the bounded choice");
  }

  // 2. export: format choice is bounded to the host vocabulary and sent as `choice`.
  {
    const { plan, seen } = await prepare("works.export");
    assert.equal(plan.recordCard?.input?.field, "choice", "export card input field");
    assert.deepEqual(plan.recordCard?.input?.choices?.map(c => c.value), ["html", "markdown"], "export choices");
    const result = await plan.run("html");
    assert.equal(result.ok, true, "export confirm succeeds");
    assert.deepEqual(seen[1].body.confirmation, { confirmed: true, choice: "html" }, "export confirm carries only the chosen format");
  }

  // 3. A bare tap with no host choice is refused and sends nothing.
  {
    const { plan, seen } = await prepare("works.review");
    const result = await plan.run(undefined);
    assert.equal(result.ok, false, "bare tap refuses");
    assert.equal(seen.length, 1, "bare tap sends no slot-confirm");
  }

  // 4. A choice outside the host vocabulary is refused before any request.
  {
    const { plan, seen } = await prepare("works.review");
    const result = await plan.run("delete");
    assert.equal(result.ok, false, "unknown decision refuses");
    assert.equal(seen.length, 1, "unknown decision sends no slot-confirm");
  }

  // 5. Confirmation is single-use: a second run never reaches the host.
  {
    const { plan, seen } = await prepare("works.review");
    await plan.run("changes");
    const again = await plan.run("changes");
    assert.equal(again.ok, false, "reuse refuses");
    assert.equal(seen.filter(s => s.path === "slot-confirm").length, 1, "only one confirm is sent");
  }
}

main().then(
  () => { globalThis.fetch = savedFetch; console.log("worksConfirm: ok"); },
  error => { globalThis.fetch = savedFetch; throw error; },
);
