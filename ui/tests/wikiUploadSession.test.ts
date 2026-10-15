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

class FakeXHR {
  static sent: FakeXHR[] = [];
  status = 0;
  statusText = "";
  responseText = "";
  aborted = false;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  upload: { onprogress: ((event: ProgressEvent) => void) | null } = { onprogress: null };
  open(_method: string, _url: string): void {}
  setRequestHeader(_name: string, _value: string): void {}
  send(_body: unknown): void { FakeXHR.sent.push(this); }
  abort(): void {
    if (this.aborted) return;
    this.aborted = true;
    this.onabort?.();
  }
  respond(status: number, body: unknown): void {
    this.status = status;
    this.statusText = status >= 200 && status < 300 ? "OK" : "Error";
    this.responseText = JSON.stringify(body);
    this.onload?.();
  }
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
  const { Window } = require<{ Window: new (options: { url: string }) => Window }>("happy-dom");
  const win = new Window({ url: "http://localhost/wiki" });
  for (const name of ["window", "document", "navigator", "Node", "HTMLElement", "HTMLInputElement", "Event", "location", "history", "sessionStorage", "localStorage"] as const) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
  }
  for (const name of ["addEventListener", "removeEventListener"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "XMLHttpRequest", { value: FakeXHR, configurable: true });
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
  globalThis.fetch = async (input: RequestInfo | URL) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/meta") {
      return new Response(JSON.stringify(signedIn), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    throw new Error(`unexpected ${url.pathname}`);
  };

  const host = document.createElement("div");
  document.body.appendChild(host);
  let root: ReturnType<typeof createRoot> | undefined;
  let resetRoot: ReturnType<typeof createRoot> | undefined;
  try {
  root = createRoot(host);
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

  const input = host.querySelector('input[type="file"]');
  if (!(input instanceof HTMLInputElement)) throw new Error("file input missing");
  const add = async (names: string[]) => {
    Object.defineProperty(input, "files", {
      configurable: true,
      value: fileList(names.map((name) => new File([name], name))),
    });
    await React.act(async () => { input.dispatchEvent(new Event("change", { bubbles: true })); });
  };
  await add(["one.txt", "two.txt"]);
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:uploading", "two.txt:queued"]), "the first file is in flight and the next stays queued");
  const opened = FakeXHR.sent.length;
  const first = FakeXHR.sent.at(-1);
  if (!first) throw new Error("missing first transfer");

  await React.act(async () => { pausePrivateReads(); });
  assert(first.aborted, "hiding the admitted workspace aborts the live transfer");
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:queued", "two.txt:queued"]), "both files stay queued for an explicit retry");
  await React.act(async () => { first.respond(200, { extraction: { status: "indexed" } }); });
  assert(sameCount(uploaded, 0) && sameCount(toasts.length, 0), "the hidden completion does not finish or toast");
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:queued", "two.txt:queued"]), "the hidden completion does not mark a retained row done");
  await React.act(async () => { allowPrivateReads(); });
  await flush();
  assert(FakeXHR.sent.length === opened, "resume does not open the next transfer");
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:queued", "two.txt:queued"]), "resume leaves the queue waiting");
  assert(sessionKey() === "admitted-tab-key" && host.querySelector("[data-private-workspace='wiki']"), "pause and resume keep the admitted view and key");

  const retry = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Retry uploads");
  assert(retry, "the held queue offers retry");
  await React.act(async () => { retry.click(); });
  assert(rowStates(host)[0] === "one.txt:uploading" && rowStates(host)[1] === "two.txt:queued", "retry uploads the abandoned file and holds the next");
  const retried = FakeXHR.sent.at(-1);
  if (!retried || retried === first) throw new Error("retry did not open a transfer");
  await React.act(async () => { retried.respond(200, { extraction: { status: "indexed" } }); });
  assert(rowStates(host)[0] === "one.txt:done" && rowStates(host)[1] === "two.txt:uploading", "a live success advances to the next file");
  assert(sameCount(uploaded, 1), "a live success reports the upload");
  const second = FakeXHR.sent.at(-1);
  if (!second || second === retried) throw new Error("the second file did not start after success");
  await React.act(async () => { second.respond(200, { extraction: { status: "skipped", reason: "binary" } }); });
  assert(JSON.stringify(rowStates(host)) === JSON.stringify(["one.txt:done", "two.txt:done"]), "the following file completes");
  assert(toasts.includes("two.txt was saved; text extraction skipped: binary"), "a live extraction warning still toasts");

  await React.act(async () => { pausePrivateReads(); });
  await React.act(async () => { allowPrivateReads(); });
  const beforeFresh = FakeXHR.sent.length;
  await add(["fresh.txt"]);
  assert(!Array.from(host.querySelectorAll("button")).some((button) => button.textContent === "Retry uploads"), "a new selection after an empty pause starts without retry");
  assert(rowStates(host).includes("fresh.txt:uploading"), "a new selection after an empty pause uploads");
  const fresh = FakeXHR.sent.at(-1);
  if (!fresh || FakeXHR.sent.length === beforeFresh) throw new Error("fresh selection did not open a transfer");
  await React.act(async () => { fresh.respond(200, { extraction: { status: "indexed" } }); });

  const beforeGap = FakeXHR.sent.length;
  await React.act(async () => {
    Object.defineProperty(input, "files", {
      configurable: true,
      value: fileList([new File(["batch-a"], "batch-a.txt"), new File(["batch-b"], "batch-b.txt")]),
    });
    input.dispatchEvent(new Event("change", { bubbles: true }));
    pausePrivateReads();
  });
  assert(FakeXHR.sent.length === beforeGap, "a queued batch paused before the next transfer does not start");
  assert(JSON.stringify(rowStates(host).slice(-2)) === JSON.stringify(["batch-a.txt:queued", "batch-b.txt:queued"]), "the paused batch stays queued");
  await React.act(async () => { allowPrivateReads(); });
  await flush();
  assert(FakeXHR.sent.length === beforeGap, "re-admission does not start a batch paused before its transfer");
  const gapRetry = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Retry uploads");
  assert(gapRetry, "the paused batch offers retry");
  await React.act(async () => { gapRetry.click(); });
  const gap = FakeXHR.sent.at(-1);
  if (!gap) throw new Error("gap retry did not open a transfer");
  await React.act(async () => { gap.respond(200, { extraction: { status: "indexed" } }); });
  const gapNext = FakeXHR.sent.at(-1);
  if (!gapNext || gapNext === gap) throw new Error("the second paused file did not start after retry");
  await React.act(async () => { gapNext.respond(200, { extraction: { status: "indexed" } }); });

  await add(["forbidden.txt"]);
  const forbidden = FakeXHR.sent.at(-1);
  if (!forbidden) throw new Error("missing role refusal");
  await React.act(async () => { forbidden.respond(403, { error: "operator only", check: "operator_only" }); });
  assert(host.querySelector("[data-auth-state='ready']") && host.querySelector("[data-private-workspace='wiki']"), "a role 403 leaves the admitted workspace mounted");
  assert(sessionKey() === "admitted-tab-key", "a role 403 keeps the tab credential");
  assert(rowStates(host).includes("forbidden.txt:error"), "the role refusal stays on that file");

  await add(["unrelated.txt"]);
  const unrelated = FakeXHR.sent.at(-1);
  if (!unrelated) throw new Error("missing unrelated refusal");
  await React.act(async () => { unrelated.respond(401, { error: "not a session", code: "caller_identity" }); });
  assert(host.querySelector("[data-auth-state='ready']") && sessionKey() === "admitted-tab-key", "an unrelated 401 keeps the admitted view and key");
  assert(rowStates(host).includes("unrelated.txt:error"), "an unrelated 401 stays a file error");

  await add(["late.txt"]);
  const late = FakeXHR.sent.at(-1);
  if (!late) throw new Error("missing late transfer");
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
  const local = FakeXHR.sent.at(-1);
  if (!local) throw new Error("missing local refusal");
  await React.act(async () => { local.respond(401, { error: "sign in", code: "operator_session_required" }); });
  await flush();
  assert(host.querySelector("[data-auth-state='expired']"), "a current local session 401 ends the admitted view");
  assert(!host.querySelector("[data-private-workspace='wiki']"), "a current local session 401 removes the private workspace");
  assert(sessionKey() === null, "a current local session 401 clears the tab credential");

  allowPrivateReads();
  const resetHost = document.createElement("div");
  document.body.appendChild(resetHost);
  resetRoot = createRoot(resetHost);
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
  const hosted = FakeXHR.sent.at(-1);
  if (!hosted) throw new Error("missing hosted refusal");
  await React.act(async () => {
    hosted.respond(401, { ok: false, error: { code: "board_session_required", message: "sign in through your workspace" } });
  });
  await flush();
  assert(host.querySelector("[data-auth-state='expired']") && !host.querySelector("[data-private-workspace='wiki']"), "a current hosted session 401 removes the private workspace");
  assert(sessionKey() === null, "a current hosted session 401 clears the tab credential");

  console.log("wiki upload session checks passed");
  } finally {
    root?.unmount();
    resetRoot?.unmount();
    win.close();
    loader.prototype.require = originalRequire;
  }
}

void main().catch((error: unknown) => {
  console.error(error);
  require<{ exitCode: number }>("process").exitCode = 1;
});
