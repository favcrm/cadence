import { useEffect, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { appExplorer, notifyAppsChanged, type CatalogCard } from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import { AppGlyph, TrustChip } from "./shared";
import InstallCheckPanel from "./InstallCheckPanel";
import { appErrorCopy, appLoadErrorCopy } from "./appErrors";
import "./explorer.css";

/**
 * One catalog app's detail (CAD-1129, `/apps/catalog/<id>`): the
 * OS-style page — hero, screenshots, "What it can do", "What it can
 * access" and the never-line. An installed app gets a Manage tab; a
 * member sees the same access sentences and "Ask an admin", never the
 * operator's internals.
 */
export default function CatalogDetail({ id, viewer }: { id: string; viewer: Viewer }) {
  const [card, setCard] = useState<CatalogCard | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [reviewing, setReviewing] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const isOp = viewer.operator && !viewer.readOnly;
  const installed = card?.state === "installed" || card?.state === "off";
  const removed = card?.removed === true;

  useEffect(() => {
    const controller = new AbortController();
    setError(null);
    void appExplorer.entry(id, controller.signal)
      .then((c) => { if (!controller.signal.aborted) setCard(c); })
      .catch((e: unknown) => { if (!controller.signal.aborted) setError(appLoadErrorCopy(e, "Could not load the app. Try again in a moment.")); });
    return () => controller.abort();
  }, [id]);

  // The install starts with a check (CAD-1194); the panel installs the
  // checked digest.
  const install = () => { setNotice(null); setReviewing(true); };
  const requestInstall = () => {
    if (!card) return;
    setBusy(true);
    void appExplorer.requestInstall(card.id)
      .then(() => { setCard({ ...card, requested_by_me: true, state: "requested" }); setNotice("Asked — your admin will see the request."); })
      .catch((e: unknown) => setNotice(appErrorCopy(e, "Could not send the request. Try again in a moment.")))
      .finally(() => setBusy(false));
  };
  const restore = () => {
    if (!card?.install_id) return;
    setBusy(true); setNotice(null);
    void appExplorer.restore(card.install_id)
      .then(() => { setNotice("Restored — it's live again."); setCard({ ...card, state: "installed", removed: false, restorable: undefined }); notifyAppsChanged(); })
      .catch((e: unknown) => setNotice(appErrorCopy(e, "Restore didn't finish. Try again in a moment.")))
      .finally(() => setBusy(false));
  };

  if (error) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
      <div className="card px-4 py-5" role="alert">
        <h2 className="font-medium text-ink-100 mb-1">Couldn't load the app</h2>
        <p className="text-label text-ink-400 mb-3">{error}</p>
        <Link href="/apps/explore" className="btn">Back to Explore</Link>
      </div>
    </main>;
  }
  if (!card) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
      <p className="text-label text-ink-400" role="status">Loading…</p>
    </main>;
  }

  const access = card.access ?? [];
  const can = card.can ?? card.listing?.can ?? [];
  const shots = card.screenshots ?? card.listing?.screenshots ?? [];
  const about = card.about ?? card.listing?.about;
  const setup = card.setup ?? card.listing?.setup ?? [];
  const data = card.data ?? card.listing?.data;

  return (
    <main className="apps-detail px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={card.title}>
      <Link href="/apps/explore" className="text-label text-ink-400 hover:text-ink-100">← Explore</Link>

      <div className="hero mt-3">
        <AppGlyph name={card.name} icon={card.listing?.icon} size="lg" />
        <div className="min-w-0">
          <h1>{card.title}</h1>
          <p className="tag">{card.listing?.tagline}</p>
          <div className="by">
            <TrustChip trust={card.trust} />
            <span>·</span>
            <span>{card.listing?.category ?? "App"}</span>
            {installed && <span className="chip ok">Installed</span>}
            {card.state === "off" && <span className="chip">Access off</span>}
            {removed && <span className="chip">Removed</span>}
          </div>
        </div>
      </div>

      {isOp && reviewing && !installed && !removed && (
        <div className="mt-4">
          <InstallCheckPanel
            source={`builtin:${card.id}`}
            label={card.title}
            install={(digest) => appExplorer.installEntry(card.id, digest)}
            onInstalled={() => { setReviewing(false); setNotice("Installed — it's in your apps now."); setCard({ ...card, state: "installed" }); notifyAppsChanged(); }}
            onClose={() => setReviewing(false)}
          />
        </div>
      )}
      {notice && <div className="alert info mt-4" role="status">{notice}</div>}
      {card.source_kind === "git" && (
        <div className="alert warn mt-4">Added from a Git URL by your team. Cadence checked its file list but hasn't reviewed what it does.</div>
      )}
      {removed && (
        <div className="alert warn mt-4" role="status">
          {card.title} was removed. {card.restorable ? "You can restore it, with the same data, for 30 days after it was removed." : "The 30-day restore window has closed, so it stays removed."}
        </div>
      )}
      {card.state === "off" && (
        <div className="alert warn mt-4">Access is off — {card.title} can't run or use its connections until it's turned back on.</div>
      )}

      <div className="dgrid">
        <div className="min-w-0">
          {shots.length > 0 && (
            <div className="shots" aria-label="Screenshots">
              {shots.map((s, i) => (
                <div key={i} className="shot">
                  <div className="mui">screenshot</div>
                  {s.caption && <span className="cap">{s.caption}</span>}
                </div>
              ))}
            </div>
          )}
          {about && <p className="about">{about}</p>}

          {can.length > 0 && (
            <div className="blk">
              <h2>What it can do</h2>
              <ul className="can">
                {can.map((c, i) => <li key={i}><span aria-hidden>✓</span><span>{c}</span></li>)}
              </ul>
            </div>
          )}

          <div className="blk">
            <h2>What it can access</h2>
            <div className="access">
              {access.map((row, i) => (
                <div key={i} className="acc">
                  <span className="ai" aria-hidden>{row.icon}</span>
                  <div>
                    <b>{row.title}</b>
                    <span>{row.sentence}</span>
                    {row.note && <span className="block">{row.note}</span>}
                  </div>
                </div>
              ))}
              <div className="never">
                <span aria-hidden>⛨</span>
                <span>{card.never ?? "It never sees your passwords — your logins stay with Cadence."}</span>
              </div>
            </div>
          </div>

          {setup && setup.length > 0 && (
            <div className="blk">
              <h2>Setup</h2>
              <ul className="can">
                {setup.map((s, i) => (
                  <li key={i}>
                    <span aria-hidden>◦</span>
                    <span><b>{s.label}</b>{s.help && <span className="block text-ink-500">{s.help}</span>}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {data && (data.stores?.length || data.personal) && (
            <div className="blk">
              <h2>Your data</h2>
              <div className="access">
                {(data.stores ?? []).map((r, i) => <div key={`r${i}`} className="acc"><span className="ai">●</span><div><b>Keeps</b><span>{r}</span></div></div>)}
                {data.personal && <div className="alert warn mt-2">Holds personal data.</div>}
              </div>
            </div>
          )}
        </div>

        <aside className="card side-card p-4 flex flex-col gap-3">
          {installed && card.install_id ? (
            <>
              <Link className="btn btn-primary btn-lg" href={`/app-installations/${card.install_id}`}>Open {card.title}</Link>
              {isOp && <Link className="btn" href={`/apps/manage/${card.install_id}`}>Manage</Link>}
              {!isOp && <span className="text-micro text-ink-500">Ask an admin to manage access.</span>}
            </>
          ) : removed ? (
            isOp && card.restorable ? (
              <>
                <Button className="btn-primary btn-lg" disabled={busy} onClick={restore}>{busy ? "Restoring…" : "Restore"}</Button>
                <span className="text-micro text-ink-500">Brings back {card.title} with its data.</span>
              </>
            ) : (
              <span className="text-micro text-ink-500">{isOp ? "The restore window has closed." : "This app was removed. Ask an admin."}</span>
            )
          ) : card.requested_by_me || card.state === "requested" ? (
            <>
              <Button className="btn-lg" disabled>Asked</Button>
              <span className="text-micro text-ink-500">Your admin will see the request.</span>
            </>
          ) : isOp ? (
            <>
              <Button className="btn-primary btn-lg" disabled={busy || reviewing} onClick={install}>Install</Button>
              <p className="text-micro text-ink-500">
                Installing lets {card.title} use what's listed under <b>What it can access</b>. You can turn its access off or remove it at any time.
              </p>
            </>
          ) : (
            <>
              <Button className="btn-lg" disabled={busy} onClick={requestInstall}>{busy ? "Asking…" : "Ask an admin to install"}</Button>
              <span className="text-micro text-ink-500">Only admins can install apps.</span>
            </>
          )}
          <dl className="dl mt-3">
            <dt>Made by</dt><dd><TrustChip trust={card.trust} /></dd>
            <dt>Version</dt><dd>{card.version}</dd>
            <dt>Category</dt><dd>{card.listing?.category ?? "App"}</dd>
            {isOp && card.digest && <><dt>Digest</dt><dd className="num break-all">{card.digest}</dd></>}
          </dl>
        </aside>
      </div>
    </main>
  );
}
