import { parseChild, type RecordCallVerb, type RecordSlotVerb } from "../src/features/workspace-apps/screen/screenProtocol";
import { parseMount, type MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import { recordServices } from "../src/features/workspace-apps/screen/screenServices";
import { SlotController } from "../src/features/workspace-apps/screen/screenSlot";

function assert(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }
const token = "a".repeat(64), confirmationToken = "b".repeat(64);
const receipt: MountReceipt = {
  mount: `/api/app-screen/${"c".repeat(64)}`, bridge_nonce: "d".repeat(64), generation: 1,
  tag: "app", action_token: token, tools: [], host_contract: "screen-records.v1",
  operations: ["catalog.call", "catalog.status", "catalog.confirm"], navigation: null,
  install_id: "inst-1", context_id: "ctx-1", digest: `sha256:${"e".repeat(64)}`,
  display_scope_key: "f".repeat(64), display_session_key: "1".repeat(64), remote_images: [],
};
const args = { slot: "lookup", request_id: "synthetic-request-1", input: { body: { query: "example" }, units: 1 } };
const reply = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });

async function main(): Promise<void> {
  assert(parseMount(receipt, "app"), "the mounted app must admit declared catalog operations");
  assert(parseChild({ v: 2, op: "call", id: "call", verb: "catalog.call", args }, receipt.operations ?? [])?.op === "call", "catalog call is a granted call");
  assert(parseChild({ v: 2, op: "call", id: "status", verb: "catalog.status", args }, receipt.operations ?? [])?.op === "call", "catalog status is a granted call");
  assert(parseChild({ v: 2, op: "slot", id: "confirm", verb: "catalog.confirm", args, anchor: "footer" }, receipt.operations ?? [])?.op === "slot", "catalog confirmation is a host slot");
  assert(parseChild({ v: 2, op: "call", id: "forge", verb: "catalog.confirm", args }, receipt.operations ?? []) === null, "a frame cannot invoke confirmation as a call");

  const requests: { url: string; body: Record<string, unknown> }[] = [];
  let malformedPrice = false, loseConfirmation = false, requireConfirmation = false, denyAccess = false, missingCatalog = false, commands = 0;
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    assert(init?.body, "missing request body");
    const body = JSON.parse(String(init.body)) as Record<string, unknown>;
    requests.push({ url, body });
    if (missingCatalog && url.startsWith("/api/app-screen-catalog/")) return reply({ code: "not_found", error: "Not found" }, 404);
    if (url === "/api/app-screen-services/call") return reply({ rows: [] });
    if (denyAccess) return reply({ code: "screen_access_denied", error: "Access denied" }, 403);
    if (requireConfirmation && url.endsWith("/call")) return reply({ code: "confirmation_required", error: "Confirm" }, 409);
    if (url.endsWith("/prepare")) return reply({ confirmation_token: confirmationToken, card: { operation: "catalog.confirm", facts: {
      tool: "Synthetic lookup", slug: "synthetic.lookup", effect: "read",
      price: { currency: "USD", scale: 6, amount: malformedPrice ? "unknown" : "0.123456" }, input: args.input,
    } } });
    if (url.endsWith("/confirm")) {
      if (loseConfirmation) throw new Error("reply lost after possible commit");
      return reply({ slug: "synthetic.lookup", effect: "read", price: { currency: "USD", scale: 6, amount: "0.123456" }, result: { value: "done", action_token: "hidden" } });
    }
    if (url.endsWith("/status")) return reply({ state: "settled", result_available: false });
    return reply({ slug: "synthetic.lookup", effect: "read", price: { currency: "USD", scale: 6, amount: "0.000000" }, result: { value: "free", action_token: "hidden" }, host_command: { operation: "screen.route.patch", args: {} } });
  }) as typeof fetch;
  const host = { current: () => true, refused: () => {}, command: () => { commands += 1; } };
  const services = recordServices(receipt, "synthetic-catalog", host);
  const free = await services.call("catalog.call" as RecordCallVerb, args);
  assert(free.ok && !JSON.stringify(free).includes("hidden"), "free result is returned without authority fields");
  assert(commands === 0 && !JSON.stringify(free).includes("host_command"), "provider data cannot direct the host");
  assert(requests[0]?.url === "/api/app-screen-catalog/call", "free call reaches the catalog endpoint");
  assert(requests[0]?.body.action_token === token && requests[0]?.body.slot === args.slot, "the host supplies the mount token and slot");
  const beforeForged = requests.length;
  const forged = await services.call("catalog.call" as RecordCallVerb, { ...args, action_token: "forged" });
  assert(!forged.ok && requests.length === beforeForged, "a frame cannot override the host token");
  const accountOverride = await services.call("catalog.call" as RecordCallVerb, { ...args, input: { body: { nested: [{ connection_id: "forged" }] } } });
  assert(!accountOverride.ok && requests.length === beforeForged, "a frame cannot choose the host account, even in nested input");
  requireConfirmation = true;
  const paid = await services.call("catalog.call" as RecordCallVerb, args);
  assert(!paid.ok && paid.refusal.code === "confirmation_required", "paid calls tell the frame to request host confirmation");
  requireConfirmation = false;
  const status = await services.call("catalog.status" as RecordCallVerb, args);
  assert(status.ok && requests.at(-1)?.url === "/api/app-screen-catalog/status", "status reconciles the same request");
  assert(requests.at(-1)?.body.request_id === args.request_id && JSON.stringify(requests.at(-1)?.body.input) === JSON.stringify(args.input), "status keeps the original request id and input");

  const plan = await services.planner("catalog.confirm" as RecordSlotVerb, args);
  assert("run" in plan && plan.recordCard, "confirmation requires a host card");
  const facts = JSON.stringify(plan.recordCard);
  assert(facts.includes("Synthetic lookup") && facts.includes("0.123456") && facts.includes("example"), "host card shows the exact quoted tool, price and input");
  assert(!facts.includes(confirmationToken), "confirmation token never reaches the card");
  assert(requests.at(-1)?.url === "/api/app-screen-catalog/prepare", "confirmation is prepared by the host");
  const confirmed = await plan.run();
  assert(confirmed.ok && !JSON.stringify(confirmed).includes("hidden"), "confirmed result hides authority fields");
  assert(requests.at(-1)?.url === "/api/app-screen-catalog/confirm" && requests.at(-1)?.body.confirmation_token === confirmationToken, "only the host sends the prepared token");
  const count = requests.length;
  const repeated = await plan.run();
  assert(!repeated.ok && requests.length === count, "a confirmation cannot be replayed");

  malformedPrice = true;
  const invalid = await services.planner("catalog.confirm" as RecordSlotVerb, args);
  assert(!("run" in invalid), "an invalid quoted price cannot be confirmed");
  malformedPrice = false;
  loseConfirmation = true;
  const uncertainPlan = await services.planner("catalog.confirm" as RecordSlotVerb, args);
  assert("run" in uncertainPlan, "a second independent intent can be prepared");
  const uncertain = await uncertainPlan.run();
  assert(!uncertain.ok && uncertain.refusal.code === "outcome_uncertain", "a lost confirmation reply stays uncertain");
  const afterLoss = requests.length;
  await uncertainPlan.run();
  assert(requests.length === afterLoss, "a lost confirmation is never retried automatically");

  let current = true;
  const revocable = recordServices(receipt, "revocable-catalog", { current: () => current, refused: () => {}, command: () => {} });
  const revokedPlan = await revocable.planner("catalog.confirm" as RecordSlotVerb, args);
  assert("run" in revokedPlan, "a live mount can prepare confirmation");
  current = false;
  const beforeRevoked = requests.length;
  const revoked = await revokedPlan.run();
  assert(!revoked.ok && requests.length === beforeRevoked, "a revoked mount cannot confirm");

  let refused = 0;
  const deniedServices = recordServices(receipt, "denied-catalog", { current: () => true, refused: () => { refused += 1; }, command: () => {} });
  denyAccess = true;
  const denied = await deniedServices.call("catalog.status" as RecordCallVerb, args);
  assert(!denied.ok && denied.refusal.code === "forbidden" && refused === 1, "server authorization denial retires the mount");
  const afterDenial = requests.length;
  await deniedServices.call("catalog.status" as RecordCallVerb, args);
  assert(requests.length === afterDenial, "a denied mount cannot keep calling");

  denyAccess = false;
  loseConfirmation = false;
  let now = 0;
  const slots = new SlotController(() => {}, () => {}, (verb, input) => services.planner(verb as RecordSlotVerb, input), () => now);
  await slots.request({ v: 2, op: "slot", id: "paid", verb: "catalog.confirm", args, anchor: "footer" });
  const view = slots.view();
  assert(view?.recordCard, "the catalog confirmation is drawn by the host");
  now = 1000;
  const beforeTap = requests.length;
  assert(!slots.tap(view.token, false, now) && requests.length === beforeTap, "an untrusted frame event cannot approve");
  assert(slots.tap(view.token, true, now), "a trusted host press can approve");
  assert(requests.at(-1)?.url === "/api/app-screen-catalog/confirm", "only the approved host press confirms");
  slots.close();

  missingCatalog = true;
  let oldWorkerRefused = 0;
  const oldWorker = recordServices({ ...receipt, operations: [...receipt.operations!, "records.list"] }, "old-worker", {
    current: () => true, refused: () => { oldWorkerRefused += 1; }, command: () => {},
  });
  const missing = await oldWorker.call("catalog.status", args);
  const stillWorks = await oldWorker.call("records.list", {});
  assert(!missing.ok && missing.refusal.code === "not_found" && stillWorks.ok && oldWorkerRefused === 0, "an older worker refuses catalog without retiring unrelated operations");
}
void main().catch(error => { throw error; });
