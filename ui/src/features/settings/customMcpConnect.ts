import { api } from "../../lib/api";
import type {
  CustomMcpAuthKind,
  CustomMcpConnectionSummary,
  CustomMcpOAuthDiscovery,
} from "../../lib/types";
import {
  customMcpAuthorizationUrl,
  customMcpErrorText,
  isSameSiteHost,
  parseCustomHeadersInput,
  validateCustomMcpUrlInput,
} from "./customMcpView";

export interface CustomMcpConnectInput {
  name: string;
  endpointUrl: string;
  /** Optional pre-computed URL validation error (e.g. origin lock check). */
  urlError?: string | null;
  visibility: "shared" | "personal";
  authKind: CustomMcpAuthKind;
  bearerToken: string;
  customHeadersText: string;
  oauthClientId: string;
  oauthClientSecret: string;
  oauthScopesText: string;
  requiredScopesPreset?: string[];
  /** Base return URL for chat setup-card flows (appends `?mcp_attempt=<nonce>`). */
  returnToBase?: string;
  /** Reject direct non-OAuth completion when an OAuth flow is expected. */
  requireOAuthRedirect?: boolean;
}

export type CustomMcpConnectResult =
  | { ok: false; error: string }
  | {
      ok: true;
      kind: "oauth_redirect";
      authorizationUrl: string;
      connectionId: string;
      normalizedEndpoint: string;
      nonce: string;
      sameSite: boolean;
      discovery?: CustomMcpOAuthDiscovery;
    }
  | {
      ok: true;
      kind: "connected";
      connection: CustomMcpConnectionSummary;
    };

/**
 * Shared validation, `POST /api/connections/mcp` submission, OAuth scope check,
 * and same-site check used by both the Settings drawer and the chat Confirm Modal.
 */
export async function submitCustomMcpConnect(
  input: CustomMcpConnectInput,
): Promise<CustomMcpConnectResult> {
  const trimmedName = input.name.trim();
  if (!trimmedName) {
    return { ok: false, error: "Enter a name for this Custom MCP server." };
  }
  const urlError =
    input.urlError !== undefined
      ? input.urlError
      : validateCustomMcpUrlInput(input.endpointUrl);
  if (urlError !== null) {
    return { ok: false, error: urlError };
  }

  let parsedHeaders: Array<{ name: string; value: string }> | undefined;
  if (input.authKind === "header") {
    const checked = parseCustomHeadersInput(input.customHeadersText);
    if (checked.error !== null) {
      return { ok: false, error: checked.error };
    }
    parsedHeaders = checked.headers;
  }
  if (input.authKind === "bearer" && !input.bearerToken.trim()) {
    return { ok: false, error: "Enter the Bearer token or API key." };
  }
  if (input.authKind === "oauth_static" && !input.oauthClientId.trim()) {
    return { ok: false, error: "Enter the OAuth client_id." };
  }

  const manualScopes = input.oauthScopesText
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
  const isSetupFlow = Boolean(input.returnToBase);
  const requiredScopes = isSetupFlow
    ? input.requiredScopesPreset ??
      (input.authKind === "oauth_static" && manualScopes.length ? manualScopes : undefined)
    : undefined;
  const nonce = isSetupFlow ? crypto.randomUUID() : "";
  const returnTo = input.returnToBase
    ? (() => {
        const url = new URL(input.returnToBase);
        url.searchParams.set("mcp_attempt", nonce);
        return url.href;
      })()
    : typeof window !== "undefined"
      ? `${window.location.origin}/settings/connections`
      : undefined;

  try {
    const reply = await api.customMcpCreate({
      name: trimmedName,
      endpoint_url: input.endpointUrl.trim(),
      auth_kind: isSetupFlow && input.authKind === "auto" ? "oauth" : input.authKind,
      visibility: input.visibility,
      ...(requiredScopes ? { required_scopes: requiredScopes } : {}),
      ...(input.authKind === "bearer" ? { bearer_token: input.bearerToken.trim() } : {}),
      ...(input.authKind === "header" && parsedHeaders ? { custom_headers: parsedHeaders } : {}),
      ...(input.authKind === "oauth_static"
        ? {
            oauth_client: {
              client_id: input.oauthClientId.trim(),
              ...(input.oauthClientSecret.trim()
                ? { client_secret: input.oauthClientSecret.trim() }
                : {}),
              ...(manualScopes.length > 0 ? { scopes: manualScopes } : {}),
            },
          }
        : {}),
      ...(returnTo ? { return_to: returnTo } : {}),
    });

    if (reply.mode === "oauth_redirect") {
      const safeUrl = customMcpAuthorizationUrl(reply);
      if (safeUrl === null) {
        return { ok: false, error: "Server did not return a valid https:// authorization URL." };
      }
      if (requiredScopes) {
        const requested = new URL(safeUrl).searchParams.getAll("scope");
        const actualScopes = requested[0]?.split(" ") ?? [];
        if (
          requested.length !== 1 ||
          actualScopes.length !== requiredScopes.length ||
          requiredScopes.some((scope) => !actualScopes.includes(scope))
        ) {
          return {
            ok: false,
            error: "The provider did not request exactly the required OAuth scopes.",
          };
        }
      }
      const trimmedUrl = input.endpointUrl.trim();
      return {
        ok: true,
        kind: "oauth_redirect",
        authorizationUrl: safeUrl,
        connectionId: reply.connection_id,
        normalizedEndpoint: new URL(trimmedUrl).href,
        nonce,
        sameSite: isSameSiteHost(trimmedUrl, safeUrl),
        ...(reply.discovery ? { discovery: reply.discovery } : {}),
      };
    }

    if (input.requireOAuthRedirect) {
      return { ok: false, error: "This server did not start OAuth sign-in." };
    }
    return { ok: true, kind: "connected", connection: reply.connection };
  } catch (e: unknown) {
    return { ok: false, error: customMcpErrorText(e) };
  }
}
