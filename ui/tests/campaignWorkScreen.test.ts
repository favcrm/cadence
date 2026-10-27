import { parseChild, RECORD_CALL_VERBS, RECORD_OPERATIONS, RECORD_SLOT_VERBS, type RecordSlotVerb, type ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import { parseMount, ScreenChannel, type MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import { recordServices, type HostCommand } from "../src/features/workspace-apps/screen/screenServices";
import type { ActionArgs } from "../src/features/workspace-apps/screen/screenProtocol";

// CAD-1827: the merged CRM package declares `campaign-work.show` and
// `campaign-work.save`, and the host mints its mount receipt from that
// declaration. Before this fix the browser bridge did not admit either verb,
// so the receipt was invalid (`Invalid screen mount receipt`) and every work
// call was rejected — the daemon tests could not see it because they drive the
// relay directly. This file exercises the real receipt parser, the real child
// call parser and the real record services against a receipt shaped exactly as
// the host mints it for that package.
//
// Only `fetch` is supplied by this fixture; it is input, not authority. The
// command sink is a recorder, not a renderer. No browser or network runs.
const assert = {
  equal(actual: unknown, expected: unknown, why = "values must match"): void {
    if (actual !== expected) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
  ok(value: unknown, why = "condition must hold"): void { if (!value) throw new Error(why); },
};

/** One checked field of a JSON-shaped value. `unknown` stays unknown until a
 *  caller narrows it, so no member access trusts an unchecked shape. */
function field(value: unknown, key: string): unknown {
  if (value === null || typeof value !== "object") return undefined;
  // Named const, checked just above: an object read by one string key.
  const row = value as Record<string, unknown>;
  return row[key];
}

/** The refusal code of an ActionResult, or undefined for an ok result. */
function refusalCode(result: unknown): unknown {
  return field(field(result, "refusal"), "code");
}

const SHOW = "campaign-work.show" as const;
const SAVE = "campaign-work.save" as const;
const TOKEN = "a".repeat(64);

// The operations the merged CRM bundle declares for this contract, as the
// host's package reader admits them.
const PACKAGE_OPERATIONS: readonly string[] = [SHOW, SAVE];
const receipt: MountReceipt = {
  mount: `/api/app-screen/${"b".repeat(64)}`,
  bridge_nonce: "c".repeat(64),
  generation: 1,
  tag: "crm",
  action_token: TOKEN,
  tools: [],
  host_contract: "screen-records.v1",
  operations: [...PACKAGE_OPERATIONS],
  navigation: null,
  install_id: "inst-1",
  context_id: "ctx-1",
  digest: `sha256:${"d".repeat(64)}`,
  display_scope_key: "e".repeat(64),
  display_session_key: "f".repeat(64),
  remote_images: [],
};

// The closed vocabularies admit both verbs, and nothing invented does.
for (const verb of PACKAGE_OPERATIONS) {
  assert.ok((RECORD_CALL_VERBS as readonly string[]).includes(verb), `${verb} is a CALL verb`);
  assert.ok(RECORD_OPERATIONS.includes(verb), `${verb} is a records operation`);
  assert.equal((RECORD_SLOT_VERBS as readonly string[]).includes(verb), false, `${verb} is never a SLOT`);
}
assert.equal(RECORD_OPERATIONS.includes("campaign-work.send"), false, "an invented campaign verb stays out");

// The receipt the host mints for this package parses — the mount is valid.
const mounted = parseMount(receipt, "crm");
assert.ok(mounted, "the campaign-work receipt must parse (fail-closed invalid_receipt otherwise)");
assert.equal(mounted?.host_contract, "screen-records.v1");
assert.ok(mounted?.operations?.includes(SHOW), "receipt retains the declared show operation");

// A receipt that grew an operation the bridge does not know still refuses.
assert.equal(parseMount({ ...receipt, operations: [...PACKAGE_OPERATIONS, "campaign-work.approve"] }, "crm"), null,
  "an unknown operation keeps the receipt invalid");

// The child call parser admits both verbs for the granted records frame.
for (const verb of PACKAGE_OPERATIONS) {
  const parsed = parseChild({ v: 2, op: "call", id: "k1", verb, args: { campaign_id: "welcome" } }, PACKAGE_OPERATIONS);
  assert.ok(parsed !== null && parsed.v === 2 && parsed.op === "call" && parsed.verb === verb, `${verb} parses as a granted call`);
}
// An operation the receipt did not grant is refused even if it is known.
assert.equal(parseChild({ v: 2, op: "call", id: "k1", verb: SHOW, args: {} }, [SAVE]), null,
  "a call outside the receipt's granted set is refused");

// The lifecycle call gate is the third place the verbs had to be admitted:
// a records frame whose receipt grants them hands them to the host actions
// unchanged, while an ungranted verb still closes the mount.
class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  closed = false;
  start(): void {}
  close(): void { this.closed = true; }
  postMessage(): void {}
  receive(value: unknown): void { this.onmessage?.({ data: value }); }
}
const lifecycleCalls: string[] = [];
const digest = `sha256:${"d".repeat(64)}`;
const projection: ScreenPush = {
  v: 1, op: "screen", install_id: "inst-1", digest, tag: "crm", context_id: "ctx-1",
  installation: { install_id: "inst-1", digest, title: "CRM", name: "crm", version: "1", summary: "" },
  contexts: [{ id: "ctx-1", label: "context" }], runs: [], outbox: [], now: 1,
  // A v2 push carries the (empty) publish-intent section.
  publish_intents: { status: "loading" as const, withheld: 0, rows: [] },
};
const source = {} as Window;
const port = new Port();
let lifecycleFailed = 0;
const channel = new ScreenChannel(source, receipt, projection, () => {}, () => { lifecycleFailed += 1; }, () => {},
  async () => null, {
    call: async verb => { lifecycleCalls.push(verb); return { ok: true, data: null }; },
    planner: async () => ({ code: "forbidden", text: "not a slot" }),
    onSlot: () => {},
    onLink: () => {},
  });
channel.receive({
  source, origin: "null",
  data: { v: 1, op: "init", tag: "crm", bridge_nonce: receipt.bridge_nonce, generation: 1 },
  ports: [port as unknown as MessagePort],
});
port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1"] });
assert.equal(lifecycleFailed, 0, "the paired records frame accepts its ready");
port.receive({ v: 2, op: "call", id: "life-1", verb: SHOW, args: { campaign_id: "welcome" } });
assert.equal(lifecycleCalls[0], SHOW, "the lifecycle gate admits the work read");
port.receive({ v: 2, op: "call", id: "life-2", verb: SAVE, args: { campaign_id: "welcome", expected_revision: 0, assignee: "Ada" } });
assert.equal(lifecycleCalls[1], SAVE, "the lifecycle gate admits the work write");
assert.equal(port.closed, false, "an admitted call keeps the mount");
port.receive({ v: 2, op: "call", id: "life-3", verb: "records.update", args: {} });
assert.equal(port.closed, true, "an ungranted call still closes the mount");

type Reply = { status: number; body: unknown };
type Deferred<T> = { promise: Promise<T>; resolve: (value: T) => void };

/** A promise the test settles itself. The executor form is the only typed
 *  spelling here: `tsconfig.tests.json` targets the ES2023 lib, which does
 *  not declare `Promise.withResolvers`. */
function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(settle => { resolve = settle; });
  return { promise, resolve };
}
type Request = { operation: string; args: ActionArgs; reply: (reply: Reply) => void };

function fixture() {
  const requests: Request[] = [];
  const commands: HostCommand[] = [];
  let refused = 0;
  let current = true;
  globalThis.fetch = ((url: string, init?: RequestInit) => {
    assert.equal(url, "/api/app-screen-services/call", "only the owned service-call route");
    assert.ok(init, "request init is required");
    if (!init) throw new Error("Missing request init");
    assert.equal(init.method, "POST");
    assert.equal(init.credentials, "same-origin");
    const body = JSON.parse(String(init.body));
    assert.equal(body.action_token, TOKEN);
    const reply = deferred<Response>();
    requests.push({
      operation: body.operation,
      args: body.args,
      reply: answer => reply.resolve(new Response(JSON.stringify(answer.body), {
        status: answer.status, headers: { "Content-Type": "application/json" },
      })),
    });
    return reply.promise;
  }) as typeof fetch;
  const services = recordServices(receipt, "crm-work-domain", {
    current: () => current,
    refused: () => { refused += 1; },
    command: command => { commands.push(command); },
  });
  return {
    services,
    requests,
    commands,
    refused: () => refused,
    retire: () => { current = false; },
    restore: () => { current = true; },
  };
}

async function run(): Promise<void> {
  // A read and a write both reach the real service path.
  const fx = fixture();
  const show = fx.services.call(SHOW, { campaign_id: "welcome" });
  assert.equal(fx.requests.length, 1, "show reaches the bridge");
  assert.equal(fx.requests[0].operation, SHOW);
  fx.requests[0].reply({ status: 200, body: {
    contract: "crm-campaign-work/v1",
    source: { app: "crm", install_id: "inst-1", context_id: "ctx-1" },
    campaign_id: "welcome", work: null, content: null,
  } });
  const shown = await show;
  assert.ok(shown.ok, "a served show is an ok result");
  assert.equal(field(shown.ok ? shown.data : null, "contract"), "crm-campaign-work/v1");

  const save = fx.services.call(SAVE, {
    campaign_id: "welcome", expected_revision: 0, assignee: "Ada",
  });
  assert.equal(fx.requests.length, 2, "save reaches the bridge");
  assert.equal(fx.requests[1].operation, SAVE);
  fx.requests[1].reply({ status: 200, body: { work: { revision: 1 } } });
  const saved = await save;
  assert.ok(saved.ok, "a served save is an ok result");

  // A stale save keeps the host's own code: the app's conflict re-read is
  // keyed on `stale_revision`, not on a generic conflict.
  const stale = fx.services.call(SAVE, { campaign_id: "welcome", expected_revision: 0, due_at: "2026-12-01" });
  fx.requests[2].reply({ status: 409, body: { code: "stale_revision", text: "These campaign work details changed." } });
  const refusedSave = await stale;
  assert.equal(refusedSave.ok, false, "a stale save is refused");
  assert.equal(refusalCode(refusedSave), "stale_revision", "the stale code survives the bridge");

  // A member's save is refused by the host (403): the bridge reports
  // `forbidden` and retires the owned mount, exactly like every other loss.
  const member = fixture();
  const memberSave = member.services.call(SAVE, { campaign_id: "welcome", expected_revision: 1, due_at: "2026-12-02" });
  member.requests[0].reply({ status: 403, body: { code: "forbidden", text: "screen service requires operator-role public session" } });
  const denied = await memberSave;
  assert.equal(denied.ok, false, "a member save is refused");
  assert.equal(refusalCode(denied), "forbidden", "the member refusal is forbidden, not a generic failure");
  assert.equal(member.refused(), 1, "a lost mount is reported to the host");
  assert.equal(member.commands.length, 0, "no host command is fabricated");

  // Save is a direct CALL, never a SLOT: a planner request is refused
  // without reaching the wire. The cast asks the planner to treat a CALL
  // verb as a slot; the closed slot set must still refuse it.
  const asSlot = SAVE as unknown as RecordSlotVerb;
  const planner = await fx.services.planner(asSlot, { campaign_id: "welcome", expected_revision: 1 });
  assert.equal(field(planner, "code"), "forbidden", "save is not a confirmation slot");
  assert.equal(fx.requests.length, 3, "a refused planner never reaches the wire");

  // A retired mount refuses reads too.
  const gone = fixture();
  gone.retire();
  const afterRetire = await gone.services.call(SHOW, { campaign_id: "welcome" });
  assert.equal(afterRetire.ok, false, "a retired mount refuses show");
  assert.equal(gone.requests.length, 0, "a retired mount never reaches the wire");
  gone.restore();
}

run().catch(error => { throw error; });
