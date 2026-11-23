import { useContext, useState } from "react";
import { ApiError, api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { usingPrivateOperation } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import { routePath } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import { HireBlock, WriteGate } from "../auth/WriteGate";
import { useImageConversation } from "../canvas/imageConversation";
import { CustomMcpConnectDrawer } from "../settings/CustomMcpDrawer";
import { customMcpAuthorizationUrl, customMcpErrorText, customMcpReturn } from "../settings/customMcpView";
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
  const [busy, setBusy] = useState(false);
  const [continuing, setContinuing] = useState(false);
  const [resumed, setResumed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const setup = parseCustomMcpSetup(entry);
  if (setup && writeBlock === null && operatorBlock === null) {
    return <CustomMcpSetupCard entry={entry} setup={setup} density={density} />;
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
      if (typeof window !== "undefined") {
        navigate(
          window.location.pathname + withoutConnectParams(window.location.search) + window.location.hash,
          { replace: true },
        );
      }
      setResumed(true);
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

function CustomMcpSetupCard({ entry, setup, density }: {
  entry: ThreadEntry;
  setup: CustomMcpSetup;
  density: CardDensity;
}) {
  const t = useTranslate();
  const conversation = useImageConversation();
  const [drawer, setDrawer] = useState(false);
  const [busy, setBusy] = useState(false);
  const [resumed, setResumed] = useState(false);
  const [cleared, setCleared] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const url = new URL(window.location.href);
  const callback = !cleared && url.searchParams.get("mcp_setup") === String(entry.seq)
    ? customMcpReturn(url.search)
    : null;
  const key = `cadence:mcp-setup:${conversation}:${entry.seq}`;
  let connId: string | null = null;
  if (!cleared && conversation) {
    try {
      connId = window.sessionStorage.getItem(key);
    } catch {
      // No verified return without the local enrollment receipt.
    }
  }
  const returned = connId && callback?.connectionId === connId ? callback : null;
  const ready = returned?.outcome === "connected" && !!conversation;
  const settings = `${routePath({ screen: "settings", section: "connections" })}?mcp_focus=${encodeURIComponent(connId ?? setup.label)}`;
  const clearReturn = () => {
    const clean = new URL(window.location.href);
    clean.search = withoutConnectParams(clean.search);
    clean.searchParams.delete("mcp_setup");
    navigate(`${clean.pathname}${clean.search}${clean.hash}`, { replace: true });
    setCleared(true);
    window.sessionStorage.removeItem(key);
  };

  const retryOAuth = async () => {
    if (busy || !connId) return;
    setBusy(true);
    setError(null);
    try {
      const started = await api.customMcpOAuthStart(connId, { return_to: url.href });
      const authUrl = customMcpAuthorizationUrl(started);
      if (started.connection_id !== connId || authUrl === null) {
        setError("Server did not return a valid https:// authorization URL.");
        return;
      }
      window.location.assign(authUrl);
    } catch (e: unknown) {
      if (e instanceof ApiError && e.status === 404) clearReturn();
      setError(customMcpErrorText(e));
    } finally {
      setBusy(false);
    }
  };

  const continueReply = async () => {
    if (busy || !ready || !conversation || !connId) return;
    setBusy(true);
    setError(null);
    try {
      await usingPrivateOperation(undefined, async (operation) => {
        const { connections } = await api.customMcpList(true, operation);
        const row = connections.find((item) => item.id === connId);
        if (!row) {
          clearReturn();
          setError("This connection is no longer available. Start a new connection.");
          return;
        }
        if (row.status !== "active" || row.name !== setup.label ||
            row.endpoint_host !== new URL(setup.endpointUrl).host ||
            (row.auth_kind !== "oauth" && row.auth_kind !== "oauth_static") ||
            (setup.requiredScopes && (row.oauth_scopes.length !== setup.requiredScopes.length ||
              setup.requiredScopes.some((scope) => !row.oauth_scopes.includes(scope))))) {
          setError("Cadence could not verify this Custom MCP connection.");
          return;
        }
        if (!row.tools?.some((tool) => tool.mode === "read" && !tool.drifted)) {
          setError("Enable the required read tool in Connections, then try again.");
          return;
        }
        const sent = await sendToMaster(
          `Custom MCP connection ${connId} has enabled read tools. Continue my original request with this connection.`,
          undefined, undefined, undefined, conversation, operation,
        );
        if (!sent.ok) {
          setError(sent.error);
          return;
        }
        clearReturn();
        setResumed(true);
      });
    } catch (e: unknown) {
      setError(customMcpErrorText(e));
    } finally {
      setBusy(false);
    }
  };

  const openSetup = () => {
    setError(null);
    try {
      window.sessionStorage.removeItem(key);
      setDrawer(true);
    } catch {
      setError("Allow session storage in this tab to finish OAuth and return to this conversation.");
    }
  };
  url.search = withoutConnectParams(url.search);
  url.searchParams.set("mcp_setup", String(entry.seq));
  return (
    <>
      <TurnCard kind="connect" className="card chatcard" bodyClassName="chatcard-body" density={density}
        data-kind="connect_ref" label={t("Connect card")} kindLabel={t("Connection")} subject={setup.label}
        footer={resumed ? (
          <span className="text-micro text-ok font-medium">{t("Reply resumed")}</span>
        ) : ready ? (
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" className="btn btn-primary" disabled={busy} onClick={() => void continueReply()}>
              {busy ? t("Checking connection…") : t("Verify and continue")}
            </button>
            <a className="btn btn-sm btn-ghost" href={settings} target="_blank" rel="noopener noreferrer">
              {t("Manage in Connections")}
            </a>
          </div>
        ) : connId ? (
          <button type="button" className="btn btn-primary" disabled={busy} onClick={() => void retryOAuth()}>
            {busy ? t("Opening sign-in…") : t("Retry OAuth sign-in")}
          </button>
        ) : (
          <button type="button" className="btn btn-primary" disabled={!conversation} onClick={openSetup}>
            {t("Connect {label}").replace("{label}", () => setup.label)}
          </button>
        )}
      >
        <p className="text-label text-ink-300 break-words">{plainText(entry.text, 500)}</p>
        {ready && <p className="text-micro text-ink-300">{t("Review and enable the required read tool in Connections before continuing.")}</p>}
        {returned && returned.outcome !== "connected" && (
          <p className="text-micro text-warn" role="status">{t("OAuth sign-in did not finish — try connecting again.")}</p>
        )}
        {error && <p className="text-label text-fail break-words" role="alert">{t(error)}</p>}
      </TurnCard>
      {drawer && (
        <CustomMcpConnectDrawer
          setup={{ ...setup, returnTo: url.href }}
          onClose={() => setDrawer(false)}
          onConnected={() => setDrawer(false)}
          onOAuthStarted={(id) => {
            window.sessionStorage.setItem(key, id);
            setCleared(false);
          }}
        />
      )}
    </>
  );
}
