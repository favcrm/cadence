import { useCallback, useEffect, useState } from "react";
import Button from "../../ui/Button";
import { navigate } from "../../lib/useLocation";
import { retainedRequest, completeRequest } from "./requests";
import { accountLabel, connectReturnTo, isGrantId, publicationBinding, sendSlot } from "./socialConnect";
import { workspaceApps, type AppBinding, type Installation, type PublishDestination } from "./workspaceApps";

type Listed = { unavailable: boolean; destinations: PublishDestination[] } | "loading";

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
  const installId = installation.install_id;
  const slot = sendSlot(installation);
  const binding = slot ? publicationBinding(bindings, slot, contextId, installation.digest) : null;
  const [listed, setListed] = useState<Listed>("loading");
  const [grant, setGrant] = useState("");
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
      else navigate("/settings/connections");
    } catch { setNote("Couldn't open the Instagram connect page. Try again."); }
  };
  const current = binding?.config.publish ?? null;
  const rows = listed === "loading" ? [] : listed.destinations;
  const stale = !!current && listed !== "loading" && !listed.unavailable && !rows.some(row => row.destination_id === current.destination_id);
  const grantOk = isGrantId(grant.trim());
  return (
    <section className="wa-panel wa-stack" aria-label="Publish to Instagram" data-publish-instagram>
      <h2>Publish to Instagram</h2>
      <p className="wa-muted">Connect your Instagram account once, then choose where this app publishes.</p>
      {listed === "loading" && <p className="wa-muted" role="status">Checking Instagram…</p>}
      {listed !== "loading" && listed.unavailable && (
        <p className="wa-alert" role="alert">Couldn’t check Instagram right now. <Button size="sm" onClick={() => load()}>Try again</Button></p>
      )}
      {listed !== "loading" && !listed.unavailable && rows.length === 0 && !stale && <p className="wa-muted">Not connected</p>}
      {stale && current && (
        <p className="wa-alert">Connection expired — {accountLabel(current.destination_label)} can’t publish until you reconnect.</p>
      )}
      {rows.length > 0 && (
        <label className="wa-field">
          <span>Send grant</span>
          <input className="wa-input" type="text" value={grant} placeholder="dpq_…" autoComplete="off" spellCheck={false}
            onChange={event => setGrant(event.target.value)} disabled={!canWrite || busy} />
          <span className="wa-muted">{grant && !grantOk ? "A send grant id is dpq_ followed by 8–64 letters, digits, _ or -." : "The grant your AgenticOS owner approved for publishing."}</span>
        </label>
      )}
      <ul className="wa-stack" style={{ listStyle: "none", padding: 0, margin: 0 }}>
        {rows.map(row => {
          const using = current?.destination_id === row.destination_id;
          return (
            <li key={row.destination_id} className="wa-row" data-destination={row.destination_id}>
              <span>Connected: <strong>{accountLabel(row.label || row.destination_id)}</strong></span>
              {using ? <span className="wa-kicker">Publishing here</span> : (
                <Button size="sm" variant="primary" disabled={!canWrite || busy || !grantOk}
                  onClick={() => void mutate(async () => {
                    const key = JSON.stringify([installId, "use-destination", contextId, slot, row.destination_id, binding?.revision ?? 0]);
                    try {
                      await workspaceApps.useDestination(installId, {
                        destination_id: row.destination_id, grant_id: grant.trim(),
                        ...(contextId ? { context_id: contextId } : {}),
                        ...(binding ? { expected_revision: binding.revision } : { request_id: retainedRequest(key) }),
                      });
                      completeRequest(key);
                    } catch {
                      throw new Error("Couldn’t use that account for publishing. Reload and try again — nothing was changed.");
                    }
                  })}>Use for publishing</Button>
              )}
            </li>
          );
        })}
      </ul>
      {note && <p className="wa-alert" role="alert">{note}</p>}
      {canWrite && listed !== "loading" && (
        <div className="wa-row">
          <Button onClick={() => void connect()} disabled={busy}>{stale ? "Reconnect" : rows.length ? "Connect another account" : "Connect Instagram"}</Button>
        </div>
      )}
    </section>
  );
}
