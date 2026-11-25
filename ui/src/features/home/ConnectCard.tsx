import { useContext, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ApiError, api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { usingPrivateOperation } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import { routePath } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import { useMaybeResource } from "../../lib/useResource";
import { useVisiblePoll } from "../../lib/useVisiblePoll";
import type { Connection, CustomMcpAuthKind, CustomMcpConnectionSummary } from "../../lib/types";
import { HireBlock, WriteGate } from "../auth/WriteGate";
import { useImageConversation } from "../canvas/imageConversation";
import { connectionNeedsAttention, hostedAccount } from "../settings/connectionsView";
import {
  customMcpAuthOptionLabel,
  customMcpAuthorizationUrl,
  customMcpErrorText,
  customMcpReturn,
  validateCustomMcpUrlInput,
} from "../settings/customMcpView";
import { submitCustomMcpConnect } from "../settings/customMcpConnect";
import { hostedConnectError, hostedConnectUrl, UI_HOSTED_REFUSED } from "../settings/hostedConnect";
import { withoutConnectParams } from "../workspace-apps/socialConnect";
import { TurnCard, type CardDensity } from "../../ui/TurnCard";
import { parseConnectRef, parseCustomMcpSetup, plainText, type CustomMcpSetup } from "./chatCards";
import { sendToMaster } from "./send";
import type { ThreadEntry } from "./thread";

export type ConnectToolkitStatus = "disconnected" | "waiting" | "connected" | "reconnect";

/**
 * CAD-2764: derive live connection status for a platform toolkit from
 * `resources.connections`, preferring the newest healthy enrolled row.
 */
export function toolkitConnectionState(
  toolkit: string,
  label: string,
  rows: readonly Connection[] | null,
): { status: "disconnected" | "connected" | "reconnect"; account: string | null } {
  if (!rows) return { status: "disconnected", account: null };
  const matching = rows.filter((row) => row.provider === toolkit && row.kind === "enrolled");
  if (matching.length === 0) return { status: "disconnected", account: null };
  const active = matching.filter((row) => !connectionNeedsAttention(row));
  if (active.length > 0) {
    let newest = active[0];
    let newestAt = hostedAccount(newest)?.connectedAt ?? 0;
    for (let i = 1; i < active.length; i += 1) {
      const candidate = active[i];
      const at = hostedAccount(candidate)?.connectedAt ?? 0;
      if (at > newestAt) {
        newest = candidate;
        newestAt = at;
      }
    }
    const hostedLabel = hostedAccount(newest)?.label ?? "";
    const trimmed = newest.account.trim();
    const account = trimmed !== "" && trimmed !== label && trimmed !== hostedLabel ? trimmed : null;
    return { status: "connected", account };
  }
  return { status: "reconnect", account: null };
}

/**
 * CAD-2443 / CAD-2556: the host's `connect_ref` card. For platform toolkits it
 * starts `/api/connection-providers/:toolkit/connect` and self-updates from
 * `resources.connections`. For Custom MCP it launches
 * `/api/connections/mcp/:id/oauth/start` (falling back to Settings > Connections
 * when credentials are manual) and offers an inline "Continue reply" resume
 * button when returning to this conversation with `?aos_connect=connected`.
 */
export function ConnectCard({ entry, density = "full" }: { entry: ThreadEntry; density?: CardDensity }) {
  const t = useTranslate();
  const writeBlock = useContext(WriteGate);
  const operatorBlock = useContext(HireBlock);
  const conversation = useImageConversation();
  const [busy, setBusy] = useState(false);
  const [continuing, setContinuing] = useState(false);
  const [resumed, setResumed] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const setup = parseCustomMcpSetup(entry);
  const model = parseConnectRef(entry);
  const isOwner = writeBlock === null && operatorBlock === null;
  const isSignedIn = writeBlock === null;
  const isPlatformOwner = !setup && model !== null && model.toolkit !== "custom_mcp" && isOwner;
  const connectionsRes = isPlatformOwner ? resources.connections : null;
  const connectionsState = useMaybeResource(connectionsRes);
  const live = isPlatformOwner
    ? toolkitConnectionState(model.toolkit, model.label, connectionsState?.data ?? null)
    : null;

  useEffect(() => {
    if (connectionsRes) void connectionsRes.revalidate();
  }, [connectionsRes]);

  useEffect(() => {
    if (live?.status === "connected" && waiting) setWaiting(false);
  }, [live?.status, waiting]);

  useVisiblePoll(
    isPlatformOwner && live?.status !== "connected",
    waiting ? 3_000 : null,
    () => {
      void resources.connections.refresh();
    },
    { onFocus: true, maxDurationMs: waiting ? 120_000 : undefined },
  );

  if (setup && isSignedIn) {
    return <CustomMcpSetupCard key={`${conversation}:${entry.seq}`} entry={entry} setup={setup} density={density} conversation={conversation} canShareWorkspace={isOwner} />;
  }
  if (!model) return <p className="text-micro text-ink-500 break-words chatcard-line">· {plainText(entry.text, 500)}</p>;
  const isCustomMcp = model.toolkit === "custom_mcp";
  // Platform toolkits require owner; personal Custom MCP reconnect cards are available to any signed-in member.
  if (!isOwner && !(isCustomMcp && isSignedIn)) {
    return <p className="text-micro text-ink-500 break-words chatcard-line" data-kind="connect_ref">· {plainText(entry.text, 500)}</p>;
  }
  const mcpReturned =
    isCustomMcp && typeof window !== "undefined" &&
      !new URLSearchParams(window.location.search).has("mcp_setup")
      ? customMcpReturn(window.location.search)
      : null;
  const readyToContinue = isCustomMcp && mcpReturned?.outcome === "connected" && !resumed;
  const platformStatus: ConnectToolkitStatus | null =
    !isCustomMcp && live
      ? live.status === "connected"
        ? "connected"
        : waiting
          ? "waiting"
          : live.status === "reconnect"
            ? "reconnect"
            : "disconnected"
      : null;
  const statusPill =
    platformStatus === "connected"
      ? { text: t("Connected"), tone: "ok" as const }
      : platformStatus === "waiting"
        ? { text: t("Waiting…"), tone: "info" as const }
        : platformStatus === "reconnect"
          ? { text: t("Reconnect needed"), tone: "warn" as const }
          : undefined;
  const label =
    isCustomMcp || platformStatus === "reconnect"
      ? t("Reconnect {label}").replace("{label}", () => model.label)
      : t("Connect {label}").replace("{label}", () => model.label);
  const bodyText =
    platformStatus === "connected"
      ? live?.account
        ? t("{label} connected · {account}")
            .replace("{label}", () => model.label)
            .replace("{account}", () => live.account!)
        : t("{label} connected").replace("{label}", () => model.label)
      : platformStatus === "waiting"
        ? t("Finish signing in to {label} in the opened tab.").replace("{label}", () => model.label)
        : platformStatus === "reconnect"
          ? t("Your {label} connection needs to be reconnected.").replace("{label}", () => model.label)
          : plainText(entry.text, 500);

  const openConnectionsFocus = () => {
    const dest = routePath({ screen: "settings", section: "connections" });
    navigate(`${dest}?mcp_focus=${encodeURIComponent(model.label)}`);
  };

  const connect = () => {
    if (isCustomMcp) {
      if (busy) return;
      setBusy(true);
      setError(null);
      api
        .customMcpList(false)
        .then(async (list) => {
          const norm = model.label.toLowerCase();
          const match = (list.connections ?? []).find(
            (row) => row.name.toLowerCase() === norm || row.id === model.label,
          );
          if (!match || (match.auth_kind !== "oauth" && match.auth_kind !== "oauth_static")) {
            openConnectionsFocus();
            return;
          }
          const returnTo =
            typeof window !== "undefined"
              ? `${window.location.origin}${window.location.pathname}`
              : undefined;
          const started = await api.customMcpOAuthStart(
            match.id,
            returnTo ? { return_to: returnTo } : {},
          );
          const safeUrl = customMcpAuthorizationUrl(started);
          if (safeUrl === null) {
            setError("Server did not return a valid https:// authorization URL.");
            return;
          }
          if (typeof window !== "undefined") window.location.assign(safeUrl);
        })
        .catch((e: unknown) => setError(customMcpErrorText(e)))
        .finally(() => setBusy(false));
      return;
    }
    if (busy) return;
    setBusy(true);
    setError(null);
    api
      .connectionProviderConnect(model.toolkit)
      .then((reply) => {
        const url = hostedConnectUrl(reply);
        if (url === null) {
          setError(UI_HOSTED_REFUSED);
          return;
        }
        window.open(url, "_blank", "noopener,noreferrer");
        setWaiting(true);
      })
      .catch((e: unknown) => setError(hostedConnectError(e)))
      .finally(() => setBusy(false));
  };

  const continueReply = async () => {
    if (continuing || !readyToContinue) return;
    setContinuing(true);
    setError(null);
    const returnUrl = window.location.href;
    try {
      const connId = mcpReturned?.connectionId;
      const note = connId
        ? `Custom MCP "${model.label}" reconnected (${connId}). Continue replying cleanly.`
        : `Custom MCP "${model.label}" reconnected. Continue replying cleanly.`;
      const conversation = resources.masterThread.get().data?.conversation;
      const sent = await sendToMaster(note, undefined, undefined, undefined, conversation);
      if (!sent.ok) {
        setError(sent.error);
        return;
      }
      if (typeof window !== "undefined" && window.location.href === returnUrl) {
        navigate(
          window.location.pathname + withoutConnectParams(window.location.search) + window.location.hash,
          { replace: true },
        );
        setResumed(true);
      }
    } finally {
      setContinuing(false);
    }
  };

  return (
    <TurnCard
      kind="connect"
      className="card chatcard"
      bodyClassName="chatcard-body"
      density={density}
      data-kind="connect_ref"
      data-connect-state={platformStatus ?? undefined}
      label={t("Connect card")}
      kindLabel={t("Connection")}
      subject={model.label}
      status={statusPill}
      footer={
        platformStatus === "connected" ? undefined : readyToContinue ? (
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              className="btn btn-primary"
              data-continue-mcp={model.label}
              disabled={continuing}
              onClick={() => void continueReply()}
            >
              {continuing ? t("Resuming…") : t("Continue reply")}
            </button>
            <button type="button" className="btn btn-sm btn-ghost" onClick={openConnectionsFocus}>
              {t("Manage in Connections")}
            </button>
          </div>
        ) : resumed ? (
          <span className="text-micro text-ok font-medium">{t("Reply resumed")}</span>
        ) : (
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" className="btn btn-primary" data-connect-toolkit={model.toolkit} disabled={busy} onClick={connect}>
              {busy ? t("Opening sign-in…") : label}
            </button>
            {isCustomMcp && (
              <button type="button" className="btn btn-sm btn-ghost" onClick={openConnectionsFocus}>
                {t("Settings")}
              </button>
            )}
          </div>
        )
      }
    >
      <p className="text-label text-ink-300 break-words">{bodyText}</p>
      {mcpReturned?.outcome === "connected" && (
        <p className="text-micro text-ok break-words" role="status">
          {resumed ? t("Reply resumed") : t("Reconnected — continue when ready.")}
        </p>
      )}
      {mcpReturned && mcpReturned.outcome !== "connected" && (
        <p className="text-micro text-warn break-words" role="status">
          {t("OAuth sign-in did not finish — try reconnecting again.")}
        </p>
      )}
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {t(error)}
        </p>
      )}
    </TurnCard>
  );
}

// Browser storage only locates the original attempt. It never establishes a grant.
type SetupAttempt = { id: string; endpoint: string; nonce: string; profile?: string };
type StandingOffer = { profile: string; endpoint_url: string; required_scopes: string[]; tool_name: string };

function resumeStandingRead(id: string, tool: string): string {
  return `Custom MCP connection ${id} is ready. If my request in this chat is waiting for a read, finish only that read using ${tool}. If I only asked to connect, confirm that it is connected without running any tools.`;
}

function parseSetupAttempt(value: string | null, endpoint: string): SetupAttempt | null {
  try {
    const [id, savedEndpoint = endpoint, nonce = "", profile] = (value ?? "").split("\n");
    return id ? { id, endpoint: new URL(savedEndpoint).href, nonce, ...(profile ? { profile } : {}) } : null;
  } catch {
    return null;
  }
}

function sameSetupAttempt(a: SetupAttempt | null, b: SetupAttempt | null): boolean {
  return a !== null && b !== null && a.id === b.id && a.endpoint === b.endpoint && a.nonce === b.nonce && a.profile === b.profile;
}

function readSetupAttempt(key: string, endpoint: string): SetupAttempt | null {
  for (const source of ["localStorage", "sessionStorage"] as const) {
    try {
      const attempt = parseSetupAttempt(window[source].getItem(key), endpoint);
      if (attempt) return attempt;
    } catch {
      // The other storage type may still be available.
    }
  }
  return null;
}

async function setupMessageId(conversation: string, seq: number, attempt: SetupAttempt): Promise<string> {
  const bytes = new TextEncoder().encode(JSON.stringify([conversation, seq, attempt.id, attempt.endpoint, attempt.nonce]));
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return `mcp-setup-${Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

function matchesSetupScopes(actual: readonly string[], expected: string | undefined): boolean {
  if (expected === undefined) return true;
  const scopes = expected.split(" ");
  return actual.length === scopes.length && scopes.every((scope) => actual.includes(scope));
}

function CustomMcpSetupConfirmModal({
  setup,
  endpoint,
  returnTo,
  editEndpoint,
  canShareWorkspace,
  onClose,
  onOAuthStarted,
  onConnected,
}: {
  setup: { label: string; endpointUrl: string; requiredScopes?: string[]; returnTo: string };
  endpoint: string;
  returnTo: string;
  editEndpoint: boolean;
  canShareWorkspace: boolean;
  onClose: () => void;
  onOAuthStarted: (connectionId: string, endpointUrl: string, nonce: string) => void;
  onConnected: (connection: CustomMcpConnectionSummary) => void;
}) {
  const t = useTranslate();
  const baseId = useId();
  const [endpointUrl, setEndpointUrl] = useState(endpoint);
  const [editingUrl, setEditingUrl] = useState(editEndpoint);
  const [visibility, setVisibility] = useState<"personal" | "shared">("personal");
  const [authKind, setAuthKind] = useState<CustomMcpAuthKind>("auto");
  const [bearerToken, setBearerToken] = useState("");
  const [customHeadersText, setCustomHeadersText] = useState("");
  const [oauthClientId, setOauthClientId] = useState("");
  const [oauthClientSecret, setOauthClientSecret] = useState("");
  const [oauthScopesText, setOauthScopesText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [crossSite, setCrossSite] = useState<{ authorizationUrl: string; host: string } | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);

  const urlError = validateCustomMcpUrlInput(endpointUrl);

  const submit = async () => {
    if (busy) return;
    setError(null);
    setBusy(true);
    try {
      const res = await submitCustomMcpConnect({
        name: setup.label,
        endpointUrl,
        urlError,
        visibility,
        authKind,
        bearerToken,
        customHeadersText,
        oauthClientId,
        oauthClientSecret,
        oauthScopesText,
        requiredScopesPreset: setup.requiredScopes,
        returnToBase: returnTo,
        requireOAuthRedirect:
          authKind === "auto" || authKind === "oauth" || authKind === "oauth_static",
      });
      if (!res.ok) {
        setError(t(res.error));
        return;
      }
      if (res.kind === "oauth_redirect") {
        onOAuthStarted(res.connectionId, res.normalizedEndpoint, res.nonce);
        if (res.sameSite) {
          if (typeof window !== "undefined") window.location.assign(res.authorizationUrl);
          return;
        }
        setCrossSite({
          authorizationUrl: res.authorizationUrl,
          host: res.discovery?.authorization_host ?? new URL(res.authorizationUrl).host,
        });
        return;
      }
      onConnected(res.connection);
    } finally {
      // Secrets exist only inside this single request; clear unconditionally.
      setBearerToken("");
      setCustomHeadersText("");
      setOauthClientSecret("");
      setBusy(false);
    }
  };

  const modal = (
    <div
      className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50 p-4"
      role="presentation"
      onClick={() => {
        if (!busy) onClose();
      }}
    >
      <div
        className="card p-5 max-w-md w-full max-h-[calc(100dvh-2rem)] overflow-y-auto space-y-3 border border-ink-700 shadow-xl min-w-0"
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${baseId}-title`}
        data-mcp-confirm-modal
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0">
            <h2 id={`${baseId}-title`} className="text-heading font-semibold text-ink-100 break-words">
              {t("Connect {label}").replace("{label}", () => setup.label)}
            </h2>
            <p className="text-micro text-ink-400 mt-0.5 break-words">
              {t("Read-only tools run automatically. Write actions will ask for your approval.")}
            </p>
          </div>
          <button
            type="button"
            className="btn btn-ghost btn-sm shrink-0"
            disabled={busy}
            aria-label={t("Close")}
            onClick={onClose}
          >
            ✕
          </button>
        </div>

        {crossSite ? (
          <div className="space-y-3" data-mcp-cross-site-confirm>
            <div className="rounded border border-accent/40 bg-accent/10 p-3 space-y-1.5 text-micro text-ink-300">
              <p className="text-label font-medium text-ink-100">{t("Confirm external sign-in host")}</p>
              <p className="break-words">
                {t("This server redirects OAuth sign-in to")} <code className="num text-ink-100">{crossSite.host}</code>.
              </p>
            </div>
            <div className="flex flex-wrap justify-end gap-2">
              <button type="button" className="btn btn-ghost btn-sm" onClick={() => setCrossSite(null)}>
                {t("Back")}
              </button>
              <a href={crossSite.authorizationUrl} className="btn btn-primary btn-sm">
                {t("Continue to OAuth sign-in →")}
              </a>
            </div>
          </div>
        ) : (
          <form
            className="space-y-3 min-w-0"
            onSubmit={(e) => {
              e.preventDefault();
              void submit();
            }}
          >
            <div className="space-y-1">
              <div className="flex items-center justify-between gap-2">
                <label htmlFor={`${baseId}-url`} className="text-label font-medium text-ink-200">
                  {t("Endpoint")}
                </label>
                <button
                  type="button"
                  className="lnk text-micro"
                  disabled={busy}
                  onClick={() => setEditingUrl((v) => !v)}
                >
                  {editingUrl ? t("Lock") : t("Edit endpoint")}
                </button>
              </div>
              <input
                id={`${baseId}-url`}
                type="url"
                required
                autoComplete="off"
                spellCheck={false}
                value={endpointUrl}
                onChange={(e) => setEndpointUrl(e.target.value)}
                readOnly={!editingUrl}
                disabled={busy}
                className="field w-full num text-micro"
              />
            </div>

            {setup.requiredScopes && setup.requiredScopes.length > 0 && (
              <p className="text-micro text-ink-300 break-words">
                {t("Required OAuth scopes")}: <code>{setup.requiredScopes.join(" ")}</code>
              </p>
            )}

            <div className="space-y-1">
              <span className="text-label font-medium text-ink-200">{t("Who can use this server")}</span>
              {canShareWorkspace ? (
                <div className="flex flex-wrap gap-2 pt-0.5" role="radiogroup" aria-label={t("Who can use this server")}>
                  <button
                    type="button"
                    role="radio"
                    aria-checked={visibility === "personal"}
                    disabled={busy}
                    className={`btn btn-sm ${visibility === "personal" ? "btn-primary" : "btn-ghost"}`}
                    onClick={() => setVisibility("personal")}
                  >
                    {t("Personal (just me)")}
                  </button>
                  <button
                    type="button"
                    role="radio"
                    aria-checked={visibility === "shared"}
                    disabled={busy}
                    className={`btn btn-sm ${visibility === "shared" ? "btn-primary" : "btn-ghost"}`}
                    onClick={() => setVisibility("shared")}
                  >
                    {t("Shared with company")}
                  </button>
                </div>
              ) : (
                <p className="text-micro text-ink-400 break-words">
                  {t("Personal (just me) — only your own assistant turns can use this server.")}
                </p>
              )}
            </div>

            <details className="text-micro text-ink-300" open={authKind !== "auto"}>
              <summary className="cursor-pointer select-none font-medium text-ink-300">
                {t("Advanced auth")}: <span className="text-ink-200">{t(customMcpAuthOptionLabel(authKind))}</span>
              </summary>
              <div className="mt-2 space-y-2">
                <div className="flex flex-wrap gap-1.5">
                  {(["auto", "oauth", "oauth_static", "bearer", "header"] as const).map((kind) => (
                    <button
                      key={kind}
                      type="button"
                      disabled={busy}
                      className={`btn btn-sm ${authKind === kind ? "btn-primary" : "btn-ghost"}`}
                      onClick={() => setAuthKind(kind)}
                    >
                      {t(customMcpAuthOptionLabel(kind))}
                    </button>
                  ))}
                </div>
                {authKind === "bearer" && (
                  <input
                    type="password"
                    autoComplete="off"
                    value={bearerToken}
                    onChange={(e) => setBearerToken(e.target.value)}
                    placeholder={t("Bearer token")}
                    className="field w-full num"
                    disabled={busy}
                  />
                )}
                {authKind === "header" && (
                  <textarea
                    rows={2}
                    value={customHeadersText}
                    onChange={(e) => setCustomHeadersText(e.target.value)}
                    placeholder="x-api-key: value"
                    className="field w-full num"
                    disabled={busy}
                  />
                )}
                {authKind === "oauth_static" && (
                  <div className="space-y-1.5">
                    <input
                      type="text"
                      autoComplete="off"
                      value={oauthClientId}
                      onChange={(e) => setOauthClientId(e.target.value)}
                      placeholder={t("OAuth client ID")}
                      className="field w-full num"
                      disabled={busy}
                    />
                    <input
                      type="password"
                      autoComplete="off"
                      value={oauthClientSecret}
                      onChange={(e) => setOauthClientSecret(e.target.value)}
                      placeholder={t("OAuth client secret (optional)")}
                      className="field w-full num"
                      disabled={busy}
                    />
                    <input
                      type="text"
                      autoComplete="off"
                      value={oauthScopesText}
                      onChange={(e) => setOauthScopesText(e.target.value)}
                      placeholder={t("Scopes (space-separated, optional)")}
                      className="field w-full num"
                      disabled={busy}
                    />
                  </div>
                )}
              </div>
            </details>

            {error && (
              <p className="text-micro text-fail break-words" role="alert">
                {error}
              </p>
            )}

            <div className="flex flex-wrap justify-end gap-2 pt-1">
              <button type="button" className="btn btn-ghost btn-sm" disabled={busy} onClick={onClose}>
                {t("Cancel")}
              </button>
              <button type="submit" className="btn btn-primary btn-sm" disabled={busy || urlError !== null}>
                {busy ? t("Connecting…") : t("Confirm & Connect")}
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );

  return typeof document !== "undefined" ? createPortal(modal, document.body) : modal;
}

function CustomMcpSetupCard({ entry, setup, density, conversation, canShareWorkspace }: {
  entry: ThreadEntry;
  setup: CustomMcpSetup;
  density: CardDensity;
  conversation: string | null;
  canShareWorkspace: boolean;
}) {
  const t = useTranslate();
  const [modal, setModal] = useState<"connect" | "edit" | null>(null);
  const [busy, setBusy] = useState(false);
  const [resumed, setResumed] = useState(false);
  const [resumedPreset, setResumedPreset] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [connection, setConnection] = useState<CustomMcpConnectionSummary | null>(null);
  const [recheck, setRecheck] = useState(0);
  const [offer, setOffer] = useState<StandingOffer | null | undefined>(undefined);
  const [existingCatalog, setExistingCatalog] = useState<CustomMcpConnectionSummary | null>(null);
  const [authorizationUrl, setAuthorizationUrl] = useState<string | null>(null);
  const autoResumeAttempt = useRef<string | null>(null);
  const url = new URL(window.location.href);
  const originalHref = url.href;
  const callback = url.searchParams.get("mcp_setup") === String(entry.seq) ? customMcpReturn(url.search) : null;
  const key = `cadence:mcp-setup:${conversation}:${entry.seq}`;
  const attempt = conversation ? readSetupAttempt(key, setup.endpointUrl) : null;
  const connId = attempt?.id;
  const endpoint = attempt?.endpoint ?? setup.endpointUrl;
  const presetAttempt = !!attempt?.profile;
  const outcome = connId && callback?.connectionId === connId &&
    url.searchParams.get("mcp_attempt") === (attempt?.nonce || null) ? callback.outcome : null;
  const requiredScopes = setup.requiredScopes?.join(" ");
  url.search = withoutConnectParams(url.search);
  url.searchParams.delete("mcp_attempt");
  url.searchParams.set("mcp_setup", String(entry.seq));
  const returnTo = url.href;

  function findOffer(list: { standing_read_offers?: StandingOffer[] }, profile?: string): StandingOffer | undefined {
    return list.standing_read_offers?.find((item) => {
      try {
        return !!item.profile && !!item.tool_name && (!profile || item.profile === profile) &&
          new URL(item.endpoint_url).href === new URL(setup.endpointUrl).href &&
          matchesSetupScopes(item.required_scopes, requiredScopes);
      } catch { return false; }
    });
  }

  function eligible(row: CustomMcpConnectionSummary, selected: StandingOffer): boolean {
    try {
      return row.connected_by_you === true && new URL(row.endpoint_url).href === new URL(selected.endpoint_url).href &&
        (row.auth_kind === "oauth" || row.auth_kind === "oauth_static") &&
        matchesSetupScopes(row.oauth_scopes, selected.required_scopes.join(" ")) &&
        (row.standing_read_access === "confirm" || row.standing_read_access === "allowed") &&
        (row.standing_read_access === "confirm" ? !row.standing_read_profile || row.standing_read_profile === selected.profile
          : row.standing_read_profile === selected.profile);
    } catch { return false; }
  }

  useEffect(() => {
    if (outcome === "connected") return;
    const controller = new AbortController();
    void usingPrivateOperation(undefined, (operation) => api.customMcpList(true, operation), controller.signal)
      .then((list) => {
        if (controller.signal.aborted) return;
        const selected = findOffer(list, attempt?.profile);
        setOffer(selected ?? null);
        const rows = selected ? list.connections.filter((row) => eligible(row, selected)) : [];
        setExistingCatalog(rows.find((row) => row.standing_read_access === "allowed") ?? rows[0] ?? null);
      }, () => { if (!controller.signal.aborted) setOffer(null); });
    return () => controller.abort();
  }, [setup.endpointUrl, requiredScopes, attempt?.profile, outcome]);

  function verified(row: CustomMcpConnectionSummary | undefined): row is CustomMcpConnectionSummary {
    if (!row) return false;
    try {
      return row.id === connId && row.connected_by_you === true &&
        row.name === setup.label && new URL(row.endpoint_url).href === endpoint &&
        (row.auth_kind === "oauth" || row.auth_kind === "oauth_static") &&
        matchesSetupScopes(row.oauth_scopes, requiredScopes);
    } catch { return false; }
  }

  useEffect(() => {
    if (outcome !== "connected" || resumed) return;
    const controller = new AbortController();
    setConnection(null);
    setError(null);
    void usingPrivateOperation(undefined, async (operation) => {
      const list = await api.customMcpList(true, operation);
      const selected = presetAttempt ? findOffer(list, attempt?.profile) : undefined;
      setOffer(selected ?? null);
      if (presetAttempt && (!selected || !attempt?.nonce)) return undefined;
      return list.connections.find((row) => verified(row) && (selected
        ? eligible(row, selected) && row.standing_read_profile === selected.profile
        : !row.standing_read_profile));
    }, controller.signal).then((row) => {
      if (controller.signal.aborted) return;
      if (row) setConnection(row);
      else setError("Cadence could not verify this Custom MCP connection.");
    }, (e: unknown) => {
      if (!controller.signal.aborted) setError(customMcpErrorText(e));
    });
    return () => controller.abort();
  }, [connId, endpoint, outcome, requiredScopes, setup.label, resumed, recheck, presetAttempt]);

  function remember(id: string, endpointUrl: string, nonce: string, profile?: string) {
    const value = `${id}\n${new URL(endpointUrl).href}\n${nonce}${profile ? `\n${profile}` : ""}`;
    // A same-origin tab can recover an attempt; restricted browsers still support same-tab return.
    try {
      window.localStorage.setItem(key, value);
    } catch {
      window.sessionStorage.setItem(key, value);
    }
  }

  function liveCallback(expected: SetupAttempt): boolean {
    const live = new URL(window.location.href);
    const returned = customMcpReturn(live.search);
    return live.pathname === url.pathname &&
      live.searchParams.get("conversation") === url.searchParams.get("conversation") &&
      live.searchParams.get("ctx") === url.searchParams.get("ctx") &&
      live.searchParams.get("mcp_setup") === String(entry.seq) &&
      live.searchParams.get("mcp_attempt") === (expected.nonce || null) &&
      returned?.connectionId === expected.id && returned.outcome === callback?.outcome &&
      sameSetupAttempt(readSetupAttempt(key, setup.endpointUrl), expected);
  }

  function forget(expected: SetupAttempt) {
    if (!sameSetupAttempt(readSetupAttempt(key, setup.endpointUrl), expected)) return;
    const cleanRoute = liveCallback(expected);
    for (const source of ["localStorage", "sessionStorage"] as const) {
      try {
        if (sameSetupAttempt(parseSetupAttempt(window[source].getItem(key), setup.endpointUrl), expected)) {
          window[source].removeItem(key);
        }
      } catch {
        // An unavailable storage type held no usable receipt.
      }
    }
    if (cleanRoute) {
      const live = new URL(window.location.href);
      for (const param of ["aos_connect", "toolkit", "connection_id", "mcp_setup", "mcp_attempt"]) {
        live.searchParams.delete(param);
      }
      navigate(`${live.pathname}${live.search}${live.hash}`, { replace: true });
    }
  }

  async function run(work: () => Promise<void>) {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (e: unknown) {
      setError(customMcpErrorText(e));
    } finally {
      setBusy(false);
    }
  }

  function retryOAuth() {
    if (!connId || !attempt) return;
    void run(async () => {
      let retryScopes = requiredScopes;
      if (presetAttempt) {
        const list = await api.customMcpList();
        const selected = findOffer(list, attempt.profile);
        const row = list.connections.find(verified);
        if (!selected || !row || !eligible(row, selected)) throw new Error("Cadence could not verify this Custom MCP connection.");
        retryScopes = selected.required_scopes.join(" ");
      }
      try {
        const nonce = crypto.randomUUID();
        const retryReturnTo = new URL(returnTo);
        retryReturnTo.searchParams.set("mcp_attempt", nonce);
        const started = await api.customMcpOAuthStart(connId, { return_to: retryReturnTo.href });
        const authUrl = customMcpAuthorizationUrl(started);
        if (started.connection_id !== connId || authUrl === null) {
          setError("Server did not return a valid https:// authorization URL.");
          return;
        }
        if (retryScopes !== undefined) {
          const actual = new URL(authUrl).searchParams.getAll("scope");
          if (actual.length !== 1 || !matchesSetupScopes(actual[0].split(" "), retryScopes)) {
            setError("The provider did not request exactly the required OAuth scopes.");
            return;
          }
        }
        if (window.location.href !== originalHref ||
            !sameSetupAttempt(readSetupAttempt(key, setup.endpointUrl), attempt)) return;
        remember(connId, endpoint, nonce, attempt.profile);
        window.location.assign(authUrl);
      } catch (e: unknown) {
        if (e instanceof ApiError && e.status === 404) forget(attempt);
        throw e;
      }
    });
  }

  function continueReply(directConnectionId?: string) {
    const targetConnId = directConnectionId ?? connId;
    if (!conversation || !targetConnId) return;
    if (!directConnectionId && !attempt) return;
    void run(() => usingPrivateOperation(undefined, async (operation) => {
      const list = await api.customMcpList(true, operation);
      const selected = !directConnectionId && presetAttempt ? findOffer(list, attempt!.profile) : undefined;
      const row = list.connections.find((item) =>
        directConnectionId ? item.id === directConnectionId && item.connected_by_you === true : verified(item),
      );
      if (row?.status !== "active" || (!directConnectionId && (!liveCallback(attempt!) ||
          (presetAttempt ? (!selected || !attempt!.nonce || !eligible(row, selected) ||
            row.standing_read_profile !== selected.profile || row.standing_read_access !== "allowed")
            : !!row.standing_read_profile)))) {
        setConnection(!directConnectionId && presetAttempt ? null : row ?? null);
        setError("Cadence could not verify this Custom MCP connection.");
        return;
      }
      const messageAttempt = directConnectionId
        ? { id: directConnectionId, endpoint: new URL(row.endpoint_url).href, nonce: "direct-connect" }
        : attempt!;
      const message = await setupMessageId(conversation, entry.seq, messageAttempt);
      if (!directConnectionId && !liveCallback(attempt!)) return;
      const sent = await sendToMaster(
        !directConnectionId && presetAttempt
          ? resumeStandingRead(targetConnId, selected!.tool_name)
          : `Custom MCP connection ${targetConnId} is connected. Continue my original request using only currently permitted tools. If a tool is unavailable, explain what access is needed.`,
        message, undefined, undefined, conversation, operation,
      );
      if (!sent.ok) {
        setError(sent.error);
        return;
      }
      if (!directConnectionId && attempt) {
        const current = liveCallback(attempt);
        if (current && presetAttempt) setResumedPreset(true);
        forget(attempt);
        if (current) setResumed(true);
      } else {
        setResumed(true);
      }
    }));
  }

  useEffect(() => {
    if (!attempt?.nonce || !connection || connection.status !== "active" ||
        resumed || autoResumeAttempt.current === attempt.nonce) return;
    if (presetAttempt) {
      if (!offer || connection.standing_read_access !== "allowed") return;
      autoResumeAttempt.current = attempt.nonce;
      continueReply();
      return;
    }
    const readable = (connection.tools ?? []).some((pin) => pin.mode === "read" && !pin.drifted);
    if (!readable) return;
    autoResumeAttempt.current = attempt.nonce;
    continueReply();
  }, [presetAttempt, attempt?.nonce, offer, connection, resumed]);

  function connectStandingRead() {
    if (!conversation || !offer) return;
    void run(async () => {
      const list = await api.customMcpList();
      const selected = findOffer(list, offer.profile);
      if (!selected || selected.tool_name !== offer.tool_name ||
          !matchesSetupScopes(selected.required_scopes, offer.required_scopes.join(" "))) {
        setOffer(null);
        throw new Error("Read access is no longer available. Please try again.");
      }
      const alreadyConnected = list.connections.find((row) => eligible(row, selected));
      if (alreadyConnected) {
        setExistingCatalog(alreadyConnected);
        return;
      }
      const nonce = crypto.randomUUID();
      const target = new URL(returnTo);
      target.searchParams.set("mcp_attempt", nonce);
      const reply = await api.customMcpCreate({
        name: setup.label, endpoint_url: selected.endpoint_url, auth_kind: "oauth", visibility: "personal",
        required_scopes: selected.required_scopes, standing_read_profile: selected.profile, return_to: target.href,
      });
      if (reply.mode !== "oauth_redirect") throw new Error("This server did not start OAuth sign-in.");
      const safeUrl = customMcpAuthorizationUrl(reply);
      const scopes = safeUrl ? new URL(safeUrl).searchParams.getAll("scope") : [];
      if (!safeUrl || scopes.length !== 1 || !matchesSetupScopes(scopes[0].split(" "), selected.required_scopes.join(" "))) {
        throw new Error("The provider did not request exactly the required OAuth scopes.");
      }
      if (window.location.href !== originalHref) return;
      remember(reply.connection_id, selected.endpoint_url, nonce, selected.profile);
      setAuthorizationUrl(safeUrl);
    });
  }

  function allowExistingCatalog() {
    const chosen = existingCatalog;
    if (!conversation || !offer || chosen?.standing_read_access !== "confirm") return;
    void run(() => usingPrivateOperation(undefined, async (operation) => {
      const list = await api.customMcpList(true, operation);
      const selected = findOffer(list, offer.profile);
      const row = list.connections.find((item) => item.id === chosen.id);
      if (!selected || selected.tool_name !== offer.tool_name || !row || !eligible(row, selected) ||
          row.standing_read_access !== "confirm" || row.snapshot_revision !== chosen.snapshot_revision) {
        throw new Error("Cadence could not verify this Custom MCP connection.");
      }
      const { connection: allowed } = await api.customMcpAllowCatalogReads(row.id, row.snapshot_revision, operation);
      if (window.location.href !== originalHref) return;
      if (allowed.id !== row.id || !eligible(allowed, selected) ||
          allowed.standing_read_profile !== selected.profile || allowed.standing_read_access !== "allowed") {
        throw new Error("Cadence could not verify this Custom MCP connection.");
      }
      setExistingCatalog(allowed);
      const message = await setupMessageId(conversation, entry.seq, {
        id: allowed.id, endpoint: new URL(selected.endpoint_url).href, nonce: "standing-consent",
      });
      if (window.location.href !== originalHref) return;
      const sent = await sendToMaster(
        resumeStandingRead(allowed.id, selected.tool_name), message, undefined, undefined, conversation, operation,
      );
      if (!sent.ok) setError(sent.error);
    }));
  }

  const current = outcome === "connected" && connection?.id === connId ? connection : null;
  const active = current?.status === "active";
  const tools = current?.tools ?? [];
  const canRead = tools.some((pin) => pin.mode === "read" && !pin.drifted);
  const settings = `${routePath({ screen: "settings", section: "connections" })}?mcp_focus=${encodeURIComponent(connId ?? existingCatalog?.id ?? setup.label)}`;
  let status: string | null = null;
  let actions;
  if (resumed) {
    status = resumedPreset ? "Connected · Read access" : null;
    actions = resumedPreset
      ? <a className="btn btn-sm btn-ghost" href={settings}>{t("Manage in Connections")}</a>
      : <span className="text-micro text-ok font-medium">{t("Reply resumed")}</span>;
  } else if (active && presetAttempt) {
    status = current.standing_read_access === "allowed"
      ? "Connected · Read access" : "Read access is not ready. Check this connection.";
    actions = current.standing_read_access !== "allowed"
      ? <a className="btn btn-sm btn-ghost" href={settings}>{t("Manage in Connections")}</a>
      : error ? <button type="button" className="btn btn-primary" disabled={busy} onClick={() => continueReply()}>{t("Resume")}</button> : null;
  } else if (active) {
    status = canRead
      ? (busy ? "Connected. Resuming chat…" : "Connected.")
      : "Connected. No tools can run automatically.";
    actions = (!canRead || error)
      ? <button type="button" className="btn btn-primary" disabled={busy} onClick={() => continueReply()}>{t("Continue in chat")}</button>
      : <a className="btn btn-sm btn-ghost" href={settings}>{t("Manage in Connections")}</a>;
  } else if (authorizationUrl) {
    actions = <a href={authorizationUrl} className="btn btn-primary">{t("Continue to OAuth sign-in →")}</a>;
  } else if (connId) {
    if (outcome === "connected" && !current && !error) status = "Checking connection…";
    else if (current?.status === "needs_review") status = "The server changed its tools. Review the changes in Connections.";
    else if (current?.status === "auth_expired") status = "OAuth sign-in expired. Sign in again.";
    else if (outcome === "cancelled") status = "OAuth sign-in was cancelled.";
    else if (callback && outcome === null) status = "This sign-in does not match this setup.";
    else if (outcome === "failed") status = "OAuth sign-in did not finish. Retry or edit the server endpoint.";
    else if (outcome === null) status = "Waiting for OAuth sign-in.";
    actions = (
      <div className="flex flex-wrap gap-2">
        {outcome === "connected" && !current ? (
          <button type="button" className="btn btn-secondary" disabled={!error || busy} onClick={() => setRecheck((value) => value + 1)}>{t("Check connection")}</button>
        ) : current?.status !== "needs_review" && (!presetAttempt || offer) ? (
          <button type="button" className="btn btn-primary" disabled={busy} onClick={retryOAuth}>{t("Retry OAuth sign-in")}</button>
        ) : null}
        {!presetAttempt && <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => setModal("edit")}>{t("Edit endpoint")}</button>}
      </div>
    );
  } else if (offer === undefined) {
    status = "Checking connection…";
  } else if (offer && existingCatalog) {
    const canAllow = existingCatalog.standing_read_access === "confirm";
    status = existingCatalog.standing_read_access === "allowed" ? "Connected · Read access"
      : existingCatalog.status === "auth_expired" ? "OAuth sign-in expired. Sign in again."
      : canAllow ? "Use your existing sign-in and make this connection personal. Atlas can read until you disconnect."
      : "Read access is not ready. Check this connection.";
    actions = canAllow
      ? <button type="button" className="btn btn-primary" disabled={!conversation || busy} onClick={allowExistingCatalog}>{t("Allow Atlas read access")}</button>
      : <a className="btn btn-sm btn-ghost" href={settings}>{t("Manage in Connections")}</a>;
  } else if (offer) {
    actions = <button type="button" className="btn btn-primary" disabled={!conversation || busy} onClick={connectStandingRead}>{t("Connect {label} for read access").replace("{label}", () => setup.label)}</button>;
  } else {
    actions = (
      <div className="flex flex-wrap gap-2">
        <button type="button" className="btn btn-primary" disabled={!conversation} onClick={() => setModal("connect")}>{t("Connect {label}").replace("{label}", () => setup.label)}</button>
        <button type="button" className="btn btn-ghost" disabled={!conversation} onClick={() => setModal("edit")}>{t("Edit endpoint")}</button>
      </div>
    );
  }

  return (
    <>
      <TurnCard kind="connect" className="card chatcard" bodyClassName="chatcard-body" density={density}
        data-kind="connect_ref" label={t("Connect card")} kindLabel={t("Connection")} subject={setup.label}
        footer={actions}
      >
        <p className="text-label text-ink-300 break-words">{plainText(entry.text, 500)}</p>
        {offer && !connId && <p className="text-micro text-ink-300">{t("Allow Atlas to read in your personal chats until you disconnect. Other agents and workspace members cannot use this access.")}</p>}
        {status && <p className={`text-micro ${active ? "text-ok" : "text-warn"}`} role="status">{t(status)}</p>}
        {current?.status === "needs_review" && <a className="btn btn-sm btn-ghost" href={settings}>{t("Manage in Connections")}</a>}
        {error && <p className="text-label text-fail break-words" role="alert">{t(error)}</p>}
      </TurnCard>
      {modal && (
        <CustomMcpSetupConfirmModal
          setup={{ ...setup, endpointUrl: endpoint, returnTo }}
          endpoint={endpoint}
          returnTo={returnTo}
          editEndpoint={modal === "edit"}
          canShareWorkspace={canShareWorkspace}
          onClose={() => setModal(null)}
          onConnected={(conn) => {
            setModal(null);
            continueReply(conn.id);
          }}
          onOAuthStarted={remember}
        />
      )}
    </>
  );
}
