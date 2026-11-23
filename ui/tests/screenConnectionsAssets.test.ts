import {
  parseChild,
  recordArgsLimit,
  RECORD_CALL_VERBS,
  RECORD_SLOT_VERBS,
  type ScreenPush,
} from "../src/features/workspace-apps/screen/screenProtocol";
import { parseMount, ScreenChannel, type MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import { READ_ONLY_OPERATIONS, readOnlyReceipt } from "../src/features/workspace-apps/screen/screenReadOnly";
import { recordServices, SCREEN_READ_VERBS } from "../src/features/workspace-apps/screen/screenServices";

// CAD-2706: `connections.assets` is an owner-only CALL verb (not a SLOT, and
// excluded from the read-only screen allowlist). Only `fetch` is supplied to
// stand in for the hosted worker.
const assert = {
  equal(actual: unknown, expected: unknown, why = "values must match"): void {
    if (actual !== expected) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
  ok(value: unknown, why = "condition must hold"): void {
    if (!value) throw new Error(why);
  },
};

const VERB = "connections.assets" as const;
const TOKEN = "a".repeat(64);
const receipt: MountReceipt = {
  mount: `/api/app-screen/${"b".repeat(64)}`,
  bridge_nonce: "c".repeat(64),
  generation: 1,
  tag: "ads-studio",
  action_token: TOKEN,
  tools: [],
  host_contract: "screen-records.v1",
  operations: ["records.list", VERB],
  navigation: null,
  install_id: "inst-ads",
  context_id: "ctx-1",
  digest: `sha256:${"d".repeat(64)}`,
  display_scope_key: "e".repeat(64),
  display_session_key: "f".repeat(64),
  remote_images: [],
};

assert.ok((RECORD_CALL_VERBS as readonly string[]).includes(VERB), `${VERB} is a CALL verb`);
assert.equal((RECORD_SLOT_VERBS as readonly string[]).includes(VERB), false, `${VERB} is never a SLOT`);
assert.equal(SCREEN_READ_VERBS.has(VERB), false, `${VERB} is not in SCREEN_READ_VERBS`);
assert.equal(READ_ONLY_OPERATIONS.includes(VERB), false, `${VERB} is not in READ_ONLY_OPERATIONS`);
assert.ok(parseMount(receipt, "ads-studio"), "a receipt listing connections.assets parses");
assert.equal(
  JSON.stringify(readOnlyReceipt(receipt).operations),
  JSON.stringify(["records.list"]),
  "a read-only mount strips connections.assets",
);
assert.equal(recordArgsLimit(VERB), 16384, "connections.assets keeps the default argument limit");

const parsed = parseChild(
  { v: 2, op: "call", id: "k1", verb: VERB, args: { toolkit: "metaads" } },
  [VERB],
);
assert.ok(parsed !== null && parsed.op === "call" && parsed.verb === VERB, "connections.assets parses as a granted call");

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
  v: 1, op: "screen", install_id: "inst-ads", digest, tag: "ads-studio", context_id: "ctx-1",
  installation: { install_id: "inst-ads", digest, title: "Ads Studio", name: "ads-studio", version: "0.5.0", summary: "" },
  contexts: [{ id: "ctx-1", label: "context" }], runs: [], outbox: [], now: 1,
  publish_intents: { status: "loading" as const, withheld: 0, rows: [] },
};
const source = {} as Window;
const port = new Port();
const channel = new ScreenChannel(
  source,
  receipt,
  projection,
  () => {},
  () => {},
  () => {},
  async () => null,
  {
    call: async verb => { calls.push(verb); return { ok: true, data: null }; },
    planner: async () => ({ code: "forbidden", text: "not a slot" }),
    onSlot: () => {},
    onLink: () => {},
  },
);
channel.receive({
  source,
  origin: "null",
  data: { v: 1, op: "init", tag: "ads-studio", bridge_nonce: receipt.bridge_nonce, generation: 1 },
  ports: [port as unknown as MessagePort],
});
port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1"] });
port.receive({ v: 2, op: "call", id: "life-1", verb: VERB, args: { toolkit: "metaads" } });
assert.equal(calls.join(","), VERB, "connections.assets reaches the host action callback");
assert.equal(port.closed, false, "an admitted connections.assets call keeps the mount open");

async function run(): Promise<void> {
  const requests: { operation: string; args: Record<string, unknown> }[] = [];
  globalThis.fetch = ((url: string, init?: RequestInit) => {
    assert.equal(url, "/api/app-screen-services/call", "only the owned service-call route");
    if (!init) throw new Error("Missing request init");
    const body = JSON.parse(String(init.body));
    requests.push({ operation: body.operation, args: body.args });
    return Promise.resolve(
      new Response(JSON.stringify({ toolkit: "metaads", connected: false, connections: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
  }) as typeof fetch;

  let refused = 0;
  const services = recordServices(receipt, "ads-domain", {
    current: () => true,
    refused: () => { refused += 1; },
    command: () => {},
  });
  const reply = await services.call(VERB, { toolkit: "metaads" });
  assert.ok(reply.ok, "connections.assets call resolves cleanly");
  assert.equal(requests.length, 1, "one request forwarded to the worker");
  assert.equal(requests[0].operation, VERB, "operation forwarded verbatim");
  assert.equal(requests[0].args.toolkit, "metaads", "toolkit forwarded verbatim");
  assert.equal(refused, 0, "the mount is not retired");
}

run().catch(error => { throw error; });
