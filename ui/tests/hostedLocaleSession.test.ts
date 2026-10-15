import type * as ReactTypes from "react";
import type { Root } from "react-dom/client";
import type * as AuthBoundaryModule from "../src/features/auth/AuthBoundary";
import type * as LocaleModule from "../src/lib/locale";
import type * as PrivateSessionModule from "../src/lib/privateSession";

declare function require<T>(name: string): T;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => { resolve = complete; });
  return { promise, resolve };
}

async function main(): Promise<void> {
  type Dom = Window & Pick<typeof globalThis, "Node" | "HTMLElement" | "HTMLInputElement" | "HTMLButtonElement" | "Event">;
  const { Window: BrowserWindow } = require<{ Window: new (options: { url: string }) => Dom }>("happy-dom");
  const win = new BrowserWindow({ url: "http://localhost/wiki" });
  Object.defineProperties(globalThis, {
    window: { value: win, configurable: true },
    document: { value: win.document, configurable: true },
    navigator: { value: win.navigator, configurable: true },
    location: { value: win.location, configurable: true },
    history: { value: win.history, configurable: true },
    sessionStorage: { value: win.sessionStorage, configurable: true },
    localStorage: { value: win.localStorage, configurable: true },
    Node: { value: win.Node, configurable: true },
    HTMLElement: { value: win.HTMLElement, configurable: true },
    HTMLInputElement: { value: win.HTMLInputElement, configurable: true },
    HTMLButtonElement: { value: win.HTMLButtonElement, configurable: true },
    Event: { value: win.Event, configurable: true },
    addEventListener: { value: win.addEventListener.bind(win), configurable: true },
    removeEventListener: { value: win.removeEventListener.bind(win), configurable: true },
    IS_REACT_ACT_ENVIRONMENT: { value: true, configurable: true },
  });
  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };
  const React = require<typeof ReactTypes>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const { LocaleProvider, useLocale } = require<typeof LocaleModule>("../src/lib/locale");
  const AuthBoundary = require<typeof AuthBoundaryModule>("../src/features/auth/AuthBoundary").default;
  const { resetPrivateSession } = require<typeof PrivateSessionModule>("../src/lib/privateSession");
  const roots: Root[] = [];
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const preferences = { organization_locale: "en", user_locale: "en" };
  const chinese = { organization_locale: "en", user_locale: "zh-TW" };
  let actor = "first-member";
  let unavailable = false;
  let read: (signal: AbortSignal | null | undefined) => Promise<Response> = async () => json(preferences);
  let write: (signal: AbortSignal | null | undefined) => Promise<Response> = async () => json(chinese);
  const save: { result: "idle" | "pending" | "saved" | "failed" } = { result: "idle" };
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : String(input), "http://localhost");
    if (url.pathname === "/api/meta") {
      if (unavailable) return json({ error: "Verification unavailable" }, 503);
      return json({
        read_only: false, signed_in: true, actor, hosted: true, operator: false,
        tailnet_url: null, version: "0", build_commit: "x", build_time: "t",
        session: {
          id: `session-${actor}`, origin: "public", created: 1700000000, last_used: 1700000000,
          idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "synthetic",
          user: { sub: actor, name: actor, email: `${actor}@example.test`, handle: actor, role: "member" },
        },
      });
    }
    if (url.pathname === "/api/locale") return init?.method === "POST" ? write(init.signal) : read(init?.signal);
    throw new Error(`Unexpected locale acceptance request ${url.pathname}`);
  };

  function Consumer() {
    const { locale, userLocale, setPreference } = useLocale();
    return React.createElement("section", { "data-private-locale": true },
      React.createElement("output", { "data-language": true }, `${locale}|${userLocale ?? "none"}`),
      React.createElement("button", { onClick: () => {
        save.result = "pending";
        void setPreference("user", "zh-TW").then(() => { save.result = "saved"; }, () => { save.result = "failed"; });
      } }, "Save Chinese"),
    );
  }
  const { setImmediate: nextTurn } = require<{ setImmediate: (callback: () => void) => void }>("node:timers");
  // Yield for native Response.clone stream tasks and React's scheduler, not elapsed time.
  const flush = () => React.act(async () => { await new Promise<void>(nextTurn); });
  async function settle(check: () => boolean, message: string) {
    for (let attempt = 0; attempt < 40; attempt += 1) {
      await flush();
      if (check()) return;
    }
    throw new Error(message);
  }
  async function mount() {
    resetPrivateSession();
    actor = "first-member";
    unavailable = false;
    save.result = "idle";
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    roots.push(root);
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(AuthBoundary, {
        children: () => React.createElement(Consumer),
      })));
    });
    return { host, root };
  }
  async function clickSave(host: Element) {
    const button = Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find((node) => node.textContent === "Save Chinese");
    assert(button, "the admitted locale consumer offers a save action");
    await React.act(async () => { button.click(); });
  }
  async function replaceAfterUncertainty(host: Element) {
    unavailable = true;
    await React.act(async () => { win.dispatchEvent(new Event("focus")); });
    await settle(() => Boolean(host.querySelector('[data-auth-state="unavailable"]')), "uncertain proof conceals the locale consumer");
    actor = "next-member";
    unavailable = false;
    const retry = Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find((node) => node.textContent === "Retry verification");
    assert(retry, "verification offers explicit retry");
    await React.act(async () => { retry.click(); });
    await settle(() => host.querySelector("[data-language]")?.textContent === "en|en", "the replacement member loads its own preferences");
  }
  function heldBody() {
    const body = deferred<unknown>();
    return {
      body,
      response: new Response(new ReadableStream<Uint8Array>({ start(controller) {
        void body.promise.then((value) => {
          controller.enqueue(new TextEncoder().encode(JSON.stringify(value)));
          controller.close();
        });
      } }), { headers: { "Content-Type": "application/json" } }),
    };
  }

  try {
    read = async () => json({ error: { code: "board_session_required", message: "Sign in required" } }, 401);
    const refusedRead = await mount();
    await settle(() => Boolean(refusedRead.host.querySelector('[data-auth-state="expired"]')), "a current hosted locale read401 immediately ends admission");
    assert(!refusedRead.host.querySelector("[data-private-locale]"), "refused locale reads remove private content");
    await React.act(async () => { refusedRead.root.unmount(); });

    read = async () => json(preferences);
    write = async () => json({ error: { code: "board_session_required", message: "Sign in required" } }, 401);
    const refusedWrite = await mount();
    await settle(() => refusedWrite.host.querySelector("[data-language]")?.textContent === "en|en", "hosted preferences load before saving");
    await clickSave(refusedWrite.host);
    await settle(() => Boolean(refusedWrite.host.querySelector('[data-auth-state="expired"]')), "a current hosted locale write401 immediately ends admission");
    assert(!refusedWrite.host.querySelector("[data-private-locale]"), "refused locale writes remove private content");
    await React.act(async () => { refusedWrite.root.unmount(); });

    for (const refusal of [{ status: 403, body: { error: "Member only" } }, { status: 401, body: { error: "Feature credential required", code: "provider_credential_required" } }]) {
      read = async () => json(preferences);
      write = async () => json(refusal.body, refusal.status);
      const ordinary = await mount();
      await settle(() => ordinary.host.querySelector("[data-language]")?.textContent === "en|en", "ordinary-refusal case is admitted");
      await clickSave(ordinary.host);
      await settle(() => save.result === "failed", "an ordinary feature refusal rejects the save");
      assert(ordinary.host.querySelector('[data-auth-state="ready"] [data-private-locale]'), "ordinary403 and unrelated401 keep the board admitted");
      assert(ordinary.host.querySelector("[data-language]")?.textContent === "en|en", "refused saves do not replace preferences");
      await React.act(async () => { ordinary.root.unmount(); });
    }

    const oldRead = heldBody();
    let readSignal: AbortSignal | null | undefined;
    let firstRead = true;
    read = async (signal) => {
      if (!firstRead) return json(preferences);
      firstRead = false;
      readSignal = signal;
      return oldRead.response;
    };
    const pendingRead = await mount();
    await settle(() => Boolean(pendingRead.host.querySelector("[data-private-locale]")), "the old member is admitted with its locale body held");
    await replaceAfterUncertainty(pendingRead.host);
    assert(readSignal?.aborted, "uncertain verification aborts the underlying locale read through body consumption");
    await React.act(async () => { oldRead.body.resolve(chinese); });
    await flush();
    assert(pendingRead.host.querySelector("[data-language]")?.textContent === "en|en", "a retired read body cannot overwrite the replacement member's preferences");
    await React.act(async () => { pendingRead.root.unmount(); });

    read = async () => json(preferences);
    const oldWrite = heldBody();
    let writeSignal: AbortSignal | null | undefined;
    write = async (signal) => { writeSignal = signal; return oldWrite.response; };
    const pendingWrite = await mount();
    await settle(() => pendingWrite.host.querySelector("[data-language]")?.textContent === "en|en", "held-write case is admitted");
    await clickSave(pendingWrite.host);
    await replaceAfterUncertainty(pendingWrite.host);
    assert(writeSignal?.aborted, "uncertain verification aborts a held locale save");
    await React.act(async () => { oldWrite.body.resolve(chinese); });
    await settle(() => save.result === "failed", "a retired write is not reported as a successful save");
    assert(pendingWrite.host.querySelector("[data-language]")?.textContent === "en|en", "a retired save body cannot overwrite the replacement member's preferences");
    await React.act(async () => { pendingWrite.root.unmount(); });

    write = async () => json(chinese);
    const live = await mount();
    await settle(() => live.host.querySelector("[data-language]")?.textContent === "en|en", "live hosted preference read is visible");
    await clickSave(live.host);
    await settle(() => live.host.querySelector("[data-language]")?.textContent === "zh-TW|zh-TW", "a live save applies the verified user preference");
    assert(live.host.querySelector('[data-auth-state="ready"] [data-private-locale]'), "a live locale save keeps the authenticated workspace");
    console.log("hosted locale session checks passed");
  } finally {
    await React.act(async () => { for (const root of roots) root.unmount(); });
    win.close();
    loader.prototype.require = originalRequire;
  }
}

void main().catch((error: unknown) => {
  console.error(error);
  require<{ exitCode?: number }>("process").exitCode = 1;
});
