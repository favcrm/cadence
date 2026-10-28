import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, type WriteResp } from "./lib/api";
import { useLocale } from "./lib/locale";
import Agents from "./features/agents/Agents";
import "./features/agents/recruit.css";
import Apps from "./features/apps/Apps";
import { installationAppHref } from "./features/apps/appViewModel";
import AppDetail from "./features/apps/AppDetail";
import Explorer from "./features/explorer/Explorer";
import CatalogDetail from "./features/explorer/CatalogDetail";
import ManageApp from "./features/explorer/ManageApp";
import AppShell, { type ActiveInstallation } from "./features/app-shell/AppShell";
import { Loading, Notice } from "./features/app-shell/shared/States";
import type { Viewer } from "./features/projects/work";
import { buildAppNav, readLastApp, sectionFromSearch, writeLastApp, type LastApp, type VerifiedApp } from "./features/app-shell/appNav";
import { noteAppScreen } from "./features/app-shell/sidebarPrefs";
import { useInstallations, type InstallationsState } from "./features/workspace-apps/useInstallations";
import { APPS_CHANGED_EVENT } from "./features/workspace-apps/workspaceApps";
import { appLoadErrorCopy } from "./features/explorer/appErrors";
import WorkspaceApp from "./features/workspace-apps/WorkspaceApp";
import Board from "./features/projects/Board";
import WorkItemDetail from "./features/work-items/WorkItemDetail";
import { restoreDetailFocus } from "./features/work-items/focus";
import { applyDecisionDone, homeNeedIssueId, type HomeDecisionOpen, type MyWorkDraft, type WorkItemOrigin } from "./lib/workItems";
import ProjectsOverview from "./features/projects/ProjectsOverview";
import IssuePage from "./features/issues/IssuePage";
import { issuePath, laneFollowsStream, refreshIssueIds } from "./features/issues/model";
import Epics from "./features/projects/Epics";
import Milestones from "./features/projects/Milestones";
import MasterPermissions from "./features/settings/MasterPermissions";
import CanvasDrawer from "./features/canvas/CanvasDrawer";
import Memory from "./features/settings/Memory";
import ModelDefaults from "./features/settings/ModelDefaults";
import PlatformAccount from "./features/settings/PlatformAccount";
import Connections from "./features/settings/Connections";
import CredentialEntry from "./features/connections/CredentialEntry";
import CredentialAuthorize from "./features/connections/CredentialAuthorize";
import EmailSending from "./features/settings/EmailSending";
import SettingsLayout from "./features/settings/SettingsLayout";
import Templates from "./features/templates/Templates";
import Update from "./features/settings/Update";
import Outbox from "./features/outbox/Outbox";
import ThreadsPage from "./features/threads/ThreadsPage";
import OverviewView from "./features/home/Overview";
import Home from "./features/home/Home";
import DecisionsPanel from "./features/decisions/DecisionsPanel";
import MyWork from "./features/home/MyWork";
import { useHomeCount } from "./features/home/todoLocal";
import Context from "./features/projects/Context";
import { contextNavigationSearch } from "./features/projects/contextRoute";
import Workflows from "./features/projects/Workflows";
import Sidebar from "./ui/Sidebar";
import { NavList, ProjectsSection } from "./ui/NavList";
import Wiki from "./features/wiki/Wiki";
import Link from "./ui/Link";
import ProjectFilter from "./ui/ProjectFilter";
import PageState from "./ui/PageState";
import { wikiAvailable } from "./lib/wikiAvailable";
import SectionTabs from "./ui/SectionTabs";
import StatusChips from "./ui/StatusChips";
import AccountMenu from "./ui/AccountMenu";
import VersionLine from "./ui/VersionLine";
import ThemeToggle from "./ui/ThemeToggle";
import LocalLanguage from "./ui/LocalLanguage";
import BuildUpdateNotice from "./ui/BuildUpdateNotice";
import HeaderSearch from "./ui/HeaderSearch";
import NotificationInbox from "./features/threads/NotificationInbox";
import { NotificationToastProvider } from "./features/threads/NotificationToast";
import Setup from "./features/setup/Setup";
import SetupNudge from "./features/setup/SetupNudge";
import AuthBoundary from "./features/auth/AuthBoundary";
import SignIn from "./features/auth/SignIn";
import { metaAccess, wakingBanner } from "./features/auth/access";
import { hireBlock, kickoffBlock as operatorKickoffBlock, writeBlock } from "./features/auth/gate";
import { HireBlock, WriteGate } from "./features/auth/WriteGate";
import { sessionHeaders } from "./lib/sessionKey";
import { onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration, withPrivateOperation } from "./lib/privateSession";
import { buildChanged, serverBuild, subscribeSse, UI_BUILD } from "./lib/sse";
import { runningRelease } from "./lib/fmt";
import { applyDraft, composerField, sessionStore, stashDraft, takeDraft } from "./lib/draft";
import Toast, { type ToastMsg } from "./ui/Toast";
import { IconList } from "./ui/icons";
import type { BoardFilters } from "./lib/filters";
import type { UpdateBanner } from "./lib/types";
import { RESOURCE_NAMES } from "./lib/cache";
import { noteThreadsChanged } from "./features/threads/threadsClient";
import { noteMasterChanged } from "./lib/masterChanged";
import { useHomeUnread } from "./features/app-shell/chat/homeUnread";
import { AssistantUnreadProbes } from "./features/app-shell/chat/unreadShell";
import { LiveUpdates, patchRows } from "./lib/liveUpdates";
import type { Agent, AgentsPayload } from "./lib/types";
import { cache, onIssueUnavailable, resources, type MyWorkList } from "./lib/resources";
import { agentRowForEvent } from "./lib/agentOn";
import { useMaybeResource, useResource } from "./lib/useResource";
import {
  goTo,
  locationHref,
  matchRoute,
  openProject,
  projectScope,
  readLocation,
  showProjectChoices,
  withProject,
  type AppLocation,
  type Route,
  type Screen,
} from "./lib/router";
import { humanThreadsAvailable } from "./lib/humanThreads";
import { navigate, useHref } from "./lib/useLocation";
import { NavigationProvider } from "../screen-ui/src/navigation";
import { hostNavigation } from "./ui/hostNavigation";
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
  navigate(locationHref(next, contextNavigationSearch(current.route, next.route, location.search), current.route), opts);
}

const SCREEN_LABEL: Record<Screen, string> = {
  home: "home",
  overview: "overview",
  projects: "projects",
  apps: "apps",
  appsExplore: "explore",
  appsCatalog: "app",
  appsManage: "manage",
  workspaceApp: "app",
  workspaceAppKey: "app",
  agents: "agents",
  threads: "threads",
  myWork: "my work",
  wiki: "wiki",
  outbox: "outbox",
  setup: "setup",
  settings: "settings",
  credentialEntry: "secure form",
  credentialAuthorize: "authorize entry",
  login: "sign in",
  issue: "issue",
  notFound: "not found",
};

/** Screens that read the overview: Home's Needs-you rail and the full overview. */
const overviewOn = (s: Screen) => s === "home" || s === "overview";

/** Patch a loaded queue row in place; an issue that left the caller drops out. */
function mergePersonalCard(previous: MyWorkList, card: IssueCard): MyWorkList {
  const index = previous.issues.findIndex((row) => row.id === card.id);
  // New/reassigned rows need a fresh scoped projection, never an unrelated
  // board patch or an old write response that could resurrect a removed row.
  if (index < 0) return previous;
  if (card.owner !== previous.owner) {
    return { ...previous, issues: previous.issues.filter((row) => row.id !== card.id) };
  }
  const old = previous.issues[index];
  const issues = previous.issues.slice();
  issues[index] = {
    ...old,
    title: card.title,
    status: card.status,
    priority: card.priority,
    blocked: card.blocked,
    blocked_reason: card.blocked_reason ?? null,
    checks: card.checks,
    counts: { comments: card.counts.comments },
  };
  return { ...previous, issues };
}

export default function App() {
  // CAD-1085: the trusted entry page is deliberately OUTSIDE AuthBoundary.
  // It holds no credential, renders its selector-only "continue on the
  // board" handoff even when signed out, and the auth POST document mounts
  // it with the memory-only entry key.
  const entry = matchRoute(window.location.pathname);
  if (entry.screen === "credentialEntry") {
    return (
      <NavigationProvider adapter={hostNavigation}>
        <CredentialEntry requestId={entry.requestId} />
      </NavigationProvider>
    );
  }
  // Private room alerts share the admitted workspace's concealment and reset lifecycle.
  return <NavigationProvider adapter={hostNavigation}><AuthBoundary>{(meta, recheck) => <NotificationToastProvider><Workspace meta={meta} recheck={recheck} /></NotificationToastProvider>}</AuthBoundary></NavigationProvider>;
}

/** All private routes, subscriptions and effects live below read admission. */
function Workspace({ meta, recheck }: { meta: Meta; recheck: () => Promise<boolean> }) {
  const { t } = useLocale();
  // App selection lives in the URL — the route (`/projects/cadence`) plus
  // `?view=list&issue=CAD-16` — so a refresh or pasted link restores it.
  const href = useHref();
  const loc = useMemo(() => currentLocation(), [href]);
  const { route, view, project, openId, filters } = loc;
  const screen = route.screen;
  useHomeUnread(screen === "home");
  const fullHeight = screen === "home" || screen === "wiki" || screen === "workspaceApp" || screen === "workspaceAppKey" || screen === "threads" ||
    (route.screen === "projects" && route.section === "context" && Boolean(route.slug));
  const search = href.includes("?") ? href.slice(href.indexOf("?")) : "";
  /** The href of a route, carrying the scope and drawer like `goTo`. */
  const hrefFor = (r: Route) => locationHref(goTo(loc, r), contextNavigationSearch(route, r, search), route);
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
  // The Home badge counts pending To do items only (CAD-1216).
  const homeCount = useHomeCount(overviewState.data?.needs_me);
  const issues = issuesState.data ?? [];
  const projects = projectsState.data ?? [];
  const agents = agentsState.data;
  const [health, setHealth] = useState<Health | null | "unavailable">(null);
  const [updateBanner, setUpdateBanner] = useState<UpdateBanner | null>(null);
  // The open drawer's detail, cached per id: reopening a drawer paints
  // the last payload while it revalidates.
  const detailState = useMaybeResource(openId ? resources.issue(openId) : null);
  // CAD-1811: one shared work-item detail. `openId` (the URL) names the
  // issue; the origin and any hosted Home decision are mount state.
  const [workOrigin, setWorkOrigin] = useState<WorkItemOrigin | null>(null);
  const [decision, setDecision] = useState<HomeDecisionOpen | null>(null);
  const workOpener = useRef<Element | null>(null);
  // Late decision replies read the live selection, never the render they
  // were created in (CAD-1811 review): a dismissed/replaced decision must
  // not close or replace what the operator is looking at now.
  const decisionRef = useRef<HomeDecisionOpen | null>(null);
  decisionRef.current = decision;
  // A deep-linked `?issue=` has no opener in this session: its origin is the
  // screen showing it. An explicit open records its own origin.
  const effectiveOrigin: WorkItemOrigin = workOrigin ??
    (screen === "home" ? "home" : screen === "myWork" ? "myWork" : screen === "threads" ? "threads" : "issue");
  const personalState = useMaybeResource(screen === "myWork" ? resources.myWork : null);
  // The peek's membership: one issue, asked of the server (the list holds one page).
  const membershipState = useMaybeResource(screen === "myWork" && openId ? resources.myWorkIssue(openId) : null);
  const personalWanted = useRef(screen === "myWork");
  personalWanted.current = screen === "myWork";
  const [personalDrafts] = useState(() => new Map<string, MyWorkDraft>());
  const [, notifyPersonalDrafts] = useState(0);
  useEffect(() => onPrivateSessionReset(() => personalDrafts.clear()), [personalDrafts]);
  useEffect(() => onIssueUnavailable((id) => {
    const suffix = `:${id}`;
    let changed = false;
    for (const key of personalDrafts.keys()) {
      if (key.endsWith(suffix)) changed = personalDrafts.delete(key) || changed;
    }
    if (changed) notifyPersonalDrafts((version) => version + 1);
  }), [personalDrafts]);
  const personalReturn = useRef<{ id: string; scroll: number } | null>(null);
  const previousScreen = useRef(screen);
  const restorePersonal = useRef(false);
  const [toast, setToast] = useState<ToastMsg | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [accountOpen, setAccountOpen] = useState(false);
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
  // Hosted boards upload with one multipart POST to the tenant Worker's wiki (AOS-418, which
  // also produces `Meta.wiki`); local boards use transfer jobs.
  const multipartUpload = meta?.hosted === true;
  const viewerSub = meta?.session?.user?.sub ?? null;
  // CAD-1423: change signal for the notification inbox. The verified identity is
  // still read from the daemon by the inbox itself; this only says when to re-check.
  const authEpoch = JSON.stringify([meta?.signed_in ?? null, meta?.session?.id ?? null, meta?.session?.user?.sub ?? null, meta?.read_only ?? null]);
  const personalGeneration = privateSessionGeneration();
  const personalIdentity = JSON.stringify([personalGeneration, meta.session?.id, meta.session?.user?.sub, meta.actor]);

  // CAD-1312: the header's organisation label is the hosted board's
  // configured company display name (`GET /api/platform-account`, already
  // projected server-side). Local and tailnet boards have no configured
  // organisation — the label stays the neutral "Cadence", never a
  // hostname, user or project name. One nonblocking read per page load;
  // a failure or missing name leaves the fallback.
  const [orgName, setOrgName] = useState("Cadence");
  useEffect(() => {
    if (meta === null) return;
    if (meta.platform_account_configured !== true) {
      setOrgName("Cadence");
      return;
    }
    const abort = new AbortController();
    void withPrivateOperation(async (operation) => {
      const response = await operation.fetch("/api/platform-account", { headers: sessionHeaders() });
      const data: unknown = response.ok ? await response.json() : null;
      operation.assertCurrent();
      const account = data && typeof data === "object" && "account" in data ? data.account : null;
      const company = account && typeof account === "object" && "company" in account ? account.company : null;
      const name = company && typeof company === "object" && "name" in company && typeof company.name === "string" ? company.name.trim() : "";
      setOrgName(name || "Cadence");
    }, abort.signal).catch(() => undefined);
    return () => abort.abort();
  }, [meta?.platform_account_configured, meta === null]);

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
    // The peek's quick actions need a current membership too, so a refresh
    // recovers a failed membership read without reopening the peek.
    if (personalWanted.current && openIdRef.current) void resources.myWorkIssue(openIdRef.current).invalidate();
  }, []);
  useEffect(() => {
    if (openId) void resources.issue(openId).revalidate();
  }, [openId]);
  useEffect(() => {
    setWorkOrigin(null);
  }, [screen]);
  // A route change, Back/Forward, "Open full issue" or any other selection
  // replaces or removes the hosted decision: retire it at once so a late
  // reply for that opening cannot close or advance what is shown now.
  useEffect(() => {
    if (decision === null) return;
    if (homeNeedIssueId(decision.need) === openId && effectiveOrigin === "home") return;
    decisionRef.current = null;
    setDecision(null);
  }, [decision, openId, effectiveOrigin]);

  const invalidatePersonal = useCallback(() => {
    if (personalWanted.current) void resources.myWork.invalidate();
    else resources.myWork.markInvalid();
    cache.invalidate("myWorkIssue");
  }, []);
  useEffect(() => {
    if (screen === "myWork" && openId) void resources.myWorkIssue(openId).revalidate();
  }, [screen, openId]);

  useEffect(() => {
    if (screen === "myWork" && previousScreen.current !== "myWork") restorePersonal.current = true;
    previousScreen.current = screen;
    if (screen !== "myWork" || !restorePersonal.current || !personalReturn.current ||
        !personalState?.data || personalState.status === "failed" || personalState.status === "stale") return;
    const saved = personalReturn.current;
    const frame = requestAnimationFrame(() => {
      restorePersonal.current = false;
      window.scrollTo(0, saved.scroll);
      let trigger: HTMLElement | null = null;
      document.querySelectorAll<HTMLElement>("[data-my-work-inspect]").forEach((element) => {
        if (element.dataset.myWorkInspect === saved.id) trigger = element;
      });
      const target = trigger ?? document.querySelector<HTMLElement>("[data-my-work] h1");
      if (target) {
        if (!trigger) target.tabIndex = -1;
        target.focus({ preventScroll: true });
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [screen, personalState?.data, personalState?.status]);

  // The overview costs a daemon probe + gh cache read (~seconds), so it
  // is fetched only while a screen that reads it is on screen —
  // revalidated when that screen comes back, painting the last payload
  // meanwhile. Requests coalesce.
  useEffect(() => {
    if (overviewWanted) void resources.overview.revalidate();
  }, [overviewWanted]);

  // Bootstrap is owned by AuthBoundary. A private refresh first verifies
  // the current session; failed verification pauses this entire Activity.
  const healthRequest = useRef(0);
  const refresh = useCallback(() => {
    const generation = privateSessionGeneration();
    void recheck().then((verified) => {
      if (!verified || !privateReadsAllowed() || generation !== privateSessionGeneration()) return;
      const healthProbe = ++healthRequest.current;
      api.health()
        .then((next) => {
          if (healthProbe === healthRequest.current) setHealth(next);
        })
        .catch(() => {
          if (healthProbe === healthRequest.current) setHealth("unavailable");
        });
      void resources.projects.refresh();
      void resources.issues.refresh();
      void resources.agents.refresh();
      if (overviewWantedRef.current) void resources.overview.refresh();
      if (personalWanted.current) void resources.myWork.refresh();
      loadDetail();
    });
  }, [loadDetail, recheck]);
  const retryAccess = useCallback(() => { void recheck(); }, [recheck]);
  // An unsigned reader (admitted only by --allow-anonymous-reads) is a
  // resolved non-operator: the server omits `operator` for it. Only a
  // claimed session whose proof is missing is genuinely unavailable.
  const access = metaAccess(meta);
  const { metaUnavailable } = access;
  const [recheckFinished, setRecheckFinished] = useState(false);
  useEffect(() => {
    setRecheckFinished(false);
    if (!metaUnavailable) return;
    const timer = window.setTimeout(() => setRecheckFinished(true), 60_000);
    return () => window.clearTimeout(timer);
  }, [metaUnavailable]);
  useEffect(() => { refresh(); }, [refresh]);

  const reconcile = useCallback(() => {
    // A GET begun before subscription can still be in flight. Invalidate
    // before refresh so it schedules one trailing post-subscription read;
    // refresh alone would merely join the potentially stale request.
    for (const key of RESOURCE_NAMES) {
      if (key === "overview" && !overviewWantedRef.current) resources.overview.markInvalid();
      else cache.invalidate(key);
    }
    cache.invalidate("lane");
    invalidatePersonal();
    noteThreadsChanged();
    noteMasterChanged();
    refresh();
  }, [refresh, invalidatePersonal]);

  const liveUpdates = useRef<LiveUpdates | null>(null);
  useEffect(() => {
    const updates = new LiveUpdates({
      resync: reconcile,
      invalidate: (key) => {
        // Main refetches the lane store when a frame names issue, agents,
        // or jobs. Entity patches queue that through LiveUpdates; legacy
        // resource names still have to.
        if (laneFollowsStream([key])) cache.invalidate("lane");
        if (key === "issues" || key === "issue" || key === "agents") invalidatePersonal();
        if (key === "threads") noteThreadsChanged();
        if (key === "master") noteMasterChanged();
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
          resources.myWork.mutate((body) => data.op === "delete"
            ? { ...body, issues: body.issues.filter((row) => row.id !== data.id) }
            : mergePersonalCard(body, data.issue as IssueCard));
          invalidatePersonal();
        } else if (event.type === "agent") {
          if (typeof data.id !== "string" || !["upsert", "delete"].includes(String(data.op)) ||
              (data.op === "upsert" && (!data.agent || typeof data.agent !== "object"))) {
            reconcile(); return;
          }
          if (resources.agents.get().data === null) {
            void resources.agents.invalidate();
          } else resources.agents.mutate((value) => ({ ...value,
            agents: patchRows(
              value.agents,
              data.id as string,
              data.op,
              // A delete tombstone is { op, id } with no row; only an upsert
              // carries a row worth normalizing.
              agentRowForEvent(data.op as "upsert" | "delete", data.agent as Agent | undefined) as Agent,
              (row) => row.alias,
            ),
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
      events: ["hello", "heartbeat", "issues", "agents", "jobs", "monitoring", "issue", "agent", "agent_meta", "plan", "aggregates", "threads", "master"],
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
  }, [reconcile, invalidatePersonal]);

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
      // A response can arrive after a newer GET retired this record. A
      // successful current read, not an older write response, restores it.
      if (resources.issue(resp.issue.id).get().discarded) {
        void resources.issue(resp.issue.id).invalidate();
        invalidatePersonal();
        return;
      }
      resources.issues.mutate((prev) => {
        const i = prev.findIndex((c) => c.id === resp.card.id);
        if (i === -1) return [...prev, resp.card];
        const next = prev.slice();
        next[i] = resp.card;
        return next;
      });
      resources.issue(resp.issue.id).write(() => resp.issue);
      resources.myWork.mutate((body) => mergePersonalCard(body, resp.card));
      // A status or owner change can move the row to another tab or person.
      invalidatePersonal();
      const warns = (resp.warnings ?? []).filter(Boolean);
      if (warns.length) {
        say("warn", `${verb} · ${warns.join(" · ")}`);
      } else {
        say("ok", verb);
      }
    },
    [say, invalidatePersonal],
  );

  const writeError = useCallback(
    (e: unknown, verb: string) => {
      const err = e as ApiError;
      // A conflict carries the fresh card — resync so the board shows
      // the state that won.
      if (err.card && !resources.issue(err.card.id).get().discarded) {
        resources.issues.mutate((prev) => {
          const i = prev.findIndex((c) => c.id === err.card!.id);
          if (i === -1) return prev;
          const next = prev.slice();
          next[i] = err.card!;
          return next;
        });
        resources.myWork.mutate((body) => mergePersonalCard(body, err.card!));
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

  /**
   * Open the shared detail for one issue. `nextDecision` carries the Home
   * decision that owns this opening; every other opener retires any hosted
   * decision immediately, so a late reply can never touch its successor.
   */
  const openWorkItem = useCallback((id: string, origin: WorkItemOrigin, nextDecision: HomeDecisionOpen | null = null) => {
    if (!openIdRef.current) workOpener.current = document.activeElement;
    decisionRef.current = nextDecision;
    setDecision(nextDecision);
    setWorkOrigin(origin);
    update((c) => ({ ...c, openId: id }));
  }, []);
  const openIssue = useCallback((id: string) => openWorkItem(id, "issue"), [openWorkItem]);
  const openHomeWork = useCallback((id: string) => openWorkItem(id, "home"), [openWorkItem]);
  const closeIssue = useCallback(() => update((c) => ({ ...c, openId: null })), []);
  const rememberPersonal = useCallback((id: string) => {
    personalReturn.current = { id, scroll: window.scrollY };
  }, []);
  const openPersonal = useCallback((id: string) => {
    rememberPersonal(id);
    openWorkItem(id, "myWork");
  }, [rememberPersonal, openWorkItem]);
  const openDecision = useCallback((open: HomeDecisionOpen) => {
    const id = homeNeedIssueId(open.need);
    if (id === null) return;
    openWorkItem(id, "home", open);
  }, [openWorkItem]);
  /** Close the shared detail; My work gets its reading position back. */
  const closeWorkItem = useCallback(() => {
    const saved = effectiveOrigin === "myWork" ? personalReturn.current : null;
    setDecision(null);
    closeIssue();
    restoreDetailFocus(workOpener.current);
    if (saved !== null) requestAnimationFrame(() => window.scrollTo(0, saved.scroll));
  }, [effectiveOrigin, closeIssue]);
  /** A Home decision landed: mark the card, then move to the next reading item. */
  const finishDecision = useCallback((done: HomeDecisionOpen, _key: string, text: string) => {
    const outcome = applyDecisionDone(done, text, {
      decision: decisionRef.current,
      issueId: openIdRef.current,
      origin: effectiveOrigin,
    });
    if (outcome.kind === "ignored") return;
    if (outcome.kind === "closed") {
      closeWorkItem();
      return;
    }
    const id = homeNeedIssueId(outcome.next.need);
    if (id === null) closeWorkItem();
    else openWorkItem(id, "home", outcome.next);
  }, [effectiveOrigin, closeWorkItem, openWorkItem]);
  const openAgent = useCallback(
    (alias: string | null) => update((c) => ({ ...c, route: { screen: "agents", alias } })),
    [],
  );
  // CAD-561: the draining banner. Cheap (the daemon's own view), polled
  // board-wide so an update is visible on every page, not only Settings.
  // Reads never overlap: the next one is scheduled after the last settles.
  // A hidden tab reads nothing; a visible tab reads at once on return.
  useEffect(() => {
    let stop = false;
    let inFlight = false;
    let timer: number | null = null;
    const clear = () => {
      if (timer !== null) window.clearTimeout(timer);
      timer = null;
    };
    const tick = () => {
      clear();
      if (stop || inFlight || document.hidden) return;
      inFlight = true;
      api
        .updateBanner()
        .then((next) => {
          if (!stop) setUpdateBanner(next);
        })
        .catch(() => undefined)
        .finally(() => {
          inFlight = false;
          if (!stop && !document.hidden) timer = window.setTimeout(tick, 15000);
        });
    };
    const onVisibility = () => {
      if (document.hidden) clear();
      else tick();
    };
    tick();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      stop = true;
      clear();
      document.removeEventListener("visibilitychange", onVisibility);
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
  // Board-level Apps menu (CAD-784, CAD-1116): the installed apps come
  // from the verified installation list (or the shell's receipt for the
  // open one), never from the bare route, so a forged installId in the
  // URL produces no entry. The active or last-used app also lists its
  // sections, and that stays on every screen.
  const [activeApp, setActiveApp] = useState<ActiveInstallation | null>(null);
  const reportInstallation = useCallback((info: ActiveInstallation | null) => {
    setActiveApp((prev) => {
      if (prev === null && info === null) return prev;
      if (
        prev !== null && info !== null &&
        prev.installId === info.installId && prev.kind === info.kind && prev.title === info.title &&
        JSON.stringify(prev.navigation) === JSON.stringify(info.navigation)
      ) {
        return prev;
      }
      return info;
    });
  }, []);
  // Loading, error and ready stay separate: a failed refresh keeps the last
  // good list and says so; a busy daemon is retried before it does.
  // Only the operator may read the list; member and agent sessions would get a
  // 403 and a permanent notice, so they do not ask. CAD-1537: a signed-in
  // viewer of a `--read-only` board reads it as the operator's session does.
  const screenReader = meta?.operator === false && meta.signed_in === true &&
    (meta.session?.user?.role === "member" || boardReadOnly);
  const [appsTick, setAppsTick] = useState(0);
  useEffect(() => {
    const bump = () => setAppsTick((n) => n + 1);
    window.addEventListener(APPS_CHANGED_EVENT, bump);
    return () => window.removeEventListener(APPS_CHANGED_EVENT, bump);
  }, []);
  const listReader = meta?.operator === true || screenReader;
  // CAD-1537: a member resolves app keys from `/api/app-home`, never the
  // operator-only installed-app list.
  const memberHome = screenReader && !boardReadOnly;
  const installations = useInstallations(listReader, `${route.screen}:${appsTick}`, memberHome ? "home" : "list");
  // A removed or disabled app is not an app to open: drop it from the menu.
  // It stays visible on the Apps surface, which is where it is managed.
  const installed: VerifiedApp[] = useMemo(
    () => (installations.list ?? [])
      .filter((i) => i.removed == null && (i.state ?? "active") === "active")
      .map((i) => ({ installId: i.install_id, kind: i.name, title: i.title || i.name })),
    [installations.list],
  );
  const keyInstallations = route.screen === "workspaceAppKey"
    ? (installations.list ?? []).filter((installation) =>
        installation.name === route.appKey &&
        installation.removed == null &&
        (installation.state ?? "active") === "active",
      )
    : [];
  let routeInstallId: string | null = null;
  if (route.screen === "workspaceApp") routeInstallId = route.installId;
  else if (route.screen === "workspaceAppKey" && installations.error === null && keyInstallations.length === 1) {
    routeInstallId = keyInstallations[0].install_id;
  }
  // CAD-1438: resolve an install id to its canonical `/apps/<key>` path
  // against the operator-verified list only — a removed install, an
  // unknown id or a shared key answers null (retired), never a guess.
  const resolveAppPath = useCallback(
    (installId: string) => installationAppHref(installations.list, installId),
    [installations.list],
  );
  const onAppScreen = route.screen === "workspaceApp" || route.screen === "workspaceAppKey";
  const receipt = onAppScreen && activeApp !== null && activeApp.installId === routeInstallId ? activeApp : null;
  const menuApps = receipt === null ? installed
    : installed.some((a) => a.installId === receipt.installId)
      ? installed.map((app) => app.installId === receipt.installId ? { ...app, navigation: receipt.navigation } : app)
      : [...installed, receipt];
  const activeId = onAppScreen && routeInstallId !== null
    ? menuApps.find((a) => a.installId === routeInstallId)?.installId ?? null
    : null;
  // CAD-1193: one viewer for every consumer — `access` splits the
  // `operator: null` unknown into "checking" (a probe is still in
  // flight or has never run) and "unavailable" (a completed probe
  // could not answer), and `onRetryAccess` gives the surfaces a real
  // re-check instead of a reload. `operator === true` stays the only
  // grant; both unknowns and every failure grant nothing.
  const viewer = {
    readOnly,
    operator: access.operator,
    access: access.access,
    onRetryAccess: retryAccess,
    screenReader,
  };
  const [last, setLast] = useState<LastApp | null>(() => readLastApp());
  const selectedMenuApp = menuApps.find((app) => app.installId === activeId);
  const activeSection = selectedMenuApp ? sectionFromSearch(search, selectedMenuApp.navigation) : null;
  useEffect(() => {
    if (activeId === null) return;
    setLast((prev) => {
      if (prev !== null && prev.installId === activeId && prev.section === activeSection) return prev;
      const next = { installId: activeId, section: activeSection };
      writeLastApp(next);
      return next;
    });
  }, [activeId, activeSection]);
  // CAD-1692: the sidebar unfolds Apps on entering an app, judged from the route.
  useEffect(() => { noteAppScreen(onAppScreen, activeId); }, [onAppScreen, activeId]);
  const appMenu = {
    ...buildAppNav({
      apps: menuApps,
      onAppScreen,
      activeId,
      activeHref: href,
      last,
      installHref: (installId) => {
        // CAD-1438: app menu entries emit only the canonical key route.
        // A receipt-only app (in the menu but not the list) or a shared
        // key cannot spell one — the entry falls back to the inert
        // current href rather than the retired installation-ID route.
        const href2 = resolveAppPath(installId);
        if (href2 === null) return href;
        // `installationAppHref` returns the encoded path; the route stores
        // the decoded key, and valid keys carry no encodable characters.
        const next: Route = { screen: "workspaceAppKey", appKey: decodeURIComponent(href2.slice("/apps/".length)) };
        const targetSearch = new URLSearchParams(search);
        if (installId !== activeId) {
          for (const key of ["crm", "record", "ctx", "appview", "segment", "conversation", "install"]) targetSearch.delete(key);
        }
        const nextSearch = targetSearch.toString();
        return locationHref(goTo(loc, next), contextNavigationSearch(route, next, nextSearch));
      },
    }),
    notice: installations.retrying
      ? { text: "The workspace is busy. Retrying the app list…", retrying: true, onRetry: installations.retry }
      : installations.error !== null
        ? { text: appLoadErrorCopy(installations.error, "Your apps didn't load."), retrying: false, onRetry: installations.retry }
        : null,
  };
  // One element for both account menus (sidebar row, header avatar) so they cannot drift.
  const versionLine = (
    <VersionLine
      release={runningRelease(meta)}
      build={UI_BUILD}
      href={hrefFor({ screen: "settings", section: "update" })}
      updatePending={updateBanner !== null || staleBuild !== null}
    />
  );

  return (
    <WriteGate.Provider value={meta === null
      ? metaUnavailable
        ? "The access check could not confirm this session — writes stay off until it answers."
        : "Checking write access…"
      : block}>
    <HireBlock.Provider value={meta === null
      ? metaUnavailable
        ? "The access check could not confirm this session — hiring stays off until it answers."
        : "Checking write access…"
      : hireBlock(meta)}>
    <div
      data-app-shell
      className={`grid lg:grid-cols-[208px_minmax(0,1fr)] bg-ink-900 ${
        fullHeight ? "app-workspace h-[100dvh] grid-rows-[minmax(0,1fr)] overflow-hidden" : "min-h-screen"
      }`}
    >
      <Sidebar
        noWiki={!wikiAvailable(meta)}
        screen={screen === "workspaceApp" || screen === "workspaceAppKey" ? "apps" : screen}
        navHref={hrefFor}
        appMenu={appMenu}
        homeCount={homeCount}
        project={navProject}
        projectHref={projectHref}
        projects={projects}
        issues={issuesState}
        projectsError={projectsState.status === "failed" ? projectsState.error : null}
        signedIn={meta?.signed_in ?? null}
        accountOpen={accountOpen}
        account={<AccountMenu
          onOpenChange={setAccountOpen}
          meta={meta}
          actor={actor}
          mayWrite={!readOnly}
          onChange={refresh}
          trigger="row"
          placement="above-start"
          settingsHref={hrefFor({ screen: "settings", section: "models" })}
          footer={versionLine}
        />}
      />

      {/* Workspaces fill the dynamic viewport; their panes own scrolling.
          Other screens retain the natural document flow. */}
      <div data-canvas-main className={`min-w-0 flex flex-col ${fullHeight ? "h-full min-h-0" : ""}`}>
        <header className="app-header sticky top-0 z-10 flex items-center gap-2 sm:gap-3 px-4 lg:px-8 border-b border-ink-700 bg-ink-900/95 backdrop-blur">
          <button
            onClick={() => setMenuOpen((o) => !o)}
            className="header-menu header-icon -ml-1 shrink-0 gap-1 text-ink-200"
            aria-label={t("Open navigation")}
            aria-expanded={menuOpen}
            aria-controls="mobile-navigation"
          >
            <IconList size={16} />
          </button>
          <nav className="header-breadcrumb min-w-0 flex-1" aria-label={t("Breadcrumb")}>
            {/* The organisation's display name, or the board's neutral
                label — never a hostname or project mistaken for an org. */}
            <span className="header-org hidden truncate font-medium text-ink-100 sm:inline" title={orgName}>{orgName}</span>
            <span className="hidden sm:inline text-ink-600" aria-hidden="true">/</span>
            {route.screen === "projects" && route.slug ? (
              <>
                <Link href={hrefFor({ screen: "projects", slug: null, section: "overview" })} className="hidden sm:inline-flex">{t("Projects")}</Link>
                <span className="hidden sm:inline text-ink-600" aria-hidden="true">/</span>
                <span className="truncate text-ink-100" title={route.slug}>{route.slug}</span>
              </>
            ) : <span className="truncate text-ink-200 capitalize">{route.screen === "issue" ? route.id : t(SCREEN_LABEL[screen])}</span>}
          </nav>

          <HeaderSearch noWiki={!wikiAvailable(meta)} issues={issuesState} route={route} navHref={hrefFor} />

          <div className="ml-auto flex items-center gap-1 shrink-0">
            {screen === "home" && meta?.signed_in === true && <DecisionsPanel />}
            {humanThreadsAvailable(meta) && <NotificationInbox authEpoch={authEpoch} />}
            <StatusChips
              variant="header"
              readOnly={boardReadOnly}
              health={health}
              hosted={meta?.hosted === true}
              onRefresh={refresh}
            >
              <SignIn meta={meta} onChange={refresh} access={viewer.access} recheckFinished={recheckFinished} onRetryWorkspace={reload} />
              {staleBuild !== null && staleBuild !== dismissedBuild && (
                <BuildUpdateNotice onReload={reload} onDismiss={() => setDismissedBuild(staleBuild)} />
              )}
            </StatusChips>
            {/* Desktop identity lives in the sidebar footer row; signed-out viewers keep the one-click theme button. */}
            {meta?.signed_in === true ? <div className="flex lg:hidden"><AccountMenu
              meta={meta}
              actor={actor}
              mayWrite={!readOnly}
              onChange={refresh}
              trigger="avatar"
              placement="below-end"
              settingsHref={hrefFor({ screen: "settings", section: "models" })}
              footer={versionLine}
            /></div> : <><LocalLanguage /><ThemeToggle /></>}
          </div>
        </header>

        <AssistantUnreadProbes apps={installed} />

        {updateBanner && (
          <details className="update-notice border-b border-amber-500/30 bg-amber-500/5 px-4 lg:px-8 py-2 text-label text-ink-300">
            <summary className="cursor-pointer"><span className="font-medium text-ink-200">{t("Updating Cadence")}</span> · {updateBanner.count > 0 ? `Waiting for ${updateBanner.count} active turn${updateBanner.count === 1 ? "" : "s"}` : t("No active turns remaining")}<span className="text-ink-500 ml-3">{t("Details")}</span></summary>
            <p className="num text-micro text-ink-500 mt-2 break-all">{updateBanner.pending.from ?? "?"} → {updateBanner.pending.target} · {updateBanner.pending.phase} · {updateBanner.pending.by}</p>
          </details>
        )}

        {menuOpen && (
          <div id="mobile-navigation" className="lg:hidden border-b border-ink-700 bg-ink-875 px-4 py-3 space-y-1">
            <NavList
              noWiki={!wikiAvailable(meta)}
              screen={screen === "workspaceApp" || screen === "workspaceAppKey" ? "apps" : screen}
              navHref={hrefFor}
              appMenu={appMenu}
              homeCount={homeCount}
              label={t("Workspace")}
              onNavigate={() => setMenuOpen(false)}
            />
            <div className="pt-2">
              <ProjectsSection
                project={navProject}
                projectHref={projectHref}
                projects={projects}
                issues={issuesState}
                projectsError={projectsState.status === "failed" ? projectsState.error : null}
                onNavigate={() => setMenuOpen(false)}
              />
            </div>
            {/* The header carries these as icons under lg — here they keep
                their words, so the meaning is one tap away on touch. */}
            <div className="slabel pt-2">{t("board")}</div>
            <div role="region" aria-label={t("Board")} className="flex flex-wrap items-center gap-1.5">
              <StatusChips
                variant="menu"
                readOnly={boardReadOnly}
                health={health}
                hosted={meta?.hosted === true}
                onRefresh={refresh}
              >
                <SignIn meta={meta} onChange={refresh} access={viewer.access} recheckFinished={recheckFinished} onRetryWorkspace={reload} />
              </StatusChips>
            </div>
          </div>
        )}

        {projectScope(route) === "chips" && showProjectChoices(projects.length, project) && (
          <div className="px-4 lg:px-8 pt-4">
            <ProjectFilter project={project} projects={projects} hrefFor={filterHref} />
          </div>
        )}
        {route.screen === "myWork" && (
          <MyWork
            view={route.view ?? {}}
            onView={(view) => update((c) => ({ ...c, route: { screen: "myWork", view } }), { replace: true })}
            onOpen={openPersonal}
            onDetails={rememberPersonal}
          />
        )}

        {screen === "home" && (
          <Home
            readOnly={readOnly}
            hosted={meta ? meta.hosted === true || meta.session?.origin === "public" : null}
            viewerSub={meta?.session?.user?.sub ?? null}
            viewerIsMember={meta?.session?.user?.role === "member"}
            sessionId={meta?.session?.id ?? null}
            operator={meta?.operator === true}
            overview={overviewState}
            onOpenIssue={openHomeWork}
            onOpenDecision={openDecision}
            overviewHref={hrefFor({ screen: "overview" })}
            permissionsHref={hrefFor({ screen: "settings", section: "permissions" })}
            resolveAppPath={resolveAppPath}
            setupNotice={<SetupNudge readOnly={meta ? boardReadOnly : null} />}
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
            noWiki={!wikiAvailable(meta)}
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
              ...(!wikiAvailable(meta) ? [] : [{
                label: "Context",
                href: hrefFor({
                  screen: "projects",
                  slug: route.screen === "issue" ? route.project : route.slug,
                  section: "context",
                }),
                on: route.screen === "projects" && route.section === "context",
              }]),
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
            hosted={meta?.hosted === true}
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
            viewer={viewer}
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
            viewer={viewer}
            onOpenIssue={openIssue}
            onHome={() => goRoute({ screen: "home" })}
          />
        )}
        {route.screen === "projects" && route.section === "context" && (
          <Context
            noWiki={!wikiAvailable(meta)}
            multipartUpload={multipartUpload}
            viewerSub={viewerSub}
            readOnly={readOnly}
            actor={actor}
            onToast={say}
            navHref={hrefFor}
            project={project}
          />
        )}
        {route.screen === "apps" && !route.project && (
          <Apps viewer={viewer} />
        )}
        {route.screen === "appsExplore" && (
          <Explorer viewer={viewer} />
        )}
        {route.screen === "appsCatalog" && (
          <CatalogDetail id={route.id} viewer={viewer} />
        )}
        {route.screen === "appsManage" && (
          <ManageApp key={route.installId} installId={route.installId} viewer={viewer} />
        )}
        {(route.screen === "workspaceApp" || route.screen === "workspaceAppKey") && (
          <WorkspaceAppEntry
            route={route}
            search={search}
            installations={installations}
            viewer={viewer}
            // CAD-1537: a member resolves only the canonical app-key route;
            // the retired id route stays unavailable to it.
            listReader={listReader && !(memberHome && route.screen === "workspaceApp")}
            recheckFinished={recheckFinished}
            onRetryWorkspace={reload}
            onInstallation={reportInstallation}
            onBack={() => goRoute({ screen: "apps", project: null, name: null })}
          />
        )}
        {route.screen === "apps" && route.project && route.name && (
          <AppDetail
            project={route.project}
            name={route.name}
            viewer={viewer}
            onOpenIssue={openIssue}
            onHome={() => goRoute({ screen: "home" })}
          />
        )}
        {route.screen === "agents" && (
          <Agents
            state={agentsState}
            issues={issuesState}
            open={route.alias}
            tab={route.tab ?? "overview"}
            recruit={route.recruit === true}
            ask={route.ask === true}
            onAsk={() =>
              update((c) => ({ ...c, route: { screen: "agents", alias: null, ask: true } }))
            }
            onOpenAgent={openAgent}
            onTab={(tab) =>
              update((c) => ({
                ...c,
                route: { screen: "agents", alias: route.alias, ...(tab === "overview" ? {} : { tab }) },
              }))
            }
            onRecruit={(on) =>
              update((c) => ({ ...c, route: { screen: "agents", alias: null, ...(on ? { recruit: true } : {}) } }))
            }
            onOpenIssue={openIssue}
            onRetry={() => void resources.agents.refresh()}
          />
        )}
        {route.screen === "wiki" && (
          !wikiAvailable(meta) ? (
            <PageState
              title={t("The wiki isn’t available on this board.")}
              actions={<Link href={hrefFor({ screen: "home" })}>{t("Back to Home")}</Link>}
            >
              {t("This board carries the tracker, apps and the assistant.")}
            </PageState>
          ) : (
            <Wiki
              multipartUpload={multipartUpload}
              viewerSub={viewerSub}
              route={route}
              navHref={hrefFor}
              readOnly={readOnly || meta === null}
              actor={actor}
              onToast={say}
            />
          )
        )}
        {route.screen === "threads" && (
          <ThreadsPage
            key={meta.session?.id ?? ""}
            operator={meta.operator === true}
            onOpenWorkItem={(id) => openWorkItem(id, "threads")}
          />
        )}
        {route.screen === "credentialEntry" && (
          <CredentialEntry key={route.requestId} requestId={route.requestId} />
        )}
        {route.screen === "credentialAuthorize" && (
          <CredentialAuthorize key={route.requestId} requestId={route.requestId} />
        )}
        {screen === "outbox" && <Outbox operator={meta?.operator === true} onOpenIssue={openIssue} />}
        {screen === "setup" && (
          <Setup settingsHref={hrefFor({ screen: "settings", section: "models" })} readOnly={meta ? boardReadOnly : null} />
        )}
        {route.screen === "settings" && (
          <SettingsLayout
            groups={[
              {
                label: "Workspace",
                items: [
                  { label: "Agents", href: hrefFor({ screen: "settings", section: "models" }), on: route.section === "models" },
                  { label: "Connections", href: hrefFor({ screen: "settings", section: "connections" }), on: route.section === "connections" },
                  { label: "Email sending", href: hrefFor({ screen: "settings", section: "email" }), on: route.section === "email" },
                ],
              },
              {
                label: "System",
                items: [
                  { label: "Status", href: hrefFor({ screen: "settings", section: "update" }), on: route.section === "update" },
                  ...(meta?.platform_account_configured ? [{ label: "Account", href: hrefFor({ screen: "settings", section: "account" }), on: route.section === "account" }] : []),
                  { label: "Master permissions", href: hrefFor({ screen: "settings", section: "permissions" }), on: route.section === "permissions" },
                ],
              },
            ]}
          >
            {route.section === "memory" && (
              <Memory project={project} projects={projects} projectHref={filterHref} onError={writeError} />
            )}
            {route.section === "models" && <ModelDefaults />}
            {route.section === "connections" && (
              <Connections viewer={viewer} page={route.page} />
            )}
            {route.section === "email" && <EmailSending viewer={viewer} />}
            {route.section === "templates" && <Templates />}
            {route.section === "account" && <PlatformAccount />}
            {route.section === "update" && <Update viewer={viewer} health={health} />}
            {route.section === "permissions" && <MasterPermissions viewer={{ ...viewer, boardReadOnly, signedIn: meta?.signed_in === true, sessionId: meta?.session?.id ?? null }} />}
          </SettingsLayout>
        )}
        {screen === "notFound" && (
          <main className="px-4 lg:px-8 pt-10 pb-9">
            <h1 className="text-section font-semibold text-ink-100">{t("Nothing lives here")}</h1>
            <p className="text-body text-ink-400 mt-2">
              <span className="num break-all">{route.screen === "notFound" ? route.path : ""}</span> {t("is not a page of this board.")}{" "}
              <Link href={hrefFor({ screen: "home" })} className="lnk">
                {t("Go home")}
              </Link>
            </p>
          </main>
        )}
      </div>

      {openId && route.screen !== "issue" && (
        <WorkItemDetail
          key={`${openId}:${decision?.need.key ?? ""}`}
          id={openId}
          origin={effectiveOrigin}
          detail={detailState?.data?.id === openId ? detailState.data : null}
          readState={detailState}
          onRetry={() => {
            void resources.issue(openId).refresh();
            if (screen === "myWork") void resources.myWorkIssue(openId).refresh();
          }}
          readOnly={readOnly}
          onOpen={openIssue}
          card={screen === "myWork" ? personalState?.data?.issues.find((row) => row.id === openId) ?? null : null}
          home={decision && homeNeedIssueId(decision.need) === openId ? {
            open: decision,
            onDone: finishDecision,
            onAsk: (_need, lead) => decision.ask(lead),
          } : undefined}
          myWork={screen === "myWork" ? {
            owner: membershipState?.data?.owner ?? "",
            assignment: membershipState?.data?.id === openId ? membershipState.data.assignment : null,
            membership: !membershipState?.data || membershipState.data.id !== openId || membershipState.inFlight ||
              membershipState.status === "stale" || membershipState.status === "failed"
              ? "checking"
              : membershipState.data.assigned ? "assigned" : "unassigned",
            identityKey: personalIdentity,
            drafts: personalDrafts,
            draftKey: `${personalIdentity}:${openId}`,
            onWrite: (resp, verb) => {
              if (privateReadsAllowed() && privateSessionGeneration() === personalGeneration) applyWrite(resp, verb);
            },
            onError: (error, verb) => {
              if (privateReadsAllowed() && privateSessionGeneration() === personalGeneration) {
                writeError(error, verb);
                if (error instanceof ApiError && (error.status === 401 || error.status === 403)) void recheck();
              }
            },
            writeBlock: block,
            onDraftChange: () => {
              if (privateSessionGeneration() === personalGeneration) notifyPersonalDrafts((version) => version + 1);
            },
          } : undefined}
          href={(() => {
            const fromDetail = detailState?.data?.id === openId ? detailState.data.project : null;
            const fromCard = personalState?.data?.issues.find((row) => row.id === openId)?.project
              ?? issues.find((i) => i.id === openId)?.project ?? null;
            const key = fromDetail ?? fromCard ?? (project !== "all" ? project : null);
            return key ? issuePath(key, openId) : null;
          })()}
          onClose={closeWorkItem}
        />
      )}

      <Toast msg={toast} />
      <CanvasDrawer />
    </div>
    </HireBlock.Provider>
    </WriteGate.Provider>
  );
}

function WorkspaceAppEntry({
  route,
  search,
  installations,
  viewer,
  listReader,
  recheckFinished,
  onRetryWorkspace,
  onInstallation,
  onBack,
}: {
  route: Extract<Route, { screen: "workspaceApp" | "workspaceAppKey" }>;
  /** The current query — the `install` pin is read from it. */
  search: string;
  installations: InstallationsState;
  viewer: Viewer;
  /** CAD-1537: this viewer may resolve an installed app (the operator, a
   *  signed-in `--read-only` board viewer, or a member via app-home). */
  listReader: boolean;
  recheckFinished: boolean;
  onRetryWorkspace: () => void;
  onInstallation: (installation: ActiveInstallation | null) => void;
  onBack: () => void;
}) {
  const { t } = useLocale();
  if (viewer.operator === null) {
    return (
      <main className="px-4 lg:px-8 pt-10 pb-9">
        {wakingBanner(viewer) ? (
          <Notice state="access-unavailable">
            <span role="status">{t("Workspace waking or unreachable.")} {recheckFinished ? t("Retry when ready.") : t("Checking again for up to 60 seconds…")}</span>
            {recheckFinished && <button type="button" className="lnk ml-2" onClick={onRetryWorkspace}>{t("Retry workspace")}</button>}
          </Notice>
        ) : <Loading>{t("Checking access…")}</Loading>}
      </main>
    );
  }
  if (!listReader) {
    return (
      <main className="px-4 lg:px-8 pt-10 pb-9">
        <h1 className="text-section font-semibold text-ink-100">{t(route.screen === "workspaceApp" ? "App link unavailable" : "App unavailable")}</h1>
        <p className="text-body text-ink-400 mt-2">{t("Sign in as the operator to resolve an installed app.")}</p>
        <Link href="/apps" className="lnk">{t("All apps")}</Link>
      </main>
    );
  }
  if (installations.error !== null) {
    return (
      <main className="px-4 lg:px-8 pt-10 pb-9">
        <h1 className="text-section font-semibold text-ink-100">{t("Couldn’t load installed apps")}</h1>
        <p className="text-body text-ink-400 mt-2" role="alert">
          {t(appLoadErrorCopy(installations.error, "The installed-app list is unavailable."))}
        </p>
        <button type="button" className="lnk mt-3" onClick={installations.retry}>{t("Retry")}</button>
      </main>
    );
  }
  if (installations.list === null) {
    return (
      <main className="px-4 lg:px-8 pt-10 pb-9">
        <p className="text-body text-ink-400" role="status">{t("Loading installed apps…")}</p>
      </main>
    );
  }

  const legacyId = route.screen === "workspaceApp" ? route.installId : null;
  const canonicalPath = legacyId === null ? null : installationAppHref(installations.list, legacyId);
  if (legacyId !== null && canonicalPath === null) {
    const known = installations.list.some((installation) => installation.install_id === legacyId);
    return (
      <main className="px-4 lg:px-8 pt-10 pb-9">
        <h1 className="text-section font-semibold text-ink-100">{t("App link retired")}</h1>
        <p className="text-body text-ink-400 mt-2">
          {known
            ? t("This installation’s old link cannot name it anymore — open it from your apps.")
            : t("This link does not name an installed app anymore — it may have been removed.")}
        </p>
        <Link href="/apps" className="lnk">{t("All apps")}</Link>
      </main>
    );
  }
  const matches = installations.list.filter(
    (installation) => installation.removed == null && (route.screen === "workspaceAppKey"
      ? installation.name === route.appKey
      : installation.install_id === route.installId),
  );
  const appKey = route.screen === "workspaceAppKey" ? route.appKey : matches[0].name;
  if (matches.length === 0) {
    return (
      <main className="px-4 lg:px-8 pt-10 pb-9">
        <h1 className="text-section font-semibold text-ink-100">{t("App not installed")}</h1>
        <p className="text-body text-ink-400 mt-2">{t("No active installation with app key “{appKey}” was found.").replace("{appKey}", () => appKey)}</p>
        <Link href="/apps" className="lnk">{t("All apps")}</Link>
      </main>
    );
  }
  if (matches.length > 1) {
    // CAD-1438: an ambiguous key cannot spell a canonical URL and the
    // retired ID route must not resolve it either — say so, with the
    // installations named for the operator, but no dead-end links.
    return (
      <main className="px-4 lg:px-8 pt-10 pb-9">
        <h1 className="text-section font-semibold text-ink-100">{t("App key is ambiguous")}</h1>
        <p className="text-body text-ink-400 mt-2">
          {t("More than one active installation uses app key “{appKey}”, so this route cannot pick one. Remove or rename the duplicates and the link works again.").replace("{appKey}", () => appKey)}
        </p>
        <ul className="mt-4 space-y-1 text-body text-ink-400">
          {matches.map((installation) => (
            <li key={installation.install_id}>
              {installation.title || installation.name} · {installation.project_link || "workspace"}
            </li>
          ))}
        </ul>
        <p className="mt-3"><Link href="/apps" className="lnk">{t("All apps")}</Link></p>
      </main>
    );
  }

  const installation = matches[0];
  // A pin only refuses a different installation; it never selects one or
  // grants access. Check it before either redirecting or mounting the app.
  const pinnedInstall = new URLSearchParams(search).get("install");
  if (pinnedInstall !== null && pinnedInstall !== installation.install_id) {
    return (
      <main className="px-4 lg:px-8 pt-10 pb-9">
        <h1 className="text-section font-semibold text-ink-100">{t("App link retired")}</h1>
        <p className="text-body text-ink-400 mt-2">
          {t("This link was made for a different installation of “{appKey}” — its context cannot be carried to the current one.").replace("{appKey}", () => appKey)}
        </p>
        <Link href="/apps" className="lnk">{t("All apps")}</Link>
      </main>
    );
  }
  if (legacyId !== null && canonicalPath !== null) {
    // Keep the original query bytes; add only the missing immutable pin.
    const pinnedSearch = pinnedInstall === null
      ? `${search}${search ? "&" : "?"}install=${encodeURIComponent(legacyId)}`
      : search;
    return <LegacyAppRedirect href={canonicalPath + pinnedSearch + location.hash} />;
  }
  return (
    <AppShell
      key={installation.install_id}
      installId={installation.install_id}
      viewer={viewer}
      onInstallation={onInstallation}
    >
      {(onMediaAttach, contextId, screen) => <WorkspaceApp installId={installation.install_id} appKey={appKey} viewer={viewer} onBack={onBack} contextId={contextId} onMediaAttach={onMediaAttach} onScreenDrawer={screen.onScreenDrawer} presentation={screen.presentation} onScreenClosed={screen.onScreenClosed} drawerTarget={screen.drawerTarget} />}
    </AppShell>
  );
}

/** Compatibility changes the URL only; all entry guards live above it. */
function LegacyAppRedirect({ href }: { href: string }) {
  const { t } = useLocale();
  useEffect(() => {
    navigate(href, { replace: true });
  }, [href]);
  return (
    <main className="px-4 lg:px-8 pt-10 pb-9">
      <p className="text-body text-ink-400" role="status">{t("Opening the app…")}</p>
    </main>
  );
}
