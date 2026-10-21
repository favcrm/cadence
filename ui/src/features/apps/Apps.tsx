import { useEffect, useMemo, useState } from "react";
import { resources } from "../../lib/resources";
import { useTranslate } from "../../lib/locale";
import { appExplorer, notifyAppsChanged, APPS_CHANGED_EVENT, type HomeInstallation, type FavoritesPayload, type Installation } from "../workspace-apps/workspaceApps";
import { useQuery, useResource } from "../../lib/useResource";
import type { AppRow } from "../../lib/types";
import { homeNeeds, type HomeNeed } from "../home/needs";
import { appApprovalChip, appHref, appPurpose, installedAppHref, installationAppHref, runsSummary, screenInstallHref } from "./appViewModel";
import { useBackoffLoad } from "../workspace-apps/useBackoffLoad";
import { useInstallations } from "../workspace-apps/useInstallations";
import { ResourceGate } from "../../ui/ResourceStatus";
import "./apps.css";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { appErrorCopy, appLoadErrorCopy, UNVERIFIED_APPS_COPY } from "../explorer/appErrors";
import type { Viewer } from "../projects/work";
import { AppGlyph } from "../explorer/shared";
import AppCover from "../explorer/AppCover";
import PageState from "../../ui/PageState";
import { IconApps, IconLock, IconSettings } from "../../ui/icons";
import "../explorer/explorer.css";

/**
 * The Apps home (CAD-1129, `/apps`): the OS-style landing — a Needs-
 * attention rail, the operator's/member's favorites row, the installed
 * grid and (operator) Recently removed. Cards open the app itself;
 * an operator's small settings action opens its Manage page.
 *
 * The legacy project-scoped list and the per-project catalog live
 * behind `/apps/<project>/<name>` (unchanged); this screen is the
 * workspace-level home the ticket describes.
 */
export default function Apps({ viewer }: { viewer: Viewer }) {
  const t = useTranslate();
  const [revision, setRevision] = useState(0);
  const [q, setQ] = useState("");
  const [sort, setSort] = useState<"recent" | "name" | "attention">("recent");
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

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

  // Project apps (the legacy `<project>/apps/<name>` installs) keep their
  // own section below: they are not workspace installs, so the home above
  // does not list them.
  const projectApps = useQuery(resources.apps);
  const overview = useResource(resources.overview);
  const needs = homeNeeds(overview.data?.needs_me);
  // Only the operator may read the installation list; others keep the legacy link.
  const installs = useInstallations(isOp, revision).list;
  const projectRows = projectApps.data ?? [];

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

  const favorites = useMemo(() => {
    const ids = favs?.favorites ?? [];
    return ids.map((id) => byId.get(id)).filter((h): h is HomeInstallation => !!h);
  }, [favs, byId]);

  // CAD-1312: a fully-loaded empty workspace drops the header's Explore
  // CTA — the state panel carries the same action once, next to its copy.
  const emptyWorkspace = home !== null && live.length === 0 && loadError === null;
  // When nothing exists on either list — a confirmed-empty project-apps
  // answer and no workspace apps (live or recently removed) — the screen
  // becomes one full-page state instead of stacked empty sections. The
  // sign-in refusal and a load failure get their own honest variants; a
  // partially loaded screen (stale rows, removed apps to restore, project
  // rows, a project list still loading or failed) keeps normal sections.
  const noWorkspaceApps = live.length === 0 && removedList.length === 0;
  const noProjectApps = projectApps.data != null && projectRows.length === 0;
  const bareApps =
    noWorkspaceApps && noProjectApps && (home !== null || loadError !== null);

  const noMatchCopy = t("No installed app matches “{query}” — try another word, or look in Explore.").replace("{query}", () => q);
  const [noMatchBeforeExplore, noMatchAfterExplore] = noMatchCopy.split(/Explore|「探索」/);
  const noMatchExploreLabel = noMatchCopy.match(/Explore|「探索」/)?.[0] ?? "Explore";

  const items = useMemo(() => {
    const needle = q.trim().toLowerCase();
    let list = live.filter((h) => !needle || [h.name, h.title, h.tagline].filter(Boolean).join(" ").toLowerCase().includes(needle));
    if (sort === "name") list = [...list].sort((a, b) => a.title.localeCompare(b.title));
    if (sort === "attention") {
      const rank: Record<string, number> = { attention: 0, setup: 1, update: 2, off: 3, ok: 4 };
      list = [...list].sort((a, b) => (rank[a.attention.state] ?? 4) - (rank[b.attention.state] ?? 4));
    }
    if (sort === "recent" && favs?.recent) {
      const order = new Map(favs.recent.map((id, i) => [id, i]));
      list = [...list].sort((a, b) => (order.get(a.install_id) ?? 999) - (order.get(b.install_id) ?? 999));
    }
    return list;
  }, [live, q, sort, favs]);

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

  // CAD-1438: tiles link the canonical `/apps/<key>` route. For the
  // operator the verified install list decides uniqueness before the
  // link is emitted; for members (no list read) the route itself proves
  // uniqueness — an ambiguous or unsafe key renders no link.
  const openHref = (installation: HomeInstallation): string | null => {
    if (installs !== null) return installationAppHref(installs, installation.install_id);
    return installedAppHref(installation.name);
  };

  /**
   * An installed card opens the app itself, not its store page: the
   * canonical `/apps/<key>` route is the one destination for everyone.
   * The operator's settings action — access, updates, remove — lives
   * beside it on `/apps/manage/<install_id>`; members have no manage
   * page, so they get no icon.
   */
  const settingsHref = (h: HomeInstallation): string | null =>
    isOp ? `/apps/manage/${encodeURIComponent(h.install_id)}` : null;

  const attentionRow = (h: HomeInstallation) => {
    const s = h.attention.state;
    const label = t(s === "off" ? "Access off" : s === "setup" ? "Finish setup" : s === "update" ? "Update ready" : "Needs attention");
    return (
      <div key={h.install_id} className="attn-row">
        <AppGlyph name={h.name} icon={h.icon} size="sm" />
        <span className="txt"><b>{h.title}</b> · {h.attention.message ?? label}</span>
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

  return (
    <main className={`apps-home px-4 lg:px-8 min-w-0 ${bareApps ? "flex" : "pt-4 pb-9"}`} aria-label={t("apps")}>
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
              <Button className="btn-primary" href="/apps/explore">{t("Explore apps")}</Button>
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
      <div className="ohead flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">{t("Apps")}</h1>
        <span className="grow" />
        {!emptyWorkspace && <Button className="btn-primary" href="/apps/explore">{t("Explore apps")}</Button>}
      </div>

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

          {!emptyWorkspace && (
          <section className="sec" aria-label={t("Favorites")}>
            <div className="sechead">
              <h2>{t("Favorites")}</h2>
              {live.length > 0 && (
                <>
                  <span className="grow" />
                  <details className="favpick">
                    <summary>{t("Choose favorites")}</summary>
                    <div className="favpick-list" role="group" aria-label={t("Choose favorites")}>
                      {live.map((h) => (
                        <label key={h.install_id} className="favpick-row">
                          <input type="checkbox"
                            checked={favs?.favorites.includes(h.install_id) ?? false}
                            disabled={favs === null}
                            onChange={() => pin(h.install_id)} />
                          <span className="favpick-name">{h.title}</span>
                        </label>
                      ))}
                    </div>
                  </details>
                </>
              )}
            </div>
            {favorites.length > 0 ? (
              <div className="favs">
                {favorites.map((h) => (
                  <div key={h.install_id} className="card fav">
                    {(() => {
                      const href = openHref(h);
                      return href === null ? (
                        <span className="open" aria-label={`${t("Open")} ${h.title}`}>
                          <AppGlyph name={h.name} icon={h.icon} />
                          <span className="nm">{h.title}</span>
                          <span className="ln">{t("No unique app page — see Manage")}</span>
                        </span>
                      ) : (
                        <Link href={href} className="open"
                          onClick={() => opened(h.install_id)} aria-label={`${t("Open")} ${h.title}`}>
                          <AppGlyph name={h.name} icon={h.icon} />
                          <span className="nm">{h.title}</span>
                          <span className="ln">{h.tagline}</span>
                        </Link>
                      );
                    })()}
                    <button className="star pinned absolute top-2 right-2" aria-label={`${t("Unpin")} ${h.title}`}
                      onClick={() => pin(h.install_id)}>★</button>
                  </div>
                ))}
              </div>
            ) : favs === null ? (
              <p className="hint">{t(favLoad.error ? "Favorites could not be loaded." : "Loading favorites…")}</p>
            ) : (
              <div className="favempty">
                <span aria-hidden>★</span>
                <span>{t("Use Choose favorites to keep everyday apps here and in the sidebar.")}</span>
              </div>
            )}
          </section>
          )}

          {live.length > 0 && (
            <section className="sec" aria-label={t("Installed")}>
              <div className="sechead">
                <h2>{t("Installed")}</h2>
                <span className="chip">{live.length}</span>
                <span className="grow" />
                <div className="sec-tools">
                  <input className="field search" placeholder={t("Search your apps")} value={q} onChange={(e) => setQ(e.target.value)} aria-label={t("Search installed apps")} />
                  <select className="field sort" value={sort} onChange={(e) => setSort(e.target.value as typeof sort)} aria-label={t("Sort")}>
                    <option value="recent">{t("Recently used")}</option>
                    <option value="name">{t("Name")}</option>
                    <option value="attention">{t("Needs attention first")}</option>
                  </select>
                </div>
              </div>
              {items.length === 0 ? (
                <div className="card px-4 py-5 text-secondary text-ink-400">
                  {noMatchBeforeExplore}<Link href="/apps/explore">{noMatchExploreLabel}</Link>{noMatchAfterExplore}
                </div>
              ) : (
                <div className="apps-grid">
                  {items.map((h) => (
                    <InstalledCard key={h.install_id} h={h}
                      href={openHref(h)}
                      settingsHref={settingsHref(h)}
                      onOpen={() => opened(h.install_id)} />
                  ))}
                </div>
              )}
              <Link href="/apps/explore" className="explore-card mt-3" aria-label={t("Find more apps")}>
                <span aria-hidden>⌖</span>
                <span className="flex-1"><b>{t("Find more apps")}</b><small className="block">{t("Bookings, reviews, invoices and more, made for small businesses.")}</small></span>
                <span aria-hidden>→</span>
              </Link>
            </section>
          )}

          {isOp && removedList.length > 0 && (
            <section className="sec" aria-label={t("Recently removed")}>
              <div className="sechead"><h2>{t("Recently removed")}</h2><span className="chip">{removedList.length}</span></div>
              <p className="hint mb-2">{t("Removed apps restore for 30 days, then they're deleted for good.")}</p>
              <div className="card p-0 divide-y divide-ink-800">
                {removedList.map((h) => (
                  <div key={h.install_id} className="rrow flex items-center gap-3 p-3">
                    <AppGlyph name={h.name} icon={h.icon} size="sm" />
                    <div className="grow"><b>{h.title}</b><small className="block text-ink-500">{t(h.attention.action === "restore" ? "Removed" : "Removed · restore window closed")}</small></div>
                    {h.attention.action === "restore" && (
                      <Button className="btn-sm" disabled={busy === h.install_id} onClick={() => restore(h.install_id)}>
                        {t(busy === h.install_id ? "Restoring…" : "Restore")}
                      </Button>
                    )}
                  </div>
                ))}
              </div>
            </section>
          )}
        </>
      )}
      {(projectApps.data === null || projectApps.error !== null || projectRows.length > 0 || isOp) && (
        <section className="sec" aria-label={t("Project apps")}>
          <div className="sechead"><h2>{t("Project apps")}</h2>{projectApps.data && <span className="chip">{projectRows.length}</span>}</div>
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
    </main>
  );
}
/**
 * One installed app's gallery card: the card opens the app, and the
 * operator's small settings action opens its Manage page. Catalogue/
 * listing fields come from the real `app_home` row — absent fields
 * leave clean slots, never fixture text.
 */
function InstalledCard({ h, href, settingsHref, onOpen }: {
  h: HomeInstallation;
  href: string | null;
  settingsHref: string | null;
  onOpen: () => void;
}) {
  const t = useTranslate();
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
  const summary = listing?.tagline ?? h.tagline;
  const publisher = listing?.publisher?.name;
  const body = (
    <>
      <AppCover name={h.name} title={h.title} category={category ?? ""} />
      <div className="app-card-copy">
        {category && <span className="card-category">{category}</span>}
        <h3 className="app-card-name" id={nameId}>{h.title}</h3>
        {summary && <p className="app-card-summary">{summary}</p>}
        {publisher && <span className="card-publisher">{publisher}</span>}
        {h.project && <span className="card-publisher">{t("Project")}: {h.project}</span>}
        <div className="card-footer">
          <span>{h.version ? <>{t("Version")} <b>{h.version}</b></> : t("Installed")}</span>
          <span className="app-status" id={statusId} data-state={tone}>{t(statusKey)}</span>
        </div>
      </div>
    </>
  );
  return (
    <article className="card app-card">
      {href === null ? (
        <div className="app-card-link">
          {body}
          <div className="app-card-copy-note">{t("No unique app page — see Manage")}</div>
        </div>
      ) : (
        <Link className="app-card-link" href={href}
          aria-labelledby={nameId} aria-describedby={statusId}
          onClick={onOpen}>
          {body}
        </Link>
      )}
      {settingsHref !== null && (
        <Link className="app-card-settings" href={settingsHref}
          aria-label={`${t("Settings")} — ${h.title}`} title={t("Settings")}>
          <IconSettings size={15} />
        </Link>
      )}
    </article>
  );
}

/** A monogram tile — the app's first letter, on the board's accent. */
function AppIcon({ label }: { label: string }) {
  const letter = (label.trim()[0] ?? "A").toUpperCase();
  return (
    <span
      aria-hidden
      className="shrink-0 w-9 h-9 rounded-lg bg-accent/15 text-accent grid place-items-center text-cardtitle font-semibold"
    >
      {letter}
    </span>
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
  const href = screenHref ?? legacyHref;
  return (
    <li className="card px-3.5 py-3 min-w-0" data-app={row.name ?? undefined}>
      <div className="app-card-layout">
        <AppIcon label={title} />
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
          <p className="text-micro text-ink-500 mt-0.5 break-words">{t("Project")}: {row.project}</p>
          <p className="text-label text-ink-400 mt-0.5 break-words">{appPurpose(row)}</p>
          <p className="text-micro text-ink-500 mt-1" data-summary>
            {row.error ? (
              row.error
            ) : appName ? (
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

/**
 * The card's live line: the app's runs — the same shared store the app
 * page uses. Its own component so the runs hook is called
 * unconditionally, for the rows that have an app to read (CAD-571 N2).
 */
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
