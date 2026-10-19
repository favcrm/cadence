import { useMemo, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { useTranslate } from "../../lib/locale";
import { appExplorer, notifyAppsChanged, type CatalogCard } from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import { AppGlyph, TrustChip, InstallStateChip, useEscape } from "./shared";
import { appErrorCopy, appLoadErrorCopy } from "./appErrors";
import { displayUiNotice, type UiNotice } from "../../lib/uiNotice";
import InstallCheckPanel from "./InstallCheckPanel";
import { Notice } from "../app-shell/shared/States";
import { useBackoffLoad } from "../workspace-apps/useBackoffLoad";
import "./explorer.css";
import { PublicDirectory } from "./publicDirectory";
import AppCover from "./AppCover";

/**
 * The Apps Explorer (CAD-1471, `/apps/explore`). Published apps come from
 * the public catalog. The retired built-in catalog stays as tombstones.
 * A checked Git URL remains an operator-only local source.
 */
export default function Explorer({ viewer }: { viewer: Viewer }) {
  const t = useTranslate();
  const [revision, setRevision] = useState(0);
  const [gitOpen, setGitOpen] = useState(false);
  const [notice, setNotice] = useState<UiNotice | null>(null);

  const isOp = viewer.operator === true && !viewer.readOnly;

  // The retired catalog still answers: tombstones for ids history names,
  // never a fake "no apps" or up-to-date state. A failed read is shown.
  const catalogLoad = useBackoffLoad((signal) => appExplorer.catalog(signal).then((r) => r.catalog), revision, viewer.operator !== null);
  const requestsLoad = useBackoffLoad((signal) => appExplorer.requests(signal).then((r) => r.requests), revision, isOp);
  const catalog = catalogLoad.data;
  const requests = requestsLoad.data;
  const error = catalogLoad.error === null ? null : appLoadErrorCopy(catalogLoad.error, "The catalog didn't load.");

  const tombstones = useMemo(() => catalog ?? [], [catalog]);


  if (viewer.operator === null && viewer.access === "unavailable") {
    return (
      <main className="apps-explore px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="explore apps">
        <h1 className="text-section font-semibold text-ink-100 mb-4">{t("Explore apps")}</h1>
        <PublicDirectory viewer={viewer} />
        <Notice state="access-unavailable" onRetry={viewer.onRetryAccess} retryLabel="Retry access check">
          Access could not be confirmed. Retry the access check before acting.
        </Notice>
      </main>
    );
  }

  return (
    <main className="apps-explore px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="explore apps">
      <div className="ohead flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">{t("Explore apps")}</h1>
        <span className="grow" />
        <div className="popwrap relative">
          <Button className="btn-sm" onClick={() => setGitOpen((v) => !v)} aria-expanded={gitOpen}>
            More ▾
          </Button>
          {gitOpen && (
            <div className="pmenu" role="menu">
              {isOp ? (
                <button className="w-full text-left" onClick={() => setGitOpen(false)} data-git>
                  {t("Add from Git URL…")}<small className="block text-ink-500">{t("For custom apps your team built")}</small>
                </button>
              ) : (
                <button className="w-full text-left opacity-50" disabled>
                  Add from Git URL<small className="block text-ink-500">Admins only</small>
                </button>
              )}
              <Link href="/apps" className="block">Your installed apps</Link>
            </div>
          )}
        </div>
      </div>
      <PublicDirectory viewer={viewer} />

      {isOp && gitOpen && (
        <GitAdd
          onDone={() => { setGitOpen(false); setNotice("Installed — it's in your apps now."); setRevision((r) => r + 1); notifyAppsChanged(); }}
          onClose={() => setGitOpen(false)}
        />
      )}

      {notice && <div className="alert info mb-4" role="status">{displayUiNotice(notice, t)}</div>}

      {isOp && requests && requests.length > 0 && (
        <div className="alert info mb-4" role="status">
          {t(requests.length === 1 ? "{count} teammate asked for an app — see the request count on its card." : "{count} teammates asked for an app — see the request count on its card.").replace("{count}", String(requests.length))}
        </div>
      )}

      {error !== null && (
        <div className="err card px-4 py-3 mb-4 text-label text-ink-400" role="alert">
          <b className="text-ink-100">{t("Couldn't load the app catalog.")}</b> {t(error)} <Button onClick={() => setRevision((r) => r + 1)}>{t("Try again")}</Button>
        </div>
      )}
      {catalogLoad.retrying && <p className="text-label text-ink-400 mb-2" role="status">{t("The workspace is busy. Retrying…")}</p>}

      {tombstones.length > 0 && (
        <section className="sec" aria-label={t("Earlier catalog apps")}>
          <h2 className="text-cardtitle font-medium text-ink-100 mb-3">{t("From the retired catalog")}</h2>
          <div className="pubdir-grid">
            {tombstones.map((c) => (
              <TombstoneCard key={c.id} card={c} isOp={isOp} />
            ))}
          </div>
        </section>
      )}
    </main>
  );
}

/** Retired listings stay discoverable; Open and Restore live on the detail page. */
function TombstoneCard({ card, isOp }: { card: CatalogCard; isOp: boolean }) {
  const t = useTranslate();
  return (
    <article className="pub-card">
      <Link className="pub-card-link" href={`/apps/catalog/${encodeURIComponent(card.id)}`}
        aria-labelledby={`retired-name-${card.id}`} aria-describedby={`retired-state-${card.id}`}>
        <AppCover name={card.name} title={card.title} category={card.listing?.category} />
        <span className="pub-card-copy">
          <span className="pub-card-category">{t("Retired source")}</span>
          <span className="pub-card-title" id={`retired-name-${card.id}`}>{card.title}</span>
          <span className="pub-card-summary">{card.retired_reason ?? t("Its built-in catalog entry was retired. Update or reinstall it from the app's own repository.")}</span>
          <span className="pub-card-footer" id={`retired-state-${card.id}`}>
            <InstallStateChip state={card.removed ? "removed" : card.state} />
            <span>{t("Retired")}</span>
          </span>
          {isOp && (card.request_count ?? 0) > 0 && (
            <span className="text-micro text-ink-500">{t("{count} app requests").replace("{count}", String(card.request_count))}</span>
          )}
        </span>
      </Link>
    </article>
  );
}

/** The operator's "Add from Git URL" — a live check against the repo
 * (HTTPS, public host), then the install review: the daemon's
 * install-check shows exactly what would be installed and the install
 * sends that digest. Never a catalog row: it ships nothing to the
 * catalog list. */
function GitAdd({ onDone, onClose }: { onDone: () => void; onClose: () => void }) {
  const t = useTranslate();
  const [url, setUrl] = useState("");
  const [checking, setChecking] = useState(false);
  const [found, setFound] = useState<CatalogCard | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEscape(true, onClose);
  const check = () => {
    setChecking(true); setErr(null); setFound(null);
    void appExplorer.gitCheck(url)
      .then((card) => setFound(card))
      .catch((e: unknown) => setErr(appErrorCopy(e, "That URL doesn't hold a Cadence app.")))
      .finally(() => setChecking(false));
  };
  return (
    <div className="card p-4 mb-4" role="group" aria-label={t("Add from Git URL")}>
      <h3 className="font-medium text-ink-100 mb-2">{t("Add from Git URL")}</h3>
      <input className="field w-full" placeholder="https://github.com/your-team/app" value={url}
        onChange={(e) => { setUrl(e.target.value); setFound(null); }} aria-label={t("Git URL")} />
      <div className="flex gap-2 mt-3">
        <Button className="btn-sm btn-primary" disabled={checking || !url.startsWith("https://")} onClick={check}>
          {checking ? t("Checking…") : t("Check it")}
        </Button>
        <Button className="btn-sm" onClick={onClose}>{t("Close")}</Button>
      </div>
      {err && <div className="alert fail mt-3" role="alert">{err}</div>}
      {found && (
        <div className="mt-3">
          <div className="flex items-center gap-3 mb-2">
            <AppGlyph name={found.name} icon={found.listing?.icon} />
            <div className="min-w-0">
              <div className="font-medium text-ink-100">{found.title}</div>
              <div className="text-micro text-ink-500"><TrustChip trust="unreviewed" /> · {found.version}</div>
            </div>
          </div>
          <div className="alert warn mb-2">{t("Added from a Git URL by your team. Cadence checked its file list but hasn't reviewed what it does.")}</div>
          <InstallCheckPanel
            key={`${url}@${found.commit}`}
            source={url}
            label={found.title}
            install={(digest) => appExplorer.installSource(url, digest)}
            onInstalled={onDone}
            onClose={() => setFound(null)}
          />
        </div>
      )}
    </div>
  );
}
