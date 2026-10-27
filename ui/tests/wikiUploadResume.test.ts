/**
 * An import that is in flight is not an interrupted import. `importFile`
 * records a resume entry as soon as `begin` answers, so the pane must not
 * offer to continue or forget the transfer it is running; a record left by an
 * earlier session still has to be offered, and finishing clears the offer.
 */
import type * as ReactModule from "react";
import type * as AuthBoundaryModule from "../src/features/auth/AuthBoundary";
import type * as UploadPaneModule from "../src/features/wiki/UploadPane";
import type * as TransferModule from "../src/features/wiki/transfer";
import type * as SessionKeyModule from "../src/lib/sessionKey";

declare function require<T = unknown>(name: string): T;

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

interface PendingCall {
  name: string;
  op: string;
  aborted: boolean;
  settled: boolean;
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
        aborted: false,
        settled: false,
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

const FORGET = "Forget this interrupted import on this browser";

function offersResume(host: HTMLElement): boolean {
  return Array.from(host.querySelectorAll("button")).some((button) => button.textContent === FORGET);
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
  const Transfer = require<typeof TransferModule>("../src/features/wiki/transfer");
  const { setSessionKey } = require<typeof SessionKeyModule>("../src/lib/sessionKey");

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
    setSessionKey("admitted-tab-key");
    // A record left behind by an earlier session, present when this pane
    // mounts.
    Transfer.writeResume({
      job_id: "stale-job",
      digest: "stale-digest",
      root: "global/guides",
      update: false,
      id: "file",
      path: "stale.txt",
      kind: "text",
      size: 4,
      sha256: "stale-sha",
      if_rev: "none",
    });
    const flush = async () => { await React.act(async () => { await Promise.resolve(); }); };
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(AuthBoundary, {
        children: () => React.createElement("div", { "data-private-workspace": "wiki" }, React.createElement(UploadPane, {
          dir: "global/guides",
          navHref: () => "/wiki",
          readOnly: false,
          onToast: () => {},
          onUploaded: () => {},
        })),
      })));
    });
    await flush();
    assert(host.querySelector("[data-private-workspace='wiki']"), "verified metadata mounts the private upload workspace");

    // The record from the earlier session is offered, and forgetting works.
    assert(offersResume(host), "a resume record from an earlier session is offered");
    const forget = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === FORGET);
    assert(forget, "the offered record comes with a forget control");
    await React.act(async () => { forget.click(); });
    assert(!offersResume(host) && Transfer.readResume("global/guides").length === 0, "forgetting clears the record");

    const add = async (names: string[]) => {
      const input = host.querySelector('input[type="file"]');
      if (!(input instanceof HTMLInputElement) || !input.isConnected) throw new Error("file input missing");
      Object.defineProperty(input, "files", {
        configurable: true,
        value: fileList(names.map((name) => new File([name], name))),
      });
      await React.act(async () => { input.dispatchEvent(new Event("change", { bubbles: true })); });
    };
    const act = (fn: () => void) => React.act(async () => { fn(); });

    // The running import records itself, but is not offered as interrupted.
    await add(["one.txt"]);
    const begin = await waitCall("one.txt", undefined, flush);
    await act(() => { begin.respond(200, { job_id: "job-one", digest: "digest-one", root: "global/guides", source_id: "board", state: "open", entries: 1 }); });
    const status = await waitCall("one.txt", begin, flush);
    await act(() => {
      status.respond(200, {
        job_id: "job-one",
        digest: "digest-one",
        source_id: "board",
        root: "global/guides",
        state: "open",
        index: "pending",
        entries: [{ entry_id: "file", path: "one.txt", received: 0, size: 7, outcome: "pending", extraction: "not_applicable" }],
        next_offset: null,
      });
    });
    await flush();
    assert(Transfer.readResume("global/guides").length === 1, "the running import recorded a resume entry");
    assert(rowStates(host).includes("one.txt:uploading"), "the import is in flight with a resume entry on disk");
    assert(!offersResume(host), "an in-flight import is not offered as an interrupted one");

    // Finishing leaves no offer either.
    const chunk = await waitCall("one.txt", status, flush);
    await act(() => { chunk.respond(200, { job_id: "job-one", entry_id: "file", received: 7, size: 7 }); });
    const applied = await waitCall("one.txt", chunk, flush);
    await act(() => { applied.respond(200, { job_id: "job-one", entry_id: "file", path: "one.txt", outcome: "applied", extraction: "indexed", rev: "fnv1a:0123456789abcdef" }); });
    const finish = await waitCall("one.txt", applied, flush);
    await act(() => { finish.respond(200, { job_id: "job-one", digest: "digest-one", root: "global/guides", state: "complete", index: "covered" }); });
    await flush();
    assert(rowStates(host).includes("one.txt:done"), "the import finishes");
    assert(!offersResume(host), "a finished import is not offered as an interrupted one");

    console.log("wiki upload resume checks passed");
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
