import { makePlanner, runCall, type ActionContext } from "../src/features/workspace-apps/screen/screenActions";
import { ScreenChannel } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import type { Installation } from "../src/features/workspace-apps/workspaceApps";
import { allowPrivateReads, pausePrivateReads, privateSessionGeneration, resetPrivateSession } from "../src/lib/privateSession";

// A retired Manage-connections action must not assign, navigate, or show a link
// after pause/reset/re-admission or frame close. A current failure still may.
declare function require(name: string): unknown;
const process = require("process") as { exit(code: number): never };
function check(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
function deferred<T>(): { promise: Promise<T>; resolve: (value: T | PromiseLike<T>) => void; reject: (reason?: unknown) => void } {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

(globalThis as { window?: { location: { origin: string } } }).window = { location: { origin: "https://ef.cadencecloud.app" } };

type Gate = { path: string; finish: (response: Response) => void; fail: (error: unknown) => void };
const gates: Gate[] = [];
globalThis.fetch = (input: RequestInfo | URL) => {
  const { promise, resolve, reject } = deferred<Response>();
  gates.push({ path: String(input), finish: resolve, fail: reject });
  return promise;
};

const installation = { install_id: "inst1", name: "social-content", digest: "sha256:d1" } as Installation;
const ctx = (): ActionContext => ({ installation, installId: "inst1", contextId: "ctx1", runs: [], onChanged: () => {} });
const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 2, tag: "main" };
const init = { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 2 };
const push: ScreenPush = {
  v: 1, op: "screen", install_id: "inst1", digest: "sha256:d1", tag: "main", context_id: "ctx1",
  installation: { install_id: "inst1", digest: "sha256:d1", title: "t", name: "social-content", version: "v", summary: "" },
  contexts: [{ id: "ctx1", label: "context" }], runs: [], outbox: [], now: 1,
  defaults: { context_id: "ctx1", revision: 0, values: {} },
  readiness: { ok: true, blockers: [] },
  publish_intents: { status: "ok", withheld: 0, rows: [] },
  actions: ["open-link"],
};
const hostedUrl = "https://api.example/account/ws/connections/connect?toolkit=instagram&return_to=x";

class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: unknown[] = [];
  closed = false;
  start() {}
  close() { this.closed = true; }
  postMessage(value: unknown) { this.sent.push(value); }
  receive(value: unknown) { this.onmessage?.({ data: value }); }
}

function mount() {
  const source = {} as Window;
  const port = new Port();
  const fx = { assigned: [] as string[], navigated: [] as string[], shown: [] as string[] };
  const channel = new ScreenChannel(source, receipt, push, () => {}, () => {}, () => {}, undefined, {
    call: (verb, args, ui) => runCall(ctx(), verb, args, {
      ...ui,
      navigate: (path) => { fx.navigated.push(path); },
      assign: (url) => { fx.assigned.push(url); },
    }),
    planner: makePlanner(() => ctx()),
    onSlot: () => {},
    onLink: (url) => { fx.shown.push(url); },
  });
  channel.receive({ source, origin: "null", data: init, ports: [port as unknown as MessagePort] });
  port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1"] });
  check(!port.closed && port.sent.length === 1, "the frame is ready before the action");
  return { channel, port, fx };
}

async function until(label: string, ready: () => boolean) {
  for (let i = 0; i < 30 && !ready(); i++) {
    const { promise, resolve } = deferred<void>();
    setTimeout(resolve, 0);
    await promise;
  }
  check(ready(), label);
}
async function drained() {
  for (let i = 0; i < 12; i++) {
    const { promise, resolve } = deferred<void>();
    setTimeout(resolve, 0);
    await promise;
  }
}
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
function effects(fx: { assigned: string[]; navigated: string[]; shown: string[] }) {
  return { assigned: [...fx.assigned], navigated: [...fx.navigated], shown: [...fx.shown] };
}
const none = { assigned: [] as string[], navigated: [] as string[], shown: [] as string[] };

async function connections(id: string) {
  const mounted = mount();
  const before = gates.length;
  mounted.port.receive({ v: 2, op: "call", id, verb: "open-link", args: { url: "/settings/connections" } });
  await until("connect-link started", () => gates.length === before + 1);
  check(gates[gates.length - 1].path.endsWith("/publishing/connect-link"), "the action calls connect-link");
  return { ...mounted, gate: gates[gates.length - 1] };
}

async function main() {
  const generation = privateSessionGeneration();

  // Current session: hosted handoff, local board, transport failure, and link policy.
  {
    const live = await connections("hosted");
    live.gate.finish(json({ hosted: true, url: hostedUrl }));
    await drained();
    check(JSON.stringify(effects(live.fx)) === JSON.stringify({ assigned: [hostedUrl], navigated: [], shown: [] }), "a current hosted link assigns the native URL");
  }
  {
    const live = await connections("local");
    live.gate.finish(json({ hosted: false }));
    await drained();
    check(JSON.stringify(effects(live.fx)) === JSON.stringify({ assigned: [], navigated: ["/settings/connections"], shown: [] }), "a current local board opens the connections page");
  }
  {
    const live = await connections("down");
    live.gate.fail(new TypeError("network down"));
    await drained();
    check(JSON.stringify(effects(live.fx)) === JSON.stringify({ assigned: [], navigated: ["/settings/connections"], shown: [] }), "a current network failure keeps the local page");
  }
  {
    const live = await connections("unavailable");
    live.gate.finish(json({ hosted: true, unavailable: true }));
    await drained();
    check(JSON.stringify(effects(live.fx)) === JSON.stringify(none), "hosted without an address does not leave the workspace");
  }
  {
    const blocked = mount();
    const before = gates.length;
    blocked.port.receive({ v: 2, op: "call", id: "evil", verb: "open-link", args: { url: "https://evil.example/x" } });
    await drained();
    check(gates.length === before && JSON.stringify(effects(blocked.fx)) === JSON.stringify(none), "an arbitrary external URL is not opened");
    const reply = blocked.port.sent[blocked.port.sent.length - 1];
    check(reply !== null && typeof reply === "object" && "ok" in reply && reply.ok === false, "an arbitrary external URL is refused");
  }
  {
    const instagram = mount();
    instagram.port.receive({ v: 2, op: "call", id: "ig", verb: "open-link", args: { url: "https://www.instagram.com/p/abc/" } });
    await drained();
    check(JSON.stringify(effects(instagram.fx)) === JSON.stringify({ assigned: [], navigated: [], shown: ["https://www.instagram.com/p/abc/"] }), "an allowlisted Instagram URL is shown for confirmation");
  }

  // Pause does not change generation. Re-admission must not apply the old body.
  {
    const paused = await connections("pause");
    pausePrivateReads();
    check(privateSessionGeneration() === generation, "pause keeps the generation");
    allowPrivateReads();
    check(privateSessionGeneration() === generation, "re-admission keeps the generation");
    paused.gate.finish(json({ hosted: true, url: hostedUrl }));
    await drained();
    check(JSON.stringify(effects(paused.fx)) === JSON.stringify(none), "a paused action does not assign after re-admission");
  }
  {
    const paused = await connections("pause-fail");
    pausePrivateReads();
    allowPrivateReads();
    paused.gate.fail(new TypeError("network down"));
    await drained();
    check(JSON.stringify(effects(paused.fx)) === JSON.stringify(none), "a paused action does not fall back to the local page");
  }
  {
    const reset = await connections("reset");
    resetPrivateSession();
    allowPrivateReads();
    check(privateSessionGeneration() !== generation, "reset moves the generation");
    reset.gate.finish(json({ hosted: false }));
    await drained();
    check(JSON.stringify(effects(reset.fx)) === JSON.stringify(none), "a reset action does not navigate when its old response arrives");
  }
  {
    const closed = await connections("close");
    closed.channel.close();
    closed.gate.finish(json({ hosted: true, url: hostedUrl }));
    await drained();
    check(JSON.stringify(effects(closed.fx)) === JSON.stringify(none), "closing the frame drops the connect handoff");
    check(closed.port.sent.length === 1, "a closed frame receives no late reply");
  }
  {
    const again = await connections("after");
    again.gate.finish(json({ hosted: true, url: hostedUrl }));
    await drained();
    check(JSON.stringify(effects(again.fx)) === JSON.stringify({ assigned: [hostedUrl], navigated: [], shown: [] }), "a later current action can still hand off");
  }
  allowPrivateReads();
  console.log("screen connect lifetime ok");
}

main().catch(error => {
  allowPrivateReads();
  console.error(error);
  process.exit(1);
});
