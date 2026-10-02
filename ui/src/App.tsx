import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, type WriteResp } from "./lib/api";
import Agents from "./features/agents/Agents";
import Apps from "./features/apps/Apps";
import AppDetail from "./features/apps/AppDetail";
import AppShell, { type ActiveInstallation } from "./features/app-shell/AppShell";
import { crmAppMenu, type CrmSection } from "./features/app-shell/CrmOutlet";
import WorkspaceApp from "./features/workspace-apps/WorkspaceApp";
import Board from "./features/projects/Board";
import Drawer from "./features/projects/Drawer";
import ProjectsOverview from "./features/projects/ProjectsOverview";
import IssuePage from "./features/issues/IssuePage";
import { issuePath, laneFollowsStream, refreshIssueIds } from "./features/issues/model";
import Epics from "./features/projects/Epics";
import Milestones from "./features/projects/Milestones";
import MasterPermissions from "./features/settings/MasterPermissions";
import Memory from "./features/settings/Memory";
import ModelDefaults from "./features/settings/ModelDefaults";
import PlatformAccount from "./features/settings/PlatformAccount";
import Connections from "./features/settings/Connections";
import Update from "./features/settings/Update";
import Outbox from "./features/outbox/Outbox";
import OverviewView from "./features/home/Overview";
import Home from "./features/home/Home";
import Context from "./features/projects/Context";
import { contextNavigationSearch } from "./features/projects/contextRoute";
import Workflows from "./features/projects/Workflows";
import Sidebar from "./ui/Sidebar";
import { NavList, ProjectList } from "./ui/NavList";
import Wiki from "./features/wiki/Wiki";
import Link from "./ui/Link";
import ProjectFilter from "./ui/ProjectFilter";
import SectionTabs from "./ui/SectionTabs";
import StatusChips from "./ui/StatusChips";
import ThemeToggle from "./ui/ThemeToggle";
import BuildUpdateNotice from "./ui/BuildUpdateNotice";
import Setup from "./features/setup/Setup";
import SetupNudge from "./features/setup/SetupNudge";
import Login from "./features/auth/Login";
import SignIn from "./features/auth/SignIn";
import { kickoffBlock as operatorKickoffBlock, writeBlock } from "./features/auth/gate";
import { WriteGate } from "./features/auth/WriteGate";
import { sessionKey, setSessionKey } from "./lib/sessionKey";
import { buildChanged, serverBuild, subscribeSse, UI_BUILD } from "./lib/sse";
import { applyDraft, composerField, sessionStore, stashDraft, takeDraft } from "./lib/draft";
import Toast, { type ToastMsg } from "./ui/Toast";
import { IconList } from "./ui/icons";
import type { BoardFilters } from "./lib/filters";
import type { UpdateBanner } from "./lib/types";
import { RESOURCE_NAMES } from "./lib/cache";
import { LiveUpdates, patchRows } from "./lib/liveUpdates";
import type { Agent, AgentsPayload } from "./lib/types";
import { cache, resources } from "./lib/resources";
import { useMaybeResource, useResource } from "./lib/useResource";
import {
  goTo,
  locationHref,
  openProject,
  projectScope,
  readLocation,
  showProjectChoices,
  withProject,
  type AppLocation,
  type Route,
  type Screen,
} from "./lib/router";
import { navigate, useHref } from "./lib/useLocation";
import { requestIsCurrent, responseBelongsToRequest, visibleContext } from "./features/projects/projectContextGuard";
import {
  browserStoredProjectView,
  persistBrowserProjectView,
  type ProjectView,
} from "./lib/urlState";
import type { Health, IssueCard, Meta, ProjectContext } from "./lib/types";

/** The location as the app reads it — the URL is the only copy. */
function currentLocation(): AppLocation {
  return readLocation(location.pathname, location.search, browserStoredProjectView());
}

/** Move to a new location built from the current one. */
function update(fn: (current: AppLocation) => AppLocation, opts?: { replace?: boolean }): void {
  const current = currentLocation();
  const next = fn(current);
  navigate(locationHref(next, contextNavigationSearch(current.route, next.route, location.search)), opts);
}

const SCREEN_LABEL: Record<Screen, string> = {
  home: "home",
  overview: "overview",
  projects: "projects",
  apps: "apps",
  workspaceApp: "app",
  agents: "agents",
  wiki: "wiki",
  outbox: "outbox",
  setup: "setup",
  settings: "settings",
  login: "sign in",
  issue: "issue",
  notFound: "not found",
};

/** Screens that read the overview: Home's Needs-you rail and the full overview. */
const overviewOn = (s: Screen) => s === "home" || s === "overview";

export default function App() {
  // App selection lives in the URL — the route (`/projects/cadence`) plus
  // `?view=list&issue=CAD-16` — so a refresh or pasted link restores it.
  const href = useHref();
  const loc = useMemo(() => currentLocation(), [href]);
  const { route, view, project, openId, filters } = loc;
  const screen = route.screen;
  const fullHeight = screen === "home" || screen === "wiki" || screen === "workspaceApp" ||
    (route.screen === "projects" && route.section === "context" && Boolean(route.slug));
  const search = href.includes("?") ? href.slice(href.indexOf("?")) : "";
  /** The href of a route, carrying the scope and drawer like `goTo`. */
  const hrefFor = (r: Route) => locationHref(goTo(loc, r), contextNavigationSearch(route, r, search));
  const setView = useCallback(
    (v: ProjectView) => update((c) => ({ ...c, view: v }), { replace: true }),
    [],
  );
  const setFilters = useCallback(
    (f: BoardFilters) => update((c) => ({ ...c, filters: f }), { replace: true }),
    [],
  );
  const goRoute = useCallback((r: Route) => update((c) => goTo(c, r)), []);
  const [projectContext, setProjectContext] = useState<ProjectContext | null>(null);
  const [projectContextLoading, setProjectContextLoading] = useState(false);
  const projectContextRequest = useRef(0);
  const observedContextRevisions = useRef<Record<string, string>>({});
  const [query, setQuery] = useState("");
  // Per-resource state (lib/cache.ts): loading, failed and empty stay
  // distinct, and a failed refresh keeps the last good payload as stale.
  const issuesState = useResource(resources.issues);
  const projectsState = useResource(resources.projects);
  const agentsState = useResource(resources.agents);
  const overviewState = useResource(resources.overview);
  const issues = issuesState.data ?? [];
  const projects = projectsState.data ?? [];
  const agents = agentsState.data;
  const [health, setHealth] = useState<Health | null>(null);
  const [meta, setMeta] = useState<Meta | null>(null);
  const [updateBanner, setUpdateBanner] = useState<UpdateBanner | null>(null);
  // The open drawer's detail, cached per id: reopening a drawer paints
  // the last payload while it revalidates.
  const detailState = useMaybeResource(openId ? resources.issue(openId) : null);
  const [toast, setToast] = useState<ToastMsg | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  // The serving build when it differs from this bundle's — drives the
  // reload banner (CAD-573).
  const [staleBuild, setStaleBuild] = useState<string | null>(null);
  const [dismissedBuild, setDismissedBuild] = useState<string | null>(null);
  const toastTimer = useRef<number>(0);

  // Writes are off on a read-only board and, since CAD-313, until this
  // browser holds the operator's session — `block` says why.
  const boardReadOnly = meta?.read_only ?? false;
  const block = writeBlock(meta);
  const readOnly = block !== null;
  const actor = meta?.actor ?? "operator (ui)";

  useEffect(() => {
    persistBrowserProjectView(view);
  }, [view]);

  // The overview payload costs a daemon probe + gh cache read, so it
  // only fetches while a screen that reads it is on screen: Home, the
  // overview, or an Apps screen (the cards' live lines and the app
  // page's Needs-you strip mark runs against it). The refs keep
  // `refresh` and the stream handler stable.
  const overviewWanted = overviewOn(screen) || screen === "apps";
  const overviewWantedRef = useRef(overviewWanted);
  overviewWantedRef.current = overviewWanted;
  // Repository guidance is only used by the compact overview.
  const contextOn = screen === "overview";

  useEffect(() => {
    const request = ++projectContextRequest.current;
    setProjectContext(null);
    if (!contextOn || project === "all") {
      setProjectContextLoading(false);
      return;
    }
    setProjectContextLoading(true);
    const expectedRevision = observedContextRevisions.current[project];
    api
      .projectContext(project, "pm", expectedRevision)
      .then((next) => {
        if (!responseBelongsToRequest(request, projectContextRequest.current, project, next.project)) return;
        setProjectContext(next);
        if (next.snapshot.head_revision) {
          observedContextRevisions.current[project] = next.snapshot.head_revision;
        }
      })
      .catch(() => {
        if (!requestIsCurrent(request, projectContextRequest.current, project, project)) return;
        setProjectContext(null);
      })
      .finally(() => {
        if (requestIsCurrent(request, projectContextRequest.current, project, project)) {
          setProjectContextLoading(false);
        }
      });
  }, [project, contextOn]);

  // The peek's id and the issue page's id, read through refs so the
  // stream and poll handlers stay stable. The page clears `openId`, so
  // the fallback has to name the id on screen or that detail goes stale
  // while `/api/stream` is down.
  const openIdRef = useRef(openId);
  openIdRef.current = openId;
  const pageIssueRef = useRef<string | null>(route.screen === "issue" ? route.id : null);
  pageIssueRef.current = route.screen === "issue" ? route.id : null;
  const loadDetail = useCallback(() => {
    for (const id of refreshIssueIds(openIdRef.current, pageIssueRef.current)) {
      void resources.issue(id).invalidate();
      void resources.lane(id).invalidate();
    }
  }, []);
  useEffect(() => {
    if (openId) void resources.issue(openId).revalidate();
  }, [openId]);

  // The overview costs a daemon probe + gh cache read (~seconds), so it
  // is fetched only while a screen that reads it is on screen —
  // revalidated when that screen comes back, painting the last payload
  // meanwhile. Requests coalesce.
  useEffect(() => {
    if (overviewWanted) void resources.overview.revalidate();
  }, [overviewWanted]);

  // Full re-read: first load, the refresh button, focus and the poll.
  // Each resource joins a request already in flight instead of stacking.
  // The operator proof walks /proc on the server: ask for it once per
  // page load and keep the answer across the 30 s polls.
  // The proof belongs to one credential/session and board mode. Switching
  // accounts must clear it even when both sessions report signed_in=true.
  const operatorKnown = useRef(false);
  const metaIdentity = useRef<string | null>(null);
  const metaKey = useRef<string | null>(null);
  const metaRequest = useRef(0);
  const [credentialGeneration, setCredentialGeneration] = useState(0);
  const refresh = useCallback(() => {
    api.health().then(setHealth).catch(() => setHealth(null));
    const request = ++metaRequest.current;
    const sentKey = sessionKey();
    if (sentKey !== metaKey.current) {
      metaKey.current = sentKey;
      metaIdentity.current = null;
      operatorKnown.current = false;
      setCredentialGeneration((generation) => generation + 1);
      setMeta(null);
    }
    const asked = !operatorKnown.current;
    let expectedKey = sentKey;
    const current = () => request === metaRequest.current && sessionKey() === expectedKey;
    api
      .meta(asked)
      .then((next) => {
        if (!current()) return;
        const identity = JSON.stringify([next.signed_in ?? null, next.session?.id ?? null, next.read_only]);
        const changed = metaIdentity.current !== null && identity !== metaIdentity.current;
        metaIdentity.current = identity;
        // A key the server refused (expired, revoked) is dropped — only
        // the very key this request carried: a sign-in that finished
        // while it was in flight stored a newer one.
        if (next.signed_in === false && sentKey) {
          setSessionKey(null);
          metaKey.current = null;
          expectedKey = null;
          setCredentialGeneration((generation) => generation + 1);
        }
        if (changed && !asked) {
          operatorKnown.current = false;
          setMeta({ ...next, operator: undefined });
          api
            .meta(true)
            .then((fresh) => {
              if (!current()) return;
              metaIdentity.current = JSON.stringify([fresh.signed_in ?? null, fresh.session?.id ?? null, fresh.read_only]);
              operatorKnown.current = typeof fresh.operator === "boolean";
              setMeta(fresh);
            })
            .catch(() => undefined);
          return;
        }
        if (typeof next.operator === "boolean") operatorKnown.current = true;
        setMeta((prev) => ({ ...next, operator: next.operator ?? (!asked && !changed ? prev?.operator : undefined) }));
      })
      .catch(() => {
        if (!current()) return;
        // Lost meta loses the answer too: ask again on the next refresh.
        operatorKnown.current = false;
        setMeta(null);
      });
    void resources.projects.refresh();
    void resources.issues.refresh();
    void resources.agents.refresh();
    if (overviewWantedRef.current) void resources.overview.refresh();
    loadDetail();
  }, [loadDetail]);

  useEffect(() => {
    refresh();
    return () => { metaRequest.current++; };
  }, [refresh]);

  const reconcile = useCallback(() => {
    // A GET begun before subscription can still be in flight. Invalidate
    // before refresh so it schedules one trailing post-subscription read;
    // refresh alone would merely join the potentially stale request.
    for (const key of RESOURCE_NAMES) {
      if (key === "overview" && !overviewWantedRef.current) resources.overview.markInvalid();
      else cache.invalidate(key);
    }
    cache.invalidate("lane");
    refresh();
  }, [refresh]);

  const liveUpdates = useRef<LiveUpdates | null>(null);
  useEffect(() => {
    const updates = new LiveUpdates({
      resync: reconcile,
      invalidate: (key) => {
        // Main refetches the lane store when a frame names issue, agents,
        // or jobs. Entity patches queue that through LiveUpdates; legacy
        // resource names still have to.
        if (laneFollowsStream([key])) cache.invalidate("lane");
        if (key === "overview" && !overviewWantedRef.current) {
          resources.overview.markInvalid();
        } else cache.invalidate(key);
      },
      patch: (event, data) => {
        if (event.type === "issue") {
          if (typeof data.id !== "string" || !["upsert", "delete"].includes(String(data.op)) ||
              (data.op === "upsert" && (!data.issue || typeof data.issue !== "object"))) {
            reconcile(); return;
          }
          if (resources.issues.get().data === null) {
            void resources.issues.invalidate();
          } else resources.issues.mutate((rows) => patchRows(rows, data.id as string, data.op, data.issue as IssueCard, (row) => row.id));
        } else if (event.type === "agent") {
          if (typeof data.id !== "string" || !["upsert", "delete"].includes(String(data.op)) ||
              (data.op === "upsert" && (!data.agent || typeof data.agent !== "object"))) {
            reconcile(); return;
          }
          if (resources.agents.get().data === null) {
            void resources.agents.invalidate();
          } else resources.agents.mutate((value) => ({ ...value,
            agents: patchRows(value.agents, data.id as string, data.op, data.agent as Agent, (row) => row.alias),
          }));
        } else {
          if (!data.by_issue || !data.daemon || !("totals" in data)) { reconcile(); return; }
          resources.agents.mutate((value) => ({ ...value,
            daemon: data.daemon as AgentsPayload["daemon"],
            totals: data.totals as AgentsPayload["totals"],
            by_issue: data.by_issue as AgentsPayload["by_issue"],
          }));
          // Issue-page lane stores depend on runtime state, including
          // changes which leave their issue card unchanged.
          // LiveUpdates batches the dependent lane invalidation.
        }
      },
    });
    liveUpdates.current = updates;
    const sub = subscribeSse({
      url: "/api/stream?entities=1",
      events: ["hello", "heartbeat", "issues", "agents", "jobs", "monitoring", "issue", "agent", "agent_meta", "plan", "aggregates"],
      onEvent: updates.event,
      onOpen: updates.opened,
      onError: updates.failed,
      onBuild: (server) => setStaleBuild(buildChanged(server, UI_BUILD) ? server : null),
      probeBuild: serverBuild,
    });
    return () => {
      updates.close();
      sub.close();
      if (liveUpdates.current === updates) liveUpdates.current = null;
    };
  }, [reconcile]);

  // Reload is explicit — never automatic. The composer draft is
  // stashed first so one click costs no text (CAD-573).
  const reload = useCallback(() => {
    const storage = sessionStore();
    if (storage) stashDraft(storage, composerField(document)?.value);
    location.reload();
  }, []);

  // After that reload the draft waits in storage; write it back once
  // the composer mounts — the reload may land on any route.
  useEffect(() => {
    const storage = sessionStore();
    const draft = storage ? takeDraft(storage) : null;
    if (!draft) return;
    const restore = () => {
      const field = composerField(document);
      if (field) applyDraft(field, draft);
      return field !== null;
    };
    if (restore()) return;
    const observer = new MutationObserver(() => {
      if (restore()) observer.disconnect();
    });
    observer.observe(document.body, { childList: true, subtree: true });
    const giveUp = window.setTimeout(() => observer.disconnect(), 30_000);
    return () => {
      window.clearTimeout(giveUp);
      observer.disconnect();
    };
  }, []);

  // Full reads are recovery only: suppress them while named heartbeat
  // frames prove the stream healthy; reconnects resync immediately.
  useEffect(() => {
    const onFocus = () => { if (!liveUpdates.current?.healthy()) refresh(); };
    const tick = () => {
      if (document.visibilityState === "visible" && !liveUpdates.current?.healthy()) refresh();
    };
    const timer = setInterval(tick, 30_000);
    addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", tick);
    return () => {
      clearInterval(timer);
      removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [refresh]);

  const say = useCallback((kind: ToastMsg["kind"], text: string) => {
    setToast({ kind, text });
    clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 4200);
  }, []);

  const ackMonitor = useCallback(
    (monitor: string, seq: number) => {
      if (block) {
        say("err", `monitor acknowledgements are disabled — ${block}`);
        return;
      }
      api
        .monitorAck(monitor, seq)
        .then(() => {
          say("ok", `${monitor} alert ${seq} acknowledged`);
          return resources.overview.invalidate();
        })
        .catch((e) =>
          say(
            "err",
            `monitor alert acknowledgement failed: ${String(e.message ?? e)}`,
          ),
        );
    },
    [block, say],
  );

  /// A write response is authoritative: merge the fresh card into the
  /// board and the fresh detail into the drawer — no second fetch.
  const applyWrite = useCallback(
    (resp: WriteResp, verb: string) => {
      resources.issues.mutate((prev) => {
        const i = prev.findIndex((c) => c.id === resp.card.id);
        if (i === -1) return [...prev, resp.card];
        const next = prev.slice();
        next[i] = resp.card;
        return next;
      });
      resources.issue(resp.issue.id).write(() => resp.issue);
      const warns = (resp.warnings ?? []).filter(Boolean);
      if (warns.length) {
        say("warn", `${verb} · ${warns.join(" · ")}`);
      } else {
        say("ok", verb);
      }
    },
    [say],
  );

  const writeError = useCallback(
    (e: unknown, verb: string) => {
      const err = e as ApiError;
      // A conflict carries the fresh card — resync so the board shows
      // the state that won.
      if (err.card) {
        resources.issues.mutate((prev) => {
          const i = prev.findIndex((c) => c.id === err.card!.id);
          if (i === -1) return prev;
          const next = prev.slice();
          next[i] = err.card!;
          return next;
        });
      }
      say("err", `${verb} failed: ${err.message ?? e}`);
    },
    [say],
  );

  /// Drag or drawer status change. `rev` is the card's if_rev token;
  /// `rollback` restores the previous card on failure.
  const moveIssue = useCallback(
    (issue: IssueCard, status: string) => {
      if (block) {
        say("err", `writes are disabled — ${block}`);
        return;
      }
      if (issue.status === status) return;
      const prev = issue;
      // Optimistic: the card moves now, the write decides for real.
      resources.issues.mutate((all) =>
        all.map((c) => (c.id === issue.id ? { ...c, status } : c)),
      );
      api
        .patch(issue.id, { status }, issue.rev)
        .then((resp) => applyWrite(resp, `${issue.id} → ${status}`))
        .catch((e) => {
          resources.issues.mutate((all) => all.map((c) => (c.id === issue.id ? prev : c)));
          writeError(e, `${issue.id} move`);
        });
    },
    [applyWrite, writeError, block, say],
  );

  const openIssue = useCallback((id: string) => update((c) => ({ ...c, openId: id })), []);
  const closeIssue = useCallback(() => update((c) => ({ ...c, openId: null })), []);
  const openAgent = useCallback(
    (alias: string | null) => update((c) => ({ ...c, route: { screen: "agents", alias } })),
    [],
  );
  // CAD-561: the draining banner. Cheap (the daemon's own view), polled
  // board-wide so an update is visible on every page, not only Settings.
  useEffect(() => {
    let stop = false;
    const tick = () => {
      api
        .updateBanner()
        .then((next) => {
          if (!stop) setUpdateBanner(next);
        })
        .catch(() => undefined);
    };
    tick();
    const timer = window.setInterval(tick, 15000);
    return () => {
      stop = true;
      window.clearInterval(timer);
    };
  }, []);

  // The sidebar and the phone menu open a project page. The chip row
  // and the memory picker filter the screen, and only a screen that has one.
  const projectHref = (key: string) => {
    const next = openProject(loc, key);
    return locationHref(next, contextNavigationSearch(route, next.route, search));
  };
  const filterHref = (key: string) => {
    const next = withProject(loc, key);
    return locationHref(next, contextNavigationSearch(route, next.route, search));
  };
  const navProject = route.screen === "projects" ? project : null;
  const projectSlug = project === "all" ? null : project;
  // Board-level App menu (CAD-784): the shell reports its verified
  // installation receipt; the board nests that installation's
  // sections in the shared sidebar/phone menu only while the route
  // still names the same verified CRM installation. A forged route
  // or a non-CRM app never produces entries.
  const [activeApp, setActiveApp] = useState<ActiveInstallation | null>(null);
  const reportInstallation = useCallback((info: ActiveInstallation | null) => {
    setActiveApp((prev) => {
      if (prev === null && info === null) return prev;
      if (
        prev !== null && info !== null &&
        prev.installId === info.installId && prev.kind === info.kind && prev.title === info.title
      ) {
        return prev;
      }
      return info;
    });
  }, []);
  const activeCrm =
    route.screen === "workspaceApp" &&
    activeApp !== null &&
    activeApp.installId === route.installId &&
    activeApp.kind === "crm"
      ? activeApp
      : null;
  const crmQuery = new URLSearchParams(search).get("crm");
  const crmSection: CrmSection =
    crmQuery === "segments" || crmQuery === "campaigns" ? crmQuery : "customers";
  const appMenu = activeCrm ? crmAppMenu(href, activeCrm.title, crmSection) : null;

  return (
    <WriteGate.Provider value={meta === null ? "Checking write access…" : block}>
    <div
      data-app-shell
      className={`grid lg:grid-cols-[208px_minmax(0,1fr)] bg-ink-900 ${
        fullHeight ? "app-workspace h-[100dvh] grid-rows-[minmax(0,1fr)] overflow-hidden" : "min-h-screen"
      }`}
    >
      <Sidebar
        screen={screen === "workspaceApp" ? "apps" : screen}
        navHref={hrefFor}
        appMenu={appMenu}
        project={navProject}
        projectHref={projectHref}
        projects={projects}
        issues={issuesState}
        projectsError={projectsState.status === "failed" ? projectsState.error : null}
        signedIn={meta?.signed_in ?? null}
        sessionUser={meta?.session?.user}
      />

      {/* Workspaces fill the dynamic viewport; their panes own scrolling.
          Other screens retain the natural document flow. */}
      <div className={`min-w-0 flex flex-col ${fullHeight ? "h-full min-h-0" : ""}`}>
        <header className="app-header sticky top-0 z-10 flex items-center gap-2 sm:gap-3 px-4 lg:px-8 border-b border-ink-700 bg-ink-900/95 backdrop-blur">
          <button
            onClick={() => setMenuOpen((o) => !o)}
            className="header-menu header-icon -ml-1 shrink-0 gap-1 text-ink-200"
            aria-label="Open navigation"
            aria-expanded={menuOpen}
            aria-controls="mobile-navigation"
          >
            <IconList size={18} />
          </button>
          <nav className="header-breadcrumb min-w-0 flex-1" aria-label="Breadcrumb">
            {route.screen === "projects" && route.slug ? (
              <>
                <Link href={hrefFor({ screen: "projects", slug: null, section: "overview" })} className="hidden sm:inline-flex">Projects</Link>
                <span className="hidden sm:inline text-ink-600" aria-hidden="true">/</span>
                <span className="truncate text-ink-100" title={route.slug}>{route.slug}</span>
              </>
            ) : <span className="truncate text-ink-200 capitalize">{route.screen === "issue" ? route.id : SCREEN_LABEL[screen]}</span>}
          </nav>

          <div className="ml-auto flex items-center gap-0 sm:gap-2 shrink-0">
            <StatusChips
              variant="header"
              readOnly={boardReadOnly}
              mayWrite={!readOnly}
              health={health}
              actor={actor}
              onRefresh={refresh}
            >
              <SignIn meta={meta} onChange={refresh} />
              {staleBuild !== null && staleBuild !== dismissedBuild && (
                <BuildUpdateNotice onReload={reload} onDismiss={() => setDismissedBuild(staleBuild)} />
              )}
            </StatusChips>
            <ThemeToggle />
          </div>
        </header>

        {updateBanner && (
          <details className="update-notice border-b border-amber-500/30 bg-amber-500/5 px-4 lg:px-8 py-2 text-label text-ink-300">
            <summary className="cursor-pointer"><span className="font-medium text-ink-200">Updating Cadence</span> · {updateBanner.count > 0 ? `Waiting for ${updateBanner.count} active turn${updateBanner.count === 1 ? "" : "s"}` : "No active turns remaining"}<span className="text-ink-500 ml-3">Details</span></summary>
            <p className="num text-micro text-ink-500 mt-2 break-all">{updateBanner.pending.from ?? "?"} → {updateBanner.pending.target} · {updateBanner.pending.phase} · {updateBanner.pending.by}</p>
          </details>
        )}

        {menuOpen && (
          <div id="mobile-navigation" className="lg:hidden border-b border-ink-700 bg-ink-875 px-4 py-3 space-y-1">
            <NavList
              screen={screen === "workspaceApp" ? "apps" : screen}
              navHref={hrefFor}
              appMenu={appMenu}
              label="Workspace"
              onNavigate={() => setMenuOpen(false)}
            />
            <div className="slabel pt-2">projects</div>
            <nav aria-label="Projects">
              <ProjectList
                project={navProject}
                projectHref={projectHref}
                projects={projects}
                issues={issuesState}
                projectsError={projectsState.status === "failed" ? projectsState.error : null}
                onNavigate={() => setMenuOpen(false)}
              />
            </nav>
            {/* The header carries these as icons under lg — here they keep
                their words, so the meaning is one tap away on touch. */}
            <div className="slabel pt-2">board</div>
            <div role="region" aria-label="Board" className="flex flex-wrap items-center gap-1.5">
              <StatusChips
                variant="menu"
                readOnly={boardReadOnly}
                mayWrite={!readOnly}
                health={health}
                actor={actor}
                onRefresh={refresh}
              >
                <SignIn meta={meta} onChange={refresh} />
              </StatusChips>
            </div>
          </div>
        )}

        {projectScope(route) === "chips" && showProjectChoices(projects.length, project) && (
          <div className="px-4 lg:px-8 pt-4">
            <ProjectFilter project={project} projects={projects} hrefFor={filterHref} />
          </div>
        )}

        {screen === "home" && <SetupNudge readOnly={meta ? boardReadOnly : null} />}
        {screen === "home" && (
          <Home
            readOnly={readOnly}
            hosted={meta ? meta.hosted === true || meta.session?.origin === "public" : null}
            overview={overviewState}
            onOpenIssue={openIssue}
            overviewHref={hrefFor({ screen: "overview" })}
          />
        )}
        {screen === "overview" && (
          <OverviewView
            state={overviewState}
            issues={issuesState}
            onRetry={() => void resources.overview.refresh()}
            project={project}
            projects={projects}
            context={visibleContext(project, projectContext)}
            contextLoading={projectContextLoading}
            readOnly={readOnly}
            onAck={ackMonitor}
            onOpenContext={() => goRoute({ screen: "projects", slug: projectSlug, section: "context" })}
          />
        )}
        {(route.screen === "issue" || (route.screen === "projects" && route.slug)) && (
          <SectionTabs
            label={route.screen === "issue" ? route.project : route.slug!}
            tabs={[
              {
                label: "Overview",
                href: hrefFor({ screen: "projects", slug: route.screen === "issue" ? route.project : route.slug, section: "overview" }),
                on: route.screen === "projects" && route.section === "overview",
              },
              {
                label: "Issues",
                href: hrefFor({
                  screen: "projects",
                  slug: route.screen === "issue" ? route.project : route.slug,
                  section: "issues",
                }),
                on: route.screen === "issue" || route.section === "issues",
              },
              {
                label: "Epics",
                href: hrefFor({
                  screen: "projects",
                  slug: route.screen === "issue" ? route.project : route.slug,
                  section: "epics",
                }),
                on: route.screen === "projects" && route.section === "epics",
              },
              {
                label: "Milestones",
                href: hrefFor({
                  screen: "projects",
                  slug: route.screen === "issue" ? route.project : route.slug,
                  section: "milestones",
                }),
                on: route.screen === "projects" && route.section === "milestones",
              },
              {
                label: "Workflows",
                href: hrefFor({
                  screen: "projects",
                  slug: route.screen === "issue" ? route.project : route.slug,
                  section: "workflows",
                }),
                on: route.screen === "projects" && route.section === "workflows",
              },
              {
                label: "Context",
                href: hrefFor({
                  screen: "projects",
                  slug: route.screen === "issue" ? route.project : route.slug,
                  section: "context",
                }),
                on: route.screen === "projects" && route.section === "context",
              },
            ]}
          />
        )}
        {route.screen === "projects" && route.section === "overview" && (
          <ProjectsOverview project={project} projects={projects} issues={issuesState} onOpenIssue={openIssue} onRetry={() => void resources.issues.refresh()} />
        )}
        {route.screen === "issue" && (
          <IssuePage
            project={route.project}
            id={route.id}
            tab={route.tab}
            tabHref={(tab) => locationHref({ ...loc, route: { ...route, tab } }, search)}
            issues={issues}
            agents={agents ?? null}
            readOnly={readOnly}
            writeBlock={block}
            kickoffBlock={operatorKickoffBlock(meta)}
            onWrite={applyWrite}
            onError={writeError}
            onOpen={(issueId) => {
              const card = issues.find((i) => i.id === issueId);
              const nextProject = card?.project ?? route.project;
              update((c) => ({
                ...c,
                openId: null,
                project: nextProject,
                route: { screen: "issue", project: nextProject, id: issueId, tab: "overview" },
              }));
            }}
            planHref={hrefFor({ screen: "home" })}
            onToast={(text) => say("ok", text)}
          />
        )}
        {route.screen === "projects" && route.section === "issues" && (
          <Board
            key={credentialGeneration}
            issues={issuesState}
            onRetry={() => void resources.issues.refresh()}
            projects={projects}
            agents={agents}
            health={health}
            project={project}
            view={view}
            onView={setView}
            query={query}
            readOnly={readOnly || meta === null}
            sessionId={meta === null ? undefined : meta.session?.id ?? null}
            actor={actor}
            onQuery={setQuery}
            filters={filters}
            onFilters={setFilters}
            onOpen={openIssue}
            onMove={moveIssue}
            onCreated={applyWrite}
            onError={writeError}
            onAgents={() => goRoute({ screen: "agents", alias: null })}
          />
        )}
        {route.screen === "projects" && route.slug && route.section === "epics" && (
          <Epics
            project={route.slug}
            issues={issuesState}
            viewer={{ readOnly, operator: meta?.operator === true }}
            onOpenIssue={openIssue}
            onRetry={() => void resources.issues.refresh()}
          />
        )}
        {route.screen === "projects" && route.slug && route.section === "milestones" && (
          <Milestones project={route.slug} issues={issuesState} onOpenIssue={openIssue} />
        )}
        {route.screen === "projects" && route.slug && route.section === "workflows" && (
          <Workflows
            project={route.slug}
            viewer={{ readOnly, operator: meta?.operator === true }}
            onOpenIssue={openIssue}
            onHome={() => goRoute({ screen: "home" })}
          />
        )}
        {route.screen === "projects" && route.section === "context" && (
          <Context
            readOnly={readOnly}
            actor={actor}
            onToast={say}
            navHref={hrefFor}
            project={project}
          />
        )}
        {route.screen === "apps" && !route.project && (
          <Apps project={project} viewer={{ readOnly, operator: meta?.operator === true }} />
        )}
        {route.screen === "workspaceApp" && (
          <AppShell installId={route.installId} viewer={{ readOnly, operator: meta?.operator === true }} onInstallation={reportInstallation}>
            <WorkspaceApp installId={route.installId} viewer={{ readOnly, operator: meta?.operator === true }} onBack={() => goRoute({ screen: "apps", project: null, name: null })} />
          </AppShell>
        )}
        {route.screen === "apps" && route.project && route.name && (
          <AppDetail
            project={route.project}
            name={route.name}
            viewer={{ readOnly, operator: meta?.operator === true }}
            onOpenIssue={openIssue}
            onHome={() => goRoute({ screen: "home" })}
          />
        )}
        {route.screen === "agents" && (
          <Agents
            state={agentsState}
            issues={issuesState}
            project={project}
            open={route.alias}
            onOpenAgent={openAgent}
            onOpenIssue={openIssue}
            onRetry={() => void resources.agents.refresh()}
            onRetryAssignments={() => void resources.issues.refresh()}
          />
        )}
        {route.screen === "wiki" && (
          <Wiki
            route={route}
            navHref={hrefFor}
            readOnly={readOnly}
            actor={actor}
            onToast={say}
          />
        )}
        {screen === "outbox" && <Outbox operator={meta?.operator === true} onOpenIssue={openIssue} />}
        {screen === "setup" && (
          <Setup settingsHref={hrefFor({ screen: "settings", section: "models" })} readOnly={meta ? boardReadOnly : null} />
        )}
        {route.screen === "settings" && (
          <SectionTabs
            label="settings"
            tabs={[
              { label: "Models", href: hrefFor({ screen: "settings", section: "models" }), on: route.section === "models" },
              { label: "Connections", href: hrefFor({ screen: "settings", section: "connections" }), on: route.section === "connections" },
              { label: "Memory", href: hrefFor({ screen: "settings", section: "memory" }), on: route.section === "memory" },
              { label: "Update", href: hrefFor({ screen: "settings", section: "update" }), on: route.section === "update" },
              ...(meta?.platform_account_configured ? [{ label: "Account", href: hrefFor({ screen: "settings", section: "account" }), on: route.section === "account" }] : []),
              {
                label: "Master permissions",
                href: hrefFor({ screen: "settings", section: "permissions" }),
                on: route.section === "permissions",
              },
            ]}
          />
        )}
        {route.screen === "settings" && route.section === "memory" && (
          <Memory project={project} projects={projects} projectHref={filterHref} onError={writeError} />
        )}
        {route.screen === "settings" && route.section === "models" && <ModelDefaults />}
        {route.screen === "settings" && route.section === "connections" && (
          <Connections viewer={{ readOnly, operator: meta?.operator === true }} />
        )}
        {route.screen === "settings" && route.section === "account" && <PlatformAccount />}
        {route.screen === "settings" && route.section === "update" && (
          <Update viewer={{ readOnly, operator: meta?.operator === true }} />
        )}
        {route.screen === "settings" && route.section === "permissions" && <MasterPermissions viewer={{ readOnly, operator: meta?.operator === true, boardReadOnly, signedIn: meta?.signed_in === true, sessionId: meta?.session?.id ?? null }} />}
        {screen === "login" && (
          <Login
            onSignedIn={() => {
              refresh();
              window.setTimeout(() => goRoute({ screen: "home" }), 900);
            }}
          />
        )}
        {screen === "notFound" && (
          <main className="px-4 lg:px-8 pt-10 pb-9">
            <h1 className="text-section font-semibold text-ink-100">Nothing lives here</h1>
            <p className="text-body text-ink-400 mt-2">
              <span className="num break-all">{route.screen === "notFound" ? route.path : ""}</span> is
              not a page of this board.{" "}
              <Link href={hrefFor({ screen: "home" })} className="lnk">
                Go home
              </Link>
            </p>
          </main>
        )}
      </div>

      {openId && route.screen !== "issue" && (
        <Drawer
          key={openId}
          id={openId}
          detail={detailState?.data?.id === openId ? detailState.data : null}
          readState={detailState}
          onRetry={() => void resources.issue(openId).refresh()}
          href={(() => {
            const fromDetail = detailState?.data?.id === openId ? detailState.data.project : null;
            const fromCard = issues.find((i) => i.id === openId)?.project ?? null;
            const key = fromDetail ?? fromCard ?? (project !== "all" ? project : null);
            return key ? issuePath(key, openId) : null;
          })()}
          onClose={closeIssue}
        />
      )}

      <Toast msg={toast} />
    </div>
    </WriteGate.Provider>
  );
}
