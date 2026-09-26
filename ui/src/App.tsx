import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, type WriteResp } from "./lib/api";
import Agents from "./features/agents/Agents";
import Apps from "./features/apps/Apps";
import AppDetail from "./features/apps/AppDetail";
import Board from "./features/projects/Board";
import Drawer from "./features/projects/Drawer";
import Epics from "./features/projects/Epics";
import Milestones from "./features/projects/Milestones";
import Memory from "./features/settings/Memory";
import ModelDefaults from "./features/settings/ModelDefaults";
import Update from "./features/settings/Update";
import Outbox from "./features/outbox/Outbox";
import OverviewView from "./features/home/Overview";
import Home from "./features/home/Home";
import Context from "./features/projects/Context";
import Workflows from "./features/projects/Workflows";
import Sidebar from "./ui/Sidebar";
import Link from "./ui/Link";
import SectionTabs from "./ui/SectionTabs";
import StatusChips from "./ui/StatusChips";
import ThemeToggle from "./ui/ThemeToggle";
import Setup from "./features/setup/Setup";
import SetupNudge from "./features/setup/SetupNudge";
import Login from "./features/auth/Login";
import SignIn from "./features/auth/SignIn";
import { writeBlock } from "./features/auth/gate";
import { WriteGate } from "./features/auth/WriteGate";
import { sessionKey, setSessionKey } from "./lib/sessionKey";
import { buildChanged, serverBuild, subscribeSse, UI_BUILD } from "./lib/sse";
import { applyDraft, composerField, sessionStore, stashDraft, takeDraft } from "./lib/draft";
import Toast, { type ToastMsg } from "./ui/Toast";
import { Logo } from "./ui/Logo";
import { countLabel, issueCounts } from "./lib/counts";
import type { BoardFilters } from "./lib/filters";
import type { UpdateBanner } from "./lib/types";
import { invalidatedBy } from "./lib/cache";
import { cache, resources } from "./lib/resources";
import { useMaybeResource, useResource } from "./lib/useResource";
import {
  goTo,
  locationHref,
  NAV,
  readLocation,
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
  navigate(locationHref(fn(currentLocation()), location.search), opts);
}

const SCREEN_LABEL: Record<Screen, string> = {
  home: "home",
  overview: "overview",
  projects: "projects",
  apps: "apps",
  agents: "agents",
  outbox: "outbox",
  setup: "setup",
  settings: "settings",
  login: "sign in",
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
  const search = href.includes("?") ? href.slice(href.indexOf("?")) : "";
  /** The href of a route, carrying the scope and drawer like `goTo`. */
  const hrefFor = (r: Route) => locationHref(goTo(loc, r), search);
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
  const [projectContextError, setProjectContextError] = useState<string | null>(null);
  const [projectContextErrorProject, setProjectContextErrorProject] = useState<string | null>(null);
  const [projectContextRefresh, setProjectContextRefresh] = useState(0);
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
  // The overview shows the project context compactly; Projects → context in full.
  const contextOn =
    screen === "overview" || (route.screen === "projects" && route.section === "context");

  useEffect(() => {
    const request = ++projectContextRequest.current;
    setProjectContext(null);
    setProjectContextError(null);
    setProjectContextErrorProject(null);
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
      .catch((error) => {
        if (!requestIsCurrent(request, projectContextRequest.current, project, project)) return;
        setProjectContextError(String(error?.message ?? error));
        setProjectContextErrorProject(project);
      })
      .finally(() => {
        if (requestIsCurrent(request, projectContextRequest.current, project, project)) {
          setProjectContextLoading(false);
        }
      });
  }, [project, contextOn, projectContextRefresh]);

  // The open drawer's id, read through a ref so the stream and poll
  // handlers stay stable while the drawer changes.
  const openIdRef = useRef(openId);
  openIdRef.current = openId;
  const loadDetail = useCallback(() => {
    const id = openIdRef.current;
    if (id) void resources.issue(id).invalidate();
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
  // A change of sign-in (CAD-313) changes the answer: it is asked again.
  const operatorKnown = useRef(false);
  const signedIn = useRef<boolean | undefined>(undefined);
  const refresh = useCallback(() => {
    api.health().then(setHealth).catch(() => setHealth(null));
    const asked = !operatorKnown.current;
    const sentKey = sessionKey();
    api
      .meta(asked)
      .then((next) => {
        const changed = signedIn.current !== undefined && next.signed_in !== signedIn.current;
        signedIn.current = next.signed_in;
        // A key the server refused (expired, revoked) is dropped — only
        // the very key this request carried: a sign-in that finished
        // while it was in flight stored a newer one.
        if (next.signed_in === false && sentKey && sessionKey() === sentKey) setSessionKey(null);
        if (changed && !asked) {
          operatorKnown.current = false;
          setMeta({ ...next, operator: undefined });
          api
            .meta(true)
            .then((fresh) => {
              if (typeof fresh.operator === "boolean") operatorKnown.current = true;
              setMeta(fresh);
            })
            .catch(() => undefined);
          return;
        }
        if (typeof next.operator === "boolean") operatorKnown.current = true;
        setMeta((prev) => ({ ...next, operator: next.operator ?? prev?.operator }));
      })
      .catch(() => {
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

  useEffect(refresh, [refresh]);

  // Live updates: each /api/stream frame names the resources it
  // invalidates (`{"resources":[...]}`, `event_resources` in src/ui.rs);
  // only those refetch, coalesced per resource. The stream's `hello`
  // frame — and /api/version once it has been down ~15 s — reports the
  // serving build: a mismatch means this bundle predates a rollout and
  // the banner offers a reload (CAD-573). The 30 s poll below stays as
  // the fallback while the stream is down.
  useEffect(() => {
    const sub = subscribeSse({
      url: "/api/stream",
      events: ["issues", "agents", "jobs", "monitoring"],
      onEvent: (e) => {
        for (const name of invalidatedBy(e.data)) {
          // Families are keyed stores — invalidate the prefix, not one entry.
          if (
            name === "issue" ||
            name === "workflows" ||
            name === "app" ||
            name === "app_runs" ||
            name === "app_outputs"
          ) {
            cache.invalidate(name);
          } else if (name === "overview") {
            // Hidden overview: skip — it revalidates when a screen that
            // reads it opens.
            if (overviewWantedRef.current) void resources.overview.invalidate();
          } else void resources[name].invalidate();
        }
      },
      onBuild: (server) => setStaleBuild(buildChanged(server, UI_BUILD) ? server : null),
      probeBuild: serverBuild,
    });
    return () => sub.close();
  }, [loadDetail]);

  // The banner's only action — never automatic. The composer draft is
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

  // Fallback poll — re-read on focus and every 30s while the tab is
  // visible, covering any gap while the stream reconnects.
  useEffect(() => {
    const onFocus = () => refresh();
    const tick = () => {
      if (document.visibilityState === "visible") refresh();
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

  const projectHref = (key: string) => locationHref(withProject(loc, key), search);
  const projectSlug = project === "all" ? null : project;

  return (
    <WriteGate.Provider value={block}>
    {staleBuild !== null && (
      <div
        role="alert"
        data-build-banner
        className="fixed inset-x-0 top-0 z-50 flex items-center justify-center gap-3 border-b border-ink-900/20 bg-warn px-4 py-2.5 text-ink-900"
      >
        <span className="text-label font-medium">Cadence was updated</span>
        <span className="num hidden sm:inline text-micro opacity-60">({staleBuild})</span>
        <button
          type="button"
          onClick={reload}
          className="h-7 shrink-0 rounded bg-ink-900 px-3 text-label font-medium text-ink-100 hover:opacity-85"
        >
          Reload
        </button>
      </div>
    )}
    <div className="grid lg:grid-cols-[208px_minmax(0,1fr)] min-h-screen bg-ink-900">
      <Sidebar
        screen={screen}
        navHref={hrefFor}
        project={project}
        projectHref={projectHref}
        projects={projects}
        issues={issuesState}
        projectsError={projectsState.status === "failed" ? projectsState.error : null}
        signedIn={meta?.signed_in ?? null}
      />

      <div className="min-w-0 flex flex-col">
        <header className="sticky top-0 z-10 h-[2.85rem] flex items-center gap-3 px-4 lg:px-8 border-b border-ink-700 bg-ink-900/95 backdrop-blur">
          <button
            onClick={() => setMenuOpen((o) => !o)}
            className="lg:hidden -ml-1 inline-flex items-center gap-1.5 h-8 px-2 rounded text-ink-200 hover:bg-ink-800"
            aria-label="menu"
            aria-expanded={menuOpen}
          >
            <Logo size={17} />
            <span className="text-label font-medium">cadence</span>
            <svg
              width="10"
              height="10"
              viewBox="0 0 10 10"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              className={`transition-transform ${menuOpen ? "rotate-180" : ""}`}
            >
              <path d="M2 3.5l3 3 3-3" />
            </svg>
          </button>
          <div className="num text-label text-ink-500 min-w-0 truncate">
            <span className="hidden sm:inline text-ink-300">cadence</span>
            <span className="hidden sm:inline"> / </span>
            <span className="text-ink-100">{SCREEN_LABEL[screen]}</span>
          </div>

          <div className="ml-auto flex items-center gap-2 shrink-0">
            <StatusChips
              variant="header"
              readOnly={boardReadOnly}
              mayWrite={!readOnly}
              health={health}
              actor={actor}
              onRefresh={refresh}
            >
              <SignIn meta={meta} onChange={refresh} />
            </StatusChips>
            <ThemeToggle />
          </div>
        </header>

        {updateBanner && (
          <div className="border-b border-amber-500/40 bg-amber-500/10 px-4 lg:px-8 py-2 text-body text-ink-200">
            <span className="font-semibold">Update in progress</span> —{" "}
            {updateBanner.pending.from ?? "?"} →{" "}
            <span className="num">{updateBanner.pending.target}</span> (
            {updateBanner.pending.phase}) by {updateBanner.pending.by}
            {updateBanner.count > 0
              ? ` · waiting for ${updateBanner.count} turn${updateBanner.count === 1 ? "" : "s"}`
              : " · nothing in flight"}
          </div>
        )}

        {menuOpen && (
          <nav className="lg:hidden border-b border-ink-700 bg-ink-875 px-4 py-3 space-y-1">
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-1.5">
              {NAV.map((item) => (
                <Link
                  key={item.screen}
                  href={hrefFor(item.route)}
                  onClick={() => setMenuOpen(false)}
                  aria-current={screen === item.screen ? "page" : undefined}
                  className={`h-9 inline-flex items-center justify-center rounded text-secondary ${
                    screen === item.screen
                      ? "bg-accent/15 text-accent font-medium"
                      : "bg-ink-800 text-ink-300"
                  }`}
                >
                  {item.label}
                </Link>
              ))}
            </div>
            <div className="slabel pt-2">projects</div>
            <div className="grid gap-1">
              <Link
                href={projectHref("all")}
                onClick={() => setMenuOpen(false)}
                className={`flex items-center justify-between h-8 px-2.5 rounded text-label ${
                  project === "all"
                    ? "bg-accent/15 text-accent"
                    : "text-ink-300 hover:bg-ink-800"
                }`}
              >
                All projects
                <span
                  className="num text-micro text-ink-500"
                  title={issuesState.data ? countLabel(issueCounts(issues)) : undefined}
                >
                  {issuesState.data ? issueCounts(issues).open : "…"}
                </span>
              </Link>
              {projects.map((p) => (
                <Link
                  key={p.key}
                  href={projectHref(p.key)}
                  onClick={() => setMenuOpen(false)}
                  className={`flex items-center justify-between h-8 px-2.5 rounded text-label ${
                    project === p.key
                      ? "bg-accent/15 text-accent"
                      : "text-ink-300 hover:bg-ink-800"
                  }`}
                >
                  <span className="truncate">{p.key}</span>
                  <span
                    className="num text-micro text-ink-500"
                    title={issuesState.data ? countLabel(issueCounts(issues, p.key)) : undefined}
                  >
                    {p.prefix} {issuesState.data ? issueCounts(issues, p.key).open : "…"}
                  </span>
                </Link>
              ))}
            </div>
            {/* The header carries these as icons under lg — here they keep
                their words, so the meaning is one tap away on touch. */}
            <div className="slabel pt-2">board</div>
            <div className="flex flex-wrap items-center gap-1.5">
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
          </nav>
        )}

        {screen === "home" && <SetupNudge readOnly={meta ? boardReadOnly : null} />}
        {screen === "home" && (
          <Home
            readOnly={readOnly}
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
        {route.screen === "projects" && route.slug && (
          <SectionTabs
            label={route.slug}
            tabs={[
              { label: "Issues", href: hrefFor({ ...route, section: "issues" }), on: route.section === "issues" },
              { label: "Epics", href: hrefFor({ ...route, section: "epics" }), on: route.section === "epics" },
              {
                label: "Milestones",
                href: hrefFor({ ...route, section: "milestones" }),
                on: route.section === "milestones",
              },
              {
                label: "Workflows",
                href: hrefFor({ ...route, section: "workflows" }),
                on: route.section === "workflows",
              },
              { label: "Context", href: hrefFor({ ...route, section: "context" }), on: route.section === "context" },
            ]}
          />
        )}
        {route.screen === "projects" && route.section === "issues" && (
          <Board
            issues={issuesState}
            onRetry={() => void resources.issues.refresh()}
            projects={projects}
            agents={agents}
            health={health}
            project={project}
            view={view}
            onView={setView}
            query={query}
            readOnly={readOnly}
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
            project={project}
            context={visibleContext(project, projectContext)}
            contextLoading={projectContextLoading}
            contextError={projectContextErrorProject === project ? projectContextError : null}
            onRetryContext={() => setProjectContextRefresh((value) => value + 1)}
          />
        )}
        {route.screen === "apps" && !route.project && (
          <Apps project={project} viewer={{ readOnly, operator: meta?.operator === true }} />
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
          />
        )}
        {screen === "outbox" && <Outbox operator={meta?.operator === true} />}
        {screen === "setup" && (
          <Setup settingsHref={hrefFor({ screen: "settings", section: "models" })} readOnly={meta ? boardReadOnly : null} />
        )}
        {route.screen === "settings" && (
          <SectionTabs
            label="settings"
            tabs={[
              { label: "Models", href: hrefFor({ screen: "settings", section: "models" }), on: route.section === "models" },
              { label: "Memory", href: hrefFor({ screen: "settings", section: "memory" }), on: route.section === "memory" },
              { label: "Update", href: hrefFor({ screen: "settings", section: "update" }), on: route.section === "update" },
            ]}
          />
        )}
        {route.screen === "settings" && route.section === "memory" && (
          <Memory project={project} onError={writeError} />
        )}
        {route.screen === "settings" && route.section === "models" && <ModelDefaults />}
        {route.screen === "settings" && route.section === "update" && (
          <Update viewer={{ readOnly, operator: meta?.operator === true }} />
        )}
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

      {openId && (
        <Drawer
          key={openId}
          id={openId}
          agents={agents}
          projects={projects}
          pmDir={health?.pm_dir}
          detail={detailState?.data?.id === openId ? detailState.data : null}
          readOnly={readOnly}
          actor={actor}
          onClose={closeIssue}
          onOpen={openIssue}
          onWrite={applyWrite}
          onError={writeError}
        />
      )}

      <Toast msg={toast} />
    </div>
    </WriteGate.Provider>
  );
}
