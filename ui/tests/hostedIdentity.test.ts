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
  act(() => root.render(createElement(LocaleProvider, null, createElement(AccountMenu, { meta, onChange: () => undefined, trigger: "avatar", placement: "below-end" }))));
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
  const { openAttachmentInTab } = require("../src/features/home/AttachmentMedia") as typeof import("../src/features/home/AttachmentMedia");
  const { privateSessionGeneration, privateReadsAllowed } = require("../src/lib/privateSession") as typeof PrivateSessionModule;
  const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
  const signedIn = { ...base, hosted: false, operator: false, session: { id: "local-member", origin: "loopback", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test", user: { sub: "member", name: "Member", email: "member@example.test", handle: "member", role: "member" } } };
  let response: () => Response | Promise<Response> = () => json(signedIn);
  let projectsResponse = () => json({ projects: [{ key: "private-project" }] });
  let healthResponse = () => json({ ok: true, pm_present: true, projects: 2, issues: 9, daemon: "reachable", embedded: true });
  let healthExpectedSession: string | null = null;
  let healthValidator: string | null = null;
  const proofHeaders: (string | null)[] = [];
  let lateRead: ((response: Response) => void) | undefined;
  let pdfExpectedSession: string | null = null;
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
    if (url.pathname.startsWith("/api/chat/attachments/")) {
      pdfExpectedSession = new Headers(init?.headers).get("X-Cadence-Expected-Session");
      return new Response("synthetic PDF from an older host");
    }
    if (url.pathname === "/api/health") {
      healthExpectedSession = new Headers(init?.headers).get("X-Cadence-Expected-Session");
      healthValidator = new Headers(init?.headers).get("If-None-Match");
      return healthResponse();
    }
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
    response = () => json({ ...signedIn, hosted: true });
    await retry();
    assert(host.querySelector("[data-auth-state='unavailable'] h1") && !privateReadsAllowed(), "a hosted claim with a local session cannot bootstrap");
    assert(host.textContent?.includes("Hosted workspace"), "a contradictory hosted claim keeps the hosted recovery path");
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

    // A hosted renewal changes its deadline, not the owner of an unsent draft.
    const hostedSession = { ...signedIn, hosted: true, session: { ...signedIn.session, id: "hosted-session", origin: "public" } };
    response = () => json(hostedSession);
    await focus();
    const oldOpen = win.open;
    let pdfClosed = false;
    win.open = (() => ({ close: () => { pdfClosed = true; } })) as typeof win.open;
    try {
      await React.act(async () => {
        openAttachmentInTab("synthetic-pdf", "synthetic-conversation");
        for (let i = 0; i < 20; i++) await Promise.resolve();
      });
      assert(pdfExpectedSession === "hosted-session", "verified metadata pins the hosted PDF request");
      assert(pdfClosed, "an older host without the session proof cannot open the PDF");
    } finally {
      win.open = oldOpen;
    }
    await React.act(async () => (host.querySelector("[data-private-workspace] button") as HTMLButtonElement).click());
    healthResponse = () => new Response(JSON.stringify({ ok: true, pm_present: true, projects: 2, issues: 9, daemon: "reachable", embedded: true }), {
      headers: { "Content-Type": "application/json", "ETag": '"health-v1"', "X-Cadence-Verified-Session": "hosted-session" },
    });
    let healthIssues = 0;
    await React.act(async () => { healthIssues = (await api.health()).issues; });
    assert(healthExpectedSession === "hosted-session" && healthIssues === 9, "hosted health is delivered only for the requesting tab's verified identity");
    for (const { proof, status } of [
      { proof: null, status: 304 },
      { proof: null, status: 200 },
      { proof: "different-session", status: 200 },
    ]) {
      let decoded = false;
      healthResponse = () => {
        const reply = new Response(status === 304 ? null : JSON.stringify({ issues: 777 }), {
          status, headers: proof === null ? {} : { "X-Cadence-Verified-Session": proof },
        });
        const decode = reply.json.bind(reply);
        reply.json = () => { decoded = true; return decode(); };
        return reply;
      };
      let rejected = false;
      await React.act(async () => { await api.health().catch(() => { rejected = true; }); });
      if (status === 304) assert(healthValidator === '"health-v1"', "the unproved 304 attempts to reuse a cached health result");
      assert(rejected && !decoded && !privateReadsAllowed(), "unproved hosted health is rejected before its body is decoded");
      assert(host.querySelector(`[data-auth-state='${proof === null ? "updating" : "unavailable"}'] h1`), "unproved hosted health pauses the workspace with recovery guidance");
      const checks = proofHeaders.length;
      await focus();
      assert(proofHeaders.length === checks, "an unproved health read does not repeatedly reopen the workspace on focus");
      response = () => json(hostedSession);
      await retry();
      assert(host.querySelector("output")?.textContent === "unsaved private draft", "same-session health recovery retains the in-memory draft");
    }
    for (const { status, code, state } of [
      { status: 409, code: "session_changed", state: "unavailable" },
      { status: 401, code: "board_session_required", state: "unavailable" },
      { status: 503, code: "sign_in_upgrade_required", state: "updating" },
    ]) {
      healthResponse = () => json({ error: { code, message: "Verification required." } }, status);
      await React.act(async () => { await api.health().catch(() => undefined); });
      assert(host.querySelector(`[data-auth-state='${state}'] h1`) && !privateReadsAllowed(), "unproved health refusal pauses private work until reverified");
      response = () => json(hostedSession);
      await retry();
      assert(host.querySelector("output")?.textContent === "unsaved private draft", "session uncertainty does not discard a draft");
    }
    const beforeUpgrade = privateSessionGeneration();
    response = () => json({ error: { code: "sign_in_upgrade_required", message: "Updating" } }, 503);
    await focus();
    assert(host.querySelector("[data-auth-state='updating'] h1")?.textContent === "Sign-in verification is updating.", "upgrade explains why verification is unavailable");
    assert(privateSessionGeneration() === beforeUpgrade, "an upgrade retains the current in-memory session");
    response = () => json(hostedSession);
    await retry();
    assert(host.querySelector("output")?.textContent === "unsaved private draft", "same-session recovery after an upgrade retains the in-memory draft");
    const beforeRenewal = privateSessionGeneration();
    response = () => json({ ...hostedSession, session: { ...hostedSession.session, expires_at: 4102473600, idle_expires_at: 4102473600 } });
    await focus();
    assert(privateSessionGeneration() === beforeRenewal, "hosted renewal keeps the same private session");
    assert(host.querySelector("output")?.textContent === "unsaved private draft", "hosted renewal keeps the unsent draft");

    // A once-hosted tab must never reinterpret a contradictory or incomplete
    // metadata response as a local/anonymous admission.
    for (const downgraded of [
      { ...signedIn, signed_in: false, hosted: false, anonymous_reads: true, session: null },
      { ...signedIn, signed_in: false, hosted: undefined, anonymous_reads: true, session: null },
      { ...hostedSession, signed_in: false },
      { ...hostedSession, signed_in: false, session: { origin: "public" } },
      { ...signedIn, hosted: false },
      { ...signedIn, hosted: true },
      { ...hostedSession, hosted: "invalid" },
      { ...hostedSession, hosted: false },
    ]) {
      response = () => json(downgraded);
      await focus();
      assert(host.querySelector("[data-auth-state='unavailable'] h1"), "hosted metadata downgrade is verification unavailable");
      assert(!privateReadsAllowed(), "a hosted metadata downgrade cannot admit private reads");
      response = () => json(hostedSession);
      await retry();
      assert(host.querySelector("output")?.textContent === "unsaved private draft", "matching hosted recovery retains the same in-memory draft");
    }

    response = () => json({ ...hostedSession, hosted: undefined });
    await focus();
    assert(host.querySelector("[data-private-workspace]") && privateReadsAllowed(), "a valid public session remains compatible when an older server omits hosted");
    response = () => json(hostedSession);
    await focus();
    const beforeDowngradeReload = privateSessionGeneration();
    response = () => json({ ...signedIn, hosted: false });
    await React.act(async () => root.unmount());
    root = createRoot(host);
    await render();
    assert(host.querySelector("[data-auth-state='unavailable'] h1") && !privateReadsAllowed(), "reload cannot downgrade a remembered hosted identity to local");
    assert(privateSessionGeneration() === beforeDowngradeReload, "an unanswered downgrade is not a confirmed identity loss");
    response = () => json(hostedSession);
    await retry();
    assert(host.querySelector("[data-private-workspace]"), "the remembered hosted identity can recover after reload");


    // A different hosted identity must not silently replace the admitted one.
    const switched = { ...hostedSession, session: { ...hostedSession.session, id: "new-hosted-session", user: { ...hostedSession.session.user, sub: "second-user", email: "second@example.test" } } };
    response = () => json(switched);
    await focus();
    assert(host.querySelector("[data-auth-state='changed'] h1"), "a changed hosted identity requires confirmation");
    assert(!host.querySelector("[data-private-workspace]") && !privateReadsAllowed(), "the previous private tree is discarded before confirmation");
    assert(host.textContent?.includes("Unsent changes from the previous session may not have been saved."), "the confirmation warns that unsent changes may be lost");
    assert(host.textContent?.includes("second@example.test"), "the prompt names only the freshly verified account");
    response = () => json({ ...signedIn, hosted: false });
    await focus();
    assert(host.querySelector("[data-auth-state='unavailable'] h1") && !privateReadsAllowed(), "a pending account change cannot be bypassed by local metadata");
    await React.act(async () => root.unmount());
    root = createRoot(host);
    await render();
    assert(host.querySelector("[data-auth-state='unavailable'] h1") && !privateReadsAllowed(), "reload retains the pending hosted confirmation during a downgrade");
    response = () => json(switched);
    await retry();
    assert(host.querySelector("[data-auth-state='changed'] h1"), "a valid hosted answer still needs the pending confirmation");
    await focus();
    assert(host.querySelector("[data-auth-state='changed'] h1"), "automatic verification cannot bypass confirmation");
    response = () => json({ ...switched, session: { ...switched.session, user: { ...switched.session.user, email: { malformed: true } } } });
    await focus();
    assert(host.querySelector("[data-auth-state='unavailable'] h1") && !privateReadsAllowed(), "a malformed account display is unavailable, not authority");
    response = () => json(switched);
    await retry();
    assert(host.querySelector("[data-auth-state='changed'] h1"), "corrected identity still needs confirmation");
    response = () => json({ ...switched, session: { ...switched.session, user: { ...switched.session.user, email: "", name: "" } } });
    await focus();
    assert(host.textContent?.includes("Account details unavailable") && !privateReadsAllowed(), "missing display details are not invented");
    response = () => json(switched);
    await focus();

    await React.act(async () => root.unmount());
    root = createRoot(host);
    await render();
    assert(host.querySelector("[data-auth-state='changed'] h1"), "reload does not bypass pending confirmation");
    const continueWithCurrent = async () => {
      const button = Array.from(host.querySelectorAll("button")).find((entry) => entry.textContent?.trim() === "Continue with this sign-in");
      assert(button, "the changed-identity screen asks for confirmation");
      await React.act(async () => button.click());
      await flush();
    };
    const changedAgain = { ...switched, session: { ...switched.session, id: "third-hosted-session", user: { ...switched.session.user, sub: "third-user", email: "third@example.test" } } };
    response = () => json(changedAgain);
    await continueWithCurrent();
    assert(host.querySelector("[data-auth-state='changed'] h1"), "a different identity arriving during confirmation needs a new confirmation");
    assert(host.textContent?.includes("third@example.test") && !privateReadsAllowed(), "the new verified account is shown without opening private work");
    await continueWithCurrent();
    assert(host.querySelector("[data-private-workspace]") && host.querySelector("output")?.textContent === "", "confirmation opens only the newly verified identity with an empty draft");
    assert(sessionStorage.getItem("cadence.hostedIdentityConfirmationPending") === null, "successful confirmation removes the reload marker");

    // A late refusal must not erase the need to confirm the next hosted sign-in.
    response = () => json({ error: { code: "board_session_required", message: "Session required" } }, 401);
    await focus();
    assert(host.querySelector("[data-auth-state='expired'] h1"), "a confirmed hosted refusal ends the admitted session");
    await React.act(async () => root.unmount());
    root = createRoot(host);
    const afterRefusal = { ...changedAgain, session: { ...changedAgain.session, id: "fourth-hosted-session", user: { ...changedAgain.session.user, sub: "fourth-user", email: "fourth@example.test" } } };
    response = () => json(afterRefusal);
    await render();
    assert(host.querySelector("[data-auth-state='changed'] h1") && !privateReadsAllowed(), "a new hosted identity after refusal and reload still requires confirmation");
    assert(host.textContent?.includes("fourth@example.test"), "the confirmation reflects the new verified identity");
    await continueWithCurrent();
    assert(host.querySelector("[data-private-workspace]"), "the newly verified identity opens only after confirmation");
    const afterKeyChange = { ...afterRefusal, session: { ...afterRefusal.session, id: "fifth-hosted-session", user: { ...afterRefusal.session.user, sub: "fifth-user", email: "fifth@example.test" } } };
    response = () => json(afterKeyChange);
    await React.act(async () => setSessionKey("hosted-rotated-key"));
    await flush();
    assert(host.querySelector("[data-auth-state='changed'] h1") && !privateReadsAllowed(), "a credential change cannot bypass hosted identity confirmation");
    await continueWithCurrent();
    const afterRequired = { ...afterKeyChange, session: { ...afterKeyChange.session, id: "sixth-hosted-session", user: { ...afterKeyChange.session.user, sub: "sixth-user", email: "sixth@example.test" } } };
    response = () => json(afterRequired);
    await React.act(async () => window.dispatchEvent(new Event("cadence:session-required")));
    await focus();
    assert(host.querySelector("[data-auth-state='changed'] h1") && !privateReadsAllowed(), "a session-required event preserves the confirmation requirement");
    await continueWithCurrent();

    // A hosted identity remembered on a local-classified origin must retain
    // hosted recovery copy even before the first metadata request answers.
    let answerEarlyProof: ((value: Response) => void) | undefined;
    response = () => new Promise<Response>((resolve) => { answerEarlyProof = resolve; });
    await React.act(async () => root.unmount());
    root = createRoot(host);
    await render();
    const hostedDuringCheck = host.textContent?.includes("Hosted workspace");
    await React.act(async () => window.dispatchEvent(new Event("cadence:session-required")));
    const hostedAfterEarlyEnd = host.textContent?.includes("Continue with AgenticOS") && !host.textContent.includes("cadence ui login");
    assert(answerEarlyProof, "the first hosted metadata check is in flight");
    answerEarlyProof(json(afterRequired));
    await flush();
    assert(hostedDuringCheck, "a remembered hosted identity keeps hosted checking copy before metadata answers");
    assert(hostedAfterEarlyEnd, "an early session-ending event keeps the hosted sign-in action");
    let answerPendingProof: ((value: Response) => void) | undefined;
    response = () => new Promise<Response>((resolve) => { answerPendingProof = resolve; });
    await React.act(async () => root.unmount());
    root = createRoot(host);
    await render();
    const hostedWithPendingChange = host.textContent?.includes("Hosted workspace");
    assert(answerPendingProof, "the pending-confirmation metadata check is in flight");
    answerPendingProof(json(afterRequired));
    await flush();
    assert(hostedWithPendingChange, "a pending hosted confirmation keeps hosted checking copy after reload");
    assert(host.querySelector("[data-auth-state='changed'] h1"), "the early session end still requires identity confirmation");
    response = () => json(afterRequired);
    await continueWithCurrent();

    // localhost is classified as local before the first response. A remembered
    // hosted identity must still survive a refusal on reload.
    response = () => json({ error: { code: "board_session_required", message: "Session required" } }, 401);
    await React.act(async () => root.unmount());
    root = createRoot(host);
    await render();
    assert(host.querySelector("[data-auth-state='expired'] h1"), "a real refusal after reload ends the hosted session");
    assert(sessionStorage.getItem("cadence.hostedIdentityConfirmationPending") === "1", "the remembered hosted identity requires confirmation even on a local-classified host");
    response = () => json({ ...signedIn, hosted: false });
    await focus();
    assert(host.querySelector("[data-auth-state='unavailable'] h1") && !privateReadsAllowed(), "refusal after reload cannot downgrade hosted authority to local");
    response = () => json(afterRequired);
    await retry();
    assert(host.querySelector("[data-auth-state='changed'] h1") && !privateReadsAllowed(), "a sign-in after that refusal still needs confirmation");
    await continueWithCurrent();

    // A different public identity on the very first response after reload
    // cannot be admitted just because the hostname hint is local.
    const afterReloadSwitch = { ...afterRequired, session: { ...afterRequired.session, id: "seventh-hosted-session", user: { ...afterRequired.session.user, sub: "seventh-user", email: "seventh@example.test" } } };
    response = () => json(afterReloadSwitch);
    await React.act(async () => root.unmount());
    root = createRoot(host);
    await render();
    assert(host.querySelector("[data-auth-state='changed'] h1") && !privateReadsAllowed(), "a different hosted identity on reload requires confirmation");
    await continueWithCurrent();
    response = () => json({ ...afterReloadSwitch, signed_in: false, session: null, anonymous_reads: false });
    await focus();
    assert(host.querySelector("[data-auth-state='expired'] h1") && !privateReadsAllowed(), "a consistent hosted signed-out response still expires the session");
    // Start an independent fresh local-board scenario; a hosted tab cannot
    // silently change into this identity on the same origin.
    await React.act(async () => root.unmount());
    sessionStorage.removeItem("cadence.privateSessionIdentity");
    sessionStorage.removeItem("cadence.hostedIdentityConfirmationPending");
    root = createRoot(host);
    response = () => json(signedIn);
    await render();
    assert(host.querySelector("output")?.textContent === "", "a fresh local board starts with no previous draft");
    await React.act(async () => (host.querySelector("[data-private-workspace] button") as HTMLButtonElement).click());

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
    const pathWithoutFragment = "/wiki/releases?view=read";
    const dottedPath = `${pathWithoutFragment}#release-v1.2`;
    const intendedPath = `${pathWithoutFragment}#research%20%26%20development`;
    await React.act(async () => navigate(dottedPath));
    await flush();
    const hostedLink = host.querySelector('a[href^="/__platform/login"]') as HTMLAnchorElement | null;
    assert(hostedLink, "hosted refusal offers the platform sign-in handoff");
    assert(new URL(hostedLink!.href).searchParams.get("to") === `http://localhost${pathWithoutFragment}`, "hosted handoff never sends fragments to the platform");
    hostedLink.addEventListener("click", (event) => event.preventDefault());
    await React.act(async () => hostedLink.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })));
    assert(sessionStorage.getItem("cadence.auth.hostedReturnTo") === dottedPath, "punctuation in a safe anchor stays in this tab for verified restoration");
    await React.act(async () => navigate(intendedPath));
    await flush();
    await React.act(async () => hostedLink.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })));
    assert(sessionStorage.getItem("cadence.auth.hostedReturnTo") === intendedPath, "standalone ampersands in wiki anchors survive");
    for (const fragment of ["#access_token=secret", "#access_token%3Dsecret", "#bad%"]) {
      await React.act(async () => navigate(`${pathWithoutFragment}${fragment}`));
      await flush();
      await React.act(async () => hostedLink.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })));
      assert(sessionStorage.getItem("cadence.auth.hostedReturnTo") === pathWithoutFragment, "credential-style or malformed fragments are not remembered");
    }
    await React.act(async () => navigate(intendedPath));
    await flush();
    const rememberedPath = sessionStorage.getItem("cadence.auth.returnTo");
    assert(rememberedPath === intendedPath, "public deep link is remembered for the same-tab local handoff");
    await React.act(async () => hostedLink.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })));
    history.replaceState(null, "", pathWithoutFragment);
    response = () => json(hostedSession);
    await focus();
    assert(location.pathname + location.search + location.hash === intendedPath, "only verified same-tab return restores the safe anchor");
    assert(sessionStorage.getItem("cadence.auth.hostedReturnTo") === null, "the hosted return marker is single-use");

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
      assert(location.pathname + location.search + location.hash === expectedPath, `verified login returns only to a safe board route (${String(storedReturn)})`);
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
