import { useContext, useEffect, useRef, useState } from "react";
import { ApiError, api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { usingPrivateOperation } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import { routePath } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import type { CustomMcpConnectionSummary, CustomMcpToolPin } from "../../lib/types";
import { HireBlock, WriteGate } from "../auth/WriteGate";
import { useImageConversation } from "../canvas/imageConversation";
import { CustomMcpConnectDrawer } from "../settings/CustomMcpDrawer";
import { customMcpAuthorizationUrl, customMcpErrorText, customMcpReturn, isToolDestructiveHint, isToolReadOnlyHint } from "../settings/customMcpView";
import { hostedConnectError, hostedConnectUrl, UI_HOSTED_REFUSED } from "../settings/hostedConnect";
import { withoutConnectParams } from "../workspace-apps/socialConnect";
import { TurnCard, type CardDensity } from "../../ui/TurnCard";
import { parseConnectRef, parseCustomMcpSetup, plainText, type CustomMcpSetup } from "./chatCards";
import { sendToMaster } from "./send";
import type { ThreadEntry } from "./thread";

/**
 * CAD-2443 / CAD-2556: the host's `connect_ref` card. For platform toolkits it
 * starts `/api/connection-providers/:toolkit/connect`. For Custom MCP it launches
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
  const [error, setError] = useState<string | null>(null);
  const setup = parseCustomMcpSetup(entry);
  if (setup && writeBlock === null && operatorBlock === null) {
    return <CustomMcpSetupCard key={`${conversation}:${entry.seq}`} entry={entry} setup={setup} density={density} conversation={conversation} />;
  }
  const model = parseConnectRef(entry);
  if (!model) return <p className="text-micro text-ink-500 break-words chatcard-line">· {plainText(entry.text, 500)}</p>;
  // Owner only: a member or an unconfirmed session reads the text line.
  if (writeBlock !== null || operatorBlock !== null) {
    return <p className="text-micro text-ink-500 break-words chatcard-line" data-kind="connect_ref">· {plainText(entry.text, 500)}</p>;
  }
  const isCustomMcp = model.toolkit === "custom_mcp";
  const mcpReturned =
    isCustomMcp && typeof window !== "undefined" &&
      !new URLSearchParams(window.location.search).has("mcp_setup")
      ? customMcpReturn(window.location.search)
      : null;
  const readyToContinue = isCustomMcp && mcpReturned?.outcome === "connected" && !resumed;
  const label = isCustomMcp
    ? t("Reconnect {label}").replace("{label}", () => model.label)
    : t("Connect {label}").replace("{label}", () => model.label);

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
      label={t("Connect card")}
      kindLabel={t("Connection")}
      subject={model.label}
      footer={
        readyToContinue ? (
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
      <p className="text-label text-ink-300 break-words">{plainText(entry.text, 500)}</p>
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
type SetupAttempt = { id: string; endpoint: string; nonce: string; profile?: "emw_catalog_v1" };
const EMW_ENDPOINT = "https://emw-mcp.simplebuild.site/";

function parseSetupAttempt(value: string | null, endpoint: string): SetupAttempt | null {
  try {
    const [id, savedEndpoint = endpoint, nonce = "", profile] = (value ?? "").split("\n");
    return id ? { id, endpoint: new URL(savedEndpoint).href, nonce, ...(profile === "emw_catalog_v1" ? { profile } : {}) } : null;
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

function reviewableRead(pin: CustomMcpToolPin): boolean {
  return pin.mode === "approval" && !pin.drifted &&
    !["call_tool", "execute_tool", "run_tool", "use_tool"].includes(pin.tool_name.toLowerCase()) &&
    isToolReadOnlyHint(pin) && !isToolDestructiveHint(pin);
}

function CustomMcpSetupCard({ entry, setup, density, conversation }: {
  entry: ThreadEntry;
  setup: CustomMcpSetup;
  density: CardDensity;
  conversation: string | null;
}) {
  const t = useTranslate();
  const [drawer, setDrawer] = useState<"connect" | "edit" | null>(null);
  const [busy, setBusy] = useState(false);
  const [resumed, setResumed] = useState(false);
  const [resumedPreset, setResumedPreset] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [connection, setConnection] = useState<CustomMcpConnectionSummary | null>(null);
  const [recheck, setRecheck] = useState(0);
  const [profileAvailable, setProfileAvailable] = useState(false);
  const [authorizationUrl, setAuthorizationUrl] = useState<string | null>(null);
  const autoResumeAttempt = useRef<string | null>(null);
  const url = new URL(window.location.href);
  const originalHref = url.href;
  const callback = url.searchParams.get("mcp_setup") === String(entry.seq) ? customMcpReturn(url.search) : null;
  const key = `cadence:mcp-setup:${conversation}:${entry.seq}`;
  const attempt = conversation ? readSetupAttempt(key, setup.endpointUrl) : null;
  const connId = attempt?.id;
  const endpoint = attempt?.endpoint ?? setup.endpointUrl;
  const isEmw = new URL(setup.endpointUrl).href === EMW_ENDPOINT;
  const presetAttempt = attempt?.profile === "emw_catalog_v1";
  const outcome = connId && callback?.connectionId === connId &&
    url.searchParams.get("mcp_attempt") === (attempt?.nonce || null) ? callback.outcome : null;
  const requiredScopes = presetAttempt ? "catalog:read" : setup.requiredScopes?.join(" ");
  url.search = withoutConnectParams(url.search);
  url.searchParams.delete("mcp_attempt");
  url.searchParams.set("mcp_setup", String(entry.seq));
  const returnTo = url.href;

  useEffect(() => {
    if (!isEmw) return;
    const controller = new AbortController();
    void usingPrivateOperation(undefined, (operation) => api.customMcpList(true, operation), controller.signal)
      .then((list) => { if (!controller.signal.aborted) setProfileAvailable(list.standing_read_profile === "emw_catalog_v1"); },
        () => { if (!controller.signal.aborted) setProfileAvailable(false); });
    return () => controller.abort();
  }, [isEmw]);

  function verified(row: CustomMcpConnectionSummary | undefined): row is CustomMcpConnectionSummary {
    return row !== undefined && row.id === connId && row.connected_by_you === true &&
      row.name === setup.label && new URL(row.endpoint_url).href === endpoint &&
      (row.auth_kind === "oauth" || row.auth_kind === "oauth_static") &&
      matchesSetupScopes(row.oauth_scopes, requiredScopes);
  }

  useEffect(() => {
    if (outcome !== "connected" || resumed) return;
    const controller = new AbortController();
    setConnection(null);
    setError(null);
    void usingPrivateOperation(undefined, async (operation) => {
      const list = await api.customMcpList(true, operation);
      if (presetAttempt && list.standing_read_profile !== "emw_catalog_v1") return undefined;
      return list.connections.find((row) => verified(row) && (presetAttempt
        ? endpoint === EMW_ENDPOINT && !!attempt?.nonce && row.standing_read_profile === "emw_catalog_v1" && row.standing_read_access === "allowed"
        : row.standing_read_profile !== "emw_catalog_v1"));
    }, controller.signal).then((row) => {
      if (controller.signal.aborted) return;
      if (row) setConnection(row);
      else setError("Cadence could not verify this Custom MCP connection.");
    }, (e: unknown) => {
      if (!controller.signal.aborted) setError(customMcpErrorText(e));
    });
    return () => controller.abort();
  }, [connId, endpoint, outcome, requiredScopes, setup.label, resumed, recheck, presetAttempt]);

  function remember(id: string, endpointUrl: string, nonce: string, profile?: "emw_catalog_v1") {
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
    if (!connId || !attempt || (presetAttempt && !profileAvailable)) return;
    void run(async () => {
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
        if (requiredScopes !== undefined) {
          const actual = new URL(authUrl).searchParams.getAll("scope");
          if (actual.length !== 1 || !matchesSetupScopes(actual[0].split(" "), requiredScopes)) {
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

  function continueReply() {
    if (!conversation || !connId || !attempt) return;
    void run(() => usingPrivateOperation(undefined, async (operation) => {
      const list = await api.customMcpList(true, operation);
      const row = list.connections.find(verified);
      if (row?.status !== "active" || !liveCallback(attempt) ||
          (presetAttempt ? (endpoint !== EMW_ENDPOINT || !attempt.nonce || list.standing_read_profile !== "emw_catalog_v1" ||
            row.standing_read_profile !== "emw_catalog_v1" || row.standing_read_access !== "allowed")
            : row.standing_read_profile === "emw_catalog_v1")) {
        setConnection(presetAttempt ? null : row ?? null);
        setError("Cadence could not verify this Custom MCP connection.");
        return;
      }
      const message = await setupMessageId(conversation, entry.seq, attempt);
      if (!liveCallback(attempt)) return;
      const sent = await sendToMaster(
        presetAttempt
          ? `EMW catalog connection ${connId} is ready. If my request in this chat is waiting for an EMW catalog read, finish only that read using catalog.search. If I only asked to connect, confirm that it is connected without running any tools.`
          : `Custom MCP connection ${connId} is connected. Continue my original request using only currently permitted tools. If a tool is unavailable, explain what access is needed.`,
        message, undefined, undefined, conversation, operation,
      );
      if (!sent.ok) {
        setError(sent.error);
        return;
      }
      const current = liveCallback(attempt);
      if (current && presetAttempt) setResumedPreset(true);
      forget(attempt);
      if (current) setResumed(true);
    }));
  }

  useEffect(() => {
    if (!presetAttempt || !attempt?.nonce || !profileAvailable || !connection ||
        connection.status !== "active" || resumed || autoResumeAttempt.current === attempt.nonce) return;
    autoResumeAttempt.current = attempt.nonce;
    continueReply();
  }, [presetAttempt, attempt?.nonce, profileAvailable, connection, resumed]);

  function connectEmw() {
    if (!conversation || !profileAvailable || !isEmw) return;
    void run(async () => {
      const list = await api.customMcpList();
      if (list.standing_read_profile !== "emw_catalog_v1") {
        setProfileAvailable(false);
        throw new Error("Catalog access is no longer available. Please try again.");
      }
      const nonce = crypto.randomUUID();
      const target = new URL(returnTo);
      target.searchParams.set("mcp_attempt", nonce);
      const reply = await api.customMcpCreate({
        name: setup.label, endpoint_url: EMW_ENDPOINT, auth_kind: "oauth", visibility: "personal",
        required_scopes: ["catalog:read"], standing_read_profile: "emw_catalog_v1", return_to: target.href,
      });
      if (reply.mode !== "oauth_redirect") throw new Error("This server did not start OAuth sign-in.");
      const safeUrl = customMcpAuthorizationUrl(reply);
      const scopes = safeUrl ? new URL(safeUrl).searchParams.getAll("scope") : [];
      if (!safeUrl || scopes.length !== 1 || !matchesSetupScopes(scopes[0].split(" "), "catalog:read")) {
        throw new Error("The provider did not request exactly the required OAuth scopes.");
      }
      if (window.location.href !== originalHref) return;
      remember(reply.connection_id, EMW_ENDPOINT, nonce, "emw_catalog_v1");
      setAuthorizationUrl(safeUrl);
    });
  }

  function enableRead(pin: CustomMcpToolPin) {
    if (!connId || !connection || !attempt) return;
    void run(async () => {
      const { connections } = await api.customMcpList();
      const row = connections.find(verified);
      setConnection(row ?? null);
      const current = row?.tools?.find((tool) => tool.tool_name === pin.tool_name);
      if (row?.status !== "active" || !liveCallback(attempt) ||
          row.snapshot_revision !== connection.snapshot_revision ||
          !Number.isSafeInteger(connection.snapshot_revision) || connection.snapshot_revision <= 0 ||
          !/^sha256:[0-9a-f]{64}$/.test(pin.tool_digest) ||
          !Number.isSafeInteger(pin.updated_at) || pin.updated_at < 0 ||
          !current || current.tool_digest !== pin.tool_digest || current.updated_at !== pin.updated_at || !reviewableRead(current)) {
        setError("Tool access changed. Review the current tool before enabling it.");
        return;
      }
      let updated: CustomMcpConnectionSummary;
      try {
        ({ connection: updated } = await api.customMcpEnableReviewedRead(connId, pin.tool_name, {
          snapshot_revision: connection.snapshot_revision,
          tool_digest: pin.tool_digest,
          tool_updated_at: pin.updated_at,
        }));
      } catch (e: unknown) {
        if (e instanceof ApiError && e.code === "stale_revision") setConnection(null);
        throw e;
      }
      if (verified(updated)) setConnection(updated);
      else setRecheck((value) => value + 1);
    });
  }

  const current = outcome === "connected" && connection?.id === connId ? connection : null;
  const active = current?.status === "active";
  const tools = current?.tools ?? [];
  const canRead = tools.some((pin) => pin.mode === "read" && !pin.drifted);
  const readOptions = tools.filter(reviewableRead);
  const settings = `${routePath({ screen: "settings", section: "connections" })}?mcp_focus=${encodeURIComponent(connId ?? setup.label)}`;
  let status: string | null = null;
  let actions;
  if (resumed) {
    status = resumedPreset ? "Connected · Catalog read access" : null;
    actions = resumedPreset
      ? <a className="btn btn-sm btn-ghost" href={settings}>{t("Manage in Connections")}</a>
      : <span className="text-micro text-ok font-medium">{t("Reply resumed")}</span>;
  } else if (active && presetAttempt) {
    status = "Connected · Catalog read access";
    actions = error ? <button type="button" className="btn btn-primary" disabled={busy} onClick={continueReply}>{t("Resume")}</button> : null;
  } else if (active) {
    status = canRead ? "Connected." : "Connected. No tools can run automatically.";
    actions = <button type="button" className="btn btn-primary" disabled={busy} onClick={continueReply}>{t("Continue in chat")}</button>;
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
        ) : current?.status !== "needs_review" && (!presetAttempt || profileAvailable) ? (
          <button type="button" className="btn btn-primary" disabled={busy} onClick={retryOAuth}>{t("Retry OAuth sign-in")}</button>
        ) : null}
        <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => setDrawer("edit")}>{t("Edit endpoint")}</button>
      </div>
    );
  } else if (isEmw && profileAvailable && !connId) {
    actions = <button type="button" className="btn btn-primary" disabled={!conversation || busy} onClick={connectEmw}>{t("Connect EMW for catalog reads")}</button>;
  } else {
    actions = (
      <div className="flex flex-wrap gap-2">
        <button type="button" className="btn btn-primary" disabled={!conversation} onClick={() => setDrawer("connect")}>{t("Connect {label}").replace("{label}", () => setup.label)}</button>
        <button type="button" className="btn btn-ghost" disabled={!conversation} onClick={() => setDrawer("edit")}>{t("Edit endpoint")}</button>
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
        {isEmw && profileAvailable && !connId && <p className="text-micro text-ink-300">{t("Allow Atlas to read the EMW catalog in your personal chats until you disconnect. Other agents and workspace members cannot use this access.")}</p>}
        {status && <p className={`text-micro ${active ? "text-ok" : "text-warn"}`} role="status">{t(status)}</p>}
        {active && !presetAttempt && (
          <details className="text-micro text-ink-300">
            <summary className="cursor-pointer">{t("Review tool access")}</summary>
            <p>{t("Tools you enable can run without asking again.")}</p>
            {readOptions.length > 0 ? (
              <ul className="mt-2 space-y-2">
                {readOptions.map((pin) => (
                  <li key={pin.tool_name} className="flex flex-wrap items-start gap-2">
                    <div className="min-w-0 flex-1 break-words">
                      <code>{pin.tool_name}</code>
                      <p>{pin.description}</p>
                    </div>
                    <button type="button" className="btn btn-sm btn-secondary" disabled={busy}
                      aria-label={`${t("Enable read access")}: ${pin.tool_name}`} onClick={() => enableRead(pin)}
                    >{t("Enable read access")}</button>
                  </li>
                ))}
              </ul>
            ) : <p>{t("No read-only tools are available to enable here.")}</p>}
            <a className="btn btn-sm btn-ghost" href={settings} target="_blank" rel="noopener noreferrer">{t("Manage in Connections")}</a>
          </details>
        )}
        {current?.status === "needs_review" && <a className="btn btn-sm btn-ghost" href={settings}>{t("Manage in Connections")}</a>}
        {error && <p className="text-label text-fail break-words" role="alert">{t(error)}</p>}
      </TurnCard>
      {drawer && (
        <CustomMcpConnectDrawer
          setup={{ ...setup, endpointUrl: endpoint, returnTo, editEndpoint: drawer === "edit" }}
          onClose={() => setDrawer(null)}
          onConnected={() => setDrawer(null)}
          onOAuthStarted={remember}
        />
      )}
    </>
  );
}
