import { ApiError } from "../../lib/apiError";
import { fmtTime } from "../../lib/fmt";
import type {
  CustomMcpConnectionSummary,
  CustomMcpStoredAuthKind,
  CustomMcpToolMode,
  CustomMcpToolPin,
} from "../../lib/types";

/**
 * Pure presentation, validation, and return-param helpers for Custom MCP
 * connectors (CAD-2556), matching `agenticos-v2` AOS-682 contracts.
 */

const CONNECTION_ID_RE = /^conn_mcp_[A-Za-z0-9_-]{8,64}$/;
const HEADER_NAME_RE = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
const FORBIDDEN_HOST_SUFFIXES = [
  ".localhost",
  ".local",
  ".internal",
  ".home.arpa",
  ".localdomain",
  ".corp",
  ".lan",
  ".cadencecloud.app",
  ".agenticos.hk",
] as const;
const FORBIDDEN_EXACT_HOSTS = new Set([
  "localhost",
  "ip6-localhost",
  "ip6-loopback",
  "metadata.google.internal",
  "metadata.goog",
  "cadencecloud.app",
  "agenticos.hk",
  "tenant-board-control.internal",
]);
const FORBIDDEN_CUSTOM_HEADERS = new Set([
  "host",
  "content-length",
  "transfer-encoding",
  "connection",
  "proxy-authorization",
  "te",
  "trailer",
  "upgrade",
]);

function isBlockedIpv4(host: string): boolean {
  if (/^\d+$/.test(host) || /^0x[0-9a-f]+$/i.test(host)) return true;
  const parts = host.split(".");
  if (parts.length !== 4 || parts.some((p) => !/^\d{1,3}$/.test(p))) return false;
  const nums = parts.map(Number);
  if (nums.some((n) => n < 0 || n > 255)) return true;
  const [a, b, c] = nums;
  if (a === 0 || a === 10 || a === 127) return true;
  if (a === 100 && b >= 64 && b <= 127) return true;
  if (a === 169 && b === 254) return true;
  if (a === 172 && b >= 16 && b <= 31) return true;
  if (a === 192 && b === 0 && (c === 0 || c === 2)) return true;
  if (a === 192 && b === 88 && c === 99) return true;
  if (a === 192 && b === 168) return true;
  if (a === 198 && (b === 18 || b === 19)) return true;
  if (a === 198 && b === 51 && c === 100) return true;
  if (a === 203 && b === 0 && c === 113) return true;
  return a >= 224;
}

function isBlockedIpv6(raw: string): boolean {
  const host = raw.split("%")[0].toLowerCase();
  if (host === "::" || host === "::1" || host === "0:0:0:0:0:0:0:1" || host === "0:0:0:0:0:0:0:0") {
    return true;
  }
  if (host.startsWith("fc") || host.startsWith("fd")) return true;
  if (host.startsWith("fe8") || host.startsWith("fe9") || host.startsWith("fea") || host.startsWith("feb")) {
    return true;
  }
  if (host.startsWith("fec") || host.startsWith("ff") || host.startsWith("2001:db8:")) return true;
  if (host.includes("::ffff:")) return true;
  return false;
}

/** Client-side UX pre-check matching `validateCustomMcpEndpoint` (server remains authoritative). */
export function validateCustomMcpUrlInput(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed) return "Enter the server's https:// MCP endpoint URL.";
  if (trimmed.length > 2048 || /[\u0000-\u0020\u007f]/.test(trimmed)) {
    return "Endpoint URL is invalid.";
  }
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return "Enter a valid https:// URL.";
  }
  if (url.protocol !== "https:") {
    return "Custom MCP endpoints must use https://.";
  }
  if (url.username || url.password) {
    return "Endpoint URL must not embed credentials.";
  }
  const host = url.hostname.replace(/^\[|\]$/g, "").replace(/\.+$/, "").toLowerCase();
  if (!host) return "Endpoint URL must name a host.";
  if (
    FORBIDDEN_EXACT_HOSTS.has(host) ||
    FORBIDDEN_HOST_SUFFIXES.some((suffix) => host.endsWith(suffix))
  ) {
    return "Internal or platform hosts are blocked.";
  }
  if (host.includes(":")) {
    if (isBlockedIpv6(host)) return "Private or reserved IPv6 endpoints are blocked.";
    return null;
  }
  if (isBlockedIpv4(host)) {
    return "Loopback, private, or reserved IPv4 endpoints are blocked.";
  }
  if (!host.includes(".")) {
    return "Single-label internal hostnames are blocked.";
  }
  return null;
}

/** Validates `{ authorization_url }` from Custom MCP create or OAuth reconnect. */
export function customMcpAuthorizationUrl(reply: unknown): string | null {
  if (!reply || typeof reply !== "object" || Array.isArray(reply)) return null;
  const raw = (reply as { authorization_url?: unknown }).authorization_url;
  if (typeof raw !== "string") return null;
  try {
    const url = new URL(raw);
    return url.protocol === "https:" ? url.toString() : null;
  } catch {
    return null;
  }
}

export type CustomMcpReturn = {
  outcome: "connected" | "cancelled" | "failed";
  connectionId: string | null;
};

/** Parses `?aos_connect=...&toolkit=custom_mcp&connection_id=...` after OAuth callback return. */
export function customMcpReturn(search: string): CustomMcpReturn | null {
  const query = new URLSearchParams(search);
  if (query.get("toolkit") !== "custom_mcp") return null;
  const outcome = query.get("aos_connect");
  if (outcome !== "connected" && outcome !== "cancelled" && outcome !== "failed") return null;
  const rawId = query.get("connection_id");
  const connectionId = rawId !== null && CONNECTION_ID_RE.test(rawId) ? rawId : null;
  return { outcome, connectionId };
}

/** Reads `?mcp_focus=<label>` when navigated from a chat ConnectCard. */
export function mcpFocusParam(search: string): string | null {
  const raw = new URLSearchParams(search).get("mcp_focus")?.trim() ?? "";
  return raw ? raw.slice(0, 200) : null;
}

export function customMcpAuthLabel(authKind: CustomMcpStoredAuthKind): string {
  switch (authKind) {
    case "oauth":
      return "OAuth 2.1";
    case "oauth_static":
      return "OAuth client";
    case "bearer":
      return "Bearer token";
    case "header":
      return "Custom headers";
    case "none":
      return "No auth";
  }
}

export function customMcpStatusView(conn: Pick<CustomMcpConnectionSummary, "status" | "tools_summary" | "instructions_drifted">): {
  label: string;
  tone: "ok" | "warn" | "fail" | "muted";
  detail: string;
} {
  switch (conn.status) {
    case "active":
      return {
        label: "Active",
        tone: "ok",
        detail: `${conn.tools_summary.read} read · ${conn.tools_summary.approval} ask first · ${conn.tools_summary.off} off`,
      };
    case "needs_review": {
      const reasons: string[] = [];
      if (conn.tools_summary.drifted > 0) {
        reasons.push(`${conn.tools_summary.drifted} tool${conn.tools_summary.drifted === 1 ? "" : "s"} drifted`);
      }
      if (conn.instructions_drifted) reasons.push("server instructions changed");
      return {
        label: "Drift review",
        tone: "warn",
        detail: reasons.join(" · ") || "Server contract changed — review before calling drifted tools.",
      };
    }
    case "auth_expired":
      return {
        label: "Auth expired",
        tone: "fail",
        detail: "Reconnect or rotate credentials before running tools.",
      };
    case "pending_oauth":
      return {
        label: "Awaiting sign-in",
        tone: "warn",
        detail: "Finish OAuth sign-in to discover and pin tools.",
      };
    case "revoked":
      return {
        label: "Disconnected",
        tone: "muted",
        detail: "This Custom MCP server has been disconnected.",
      };
  }
}

export function customMcpToolModeLabel(mode: CustomMcpToolMode): string {
  switch (mode) {
    case "off":
      return "Off";
    case "approval":
      return "Ask approval";
    case "read":
      return "Auto-read";
  }
}

export function customMcpSyncedText(ms: number | null): string {
  return ms === null ? "Not synced yet" : fmtTime(new Date(ms).toISOString());
}

/** Parse multiline `Header-Name: Value` pairs for header-authenticated MCP servers. */
export function parseCustomHeadersInput(raw: string): {
  headers: Array<{ name: string; value: string }>;
  error: string | null;
} {
  const lines = raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  if (lines.length === 0) {
    return { headers: [], error: "Enter at least one Header: Value line." };
  }
  if (lines.length > 10) {
    return { headers: [], error: "At most 10 custom headers are allowed." };
  }
  const headers: Array<{ name: string; value: string }> = [];
  const seen = new Set<string>();
  for (const line of lines) {
    const colon = line.indexOf(":");
    if (colon <= 0) {
      return { headers: [], error: "Each header must use the format Header-Name: value." };
    }
    const name = line.slice(0, colon).trim();
    const value = line.slice(colon + 1).trim();
    if (!HEADER_NAME_RE.test(name) || name.length > 64) {
      return { headers: [], error: `Invalid header name "${name}".` };
    }
    if (FORBIDDEN_CUSTOM_HEADERS.has(name.toLowerCase())) {
      return { headers: [], error: `Header "${name}" cannot be overridden.` };
    }
    if (!value || value.length > 4096) {
      return { headers: [], error: `Header "${name}" must have a non-empty value.` };
    }
    const lower = name.toLowerCase();
    if (seen.has(lower)) {
      return { headers: [], error: `Duplicate header "${name}".` };
    }
    seen.add(lower);
    headers.push({ name, value });
  }
  return { headers, error: null };
}

const CUSTOM_MCP_ERROR_COPY: Record<string, string> = {
  owner_required: "Only a workspace owner can manage Custom MCP servers.",
  ssrf_blocked: "That endpoint or OAuth host is blocked (HTTPS public URLs only).",
  ssrf_redirect_blocked: "The remote server redirected to a blocked destination.",
  invalid_url: "Enter a valid https:// URL.",
  invalid_request: "Check the form fields and try again.",
  oauth_not_supported: "The server did not start an OAuth 2.1 flow. Try Manual auth (Bearer token or headers) if it uses static credentials.",
  oauth_start_failed: "Could not start OAuth discovery or registration with that server.",
  mcp_connect_failed: "Could not connect to the Custom MCP server or complete initialization.",
  auth_expired: "Authorization expired or was rejected by the server. Reconnect or rotate credentials.",
  drift_review_required: "The server's tools or instructions changed — review and accept drift first.",
  stale_revision: "The server snapshot changed while saving. Refresh and review again.",
  tool_disabled: "That tool is disabled.",
  invalid_guide_content_type: "Guide URL must serve plain text or Markdown (text/markdown or text/plain).",
  response_too_large: "The remote response exceeded the maximum allowed size.",
  guide_fetch_failed: "Could not fetch the Agent Guide URL.",
  validation_failed: "Check the entered fields and try again.",
  not_found: "That Custom MCP connection no longer exists.",
  mcp_upstream_error: "The remote Custom MCP server reported an error.",
  capability_unavailable: "Custom MCP operation could not be completed.",
};

export function customMcpErrorText(error: unknown): string {
  const code = error instanceof ApiError ? error.code : undefined;
  if (code && code in CUSTOM_MCP_ERROR_COPY) return CUSTOM_MCP_ERROR_COPY[code];
  return "Could not complete the Custom MCP request. Try again.";
}

export function isToolReadOnlyHint(pin: Pick<CustomMcpToolPin, "annotations">): boolean {
  return pin.annotations?.readOnlyHint === true;
}

export function isToolDestructiveHint(pin: Pick<CustomMcpToolPin, "annotations">): boolean {
  return pin.annotations?.destructiveHint === true;
}

/** Summarizes top-level schema parameter names for quick inspection in the drawer. */
export function schemaParamSummary(schema: Record<string, unknown>): string[] {
  const props = schema.properties;
  if (!props || typeof props !== "object" || Array.isArray(props)) return [];
  const required = new Set(
    Array.isArray(schema.required) ? schema.required.filter((v): v is string => typeof v === "string") : [],
  );
  return Object.keys(props)
    .slice(0, 12)
    .map((key) => (required.has(key) ? `${key}*` : key));
}
