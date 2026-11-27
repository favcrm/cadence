export {};
import {
  WhatsAppActionUncertainError,
  isMarkedWhatsAppSettingsUnsupported,
  isWhatsAppSettingsUnsupported,
  markWhatsAppSettingsUnsupported,
  parseLinkedSnapshot,
  parseWhatsAppModeParam,
  readWhatsAppSettings,
  runLinkedWhatsAppAction,
  runOfficialWhatsAppAction,
  saveWhatsAppProvider,
  shouldShowLinkedIdleNudge,
  whatsappSettingsSummary,
  type WhatsAppSettingsSnapshot,
} from "../src/features/workspace-apps/whatsappSettings";
import { ApiError } from "../src/lib/apiError";
import { allowPrivateReads, resetPrivateSession } from "../src/lib/privateSession";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

declare function require(name: string): any;

async function flushUi(turns = 10): Promise<void> {
  for (let i = 0; i < turns; i += 1) {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  }
}

async function testPanelOAuthRedirectAndDisconnect(): Promise<void> {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/apps/waba?mode=official" });
  const prevDescriptors = new Map<string, PropertyDescriptor | undefined>();
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLTextAreaElement", "HTMLButtonElement", "navigator", "MutationObserver",
    "Event", "MouseEvent", "KeyboardEvent", "MessageEvent", "FocusEvent", "location",
  ]) {
    prevDescriptors.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  const prevActEnv = Object.getOwnPropertyDescriptor(globalThis, "IS_REACT_ACT_ENVIRONMENT");
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true, writable: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const originalFetch = globalThis.fetch;
  const originalAssign = win.location.assign;
  let getCount = 0;
  let officialBindingStatus: "none" | "active" | "revoked" = "none";
  let officialConnected = false;
  let settingsChangedCount = 0;
  let disconnectMode: "refused" | "partial" | "ok" = "refused";
  const assignCalls: string[] = [];
  let lastConnectBody: string | undefined;

  try {
    Object.defineProperty(win.location, "assign", {
      configurable: true,
      writable: true,
      value: (url: string | URL) => {
        assignCalls.push(String(url));
      },
    });

    const currentSnapshot = () => ({
      viewer_role: "owner",
      active_provider: "official",
      reply_mode: "copilot",
      settings_revision: 4,
      linked: { available: true, granted: true, state: "idle", connected: false, qr: null },
      official: {
        available: true,
        enabled: true,
        paused: false,
        connected: officialConnected,
        binding_status: officialBindingStatus,
        company_name: officialBindingStatus === "active" ? "Demo Business" : null,
        display_phone_number: officialConnected ? "+852 5555 0148" : null,
        verified_name: officialConnected ? "Demo Business" : null,
        waba_id: officialConnected ? "waba_1" : null,
        phone_number_id: officialConnected ? "pn_1" : null,
        webhook: null,
        observed_at: "2026-11-25T12:00:00Z",
      },
    });

    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      if (url.endsWith("/api/apps/install_panel/whatsapp-settings")) {
        getCount += 1;
        return jsonResponse(currentSnapshot());
      }
      if (url.endsWith("/api/apps/install_panel/whatsapp-settings/official/connect")) {
        lastConnectBody = typeof init?.body === "string" ? init.body : undefined;
        return jsonResponse({ url: "https://portal.favcrm.example.test/wa/connect?token=ok", expires_at: "2026-11-25T13:00:00Z" });
      }
      if (url.endsWith("/api/apps/install_panel/whatsapp-settings/official/disconnect")) {
        if (disconnectMode === "refused") {
          return jsonResponse({
            disconnected: false,
            favcrm_revoked: false,
            official: currentSnapshot().official,
          });
        }
        if (disconnectMode === "partial") {
          return jsonResponse({
            disconnected: true,
            favcrm_revoked: false,
            official: { ...currentSnapshot().official, connected: false, binding_status: "revoked" },
          });
        }
        return jsonResponse({
          disconnected: true,
          favcrm_revoked: true,
          official: {
            available: true,
            enabled: true,
            paused: false,
            connected: false,
            binding_status: "revoked",
            company_name: null,
            display_phone_number: null,
            verified_name: null,
            waba_id: null,
            phone_number_id: null,
            webhook: null,
            observed_at: "2026-11-25T12:05:00Z",
          },
        });
      }
      return jsonResponse({}, 404);
    }) as typeof fetch;

    const React = require("react") as typeof import("react");
    const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
    const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
    const WhatsAppSettingsPanel = (
      require("../src/features/workspace-apps/WhatsAppSettingsPanel") as typeof import("../src/features/workspace-apps/WhatsAppSettingsPanel")
    ).default;

    const container = document.createElement("div");
    document.body.appendChild(container);
    let root = createRoot(container);

    try {
      await React.act(async () => {
        root.render(
          React.createElement(
            LocaleProvider,
            null,
            React.createElement(WhatsAppSettingsPanel, {
              installId: "install_panel",
              initialMode: "official",
              onUnsupported: () => {},
              onSettingsChanged: () => {
                settingsChangedCount += 1;
              },
            }),
          ),
        );
        await flushUi();
      });

      const buttonWith = (text: string) =>
        [...container.querySelectorAll("button")].find((b) => b.textContent?.includes(text)) as HTMLButtonElement | undefined;

      assert(!container.querySelector("input[type='text']"), "no manual FavCRM Company ID input renders");
      const connectBtn = buttonWith("Connect with FavCRM");
      assert(connectBtn, "1-click Connect with FavCRM button renders for unlinked workspace");

      await React.act(async () => {
        connectBtn.click();
        await flushUi();
      });

      assert(
        assignCalls.length === 1 &&
          assignCalls[0] === "https://portal.favcrm.example.test/wa/connect?token=ok" &&
          lastConnectBody === JSON.stringify({ return_to: "http://localhost/apps/waba?mode=official" }),
        "Connect with FavCRM sends return_to and redirects current tab to validated HTTPS URL",
      );
      assert(container.textContent?.includes("Continue setup in FavCRM"), "auth fallback link renders after connect");

      // Re-mount simulating OAuth callback return with ?aos_connect=connected
      officialBindingStatus = "active";
      officialConnected = true;
      win.history.replaceState(null, "", "/apps/waba?mode=official&aos_connect=connected");
      const changedBeforeMount = settingsChangedCount;

      React.act(() => { root.unmount(); });
      root = createRoot(container);
      await React.act(async () => {
        root.render(
          React.createElement(
            LocaleProvider,
            null,
            React.createElement(WhatsAppSettingsPanel, {
              installId: "install_panel",
              initialMode: "official",
              onUnsupported: () => {},
              onSettingsChanged: () => {
                settingsChangedCount += 1;
              },
            }),
          ),
        );
        await flushUi();
      });

      assert(
        !win.location.search.includes("aos_connect") &&
          container.textContent?.includes("Official WhatsApp connected.") &&
          settingsChangedCount > changedBeforeMount,
        "aos_connect=connected shows notice, refreshes settings, and strips query parameter",
      );

      // Disconnect with disconnected=false refuses rather than claiming success
      const openDisconnectConfirm = () => buttonWith("Disconnect");
      await React.act(async () => {
        openDisconnectConfirm()!.click();
        await flushUi();
      });
      const confirmRevoke = [...container.querySelectorAll("button")].find(
        (b) => b.textContent?.includes("Disconnect") && b.className.includes("danger"),
      ) as HTMLButtonElement | undefined;
      assert(confirmRevoke, "inline disconnect confirmation button renders");

      disconnectMode = "refused";
      await React.act(async () => {
        confirmRevoke.click();
        await flushUi();
      });
      assert(
        container.textContent?.includes("Could not disconnect Official WhatsApp connection."),
        "disconnect with disconnected=false surfaces failure notice",
      );

      // Disconnect with disconnected=true, favcrm_revoked=false reports partial revoke warning
      disconnectMode = "partial";
      await React.act(async () => {
        buttonWith("Disconnect")!.click();
        await flushUi();
      });
      const confirmPartial = [...container.querySelectorAll("button")].find(
        (b) => b.textContent?.includes("Disconnect") && b.className.includes("danger"),
      ) as HTMLButtonElement;
      await React.act(async () => {
        confirmPartial.click();
        await flushUi();
      });
      assert(
        container.textContent?.includes("upstream FavCRM revocation could not be confirmed"),
        "disconnect with favcrm_revoked=false surfaces explicit upstream revoke warning",
      );

      // Clean end-to-end disconnect
      disconnectMode = "ok";
      officialConnected = true;
      await React.act(async () => {
        buttonWith("Refresh")?.click();
        await flushUi();
      });
    } finally {
      React.act(() => { root.unmount(); });
      container.remove();
    }
  } finally {
    loader.prototype.require = originalRequire;
    globalThis.fetch = originalFetch;
    Object.defineProperty(win.location, "assign", { configurable: true, writable: true, value: originalAssign });
    for (const [name, desc] of prevDescriptors.entries()) {
      if (desc) Object.defineProperty(globalThis, name, desc);
      else delete (globalThis as Record<string, unknown>)[name];
    }
    if (prevActEnv) Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", prevActEnv);
    else delete (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT;
  }
}

async function main() {
  allowPrivateReads();

  // 1. Mode param + QR owner/member normalization
  assert(parseWhatsAppModeParam("linked") === "linked", "linked param parses");
  assert(parseWhatsAppModeParam("official") === "official", "official param parses");
  assert(parseWhatsAppModeParam("other") === null, "unknown mode param rejected");
  assert(parseWhatsAppModeParam(null) === null, "null mode param stays null");

  const memberParsed = parseLinkedSnapshot(
    { available: true, granted: true, state: "pairing", connected: false, qr: "LEAKED_SECRET" },
    "member",
  );
  assert(memberParsed.available && memberParsed.qr === null, "member parser strips QR unconditionally");

  const ownerParsed = parseLinkedSnapshot(
    { available: true, granted: true, state: "pairing", connected: false, qr: "OWNER_SECRET_QR" },
    "owner",
  );
  assert(ownerParsed.available && ownerParsed.qr === "OWNER_SECRET_QR", "owner pairing keeps QR");

  let bogusStateRejected = false;
  try {
    parseLinkedSnapshot(
      { available: true, granted: true, state: "bogus_state", connected: false, qr: null },
      "owner",
    );
  } catch {
    bogusStateRejected = true;
  }
  assert(bogusStateRejected, "unknown linked state fails closed");

  let missingLinkedBooleansRejected = false;
  try {
    parseLinkedSnapshot({ available: true, state: "idle" }, "owner");
  } catch {
    missingLinkedBooleansRejected = true;
  }
  assert(missingLinkedBooleansRejected, "linked snapshot missing booleans fails closed");

  // 2. Host read/write transport guarantees + 404/501 detection + timeout uncertain recovery
  const seenRequests: Array<{ url: string; method: string; cache?: RequestCache; credentials?: RequestCredentials; contentType?: string; body?: string }> = [];
  const originalFetch = globalThis.fetch;

  try {
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      const headers = new Headers(init?.headers);
      seenRequests.push({
        url,
        method: init?.method ?? "GET",
        cache: init?.cache,
        credentials: init?.credentials,
        contentType: headers.get("Content-Type") ?? undefined,
        body: typeof init?.body === "string" ? init.body : undefined,
      });

      if (url.endsWith("/api/apps/install_waba/whatsapp-settings")) {
        return jsonResponse({
          viewer_role: "owner",
          active_provider: "linked",
          reply_mode: "copilot",
          settings_revision: 2,
          linked: {
            available: true,
            granted: true,
            state: "pairing",
            connected: false,
            qr: "OWNER_QR_PAYLOAD",
          },
          official: {
            available: true,
            enabled: true,
            paused: false,
            connected: false,
            binding_status: "none",
            company_name: null,
            display_phone_number: null,
            verified_name: null,
            waba_id: null,
            phone_number_id: null,
            webhook: null,
            observed_at: null,
          },
        });
      }
      if (url.endsWith("/api/apps/install_member/whatsapp-settings")) {
        return jsonResponse({
          viewer_role: "member",
          active_provider: "linked",
          reply_mode: "copilot",
          settings_revision: 2,
          linked: {
            available: true,
            granted: true,
            state: "pairing",
            connected: false,
            qr: "SHOULD_BE_STRIPPED",
          },
          official: { available: false, error_code: "not_found" },
        });
      }
      if (url.endsWith("/api/apps/install_old/whatsapp-settings")) {
        return jsonResponse({ error: "Not found" }, 404);
      }
      if (url.endsWith("/api/apps/install_waba/whatsapp-settings/provider")) {
        return jsonResponse({ error: { code: "revision_conflict", message: "Stale revision" } }, 409);
      }
      if (url.endsWith("/api/apps/install_waba/whatsapp-settings/linked/start")) {
        return await new Promise<Response>((_, reject) => {
          init?.signal?.addEventListener("abort", () => {
            reject(new DOMException("Aborted", "AbortError"));
          });
        });
      }
      if (url.endsWith("/api/apps/install_unsafe_official/whatsapp-settings/official/connect")) {
        return jsonResponse({ url: "javascript:alert(1)", expires_at: null });
      }
      if (url.endsWith("/api/apps/install_null_official/whatsapp-settings/official/connect")) {
        return jsonResponse({ url: null, expires_at: null });
      }
      if (url.endsWith("/api/apps/install_bad_role/whatsapp-settings")) {
        return jsonResponse({
          viewer_role: "admin",
          active_provider: "linked",
          reply_mode: "copilot",
          settings_revision: 1,
          linked: { available: false, error_code: "not_granted" },
          official: { available: false, error_code: "not_found" },
        });
      }
      if (url.endsWith("/api/apps/install_waba/whatsapp-settings/official/connect")) {
        return jsonResponse({ url: "https://portal.favcrm.example.test/wa/connect?token=ok", expires_at: "2026-11-23T12:00:00Z" });
      }
      if (url.endsWith("/api/apps/install_waba/whatsapp-settings/official/rescope")) {
        return jsonResponse({
          official: {
            available: true,
            enabled: true,
            paused: false,
            connected: true,
            binding_status: "active",
            company_name: "Demo Business",
          },
        });
      }
      if (url.endsWith("/api/apps/install_waba/whatsapp-settings/official/disconnect")) {
        return jsonResponse({
          disconnected: true,
          favcrm_revoked: true,
          official: {
            available: true,
            enabled: true,
            paused: false,
            connected: false,
            binding_status: "revoked",
            company_name: null,
          },
        });
      }
      return jsonResponse({}, 500);
    }) as typeof fetch;

    const ownerSnap = await readWhatsAppSettings("install_waba");
    assert(ownerSnap.viewer_role === "owner" && ownerSnap.linked.available && ownerSnap.linked.qr === "OWNER_QR_PAYLOAD", "owner read returns pairing QR");
    assert(seenRequests[0]?.cache === "no-store" && seenRequests[0]?.credentials === "same-origin", "read sends no-store + same-origin");

    const memberSnap = await readWhatsAppSettings("install_member");
    assert(memberSnap.viewer_role === "member" && memberSnap.linked.available && memberSnap.linked.qr === null, "member read strips QR defensively");

    let badPathRejected = false;
    try {
      await readWhatsAppSettings("../escape");
    } catch {
      badPathRejected = true;
    }
    assert(badPathRejected, "unsafe installId rejected before fetch");

    let unsupportedCaught = false;
    try {
      await readWhatsAppSettings("install_old");
    } catch (err) {
      unsupportedCaught = isWhatsAppSettingsUnsupported(err);
    }
    assert(unsupportedCaught, "404 triggers graceful older-board fallback detection");
    assert(isWhatsAppSettingsUnsupported(new ApiError("Not implemented", 501)), "501 also triggers fallback");

    let conflictCode: string | undefined;
    try {
      await saveWhatsAppProvider("install_waba", "official", 1);
    } catch (err) {
      conflictCode = err instanceof ApiError ? err.code : undefined;
    }
    assert(conflictCode === "revision_conflict", "provider conflict propagates revision_conflict");
    const putReq = seenRequests.find((r) => r.method === "PUT");
    assert(putReq?.contentType === "application/json" && putReq.body === JSON.stringify({ provider: "official", expected_revision: 1 }), "PUT sends exact Content-Type and expected_revision");

    let uncertainCaught = false;
    try {
      await runLinkedWhatsAppAction("install_waba", "start", { timeoutMs: 15 });
    } catch (err) {
      uncertainCaught = err instanceof WhatsAppActionUncertainError;
    }
    assert(uncertainCaught, "aborted timeout raises WhatsAppActionUncertainError");

    let unsafeUrlRejected = false;
    try {
      await runOfficialWhatsAppAction("install_unsafe_official", "connect");
    } catch {
      unsafeUrlRejected = true;
    }
    assert(unsafeUrlRejected, "non-HTTPS Official connect link is rejected");

    let nullUrlRejected = false;
    try {
      await runOfficialWhatsAppAction("install_null_official", "connect");
    } catch {
      nullUrlRejected = true;
    }
    assert(nullUrlRejected, "missing/null Official connect link fails closed");

    let badRoleRejected = false;
    try {
      await readWhatsAppSettings("install_bad_role");
    } catch {
      badRoleRejected = true;
    }
    assert(badRoleRejected, "malformed viewer_role fails closed");

    const officialOk = await runOfficialWhatsAppAction("install_waba", "connect", {
      returnTo: "https://demo.cadencecloud.app/apps/waba?mode=official",
    });
    const connectReq = [...seenRequests].reverse().find((r) => r.url.endsWith("/official/connect"));
    assert(
      officialOk.connect?.url === "https://portal.favcrm.example.test/wa/connect?token=ok" &&
        connectReq?.body === JSON.stringify({ return_to: "https://demo.cadencecloud.app/apps/waba?mode=official" }),
      "valid HTTPS Official connect URL accepted and forwards return_to",
    );

    const rescopeOk = await runOfficialWhatsAppAction("install_waba", "rescope");
    assert(rescopeOk.official?.available && rescopeOk.official.binding_status === "active", "Official rescope refreshes snapshot");

    const disconnectOk = await runOfficialWhatsAppAction("install_waba", "disconnect");
    assert(disconnectOk.disconnected === true && disconnectOk.favcrm_revoked === true && disconnectOk.official?.available && disconnectOk.official.binding_status === "revoked", "Official disconnect returns revoked snapshot");

    // 3. Idle nudge + ManageApp summary helpers + private session reset
    const runningLinkedOfficialFocus: WhatsAppSettingsSnapshot = {
      viewer_role: "owner",
      active_provider: "official",
      reply_mode: "copilot",
      settings_revision: 3,
      linked: { available: true, granted: true, state: "connected", connected: true, qr: null },
      official: {
        available: true,
        enabled: true,
        paused: false,
        connected: true,
        binding_status: "active",
        company_name: "Alpha Co",
        display_phone_number: "+852 5555 0148",
        verified_name: "Alpha Co",
        waba_id: "waba_1",
        phone_number_id: "pn_1",
        webhook: null,
        observed_at: null,
      },
    };
    assert(shouldShowLinkedIdleNudge(runningLinkedOfficialFocus, "official", false), "owner viewing official while linked connected sees idle pause nudge");
    assert(!shouldShowLinkedIdleNudge(runningLinkedOfficialFocus, "official", true), "dismissed nudge stays hidden");
    assert(!shouldShowLinkedIdleNudge({ ...runningLinkedOfficialFocus, viewer_role: "member" }, "official", false), "members do not see pause nudge");
    assert(!shouldShowLinkedIdleNudge(runningLinkedOfficialFocus, "linked", false), "linked view hides switch nudge");

    const sumOfficial = whatsappSettingsSummary(runningLinkedOfficialFocus);
    assert(sumOfficial.activeMode === "official" && sumOfficial.statusTone === "ok" && sumOfficial.statusLabel === "Connected", "summary projects connected official");
    const sumPausedLinked = whatsappSettingsSummary({
      ...runningLinkedOfficialFocus,
      active_provider: "linked",
      linked: { available: true, granted: true, state: "stopped", connected: false, qr: null },
    });
    assert(sumPausedLinked.activeMode === "linked" && sumPausedLinked.statusTone === "muted" && sumPausedLinked.statusLabel === "Paused", "summary projects stopped linked");

    markWhatsAppSettingsUnsupported("install_old");
    assert(isMarkedWhatsAppSettingsUnsupported("install_old"), "unsupported install cached");
    resetPrivateSession();
    allowPrivateReads();
    assert(!isMarkedWhatsAppSettingsUnsupported("install_old"), "private session reset clears unsupported install cache");
  } finally {
    globalThis.fetch = originalFetch;
  }

  await testPanelOAuthRedirectAndDisconnect();
  console.log("whatsappSettings tests passed");
}

void main();
