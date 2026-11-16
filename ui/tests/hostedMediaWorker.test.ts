export {};

declare function require(name: string): unknown;
declare const process: { cwd(): string; exitCode?: number };

const { readFileSync } = require("node:fs") as { readFileSync(path: string, encoding: string): string };
const { runInNewContext } = require("node:vm") as { runInNewContext(source: string, context: object): void };

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
function equal(actual: unknown, expected: unknown, message: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected))
    throw new Error(`${message}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

const ORIGIN = "https://media.example.test";
const URL_BOUND = `${ORIGIN}/api/chat/attachments-bound/v1/chf-${"a".repeat(32)}?conversation=tc_test`;
const SESSION_A = "bsi_11111111-1111-4111-8111-111111111111";
const SESSION_B = "bsi_22222222-2222-4222-8222-222222222222";

type WorkerEvent = {
  request?: Request;
  clientId?: string;
  respondWith?: (response: Promise<Response>) => void;
  waitUntil?: (work: Promise<unknown>) => void;
  data?: unknown;
  ports?: MessagePort[];
};
type Listener = (event: WorkerEvent) => void;

async function run(): Promise<void> {
  const handlers = new Map<string, Listener[]>();
  const notifications: unknown[] = [];
  const opened: string[] = [];
  const timers = new Map<number, () => void>();
  let timerId = 0;
  let skipped = 0;
  let claimed = 0;
  let currentClient: { url: string; postMessage: (data: unknown, ports: MessagePort[]) => void } | undefined;
  let upstream: (request: Request) => Promise<Response> = async () => new Response(null, { status: 500 });
  const requests: Request[] = [];
  const worker = {
    location: { origin: ORIGIN },
    addEventListener(type: string, handler: Listener) {
      const listeners = handlers.get(type) ?? [];
      listeners.push(handler);
      handlers.set(type, listeners);
    },
    skipWaiting: async () => { skipped++; },
    clients: {
      claim: async () => { claimed++; },
      get: async (id: string) => id === "tab-a" ? currentClient : undefined,
      matchAll: async () => [],
      openWindow: async (url: string) => { opened.push(url); },
    },
    registration: { showNotification: async (...args: unknown[]) => { notifications.push(args); } },
  };
  const context = {
    self: worker,
    URL,
    Request,
    Response,
    Headers,
    MessageChannel,
    setTimeout: (callback: () => void) => { const id = ++timerId; timers.set(id, callback); return id; },
    clearTimeout: (id: number) => { timers.delete(id); },
    fetch: async (request: Request) => { requests.push(request); return upstream(request); },
  };
  runInNewContext(readFileSync(`${process.cwd()}/public/notifications-sw.js`, "utf8"), context);
  const emit = (type: string, event: WorkerEvent) => { for (const handler of handlers.get(type) ?? []) handler(event); };
  const waitEvent = async (type: string) => {
    let work: Promise<unknown> | undefined;
    emit(type, { waitUntil: (value) => { work = value; } });
    assert(work, `${type} waits for its lifecycle operation`);
    await work;
  };
  await waitEvent("install");
  await waitEvent("activate");
  equal([skipped, claimed], [1, 1], "the installed worker can control the current page");

  const capability = new MessageChannel();
  const capabilityReply = new Promise<unknown>((resolve) => { capability.port1.onmessage = (event) => resolve(event.data); });
  emit("message", { data: { type: "cadence:media-capability", version: 1 }, ports: [capability.port2] });
  equal(await capabilityReply, { type: "cadence:media-capability", version: 1 }, "the worker proves its exact media protocol");
  capability.port1.close();
  capability.port2.close();

  const dispatch = (request: Request, clientId = "tab-a"): Promise<Response> | undefined => {
    let answer: Promise<Response> | undefined;
    emit("fetch", { request, clientId, respondWith: (value) => { answer = value; } });
    return answer;
  };
  const source = () => new Request(URL_BOUND, { headers: { range: "bytes=2-5" }, credentials: "same-origin" });
  const replyWith = (ids: (string | null)[]) => {
    currentClient = {
      url: `${ORIGIN}/`,
      postMessage(data, ports) {
        equal(data, { type: "cadence:media-session", version: 1 }, "the worker asks the initiating tab");
        const port = ports[0];
        assert(port, "the request carries a reply port");
        port.postMessage({ type: "cadence:media-session", version: 1, sessionId: ids.shift() ?? null });
        port.close();
      },
    };
  };
  const rangeHeaders = (proof: string | null) => ({
    "content-type": "video/mp4",
    "content-range": "bytes 2-5/32",
    ...(proof === null ? {} : { "X-Cadence-Verified-Session": proof }),
  });

  replyWith([SESSION_A, SESSION_A]);
  upstream = async () => new Response(new Uint8Array([2, 3, 4, 5]), { status: 206, headers: rangeHeaders(SESSION_A) });
  const permitted = await dispatch(source());
  assert(permitted, "the bound media request is intercepted");
  equal(permitted.status, 206, "a verified range stays a range response");
  equal([...new Uint8Array(await permitted.arrayBuffer())], [2, 3, 4, 5], "verified video bytes reach the player");
  equal(requests.at(-1)?.headers.get("range"), "bytes=2-5", "the byte range is preserved");
  equal(requests.at(-1)?.headers.get("X-Cadence-Expected-Session"), SESSION_A, "the upstream request pins the tab session");
  equal(requests.at(-1)?.cache, "no-store", "private ranges bypass caches");
  equal(requests.at(-1)?.credentials, "same-origin", "only same-origin cookies accompany the proof");
  assert(!requests.at(-1)?.url.includes(SESSION_A), "the session ID never enters the URL");

  const count = requests.length;
  replyWith([null]);
  const unverified = await dispatch(source());
  assert(unverified, "the missing-proof request is intercepted");
  equal(unverified.status, 503, "a missing tab proof fails closed");
  equal(requests.length, count, "a missing tab proof reaches no upstream");

  let proofRequested!: () => void;
  const requestedProof = new Promise<void>((resolve) => { proofRequested = resolve; });
  currentClient = {
    url: `${ORIGIN}/`,
    postMessage(_data, ports) { ports[0]?.close(); proofRequested(); },
  };
  const timedOut = dispatch(source());
  assert(timedOut, "the silent tab request is intercepted");
  await requestedProof;
  equal(timers.size, 1, "a silent tab has a bounded proof wait");
  for (const callback of [...timers.values()]) callback();
  equal((await timedOut).status, 503, "a silent tab cannot release bytes");
  equal(requests.length, count, "a timed-out proof reaches no upstream");

  replyWith([SESSION_A, SESSION_A]);
  upstream = async () => new Response(null, {
    status: 416,
    headers: { "content-range": "bytes */32", "X-Cadence-Verified-Session": SESSION_A },
  });
  const unsatisfiable = await dispatch(source());
  assert(unsatisfiable, "an unsatisfiable range is intercepted");
  equal(unsatisfiable.status, 416, "a proved unsatisfiable range reaches the player");
  equal(unsatisfiable.headers.get("content-range"), "bytes */32", "the proved range size is retained");

  let cancelled = 0;
  replyWith([SESSION_A]);
  upstream = async () => new Response(new ReadableStream({ cancel: () => { cancelled++; } }), {
    status: 206,
    headers: rangeHeaders(SESSION_B),
  });
  const wrongProof = await dispatch(source());
  assert(wrongProof, "the mismatched response is intercepted");
  equal(wrongProof.status, 503, "a different server session never reaches the player");
  equal(cancelled, 1, "the mismatched response body is cancelled");

  replyWith([SESSION_A]);
  upstream = async () => new Response("unproved video", { status: 200, headers: rangeHeaders(null) });
  const oldServer = await dispatch(source());
  assert(oldServer, "the old-server response is intercepted");
  equal(oldServer.status, 503, "an older server cannot silently downgrade the proof");
  assert(!(await oldServer.text()).includes("unproved video"), "unproved bytes are withheld");

  cancelled = 0;
  replyWith([SESSION_A, SESSION_B]);
  upstream = async () => new Response(new ReadableStream({ cancel: () => { cancelled++; } }), {
    status: 206,
    headers: rangeHeaders(SESSION_A),
  });
  const changed = await dispatch(source());
  assert(changed, "the changed-session request is intercepted");
  equal(changed.status, 503, "a tab that changes identity during the read gets no bytes");
  equal(cancelled, 1, "the old-session response is cancelled");

  currentClient = undefined;
  const missingClient = await dispatch(source());
  assert(missingClient, "a request without a live client is intercepted");
  equal(missingClient.status, 503, "a missing client fails closed");
  assert(dispatch(new Request(`${ORIGIN}/api/projects`)) === undefined, "unrelated requests are untouched");
  assert(dispatch(new Request(`https://other.example.test/api/chat/attachments-bound/v1/chf-${"a".repeat(32)}`)) === undefined, "another origin is untouched");

  let notificationWork: Promise<unknown> | undefined;
  emit("push", {
    data: { json: () => ({ title: "Synthetic", body: "Preview" }) },
    waitUntil: (value) => { notificationWork = value; },
  });
  assert(notificationWork, "notification delivery remains registered");
  await notificationWork;
  equal((notifications[0] as unknown[])[0], "Synthetic", "existing push titles still work");
  let clickWork: Promise<unknown> | undefined;
  emit("notificationclick", {
    notification: { data: { url: "/threads?h=synthetic" }, close() {} },
    waitUntil: (value: Promise<unknown>) => { clickWork = value; },
  } as unknown as WorkerEvent);
  assert(clickWork, "notification clicks remain registered");
  await clickWork;
  equal(opened, ["/threads?h=synthetic"], "notification navigation is preserved");
  equal(timers.size, 0, "all session-proof timers are cleared");
  console.log("hosted media worker checks passed");
}

void run().catch((error: unknown) => { console.error(error); process.exitCode = 1; });
