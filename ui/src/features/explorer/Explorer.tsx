import { useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { useLocale, useTranslate } from "../../lib/locale";
import { localizedAppTitle, parseAppDisplayI18n } from "../workspace-apps/appLocale";
import { appExplorer, notifyAppsChanged, type CatalogCard } from "../workspace-apps/workspaceApps";
import type { Viewer } from "../projects/work";
import { AppGlyph, TrustChip, useEscape } from "./shared";
import { appErrorCopy, appLoadErrorCopy } from "./appErrors";
import { displayUiNotice, type UiNotice } from "../../lib/uiNotice";
import InstallCheckPanel from "./InstallCheckPanel";
import { Notice } from "../app-shell/shared/States";
import { useBackoffLoad } from "../workspace-apps/useBackoffLoad";
import "./explorer.css";
import { PublicDirectory } from "./publicDirectory";
import { AppTile } from "../apps/AppTile";

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

  const retiredListings = (publishedIds: ReadonlySet<string>) => {
    const cards = catalog?.filter((card) => !publishedIds.has(card.id)) ?? [];
    return cards.length > 0 && <details className="pubdir-retired">
      <summary>{t("From the retired catalog")}</summary>
      <div className="apps-grid">
        {cards.map((card) => <TombstoneCard key={card.id} card={card} isOp={isOp} />)}
      </div>
    </details>;
  };


  if (viewer.operator === null && viewer.access === "unavailable") {
    return (
      <main className="apps-explore px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={t("explore apps")}>
        <h1 className="text-section font-semibold text-ink-100 mb-4">{t("Explore apps")}</h1>
        <PublicDirectory viewer={viewer} />
        <Notice state="access-unavailable" onRetry={viewer.onRetryAccess} retryLabel={t("Retry access check")}>
          {t("Access could not be confirmed. Retry the access check before acting.")}
        </Notice>
      </main>
    );
  }

  return (
    <main className="apps-explore px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={t("explore apps")}>
      <div className="ohead flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">{t("Explore apps")}</h1>
        <span className="grow" />
        <div className="popwrap relative">
          <Button className="btn-sm" onClick={() => setGitOpen((v) => !v)} aria-expanded={gitOpen}>
            {t("More ▾")}
          </Button>
          {gitOpen && (
            <div className="pmenu" role="menu">
              {isOp ? (
                <button className="w-full text-left" onClick={() => setGitOpen(false)} data-git>
                  {t("Add from Git URL…")}<small className="block text-ink-500">{t("For custom apps your team built")}</small>
                </button>
              ) : (
                <button className="w-full text-left opacity-50" disabled>
                  {t("Add from Git URL")}<small className="block text-ink-500">{t("Admins only")}</small>
                </button>
              )}
              <Link href="/apps" className="block">{t("Your installed apps")}</Link>
            </div>
          )}
        </div>
      </div>
      <PublicDirectory viewer={viewer} afterListings={retiredListings} />

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

    </main>
  );
}

/** Retired listings stay discoverable; Open and Restore live on the detail page. */
function TombstoneCard({ card, isOp }: { card: CatalogCard; isOp: boolean }) {
  const { locale, t } = useLocale();
  const title = localizedAppTitle(
    { title: card.title, i18n: parseAppDisplayI18n(card.i18n, { wire: true }), listing: card.listing },
    locale,
    t,
  );
  return <AppTile
    name={card.name}
    icon={card.listing?.icon}
    title={title}
    summary={t(card.retired_reason || "Its built-in catalog entry was retired. Update or reinstall it from the app's own repository.")}
    version={card.version ? `v${card.version}` : ""}
    status={t(card.removed ? "Removed" : card.state === "off" ? "Access off" : card.state === "installed" ? "Installed" : "Retired")}
    tone={!card.removed && card.state === "installed" ? "" : "muted"}
    category={t("Retired source")}
    href={`/apps/catalog/${encodeURIComponent(card.id)}`}
    meta={isOp && (card.request_count ?? 0) > 0
      ? <span className="card-publisher">{t("{count} app requests").replace("{count}", String(card.request_count))}</span>
      : undefined}
  />;
}

/** The operator's "Add from Git URL" — a live check against the repo
 * (HTTPS, public host), then the install review: the daemon's
 * install-check shows exactly what would be installed and the install
 * sends that digest. Never a catalog row: it ships nothing to the
 * catalog list. */
function GitAdd({ onDone, onClose }: { onDone: () => void; onClose: () => void }) {
  const { locale, t } = useLocale();
  const [url, setUrl] = useState("");
  const [checking, setChecking] = useState(false);
  const [found, setFound] = useState<CatalogCard | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEscape(true, onClose);
  const foundTitle = found
    ? localizedAppTitle(
        { title: found.title, i18n: parseAppDisplayI18n(found.i18n, { wire: true }), listing: found.listing },
        locale,
        t,
      )
    : "";
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
              <div className="font-medium text-ink-100">{foundTitle}</div>
              <div className="text-micro text-ink-500"><TrustChip trust="unreviewed" /> · {found.version}</div>
            </div>
          </div>
          <div className="alert warn mb-2">{t("Added from a Git URL by your team. Cadence checked its file list but hasn't reviewed what it does.")}</div>
          <InstallCheckPanel
            key={`${url}@${found.commit}`}
            source={url}
            label={foundTitle}
            install={(digest) => appExplorer.installSource(url, digest)}
            onInstalled={onDone}
            onClose={() => setFound(null)}
          />
        </div>
      )}
    </div>
  );
}
