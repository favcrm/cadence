import { useCallback, useEffect, useState } from "react";
import Button from "../../ui/Button";
import { navigate } from "../../lib/useLocation";
import { useTranslate } from "../../lib/locale";
import { retainedRequest, completeRequest } from "./requests";
import { accountLabel, connectReturnTo, publicationBinding, sendSlot } from "./socialConnect";
import { workspaceApps, type AppBinding, type Installation, type PublishDestination } from "./workspaceApps";

type Listed = { unavailable: boolean; destinations: PublishDestination[] } | "loading";

const CONNECT_UNAVAILABLE = "Instagram connect isn’t available for this workspace yet.";
const CONNECT_FAILED = "Couldn’t open the Instagram connect page. Try again.";
const USE_FAILED = "Couldn’t use that account for publishing. Reload and try again — nothing was changed.";

/**
 * CAD-1290 — Settings > Publish to Instagram. The owner connects Instagram on
 * AgenticOS (the host composes the link; local boards keep the local
 * connections page), sees the company's connected accounts, and taps "Use for
 * publishing" to point this install's publication binding at one of them for
 * the active context. The daemon re-checks the account against the company's
 * own list, and the binding revision, so a stale or forged choice refuses there.
 */
export function PublishToInstagram({ installation, bindings, contextId, canWrite, busy, refreshKey, mutate }: {
  installation: Installation; bindings: AppBinding[]; contextId: string | null; canWrite: boolean; busy: boolean;
  /** Changes when the board came back from AgenticOS: read the accounts again. */
  refreshKey: number;
  mutate: (work: () => Promise<void>) => Promise<void>;
}) {
  const t = useTranslate();
  const installId = installation.install_id;
  const slot = sendSlot(installation);
  const binding = slot ? publicationBinding(bindings, slot, contextId, installation.digest) : null;
  const [listed, setListed] = useState<Listed>("loading");
  const [note, setNote] = useState<string | null>(null);
  const load = useCallback((signal?: AbortSignal) => {
    setListed("loading");
    workspaceApps.destinations(installId, signal)
      .then(value => { if (!signal?.aborted) setListed(value); })
      .catch(() => { if (!signal?.aborted) setListed({ unavailable: true, destinations: [] }); });
  }, [installId]);
  useEffect(() => {
    const controller = new AbortController();
    load(controller.signal);
    return () => controller.abort();
  }, [load, refreshKey]);
  // Just back from AgenticOS: the new connection can take a moment to show, so
  // read again a few times until an account appears (no restart needed).
  useEffect(() => {
    if (refreshKey === 0) return;
    let left = 5;
    const timer = window.setInterval(() => {
      if (left-- <= 0) { window.clearInterval(timer); return; }
      workspaceApps.destinations(installId).then(value => {
        if (value.destinations.length) { setListed(value); window.clearInterval(timer); }
      }).catch(() => {});
    }, 3000);
    return () => window.clearInterval(timer);
  }, [installId, refreshKey]);
  if (!slot) return null;
  const connect = async () => {
    setNote(null);
    try {
      const reply = await workspaceApps.connectLink(installId, connectReturnTo(window.location.origin, installId));
      if (reply.hosted && reply.url) window.location.assign(reply.url);
      else if (reply.hosted) setNote(CONNECT_UNAVAILABLE);
      else navigate("/settings/connections");
    } catch { setNote(CONNECT_FAILED); }
  };
  const current = binding?.config.publish ?? null;
  const rows = listed === "loading" ? [] : listed.destinations;
  const stale = !!current && listed !== "loading" && !listed.unavailable && !rows.some(row => row.destination_id === current.destination_id);
  return (
    <section className="wa-panel wa-stack" aria-label="Publish to Instagram" data-publish-instagram>
      <h2>{t("Publish to Instagram")}</h2>
      <p className="wa-muted">{t("Connect your Instagram account once, then choose where this app publishes.")}</p>
      {listed === "loading" && <p className="wa-muted" role="status">{t("Checking Instagram…")}</p>}
      {listed !== "loading" && listed.unavailable && (
        <p className="wa-alert" role="alert">{t("Couldn’t check Instagram right now.")} <Button size="sm" onClick={() => load()}>{t("Try again")}</Button></p>
      )}
      {listed !== "loading" && !listed.unavailable && rows.length === 0 && !stale && <p className="wa-muted">{t("Not connected")}</p>}
      {stale && current && (
        <p className="wa-alert">{t("Connection expired — {account} can’t publish until you reconnect.").replace("{account}", () => accountLabel(current.destination_label))}</p>
      )}
      <ul className="wa-stack" style={{ listStyle: "none", padding: 0, margin: 0 }}>
        {rows.map(row => {
          const using = current?.destination_id === row.destination_id;
          return (
            <li key={row.destination_id} className="wa-row" data-destination={row.destination_id}>
              <span>{t("Connected:")} <strong>{accountLabel(row.label || row.destination_id)}</strong></span>
              {using ? <span className="wa-kicker">{t("Publishing here")}</span> : (
                <Button size="sm" variant="primary" disabled={!canWrite || busy}
                  onClick={() => void mutate(async () => {
                    const key = JSON.stringify([installId, "use-destination", contextId, slot, row.destination_id, binding?.revision ?? 0]);
                    try {
                      await workspaceApps.useDestination(installId, {
                        destination_id: row.destination_id,
                        ...(contextId ? { context_id: contextId } : {}),
                        ...(binding ? { expected_revision: binding.revision } : { request_id: retainedRequest(key) }),
                      });
                      completeRequest(key);
                    } catch {
                      throw new Error(USE_FAILED);
                    }
                  })}>{t("Use for publishing")}</Button>
              )}
            </li>
          );
        })}
      </ul>
      {note && <p className="wa-alert" role="alert">{t(note)}</p>}
      {canWrite && listed !== "loading" && (
        <div className="wa-row">
          <Button onClick={() => void connect()} disabled={busy}>{stale ? t("Reconnect") : rows.length ? t("Connect another account") : t("Connect Instagram")}</Button>
        </div>
      )}
    </section>
  );
}
