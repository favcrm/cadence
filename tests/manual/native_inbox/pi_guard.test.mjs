import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';

const source = readFileSync(process.env.CAD1015_GUARD_SOURCE ?? new URL('../../../src/adapter/pi_turn_guard.ts', import.meta.url), 'utf8');
const fixtureSource = source.replace('import { VERSION } from "@earendil-works/pi-coding-agent";', 'const VERSION = "1.0.0";');
const {default: install} = await import(`data:text/javascript;base64,${Buffer.from(fixtureSource).toString('base64')}`);

function runtime() {
  const handlers = new Map(), commands = new Map(), messages = [], receipts = [];
  const abort = new AbortController();
  const ctx = {mode: 'rpc', idle: true, isIdle() {return this.idle;}, signal: abort.signal};
  install({
    on(name, fn) {handlers.set(name, fn);},
    registerCommand(name, command) {commands.set(name, command);},
    sendMessage(message, options) {messages.push({message, options});},
    appendEntry(kind, data) {receipts.push({kind, ...data});},
  });
  return {
    ctx, abort, messages, receipts,
    fire(name) {return handlers.get(name)?.({}, ctx);},
    context(messages) {return handlers.get('context')?.({messages}, ctx)?.messages ?? messages;},
    async command(name, payload) {await commands.get(name).handler(JSON.stringify(payload), ctx); return receipts.at(-1);},
    async start(turn = 'pi-generation-turn1') {
      await this.command('cadence-bind-turn', {request: 'bind-' + turn, turn});
      ctx.idle = false; this.fire('agent_start'); this.fire('turn_start');
    },
    steer(request = 'm1', turn = 'pi-generation-turn1', text = 'same objective') {
      return this.command('cadence-steer-turn', {request, turn, text});
    },
  };
}

test('an unverified runtime version receives no native control capability', async () => {
  const unverifiedSource = source.replace('import { VERSION } from "@earendil-works/pi-coding-agent";', 'const VERSION = "2.0.0";');
  const {default: unverified} = await import(`data:text/javascript;base64,${Buffer.from(unverifiedSource).toString('base64')}`);
  unverified({on() {assert.fail('unverified runtime registered events');}, registerCommand() {assert.fail('unverified runtime registered commands');}});
});

test('an unused binding can be cleared, but not another or an active run', async () => {
  const r = runtime();
  await r.command('cadence-bind-turn', {request: 'bind', turn: 'unused'});
  assert.equal((await r.command('cadence-abandon-turn', {request: 'wrong', turn: 'foreign'})).outcome, 'skipped_inactive');
  assert.equal((await r.command('cadence-abandon-turn', {request: 'clear', turn: 'unused'})).outcome, 'cleared');
  await r.start();
  assert.equal((await r.command('cadence-abandon-turn', {request: 'active', turn: 'pi-generation-turn1'})).outcome, 'rejected');
  assert.equal((await r.steer()).outcome, 'queued');
});

test('idle input is refused rather than queued for a later run', async () => {
  const r = runtime();
  assert.equal((await r.steer()).outcome, 'skipped_inactive');
  assert.equal(r.messages.length, 0);
  await r.start('next-turn');
  assert.equal(r.messages.length, 0);
});

test('matching active token queues plain custom input exactly once', async () => {
  const r = runtime(); await r.start();
  assert.equal((await r.steer('m1', undefined, '/compact is plain text')).outcome, 'queued');
  await r.steer('m1', undefined, '/compact is plain text');
  assert.equal(r.messages.length, 1);
  assert.equal(r.messages[0].message.customType, 'cadence-turn-guidance');
  assert.equal(r.messages[0].options.deliverAs, 'steer');
  assert.equal(r.messages[0].message.details.turn, 'pi-generation-turn1');
});

test('same id with changed body is refused, never silently deduplicated', async () => {
  const r = runtime(); await r.start(); await r.steer();
  assert.equal((await r.steer('m1', undefined, 'different instruction')).outcome, 'rejected');
  assert.equal(r.messages.length, 1);
});

test('foreign generation and stale turn never reach current run', async () => {
  const r = runtime(); await r.start();
  assert.equal((await r.steer('foreign', 'pi-foreign-turn')).outcome, 'skipped_inactive');
  r.fire('turn_end'); r.fire('agent_end'); r.ctx.idle = true; r.fire('agent_settled');
  await r.start('next-turn');
  assert.equal((await r.steer('stale', 'pi-generation-turn1')).outcome, 'skipped_inactive');
  assert.equal(r.messages.length, 0);
});

test('cancellation cannot replay unconsumed guidance in a successor context', async () => {
  const r = runtime(); await r.start(); await r.steer();
  const old = {role: 'custom', ...r.messages[0].message};
  assert.deepEqual(r.context([old]), [old]);
  r.abort.abort(); r.fire('agent_end'); r.ctx.idle = true; r.fire('agent_settled');
  await r.start('next-turn');
  const next = {role: 'user', content: 'fresh objective'};
  assert.deepEqual(r.context([old, next]), [next]);
});

test('closing turn rejects input even while runtime still reports streaming', async () => {
  const r = runtime(); await r.start();
  r.fire('turn_end');
  assert.equal(r.ctx.isIdle(), false);
  assert.equal((await r.steer()).outcome, 'skipped_inactive');
  assert.equal(r.messages.length, 0);
});

test('abort and non-RPC callers cannot enqueue guidance', async () => {
  const r = runtime(); await r.start(); r.abort.abort();
  assert.equal((await r.steer()).outcome, 'skipped_inactive');
  const other = runtime(); await other.start(); other.ctx.mode = 'tui';
  assert.equal((await other.steer()).outcome, 'rejected');
  assert.equal(r.messages.length + other.messages.length, 0);
});

test('concurrent distinct requests retain order without stealing the objective', async () => {
  const r = runtime(); await r.start();
  const replies = await Promise.all([r.steer('m1'), r.steer('m2')]);
  assert.deepEqual(replies.map(x => x.outcome), ['queued', 'queued']);
  assert.deepEqual(r.messages.map(x => x.message.details.request), ['m1', 'm2']);
});

test('invalid payload and attempts to overwrite an active binding are refused', async () => {
  const r = runtime(); await r.start();
  assert.equal((await r.command('cadence-bind-turn', {request: 'steal', turn: 'replacement'})).outcome, 'rejected');
  assert.equal((await r.steer('bad', undefined, 'x'.repeat(501))).outcome, 'rejected');
  assert.equal(r.messages.length, 0);
});
