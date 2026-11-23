import { ApiError, errorMessage, errorRequestId } from "../../lib/apiError";
import {
  onPrivateSessionReset,
  privateFetch,
  withPrivateOperation,
} from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";

export type WhatsAppProvider = "linked" | "official";

export type LinkedWhatsAppState =
  | "idle"
  | "connecting"
  | "pairing"
  | "connected"
  | "stopped"
  | "disconnected"
  | "reauthorization_required"
  | "not_granted";

export type LinkedWhatsAppSnapshot =
  | {
      available: true;
      granted: boolean;
      state: LinkedWhatsAppState;
      connected: boolean;
      qr: string | null;
    }
  | {
      available: false;
      granted: boolean;
      error_code: string;
    };

export type OfficialWhatsAppWebhook = {
  subscribed: boolean | null;
  status: number | null;
  error: string | null;
  fields: string[];
};

export type OfficialWhatsAppSnapshot =
  | {
      available: true;
      enabled: boolean;
      paused: boolean;
      connected: boolean;
      binding_status: string;
      company_name: string | null;
      display_phone_number: string | null;
      verified_name: string | null;
      waba_id: string | null;
      phone_number_id: string | null;
      webhook: OfficialWhatsAppWebhook | null;
      observed_at: string | null;
    }
  | {
      available: false;
      error_code: string;
    };

export type WhatsAppSettingsSnapshot = {
  viewer_role: "owner" | "member";
  active_provider: WhatsAppProvider | null;
  reply_mode: "copilot" | "human-only";
  settings_revision: number | null;
  linked: LinkedWhatsAppSnapshot;
  official: OfficialWhatsAppSnapshot;
};

export type WhatsAppProviderSaveResult = {
  active_provider: WhatsAppProvider;
  reply_mode: "copilot" | "human-only";
  settings_revision: number;
};

export type OfficialConnectResult = {
  url: string;
  expires_at: string | null;
};

export class WhatsAppActionUncertainError extends Error {
  constructor(message = "The request timed out before confirmation.") {
    super(message);
    this.name = "WhatsAppActionUncertainError";
  }
}

const INSTALL_ID = /^[A-Za-z0-9_-]{1,128}$/;
const LINKED_STATES = new Set<string>([
  "idle",
  "connecting",
  "pairing",
  "connected",
  "stopped",
  "disconnected",
  "reauthorization_required",
  "not_granted",
]);
const MUTATION_TIMEOUT_MS = 20_000;

const unsupportedInstalls = new Set<string>();
onPrivateSessionReset(() => {
  unsupportedInstalls.clear();
});

export function markWhatsAppSettingsUnsupported(installId: string): void {
  if (INSTALL_ID.test(installId)) unsupportedInstalls.add(installId);
}

export function isMarkedWhatsAppSettingsUnsupported(installId: string): boolean {
  return unsupportedInstalls.has(installId);
}

export function isWhatsAppSettingsUnsupported(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 404 || error.status === 501);
}

export function parseWhatsAppModeParam(value: string | null): WhatsAppProvider | null {
  if (value === "linked" || value === "official") return value;
  return null;
}

function assertSafeInstallId(installId: string): string {
  if (!INSTALL_ID.test(installId)) {
    throw new Error("Invalid WhatsApp app installation id.");
  }
  return encodeURIComponent(installId);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function errorCodeOf(body: unknown): string | undefined {
  if (!isRecord(body)) return undefined;
  if (typeof body.code === "string") return body.code;
  const nested = body.error;
  return isRecord(nested) && typeof nested.code === "string" ? nested.code : undefined;
}

function buildApiError(body: unknown, response: Response, fallback: string): ApiError {
  return new ApiError(errorMessage(body, fallback), response.status, {
    code: errorCodeOf(body),
    requestId: errorRequestId(body, response),
  });
}

function parseHttpsUrl(raw: unknown): string | null {
  if (typeof raw !== "string" || !raw.trim()) return null;
  try {
    const url = new URL(raw);
    if (url.protocol !== "https:" || url.username || url.password) return null;
    return url.href;
  } catch {
    return null;
  }
}

export function parseLinkedSnapshot(
  raw: unknown,
  viewerRole: "owner" | "member",
): LinkedWhatsAppSnapshot {
  if (!isRecord(raw) || typeof raw.available !== "boolean") {
    throw new Error("Invalid Linked WhatsApp status payload.");
  }
  if (!raw.available) {
    return {
      available: false,
      granted: raw.granted !== false,
      error_code: typeof raw.error_code === "string" ? raw.error_code : "capability_unavailable",
    };
  }
  if (
    typeof raw.state !== "string" ||
    !LINKED_STATES.has(raw.state) ||
    typeof raw.granted !== "boolean" ||
    typeof raw.connected !== "boolean"
  ) {
    throw new Error("Invalid Linked WhatsApp status payload.");
  }
  const state = raw.state as LinkedWhatsAppState;
  const granted = raw.granted;
  const connected = raw.connected;
  const qr =
    viewerRole === "owner" &&
    granted &&
    state === "pairing" &&
    typeof raw.qr === "string" &&
    raw.qr.length > 0
      ? raw.qr
      : null;
  return {
    available: true,
    granted,
    state,
    connected,
    qr,
  };
}

export function parseOfficialSnapshot(raw: unknown): OfficialWhatsAppSnapshot {
  if (!isRecord(raw) || typeof raw.available !== "boolean") {
    throw new Error("Invalid Official WhatsApp status payload.");
  }
  if (!raw.available) {
    return {
      available: false,
      error_code: typeof raw.error_code === "string" ? raw.error_code : "capability_unavailable",
    };
  }
  const webhookRaw = isRecord(raw.webhook) ? raw.webhook : null;
  const webhook: OfficialWhatsAppWebhook | null = webhookRaw
    ? {
        subscribed: typeof webhookRaw.subscribed === "boolean" ? webhookRaw.subscribed : null,
        status: typeof webhookRaw.status === "number" ? webhookRaw.status : null,
        error: typeof webhookRaw.error === "string" ? webhookRaw.error : null,
        fields: Array.isArray(webhookRaw.fields)
          ? webhookRaw.fields.filter((f): f is string => typeof f === "string")
          : [],
      }
    : null;
  const enabled = raw.enabled !== false;
  return {
    available: true,
    enabled,
    paused: typeof raw.paused === "boolean" ? raw.paused : !enabled,
    connected: raw.connected === true,
    binding_status: typeof raw.binding_status === "string" ? raw.binding_status : "none",
    company_name: typeof raw.company_name === "string" ? raw.company_name : null,
    display_phone_number:
      typeof raw.display_phone_number === "string" ? raw.display_phone_number : null,
    verified_name: typeof raw.verified_name === "string" ? raw.verified_name : null,
    waba_id: typeof raw.waba_id === "string" ? raw.waba_id : null,
    phone_number_id: typeof raw.phone_number_id === "string" ? raw.phone_number_id : null,
    webhook,
    observed_at: typeof raw.observed_at === "string" ? raw.observed_at : null,
  };
}

function parseSettingsSnapshot(raw: unknown): WhatsAppSettingsSnapshot {
  if (
    !isRecord(raw) ||
    (raw.viewer_role !== "owner" && raw.viewer_role !== "member") ||
    (raw.active_provider !== null &&
      raw.active_provider !== undefined &&
      raw.active_provider !== "linked" &&
      raw.active_provider !== "official")
  ) {
    throw new Error("Invalid WhatsApp settings response.");
  }
  const viewerRole = raw.viewer_role;
  const activeProvider = raw.active_provider ?? null;
  const replyMode = raw.reply_mode === "human-only" ? "human-only" : "copilot";
  const settingsRevision =
    typeof raw.settings_revision === "number" &&
    Number.isSafeInteger(raw.settings_revision) &&
    raw.settings_revision >= 1
      ? raw.settings_revision
      : null;
  return {
    viewer_role: viewerRole,
    active_provider: activeProvider,
    reply_mode: replyMode,
    settings_revision: settingsRevision,
    linked: parseLinkedSnapshot(raw.linked, viewerRole),
    official: parseOfficialSnapshot(raw.official),
  };
}

export async function readWhatsAppSettings(
  installId: string,
  signal?: AbortSignal,
): Promise<WhatsAppSettingsSnapshot> {
  const safeId = assertSafeInstallId(installId);
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(
      `/api/apps/${safeId}/whatsapp-settings`,
      {
        credentials: "same-origin",
        cache: "no-store",
        headers: sessionHeaders(),
      },
      operation,
    );
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) {
      throw buildApiError(body, response, "Could not load WhatsApp settings.");
    }
    return parseSettingsSnapshot(body);
  }, signal);
}

export async function saveWhatsAppProvider(
  installId: string,
  provider: WhatsAppProvider,
  expectedRevision: number | null,
  opts?: { timeoutMs?: number },
): Promise<WhatsAppProviderSaveResult> {
  const safeId = assertSafeInstallId(installId);
  return postJsonMutation(
    `/api/apps/${safeId}/whatsapp-settings/provider`,
    "PUT",
    { provider, expected_revision: expectedRevision },
    (body) => {
      if (
        !isRecord(body) ||
        (body.active_provider !== "linked" && body.active_provider !== "official") ||
        typeof body.settings_revision !== "number"
      ) {
        throw new Error("Invalid provider update response.");
      }
      return {
        active_provider: body.active_provider,
        reply_mode: body.reply_mode === "human-only" ? "human-only" : "copilot",
        settings_revision: body.settings_revision,
      };
    },
    "Could not save WhatsApp mode.",
    opts?.timeoutMs,
  );
}

export async function runLinkedWhatsAppAction(
  installId: string,
  action: "start" | "stop" | "reset",
  opts?: { timeoutMs?: number },
): Promise<LinkedWhatsAppSnapshot> {
  const safeId = assertSafeInstallId(installId);
  return postJsonMutation(
    `/api/apps/${safeId}/whatsapp-settings/linked/${action}`,
    "POST",
    {},
    (body) => {
      if (!isRecord(body)) throw new Error("Invalid Linked WhatsApp response.");
      return parseLinkedSnapshot(body.linked, "owner");
    },
    "Could not update Linked WhatsApp device.",
    opts?.timeoutMs,
  );
}

export async function runOfficialWhatsAppAction(
  installId: string,
  action: "connect" | "start" | "stop",
  opts?: { timeoutMs?: number },
): Promise<{ connect?: OfficialConnectResult; official?: OfficialWhatsAppSnapshot }> {
  const safeId = assertSafeInstallId(installId);
  return postJsonMutation(
    `/api/apps/${safeId}/whatsapp-settings/official/${action}`,
    "POST",
    {},
    (body) => {
      if (!isRecord(body)) throw new Error("Invalid Official WhatsApp response.");
      if (action === "connect") {
        const safeUrl = parseHttpsUrl(body.url);
        if (!safeUrl) {
          throw new Error("Missing or unsafe Official WhatsApp authorization link.");
        }
        return {
          connect: {
            url: safeUrl,
            expires_at: typeof body.expires_at === "string" ? body.expires_at : null,
          },
        };
      }
      return {
        official: parseOfficialSnapshot(body.official),
      };
    },
    "Could not update Official WhatsApp connection.",
    opts?.timeoutMs,
  );
}

async function postJsonMutation<T>(
  path: string,
  method: "POST" | "PUT",
  payload: object,
  parse: (body: unknown) => T,
  fallbackError: string,
  timeoutMs = MUTATION_TIMEOUT_MS,
): Promise<T> {
  const timeout = new AbortController();
  const timer = setTimeout(() => timeout.abort(), timeoutMs);
  try {
    return await withPrivateOperation(async (operation) => {
      let response: Response;
      try {
        response = await privateFetch(
          path,
          {
            method,
            credentials: "same-origin",
            cache: "no-store",
            headers: {
              "Content-Type": "application/json",
              "X-Cadence-Board": "1",
              ...sessionHeaders(),
            },
            body: JSON.stringify(payload),
          },
          operation,
        );
      } catch (err) {
        if (timeout.signal.aborted) {
          throw new WhatsAppActionUncertainError();
        }
        if (err instanceof TypeError) {
          throw new WhatsAppActionUncertainError("Connection dropped before confirmation.");
        }
        throw err;
      }
      const body: unknown = await response.json().catch(() => null);
      operation.assertCurrent();
      if (!response.ok) {
        throw buildApiError(body, response, fallbackError);
      }
      return parse(body);
    }, timeout.signal);
  } catch (err) {
    if (timeout.signal.aborted) {
      throw new WhatsAppActionUncertainError();
    }
    throw err;
  } finally {
    clearTimeout(timer);
  }
}

export function shouldShowLinkedIdleNudge(
  snapshot: WhatsAppSettingsSnapshot,
  focusedMode: WhatsAppProvider,
  dismissed: boolean,
): boolean {
  if (dismissed || snapshot.viewer_role !== "owner" || focusedMode !== "official") return false;
  if (!snapshot.linked.available || !snapshot.linked.granted) return false;
  return (
    snapshot.linked.state === "connected" ||
    snapshot.linked.state === "pairing" ||
    snapshot.linked.state === "connecting"
  );
}

export function whatsappSettingsSummary(snapshot: WhatsAppSettingsSnapshot): {
  activeMode: WhatsAppProvider | null;
  statusTone: "ok" | "warn" | "bad" | "muted";
  statusLabel: string;
} {
  const activeMode = snapshot.active_provider;
  if (activeMode === "official") {
    if (!snapshot.official.available) {
      return { activeMode, statusTone: "warn", statusLabel: "Status unavailable" };
    }
    if (snapshot.official.paused) {
      return { activeMode, statusTone: "muted", statusLabel: "Paused" };
    }
    if (snapshot.official.connected) {
      return { activeMode, statusTone: "ok", statusLabel: "Connected" };
    }
    return { activeMode, statusTone: "muted", statusLabel: "Not connected" };
  }
  if (activeMode === "linked") {
    if (!snapshot.linked.available) {
      return { activeMode, statusTone: "warn", statusLabel: "Status unavailable" };
    }
    switch (snapshot.linked.state) {
      case "connected":
        return { activeMode, statusTone: "ok", statusLabel: "Connected" };
      case "pairing":
      case "connecting":
      case "disconnected":
        return { activeMode, statusTone: "warn", statusLabel: "Connecting" };
      case "stopped":
        return { activeMode, statusTone: "muted", statusLabel: "Paused" };
      case "reauthorization_required":
        return { activeMode, statusTone: "bad", statusLabel: "Needs re-link" };
      case "not_granted":
        return { activeMode, statusTone: "muted", statusLabel: "Not enabled" };
      default:
        return { activeMode, statusTone: "muted", statusLabel: "Not linked" };
    }
  }
  return { activeMode: null, statusTone: "muted", statusLabel: "Not set" };
}
