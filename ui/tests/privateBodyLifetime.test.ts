import type * as PrivateSessionModule from "../src/lib/privateSession";
/**
 * Private response bodies stay bound to the admitted read that started them.
 * Chrome 111 / Firefox 114 / Safari 16.4 have no AbortSignal.any; the body
 * fence still has to hold. Network bytes are real ReadableStream chunks.
 */
declare function require(name: string): unknown;
const { allowPrivateReads, pausePrivateReads, privateFetch, privateReadsAllowed, privateSessionGeneration, resetPrivateSession, withPrivateOperation } =
  require("../src/lib/privateSession") as typeof PrivateSessionModule;
const process = require("process") as { argv: string[]; exitCode: number };

const assert: (condition: unknown, message: string) => asserts condition = (condition, message) => {
  if (!condition) throw new Error(message);
};
const originalFetch = globalThis.fetch;
const originalAdd = AbortSignal.prototype.addEventListener;
const originalRemove = AbortSignal.prototype.removeEventListener;
const originalAny = Object.getOwnPropertyDescriptor(AbortSignal, "any");
let abortAdds = 0;
let abortRemoves = 0;
const trackedSignals = new WeakSet<AbortSignal>();

type Deferred<T> = { promise: Promise<T>; resolve: (value: T) => void };
function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
type Gate = { stream: ReadableStream<Uint8Array>; started: Promise<void>; release: () => void; bind: (signal: AbortSignal | null | undefined) => void };
const releases: Array<() => void> = [];
function heldBody(bytes: Uint8Array, cooperative: boolean): Gate {
  const started = deferred<void>();
  const opened = deferred<void>();
  let signal: AbortSignal | null | undefined;
  let released = false;
  const release = () => {
    if (released) return;
    released = true;
    opened.resolve();
  };
  releases.push(release);
  const stream = new ReadableStream<Uint8Array>({
    async pull(controller) {
      started.resolve();
      try {
        if (!cooperative) {
          await opened.promise;
        } else {
          await new Promise<void>((resolve, reject) => {
            let settled = false;
            let listening = false;
            const finish = (failed: boolean) => {
              if (settled) return;
              settled = true;
              if (listening) signal?.removeEventListener("abort", onAbort);
              listening = false;
              if (failed) reject(new DOMException("The operation was aborted", "AbortError"));
              else resolve();
            };
            const onAbort = () => finish(true);
            opened.promise.then(() => finish(false), () => finish(true));
            if (signal?.aborted) finish(true);
            else if (signal) {
              listening = true;
              signal.addEventListener("abort", onAbort);
            }
          });
        }
        controller.enqueue(bytes);
        controller.close();
      } catch (error) {
        try { controller.error(error); } catch { /* the consumer already stopped this branch */ }
      }
    },
  }, { highWaterMark: 0 });
  return { stream, started: started.promise, release, bind: (next) => { signal = next; } };
}
function openBody(bytes: Uint8Array): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      controller.enqueue(bytes);
      controller.close();
    },
  });
}
async function readBody(response: Response): Promise<Uint8Array> {
  const body = response.body;
  assert(body, "privateFetch returns a readable body");
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  try {
    for (;;) {
      const step = await reader.read();
      if (step.done) break;
      if (step.value) chunks.push(step.value);
    }
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    throw error;
  }
  const size = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
  const out = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}
async function readPrivate(input: RequestInfo | URL, init?: RequestInit): Promise<Uint8Array> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(input, init, operation);
    const bytes = await readBody(response);
    operation.assertCurrent();
    return bytes;
  });
}
type Outcome = { ok: true; bytes: Uint8Array } | { ok: false; error: unknown };
function settle(read: Promise<Uint8Array>): Promise<Outcome> {
  return read.then((bytes) => ({ ok: true as const, bytes }), (error: unknown) => ({ ok: false as const, error }));
}
function sessionStamp(): { generation: number; allowed: boolean } {
  return { generation: privateSessionGeneration(), allowed: privateReadsAllowed() };
}
function assertUnchanged(stamp: { generation: number; allowed: boolean }, why: string): void {
  assert(privateSessionGeneration() === stamp.generation && privateReadsAllowed() === stamp.allowed, why);
}
function listenerGap(): number {
  return abortAdds - abortRemoves;
}
const decoder = new TextDecoder();
let posts = 0;
let transportSignal: AbortSignal | null = null;
function installFetch(serve: (input: RequestInfo | URL, init: RequestInit | undefined) => Response | Promise<Response>): void {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const method = (init?.method ?? "GET").toUpperCase();
    if (method !== "GET" && method !== "HEAD") posts += 1;
    transportSignal = init?.signal ?? null;
    return serve(input, init);
  }) as typeof fetch;
}
const scenario = process.argv[2] ?? "all";
const selected = (name: string) => scenario === "all" || scenario === name;

async function ordinaryRead(): Promise<void> {
  allowPrivateReads();
  const before = listenerGap();
  const stamp = sessionStamp();
  const caller = new AbortController();
  trackedSignals.add(caller.signal);
  const payload = JSON.stringify({ marker: "ordinary-private-body" });
  installFetch(() => new Response(openBody(new TextEncoder().encode(payload)), { status: 200, headers: { "content-type": "application/json" } }));
  const bytes = await readPrivate("/api/private-body", { signal: caller.signal });
  assert(decoder.decode(bytes) === payload, "an admitted privateFetch yields the full response body");
  assert(!caller.signal.aborted, "a completed read does not abort the caller");
  const completedAborted = transportSignal?.aborted;
  assertUnchanged(stamp, "a successful body read does not change admission");
  assert(posts === 0, "an ordinary read performs no private write");
  pausePrivateReads();
  assert(!caller.signal.aborted && transportSignal?.aborted === completedAborted, "a later pause does not change a completed request's cancellation state");
  allowPrivateReads();
  assert(listenerGap() === before, "abort listeners detach after the body is consumed");
}

async function preAborted(): Promise<void> {
  allowPrivateReads();
  const before = listenerGap();
  const stamp = sessionStamp();
  const postsBefore = posts;
  const caller = new AbortController();
  trackedSignals.add(caller.signal);
  caller.abort();
  installFetch(() => new Response(openBody(new TextEncoder().encode("{\"mutated\":true}")), { status: 200 }));
  const outcome = await settle(readPrivate("/api/private-body", { method: "POST", body: "{}", signal: caller.signal }));
  assert(!outcome.ok && !(outcome.error instanceof TypeError), "a pre-aborted caller is refused without AbortSignal.any");
  assert(posts === postsBefore, "a pre-aborted caller performs no private write");
  assertUnchanged(stamp, "rejecting a pre-aborted caller does not reset or pause the session");
  assert(listenerGap() === before, "abort listeners detach after a pre-aborted call");
}

async function callerAbort(cooperative: boolean): Promise<void> {
  allowPrivateReads();
  const before = listenerGap();
  const stamp = sessionStamp();
  const caller = new AbortController();
  trackedSignals.add(caller.signal);
  const payload = new TextEncoder().encode("{\"marker\":\"caller-abort-body\"}");
  const fixture: { gate: Gate | null } = { gate: null };
  installFetch((_input, init) => {
    const next = heldBody(payload, cooperative);
    next.bind(init?.signal);
    fixture.gate = next;
    return new Response(next.stream, { status: 200, headers: { "content-type": "application/json" } });
  });
  const outcome = settle(readPrivate("/api/private-body", { signal: caller.signal }));
  const installed = fixture.gate;
  assert(installed, "the caller-abort body stayed pending after headers");
  await installed.started;
  caller.abort();
  installed.release();
  const result = await outcome;
  assert(!result.ok, cooperative
    ? "aborting the caller while the body is pending rejects that read"
    : "a body that ignores transport abort cannot pass the final private-operation fence");
  assertUnchanged(stamp, "a caller abort does not reset or pause the session");
  assert(listenerGap() === before, "abort listeners detach after the caller cancels");
}

async function resetDuringDecode(): Promise<void> {
  allowPrivateReads();
  const before = listenerGap();
  const payload = new TextEncoder().encode("{\"marker\":\"stale-private-body\"}");
  const fixture: { gate: Gate | null } = { gate: null };
  const caller = new AbortController();
  trackedSignals.add(caller.signal);
  installFetch(() => {
    const next = heldBody(payload, false);
    fixture.gate = next;
    return new Response(next.stream, { status: 200, headers: { "content-type": "application/json" } });
  });
  const startedGeneration = privateSessionGeneration();
  const outcome = settle(readPrivate("/api/private-body", { signal: caller.signal }));
  const installed = fixture.gate;
  assert(installed, "the reset body stayed pending after headers");
  await installed.started;
  resetPrivateSession();
  allowPrivateReads();
  assert(privateSessionGeneration() === startedGeneration + 1 && privateReadsAllowed(), "the replacement session is admitted");
  installed.release();
  const result = await outcome;
  assert(!result.ok, "a body that arrives after reset is not a result for the new session");
  assert(listenerGap() === before, "abort listeners detach after a reset abandons the body");
}

async function failedFetch(): Promise<void> {
  allowPrivateReads();
  const before = listenerGap();
  const stamp = sessionStamp();
  const caller = new AbortController();
  trackedSignals.add(caller.signal);
  let attempted = false;
  installFetch(() => {
    attempted = true;
    throw new Error("gateway down");
  });
  const outcome = await settle(readPrivate("/api/private-body", { signal: caller.signal }));
  assert(attempted && !outcome.ok, "a failed transport rejects the private read");
  assertUnchanged(stamp, "a failed transport does not reset or pause the session");
  assert(listenerGap() === before, "abort listeners detach after a failed fetch");
}

function installListenerProbe(): void {
  const add = originalAdd as (this: AbortSignal, ...args: unknown[]) => void;
  const remove = originalRemove as (this: AbortSignal, ...args: unknown[]) => void;
  AbortSignal.prototype.addEventListener = function (this: AbortSignal, type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions) {
    if (type === "abort" && trackedSignals.has(this)) abortAdds += 1;
    add.call(this, type, listener, options);
  } as typeof AbortSignal.prototype.addEventListener;
  AbortSignal.prototype.removeEventListener = function (this: AbortSignal, type: string, listener: EventListenerOrEventListenerObject, options?: boolean | EventListenerOptions) {
    if (type === "abort" && trackedSignals.has(this)) abortRemoves += 1;
    remove.call(this, type, listener, options);
  } as typeof AbortSignal.prototype.removeEventListener;
  Object.defineProperty(AbortSignal, "any", { configurable: true, writable: true, value: undefined });
}

async function run(): Promise<void> {
  installListenerProbe();
  allowPrivateReads();
  if (selected("read")) await ordinaryRead();
  if (selected("preabort")) await preAborted();
  if (selected("caller-abort")) await callerAbort(true);
  if (selected("caller-ignored")) await callerAbort(false);
  if (selected("reset")) await resetDuringDecode();
  if (selected("failed")) await failedFetch();
  assert(listenerGap() === 0, "private body tests leave no abort listeners attached");
  console.log(`private body lifetime ${scenario}: passed`);
}

run().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
}).finally(() => {
  for (const release of releases) release();
  globalThis.fetch = originalFetch;
  AbortSignal.prototype.addEventListener = originalAdd;
  AbortSignal.prototype.removeEventListener = originalRemove;
  if (originalAny) Object.defineProperty(AbortSignal, "any", originalAny);
  else Reflect.deleteProperty(AbortSignal, "any");
});
