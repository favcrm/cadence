import type { Meta } from "../src/lib/types";
import { composerBlock, type MasterStatus } from "../src/features/home/master";
import type * as ReactModule from "react";
import type * as AuthBoundaryModule from "../src/features/auth/AuthBoundary";
import type * as ApiModule from "../src/lib/api";
import type * as CacheModule from "../src/lib/cache";
import type * as SessionKeyModule from "../src/lib/sessionKey";
import type * as PrivateSessionModule from "../src/lib/privateSession";
import type * as AuthSessionModule from "../src/features/auth/session";
import type * as LocationModule from "../src/lib/useLocation";

declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "HTMLElement", "location", "history", "sessionStorage", "Event", "MouseEvent"]) {
  Object.defineProperty(globalThis, key, { value: key === "window" ? win : win[key], configurable: true });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
for (const key of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, key, { value: win[key].bind(win), configurable: true });
const moduleLoader = require("module");
const originalRequire = moduleLoader.prototype.require;
moduleLoader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { createElement, act } = require("react");
const { createRoot } = require("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { default: AccountMenu } = require("../src/ui/AccountMenu") as typeof import("../src/ui/AccountMenu");

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const base = { read_only: false, signed_in: true, actor: "operator (ui)", tailnet_url: null, version: "0", build_commit: "x", build_time: "t" };
for (const role of ["member", "operator"]) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const meta = { ...base, session: { id: "safe-session", origin: "public", user: { name: "Fable Chen", email: "fable@example.com", role } } } as unknown as Meta;
  act(() => root.render(createElement(LocaleProvider, null, createElement(AccountMenu, { meta, actor: "x", mayWrite: true, onChange: () => undefined, trigger: "avatar", placement: "below-end" }))));
  act(() => { host.querySelector("button")?.click(); });
  const panel = host.querySelector("[role=dialog]");
  assert(panel?.textContent?.includes("Fable Chen"), "account menu must name the person");
  assert(panel?.textContent?.includes("fable@example.com") && panel.textContent.includes(role), "account menu must show email and mapped role");
  if (role === "member") assert(!panel?.textContent?.includes("operator"), "member must never be labelled operator");
  act(() => root.unmount());
  host.remove();
}

const hostedBlock = composerBlock as (block: string | null, status: MasterStatus, hosted?: boolean | null) => string | null;
for (const status of [{ kind: "absent" }, { kind: "stopped", label: "stopped" }] as MasterStatus[]) {
  const copy = hostedBlock(null, status, true) ?? "";
  assert(!copy.includes("cadence") && !copy.includes("terminal"), "hosted state must not ask for host shell access");
  assert(/unavailable/i.test(copy), "hosted state must report unavailable without inventing a starting operation");
  assert(!/starting|retry/i.test(copy), "no imaginary startup or retry action");
}
assert(hostedBlock(null, { kind: "absent" }, false)?.includes("cadence master start"), "local startup command stays available");
assert(!hostedBlock(null, { kind: "absent" }, null)?.includes("cadence"), "loading session mode must not flash hosted users a terminal command");
console.log("hosted identity checks passed");

// Exercise the actual boundary through metadata responses, not a parallel
// admission predicate: a transient outage preserves drafts; an expiry does not.
async function boundaryChecks() {
  const React = require("react") as typeof ReactModule;
  const AuthBoundary = (require("../src/features/auth/AuthBoundary") as typeof AuthBoundaryModule).default;
  const { api } = require("../src/lib/api") as typeof ApiModule;
  const { QueryCache } = require("../src/lib/cache") as typeof CacheModule;
  const { sessionKey, setSessionKey, SESSION_HEADER } = require("../src/lib/sessionKey") as typeof SessionKeyModule;
  const { privateSessionGeneration, privateReadsAllowed } = require("../src/lib/privateSession") as typeof PrivateSessionModule;
  const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
  const signedIn = { ...base, hosted: false, operator: false, session: { id: "local-member", origin: "loopback", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test", user: { sub: "member", name: "Member", email: "member@example.test", handle: "member", role: "member" } } };
  let response: () => Response | Promise<Response> = () => json(signedIn);
  let projectsResponse = () => json({ projects: [{ key: "private-project" }] });
  const proofHeaders: (string | null)[] = [];
  let lateRead: ((response: Response) => void) | undefined;
  let privateRequests = 0, effects = 0, cleanups = 0;
  globalThis.fetch = async (input, init) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/meta") {
      assert(url.searchParams.get("operator") === "1", "every boundary verification requests full metadata proof");
      assert(init?.signal, "metadata verification is abortable");
      proofHeaders.push(new Headers(init?.headers).get(SESSION_HEADER));
      return response();
    }
    privateRequests++;
    if (url.pathname === "/api/projects") return projectsResponse();
    if (url.pathname === "/api/issues") return new Promise<Response>((resolve) => { lateRead = resolve; });
    throw new Error(`Unexpected boundary request ${url.pathname}`);
  };
  const cache = new QueryCache();
  const projects = cache.resource("boundary-projects", api.projects);
  function PrivateWorkspace() {
    const [draft, setDraft] = React.useState("");
    React.useEffect(() => {
      effects++;
      void projects.refresh();
      return () => { cleanups++; };
    }, []);
    return React.createElement("section", { "data-private-workspace": true },
      React.createElement("button", { onClick: () => setDraft("unsaved private draft") }, "Edit draft"),
      React.createElement("output", null, draft));
  }
  const host = document.createElement("div");
  document.body.append(host);
  let root = createRoot(host);
  const flush = async () => {
    await React.act(async () => { await Promise.resolve(); });
  };
  const render = async () => {
    await React.act(async () => root.render(React.createElement(LocaleProvider, null,
      React.createElement(AuthBoundary, { children: () => React.createElement(PrivateWorkspace) }))));
    await flush();
  };
  const focus = async () => {
    await React.act(async () => window.dispatchEvent(new Event("focus")));
    await flush();
  };
  const retry = async () => {
    const button = Array.from(host.querySelectorAll("button")).find((button) => button.textContent?.trim() === "Retry verification");
    assert(button, "unavailable state offers verification retry");
    await React.act(async () => button.click());
    await flush();
  };
  try {
    // Seed storage directly, as on reload: no synthetic sign-in event or TTL.
    sessionStorage.setItem("cadence.sessionKey", "existing-local-key");
    const generation = privateSessionGeneration();
    response = () => json({ error: "bootstrap unavailable" }, 503);
    await render();
    assert(host.querySelector("[data-auth-state='unavailable'] h1"), "initial 503 is unknown, not signed out");
    assert(!host.querySelector("[data-private-workspace]") && privateRequests === 0, "unknown initial identity starts no private work");
    assert(sessionKey() === "existing-local-key", "initial 503 retains the stored local credential");
    assert(privateSessionGeneration() === generation, "an outage is not confirmed identity loss");
    response = () => json(signedIn);
    await retry();
    assert(host.querySelector("[data-private-workspace]"), "retry admits a verified local member");
    assert(proofHeaders.findIndex((key) => key !== "existing-local-key") === -1, "reload and retries keep sending the same tab key");
    assert(projects.get().data?.projects[0]?.key === "private-project", "admitted private query loads its cache");
    await React.act(async () => (host.querySelector("[data-private-workspace] button") as HTMLButtonElement).click());
    assert(host.querySelector("output")?.textContent === "unsaved private draft", "workspace holds an unsaved draft");

    response = () => json({ error: "temporary outage" }, 503);
    const beforeOutage = privateSessionGeneration(), requestsBeforeOutage = privateRequests;
    await focus();
    assert(host.querySelector("[data-auth-state='unavailable'] h1"), "refresh outage shows unavailable verification");
    assert(!privateReadsAllowed() && cleanups > 0, "unknown state pauses reads and cleans up private effects");
    assert(sessionKey() === "existing-local-key" && privateSessionGeneration() === beforeOutage, "transient uncertainty neither clears key nor resets identity");
    assert(privateRequests === requestsBeforeOutage, "verification outage causes no new private fetches");
    response = () => json(signedIn);
    await retry();
    assert(host.querySelector("output")?.textContent === "unsaved private draft", "same-session recovery restores the in-memory draft");
    assert(effects > 1, "private effects resume after recovery");

    // A newer authoritative refusal supersedes an already validated but delayed
    // metadata response. Neither envelope may restore the old private draft.
    for (const refusal of [
      { error: { message: "Session required", code: "board_session_required" } },
      { error: "Session required", code: "operator_session_required" },
    ]) {
      let delayedProof: ((reply: Response) => void) | undefined;
      response = () => new Promise<Response>((resolve) => { delayedProof = resolve; });
      await focus();
      assert(delayedProof, "a successful metadata proof can still be in flight at refusal");
      const beforeRefusal = privateSessionGeneration();
      projectsResponse = () => json(refusal, 401);
      await React.act(async () => { await api.projects().catch(() => undefined); });
      delayedProof(json(signedIn));
      await flush();
      assert(!host.querySelector("[data-private-workspace]"), "a delayed pre-refusal metadata success cannot reveal private content again");
      assert(!privateReadsAllowed() && privateSessionGeneration() > beforeRefusal, "confirmed read refusal withdraws admission and invalidates old observations");
      assert(projects.get().data === null, "confirmed read refusal clears the prior private query cache");
      projectsResponse = () => json({ projects: [{ key: "private-project" }] });
      response = () => json(signedIn);
      await React.act(async () => setSessionKey("existing-local-key"));
      await focus();
      assert(host.querySelector("output")?.textContent === "", "new verification after refusal cannot recover the expired draft");
      await React.act(async () => (host.querySelector("[data-private-workspace] button") as HTMLButtonElement).click());
    }

    // Hold a real API read across confirmed identity loss.
    const stale = api.issues().then(() => false, () => true);
    assert(lateRead, "old private request is actually in flight");
    response = () => json({ error: { message: "Session required", code: "board_session_required" } }, 401);
    await focus();
    assert(host.querySelector("[data-auth-state='expired'] h1"), "confirmed loss of an admitted identity is expired");
    assert(!host.querySelector("[data-private-workspace]"), "expiry unmounts, rather than hides, private state");
    assert(privateSessionGeneration() > beforeOutage && !privateReadsAllowed(), "expiry invalidates private request generations");
    assert(projects.get().data === null, "expiry clears previously loaded private cache data");
    lateRead!(json({ issues: [{ id: "old-private-issue" }] }));
    assert(await stale, "late old-generation API response is rejected");
    response = () => json(signedIn);
    await focus();
    assert(host.querySelector("[data-private-workspace]"), "verified sign-in can enter after expiry");
    assert(host.querySelector("output")?.textContent === "", "expired drafts do not reappear after signing in");

    // An ordinary local reload still uses the durable tab key.
    await React.act(async () => setSessionKey("reload-local-key"));
    await flush();
    await React.act(async () => root.unmount());
    root = createRoot(host);
    await render();
    assert(host.querySelector("[data-private-workspace]") && proofHeaders.at(-1) === "reload-local-key", "local stored key survives remount and is verified normally");

    response = () => json({ error: "Access refused" }, 403);
    await focus();
    assert(host.querySelector("[data-auth-state='refused'] h1"), "403 is an explicit access refusal");
    assert(!host.querySelector("[data-private-workspace]") && !privateReadsAllowed(), "refusal removes the private tree and read access");

    const { navigate } = require("../src/lib/useLocation") as typeof LocationModule;
    const { captureLoginNonce } = require("../src/features/auth/session") as typeof AuthSessionModule;
    const intendedPath = "/projects/cadence/issues?view=kanban";
    await React.act(async () => navigate(intendedPath));
    await flush();
    const hostedLink = host.querySelector('a[href^="/__platform/login"]') as HTMLAnchorElement | null;
    assert(hostedLink, "hosted refusal offers the platform sign-in handoff");
    assert(new URL(hostedLink!.href).searchParams.get("to") === `http://localhost${intendedPath}`, "hosted handoff preserves the absolute original board path and query");
    const rememberedPath = sessionStorage.getItem("cadence.auth.returnTo");
    assert(rememberedPath === intendedPath, "public deep link is remembered for the same-tab local handoff");

    // Exercise the actual Login nonce exchange. A returned key is not admission:
    // the /login route cannot redirect or mount private content before metadata.
    for (const [storedReturn, expectedPath] of [
      [rememberedPath, intendedPath],
      ["https://outside.example/private", "/"],
      ["//outside.example/private", "/"],
      ["/login#n=stale-link", "/"],
      ["/__platform/login", "/"],
    ]) {
      await React.act(async () => root.unmount());
      sessionStorage.clear();
      sessionStorage.setItem("cadence.auth.returnTo", storedReturn!);
      const nonce = "a".repeat(64);
      history.replaceState(null, "", `/login#n=${nonce}`);
      captureLoginNonce();
      const beforeHistory = history.length;
      const heldMeta: ((reply: Response) => void)[] = [];
      let exchanges = 0;
      globalThis.fetch = async (input, init) => {
        const url = new URL(String(input), "http://localhost");
        if (url.pathname === "/api/meta") {
          assert(url.searchParams.get("operator") === "1", "post-exchange admission requests verified metadata");
          return new Promise<Response>((resolve) => { heldMeta.push(resolve); });
        }
        if (url.pathname === "/api/session") {
          exchanges++;
          assert(init?.method === "POST" && JSON.parse(String(init.body)).nonce === nonce, "real Login exchanges its captured nonce once");
          return json({ session_key: "new-local-session-key" });
        }
        if (url.pathname === "/api/projects") return json({ projects: [] });
        throw new Error(`Unexpected login handoff request ${url.pathname}`);
      };
      root = createRoot(host);
      await render();
      assert(exchanges === 1 && sessionKey() === "new-local-session-key", "nonce exchange creates this tab's session key");
      assert(location.pathname === "/login" && !location.hash, "nonce is stripped and handoff stays on login while verification is pending");
      assert(!host.querySelector("[data-private-workspace]"), "exchanged key alone cannot mount private content");
      assert(sessionStorage.getItem("cadence.auth.returnTo") === storedReturn, "login handoff does not overwrite the pending destination");
      assert(heldMeta.length > 0, "post-exchange verification is genuinely pending");
      await React.act(async () => heldMeta[heldMeta.length - 1](json(signedIn)));
      await flush();
      assert(location.pathname + location.search === expectedPath, `verified login returns only to a safe board route (${String(storedReturn)})`);
      assert(history.length === beforeHistory, "verified login replaces the handoff entry instead of pushing it");
      assert(host.querySelector("[data-private-workspace]"), "private content mounts only after verified navigation");
    }
  } finally {
    await React.act(async () => root.unmount());
    host.remove();
    await win.happyDOM.close();
  }
}
void boundaryChecks().catch((error) => { console.error(error); require("process").exitCode = 1; });
