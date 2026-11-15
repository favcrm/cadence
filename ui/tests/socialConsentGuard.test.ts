// CAD-2221 — independent acceptance check for the host's per-account publishing
// consent guard. Reviewer-authored: it is not derived from the author's tests and
// does not touch any implementation file.
//
// It drives the REAL host planner (`makePlanner` in screenActions.ts) and the REAL
// `SlotController` (screenSlot.ts) for the `publish.consent.allow` slot. The only
// fake is the external network boundary — `globalThis.fetch` answers like the board
// would. No guard, planner or controller logic is stubbed, so removing the trusted
// check, the guard window, the press-time check or the token check from
// SlotController — or the host's own re-read of the tenant's accounts in
// `planConsentAllow` — makes this file fail.
//
// Run from `ui/` (the same transpile and module path the suite uses):
//   tmp_dir=$(mktemp -d) && tsrs -p tsconfig.tests.json --outDir "$tmp_dir" \
//     && NODE_PATH="$PWD/node_modules" node "$tmp_dir/tests/socialConsentGuard.test.js"

import { makePlanner, type ActionContext } from "../src/features/workspace-apps/screen/screenActions";
import { SLOT_GUARD_MS, SlotController, type SlotView } from "../src/features/workspace-apps/screen/screenSlot";
import type { ReplyMessage, SlotRequest } from "../src/features/workspace-apps/screen/screenProtocol";

function check(value: unknown, label: string): asserts value { if (!value) throw new Error(label); }

// The external boundary only: the board's HTTP answers, recorded as they arrive.
type Call = { path: string; method: string; body: unknown };
const calls: Call[] = [];
const consentPath = "/api/app-installations/inst1/publishing/consent";
let granted = false;
let destinations = [
  { destination_id: "d1", label: "harbour", toolkit: "instagram" },
  { destination_id: "d2", label: "kowloon", toolkit: "instagram" },
];
const json = (body: unknown): Response => new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
  const path = String(input);
  const method = (init?.method ?? "GET").toUpperCase();
  const body: unknown = init?.body ? JSON.parse(String(init.body)) : undefined;
  calls.push({ path, method, body });
  if (path === consentPath && method === "POST") {
    granted = (body as { destination_id?: unknown } | undefined)?.destination_id === "d1";
    return json({ ok: true });
  }
  if (path === consentPath) {
    return json({ available: true, consents: [{ destination_id: "d1", toolkit: "instagram", state: granted ? "allowed" : "none" }] });
  }
  if (path.endsWith("/publishing/destinations")) return json({ unavailable: false, destinations });
  return new Response(JSON.stringify({ error: `unexpected ${method} ${path}` }), { status: 404 });
};

// Only `installId` is read by the consent planner; the rest is the minimum a host action context carries.
const ctx = {
  installId: "inst1", contextId: "ctx1", runs: [], onChanged: () => {},
  installation: { install_id: "inst1", name: "social-content" },
} as unknown as ActionContext;

let now = 1000;
const sent: ReplyMessage[] = [];
// A holder, not a `let`: the controller assigns through a callback, and the type
// must stay `SlotView | null` after the await instead of narrowing to the initial null.
const live: { view: SlotView | null; settle: (() => void) | null } = { view: null, settle: null };
// The slot's own completion signal, never a sleep; the timer is only a bounded
// failure safeguard so a broken guard fails loudly instead of hanging the run.
const settleBarrier = () => new Promise<void>((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error("the consent slot never settled")), 5000);
  live.settle = () => { clearTimeout(timer); resolve(); };
});
const controller = new SlotController(
  message => sent.push(message),
  next => {
    live.view = next;
    if (next === null && live.settle) {
      const done = live.settle;
      live.settle = null;
      done();
    }
  },
  makePlanner(() => ctx),
  () => now,
);
const postCount = () => calls.filter(call => call.method === "POST" && call.path === consentPath).length;
const request = (id: string, destination: string): SlotRequest => ({
  v: 2, op: "slot", id, verb: "publish.consent.allow",
  args: { destination_id: destination, toolkit: "instagram" },
  anchor: { x: 0, y: 0, w: 120, h: 40 },
});

(async () => {
  try {
    await controller.request(request("s1", "d1"));
    // The cast keeps the declared type: the value is written only from the controller callback.
    const planned = live.view as SlotView | null;
    check(planned, "the host plans a consent button only after reading the capability and the account list");
    check(planned.card !== undefined, "the decision is drawn as a host-owned card, not a frame claim");
    check(calls.length === 2 && calls.every(call => call.method === "GET"), "planning reads; it never issues the grant POST");
    check(postCount() === 0, "no grant POST exists before any click");

    const token = planned.token;
    const since = now;

    now = since + SLOT_GUARD_MS - 1;
    check(!controller.tap(token, true, now), "a trusted click inside the guard window is ignored");
    check(postCount() === 0, "a click inside the guard window issues no grant POST");

    now = since + SLOT_GUARD_MS;
    check(!controller.tap(token, false, now), "an untrusted click after the guard is ignored");
    check(postCount() === 0, "an untrusted click issues no grant POST");

    now = since + SLOT_GUARD_MS + 1;
    check(!controller.tap(token, true, since + SLOT_GUARD_MS - 10), "a press that began inside the guard is ignored even when released after it");
    check(postCount() === 0, "a press that began inside the guard issues no grant POST");

    check(!controller.tap(`forged-${token}`, true, now), "a token the host never minted is ignored");
    check(postCount() === 0, "a forged token issues no grant POST");

    const settled = settleBarrier();
    now = since + SLOT_GUARD_MS + 2;
    check(controller.tap(token, true, now), "an eligible trusted click on the host-drawn button fires");
    await settled;

    check(postCount() === 1, "the eligible click issues exactly one grant POST");
    const write = calls.find(call => call.method === "POST");
    check(write !== undefined && write.path === consentPath && JSON.stringify(write.body) === JSON.stringify({ destination_id: "d1" }),
      "the grant names only the account the host's own tenant read verified");
    check(calls[calls.length - 1].method === "GET" && calls[calls.length - 1].path === consentPath,
      "the outcome is the board's read-back, not the write's own answer");
    const finished = sent[sent.length - 1];
    check(finished.op === "slot-state" && finished.state === "done", "the frame hears the slot finished");

    check(!controller.tap(token, true, now + 1000), "a replayed click after the slot finished does nothing");
    check(postCount() === 1, "a replay issues no second grant POST");

    // The board no longer lists the account the frame named: the host must not plan
    // a button, and no click can ever reach the grant write.
    destinations = [{ destination_id: "d2", label: "kowloon", toolkit: "instagram" }];
    await controller.request(request("s2", "d1"));
    check(live.view === null, "an account the board no longer lists draws no consent button");
    const stale = sent[sent.length - 1];
    check(stale.op === "slot-state" && stale.state === "refused" && stale.refusal?.code === "not_found",
      "a stale or mismatched account id is refused, never granted");
    check(postCount() === 1, "a stale account issues no grant POST");

    console.log("socialConsentGuard ok");
  } finally {
    controller.close();
  }
})().catch(error => setTimeout(() => { throw error; }));
