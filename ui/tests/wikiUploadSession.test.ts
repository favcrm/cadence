/**
 * Wiki uploads through the admitted workspace. Pause keeps the queue and
 * does not start the next file on resume; retry does. A known 401 on the
 * live transfer clears the private view and tab key. A role 403, an
 * unrelated 401, and a 401 from the aborted transfer do not.
 */
import type * as ReactModule from "react";
import type * as AuthBoundaryModule from "../src/features/auth/AuthBoundary";
import type * as UploadPaneModule from "../src/features/wiki/UploadPane";
import type * as SessionKeyModule from "../src/lib/sessionKey";
import type * as PrivateSessionModule from "../src/lib/privateSession";

declare function require<T = unknown>(name: string): T;

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function sameCount(actual: number, expected: number): boolean {
  return actual === expected;
}

interface PendingCall {
  name: string;
  op: string;
  init: RequestInit;
  aborted: boolean;
  settled: boolean;
  resolve: (response: Response) => void;
  reject: (error: unknown) => void;
  respond: (status: number, body: unknown) => void;
}

const calls: PendingCall[] = [];
const jobNames = new Map<string, string>();

function requestName(body: { transfer?: { op?: string; job_id?: string; manifest?: { entries?: { path?: string }[] } } }): string {
  const transfer = body.transfer;
  if (transfer?.op === "begin") return transfer.manifest?.entries?.[0]?.path ?? "";
  return jobNames.get(transfer?.job_id ?? "") ?? "";
}

function installFetch(meta: () => Response): void {
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/meta") return meta();
    if (url.pathname !== "/api/wiki/transfer" && url.pathname !== "/api/wiki/job_status") {
      throw new Error(`unexpected ${url.pathname}`);
    }
    const parsed = JSON.parse(String(init?.body ?? "{}")) as { transfer?: { op?: string; job_id?: string; manifest?: { entries?: { path?: string }[] } } };
    const op = parsed.transfer?.op ?? "";
    const name = requestName(parsed);
    return await new Promise<Response>((resolve, reject) => {
      const call: PendingCall = {
        name,
        op,
        init: init ?? {},
        aborted: false,
        settled: false,
        resolve,
        reject,
        respond(status: number, body: unknown) {
          if (call.settled) return;
          call.settled = true;
          if (op === "begin" && status >= 200 && status < 300) {
            const record = body as { job_id?: string };
            if (record.job_id) jobNames.set(record.job_id, name);
          }
          resolve(new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } }));
        },
      };
      calls.push(call);
      init?.signal?.addEventListener("abort", () => {
        if (call.settled) return;
        call.settled = true;
        call.aborted = true;
        reject(new DOMException("aborted", "AbortError"));
      }, { once: true });
    });
  };
}

async function waitCall(name: string, previous: PendingCall | undefined, flush: () => Promise<void>): Promise<PendingCall> {
  for (let attempt = 0; attempt < 40; attempt++) {
    await flush();
    const call = calls.find((item) => item.name === name && !item.settled && item !== previous);
    if (call) return call;
  }
  throw new Error(`missing transfer for ${name}; saw ${calls.map((item) => `${item.name}:${item.op}`).join(",")}`);
}

async function finishFile(
  name: string,
  previous: PendingCall | undefined,
  extraction: { status: string; reason?: string },
  flush: () => Promise<void>,
  act: (fn: () => void) => Promise<void>,
): Promise<PendingCall> {
  const jobId = `job-${name}`;
  const ops = ["begin", "status", "chunk", "apply", "finish"] as const;
  let prior = previous;
  for (const op of ops) {
    const call = await waitCall(name, prior, flush);
    if (call.op !== op) throw new Error(`${name} expected ${op}, got ${call.op}`);
    const body = op === "begin"
      ? { job_id: jobId, digest: `digest-${name}`, root: "global/guides", source_id: "board", state: "open", entries: 1 }
      : op === "status"
        ? { job_id: jobId, digest: `digest-${name}`, source_id: "board", root: "global/guides", state: "open", index: "pending", entries: [{ entry_id: "file", path: name, received: 0, size: name.length, outcome: "pending", extraction: "not_applicable" }], next_offset: null }
        : op === "chunk"
          ? { job_id: jobId, entry_id: "file", received: name.length, size: name.length }
          : op === "apply"
            ? { job_id: jobId, entry_id: "file", path: name, outcome: "applied", extraction: extraction.status, rev: "fnv1a:0123456789abcdef", ...(extraction.reason ? { error: extraction.reason } : {}) }
            : { job_id: jobId, digest: `digest-${name}`, root: "global/guides", state: "complete", index: extraction.status === "indexed" ? "covered" : "pending" };
    await act(() => { call.respond(200, body); });
    prior = call;
  }
  return prior!;
}

function fileList(files: File[]): FileList {
  const list = {
    length: files.length,
    item: (index: number) => files[index] ?? null,
    *[Symbol.iterator]() { yield* files; },
  };
  files.forEach((file, index) => {
    Object.defineProperty(list, String(index), { value: file });
  });
  return list as unknown as FileList;
}

function rowStates(host: HTMLElement): string[] {
  return Array.from(host.querySelectorAll(".wk-uprow")).map((row) => {
    const name = row.querySelector(".wk-uname")?.textContent ?? "";
    const state = ["queued", "uploading", "done", "error"].find((kind) => row.classList.contains(kind)) ?? "";
    return `${name}:${state}`;
  });
}

async function main(): Promise<void> {
  type DomConstructors = Pick<typeof globalThis, "Node" | "HTMLElement" | "HTMLInputElement" | "HTMLButtonElement" | "Event">;
  const { Window } = require<{ Window: new (options: { url: string }) => Window & DomConstructors }>("happy-dom");
  const win = new Window({ url: "http://localhost/wiki" });
  const constructors: DomConstructors = {
    Node: win.Node,
    HTMLElement: win.HTMLElement,
    HTMLInputElement: win.HTMLInputElement,
    HTMLButtonElement: win.HTMLButtonElement,
    Event: win.Event,
  };
  for (const name of ["Node", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "Event"] as const) {
    Object.defineProperty(globalThis, name, { value: constructors[name], configurable: true });
  }
  Object.defineProperty(globalThis, "window", { value: win, configurable: true });
  Object.defineProperty(globalThis, "document", { value: win.document, configurable: true });
  Object.defineProperty(globalThis, "navigator", { value: win.navigator, configurable: true });
  Object.defineProperty(globalThis, "location", { value: win.location, configurable: true });
  Object.defineProperty(globalThis, "history", { value: win.history, configurable: true });
  Object.defineProperty(globalThis, "sessionStorage", { value: win.sessionStorage, configurable: true });
  Object.defineProperty(globalThis, "localStorage", { value: win.localStorage, configurable: true });
  for (const name of ["addEventListener", "removeEventListener"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };

  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => { render: (node: unknown) => void; unmount: () => void } }>("react-dom/client");
  const { LocaleProvider } = require<{ LocaleProvider: ReactModule.ComponentType<{ children?: ReactModule.ReactNode }> }>("../src/lib/locale");
  const AuthBoundary = require<typeof AuthBoundaryModule>("../src/features/auth/AuthBoundary").default;
  const UploadPane = require<typeof UploadPaneModule>("../src/features/wiki/UploadPane").default;
  const { sessionKey, setSessionKey } = require<typeof SessionKeyModule>("../src/lib/sessionKey");
  const { pausePrivateReads, allowPrivateReads, resetPrivateSession } = require<typeof PrivateSessionModule>("../src/lib/privateSession");

  const signedIn = {
    read_only: false,
    signed_in: true,
    actor: "operator (ui)",
    tailnet_url: null,
    version: "0",
    build_commit: "x",
    build_time: "t",
    hosted: false,
    operator: true,
    session: {
      id: "local-operator",
      origin: "loopback",
      created: 1700000000,
      last_used: 1700000000,
      idle_expires_at: 4102444800,
      expires_at: 4102444800,
      user_agent: "test",
      user: { sub: "operator", name: "Operator", email: "op@example.test", handle: "op", role: "operator" },
    },
  };
  installFetch(() => new Response(JSON.stringify(signedIn), { status: 200, headers: { "Content-Type": "application/json" } }));

  const host = document.createElement("div");
  document.body.appendChild(host);
  const roots: { unmount: () => void }[] = [];
  try {
  const root = createRoot(host);
  roots.push(root);
  const toasts: string[] = [];
  let uploaded = 0;
  setSessionKey("admitted-tab-key");
  const flush = async () => { await React.act(async () => { await Promise.resolve(); }); };
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(AuthBoundary, {
      children: () => React.createElement("div", { "data-private-workspace": "wiki" }, React.createElement(UploadPane, {
        dir: "global/guides",
        navHref: () => "/wiki",
        readOnly: false,
        onToast: (_kind: "ok" | "err" | "warn", text: string) => { toasts.push(text); },
        onUploaded: () => { uploaded += 1; },
      })),
    })));
  });
  await flush();
  assert(host.querySelector("[data-private-workspace='wiki']"), "verified metadata mounts the private upload workspace");
  assert(sessionKey() === "admitted-tab-key", "admission keeps the tab credential");

  const mountedInput = (): HTMLInputElement => {
    const input = host.querySelector('input[type="file"]');
    if (!(input instanceof HTMLInputElement) || !input.isConnected) throw new Error("file input missing");
    return input;
  };
  const add = async (names: string[]) => {
    const input = mountedInput();
    Object.defineProperty(input, "files", {
      configurable: true,
      value: fileList(names.map((name) => new File([name], name))),
    });
    await React.act(async () => { input.dispatchEvent(new Event("change", { bubbles: true })); });
  };
  const act = (fn: () => void) => React.act(async () => { fn(); });
  await add(["one.txt", "two.txt"]);
  const first = await waitCall("one.txt", undefined, flush);
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:uploading", "two.txt:queued"]), "the first file is in flight and the next stays queued");
  const opened = calls.length;

  await React.act(async () => { pausePrivateReads(); });
  assert(first.aborted, "hiding the admitted workspace aborts the live transfer");
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:queued", "two.txt:queued"]), "both files stay queued for an explicit retry");
  await React.act(async () => { first.respond(200, { job_id: "late", digest: "late", state: "open", entries: 1 }); });
  assert(sameCount(uploaded, 0) && sameCount(toasts.length, 0), "the hidden completion does not finish or toast");
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:queued", "two.txt:queued"]), "the hidden completion does not mark a retained row done");
  await React.act(async () => { allowPrivateReads(); });
  await flush();
  assert(calls.length === opened, "resume does not open the next transfer");
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:queued", "two.txt:queued"]), "resume leaves the queue waiting");
  assert(sessionKey() === "admitted-tab-key" && host.querySelector("[data-private-workspace='wiki']"), "pause and resume keep the admitted view and key");

  const retry = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Retry uploads");
  assert(retry, "the held queue offers retry");
  await React.act(async () => { retry.click(); });
  assert(rowStates(host)[0] === "one.txt:uploading" && rowStates(host)[1] === "two.txt:queued", "retry uploads the abandoned file and holds the next");
  const retried = await finishFile("one.txt", first, { status: "indexed" }, flush, act);
  assert(rowStates(host)[0] === "one.txt:done" && rowStates(host)[1] === "two.txt:uploading", "a live success advances to the next file");
  assert(sameCount(uploaded, 1), "a live success reports the upload");
  const second = await finishFile("two.txt", retried, { status: "skipped", reason: "binary" }, flush, act);
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:done", "two.txt:done"]), "the following file completes");
  assert(toasts.includes("two.txt was saved; extraction skipped; index pending: binary"), "a live extraction warning still toasts");

  await React.act(async () => { pausePrivateReads(); });
  await React.act(async () => { allowPrivateReads(); });
  await add(["fresh.txt"]);
  assert(!Array.from(host.querySelectorAll("button")).some((button) => button.textContent === "Retry uploads"), "a new selection after an empty pause starts without retry");
  assert(rowStates(host).includes("fresh.txt:uploading"), "a new selection after an empty pause uploads");
  await finishFile("fresh.txt", second, { status: "indexed" }, flush, act);

  const beforeBatch = calls.length;
  await React.act(async () => {
    const input = mountedInput();
    Object.defineProperty(input, "files", {
      configurable: true,
      value: fileList([new File(["batch-a"], "batch-a.txt"), new File(["batch-b"], "batch-b.txt")]),
    });
    input.dispatchEvent(new Event("change", { bubbles: true }));
    pausePrivateReads();
  });
  const openedBeforePause = calls.slice(beforeBatch).filter((call) => call.name === "batch-a.txt" || call.name === "batch-b.txt");
  assert(openedBeforePause.every((call) => call.aborted), "pause aborts a transfer the selection already opened");
  assert(JSON.stringify(rowStates(host).slice(-2)) === JSON.stringify(["batch-a.txt:queued", "batch-b.txt:queued"]), "the paused batch stays queued");
  const heldTransfer = calls.at(-1);
  await React.act(async () => { allowPrivateReads(); });
  await flush();
  assert(calls.at(-1) === heldTransfer, "resume does not open a new transfer");
  const gapRetry = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Retry uploads");
  assert(gapRetry, "the paused batch offers retry");
  await React.act(async () => { gapRetry.click(); });
  const gap = await finishFile("batch-a.txt", heldTransfer, { status: "indexed" }, flush, act);
  const gapNext = await finishFile("batch-b.txt", gap, { status: "indexed" }, flush, act);

  await add(["forbidden.txt"]);
  const forbidden = await waitCall("forbidden.txt", gapNext, flush);
  await React.act(async () => { forbidden.respond(403, { error: "operator only", check: "operator_only" }); });
  assert(host.querySelector("[data-auth-state='ready']") && host.querySelector("[data-private-workspace='wiki']"), "a role 403 leaves the admitted workspace mounted");
  assert(sessionKey() === "admitted-tab-key", "a role 403 keeps the tab credential");
  assert(rowStates(host).includes("forbidden.txt:error"), "the role refusal stays on that file");

  await add(["unrelated.txt"]);
  const unrelated = await waitCall("unrelated.txt", forbidden, flush);
  await React.act(async () => { unrelated.respond(401, { error: "not a session", code: "caller_identity" }); });
  assert(host.querySelector("[data-auth-state='ready']") && sessionKey() === "admitted-tab-key", "an unrelated 401 keeps the admitted view and key");
  assert(rowStates(host).includes("unrelated.txt:error"), "an unrelated 401 stays a file error");

  await add(["late.txt"]);
  const late = await waitCall("late.txt", unrelated, flush);
  await React.act(async () => { setSessionKey("replacement-tab-key"); });
  await flush();
  assert(host.querySelector("[data-private-workspace='wiki']"), "the replacement session verifies a new private view");
  assert(sessionKey() === "replacement-tab-key", "the replacement credential is stored");
  assert(late.aborted, "replacing the session aborts the previous transfer");
  await React.act(async () => { late.respond(401, { error: "sign in", code: "operator_session_required" }); });
  await flush();
  assert(host.querySelector("[data-auth-state='ready']") && host.querySelector("[data-private-workspace='wiki']"), "a late 401 from the aborted transfer does not remove the replacement view");
  assert(sessionKey() === "replacement-tab-key", "a late 401 does not clear the replacement credential");

  await add(["local.txt"]);
  const local = await waitCall("local.txt", late, flush);
  await React.act(async () => { local.respond(401, { error: "sign in", code: "operator_session_required" }); });
  await flush();
  assert(host.querySelector("[data-auth-state='expired']"), "a current local session 401 ends the admitted view");
  assert(!host.querySelector("[data-private-workspace='wiki']"), "a current local session 401 removes the private workspace");
  assert(sessionKey() === null, "a current local session 401 clears the tab credential");

  allowPrivateReads();
  const resetHost = document.createElement("div");
  document.body.appendChild(resetHost);
  const resetRoot = createRoot(resetHost);
  roots.push(resetRoot);
  await React.act(async () => {
    resetRoot.render(React.createElement(LocaleProvider, null, React.createElement(UploadPane, {
      dir: "global/guides",
      navHref: () => "/wiki",
      readOnly: false,
      onToast: () => {},
      onUploaded: () => {},
    })));
  });
  const resetInput = resetHost.querySelector('input[type="file"]');
  if (!(resetInput instanceof HTMLInputElement)) throw new Error("reset file input missing");
  Object.defineProperty(resetInput, "files", {
    configurable: true,
    value: fileList([new File(["keep"], "keep.txt")]),
  });
  await React.act(async () => { resetInput.dispatchEvent(new Event("change", { bubbles: true })); });
  assert(rowStates(resetHost).some((row) => row.startsWith("keep.txt:")), "the reset fixture queued the previous identity's file");
  await React.act(async () => { resetPrivateSession(); });
  assert(rowStates(resetHost).length === 0, "a confirmed reset clears queued files and labels");
  assert(!resetHost.textContent?.includes("keep.txt"), "a confirmed reset drops the previous file name");

  await React.act(async () => { setSessionKey("hosted-tab-key"); });
  await flush();
  assert(host.querySelector("[data-private-workspace='wiki']") && sessionKey() === "hosted-tab-key", "a new key verifies the workspace again");
  await add(["hosted.txt"]);
  const hosted = await waitCall("hosted.txt", local, flush);
  await React.act(async () => {
    hosted.respond(401, { ok: false, error: { code: "board_session_required", message: "sign in through your workspace" } });
  });
  await flush();
  assert(host.querySelector("[data-auth-state='expired']") && !host.querySelector("[data-private-workspace='wiki']"), "a current hosted session 401 removes the private workspace");
  assert(sessionKey() === null, "a current hosted session 401 clears the tab credential");

  console.log("wiki upload session checks passed");
  } finally {
    for (const owned of roots) owned.unmount();
    win.close();
    loader.prototype.require = originalRequire;
  }
}

void main().catch((error: unknown) => {
  console.error(error);
  require<{ exitCode: number }>("process").exitCode = 1;
});
