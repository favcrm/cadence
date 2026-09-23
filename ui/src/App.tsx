import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, type WriteResp } from "./api";
import Agents from "./components/Agents";
import Board from "./components/Board";
import Drawer from "./components/Drawer";
import Memory from "./components/Memory";
import ModelDefaults from "./components/ModelDefaults";
import OverviewView from "./components/Overview";
import Plan from "./components/Plan";
import Sidebar from "./components/Sidebar";
import Toast, { type ToastMsg } from "./components/Toast";
import { Logo } from "./components/Logo";
import { countLabel, issueCounts } from "./counts";
import type { BoardFilters } from "./filters";
import { invalidatedBy } from "./resource";
import { resources } from "./resources";
import { useResource } from "./useResource";
import { requestIsCurrent, responseBelongsToRequest, visibleContext } from "./projectContextGuard";
import {
  browserStoredProjectView,
  persistBrowserProjectView,
  APP_TABS,
  readAppUrlState,
  serializeAppUrlState,
  type AppTab,
  type ProjectView,
} from "./urlState";
import type {
  Health,
  IssueCard,
  IssueDetail,
  Meta,
  ProjectContext,
} from "./types";

export default function App() {
  const initialState = useRef<ReturnType<typeof readAppUrlState> | null>(null);
  if (!initialState.current) {
    initialState.current = readAppUrlState(location.search, browserStoredProjectView());
  }
  const initial = initialState.current;
  const [tab, setTab] = useState<AppTab>(initial.tab);
  const [view, setView] = useState<ProjectView>(initial.view);
  const [project, setProject] = useState(initial.project);
  const [projectContext, setProjectContext] = useState<ProjectContext | null>(null);
  const [projectContextLoading, setProjectContextLoading] = useState(false);
  const [projectContextError, setProjectContextError] = useState<string | null>(null);
  const [projectContextErrorProject, setProjectContextErrorProject] = useState<string | null>(null);
  const [projectContextRefresh, setProjectContextRefresh] = useState(0);
  const projectContextRequest = useRef(0);
  const observedContextRevisions = useRef<Record<string, string>>({});
  const [query, setQuery] = useState("");
  const [filters, setFilters] = useState<BoardFilters>(initial.filters);
  // Per-resource state (resource.ts): loading, failed and empty stay
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
  const [openId, setOpenId] = useState<string | null>(initial.openId);
  const [openDetail, setOpenDetail] = useState<IssueDetail | null>(null);
  const [toast, setToast] = useState<ToastMsg | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const toastTimer = useRef<number>(0);

  const readOnly = meta?.read_only ?? false;
  const actor = meta?.actor ?? "operator (ui)";

  // App selection lives in the URL (`?tab=board&view=list&project=cadence`
  // `issue=CAD-16`) so a refresh or pasted link restores the same view.
  useEffect(() => {
    const read = () => {
      const next = readAppUrlState(location.search, browserStoredProjectView());
      setTab(next.tab);
      setView(next.view);
      setProject(next.project);
      setOpenId(next.openId);
      setFilters(next.filters);
    };
    read();
    addEventListener("popstate", read);
    return () => removeEventListener("popstate", read);
  }, []);

  useEffect(() => {
    const s = serializeAppUrlState(location.search, {
      tab,
      view,
      project,
      openId,
      filters,
    });
    history.replaceState(null, "", location.pathname + (s ? `?${s}` : ""));
  }, [tab, view, project, openId, filters]);

  useEffect(() => {
    persistBrowserProjectView(view);
  }, [view]);

  // The tab readable inside refresh's stable callback — the overview
  // payload costs a daemon probe + gh cache read, so it only fetches
  // while the tab is on screen.
  const tabRef = useRef(tab);
  tabRef.current = tab;

  useEffect(() => {
    const request = ++projectContextRequest.current;
    setProjectContext(null);
    setProjectContextError(null);
    setProjectContextErrorProject(null);
    // Overview shows the same context compactly; Plan is the full view.
    if ((tab !== "plan" && tab !== "overview") || project === "all") {
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
  }, [project, tab, projectContextRefresh]);

  // The open drawer's detail. Read through a ref so the stream and poll
  // handlers stay stable while the drawer changes.
  const openIdRef = useRef(openId);
  openIdRef.current = openId;
  const loadDetail = useCallback(() => {
    const id = openIdRef.current;
    if (!id) return;
    api
      .issue(id)
      .then((next) => {
        if (openIdRef.current === id) setOpenDetail(next);
      })
      .catch(() => {});
  }, []);
  useEffect(loadDetail, [openId, loadDetail]);

  // The overview costs a daemon probe + gh cache read (~seconds), so it
  // is fetched only while its tab is on screen — and whenever it becomes
  // the visible tab. Requests coalesce inside the resource.
  useEffect(() => {
    if (tab === "overview") void resources.overview.refresh();
  }, [tab]);

  // Full re-read: first load, the refresh button, focus and the poll.
  // Each resource joins a request already in flight instead of stacking.
  const refresh = useCallback(() => {
    api.health().then(setHealth).catch(() => setHealth(null));
    api.meta().then(setMeta).catch(() => setMeta(null));
    void resources.projects.refresh();
    void resources.issues.refresh();
    void resources.agents.refresh();
    if (tabRef.current === "overview") void resources.overview.refresh();
    loadDetail();
  }, [loadDetail]);

  useEffect(refresh, [refresh]);

  // Live updates: each /api/stream frame names the resources it
  // invalidates (`{"resources":[...]}`, `event_resources` in src/ui.rs);
  // only those refetch, coalesced per resource. EventSource reconnects
  // on its own; the 30 s poll below stays as the fallback while the
  // stream is down.
  useEffect(() => {
    const es = new EventSource("/api/stream");
    const onEvent = (e: MessageEvent<string>) => {
      for (const name of invalidatedBy(e.data)) {
        if (name === "issue") loadDetail();
        else if (name === "overview") {
          // Hidden overview: skip — it refetches when the tab opens.
          if (tabRef.current === "overview") void resources.overview.invalidate();
        } else void resources[name].invalidate();
      }
    };
    for (const source of ["issues", "agents", "jobs", "monitoring"]) {
      es.addEventListener(source, onEvent);
    }
    return () => es.close();
  }, [loadDetail]);

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
      if (readOnly) {
        say("err", "board is read-only — monitor acknowledgements are disabled");
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
    [readOnly, say],
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
      setOpenDetail((d) => (d && d.id === resp.issue.id ? resp.issue : d));
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
      if (readOnly) {
        say("err", "board is read-only — writes are disabled");
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
    [applyWrite, writeError, readOnly, say],
  );

  const openIssue = useCallback((id: string) => setOpenId(id), []);

  return (
    <div className="grid lg:grid-cols-[208px_minmax(0,1fr)] min-h-screen bg-ink-900">
      <Sidebar
        tab={tab}
        onTab={setTab}
        project={project}
        onProject={setProject}
        projects={projects}
        issues={issuesState}
        projectsError={projectsState.status === "failed" ? projectsState.error : null}
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
          <div className="num text-label text-ink-500">
            <span className="hidden sm:inline text-ink-300">cadence</span>
            <span className="hidden sm:inline"> / </span>
            <span className="text-ink-100">{tab === "board" ? "projects" : tab}</span>
          </div>

          <div className="ml-auto flex items-center gap-2">
            {readOnly && (
              <span
                className="chip bg-warn/10 text-warn"
                title="the server refuses every write — browsing only"
              >
                read-only
              </span>
            )}
            {health && (
              <span
                className={`chip ${
                  health.daemon === "reachable"
                    ? "bg-ink-800 text-ink-400"
                    : "bg-warn/10 text-warn"
                }`}
                title={
                  health.daemon === "reachable"
                    ? "daemon socket reachable"
                    : "daemon socket unreachable — runtime strip is empty"
                }
              >
                daemon {health.daemon}
              </span>
            )}
            <button
              onClick={refresh}
              className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors"
              title="re-read the folders — writes also land here from the API"
            >
              refresh
            </button>
            {!readOnly && (
              <span
                className="hidden sm:inline-flex chip bg-ink-800 text-ink-400"
                title={`writes commit to the tracker as ${actor}`}
              >
                writes: {actor}
              </span>
            )}
          </div>
        </header>

        {menuOpen && (
          <nav className="lg:hidden border-b border-ink-700 bg-ink-875 px-4 py-3 space-y-1">
            <div className="grid grid-cols-3 gap-1.5">
              {APP_TABS.map(
                (t) => (
                  <button
                    key={t}
                    onClick={() => {
                      setTab(t);
                      setMenuOpen(false);
                    }}
                    className={`h-9 rounded text-secondary capitalize ${
                      tab === t
                        ? "bg-accent/15 text-accent font-medium"
                        : "bg-ink-800 text-ink-300"
                    }`}
                  >
                    {t === "board" ? "projects" : t}
                  </button>
                ),
              )}
            </div>
            <div className="slabel pt-2">projects</div>
            <div className="grid gap-1">
              <button
                onClick={() => {
                  setProject("all");
                  setTab("board");
                  setMenuOpen(false);
                }}
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
              </button>
              {projects.map((p) => (
                <button
                  key={p.key}
                  onClick={() => {
                    setProject(p.key);
                    setTab("board");
                    setMenuOpen(false);
                  }}
                  className={`flex items-center justify-between h-8 px-2.5 rounded text-label ${
                    project === p.key
                      ? "bg-accent/15 text-accent"
                      : "text-ink-300 hover:bg-ink-800"
                  }`}
                >
                  {p.key}
                  <span
                    className="num text-micro text-ink-500"
                    title={issuesState.data ? countLabel(issueCounts(issues, p.key)) : undefined}
                  >
                    {p.prefix} {issuesState.data ? issueCounts(issues, p.key).open : "…"}
                  </span>
                </button>
              ))}
            </div>
          </nav>
        )}

        {tab === "overview" && (
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
            onOpenPlan={() => setTab("plan")}
          />
        )}
        {tab === "board" && (
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
            onAgents={() => setTab("agents")}
          />
        )}
        {tab === "agents" && (
          <Agents
            state={agentsState}
            issues={issuesState}
            project={project}
            onOpenIssue={openIssue}
            onRetry={() => void resources.agents.refresh()}
          />
        )}
        {tab === "plan" && (
          <Plan
            project={project}
            context={visibleContext(project, projectContext)}
            contextLoading={projectContextLoading}
            contextError={projectContextErrorProject === project ? projectContextError : null}
            onRetryContext={() => setProjectContextRefresh((value) => value + 1)}
          />
        )}
        {tab === "memory" && <Memory project={project} onError={writeError} />}
        {tab === "settings" && <ModelDefaults />}
      </div>

      {openId && (
        <Drawer
          key={openId}
          id={openId}
          agents={agents}
          projects={projects}
          pmDir={health?.pm_dir}
          detail={openDetail?.id === openId ? openDetail : null}
          readOnly={readOnly}
          actor={actor}
          onClose={() => setOpenId(null)}
          onOpen={openIssue}
          onWrite={applyWrite}
          onError={writeError}
        />
      )}

      <Toast msg={toast} />
    </div>
  );
}
