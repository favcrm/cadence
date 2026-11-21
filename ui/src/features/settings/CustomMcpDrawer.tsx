import { useEffect, useId, useState } from "react";
import { api } from "../../lib/api";
import { routinesApi } from "../agents/routinesApi";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import type {
  CustomMcpAuthKind,
  CustomMcpConnectionSummary,
  CustomMcpOAuthDiscovery,
  CustomMcpToolMode,
} from "../../lib/types";
import Button from "../../ui/Button";
import { IconCheck, IconLock, IconRefresh, IconWarning } from "../../ui/icons";
import DrawerShell, { type DrawerTab } from "../app-shell/shared/DrawerShell";
import { visibilityBadge } from "./connectionsView";
import {
  customMcpAuthLabel,
  customMcpAuthOptionLabel,
  customMcpAuthorizationUrl,
  customMcpErrorText,
  customMcpStatusView,
  customMcpSyncedText,
  customMcpToolModeLabel,
  isToolDestructiveHint,
  isToolReadOnlyHint,
  parseCustomHeadersInput,
  schemaParamSummary,
  validateCustomMcpUrlInput,
} from "./customMcpView";
import SettingsDrawer from "./SettingsDrawer";

export type CustomMcpDrawerTab = "tools" | "guide" | "policies" | "auth";

function statusChipClass(tone: "ok" | "warn" | "fail" | "muted"): string {
  switch (tone) {
    case "ok": return "custmcp-chip-ok";
    case "warn": return "custmcp-chip-warn";
    case "fail": return "custmcp-chip-fail";
    case "muted": return "bg-ink-800 text-ink-400";
  }
}

function MetaRow({ label, value, mono = true }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex flex-wrap gap-x-2">
      <dt className="text-ink-500">{label}</dt>
      <dd className={mono ? "num text-ink-200 break-all" : "text-ink-200"}>{value}</dd>
    </div>
  );
}

function PolicyRadio({ checked, disabled, title, sub, onClick }: {
  checked: boolean; disabled: boolean; title: string; sub: string; onClick: () => void;
}) {
  return (
    <button type="button" role="radio" aria-checked={checked} disabled={disabled} className="conn-policy-opt" onClick={onClick}>
      <b>{title}</b><small>{sub}</small>
    </button>
  );
}

/**
 * Connect a new remote Streamable HTTP Custom MCP server.
 * Auto MCP OAuth 2.1 + PKCE (RFC 9728 / DCR) is the default; Manual Auth
 * (Bearer, Headers, Pre-registered OAuth client, No auth) sits in `<details>`.
 */
export function CustomMcpConnectDrawer({
  onClose,
  onConnected,
  onOAuthStarted,
}: {
  onClose: () => void;
  onConnected: (connection: CustomMcpConnectionSummary) => void;
  onOAuthStarted: (connectionId: string) => void;
}) {
  const t = useTranslate();
  const baseId = useId();
  const [name, setName] = useState("");
  const [endpointUrl, setEndpointUrl] = useState("");
  const [urlTouched, setUrlTouched] = useState(false);
  const [visibility, setVisibility] = useState<"shared" | "personal">("shared");
  const [authKind, setAuthKind] = useState<CustomMcpAuthKind>("auto");
  const [bearerToken, setBearerToken] = useState("");
  const [customHeadersText, setCustomHeadersText] = useState("");
  const [oauthClientId, setOauthClientId] = useState("");
  const [oauthClientSecret, setOauthClientSecret] = useState("");
  const [oauthScopesText, setOauthScopesText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [discoveryStep, setDiscoveryStep] = useState<{
    authorizationUrl: string;
    connectionId: string;
    discovery?: CustomMcpOAuthDiscovery;
  } | null>(null);

  const urlError = validateCustomMcpUrlInput(endpointUrl);

  const submit = async () => {
    if (busy) return;
    setUrlTouched(true);
    setError(null);
    const trimmedName = name.trim();
    if (!trimmedName) {
      setError("Enter a name for this Custom MCP server.");
      return;
    }
    if (urlError !== null) {
      setError(urlError);
      return;
    }

    let parsedHeaders: Array<{ name: string; value: string }> | undefined;
    if (authKind === "header") {
      const checked = parseCustomHeadersInput(customHeadersText);
      if (checked.error !== null) {
        setError(checked.error);
        return;
      }
      parsedHeaders = checked.headers;
    }
    if (authKind === "bearer" && !bearerToken.trim()) {
      setError("Enter the Bearer token or API key.");
      return;
    }
    if (authKind === "oauth_static" && !oauthClientId.trim()) {
      setError("Enter the OAuth client_id.");
      return;
    }

    const scopes = oauthScopesText
      .split(/[\s,]+/)
      .map((s) => s.trim())
      .filter(Boolean);
    const returnTo =
      typeof window !== "undefined"
        ? `${window.location.origin}/settings/connections`
        : undefined;

    setBusy(true);
    try {
      const reply = await api.customMcpCreate({
        name: trimmedName,
        endpoint_url: endpointUrl.trim(),
        auth_kind: authKind,
        visibility,
        ...(authKind === "bearer" ? { bearer_token: bearerToken.trim() } : {}),
        ...(authKind === "header" && parsedHeaders ? { custom_headers: parsedHeaders } : {}),
        ...(authKind === "oauth_static"
          ? {
              oauth_client: {
                client_id: oauthClientId.trim(),
                ...(oauthClientSecret.trim() ? { client_secret: oauthClientSecret.trim() } : {}),
                ...(scopes.length > 0 ? { scopes } : {}),
              },
            }
          : {}),
        ...(returnTo ? { return_to: returnTo } : {}),
      });
      if (reply.mode === "oauth_redirect") {
        const safeUrl = customMcpAuthorizationUrl(reply);
        if (safeUrl === null) {
          setError("Server did not return a valid https:// authorization URL.");
          return;
        }
        onOAuthStarted(reply.connection_id);
        setDiscoveryStep({
          authorizationUrl: safeUrl,
          connectionId: reply.connection_id,
          ...(reply.discovery ? { discovery: reply.discovery } : {}),
        });
        return;
      }
      onConnected(reply.connection);
    } catch (e: unknown) {
      setError(customMcpErrorText(e));
    } finally {
      // Secrets exist only inside this single request; clear unconditionally.
      setBearerToken("");
      setCustomHeadersText("");
      setOauthClientSecret("");
      setBusy(false);
    }
  };

  const previewHost =
    discoveryStep?.discovery?.authorization_host ??
    (discoveryStep ? new URL(discoveryStep.authorizationUrl).host : "");

  return (
    <SettingsDrawer title={t("Connect custom MCP server")} onClose={onClose}>
      <div className="custmcp-steps" aria-label={t("Setup steps")}>
        <span className="custmcp-step" data-active={!discoveryStep ? "true" : undefined}>{t("1. Endpoint")}</span>
        <span className="custmcp-step" data-active={discoveryStep ? "true" : undefined}>{t("2. Discovery & Consent")}</span>
        <span className="custmcp-step">{t("3. Pinned Tools")}</span>
      </div>
      <p className="text-label text-ink-400 break-words">
        {t("Connect a remote Streamable HTTP (https://) MCP server. Auto OAuth 2.1 + PKCE discovers metadata automatically; tools stay pinned and start in safe Approval or Off mode.")}
      </p>
      {discoveryStep ? (
        <div className="rounded border border-accent/40 bg-accent/10 p-3 space-y-2.5" role="status" data-mcp-discovery-preview>
          <p className="text-label font-medium text-ink-100">{t("OAuth discovery verified")}</p>
          <dl className="text-micro space-y-1 text-ink-300">
            <MetaRow label={t("Authorization host")} value={previewHost} />
            {discoveryStep.discovery?.issuer && <MetaRow label={t("OAuth issuer")} value={discoveryStep.discovery.issuer} />}
            <MetaRow label={t("Client registration")} mono={false} value={discoveryStep.discovery?.registration === "static" ? t("Static pre-registered OAuth client") : t("Dynamic Client Registration (RFC 7591)")} />
            {discoveryStep.discovery?.resource_metadata_url && <MetaRow label={t("Resource metadata")} value={discoveryStep.discovery.resource_metadata_url} />}
            {discoveryStep.discovery && discoveryStep.discovery.scopes.length > 0 && <MetaRow label={t("Scopes")} value={discoveryStep.discovery.scopes.join(" ")} />}
          </dl>
          <p className="text-micro text-ink-300 break-words">
            {t("Approve access at the provider. You will return to Settings → Connections to review pinned tools.")}
          </p>
          <div className="flex flex-wrap gap-2">
            <a href={discoveryStep.authorizationUrl} className="btn btn-primary btn-sm">{t("Continue to OAuth sign-in →")}</a>
            <Button size="sm" variant="ghost" onClick={() => setDiscoveryStep(null)}>{t("Back")}</Button>
            <Button size="sm" variant="ghost" onClick={onClose}>{t("Close")}</Button>
          </div>
        </div>
      ) : (
        <form className="space-y-3 min-w-0" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
          <div>
            <label htmlFor={`${baseId}-name`} className="text-label font-medium text-ink-200">{t("Server name")}</label>
            <input id={`${baseId}-name`} type="text" maxLength={80} required autoComplete="off" value={name} onChange={(e) => setName(e.target.value)} placeholder="Acme Billing MCP" className="field w-full mt-1" disabled={busy} />
          </div>
          <div>
            <label htmlFor={`${baseId}-url`} className="text-label font-medium text-ink-200">{t("MCP endpoint URL")}</label>
            <input
              id={`${baseId}-url`} type="url" required autoComplete="off" spellCheck={false} value={endpointUrl}
              onChange={(e) => { setEndpointUrl(e.target.value); if (!urlTouched) setUrlTouched(true); }}
              placeholder="https://mcp.example.com/v1/mcp" className="field w-full mt-1 num" disabled={busy}
            />
            {urlTouched && endpointUrl.trim() !== "" && urlError !== null && (
              <p className="text-micro text-fail mt-1 break-words" role="alert">{t(urlError)}</p>
            )}
          </div>
          <div className="conn-policy-group">
            <span className="slabel">{t("Who can use this server")}</span>
            <div className="conn-policy-grid" role="radiogroup" aria-label={t("Who can use this server")}>
              <PolicyRadio checked={visibility === "shared"} disabled={busy} title={t("Shared with company")} sub={t("All workspace members and approved agents can use enabled tools.")} onClick={() => setVisibility("shared")} />
              <PolicyRadio checked={visibility === "personal"} disabled={busy} title={t("Just me (Personal)")} sub={t("Only your own turns can access this server.")} onClick={() => setVisibility("personal")} />
            </div>
          </div>
          <details className="custmcp-manual" open={authKind !== "auto"}>
            <summary className="text-label font-medium text-ink-300 cursor-pointer select-none">
              {t("Authentication mode")}: <span className="text-ink-200">{t(customMcpAuthOptionLabel(authKind))}</span>
            </summary>
            <div className="mt-2.5 space-y-2.5">
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-1.5" role="radiogroup" aria-label={t("Authentication mode")}>
                {(
                  [
                    { id: "auto", label: "Auto MCP OAuth 2.1", hint: "RFC 9728 discovery + DCR" },
                    { id: "bearer", label: "Bearer token", hint: "Static API key or PAT" },
                    { id: "header", label: "Custom headers", hint: "X-API-Key / Tenant headers" },
                    { id: "oauth_static", label: "OAuth client ID", hint: "Pre-registered client + PKCE" },
                    { id: "none", label: "No auth", hint: "Public read-only HTTPS server" },
                  ] as const
                ).map((opt) => (
                  <button key={opt.id} type="button" role="radio" aria-checked={authKind === opt.id} disabled={busy} className="conn-policy-opt" onClick={() => setAuthKind(opt.id)}>
                    <b>{t(opt.label)}</b>
                    <small>{t(opt.hint)}</small>
                  </button>
                ))}
              </div>
              {authKind === "bearer" && (
                <div>
                  <label htmlFor={`${baseId}-bearer`} className="text-label text-ink-300">{t("Bearer token")}</label>
                  <input id={`${baseId}-bearer`} type="password" autoComplete="off" spellCheck={false} value={bearerToken} onChange={(e) => setBearerToken(e.target.value)} placeholder="sk_live_..." className="field w-full mt-1 num" disabled={busy} />
                </div>
              )}
              {authKind === "header" && (
                <div>
                  <label htmlFor={`${baseId}-headers`} className="text-label text-ink-300">{t("Custom HTTP headers (one Header: Value per line)")}</label>
                  <textarea id={`${baseId}-headers`} rows={3} autoComplete="off" spellCheck={false} value={customHeadersText} onChange={(e) => setCustomHeadersText(e.target.value)} placeholder={"X-API-Key: ...\nX-Tenant-Id: acme"} className="field w-full mt-1 num" disabled={busy} />
                </div>
              )}
              {authKind === "oauth_static" && (
                <div className="space-y-2">
                  <div>
                    <label htmlFor={`${baseId}-cid`} className="text-label text-ink-300">{t("OAuth Client ID")}</label>
                    <input id={`${baseId}-cid`} type="text" autoComplete="off" spellCheck={false} value={oauthClientId} onChange={(e) => setOauthClientId(e.target.value)} className="field w-full mt-1 num" disabled={busy} />
                  </div>
                  <div>
                    <label htmlFor={`${baseId}-csec`} className="text-label text-ink-300">{t("OAuth Client Secret (optional)")}</label>
                    <input id={`${baseId}-csec`} type="password" autoComplete="off" spellCheck={false} value={oauthClientSecret} onChange={(e) => setOauthClientSecret(e.target.value)} className="field w-full mt-1 num" disabled={busy} />
                  </div>
                  <div>
                    <label htmlFor={`${baseId}-scopes`} className="text-label text-ink-300">{t("Scopes (optional, space-separated)")}</label>
                    <input id={`${baseId}-scopes`} type="text" autoComplete="off" spellCheck={false} value={oauthScopesText} onChange={(e) => setOauthScopesText(e.target.value)} placeholder="mcp:read mcp:write" className="field w-full mt-1 num" disabled={busy} />
                  </div>
                </div>
              )}
              {(authKind === "bearer" || authKind === "header" || authKind === "oauth_static") && (
                <p className="text-micro text-ink-500 break-words">
                  {t("Credentials are encrypted at rest inside Cadence custody and cleared from this browser immediately after saving.")}
                </p>
              )}
            </div>
          </details>
          {error && <p className="text-label text-fail break-words" role="alert">{t(error)}</p>}
          <div className="conn-actions flex flex-wrap gap-2">
            <Button variant="primary" type="submit" loading={busy}>
              {busy ? t("Connecting…") : authKind === "auto" || authKind === "oauth" || authKind === "oauth_static" ? t("Connect via OAuth →") : t("Connect & discover tools")}
            </Button>
            <Button variant="ghost" onClick={onClose} disabled={busy}>{t("Cancel")}</Button>
          </div>
        </form>
      )}
    </SettingsDrawer>
  );
}

/**
 * Inspect and manage one connected Custom MCP server inside DrawerShell.
 * Non-owners see read-only tools, instructions, guide, and auth metadata.
 */
export function CustomMcpInspectDrawer({
  connection,
  canWrite,
  initialTab = "tools",
  onClose,
  onUpdated,
  onRevoked,
}: {
  connection: CustomMcpConnectionSummary;
  canWrite: boolean;
  initialTab?: CustomMcpDrawerTab;
  onClose: () => void;
  onUpdated: (next: CustomMcpConnectionSummary) => void;
  onRevoked: (id: string) => void;
}) {
  const t = useTranslate();
  const canEdit = canWrite;
  const [tab, setTab] = useState<CustomMcpDrawerTab>(initialTab);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<{ ok: boolean; text: string } | null>(null);

  // Tool modes draft keyed by tool_name.
  const [modeDraft, setModeDraft] = useState<Record<string, CustomMcpToolMode>>(() =>
    Object.fromEntries((connection.tools ?? []).map((pin) => [pin.tool_name, pin.mode])),
  );

  // Agent guide draft & import URL.
  const [guideDraft, setGuideDraft] = useState(connection.agent_guide_md ?? "");
  const [guideSource, setGuideSource] = useState<string | null>(connection.agent_guide_source);
  const [importUrl, setImportUrl] = useState(connection.agent_guide_source ?? "");

  // Auth rotation & disconnect states.
  const [rotateKind, setRotateKind] = useState<"bearer" | "header" | "none">("bearer");
  const [rotateToken, setRotateToken] = useState("");
  const [rotateHeadersText, setRotateHeadersText] = useState("");
  const [revokeOpen, setRevokeOpen] = useState(false);
  const [revokeArmed, setRevokeArmed] = useState(false);

  // Routine & agent policy draft (for Policies tab).
  const agentsState = useMaybeResource(canEdit ? resources.agents : null);
  useEffect(() => {
    if (canEdit && tab === "policies") {
      void resources.agents.revalidate();
    }
  }, [canEdit, tab]);
  const [policyLoaded, setPolicyLoaded] = useState(!canEdit);
  const [baseVis, setBaseVis] = useState<"shared" | "personal">(connection.visibility);
  const [baseWriteMode, setBaseWriteMode] = useState<"ask_approval" | "read_only">("ask_approval");
  const [baseExt, setBaseExt] = useState(false);
  const [baseAgents, setBaseAgents] = useState<string[]>([]);
  const [visDraft, setVisDraft] = useState<"shared" | "personal">(connection.visibility);
  const [writeModeDraft, setWriteModeDraft] = useState<"ask_approval" | "read_only">("ask_approval");
  const [extDraft, setExtDraft] = useState(false);
  const [agentsDraft, setAgentsDraft] = useState<string[]>([]);

  useEffect(() => {
    setVisDraft(connection.visibility);
    setBaseVis(connection.visibility);
  }, [connection.id, connection.visibility]);

  useEffect(() => {
    if (!canEdit) {
      setPolicyLoaded(true);
      return;
    }
    let alive = true;
    setPolicyLoaded(false);
    routinesApi
      .connectionPolicy(connection.id)
      .then((res) => {
        if (!alive) return;
        const p = res.policy;
        const vis = p.visibility === "personal" ? "personal" : "shared";
        const wm = p.write_mode === "read_only" ? "read_only" : "ask_approval";
        const ext = Boolean(p.allow_external_triggers);
        const ag = Array.isArray(p.allowed_agents) ? p.allowed_agents : [];
        setBaseVis(vis);
        setVisDraft(vis);
        setBaseWriteMode(wm);
        setWriteModeDraft(wm);
        setBaseExt(ext);
        setExtDraft(ext);
        setBaseAgents(ag);
        setAgentsDraft(ag);
        setPolicyLoaded(true);
      })
      .catch(() => {
        if (alive) setPolicyLoaded(true);
      });
    return () => {
      alive = false;
    };
  }, [canEdit, connection.id]);

  useEffect(() => {
    setModeDraft(
      Object.fromEntries((connection.tools ?? []).map((pin) => [pin.tool_name, pin.mode])),
    );
  }, [connection.id, connection.snapshot_revision, connection.pending_revision, connection.tools]);

  useEffect(() => {
    setGuideDraft(connection.agent_guide_md ?? "");
    setGuideSource(connection.agent_guide_source);
    if (connection.agent_guide_source) setImportUrl(connection.agent_guide_source);
  }, [connection.id, connection.agent_guide_md, connection.agent_guide_source]);

  useEffect(() => {
    setTab(initialTab);
  }, [initialTab, connection.id]);

  const statusView = customMcpStatusView(connection);
  const tools = connection.tools ?? [];
  const changedTools = tools
    .filter((pin) => modeDraft[pin.tool_name] && modeDraft[pin.tool_name] !== pin.mode)
    .map((pin) => ({ tool_name: pin.tool_name, mode: modeDraft[pin.tool_name]! }));
  const toolsDirty = changedTools.length > 0;
  const guideDirty = guideDraft !== (connection.agent_guide_md ?? "");
  const isOAuth = connection.auth_kind === "oauth" || connection.auth_kind === "oauth_static";
  const hasDrift =
    connection.status === "needs_review" ||
    connection.tools_summary.drifted > 0 ||
    connection.instructions_drifted;

  const returnTo =
    typeof window !== "undefined" ? `${window.location.origin}/settings/connections` : undefined;

  const runAction = async (key: string, fn: () => Promise<void>) => {
    if (!canEdit || busyAction) return;
    setBusyAction(key);
    setFeedback(null);
    try {
      await fn();
    } catch (e: unknown) {
      setFeedback({ ok: false, text: customMcpErrorText(e) });
    } finally {
      setBusyAction(null);
    }
  };

  const syncTools = () => runAction("sync", async () => {
    const res = await api.customMcpSync(connection.id);
    onUpdated(res.connection);
    setFeedback({ ok: true, text: t("Tool snapshot synced from remote MCP server.") });
  });

  const saveToolModes = () => {
    if (changedTools.length === 0) return;
    return runAction("tools", async () => {
      const res = await api.customMcpUpdateTools(connection.id, changedTools);
      onUpdated(res.connection);
      setFeedback({ ok: true, text: t("Tool permissions saved.") });
    });
  };

  const acceptDrift = () => runAction("drift", async () => {
    const res = await api.customMcpAcceptDrift(connection.id, changedTools.length > 0 ? changedTools : undefined);
    onUpdated(res.connection);
    setFeedback({ ok: true, text: t("Pending changes accepted into a new pinned revision.") });
  });

  const reconnectOAuth = () => runAction("oauth", async () => {
    const res = await api.customMcpOAuthStart(connection.id, returnTo ? { return_to: returnTo } : {});
    const safeUrl = customMcpAuthorizationUrl(res);
    if (safeUrl === null) {
      setFeedback({ ok: false, text: t("Server did not return a valid https:// authorization URL.") });
      return;
    }
    if (typeof window !== "undefined") window.location.assign(safeUrl);
  });

  const importGuide = () => {
    const urlErr = validateCustomMcpUrlInput(importUrl);
    if (urlErr !== null) {
      if (canEdit && !busyAction) setFeedback({ ok: false, text: t(urlErr) });
      return;
    }
    return runAction("import-guide", async () => {
      const res = await api.customMcpImportGuide(connection.id, importUrl.trim());
      setGuideDraft(res.markdown);
      setGuideSource(res.source_url);
      setFeedback({ ok: true, text: t("Imported Markdown draft for review — click Save agent guide to apply.") });
    });
  };

  const saveGuide = () => runAction("save-guide", async () => {
    const res = await api.customMcpUpdateGuide(connection.id, { markdown: guideDraft, source: guideSource });
    onUpdated(res.connection);
    setFeedback({ ok: true, text: t("Workspace Agent Guide saved.") });
  });

  const savePolicy = () => runAction("policy", async () => {
    const effectiveExt = visDraft === "shared" ? extDraft : false;
    const res = await routinesApi.connectionPolicySet(connection.id, {
      visibility: visDraft,
      write_mode: writeModeDraft,
      allow_external_triggers: effectiveExt,
      allowed_agents: agentsDraft,
    });
    const nextVis = res.policy.visibility === "personal" ? "personal" : "shared";
    const nextWm = res.policy.write_mode === "read_only" ? "read_only" : "ask_approval";
    const nextExt = Boolean(res.policy.allow_external_triggers);
    const nextAg = Array.isArray(res.policy.allowed_agents) ? res.policy.allowed_agents : [];
    setBaseVis(nextVis); setVisDraft(nextVis);
    setBaseWriteMode(nextWm); setWriteModeDraft(nextWm);
    setBaseExt(nextExt); setExtDraft(nextExt);
    setBaseAgents(nextAg); setAgentsDraft(nextAg);
    onUpdated({ ...connection, visibility: nextVis });
    setFeedback({ ok: true, text: t("Access settings saved.") });
  });

  const toggleAllowedAgent = (alias: string) => {
    setAgentsDraft((prev) => (prev.includes(alias) ? prev.filter((item) => item !== alias) : [...prev, alias]));
  };

  const submitRotate = async () => {
    if (!canEdit || busyAction) return;
    let customHeaders: Array<{ name: string; value: string }> | undefined;
    if (rotateKind === "bearer" && !rotateToken.trim()) {
      setFeedback({ ok: false, text: t("Enter the new Bearer token.") });
      return;
    }
    if (rotateKind === "header") {
      const checked = parseCustomHeadersInput(rotateHeadersText);
      if (checked.error !== null) {
        setFeedback({ ok: false, text: t(checked.error) });
        return;
      }
      customHeaders = checked.headers;
    }
    setBusyAction("rotate");
    setFeedback(null);
    try {
      const res = await api.customMcpRotateAuth(connection.id, {
        auth_kind: rotateKind,
        ...(rotateKind === "bearer" ? { bearer_token: rotateToken.trim() } : {}),
        ...(rotateKind === "header" && customHeaders ? { custom_headers: customHeaders } : {}),
      });
      onUpdated(res.connection);
      setFeedback({ ok: true, text: t("Credentials updated.") });
    } catch (e: unknown) {
      setFeedback({ ok: false, text: customMcpErrorText(e) });
    } finally {
      setRotateToken("");
      setRotateHeadersText("");
      setBusyAction(null);
    }
  };

  const submitRevoke = () => {
    if (!canEdit || !revokeArmed || busyAction) return;
    setBusyAction("revoke");
    setFeedback(null);
    void api.customMcpRevoke(connection.id)
      .then(() => onRevoked(connection.id))
      .catch((e: unknown) => {
        setFeedback({ ok: false, text: customMcpErrorText(e) });
        setBusyAction(null);
      });
  };

  const okBanner = feedback?.ok ? <div className="custmcp-ok-banner" role="status">{feedback.text}</div> : null;

  const toolsPanel = (
    <div className="space-y-3 min-w-0">
      {okBanner}
      <div className="flex flex-wrap items-center justify-between gap-2 rounded border border-ink-700 bg-ink-850 px-3 py-2">
        <dl className="text-micro text-ink-300 space-y-0.5 min-w-0">
          <MetaRow label={t("Endpoint")} value={connection.endpoint_url} />
          <MetaRow label={t("Snapshot")} value={`v${connection.snapshot_revision}${connection.server_name ? ` · ${connection.server_name}` : ""}${connection.server_version ? ` (${connection.server_version})` : ""}`} />
          <MetaRow label={t("Last synced")} value={customMcpSyncedText(connection.last_synced_at)} />
        </dl>
        {canEdit && (
          <Button size="sm" variant="secondary" loading={busyAction === "sync"} disabled={busyAction !== null} icon={<IconRefresh size={13} />} onClick={() => void syncTools()}>
            {busyAction === "sync" ? t("Syncing…") : t("Sync tools")}
          </Button>
        )}
      </div>
      {connection.status === "auth_expired" && (
        <div className="rounded border border-fail/40 bg-fail/10 px-3 py-2 space-y-1.5" role="alert">
          <p className="text-label font-medium text-fail">{t("Authorization expired")}</p>
          <p className="text-micro text-ink-200 break-words">
            {t("The remote server rejected the stored credentials. Reconnect or rotate credentials before running tools.")}
          </p>
          {canEdit && (
            <div className="flex flex-wrap gap-2 pt-1">
              {isOAuth ? (
                <Button size="sm" variant="primary" loading={busyAction === "oauth"} onClick={() => void reconnectOAuth()}>{t("Reconnect via OAuth")}</Button>
              ) : (
                <Button size="sm" variant="primary" onClick={() => setTab("auth")}>{t("Update credentials")}</Button>
              )}
            </div>
          )}
        </div>
      )}
      {hasDrift && (
        <div className="rounded border border-warn/40 bg-warn/10 px-3 py-2 space-y-1.5" role="status">
          <div className="flex items-center gap-1.5 text-label font-medium text-warn">
            <IconWarning size={14} /><span>{t("Server contract drift detected")}</span>
          </div>
          <p className="text-micro text-ink-200 break-words">
            {t("Changed tools and server instructions are held back until an owner reviews them. Previously auto-read tools that changed have failed closed to Ask approval.")}
          </p>
          {canEdit && (
            <Button size="sm" variant="primary" loading={busyAction === "drift"} disabled={busyAction !== null} onClick={() => void acceptDrift()}>
              {busyAction === "drift" ? t("Accepting…") : t("Accept drift & pin revision")}
            </Button>
          )}
        </div>
      )}
      {tools.length === 0 ? (
        <p className="text-label text-ink-400">{t("No tools are currently pinned for this server. Run Sync tools after sign-in finishes.")}</p>
      ) : (
        <ul className="custmcp-tools" aria-label={t("Pinned MCP tools")}>
          {tools.map((pin) => {
            const currentMode = modeDraft[pin.tool_name] ?? pin.mode;
            const params = schemaParamSummary(pin.input_schema);
            return (
              <li key={pin.tool_name} className="custmcp-tool" data-drifted={pin.drifted ? "true" : undefined}>
                <div className="flex flex-wrap items-start justify-between gap-2 min-w-0">
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5 min-w-0">
                      <code className="num text-label font-medium text-ink-100 break-all">{pin.tool_name}</code>
                      {isToolReadOnlyHint(pin) && <span className="chip bg-ink-800 text-ink-300">{t("Read-only hint")}</span>}
                      {isToolDestructiveHint(pin) && <span className="chip custmcp-chip-warn">{t("Destructive hint")}</span>}
                      {pin.drifted && <span className="chip custmcp-chip-warn">{t("Drifted")}</span>}
                    </div>
                    {pin.description && <p className="text-micro text-ink-400 mt-1 break-words">{pin.description}</p>}
                    {pin.drifted && pin.pending_description !== null && (
                      <p className="text-micro text-warn mt-1 break-words">{t("Pending description")}: {pin.pending_description}</p>
                    )}
                    {params.length > 0 && <p className="text-micro text-ink-500 mt-1 num break-words">{t("Args")}: {params.join(", ")}</p>}
                  </div>
                  {canEdit ? (
                    <div className="custmcp-mode-group" role="radiogroup" aria-label={`${pin.tool_name} ${t("mode")}`}>
                      {(["off", "approval", "read"] as const).map((modeOpt) => (
                        <button
                          key={modeOpt} type="button" role="radio" aria-checked={currentMode === modeOpt}
                          disabled={busyAction !== null} className="custmcp-mode-btn"
                          onClick={() => setModeDraft((cur) => ({ ...cur, [pin.tool_name]: modeOpt }))}
                        >
                          {t(customMcpToolModeLabel(modeOpt))}
                        </button>
                      ))}
                    </div>
                  ) : (
                    <span className="chip bg-ink-800 text-ink-300 shrink-0">{t(customMcpToolModeLabel(pin.mode))}</span>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
      {canEdit && toolsDirty && (
        <div className="flex flex-wrap items-center gap-2 pt-2 border-t border-ink-700">
          <Button size="sm" variant="primary" loading={busyAction === "tools"} disabled={busyAction !== null} onClick={() => void saveToolModes()}>
            {busyAction === "tools" ? t("Saving…") : t("Save tool modes")}
          </Button>
          <Button size="sm" variant="ghost" disabled={busyAction !== null} onClick={() => setModeDraft(Object.fromEntries(tools.map((pin) => [pin.tool_name, pin.mode])))}>
            {t("Reset")}
          </Button>
        </div>
      )}
    </div>
  );

  const guidePanel = (
    <div className="space-y-4 min-w-0">
      {okBanner}
      <section className="space-y-1.5 min-w-0">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h4 className="slabel">{t("Pinned server instructions (initialize.instructions)")}</h4>
          <span className="chip bg-ink-800 text-ink-400">{t("Untrusted data")}</span>
        </div>
        <p className="text-micro text-ink-500 break-words">
          {t("Captured automatically when syncing tools and passed to agents on-demand inside an untrusted data wrapper.")}
        </p>
        {connection.server_instructions?.trim() ? (
          <pre className="custmcp-pre" tabIndex={0}>{connection.server_instructions}</pre>
        ) : (
          <p className="text-label text-ink-400">{t("This server did not report initialize.instructions.")}</p>
        )}
        {connection.instructions_drifted && (
          <div className="rounded border border-warn/40 bg-warn/10 p-2.5 space-y-1">
            <p className="text-micro font-medium text-warn">{t("Pending upstream instructions (not yet pinned)")}</p>
            <pre className="custmcp-pre" tabIndex={0}>{connection.pending_server_instructions ?? t("(cleared upstream)")}</pre>
          </div>
        )}
      </section>
      <section className="space-y-2 border-t border-ink-700 pt-3 min-w-0">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h4 className="slabel">{t("Workspace Agent Guide (Owner-reviewed Markdown)")}</h4>
          {guideSource && <span className="text-micro text-ink-500 num break-all">{guideSource}</span>}
        </div>
        <p className="text-micro text-ink-400 break-words">
          {t("Optional owner-approved usage rules (workflow order, field defaults, or imported llms.txt notes) returned alongside find_tools({ id }).")}
        </p>
        {canEdit ? (
          <>
            <div className="flex flex-wrap gap-2">
              <input
                type="url" value={importUrl} onChange={(e) => setImportUrl(e.target.value)}
                placeholder="https://docs.example.com/llms.txt" aria-label={t("Import Markdown from URL")}
                className="field flex-1 min-w-[12rem] num" disabled={busyAction !== null}
              />
              <Button size="sm" variant="secondary" loading={busyAction === "import-guide"} disabled={busyAction !== null || importUrl.trim() === ""} onClick={() => void importGuide()}>
                {busyAction === "import-guide" ? t("Importing…") : t("Import llms.txt / Docs")}
              </Button>
            </div>
            <textarea
              rows={8} maxLength={32768} value={guideDraft} onChange={(e) => setGuideDraft(e.target.value)}
              placeholder="# Usage notes for agents&#10;- Call search_customers before create_invoice..."
              aria-label={t("Workspace Agent Guide Markdown")} className="field w-full num" disabled={busyAction !== null}
            />
            <div className="flex flex-wrap items-center justify-between gap-2">
              <span className="text-micro text-ink-500 num">{guideDraft.length} / 32768</span>
              <div className="flex flex-wrap gap-2">
                {guideDraft !== "" && (
                  <Button size="sm" variant="ghost" disabled={busyAction !== null} onClick={() => { setGuideDraft(""); setGuideSource(null); }}>
                    {t("Clear draft")}
                  </Button>
                )}
                <Button size="sm" variant="primary" loading={busyAction === "save-guide"} disabled={busyAction !== null || !guideDirty} onClick={() => void saveGuide()}>
                  {busyAction === "save-guide" ? t("Saving…") : t("Save agent guide")}
                </Button>
              </div>
            </div>
          </>
        ) : connection.agent_guide_md?.trim() ? (
          <pre className="custmcp-pre" tabIndex={0}>{connection.agent_guide_md}</pre>
        ) : (
          <p className="text-label text-ink-400">{t("No workspace agent guide saved.")}</p>
        )}
      </section>
    </div>
  );

  const rosterAgents = agentsState?.data?.agents ?? [];
  const effectiveExt = visDraft === "shared" ? extDraft : false;
  const policyDirty =
    visDraft !== baseVis || writeModeDraft !== baseWriteMode || effectiveExt !== baseExt ||
    [...agentsDraft].sort().join(",") !== [...baseAgents].sort().join(",");

  const policiesPanel = (
    <div className="space-y-4 min-w-0">
      {okBanner}
      {!canEdit ? (
        <div className="conn-policy" data-mcp-policy-readonly>
          <div className="conn-policy-group">
            <span className="slabel">{t("Who can use this server")}</span>
            <p className="text-label text-ink-200">{t(connection.visibility === "shared" ? "Shared with company" : "Personal")}</p>
            <p className="text-micro text-ink-400 break-words">{t("Everyone in the workspace and allowed agents can use enabled read tools.")}</p>
          </div>
          <div className="conn-policy-group">
            <span className="slabel">{t("Action safety")}</span>
            <p className="text-micro text-ink-400 break-words">{t("Only workspace owners can approve writes or change routine and agent policies.")}</p>
          </div>
        </div>
      ) : (
        <div className="conn-policy" data-connection-policy>
          {!policyLoaded && <p className="text-micro text-ink-500">{t("Loading access policies…")}</p>}
          <div className="conn-policy-group">
            <span className="slabel">{t("Who can use this server")}</span>
            <div className="conn-policy-grid" role="radiogroup" aria-label={t("Who can use this server")}>
              <PolicyRadio checked={visDraft === "shared"} disabled={busyAction !== null} title={t("Shared with workspace")} sub={t("Team members and assigned routines can use this connection.")} onClick={() => setVisDraft("shared")} />
              <PolicyRadio checked={visDraft === "personal"} disabled={busyAction !== null} title={t("Just me (Personal)")} sub={t("Only your turns and routines pinned to this account can use it.")} onClick={() => setVisDraft("personal")} />
            </div>
          </div>
          {rosterAgents.length > 0 && (
            <div className="conn-policy-group">
              <span className="slabel">{t("Agent access")}</span>
              <p className="text-micro text-ink-400 break-words">
                {visDraft === "shared"
                  ? t("All workspace agents may call this shared MCP server. Restrict access by switching to Personal or configuring agent permissions below.")
                  : t("Only your turns can use this server. Specific agents can be restricted below.")}
              </p>
              <div className="space-y-1">
                {rosterAgents.map((agentRow) => {
                  const checked = agentsDraft.includes(agentRow.alias);
                  return (
                    <button key={agentRow.alias} type="button" role="checkbox" aria-checked={checked} disabled={busyAction !== null} className="conn-policy-toggle" onClick={() => toggleAllowedAgent(agentRow.alias)}>
                      <span className="conn-policy-box" aria-hidden>{checked ? "✓" : ""}</span>
                      <span className="min-w-0 break-words num">{agentRow.alias}</span>
                    </button>
                  );
                })}
              </div>
            </div>
          )}
          <div className="conn-policy-group">
            <span className="slabel">{t("Action safety")}</span>
            <div className="conn-policy-grid" role="radiogroup" aria-label={t("Action safety")}>
              <PolicyRadio checked={writeModeDraft === "ask_approval"} disabled={busyAction !== null} title={t("Ask for approval first")} sub={t("Drafts and external writes wait for approval.")} onClick={() => setWriteModeDraft("ask_approval")} />
              <PolicyRadio checked={writeModeDraft === "read_only"} disabled={busyAction !== null} title={t("Read only")} sub={t("Agents can read data; external writes are blocked.")} onClick={() => setWriteModeDraft("read_only")} />
            </div>
          </div>
          {visDraft === "shared" && (
            <div className="conn-policy-group">
              <span className="slabel">{t("Incoming triggers")}</span>
              <button type="button" role="checkbox" aria-checked={extDraft} disabled={busyAction !== null} className="conn-policy-toggle" onClick={() => setExtDraft((v) => !v)}>
                <span className="conn-policy-box" aria-hidden>{extDraft ? "✓" : ""}</span>
                <span className="min-w-0 break-words">{t("Allow incoming emails or webhooks to trigger actions")}</span>
              </button>
            </div>
          )}
          {policyDirty && (
            <div>
              <Button size="sm" variant="primary" loading={busyAction === "policy"} disabled={busyAction !== null} onClick={() => void savePolicy()}>
                {busyAction === "policy" ? t("Saving…") : t("Save access settings")}
              </Button>
            </div>
          )}
        </div>
      )}
    </div>
  );

  const authPanel = (
    <div className="space-y-4 min-w-0">
      {okBanner}
      <div className="space-y-1.5">
        <span className="slabel">{t("Authentication")}</span>
        <dl className="text-label space-y-1">
          <MetaRow label={t("Mode")} value={t(customMcpAuthLabel(connection.auth_kind))} mono={false} />
          {connection.auth_hint && <MetaRow label={t("Credential / headers")} value={connection.auth_hint} />}
          {connection.oauth_issuer && <MetaRow label={t("OAuth issuer")} value={connection.oauth_issuer} />}
          {connection.oauth_scopes.length > 0 && <MetaRow label={t("Scopes")} value={connection.oauth_scopes.join(" ")} />}
        </dl>
      </div>
      {canEdit && (
        <div className="border-t border-ink-700 pt-3 space-y-2.5">
          {isOAuth ? (
            <div className="space-y-2">
              <p className="text-label text-ink-300 break-words">{t("Re-authorize with the remote MCP server via OAuth 2.1 + PKCE.")}</p>
              <Button size="sm" variant="primary" loading={busyAction === "oauth"} disabled={busyAction !== null} onClick={() => void reconnectOAuth()}>
                {t("Reconnect via OAuth")}
              </Button>
            </div>
          ) : (
            <form className="space-y-2.5" onSubmit={(e) => { e.preventDefault(); void submitRotate(); }}>
              <span className="slabel">{t("Rotate credentials")}</span>
              <div className="flex flex-wrap gap-1.5" role="radiogroup" aria-label={t("Credential type")}>
                {(["bearer", "header", "none"] as const).map((kindOpt) => (
                  <button key={kindOpt} type="button" role="radio" aria-checked={rotateKind === kindOpt} disabled={busyAction !== null} className="conn-policy-opt flex-1" onClick={() => setRotateKind(kindOpt)}>
                    <b>{kindOpt === "bearer" ? t("Bearer token") : kindOpt === "header" ? t("Custom headers") : t("No auth")}</b>
                  </button>
                ))}
              </div>
              {rotateKind === "bearer" && (
                <input
                  type="password" autoComplete="off" spellCheck={false} value={rotateToken}
                  onChange={(e) => setRotateToken(e.target.value)} placeholder={t("New Bearer token")}
                  aria-label={t("New Bearer token")} className="field w-full num" disabled={busyAction !== null}
                />
              )}
              {rotateKind === "header" && (
                <textarea
                  rows={3} autoComplete="off" spellCheck={false} value={rotateHeadersText}
                  onChange={(e) => setRotateHeadersText(e.target.value)} placeholder={"X-API-Key: ..."}
                  aria-label={t("Replacement HTTP headers")} className="field w-full num" disabled={busyAction !== null}
                />
              )}
              <Button size="sm" variant="secondary" type="submit" loading={busyAction === "rotate"} disabled={busyAction !== null}>
                {busyAction === "rotate" ? t("Updating…") : t("Update credentials")}
              </Button>
            </form>
          )}
        </div>
      )}
      {canEdit && (
        <div className="border-t border-ink-700 pt-3 space-y-2">
          {!revokeOpen ? (
            <Button size="sm" variant="danger" disabled={busyAction !== null} onClick={() => setRevokeOpen(true)}>{t("Disconnect server…")}</Button>
          ) : (
            <div className="space-y-2">
              <p className="text-label text-ink-200 break-words">
                {t("Disconnecting removes stored credentials and pinned tools for this Custom MCP server immediately.")}
              </p>
              <label className="flex items-start gap-2 text-label text-ink-300">
                <input type="checkbox" checked={revokeArmed} onChange={(e) => setRevokeArmed(e.target.checked)} disabled={busyAction !== null} className="mt-0.5" />
                <span>{t("I understand disconnecting disables all tools from this server.")}</span>
              </label>
              <div className="flex flex-wrap gap-2">
                <Button size="sm" variant="danger" loading={busyAction === "revoke"} disabled={!revokeArmed || busyAction !== null} onClick={() => void submitRevoke()}>
                  {busyAction === "revoke" ? t("Disconnecting…") : t("Disconnect")}
                </Button>
                <Button size="sm" variant="ghost" disabled={busyAction !== null} onClick={() => { setRevokeOpen(false); setRevokeArmed(false); }}>
                  {t("Cancel")}
                </Button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );

  const tabs: DrawerTab[] = [
    { id: "tools", label: `${t("Pinned Tools")} (${tools.length})`, panel: toolsPanel },
    { id: "guide", label: t("Instructions & Guide"), panel: guidePanel },
    { id: "policies", label: t("Policies"), panel: policiesPanel },
    { id: "auth", label: t("Auth & Access"), panel: authPanel },
  ];

  const warningText = feedback && !feedback.ok ? feedback.text : undefined;

  return (
    <DrawerShell
      kind="custom-mcp" label={connection.name} kicker={t("Custom MCP server")} title={connection.name} subtitle={connection.endpoint_host}
      pills={
        <>
          <span className={`chip ${statusChipClass(statusView.tone)}`}>{statusView.tone === "ok" && <IconCheck size={11} />}{t(statusView.label)}</span>
          <span className="chip bg-ink-800 text-ink-300"><IconLock size={11} />{t(customMcpAuthLabel(connection.auth_kind))}</span>
          <span className="chip bg-ink-800 text-ink-300">{t(visibilityBadge({ visibility: connection.visibility }))}</span>
        </>
      }
      warning={warningText} tabs={tabs} tab={tab}
      onTab={(nextId) => { if (nextId === "tools" || nextId === "guide" || nextId === "policies" || nextId === "auth") setTab(nextId); }}
      note={canEdit ? undefined : t("Read-only view — only workspace owners can edit or disconnect Custom MCP servers.")}
      onClose={onClose}
    />
  );
}
