import { canOpenInstall } from "../explorer/installAccess";
import { useEffect, useMemo, useState } from "react";
import { resources } from "../../lib/resources";
import { navigate, useHref } from "../../lib/useLocation";
import { useLocale, useTranslate } from "../../lib/locale";
import { localizedAppCategory, localizedAppTagline, localizedAppTitle, parseAppDisplayI18n, type TranslateFn } from "../workspace-apps/appLocale";
import { appExplorer, notifyAppsChanged, APPS_CHANGED_EVENT, type HomeInstallation, type FavoritesPayload, type Installation } from "../workspace-apps/workspaceApps";
import { useQuery, useResource } from "../../lib/useResource";
import type { AppRow } from "../../lib/types";
import { homeNeeds, type HomeNeed } from "../home/needs";
import { appApprovalChip, appHref, appPurpose, installedAppHref, installationAppHref, runsSummary, screenInstallHref } from "./appViewModel";
import { useBackoffLoad } from "../workspace-apps/useBackoffLoad";
import { useInstallations } from "../workspace-apps/useInstallations";
import { PrivateAppDialog } from "../workspace-apps/PrivateAppDialog";
import { ResourceGate } from "../../ui/ResourceStatus";
import "./apps.css";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { appErrorCopy, appLoadErrorCopy, UNVERIFIED_APPS_COPY } from "../explorer/appErrors";
import type { Viewer } from "../projects/work";
import { AppGlyph } from "../explorer/shared";
import PageState from "../../ui/PageState";
import { IconApps, IconLock, IconSearch, IconSettings } from "../../ui/icons";
import "../explorer/explorer.css";

function homeAppTitle(h: HomeInstallation, locale: string, t: TranslateFn): string {
  return localizedAppTitle(
    { title: h.title, i18n: parseAppDisplayI18n(h.i18n, { wire: true }), listing: h.listing },
    locale,
    t,
  );
}

function homeAppTagline(h: HomeInstallation, locale: string, t: TranslateFn): string {
  return localizedAppTagline(
    { tagline: h.tagline, i18n: parseAppDisplayI18n(h.i18n, { wire: true }), listing: h.listing },
    locale,
    t,
  );
}

type FilterKey = "all" | "pinned" | "attention" | `cat:${string}`;

/**
 * The Apps home (`/apps`): a macOS Applications-style launcher with bespoke
 * app icons, Spotlight-style search & filter bar, quick-pin dock, and grid/list
 * toggle. Cards open the app itself; an operator's settings action opens Manage.
 */
export default function Apps({ viewer }: { viewer: Viewer }) {
  const { locale, t } = useLocale();
  const [revision, setRevision] = useState(0);
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<FilterKey>("all");
  const [layout, setLayout] = useState<"grid" | "list">("grid");
  const [sort, setSort] = useState<"recent" | "name" | "attention">("recent");
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [privateOpen, setPrivateOpen] = useState(false);
  const href = useHref();
  const reviewId = new URLSearchParams(href.split("?")[1] ?? "").get("review");
  const closeReview = () => {
    const url = new URL(window.location.href);
    url.searchParams.delete("review");
    navigate(url.pathname + url.search + url.hash, { replace: true });
  };

  const isOp = viewer.operator === true && !viewer.readOnly;

  useEffect(() => {
    const bump = () => setRevision((r) => r + 1);
    window.addEventListener(APPS_CHANGED_EVENT, bump);
    return () => window.removeEventListener(APPS_CHANGED_EVENT, bump);
  }, []);

  // CAD-1189: every list here loads with the shared busy backoff, keeps
  // its last good answer on a failed refresh, and never reads an error as
  // "no apps".
  const homeLoad = useBackoffLoad((signal) => appExplorer.home(signal).then((r) => r.installations), revision);
  const favLoad = useBackoffLoad((signal) => appExplorer.favorites(signal), revision);
  const home = homeLoad.data;
  const [pinned, setPinned] = useState<FavoritesPayload | null>(null);
  useEffect(() => setPinned(null), [favLoad.data]);
  const favs = pinned ?? favLoad.data;
  const rawLoadError = homeLoad.error ?? favLoad.error;
  const loadError = rawLoadError === null ? null : appLoadErrorCopy(rawLoadError, "Your apps didn't load.");
  const retrying = homeLoad.retrying || favLoad.retrying;
  const retryAll = () => { setActionError(null); setRevision((r) => r + 1); };

  // Legacy `<project>/apps/<name>` rows have a non-empty `project` key.
  // Workspace-level rows from `/api/apps` (`project === ""`) are already listed
  // in `app_home` above, so we exclude them from Project apps and borrow their
  // summary copy when `app_home` omits a tagline.
  const projectApps = useQuery(resources.apps);
  const overview = useResource(resources.overview);
  const needs = homeNeeds(overview.data?.needs_me);
  // Only the operator may read the installation list; others keep the legacy link.
  const installs = useInstallations(isOp, revision).list;
  const allApiApps = projectApps.data ?? [];
  const projectRows = useMemo(
    () => allApiApps.filter((row) => Boolean(row.project?.trim())),
    [allApiApps],
  );
  const summaryByName = useMemo(() => {
    const map = new Map<string, string>();
    for (const row of allApiApps) {
      const name = row.name?.trim();
      const summary = row.summary?.trim();
      if (name && summary && !map.has(name)) map.set(name, summary);
    }
    return map;
  }, [allApiApps]);

  const byId = useMemo(() => {
    const map = new Map<string, HomeInstallation>();
    (home ?? []).forEach((h) => map.set(h.install_id, h));
    return map;
  }, [home]);

  const live = useMemo(() => (home ?? []).filter((h) => h.attention.state !== "removed"), [home]);
  const removedList = useMemo(() => (home ?? []).filter((h) => h.attention.state === "removed"), [home]);

  const attention = useMemo(() => live.filter((h) => {
    const s = h.attention.state;
    return s === "setup" || s === "attention" || (s === "update" && isOp) || s === "off";
  }), [live, isOp]);
  const attentionIds = useMemo(() => new Set(attention.map((h) => h.install_id)), [attention]);

  const favoriteIds = useMemo(() => new Set(favs?.favorites ?? []), [favs]);
  const favorites = useMemo(() => {
    const ids = favs?.favorites ?? [];
    return ids.map((id) => byId.get(id)).filter((h): h is HomeInstallation => !!h);
  }, [favs, byId]);

  const categories = useMemo(() => {
    const set = new Set<string>();
    for (const h of live) {
      const cat = h.listing?.category?.trim();
      if (cat) set.add(cat);
    }
    return Array.from(set).sort((a, b) => a.localeCompare(b));
  }, [live]);

  // CAD-1312: a fully-loaded empty workspace drops the header's Explore
  // CTA — the state panel carries the same action once, next to its copy.
  const emptyWorkspace = home !== null && live.length === 0 && loadError === null;
  const noWorkspaceApps = live.length === 0 && removedList.length === 0;
  const noProjectApps = projectApps.data != null && projectRows.length === 0;
  const bareApps =
    noWorkspaceApps && noProjectApps && (home !== null || loadError !== null);

  const noMatchCopy = t("No installed app matches “{query}” — try another word, or look in Explore.").replace("{query}", () => q);
  const [noMatchBeforeExplore, noMatchAfterExplore] = noMatchCopy.split(/Explore|「探索」/);
  const noMatchExploreLabel = noMatchCopy.match(/Explore|「探索」/)?.[0] ?? "Explore";

  const localizedById = useMemo(() => new Map(
    (home ?? []).map((h) => {
      const title = homeAppTitle(h, locale, t);
      const rawTagline = homeAppTagline(h, locale, t).trim();
      const fallbackSummary = summaryByName.get(h.name) ?? "";
      return [h.install_id, { title, tagline: rawTagline || fallbackSummary }];
    }),
  ), [home, locale, t, summaryByName]);

  const items = useMemo(() => {
    const needle = q.trim().toLowerCase();
    let list = live.filter((h) => {
      if (filter === "pinned" && !favoriteIds.has(h.install_id)) return false;
      if (filter === "attention" && !attentionIds.has(h.install_id)) return false;
      if (filter.startsWith("cat:")) {
        const wanted = filter.slice(4);
        if ((h.listing?.category?.trim() ?? "") !== wanted) return false;
      }
      if (!needle) return true;
      const loc = localizedById.get(h.install_id);
      return [h.name, h.title, loc?.title, h.tagline, loc?.tagline, h.listing?.category]
        .filter(Boolean)
        .join(" ")
        .toLowerCase()
        .includes(needle);
    });
    if (sort === "name") {
      list = [...list].sort((a, b) => (localizedById.get(a.install_id)?.title ?? a.title).localeCompare(localizedById.get(b.install_id)?.title ?? b.title));
    }
    if (sort === "attention") {
      const rank: Record<string, number> = { attention: 0, setup: 1, update: 2, off: 3, ok: 4 };
      list = [...list].sort((a, b) => (rank[a.attention.state] ?? 4) - (rank[b.attention.state] ?? 4));
    }
    if (sort === "recent" && favs?.recent) {
      const order = new Map(favs.recent.map((id, i) => [id, i]));
      list = [...list].sort((a, b) => (order.get(a.install_id) ?? 999) - (order.get(b.install_id) ?? 999));
    }
    return list;
  }, [live, q, filter, sort, favs, favoriteIds, attentionIds, localizedById]);

  const pin = (id: string) => {
    const cur = favs?.favorites ?? [];
    const next = cur.includes(id) ? cur.filter((f) => f !== id) : [...cur, id];
    setPinned(favs ? { ...favs, favorites: next } : { favorites: next, workspace_default: [], recent: [] });
    void appExplorer.putFavorites(next).catch(() => { setPinned(null); setRevision((r) => r + 1); });
  };
  const opened = (id: string) => { void appExplorer.opened(id).catch(() => {}); };
  const restore = (id: string) => {
    setBusy(id);
    void appExplorer.restore(id)
      .then(notifyAppsChanged)
      .catch((e: unknown) => setActionError(t(appErrorCopy(e, "Restore didn't finish. Try again in a moment."))))
      .finally(() => setBusy(null));
  };

  // CAD-1438: tiles link the canonical `/apps/<key>` route.
  const openHref = (installation: HomeInstallation): string | null => {
    if (installation.attention.state === "off" || installation.attention.state === "removed") return null;
    if (installs !== null) return installationAppHref(installs, installation.install_id);
    return installedAppHref(installation.name);
  };

  const settingsHref = (h: HomeInstallation): string | null =>
    isOp ? `/apps/manage/${encodeURIComponent(h.install_id)}` : null;

  const attentionRow = (h: HomeInstallation) => {
    const s = h.attention.state;
    const label = t(s === "off" ? "Access off" : s === "setup" ? "Finish setup" : s === "update" ? "Update ready" : "Needs attention");
    const title = localizedById.get(h.install_id)?.title ?? homeAppTitle(h, locale, t);
    return (
      <div key={h.install_id} className="attn-row">
        <AppGlyph name={h.name} icon={h.icon} title={title} size="sm" />
        <span className="txt"><b>{title}</b> · {h.attention.message ? t(h.attention.message) : label}</span>
        {isOp ? (
          <Button className="btn-sm" href={(s === "off" || s === "update" ? `/apps/manage/${h.install_id}` : openHref(h)) ?? `/apps/manage/${h.install_id}`}>
            {t(s === "off" ? "Manage access" : s === "update" ? "Review update" : "Finish setup")}
          </Button>
        ) : (
          <span className="hint">{t("An admin needs to handle this")}</span>
        )}
      </div>
    );
  };

  const showProjectAppsSection =
    projectApps.data === null ||
    projectApps.error !== null ||
    projectRows.length > 0 ||
    (isOp && live.length === 0);

  return (
    <main className={`apps-home px-4 lg:px-8 min-w-0 ${bareApps ? "flex" : "pt-5 pb-10"}`} aria-label={t("apps")}>
      {bareApps ? (
        <PageState
          title={
            loadError === UNVERIFIED_APPS_COPY
              ? t("Sign in to view your apps")
              : loadError !== null
                ? t("Apps could not be loaded")
                : t("No apps installed yet")
          }
          icon={loadError === UNVERIFIED_APPS_COPY ? <IconLock size={32} /> : <IconApps size={32} />}
          actions={
            loadError !== null && loadError !== UNVERIFIED_APPS_COPY ? (
              <Button onClick={retryAll}>{t("Retry")}</Button>
            ) : (
              <>
                <Button className="btn-primary" href="/apps/explore">{t("Explore apps")}</Button>
                {isOp && loadError === null && <Button onClick={() => setPrivateOpen(true)}>{t("Add private app")}</Button>}
              </>
            )
          }
        >
          {loadError === UNVERIFIED_APPS_COPY
            ? `${t(loadError)} ${t("Use Sign in in the status bar.")}`
            : loadError !== null
              ? t(loadError)
              : isOp
                ? t("Apps add new skills to your workspace — a customer list, social posts and more. Browse what's available and install one in a tap.")
                : t("Your admin hasn't installed any apps yet. Browse what's available and ask for one.")}
        </PageState>
      ) : (
        <>
          <header className="apps-hero">
            <div className="apps-hero-copy">
              <div className="apps-hero-title-row">
                <h1 className="apps-page-title">{t("Apps")}</h1>
                {live.length > 0 && <span className="apps-count-pill num">{live.length}</span>}
              </div>
              <p className="apps-hero-sub">{t("Launch installed workspace apps, pin daily tools, or manage permissions.")}</p>
            </div>
            <div className="apps-hero-actions">
              {!emptyWorkspace && <Button className="btn-primary" href="/apps/explore">{t("Explore apps")}</Button>}
              {isOp && <Button onClick={() => setPrivateOpen(true)}>{t("Add private app")}</Button>}
            </div>
          </header>

          {actionError && <div className="alert fail mb-4" role="alert">{actionError}</div>}
          {loadError !== null && (
            <div className="apps-state card mb-4" role="alert">
              {loadError === UNVERIFIED_APPS_COPY ? (
                <span className="apps-state-icon text-ink-500" aria-hidden><IconLock size={16} /></span>
              ) : null}
              <div className="apps-state-body">
                <h2 className="apps-state-title">
                  {loadError === UNVERIFIED_APPS_COPY ? t("Sign in to view workspace apps") : t("Apps could not be loaded")}
                </h2>
                <p className="apps-state-copy">{loadError === null ? null : t(loadError)}</p>
              </div>
              <Button className="btn-sm" onClick={retryAll}>{t("Retry")}</Button>
            </div>
          )}
          {retrying && <p className="text-label text-ink-400 mb-2" role="status">{t("The workspace is busy. Retrying…")}</p>}
          {home === null ? (
            loadError === null && !retrying && <p className="text-label text-ink-400" role="status">{t("Loading your apps…")}</p>
          ) : (
            <>
              {emptyWorkspace && (
                <div className="apps-state card my-4">
                  <span className="apps-state-icon text-accent" aria-hidden><IconApps size={18} /></span>
                  <div className="apps-state-body">
                    <h2 className="apps-state-title">{t("No apps installed yet")}</h2>
                    <p className="apps-state-copy">
                      {isOp
                        ? t("Apps add new skills to your workspace — a customer list, social posts and more. Browse what's available and install one in a tap.")
                        : t("Your admin hasn't installed any apps yet. Browse what's available and ask for one.")}
                    </p>
                  </div>
                  <Button className="btn-primary" href="/apps/explore">{t("Explore apps")}</Button>
                </div>
              )}

              {attention.length > 0 && (
                <section className="attn" aria-label={t("Needs attention")}>
                  <div className="attn-head"><span aria-hidden>⚠</span>{attention.length} {t(attention.length === 1 ? "app needs attention" : "apps need attention")}</div>
                  {attention.map(attentionRow)}
                </section>
              )}

              {live.length > 0 && (
                <div className="apps-launcher-bar" role="search">
                  <div className="apps-search-wrap">
                    <span className="apps-search-ic" aria-hidden><IconSearch size={14} /></span>
                    <input
                      className="field apps-search-input"
                      type="search"
                      placeholder={t("Search your apps")}
                      value={q}
                      onChange={(e) => setQ(e.target.value)}
                      aria-label={t("Search installed apps")}
                    />
                  </div>
                  <div className="apps-filter-pills" role="group" aria-label={t("Filter apps")}>
                    <button
                      type="button"
                      className="apps-filter-pill"
                      aria-pressed={filter === "all"}
                      onClick={() => setFilter("all")}
                    >
                      {t("All")}
                    </button>
                    <button
                      type="button"
                      className="apps-filter-pill"
                      aria-pressed={filter === "pinned"}
                      onClick={() => setFilter("pinned")}
                    >
                      <span aria-hidden>★</span> {t("Favorites")}
                      {favorites.length > 0 && <span className="apps-pill-n num">{favorites.length}</span>}
                    </button>
                    {attention.length > 0 && (
                      <button
                        type="button"
                        className="apps-filter-pill"
                        aria-pressed={filter === "attention"}
                        onClick={() => setFilter("attention")}
                      >
                        {t("Needs attention")}
                        <span className="apps-pill-n num">{attention.length}</span>
                      </button>
                    )}
                    {categories.map((cat) => (
                      <button
                        key={cat}
                        type="button"
                        className="apps-filter-pill"
                        aria-pressed={filter === `cat:${cat}`}
                        onClick={() => setFilter(`cat:${cat}`)}
                      >
                        {localizedAppCategory(cat, locale)}
                      </button>
                    ))}
                  </div>
                  <div className="apps-bar-right">
                    <select className="field apps-sort" value={sort} onChange={(e) => setSort(e.target.value as typeof sort)} aria-label={t("Sort")}>
                      <option value="recent">{t("Recently used")}</option>
                      <option value="name">{t("Name")}</option>
                      <option value="attention">{t("Needs attention first")}</option>
                    </select>
                    <div className="apps-view-switch" role="group" aria-label={t("Layout")}>
                      <button
                        type="button"
                        className="apps-view-btn"
                        aria-pressed={layout === "grid"}
                        aria-label={t("Grid view")}
                        title={t("Grid view")}
                        onClick={() => setLayout("grid")}
                      >
                        <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
                          <rect x="1" y="1" width="6" height="6" rx="1.5" />
                          <rect x="9" y="1" width="6" height="6" rx="1.5" />
                          <rect x="1" y="9" width="6" height="6" rx="1.5" />
                          <rect x="9" y="9" width="6" height="6" rx="1.5" />
                        </svg>
                      </button>
                      <button
                        type="button"
                        className="apps-view-btn"
                        aria-pressed={layout === "list"}
                        aria-label={t("List view")}
                        title={t("List view")}
                        onClick={() => setLayout("list")}
                      >
                        <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
                          <rect x="1" y="2" width="14" height="2.2" rx="1" />
                          <rect x="1" y="6.9" width="14" height="2.2" rx="1" />
                          <rect x="1" y="11.8" width="14" height="2.2" rx="1" />
                        </svg>
                      </button>
                    </div>
                  </div>
                </div>
              )}

              {!emptyWorkspace && (
                <section className="sec apps-fav-sec" aria-label={t("Favorites")}>
                  <div className="apps-kicker-row">
                    <h2 className="apps-kicker">{t("Favorites")}</h2>
                    {favorites.length > 0 && <span className="apps-count-pill num">{favorites.length}</span>}
                    {live.length > 0 && (
                      <>
                        <span className="grow" />
                        <details className="favpick">
                          <summary>{t("Choose favorites")}</summary>
                          <div className="favpick-list" role="group" aria-label={t("Choose favorites")}>
                            {live.map((h) => (
                              <label key={h.install_id} className="favpick-row">
                                <input
                                  type="checkbox"
                                  checked={favoriteIds.has(h.install_id)}
                                  disabled={favs === null}
                                  onChange={() => pin(h.install_id)}
                                />
                                <span className="favpick-name">{localizedById.get(h.install_id)?.title ?? homeAppTitle(h, locale, t)}</span>
                              </label>
                            ))}
                          </div>
                        </details>
                      </>
                    )}
                  </div>
                  {favorites.length > 0 ? (
                    <div className="apps-pinned-dock">
                      {favorites.map((h) => {
                        const localized = localizedById.get(h.install_id);
                        const title = localized?.title ?? homeAppTitle(h, locale, t);
                        const openUrl = openHref(h);
                        return (
                          <div key={h.install_id} className="apps-pinned-chip">
                            {openUrl === null ? (
                              <span className="apps-pinned-open" aria-label={`${t("Open")} ${title}`}>
                                <AppGlyph name={h.name} icon={h.icon} title={title} size="sm" />
                                <span className="apps-pinned-name">{title}</span>
                              </span>
                            ) : (
                              <Link
                                href={openUrl}
                                className="apps-pinned-open"
                                onClick={() => opened(h.install_id)}
                                aria-label={`${t("Open")} ${title}`}
                              >
                                <AppGlyph name={h.name} icon={h.icon} title={title} size="sm" />
                                <span className="apps-pinned-name">{title}</span>
                              </Link>
                            )}
                            <button
                              type="button"
                              className="apps-pinned-star"
                              aria-label={`${t("Unpin")} ${title}`}
                              title={t("Unpin")}
                              onClick={() => pin(h.install_id)}
                            >
                              ★
                            </button>
                          </div>
                        );
                      })}
                    </div>
                  ) : favs === null ? (
                    <p className="hint">{t(favLoad.error ? "Favorites could not be loaded." : "Loading favorites…")}</p>
                  ) : (
                    <p className="apps-fav-inline-hint">
                      <span aria-hidden>★</span>
                      <span>{t("Pin any app with ★ or Choose favorites to keep everyday tools here and in the sidebar.")}</span>
                    </p>
                  )}
                </section>
              )}

              {live.length > 0 && (
                <section className="sec" aria-label={t("Installed")}>
                  <div className="apps-kicker-row">
                    <h2 className="apps-kicker">{t("Installed")}</h2>
                    <span className="apps-count-pill num">{items.length}</span>
                  </div>
                  {items.length === 0 ? (
                    <div className="card px-4 py-5 text-secondary text-ink-400">
                      {q.trim() ? (
                        <>
                          {noMatchBeforeExplore}<Link href="/apps/explore">{noMatchExploreLabel}</Link>{noMatchAfterExplore}
                        </>
                      ) : (
                        t("No installed apps match the current filter.")
                      )}
                    </div>
                  ) : (
                    <div className={`apps-grid ${layout === "list" ? "is-list" : ""}`}>
                      {items.map((h) => (
                        <InstalledCard
                          key={h.install_id}
                          h={h}
                          layout={layout}
                          pinned={favoriteIds.has(h.install_id)}
                          canPin={favs !== null}
                          localized={localizedById.get(h.install_id)}
                          href={openHref(h)}
                          settingsHref={settingsHref(h)}
                          onPin={() => pin(h.install_id)}
                          onOpen={() => opened(h.install_id)}
                        />
                      ))}
                    </div>
                  )}
                </section>
              )}

              {isOp && removedList.length > 0 && (
                <section className="sec" aria-label={t("Recently removed")}>
                  <div className="apps-kicker-row">
                    <h2 className="apps-kicker">{t("Recently removed")}</h2>
                    <span className="apps-count-pill num">{removedList.length}</span>
                  </div>
                  <p className="hint mb-2">{t("Removed apps restore for 30 days, then they're deleted for good.")}</p>
                  <div className="card p-0 divide-y divide-ink-800">
                    {removedList.map((h) => {
                      const title = localizedById.get(h.install_id)?.title ?? homeAppTitle(h, locale, t);
                      return (
                        <div key={h.install_id} className="rrow flex items-center gap-3 p-3">
                          <AppGlyph name={h.name} icon={h.icon} title={title} size="sm" />
                          <div className="grow">
                            <b>{title}</b>
                            <small className="block text-ink-500">
                              {t(h.attention.action === "restore" ? "Removed" : "Removed · restore window closed")}
                            </small>
                          </div>
                          {h.attention.action === "restore" && (
                            <Button className="btn-sm" disabled={busy === h.install_id} onClick={() => restore(h.install_id)}>
                              {t(busy === h.install_id ? "Restoring…" : "Restore")}
                            </Button>
                          )}
                        </div>
                      );
                    })}
                  </div>
                </section>
              )}
            </>
          )}
          {showProjectAppsSection && (
            <section className="sec" aria-label={t("Project apps")}>
              <div className="apps-kicker-row">
                <h2 className="apps-kicker">{t("Project apps")}</h2>
                {projectApps.data && <span className="apps-count-pill num">{projectRows.length}</span>}
              </div>
              <ResourceGate
                state={projectApps}
                loading={t("loading apps…")}
                failed={t("could not load apps")}
                onRetry={() => void resources.apps.invalidate()}
              />
              {projectApps.data && projectRows.length === 0 && (
                <div className="apps-substate text-label text-ink-500">
                  <p>{t("No project apps installed")}.</p>
                  <details className="apps-substate-install">
                    <summary>{t("Install a project app")}</summary>
                    <code className="num text-ink-300">
                      cadence app install &lt;path|git-url&gt; --project &lt;key&gt;
                    </code>
                  </details>
                </div>
              )}
              <ul className="space-y-2.5">
                {projectRows.map((row, i) => (
                  <AppCard key={`${row.project}/${row.name ?? i}`} row={row} installs={installs} needs={needs} />
                ))}
              </ul>
            </section>
          )}
        </>
      )}
      {reviewId !== null && !isOp && <div className="alert warn" role="status">{t("Only a current workspace owner can review this request.")}</div>}
      {reviewId !== null && isOp && <PrivateAppDialog key={reviewId} reviewId={reviewId} onClose={closeReview} onInstalled={() => setRevision((r) => r + 1)} />}
      {privateOpen && reviewId === null && <PrivateAppDialog onClose={() => setPrivateOpen(false)} onInstalled={() => setRevision((r) => r + 1)} />}
    </main>
  );
}

/**
 * One installed app's launcher tile: bespoke SVG icon, title, real summary,
 * and version/status footer. The card opens the app; quick-pin and operator
 * settings sit outside the main link so cards stay single-link and keyboard-clean.
 */
function InstalledCard({
  h,
  layout,
  pinned,
  canPin,
  localized,
  href,
  settingsHref,
  onPin,
  onOpen,
}: {
  h: HomeInstallation;
  layout: "grid" | "list";
  pinned: boolean;
  canPin: boolean;
  localized?: { title: string; tagline: string };
  href: string | null;
  settingsHref: string | null;
  onPin: () => void;
  onOpen: () => void;
}) {
  const { locale, t } = useLocale();
  const state = h.attention.state;
  const statusKey =
    state === "off" ? "Access off"
    : state === "setup" ? "Finish setup"
    : state === "update" ? "Update ready"
    : state === "attention" ? "Needs attention"
    : "Installed";
  const tone = state === "update" ? "info" : state === "setup" || state === "attention" ? "warn" : "";
  const nameId = `app-card-name-${h.install_id}`;
  const statusId = `app-card-status-${h.install_id}`;
  const listing = h.listing;
  const category = listing?.category;
  const title = localized?.title ?? homeAppTitle(h, locale, t);
  const summary = localized?.tagline ?? homeAppTagline(h, locale, t);
  const publisher = listing?.publisher?.name;
  const body = (
    <>
      <div className="app-tile-icon">
        <AppGlyph name={h.name} icon={h.icon} title={title} size={layout === "list" ? "sm" : "xl"} />
      </div>
      <div className="app-card-copy">
        <div className="app-card-title-group">
          <h3 className="app-card-name" id={nameId}>{title}</h3>
          {category && <span className="card-category">{localizedAppCategory(category, locale)}</span>}
        </div>
        {summary && <p className="app-card-summary">{summary}</p>}
        {publisher && <span className="card-publisher">{publisher}</span>}
        {h.project && <span className="card-publisher">{t("Project")}: {h.project}</span>}
        <div className="card-footer">
          <span className="num">{h.version ? `v${h.version}` : t("Installed")}</span>
          <span className="app-status" id={statusId} data-state={tone}>{t(statusKey)}</span>
        </div>
      </div>
    </>
  );
  return (
    <article className={`card app-card ${pinned ? "is-pinned" : ""}`}>
      {href === null ? (
        <div className="app-card-link">
          {body}
          <div className="app-card-copy-note">{t("No unique app page — see Manage")}</div>
        </div>
      ) : (
        <Link
          className="app-card-link"
          href={href}
          aria-labelledby={nameId}
          aria-describedby={statusId}
          onClick={onOpen}
        >
          {body}
        </Link>
      )}
      <div className="app-card-actions">
        <button
          type="button"
          className={`app-card-action-btn ${pinned ? "is-pinned" : ""}`}
          disabled={!canPin}
          aria-pressed={pinned}
          aria-label={`${t(pinned ? "Unpin" : "Pin")} ${title}`}
          title={t(pinned ? "Unpin" : "Pin")}
          onClick={onPin}
        >
          {pinned ? "★" : "☆"}
        </button>
        {settingsHref !== null && (
          <Link
            className="app-card-settings"
            href={settingsHref}
            aria-label={`${t("Settings")} — ${title}`}
            title={t("Settings")}
          >
            <IconSettings size={14} />
          </Link>
        )}
      </div>
    </article>
  );
}

function AppCard({
  row,
  installs,
  needs,
}: {
  row: AppRow;
  installs: Installation[] | null;
  needs: HomeNeed[];
}) {
  const t = useTranslate();
  const approval = appApprovalChip(row, t);
  const title = row.title?.trim() || row.name || t("App");
  const appName = row.name;
  const legacyHref = appName ? appHref(row.project, appName) : null;
  const screenHref = appName ? screenInstallHref(installs, row.project, appName) : null;
  const sameName = appName ? (installs ?? []).filter((i) => i.name === appName) : [];
  const blocked = sameName.length > 0 && sameName.every((i) => !canOpenInstall(i.state, i.removed));
  const href = blocked ? null : (screenHref ?? legacyHref);
  return (
    <li className="card px-3.5 py-3 min-w-0" data-app={row.name ?? undefined}>
      <div className="app-card-layout">
        <AppGlyph name={appName ?? title} title={title} size="sm" />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2 min-w-0">
            {href ? (
              <Link href={href} className="min-w-0 break-words hover:text-accent">
                <span className="text-cardtitle font-medium text-ink-100">{title}</span>
              </Link>
            ) : (
              <span className="text-cardtitle font-medium text-ink-100">{title}</span>
            )}
            <span className={`chip shrink-0 ${approval.cls}`}>{approval.text}</span>
          </div>
          {row.project && (
            <p className="text-micro text-ink-500 mt-0.5 break-words">{t("Project")}: {row.project}</p>
          )}
          <p className="text-label text-ink-400 mt-0.5 break-words">{appPurpose(row)}</p>
          <p className="text-micro text-ink-500 mt-1" data-summary>
            {row.error ? (
              row.error
            ) : row.project && appName ? (
              <AppActivity project={row.project} name={appName} needs={needs} />
            ) : (
              t("activity unavailable")
            )}
          </p>
        </div>
        {href && <Button href={href} className="app-card-action" aria-label={`${t("Open")} ${title} ${t("in")} ${row.project}`}>{t("Open app")}</Button>}
        {screenHref && legacyHref && <Link href={legacyHref} className="text-micro text-ink-500 hover:text-accent">{t("Legacy page")}</Link>}
      </div>
    </li>
  );
}

function AppActivity({
  project,
  name,
  needs,
}: {
  project: string;
  name: string;
  needs: HomeNeed[];
}) {
  const t = useTranslate();
  const runs = useQuery(resources.appRuns(`${project}/${name}`));
  if (runs.status === "failed") return <>{t("activity unavailable")}</>;
  if (!runs.data) return <>{t("reading activity…")}</>;
  return <>{runsSummary(runs.data, needs, t) ?? t("Nothing running yet")}</>;
}
