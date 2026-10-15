import { recordServices, type HostCommand } from "../src/features/workspace-apps/screen/screenServices";
import type { ActionArgs, ActionResult } from "../src/features/workspace-apps/screen/screenProtocol";
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";

// Independently authored bug/liveness acceptance. Real HOST code, controlled fetch INPUT only.
// This command sink is not a renderer and does not prove server/Native authority or Browser QA.
// No Node typings, external test framework, browser, network or package additions are needed.
const assert = {
  equal(actual: unknown, expected: unknown, why = "values must match"): void {
    if (actual !== expected) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
  deepEqual(actual: unknown, expected: unknown, why = "structures must match"): void {
    const canonical = (value: unknown): string => {
      if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
      if (value !== null && typeof value === "object") return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(",")}}`;
      return JSON.stringify(value) ?? "undefined";
    };
    if (canonical(actual) !== canonical(expected)) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
  ok(value: unknown, why = "condition must hold"): void { if (!value) throw new Error(why); },
};
const savedFetch = globalThis.fetch;
type PreviewOperation = "screen.preview.show" | "screen.preview.clear";
const SHOW = 'screen.preview.show', CLEAR = 'screen.preview.clear';
const idA = 'own-preview-a', idB = 'own-preview-b';
const showArgs = (preview_id = idA, marker = 'initial'): ActionArgs => ({ preview_id,
  document: `<p>${marker}</p>`, anchor: { x: 12, y: 24, w: 320, h: 384 }, mode: 'draft' });
function deferred<T>() {
  let resolvePromise!: (value: T) => void, rejectPromise!: (error: unknown) => void;
  const promise = new Promise<T>((resolve, reject) => { resolvePromise = resolve; rejectPromise = reject; });
  return { promise, resolve: resolvePromise, reject: rejectPromise };
}
type HeldText = { body: ReturnType<typeof deferred<string>>; entered: ReturnType<typeof deferred<void>>; raw: unknown };
type RequestInput = { operation: PreviewOperation; args: ActionArgs; response: ReturnType<typeof deferred<Response>>; answered: boolean; bodyWait: HeldText | null };
type Started = RequestInput & { result: Promise<ActionResult> };
function fixture() {
  const requests: RequestInput[] = [], commands: HostCommand[] = [], results: Promise<ActionResult>[] = [];
  let current = true, display: ActionArgs | null = null, refused = 0;
  const token = 'a'.repeat(64); // Deliberately synthetic, never a live capability.
  globalThis.fetch = (url, init) => {
    assert.equal(url, '/api/app-screen-services/call', 'Only the owned service-call input is allowed');
    assert.ok(init, 'Request init is required');
    if (!init) throw new Error('Missing request init');
    assert.equal(init.method, 'POST');
    assert.equal(init.credentials, 'same-origin');
    assert.equal(init.cache, 'no-store');
    const body = JSON.parse(String(init.body));
    assert.equal(body.action_token, token);
    assert.deepEqual(Object.keys(body).sort(), ['action_token', 'args', 'operation']);
    assert.ok([SHOW, CLEAR].includes(body.operation));
    // No real session header is supplied or read in this Node-only fixture.
    assert.equal(new Headers(init.headers).get('X-Cadence-Session'), null);
    const response = deferred<Response>();
    const request: RequestInput = { operation: body.operation, args: body.args, response, answered: false, bodyWait: null };
    requests.push(request);
    return response.promise;
  };
  const receipt: MountReceipt = {
    mount: `/api/app-screen/${'b'.repeat(64)}`, bridge_nonce: 'c'.repeat(64), generation: 1,
    tag: 'own-preview', action_token: token, tools: [], host_contract: 'screen-records.v1',
    operations: [SHOW, CLEAR], navigation: null, install_id: 'own-install', context_id: 'own-context',
    digest: `sha256:${'d'.repeat(64)}`, display_scope_key: 'e'.repeat(64),
    display_session_key: 'f'.repeat(64), remote_images: [],
  };
  const services = recordServices(receipt, 'own-preview-domain', {
    current: () => current,
    refused: () => { refused += 1; },
    command: command => {
      commands.push(structuredClone(command));
      // Minimal observable command sink only; this is NOT ScreenOutlet/Native validation.
      if (command.operation === SHOW) display = structuredClone(command.args);
      else if (command.operation === CLEAR && display?.preview_id === command.args.preview_id) display = null;
      else assert.equal(command.operation, CLEAR, 'Unexpected host command');
    },
  });
  function start(operation: PreviewOperation, args: ActionArgs): Started {
    const count = requests.length;
    const result = services.call(operation, args);
    results.push(result);
    assert.equal(requests.length, count + 1, 'Positive control: permitted call reaches genuine post/fetch');
    const request = requests[count];
    assert.equal(request.operation, operation);
    assert.deepEqual(request.args, args);
    return Object.assign(request, { result });
  }
  function reply(request: RequestInput, status = 200, pauseText = false, responseRaw?: unknown): HeldText | null {
    assert.equal(request.answered, false, 'Every controlled response is completed once');
    request.answered = true;
    const raw = responseRaw === undefined ? (status === 200
      ? { preview_id: request.args.preview_id, host_command: { operation: request.operation, args: request.args } }
      : { code: 'preview_fixture_refused' }) : responseRaw;
    const response = new Response(JSON.stringify(raw), { status, headers: { 'Content-Type': 'application/json' } });
    if (pauseText) {
      const body = deferred<string>(), entered = deferred<void>();
      request.bodyWait = { body, entered, raw };
      Object.defineProperty(response, 'text', { value: () => { entered.resolve(undefined); return body.promise; } });
    }
    request.response.resolve(response);
    return request.bodyWait;
  }
  async function success(request: Started) {
    reply(request);
    const result = await request.result;
    assert.equal(result.ok, true, 'Positive control: successful genuine call');
    if (!result.ok) throw new Error('Successful fixture reply was refused');
    assert.deepEqual(result.data, { preview_id: request.args.preview_id }, 'Genuine deliver sanitizes host_command from child data');
    return result;
  }
  async function initial() {
    const request = start(SHOW, showArgs());
    await success(request);
    assert.equal(commands.length, 1, 'Positive control: successful show delivered exactly once');
    assert.deepEqual(commands[0], { operation: SHOW, args: request.args });
    assert.deepEqual(display, request.args, 'Command sink records the initial own preview');
  }
  return { requests, commands, services, start, reply, success, initial,
    get display() { return display; }, get refused() { return refused; },
    setNotCurrent() { current = false; },
    snapshot() { return { requests: requests.map(r => ({ operation: r.operation, args: r.args })), commands, display, refused }; },
    async cleanup() {
      current = false; services.retire();
      for (const request of requests) {
        if (!request.answered) { request.answered = true; request.response.reject(new Error('Owned fixture cleanup')); }
        if (request.bodyWait) request.bodyWait.body.resolve(JSON.stringify(request.bodyWait.raw));
      }
      await Promise.allSettled(results);
    },
  };
}
type Fixture = ReturnType<typeof fixture>;
const cases: { name: string; run: (f: Fixture) => Promise<void> }[] = [];
const check = (name: string, run: (f: Fixture) => Promise<void>) => cases.push({ name, run });
for (const failure of ['http400', 'transport']) check(`latest-refusal-${failure}`, async f => {
  await f.initial();
  const request = f.start(SHOW, { ...showArgs(idA, 'latest-refused'), anchor: { x: 12, y: -8, w: 320, h: 384 } });
  if (failure === 'http400') f.reply(request, 400);
  else { request.answered = true; request.response.reject(new TypeError('Owned transport refusal')); }
  const result = await request.result;
  assert.equal(result.ok, false, 'Refusal must not report successful preview readiness');
  if (result.ok) throw new Error('Refusal unexpectedly claimed success');
  assert.equal(result.refusal.code, 'failed', '400/transport is not an authorization retirement');
  assert.equal(f.refused, 0, 'Non-auth refusal must leave the mount live');
  assert.equal(f.requests.length, 2, 'Failure cleanup must be LOCAL, without another fetch/RPC');
  assert.deepEqual(f.commands.slice(1), [{ operation: CLEAR, args: { preview_id: idA } }],
    'Latest failed show MUST issue exactly one same-ID local host.command clear');
  assert.equal(f.display, null, 'Command sink must receive removal of its existing matching preview');
});
check('late-old-failure-after-new-success', async f => {
  await f.initial();
  const old = f.start(SHOW, showArgs(idA, 'old-pending'));
  const newer = f.start(SHOW, showArgs(idA, 'newest-success'));
  await f.success(newer);
  assert.deepEqual(f.display, newer.args);
  const before = f.commands.length;
  f.reply(old, 400);
  assert.equal((await old.result).ok, false);
  assert.equal(f.commands.length, before, 'Old failure must not command-clear a newer successful show');
  assert.deepEqual(f.display, newer.args);
  assert.equal(f.requests.length, 3);
  assert.equal(f.refused, 0);
});
check('late-old-positive-after-own-clear', async f => {
  await f.initial();
  const old = f.start(SHOW, showArgs(idA, 'late-positive'));
  const clear = f.start(CLEAR, { preview_id: idA });
  await f.success(clear);
  assert.equal(f.display, null, 'Positive control: genuine clear reply reaches command sink');
  const before = f.commands.length;
  f.reply(old);
  await old.result;
  assert.equal(f.commands.length, before, 'Late old show must not redisplay after matching own clear');
  assert.equal(f.display, null);
  assert.equal(f.requests.length, 3);
});
check('old-clear-reply-after-new-same-id-show', async f => {
  await f.initial();
  const clear = f.start(CLEAR, { preview_id: idA });
  const newer = f.start(SHOW, showArgs(idA, 'replacement'));
  await f.success(newer);
  const before = f.commands.length;
  f.reply(clear);
  await clear.result;
  assert.equal(f.commands.length, before, 'Old clear response must not erase newer SAME-ID show');
  assert.deepEqual(f.display, newer.args);
  assert.equal(f.requests.length, 3);
});
check('different-id-clear-does-not-cancel-new-show', async f => {
  await f.initial();
  const newer = f.start(SHOW, showArgs(idB, 'different-id-new'));
  const clear = f.start(CLEAR, { preview_id: idA });
  await f.success(clear);
  assert.equal(f.display, null, 'Positive control: old ID clear still works while different ID show is pending');
  await f.success(newer);
  assert.deepEqual(f.display, newer.args, 'Unrelated old-ID clear cannot invalidate different-ID pending show');
  const count = f.commands.length;
  const laterClear = f.start(CLEAR, { preview_id: idA });
  await f.success(laterClear);
  assert.equal(f.commands.length, count + 1, 'Different-ID clear can be delivered without harming current display');
  assert.deepEqual(f.display, newer.args);
  assert.equal(f.requests.length, 4);
});
for (const boundary of ['retire', 'not-current']) {
  for (const outcome of ['positive', 'failure']) {
    for (const stage of ['fetch', 'text']) {
      check(`${boundary}-late-${outcome}-at-${stage}`, async f => {
        await f.initial();
        const request = f.start(SHOW, showArgs(idA, 'pending-at-retirement'));
        const status = outcome === 'positive' ? 200 : 400;
        let held: HeldText | null = null;
        if (stage === 'text') {
          held = f.reply(request, status, true);
          if (!held) throw new Error('Missing controlled text boundary');
          await held.entered.promise; // Exact text() await boundary, no polling or sleep.
        }
        if (boundary === 'retire') f.services.retire();
        else f.setNotCurrent();
        const before = f.commands.length;
        if (stage === 'fetch') f.reply(request, status);
        else { if (!held) throw new Error('Missing held text'); held.body.resolve(JSON.stringify(held.raw)); }
        assert.equal((await request.result).ok, false, 'Retired/noncurrent call cannot claim readiness');
        assert.equal(f.commands.length, before, 'No show OR cleanup command after mount retirement/noncurrent');
        assert.equal(f.refused, 0, 'Already noncurrent mount must not emit a new refused callback');
        assert.equal(f.requests.length, 2, 'No post-retirement cleanup RPC');
      });
    }
  }
}

check('late-old-malformed-envelope-after-new-success', async f => {
  await f.initial();
  const old = f.start(SHOW, showArgs(idA, 'old-malformed-pending'));
  const newer = f.start(SHOW, showArgs(idA, 'newest-valid-success'));
  await f.success(newer);
  assert.equal(f.commands.length, 2, 'Positive control: initial and newer valid shows each deliver once');
  assert.deepEqual(f.display, newer.args, 'Positive control: newer same-ID display is visible');
  f.reply(old, 200, false, {
    preview_id: idA,
    host_command: { operation: SHOW, args: old.args, forged: true },
  });
  const result = await old.result;
  assert.equal(result.ok, false, 'Stale 200 reply must still pass closed host-command envelope validation');
  if (result.ok) throw new Error('Malformed stale envelope unexpectedly claimed success');
  assert.equal(result.refusal.code, 'failed', 'Malformed stale envelope is a failed reply, not authorization retirement');
  assert.equal(f.commands.length, 2, 'Malformed stale reply must emit no show or cleanup command');
  assert.equal(f.refused, 0, 'Malformed stale reply must emit no refused callback');
  assert.equal(f.requests.length, 3, 'Malformed stale reply must cause no extra fetch/RPC');
  assert.deepEqual(f.display, newer.args, 'Malformed stale reply must leave the newer same-ID display unchanged');
});

export async function runScreenPreviewRefusalCheck() {
  const processLike = (globalThis as unknown as { process?: { argv: string[]; exitCode?: number } }).process;
  const option = processLike?.argv.indexOf("--case") ?? -1;
  const selectedCase = option >= 0 ? processLike!.argv[option + 1] : "all";
  const chosen = selectedCase === "all" ? cases : cases.filter(c => c.name === selectedCase);
  if (!chosen.length) throw new Error(`Unknown case: ${selectedCase}`);
  if (typeof Response !== "function") throw new Error("Existing Node Response is required");
  if (typeof globalThis.sessionStorage !== "undefined") throw new Error("Use isolated Node without ambient session storage");
  const results: { name: string; result: "PASS" | "FAIL"; failure?: string; evidence: ReturnType<Fixture["snapshot"]> }[] = [];
  try {
    for (const test of chosen) {
      const f = fixture();
      try {
        await test.run(f);
        results.push({ name: test.name, result: "PASS", evidence: f.snapshot() });
      } catch (error) {
        results.push({ name: test.name, result: "FAIL", failure: error instanceof Error ? error.stack : String(error), evidence: f.snapshot() });
      } finally { await f.cleanup(); globalThis.fetch = savedFetch; }
    }
  } finally { globalThis.fetch = savedFetch; }
  const result = results.some(r => r.result === "FAIL") ? "FAIL" : "PASS";
  console.log(JSON.stringify({ check: "screenPreviewRefusal", result, cases: results,
    limitation: "Controlled protocol inputs; command spy only. NOT real server, Native authority, actual rendering or Browser QA." }, null, 2));
  if (result === "FAIL" && processLike) processLike.exitCode = 1;
  return { result, cases: results };
}

// Existing ui/tests tsc + Node loop runs this file directly; the scratch driver awaits the same promise.
export const screenPreviewRefusalCompletion = runScreenPreviewRefusalCheck();
void screenPreviewRefusalCompletion.catch(error => {
  console.error(error);
  const processLike = (globalThis as unknown as { process?: { exitCode?: number } }).process;
  if (processLike) processLike.exitCode = 1;
  else queueMicrotask(() => { throw error; });
});
