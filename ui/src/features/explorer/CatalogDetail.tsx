import { useEffect, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { appExplorer, notifyAppsChanged, type CatalogCard } from "../workspace-apps/workspaceApps";
import { useInstallations } from "../workspace-apps/useInstallations";
import { installationAppHref, installedAppHref } from "../apps/appViewModel";
import type { Viewer } from "../projects/work";
import { AppGlyph, TrustChip } from "./shared";
import { appErrorCopy, appLoadErrorCopy } from "./appErrors";
import { useTranslate } from "../../lib/locale";
import "./explorer.css";
import { PublicAppDetail } from "./publicDirectory";

/**
 * One catalog app (`/apps/catalog/<id>`). A published public app renders
 * here. A retired built-in id still shows its tombstone and real install
 * state, with no marketplace hand-off and no Install for that source.
 */
export default function CatalogDetail({ id, viewer }: { id: string; viewer: Viewer }) {
  const t = useTranslate();
  const isOp = viewer.operator === true && !viewer.readOnly;
  const [card, setCard] = useState<CatalogCard | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [publicHit, setPublicHit] = useState<"pending" | "yes" | "no">("pending");
  // CAD-1438: Open spells the canonical `/apps/<key>` route — for the
  // operator resolved against the verified install list, for a member
  // the key route itself proves the mapping before rendering anything.
  const installs = useInstallations(viewer.operator === true, id).list;
  const installed = card?.state === "installed" || card?.state === "off";
  const removed = card?.removed === true;
  const retired = card?.retired === true;

  useEffect(() => {
    setPublicHit("pending");
    if (viewer.operator === null) return;
    const controller = new AbortController();
    setError(null);
    void appExplorer.entry(id, controller.signal)
      .then((c) => { if (!controller.signal.aborted) setCard(c); })
      .catch((e: unknown) => { if (!controller.signal.aborted) setError(appLoadErrorCopy(e, "Could not load the app. Try again in a moment.")); });
    return () => controller.abort();
  }, [id, viewer.operator]);

  const restore = () => {
    if (!card?.install_id) return;
    setBusy(true); setNotice(null);
    void appExplorer.restore(card.install_id)
      .then(() => { setNotice("Restored — it's live again."); setCard({ ...card, state: "installed", removed: false, restorable: undefined }); notifyAppsChanged(); })
      .catch((e: unknown) => setNotice(appErrorCopy(e, "Restore didn't finish. Try again in a moment.")))
      .finally(() => setBusy(false));
  };

  if (publicHit !== "no") {
    return (
      <main className="apps-detail px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={t("app catalog detail")}>
        <PublicAppDetail id={id} viewer={viewer} onResolved={(found) => setPublicHit(found ? "yes" : "no")} />
      </main>
    );
  }

  if (viewer.operator === null) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9" aria-label={t("app catalog detail")}>
      <h1 className="text-section font-semibold">{t("No such app.")}</h1>
      <Link href="/apps/explore">{t("Back to Explore")}</Link>
    </main>;
  }
  if (error) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
      <div className="card px-4 py-5" role="alert">
        <h2 className="font-medium text-ink-100 mb-1">{t("Couldn't load the app")}</h2>
        <p className="text-label text-ink-400 mb-3">{t(error)}</p>
        <Link href="/apps/explore" className="btn">{t("Back to Explore")}</Link>
      </div>
    </main>;
  }
  if (!card) {
    return <main className="apps-detail px-4 lg:px-8 pt-4 pb-9">
      <p className="text-label text-ink-400" role="status">{t("Loading…")}</p>
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
      <Link href="/apps/explore" className="text-label text-ink-400 hover:text-ink-100">{t("← Explore")}</Link>

      <div className="hero mt-3">
        <AppGlyph name={card.name} icon={card.listing?.icon} size="lg" />
        <div className="min-w-0">
          <h1>{card.title}</h1>
          <p className="tag">{card.listing?.tagline}</p>
          <div className="by">
            <TrustChip trust={card.trust} />
            <span>·</span>
            <span>{card.listing?.category ?? "App"}</span>
            {installed && <span className="chip ok">{t("Installed")}</span>}
            {card.state === "off" && <span className="chip">{t("Access off")}</span>}
            {removed && <span className="chip">{t("Removed")}</span>}
          </div>
        </div>
      </div>

      {notice && <div className="alert info mt-4" role="status">{t(notice)}</div>}
      {retired && (
        <div className="alert warn mt-4" role="status">
          {card.retired_reason
            ? t(card.retired_reason)
            : t("This built-in catalog app was retired — the host no longer ships it. The app now lives in its own repository and the AgenticOS marketplace.")}
        </div>
      )}
      {card.source_kind === "git" && (
        <div className="alert warn mt-4">{t("Added from a Git URL by your team. Cadence checked its file list but hasn't reviewed what it does.")}</div>
      )}
      {removed && (
        <div className="alert warn mt-4" role="status">
          {t(card.restorable ? "{title} was removed. You can restore it, with the same data, for 30 days after it was removed." : "{title} was removed. The 30-day restore window has closed, so it stays removed.").replace("{title}", () => card.title)}
        </div>
      )}
      {card.state === "off" && (
        <div className="alert warn mt-4">{t("Access is off — {title} can't run or use its connections until it's turned back on.").replace("{title}", () => card.title)}</div>
      )}

      <div className="dgrid">
        <div className="min-w-0">
          {shots.length > 0 && (
            <div className="shots" aria-label={t("Screenshots")}>
              {shots.map((s, i) => (
                <div key={i} className="shot">
                  <div className="mui">{t("screenshot")}</div>
                  {s.caption && <span className="cap">{s.caption}</span>}
                </div>
              ))}
            </div>
          )}
          {about && <p className="about">{about}</p>}

          {can.length > 0 && (
            <div className="blk">
              <h2>{t("What it can do")}</h2>
              <ul className="can">
                {can.map((c, i) => <li key={i}><span aria-hidden>✓</span><span>{c}</span></li>)}
              </ul>
            </div>
          )}

          <div className="blk">
            <h2>{t("What it can access")}</h2>
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
                <span>{card.never ?? t("It never sees your passwords — your logins stay with Cadence.")}</span>
              </div>
            </div>
          </div>

          {setup && setup.length > 0 && (
            <div className="blk">
              <h2>{t("Setup")}</h2>
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
              <h2>{t("Your data")}</h2>
              <div className="access">
                {(data.stores ?? []).map((r, i) => <div key={`r${i}`} className="acc"><span className="ai">●</span><div><b>{t("Keeps")}</b><span>{r}</span></div></div>)}
                {data.personal && <div className="alert warn mt-2">{t("Holds personal data.")}</div>}
              </div>
            </div>
          )}
        </div>

        <aside className="card side-card p-4 flex flex-col gap-3">
          {installed && card.install_id ? (
            <>
              {(() => {
                const href = isOp
                  ? installationAppHref(installs, card.install_id)
                  : installedAppHref(card.name);
                return href === null ? (
                  <span className="text-micro text-ink-500">{t("This install has no unique app page — see Manage.")}</span>
                ) : (
                  <Link className="btn btn-primary btn-lg" href={href}>Open {card.title}</Link>
                );
              })()}
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
          ) : retired ? (
            <span className="text-micro text-ink-500">
              {t("The built-in source is retired. A current release appears in the public directory when it is published.")}
            </span>
          ) : card.requested_by_me || card.state === "requested" ? (
            <>
              <Button className="btn-lg" disabled>Asked</Button>
              <span className="text-micro text-ink-500">Your admin will see the request.</span>
            </>
          ) : (
            <>
              <Button className="btn-lg" disabled>{t("Install unavailable")}</Button>
              <span className="text-micro text-ink-500">{t("This catalog entry is retired — see the notice above.")}</span>
            </>
          )}
          <dl className="dl mt-3">
            <dt>Made by</dt><dd><TrustChip trust={card.trust} /></dd>
            {card.version && <><dt>Version</dt><dd>{card.version}</dd></>}
            {card.listing?.category && <><dt>Category</dt><dd>{card.listing.category}</dd></>}
            {retired && <><dt>Source</dt><dd>{t("Retired built-in catalog")}</dd></>}
            {isOp && card.digest && <><dt>Digest</dt><dd className="num break-all">{card.digest}</dd></>}
          </dl>
        </aside>
      </div>
    </main>
  );
}
