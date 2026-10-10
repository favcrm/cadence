import { useEffect, useMemo, useState } from "react";
import { resources } from "../../lib/resources";
import { ApiError } from "../../lib/api";
import { useLocale } from "../../lib/locale";
import { prefetchWorkspaceApp, readInstallationsFresh, readInstallationsSnapshot, dropInstallationsSnapshot, appExplorer, notifyAppsChanged, type HomeInstallation, type FavoritesPayload, type Installation } from "../workspace-apps/workspaceApps";
import { useQuery, useResource } from "../../lib/useResource";
import type { AppRow } from "../../lib/types";
import { homeNeeds, type HomeNeed } from "../home/needs";
import { appApprovalChip, appHref, appPurpose, installedAppHref, runsSummary, screenInstallHref } from "./appViewModel";
import { useBackoffLoad } from "../workspace-apps/useBackoffLoad";
import { useInstallations } from "../workspace-apps/useInstallations";
import { ResourceGate } from "../../ui/ResourceStatus";
import "./apps.css";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { appErrorCopy, appLoadErrorCopy, UNVERIFIED_APPS_COPY } from "../explorer/appErrors";
import type { Viewer } from "../projects/work";
import { AppGlyph } from "../explorer/shared";
import PageState from "../../ui/PageState";
import { IconApps, IconLock } from "../../ui/icons";
import "../explorer/explorer.css";

/**
 * The Apps home (CAD-1129, `/apps`): the OS-style landing — a Needs-
 * attention rail, the operator's/member's favorites row, the installed
 * grid and (operator) Recently removed. "Open" opens the app's own
 * page (`/app-installations/<id>`); "Manage" opens its operator tab.
 *
 * The legacy project-scoped list and the per-project catalog live
 * behind `/apps/<project>/<name>` (unchanged); this screen is the
 * workspace-level home the ticket describes.
 */
export default function Apps({ project, viewer }: { project: string; viewer: Viewer }) {
  const { t } = useLocale();
  const [revision, setRevision] = useState(0);
  const [q, setQ] = useState("");
  const [sort, setSort] = useState<"recent" | "name" | "attention">("recent");
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const isOp = viewer.operator === true && !viewer.readOnly;

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
  const projectRows = (projectApps.data ?? []).filter((r) => project === "all" || r.project === project);

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

  const noMatchCopy = t("No installed app matches “{query}” — try another word, or look in Explore.").replace("{query}", q);
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
      .then(() => { setRevision((r) => r + 1); notifyAppsChanged(); })
      .catch((e: unknown) => setActionError(t(appErrorCopy(e, "Restore didn't finish. Try again in a moment."))))
      .finally(() => setBusy(null));
  };

  const openHref = (installation: HomeInstallation) => {
    const sameKey = live.filter((item) => item.name === installation.name);
    const stableHref = `/app-installations/${encodeURIComponent(installation.install_id)}`;
    if (sameKey.length !== 1) return stableHref;
    return installedAppHref(installation.name) ?? stableHref;
  };

  const attentionRow = (h: HomeInstallation) => {
    const s = h.attention.state;
    const label = t(s === "off" ? "Access off" : s === "setup" ? "Finish setup" : s === "update" ? "Update ready" : "Needs attention");
    return (
      <div key={h.install_id} className="attn-row">
        <AppGlyph name={h.name} icon={h.icon} size="sm" />
        <span className="txt"><b>{h.title}</b> · {h.attention.message ?? label}</span>
        {isOp ? (
          <Button className="btn-sm" href={s === "off" || s === "update" ? `/apps/manage/${h.install_id}` : openHref(h)}>
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

      {isOp && <WorkspaceCatalog />}
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
            </div>
            {favorites.length > 0 ? (
              <div className="favs">
                {favorites.map((h) => (
                  <div key={h.install_id} className="card fav">
                    <Link href={openHref(h)} className="open"
                      onClick={() => opened(h.install_id)} aria-label={`${t("Open")} ${h.title}`}>
                      <AppGlyph name={h.name} icon={h.icon} />
                      <span className="nm">{h.title}</span>
                      <span className="ln">{h.tagline}</span>
                    </Link>
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
                <span>{t("Pin the apps you use every day. Tap the star on any app below and it appears here and in the sidebar.")}</span>
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
                <input className="field search" placeholder={t("Search your apps")} value={q} onChange={(e) => setQ(e.target.value)} aria-label={t("Search installed apps")} />
                <select className="field sort" value={sort} onChange={(e) => setSort(e.target.value as typeof sort)} aria-label={t("Sort")}>
                  <option value="recent">{t("Recently used")}</option>
                  <option value="name">{t("Name")}</option>
                  <option value="attention">{t("Needs attention first")}</option>
                </select>
              </div>
              {items.length === 0 ? (
                <div className="card px-4 py-5 text-secondary text-ink-400">
                  {noMatchBeforeExplore}<Link href="/apps/explore">{noMatchExploreLabel}</Link>{noMatchAfterExplore}
                </div>
              ) : (
                <div className="igrid">
                  {items.map((h) => {
                    const pinned = favs?.favorites.includes(h.install_id) ?? false;
                    return (
                      <div key={h.install_id} className="card itile">
                        <Link className="open" href={openHref(h)}
                          onClick={() => opened(h.install_id)} aria-label={`Open ${h.title}`}>
                          <AppGlyph name={h.name} icon={h.icon} size="sm" />
                          <div className="min-w-0 flex-1">
                            <div className="nm">{h.title}</div>
                            <div className="ln">{h.tagline}</div>
                            {h.project && <div className="ln text-ink-500">{t("Project")}: {h.project}</div>}
                            {h.attention.state !== "ok" && (
                              <div className="mt-1"><span className={`chip ${h.attention.state === "update" ? "info" : h.attention.state === "off" ? "" : "warn"}`}>
                                {t(h.attention.state === "update" ? "Update" : h.attention.state === "off" ? "Access off" : h.attention.state === "setup" ? "Finish setup" : "Needs attention")}
                              </span></div>
                            )}
                          </div>
                        </Link>
                        <div className="acts">
                          <button className={`star ${pinned ? "pinned" : ""}`} aria-pressed={pinned}
                            aria-label={`${t(pinned ? "Unpin" : "Pin")} ${h.title}`}
                            onClick={() => pin(h.install_id)}>{pinned ? "★" : "☆"}</button>
                          {isOp && <Link className="star" href={`/apps/manage/${h.install_id}`} aria-label={`Manage ${h.title}`}>⚙</Link>}
                        </div>
                      </div>
                    );
                  })}
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
            <p>{t("No project apps installed")}{project === "all" ? "" : ` ${t("in")} ${project}`}.</p>
            <details className="apps-substate-install">
              <summary>{t("Install a project app")}</summary>
              <code className="num text-ink-300">
                cadence app install &lt;path|git-url&gt; --project {project === "all" ? "<key>" : project}
              </code>
            </details>
          </div>
        )}
        <ul className="space-y-2.5">
          {projectRows.map((row, i) => (
            <AppCard key={`${row.project}/${row.name ?? i}`} row={row} installs={installs} needs={needs} showProject={project === "all"} />
          ))}
        </ul>
      </section>
        </>
      )}
    </main>
  );
}

/** Workspace installations remain visible independently of the legacy project filter. */
function WorkspaceCatalog() {
  // CAD-1137: a revisit paints this session's last list at once and
  // refetches behind it; only the very first read shows the load line.
  const [rows, setRows] = useState<Installation[] | null>(() =>
    readInstallationsSnapshot()?.filter(row => row.storage_kind === "workspace") ?? null,
  );
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    const cached = readInstallationsSnapshot()?.filter(row => row.storage_kind === "workspace") ?? null;
    setRows(cached);
    setError(null);
    void readInstallationsFresh(controller.signal).then(value => {
      if (!controller.signal.aborted) setRows(value.filter(row => row.storage_kind === "workspace"));
    }).catch((cause: unknown) => {
      if (controller.signal.aborted) return;
      if (cause instanceof ApiError && [401, 403].includes(cause.status)) {
        dropInstallationsSnapshot();
        setRows(null);
      }
      setError(cause instanceof Error ? cause.message : "Could not load workspace apps");
    });
    return () => controller.abort();
  }, [revision]);
  return <section aria-label="Workspace apps" className="mb-5">
    <h2 className="text-cardtitle font-medium text-ink-100 mb-2">Workspace apps</h2>
    {error && <div className="card px-4 py-3 text-label text-ink-400" role="alert">
      {error} <Button onClick={() => setRevision(value => value + 1)}>Retry</Button>
    </div>}
    {rows === null ? error ? null : <p className="text-label text-ink-400" role="status">Loading workspace apps…</p> : rows.length === 0 ? <div className="card px-4 py-3 text-label text-ink-400">
      No workspace apps installed. Install a package with <code>cadence app catalog install &lt;path|git-url&gt;</code>.
    </div> : <ul className="space-y-2.5">{rows.map(row => <li key={row.install_id} className="card px-3.5 py-3 min-w-0"
      onMouseEnter={() => prefetchWorkspaceApp(row.install_id)} onFocus={() => prefetchWorkspaceApp(row.install_id)}>
      <div className="app-card-layout">
        <AppIcon label={row.title || row.name} />
        <div className="min-w-0 flex-1">
          <Link href={`/app-installations/${encodeURIComponent(row.install_id)}`} className="text-cardtitle font-medium text-ink-100 hover:text-accent">{row.title || row.name}</Link>
          {row.approved !== true && <span className="chip ml-2">Access off</span>}
          <p className="text-label text-ink-400 mt-0.5 break-words">{row.summary}</p>
        </div>
        <Button href={`/app-installations/${encodeURIComponent(row.install_id)}`} className="app-card-action">Open app</Button>
      </div>
    </li>)}</ul>}
  </section>;
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
  showProject,
}: {
  row: AppRow;
  installs: Installation[] | null;
  needs: HomeNeed[];
  showProject: boolean;
}) {
  const { t } = useLocale();
  const approval = appApprovalChip(row);
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
          {showProject && <p className="text-micro text-ink-500 mt-0.5 break-words">{t("Project")}: {row.project}</p>}
          <p className="text-label text-ink-400 mt-0.5 break-words">{appPurpose(row)}</p>
          <p className="text-micro text-ink-500 mt-1" data-summary>
            {row.error ? (
              row.error
            ) : appName ? (
              <AppActivity project={row.project} name={appName} needs={needs} />
            ) : (
              "activity unavailable"
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
  const { t } = useLocale();
  const runs = useQuery(resources.appRuns(`${project}/${name}`));
  if (runs.status === "failed") return <>{t("activity unavailable")}</>;
  if (!runs.data) return <>{t("reading activity…")}</>;
  return <>{runsSummary(runs.data, needs) ?? t("Nothing running yet")}</>;
}
