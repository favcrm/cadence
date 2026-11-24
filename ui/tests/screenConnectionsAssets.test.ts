import {
  parseChild,
  recordArgsLimit,
  RECORD_CALL_VERBS,
  RECORD_SLOT_VERBS,
  type ScreenPush,
} from "../src/features/workspace-apps/screen/screenProtocol";
import { parseMount, ScreenChannel, type MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import { READ_ONLY_OPERATIONS, readOnlyReceipt } from "../src/features/workspace-apps/screen/screenReadOnly";
import { admitDisplaySession, withReadObservations } from "../src/features/workspace-apps/screen/screenDisplayCache";
import {
  hostedConnectCommandUrl,
  recordServices,
  SCREEN_READ_VERBS,
  type HostCommand,
} from "../src/features/workspace-apps/screen/screenServices";

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

  // CAD-2734: `connections.connect` is an owner-only CALL verb (never a SLOT or read-only verb)
  const CONNECT_VERB = "connections.connect" as const;
  assert.ok((RECORD_CALL_VERBS as readonly string[]).includes(CONNECT_VERB), `${CONNECT_VERB} is a CALL verb`);
  assert.equal((RECORD_SLOT_VERBS as readonly string[]).includes(CONNECT_VERB), false, `${CONNECT_VERB} is never a SLOT`);
  assert.equal(SCREEN_READ_VERBS.has(CONNECT_VERB), false, `${CONNECT_VERB} is not in SCREEN_READ_VERBS`);
  assert.equal(READ_ONLY_OPERATIONS.includes(CONNECT_VERB), false, `${CONNECT_VERB} is not in READ_ONLY_OPERATIONS`);
  assert.equal(recordArgsLimit(CONNECT_VERB), 16384, "connections.connect keeps the default argument limit");

  const connectReceipt: MountReceipt = { ...receipt, operations: ["records.list", CONNECT_VERB] };
  assert.equal(
    JSON.stringify(readOnlyReceipt(connectReceipt).operations),
    JSON.stringify(["records.list"]),
    "a read-only mount strips connections.connect",
  );
  const parsedConnect = parseChild(
    { v: 2, op: "call", id: "k2", verb: CONNECT_VERB, args: { toolkit: "metaads", section: "accounts" } },
    [CONNECT_VERB],
  );
  assert.ok(parsedConnect !== null && parsedConnect.op === "call" && parsedConnect.verb === CONNECT_VERB, "connections.connect parses as a granted call");

  // hostedConnectCommandUrl accepts https URLs and refuses non-https, extra keys, or controls.
  assert.equal(hostedConnectCommandUrl({ url: "https://connect.composio.dev/link/123" }), "https://connect.composio.dev/link/123");
  assert.equal(hostedConnectCommandUrl({ url: "http://connect.composio.dev/link/123" }), null, "http URL refused");
  assert.equal(hostedConnectCommandUrl({ url: "javascript:alert(1)" }), null, "javascript URL refused");
  assert.equal(hostedConnectCommandUrl({ url: "https://example.com/a\nb" }), null, "control char refused");
  assert.equal(hostedConnectCommandUrl({ url: "https://example.com/", extra: 1 }), null, "extra key refused");

  const hostedRedirectUrl = "https://connect.composio.dev/link/metaads-session";
  globalThis.fetch = ((url: string, init?: RequestInit) => {
    assert.equal(url, "/api/app-screen-services/call", "connections.connect uses service call route");
    const body = JSON.parse(String(init?.body));
    requests.push({ operation: body.operation, args: body.args });
    return Promise.resolve(
      new Response(
        JSON.stringify({
          started: true,
          toolkit: "metaads",
          host_command: { operation: CONNECT_VERB, args: { url: hostedRedirectUrl } },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );
  }) as typeof fetch;

  const hostCommands: HostCommand[] = [];
  const connectServices = recordServices(connectReceipt, "ads-connect-domain", {
    current: () => true,
    refused: () => { refused += 1; },
    command: c => { hostCommands.push(c); },
  });
  const connectReply = await connectServices.call(CONNECT_VERB, { toolkit: "metaads", section: "accounts" });
  if (!connectReply.ok) throw new Error("connections.connect must resolve ok");
  assert.equal(hostCommands.length, 1, "host_command delivered to host callback");
  assert.equal(hostCommands[0].operation, CONNECT_VERB, "host_command names connections.connect");
  assert.equal(hostCommands[0].args.url, hostedRedirectUrl, "host_command carries the https URL");
  const stripped = connectReply.data as Record<string, unknown>;
  assert.equal("host_command" in stripped, false, "host_command is stripped before reaching the iframe");
  assert.equal(refused, 0, "the connect mount is not retired");

  // CAD-2751: `connections.ads.campaigns` is an owner-only CALL verb (never a SLOT,
  // never a read-only mount verb, and excluded from observeRead display cache).
  const ADS_CAMPAIGNS_VERB = "connections.ads.campaigns" as const;
  assert.ok((RECORD_CALL_VERBS as readonly string[]).includes(ADS_CAMPAIGNS_VERB), `${ADS_CAMPAIGNS_VERB} is a CALL verb`);
  assert.equal((RECORD_SLOT_VERBS as readonly string[]).includes(ADS_CAMPAIGNS_VERB), false, `${ADS_CAMPAIGNS_VERB} is never a SLOT`);
  assert.equal(SCREEN_READ_VERBS.has(ADS_CAMPAIGNS_VERB), false, `${ADS_CAMPAIGNS_VERB} is not in SCREEN_READ_VERBS`);
  assert.equal(READ_ONLY_OPERATIONS.includes(ADS_CAMPAIGNS_VERB), false, `${ADS_CAMPAIGNS_VERB} is not in READ_ONLY_OPERATIONS`);
  assert.equal(recordArgsLimit(ADS_CAMPAIGNS_VERB), 16384, "connections.ads.campaigns keeps the default argument limit");

  const adsReceipt: MountReceipt = { ...receipt, operations: ["records.list", ADS_CAMPAIGNS_VERB] };
  assert.equal(
    JSON.stringify(readOnlyReceipt(adsReceipt).operations),
    JSON.stringify(["records.list"]),
    "a read-only mount strips connections.ads.campaigns",
  );
  const parsedAdsCall = parseChild(
    { v: 2, op: "call", id: "k3", verb: ADS_CAMPAIGNS_VERB, args: { toolkit: "metaads", connection_id: "conn_1", account_id: "act_123" } },
    [ADS_CAMPAIGNS_VERB],
  );
  assert.ok(parsedAdsCall !== null && parsedAdsCall.op === "call" && parsedAdsCall.verb === ADS_CAMPAIGNS_VERB, "connections.ads.campaigns parses as a granted call");

  admitDisplaySession(receipt.display_session_key!);
  globalThis.fetch = ((url: string, init?: RequestInit) => {
    assert.equal(url, "/api/app-screen-services/call", "connections.ads.campaigns uses service call route");
    const body = JSON.parse(String(init?.body));
    requests.push({ operation: body.operation, args: body.args });
    return Promise.resolve(
      new Response(
        JSON.stringify({ toolkit: "metaads", status: "ok", campaigns: [] }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );
  }) as typeof fetch;

  const adsServices = recordServices(adsReceipt, "ads-import-domain", {
    current: () => true,
    refused: () => { refused += 1; },
    command: () => {},
  });
  const adsReply = await adsServices.call(ADS_CAMPAIGNS_VERB, { toolkit: "metaads", connection_id: "conn_1", account_id: "act_123" });
  assert.ok(adsReply.ok, "connections.ads.campaigns call resolves ok");
  const observed = withReadObservations(projection, receipt.display_scope_key!, [ADS_CAMPAIGNS_VERB]);
  assert.equal(observed.read_observations?.length ?? 0, 0, "connections.ads.campaigns must not enter observeRead display cache");
  assert.equal(refused, 0, "the ads import mount is not retired");

  // CAD-2769: allowlisted connections.* host errors pass through sanitized { code, text }
  // without retiring the active mount; board_session_required (401) still retires it.
  const stubRefusal = (status: number, code: string, message: string) => {
    globalThis.fetch = (() =>
      Promise.resolve(
        new Response(JSON.stringify({ ok: false, error: { code, message } }), {
          status,
          headers: { "Content-Type": "application/json" },
        }),
      )) as typeof fetch;
  };
  const nonFatalCases: Array<{ status: number; code: string; message: string }> = [
    { status: 503, code: "upstream_unavailable", message: "The ad platform is temporarily unavailable." },
    { status: 502, code: "upstream_rejected", message: "The ad platform rejected the campaign read request." },
    { status: 429, code: "rate_limited", message: "The ad platform rate limit was reached. Retry shortly." },
    { status: 401, code: "needs_reconnect", message: "Reconnect your ad platform to continue importing campaigns." },
    { status: 403, code: "account_not_authorized", message: "The ad platform denied access to this ad account." },
  ];
  for (const c of nonFatalCases) {
    stubRefusal(c.status, c.code, c.message);
    const errReply = await adsServices.call(ADS_CAMPAIGNS_VERB, { toolkit: "metaads", connection_id: "conn_1", account_id: "act_123" });
    assert.equal(errReply.ok, false, `${c.code} returns failure`);
    if (errReply.ok) throw new Error("expected failure");
    assert.equal(errReply.refusal.code, c.code, `${c.code} code passes through`);
    assert.equal(errReply.refusal.text, c.message, `${c.code} message passes through`);
    assert.equal(refused, 0, `${c.code} keeps mount alive`);
  }

  stubRefusal(401, "board_session_required", "A board session is required.");
  const sessionLost = await adsServices.call(ADS_CAMPAIGNS_VERB, { toolkit: "metaads", connection_id: "conn_1", account_id: "act_123" });
  assert.equal(sessionLost.ok, false, "board_session_required returns failure");
  if (sessionLost.ok) throw new Error("expected failure");
  assert.equal(sessionLost.refusal.code, "unauthorized", "board_session_required maps to unauthorized");
  assert.equal(refused, 1, "board_session_required retires the mount");
}

run().catch(error => { throw error; });
