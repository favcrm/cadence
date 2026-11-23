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

    const officialOk = await runOfficialWhatsAppAction("install_waba", "connect");
    assert(officialOk.connect?.url === "https://portal.favcrm.example.test/wa/connect?token=ok", "valid HTTPS Official connect URL accepted");

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

  console.log("whatsappSettings tests passed");
}

void main();
