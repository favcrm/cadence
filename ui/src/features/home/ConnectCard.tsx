import { useContext, useState } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import { routePath } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import { HireBlock, WriteGate } from "../auth/WriteGate";
import { customMcpAuthorizationUrl, customMcpErrorText, customMcpReturn } from "../settings/customMcpView";
import { hostedConnectError, hostedConnectUrl, UI_HOSTED_REFUSED } from "../settings/hostedConnect";
import { withoutConnectParams } from "../workspace-apps/socialConnect";
import { TurnCard, type CardDensity } from "../../ui/TurnCard";
import { parseConnectRef, plainText } from "./chatCards";
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
  const model = parseConnectRef(entry);
  if (!model) return <p className="text-micro text-ink-500 break-words chatcard-line">· {plainText(entry.text, 500)}</p>;
  // Owner only: a member or an unconfirmed session reads the text line.
  if (writeBlock !== null || operatorBlock !== null) {
    return <p className="text-micro text-ink-500 break-words chatcard-line" data-kind="connect_ref">· {plainText(entry.text, 500)}</p>;
  }
  const isCustomMcp = model.toolkit === "custom_mcp";
  const mcpReturned =
    isCustomMcp && typeof window !== "undefined" ? customMcpReturn(window.location.search) : null;
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
