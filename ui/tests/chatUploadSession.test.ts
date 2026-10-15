/**
 * Chat file upload through the admitted workspace. Completing the live XHR
 * with a known local or hosted 401 removes the private view and tab key.
 * A role 403 and an unrelated 401 leave both in place. A 401 after the
 * transfer was aborted for a replacement session does not.
 */
import type * as ReactModule from "react";
import type * as AuthBoundaryModule from "../src/features/auth/AuthBoundary";
import type * as ApiModule from "../src/lib/api";
import type * as SessionKeyModule from "../src/lib/sessionKey";

declare function require<T = unknown>(name: string): T;

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
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

const receipt = {
  id: `chf-${"a".repeat(32)}`,
  sha256: "b".repeat(64),
  size: 4,
  mime: "text/plain",
  name: "notes.txt",
  scope: "home",
  context_id: "",
  uploader: "operator",
  user_id: `acu-${"c".repeat(32)}`,
  thread_id: "thread-1",
  location_version: 1 as const,
  created: 1,
};

async function main(): Promise<void> {
  type DomConstructors = Pick<typeof globalThis, "Node" | "HTMLElement" | "HTMLInputElement" | "HTMLButtonElement" | "Event">;
  const { Window } = require<{ Window: new (options: { url: string }) => Window & DomConstructors }>("happy-dom");
  const win = new Window({ url: "http://localhost/" });
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
  const { api } = require<typeof ApiModule>("../src/lib/api");
  const { sessionKey, setSessionKey } = require<typeof SessionKeyModule>("../src/lib/sessionKey");

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

  const file = new File(["note"], "notes.txt");
  function Uploader() {
    const [label, setLabel] = React.useState("idle");
    return React.createElement("section", { "data-private-workspace": "chat" },
      React.createElement("button", {
        onClick: () => {
          setLabel("uploading");
          void api.chatFileUpload(file).then(
            (row) => setLabel(`saved:${row.id}`),
            (error: unknown) => setLabel(error instanceof DOMException ? "canceled" : "refused"),
          );
        },
      }, "Upload notes"),
      React.createElement("output", null, label));
  }

  const host = document.createElement("div");
  document.body.appendChild(host);
  const roots: { unmount: () => void }[] = [];
  try {
  const root = createRoot(host);
  roots.push(root);
  setSessionKey("admitted-tab-key");
  const flush = async () => { await React.act(async () => { await Promise.resolve(); }); };
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(AuthBoundary, {
      children: () => React.createElement(Uploader),
    })));
  });
  await flush();
  const upload = () => {
    const button = Array.from(host.querySelectorAll("button")).find((item) => item.textContent === "Upload notes");
    if (!(button instanceof HTMLButtonElement) || !button.isConnected) throw new Error("the admitted workspace offers chat upload");
    return React.act(async () => { button.click(); });
  };
  const output = () => host.querySelector("output")?.textContent ?? "";
  assert(host.querySelector("[data-private-workspace='chat']") && sessionKey() === "admitted-tab-key", "verified metadata mounts the private chat workspace");

  await upload();
  const saved = FakeXHR.sent.at(-1);
  if (!saved) throw new Error("missing saved transfer");
  await React.act(async () => { saved.respond(200, receipt); });
  assert(output() === `saved:${receipt.id}`, "a live chat upload shows the minted row");
  assert(host.querySelector("[data-auth-state='ready']") && sessionKey() === "admitted-tab-key", "a saved upload keeps the admitted view and key");

  await upload();
  const forbidden = FakeXHR.sent.at(-1);
  if (!forbidden) throw new Error("missing role refusal");
  await React.act(async () => { forbidden.respond(403, { error: "operator only", check: "operator_only" }); });
  assert(output() === "refused", "a role 403 stays a feature refusal");
  assert(host.querySelector("[data-private-workspace='chat']") && sessionKey() === "admitted-tab-key", "a role 403 keeps the admitted view and key");

  await upload();
  const unrelated = FakeXHR.sent.at(-1);
  if (!unrelated) throw new Error("missing unrelated refusal");
  await React.act(async () => { unrelated.respond(401, { error: "caller not attributed", code: "caller_identity" }); });
  assert(output() === "refused", "an unrelated 401 stays a feature refusal");
  assert(host.querySelector("[data-auth-state='ready']") && sessionKey() === "admitted-tab-key", "an unrelated 401 keeps the admitted view and key");

  await upload();
  const late = FakeXHR.sent.at(-1);
  if (!late) throw new Error("missing late transfer");
  await React.act(async () => { setSessionKey("replacement-tab-key"); });
  await flush();
  assert(host.querySelector("[data-private-workspace='chat']") && sessionKey() === "replacement-tab-key", "the replacement session has its own admitted view");
  assert(late.aborted, "the replacement session aborts the previous chat upload");
  await React.act(async () => { late.respond(401, { error: "stale", code: "operator_session_required" }); });
  await flush();
  assert(host.querySelector("[data-auth-state='ready']") && host.querySelector("[data-private-workspace='chat']"), "a late aborted 401 does not remove the replacement view");
  assert(sessionKey() === "replacement-tab-key", "a late aborted 401 does not clear the replacement credential");

  await upload();
  const local = FakeXHR.sent.at(-1);
  if (!local || local === late || local.aborted) throw new Error("missing local refusal");
  await React.act(async () => { local.respond(401, { error: "a local board session is required", code: "operator_session_required" }); });
  await flush();
  assert(host.querySelector("[data-auth-state='expired']") && !host.querySelector("[data-private-workspace='chat']"), "a current local 401 removes the private view");
  assert(sessionKey() === null, "a current local 401 clears the tab credential");

  await React.act(async () => { setSessionKey("hosted-tab-key"); });
  await flush();
  assert(host.querySelector("[data-private-workspace='chat']"), "a replacement key admits the workspace again");
  await upload();
  const hosted = FakeXHR.sent.at(-1);
  if (!hosted || hosted === local || hosted.aborted) throw new Error("missing hosted refusal");
  await React.act(async () => {
    hosted.respond(401, { ok: false, error: { code: "board_session_required", message: "sign in through your workspace" } });
  });
  await flush();
  assert(host.querySelector("[data-auth-state='expired']") && !host.querySelector("[data-private-workspace='chat']"), "a current hosted 401 removes the private view");
  assert(sessionKey() === null, "a current hosted 401 clears the tab credential");

  console.log("chat upload session checks passed");
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
