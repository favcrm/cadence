import { parseChild, RECORD_CALL_VERBS, RECORD_SLOT_VERBS, type ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import { parseMount, ScreenChannel, type MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import { recordServices } from "../src/features/workspace-apps/screen/screenServices";

// CAD-2263: a screen declaring `records.batch` and `records.aggregate` must
// mount, and the board must forward both calls to the hosted worker instead of
// closing the frame. The hosted release check refuses a worker whose verbs the
// board lacks, so the call lists are the contract under test. Only `fetch` is
// supplied; it stands for the worker and is input, not authority.
const assert = {
  equal(actual: unknown, expected: unknown, why = "values must match"): void {
    if (actual !== expected) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
  ok(value: unknown, why = "condition must hold"): void { if (!value) throw new Error(why); },
};

const BATCH = "records.batch" as const;
const AGGREGATE = "records.aggregate" as const;
const VERBS: readonly string[] = [BATCH, AGGREGATE];
const TOKEN = "a".repeat(64);
const receipt: MountReceipt = {
  mount: `/api/app-screen/${"b".repeat(64)}`,
  bridge_nonce: "c".repeat(64),
  generation: 1,
  tag: "app",
  action_token: TOKEN,
  tools: [],
  host_contract: "screen-records.v1",
  operations: [...VERBS],
  navigation: null,
  install_id: "inst-1",
  context_id: "ctx-1",
  digest: `sha256:${"d".repeat(64)}`,
  display_scope_key: "e".repeat(64),
  display_session_key: "f".repeat(64),
  remote_images: [],
};

for (const verb of VERBS) {
  assert.ok((RECORD_CALL_VERBS as readonly string[]).includes(verb), `${verb} is a CALL verb`);
  assert.equal((RECORD_SLOT_VERBS as readonly string[]).includes(verb), false, `${verb} is never a SLOT`);
}
assert.ok(parseMount(receipt, "app"), "a receipt listing both verbs parses");
assert.equal(parseMount({ ...receipt, operations: [...VERBS, "records.truncate"] }, "app"), null, "an unknown verb still refuses the receipt");
for (const verb of VERBS) {
  const parsed = parseChild({ v: 2, op: "call", id: "k1", verb, args: { table: "t" } }, VERBS);
  assert.ok(parsed !== null && parsed.op === "call" && parsed.verb === verb, `${verb} parses as a granted call`);
}

class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  closed = false;
  start(): void {}
  close(): void { this.closed = true; }
  postMessage(): void {}
  receive(value: unknown): void { this.onmessage?.({ data: value }); }
}
const calls: string[] = [];
const digest = `sha256:${"d".repeat(64)}`;
const projection: ScreenPush = {
  v: 1, op: "screen", install_id: "inst-1", digest, tag: "app", context_id: "ctx-1",
  installation: { install_id: "inst-1", digest, title: "App", name: "app", version: "1", summary: "" },
  contexts: [{ id: "ctx-1", label: "context" }], runs: [], outbox: [], now: 1,
  publish_intents: { status: "loading" as const, withheld: 0, rows: [] },
};
const source = {} as Window;
const port = new Port();
const channel = new ScreenChannel(source, receipt, projection, () => {}, () => {}, () => {},
  async () => null, {
    call: async verb => { calls.push(verb); return { ok: true, data: null }; },
    planner: async () => ({ code: "forbidden", text: "not a slot" }),
    onSlot: () => {},
    onLink: () => {},
  });
channel.receive({
  source, origin: "null",
  data: { v: 1, op: "init", tag: "app", bridge_nonce: receipt.bridge_nonce, generation: 1 },
  ports: [port as unknown as MessagePort],
});
port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1"] });
port.receive({ v: 2, op: "call", id: "life-1", verb: BATCH, args: { table: "t", rows: [] } });
port.receive({ v: 2, op: "call", id: "life-2", verb: AGGREGATE, args: { table: "t" } });
assert.equal(calls.join(","), `${BATCH},${AGGREGATE}`, "both verbs are handed to the host actions");
assert.equal(port.closed, false, "an admitted call keeps the mount");
port.receive({ v: 2, op: "call", id: "life-3", verb: "records.update", args: {} });
assert.equal(port.closed, true, "a verb the receipt does not list still closes the mount");

async function run(): Promise<void> {
  const requests: { operation: string }[] = [];
  globalThis.fetch = ((url: string, init?: RequestInit) => {
    assert.equal(url, "/api/app-screen-services/call", "only the owned service-call route");
    if (!init) throw new Error("Missing request init");
    const body = JSON.parse(String(init.body));
    requests.push({ operation: body.operation });
    return Promise.resolve(new Response(JSON.stringify({ rows: [] }), { status: 200, headers: { "Content-Type": "application/json" } }));
  }) as typeof fetch;
  let refused = 0;
  const services = recordServices(receipt, "records-domain", { current: () => true, refused: () => { refused += 1; }, command: () => {} });
  const batch = await services.call(BATCH, { table: "t", rows: [] });
  const aggregate = await services.call(AGGREGATE, { table: "t" });
  assert.ok(batch.ok && aggregate.ok, "both verbs are served through the call route");
  assert.equal(requests.map(r => r.operation).join(","), `${BATCH},${AGGREGATE}`, "both verbs reach the worker");
  assert.equal(refused, 0, "the mount is not retired");
}

run().catch(error => { throw error; });
