import { useContext, useState } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { HireBlock, WriteGate } from "../auth/WriteGate";
import { hostedConnectError, hostedConnectUrl, UI_HOSTED_REFUSED } from "../settings/hostedConnect";
import { TurnCard, type CardDensity } from "../../ui/TurnCard";
import { parseConnectRef, plainText } from "./chatCards";
import type { ThreadEntry } from "./thread";

/**
 * CAD-2443: the host's `connect_ref` card. It carries a toolkit and a label,
 * never a URL: the owner's button starts the board's own hosted connect flow
 * (`POST /api/connection-providers/:toolkit/connect`) and opens the https
 * sign-in it returns in a new tab. A member, or a payload that does not
 * parse, sees the entry's text line instead.
 */
export function ConnectCard({ entry, density = "full" }: { entry: ThreadEntry; density?: CardDensity }) {
  const t = useTranslate();
  const writeBlock = useContext(WriteGate);
  const operatorBlock = useContext(HireBlock);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const model = parseConnectRef(entry);
  if (!model) return <p className="text-micro text-ink-500 break-words chatcard-line">· {plainText(entry.text, 500)}</p>;
  // Owner only: a member or an unconfirmed session reads the text line.
  if (writeBlock !== null || operatorBlock !== null) {
    return <p className="text-micro text-ink-500 break-words chatcard-line" data-kind="connect_ref">· {plainText(entry.text, 500)}</p>;
  }
  const label = t("Connect {label}").replace("{label}", () => model.label);
  const connect = () => {
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
        <button type="button" className="btn btn-primary" data-connect-toolkit={model.toolkit} disabled={busy} onClick={connect}>
          {busy ? t("Opening sign-in…") : label}
        </button>
      }
    >
      <p className="text-label text-ink-300 break-words">{plainText(entry.text, 500)}</p>
      {error && (
        <p className="text-label text-fail break-words" role="alert">
          {t(error)}
        </p>
      )}
    </TurnCard>
  );
}
