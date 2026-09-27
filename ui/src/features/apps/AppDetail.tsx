import { useEffect, useRef, useState } from "react";
import { api, type ApiError } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { resources } from "../../lib/resources";
import { useMaybeResource, useQuery, useResource } from "../../lib/useResource";
import { useHref } from "../../lib/useLocation";
import type {
  AppDetail as AppDetailRow,
  AppPendingSend,
  AppRun,
  AppRunOutput,
  AppWorkflow,
} from "../../lib/types";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import SectionTabs from "../../ui/SectionTabs";
import Md from "../../ui/Md";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconClose } from "../../ui/icons";
import { homeNeeds, type HomeNeed } from "../home/needs";
import RunForm from "../projects/RunForm";
import ApproveApp from "./ApproveApp";
import {
  appApprovalChip,
  appFieldLabel,
  appHref,
  appNeeds,
  appPurpose,
  appWorkflowRow,
  agentStateWord,
  approvalPending,
  connectionRows,
  distinctNote,
  distinctProblem,
  doctorFindings,
  filterCounts,
  isReadyToRun,
  notReadyText,
  outputsOf,
  primaryAction,
  publishTarget,
  runFilter,
  runStages,
  runStatus,
  runTitle,
  sourceLabel,
  startLabel,
  slugProblem,
  stepRows,
  teamCandidates,
  teamFromLastRun,
  teamInputs,
  unboundSlots,
  usedSlots,
  type RunFilter,
} from "./apps";
import type { Viewer } from "../projects/work";
import "./apps.css";

type Tab = "posts" | "settings" | "how";
type AppDetailProps = {
  project: string;
  name: string;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onHome: () => void;
};

/**
 * `/apps/<project>/<name>` (CAD-563 r2) — one installed app, built
 * around what a person does with it: what needs them (the strip), a new
 * run (the drawer), the runs in flight and what has been published
 * (Posts), the team and where it publishes (Settings), and how it works
 * (How it works, with the engine's details folded away). `?new=<wf>`
 * opens the New-run drawer from a workflow link. Section links keep the
 * selected view in the URL for refresh and browser navigation.
 */
export default function AppDetail(props: AppDetailProps) {
  // Drafts, pending UI and the run drawer belong to one installation.
  return <AppPage key={`${props.project}/${props.name}`} {...props} />;
}

function AppPage({
  project,
  name,
  viewer,
  onOpenIssue,
  onHome,
}: AppDetailProps) {
  const key = `${project}/${name}`;
  const state = useQuery(resources.app(key));
  const app = state.data;
  const [running, setRunning] = useState<string | null>(null);
  // The runs and the Needs-you rail's rows: the Posts tab and the strip
  // read both. The overview is asked for by App.tsx on this screen.
  const runsState = useQuery(resources.appRuns(key));
  const runs = runsState.data ?? [];
  const overview = useResource(resources.overview);
  const needs = homeNeeds(overview.data?.needs_me);
  // The ledger is the operator's: fetch it only when the board proves
  // the operator, so a viewer without the read never sees a 403.
  const outputsRes = viewer.operator ? resources.appOutputs(key) : null;
  const outputs = useMaybeResource(outputsRes);
  useEffect(() => {
    if (outputsRes) void outputsRes.revalidate();
  }, [outputsRes]);
  const items = outputs?.data?.items ?? [];
  const pending = outputs?.data?.pending ?? [];
  // `?new=<app>/<wf>` asks for that workflow's drawer once the app has
  // loaded; consumed once, so closing it does not reopen on a refetch.
  const href = useHref();
  const params = new URLSearchParams(href.split("?")[1] ?? "");
  const selectedTab = params.get("tab");
  const tab: Tab = selectedTab === "settings" || selectedTab === "how" ? selectedTab : "posts";
  const appPath = appHref(project, name);
  const settingsHref = `${appPath}?tab=settings`;
  const want = params.get("new");
  const consumed = useRef<string | null>(null);
  useEffect(() => {
    if (want && app && consumed.current !== want) {
      consumed.current = want;
      setRunning(want);
    }
  }, [want, app]);

  const action = app ? primaryAction(app) : null;
  const approval = app ? appApprovalChip(app) : null;
  // Installation approval lives in Settings; this strip is for run decisions.
  const needsRows = app ? appNeeds(app, runs, needs, pending).filter(n => n.kind !== "approve") : [];
  const ready = app ? isReadyToRun(app) : false;
  const notReady = app ? notReadyText(app) : null;
  return (
    <>
      <main className="apps-workspace px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={`app ${key}`}>
        <Link href="/apps" className="lnk text-label">
          ← All apps
        </Link>
        <div className="flex flex-wrap items-start gap-3 mt-2 mb-3">
          <div className="min-w-0 flex-1">
            <h1 className="text-section font-semibold text-ink-100">
              {app?.title?.trim() || name}
            </h1>
            <p className="text-micro text-ink-500 mt-0.5">Project: {project}</p>
            {app && <p className="text-body text-ink-400 mt-1 break-words">{appPurpose(app)}</p>}
            <div className="flex flex-wrap items-center gap-2 mt-1.5">
              {approval && app?.approval !== "approved" && (
                <span className={`chip ${approval.cls}`}>{approval.text}</span>
              )}
              <StaleChip state={state} />
            </div>
          </div>
          {action && (
            <Button
              variant="primary"
              onClick={() => ready && setRunning(action.wf.name)}
              disabled={!ready}
              className="shrink-0"
            >
              {action.label}
            </Button>
          )}
        </div>
        {app && !ready && (
          <section
            className="app-setup card px-4 py-3 min-w-0 mb-3"
            aria-label="setup required"
          >
            <div className="min-w-0">
              <h2 className="text-label font-medium text-ink-100">Setup required</h2>
              <p className="text-label text-ink-400 mt-0.5 break-words">{notReady}</p>
            </div>
            {tab !== "settings" && <Button href={settingsHref}>Open settings</Button>}
          </section>
        )}
        <ResourceGate
          state={state}
          loading={`loading ${key}…`}
          failed={`could not load ${key}`}
          onRetry={() => void resources.app(key).invalidate()}
        />

        {app && needsRows.length > 0 && (
          <section
            className="card px-4 py-3 min-w-0 mb-3 border-warn/40"
            aria-label="needs you"
          >
            <div className="slabel mb-1.5">needs you</div>
            <ul className="space-y-1.5">
              {needsRows.map((n, i) => (
                <li key={`${n.kind}-${i}`} className="flex flex-wrap items-center gap-2 min-w-0">
                  <span className="text-label text-ink-200 min-w-0 break-words flex-1">{n.text}</span>
                  <Link href="/" className="lnk text-label shrink-0" title="open Needs you">
                    {n.kind === "release" ? "Release →" : "Answer →"}
                  </Link>
                </li>
              ))}
            </ul>
          </section>
        )}

        {app && (
          <>
            <div className="-mx-4 lg:-mx-8 mb-4">
              <SectionTabs label="App" tabs={[
                {label:"Posts",href:appPath,on:tab === "posts"},
                {label:"Settings",href:settingsHref,on:tab === "settings"},
                {label:"How it works",href:`${appPath}?tab=how`,on:tab === "how"},
              ]} />
            </div>
            {tab === "posts" && (
              <PostsTab
                runs={runs}
                runsState={runsState}
                needs={needs}
                items={items}
                pending={pending}
                ready={ready}
                viewer={viewer}
                onOpenIssue={onOpenIssue}
                onRetry={() => void resources.appRuns(key).invalidate()}
              />
            )}
            {tab === "settings" && (
              <SettingsTab
                app={app}
                runs={runs}
                viewer={viewer}
                onApproveRetry={() => void resources.app(key).invalidate()}
              />
            )}
            {tab === "how" && <HowTab app={app} runs={runs} />}
          </>
        )}
      </main>
      {running && app && (
        <RunDrawer
          project={project}
          app={app}
          wf={running}
          runs={runs}
          viewer={viewer}
          onClose={() => setRunning(null)}
          onOpenIssue={(id) => {
            setRunning(null);
            onOpenIssue(id);
          }}
          onHome={onHome}
        />
      )}
    </>
  );
}

/** Posts — one card per run, with its stage row, filters and outputs. */
function PostsTab({
  runs,
  runsState,
  needs,
  items,
  pending,
  ready,
  viewer,
  onOpenIssue,
  onRetry,
}: {
  runs: AppRun[];
  runsState: ReturnType<typeof useQuery<AppRun[]>>;
  needs: HomeNeed[];
  items: AppRunOutput[];
  pending: AppPendingSend[];
  ready: boolean;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
}) {
  const [filter, setFilter] = useState<RunFilter>("in_progress");
  const counts = filterCounts(runs, needs, items, pending);
  const shown = runs.filter((run) => {
    const mine = outputsOf(run, items, pending);
    return runFilter(run, needs, mine.items.length > 0) === filter;
  });
  return (
    <section className="min-w-0 space-y-2.5" aria-label="posts">
      {runs.length > 0 && <div className="flex flex-wrap items-center gap-1.5">
        {(
          [
            ["in_progress", "In progress"],
            ["needs_you", "Needs you"],
            ["published", "Published"],
          ] as [RunFilter, string][]
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            aria-pressed={filter === id}
            onClick={() => setFilter(id)}
            className={`chip ${
              filter === id ? "bg-accent/20 text-accent" : "bg-ink-800 text-ink-400 hover:text-ink-200"
            }`}
          >
            {label}
            {counts[id] > 0 && <span className="num ml-1 opacity-70">{counts[id]}</span>}
          </button>
        ))}
        <span className="text-micro text-ink-500 ml-auto">
          {[
            counts.in_progress > 0 ? `${counts.in_progress} in progress` : null,
            counts.needs_you > 0 ? `${counts.needs_you} need${counts.needs_you === 1 ? "s" : ""} you` : null,
            counts.published > 0 ? `${counts.published} published` : null,
          ]
            .filter(Boolean)
            .join(" · ") || "Nothing yet"}
        </span>
      </div>}
      <ResourceGate
        state={runsState}
        loading="loading posts…"
        failed="could not load posts"
        onRetry={onRetry}
      />
      {runsState.data && runs.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          <h2 className="text-cardtitle font-medium text-ink-100">No posts yet</h2>
          <p className="mt-1">{!ready
            ? "Complete setup in Settings before starting a post."
            : viewer.readOnly && viewer.operator
              ? "New posts cannot be started on this read-only board."
            : !viewer.operator
              ? "Sign in as the operator to start a post."
              : "Start a post when you’re ready. You’ll review the plan before any work runs."}</p>
        </div>
      )}
      {runsState.data && runs.length > 0 && shown.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          {filter === "needs_you" ? "No posts need your attention." : filter === "published" ? "No published posts yet." : "No posts in progress."}
        </div>
      )}
      <ul className="space-y-2.5">
        {shown.map((run) => (
          <RunCard
            key={run.epic}
            run={run}
            needs={needs}
            mine={outputsOf(run, items, pending)}
            onOpenIssue={onOpenIssue}
          />
        ))}
      </ul>
    </section>
  );
}

const STAGE_CLS: Record<string, string> = {
  done: "bg-ok/15 text-ok",
  current: "bg-accent/20 text-accent font-semibold",
  waiting: "bg-ink-800 text-ink-500",
};

function RunCard({
  run,
  needs,
  mine,
  onOpenIssue,
}: {
  run: AppRun;
  needs: HomeNeed[];
  mine: { items: AppRunOutput[]; pending: AppPendingSend[] };
  onOpenIssue: (id: string) => void;
}) {
  const status = runStatus(run, needs, mine);
  const stages = runStages(run);
  return (
    <li className="card px-3.5 py-3 min-w-0" data-run={run.epic}>
      <div className="flex flex-wrap items-center gap-2 min-w-0">
        <button
          type="button"
          onClick={() => onOpenIssue(run.epic)}
          className="text-cardtitle font-medium text-ink-100 min-w-0 break-words flex-1 text-left hover:text-accent"
          title={`open ${run.epic}`}
        >
          {runTitle(run.title)}
        </button>
        <span className="flex flex-wrap items-center gap-1.5 shrink-0">
          <span className={`chip ${status.cls}`} data-state={status.text}>
            {status.text}
          </span>
          {status.action && (
            <Link href={status.action.href} className="lnk text-label">
              {status.action.label} →
            </Link>
          )}
        </span>
      </div>
      <div className="flex flex-wrap items-center gap-1 mt-2 min-w-0">
        {stages.map((s, i) => (
          <span key={`${s.label}-${i}`} className="flex items-center gap-1 min-w-0">
            {i > 0 && (
              <span className="text-ink-600" aria-hidden>
                →
              </span>
            )}
            <span className={`chip ${STAGE_CLS[s.tone]}`} data-stage={s.tone}>
              {s.tone === "done" ? "✓ " : ""}
              {s.label}
            </span>
          </span>
        ))}
      </div>
      {run.plan.proposed_at && (
        <div className="text-micro text-ink-500 mt-1.5 num">{fmtTime(run.plan.proposed_at)}</div>
      )}
    </li>
  );
}

/** Settings — where it publishes, the team, and the app's version. */
function SettingsTab({
  app,
  runs,
  viewer,
  onApproveRetry,
}: {
  app: AppDetailRow;
  runs: AppRun[];
  viewer: Viewer;
  onApproveRetry: () => void;
}) {
  const action = primaryAction(app);
  const wf = action?.wf ?? (app.workflows ?? [])[0];
  const slots = usedSlots(app);
  return (
    <div className="app-settings min-w-0">
      {wf && <TeamEditor app={app} wf={wf} runs={runs} viewer={viewer} />}
      <div className="space-y-3 min-w-0">
      {slots.length > 0 && (
        <section className="card px-4 py-3.5 min-w-0" aria-label="publishing">
          <h2 className="text-cardtitle font-medium text-ink-100 mb-2">Publishing</h2>
          <ul className="space-y-1">
            {slots.map((slot) => (
              <li key={slot} className="text-label text-ink-200">
                Publishes to: <span className="text-ink-100">{publishTarget(app, slot)}</span>
              </li>
            ))}
          </ul>
        </section>
      )}
      <section className="card px-4 py-3.5 min-w-0" aria-label="app">
        <h2 className="text-cardtitle font-medium text-ink-100 mb-2">App version</h2>
        <div className="flex flex-wrap items-center gap-2">
          {app.version && <span className="chip bg-ink-800 text-ink-300">v{app.version}</span>}
          {approvalPending(app) && <ApproveApp row={app} viewer={viewer} />}
        </div>
        {approvalPending(app) && (
          <p className="text-micro text-ink-500 mt-2 break-words">
            {app.approval === "changed" ? "The app changed since its last approval. Review this version before approving it again." : "Review this version before approving it for use."}
          </p>
        )}
        {app.approval === "approved" && !app.error && (
          <p className="text-micro text-ink-500 mt-2 break-words">
            This version is approved.
          </p>
        )}
        {app.error && <p className="text-label text-fail mt-2 break-words" role="alert">{app.error}</p>}
        <Button
          variant="ghost"
          onClick={onApproveRetry}
          className="mt-2"
        >
          Check for changes
        </Button>
      </section>
      </div>
    </div>
  );
}

/**
 * Settings → Team (CAD-577): one picker per role, listing the registered
 * agents with their state, saved as the app's default team (an
 * operator-only write, stored with the install record, not in the
 * digest). "Add worker" joins a new Devin worker for the role — the
 * daemon's `app_add_worker` — with a unique prefixed alias, under the
 * operator (a group root).
 */
function TeamEditor({
  app,
  wf,
  runs,
  viewer,
}: {
  app: AppDetailRow;
  wf: AppWorkflow;
  runs: AppRun[];
  viewer: Viewer;
}) {
  const roles = teamInputs(wf);
  const inputs = wf.inputs ?? [];
  const agentsState = useResource(resources.agents);
  const candidates = teamCandidates(agentsState.data?.agents);
  const saved = app.team ?? {};
  const lastRun = teamFromLastRun(wf, runs);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [adding, setAdding] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const canEdit = viewer.operator && !viewer.readOnly;
  const valueOf = (role: string) => draft[role] ?? saved[role] ?? lastRun[role] ?? "";
  const dirty = roles.some((r) => valueOf(r) !== (saved[r] ?? ""));

  const save = () => {
    if (busy || !canEdit) return;
    setBusy(true);
    setNote(null);
    setFailed(false);
    const assignments = Object.fromEntries(roles.map((r) => [r, valueOf(r)]));
    const team = roles.map((r) => `${r}=${assignments[r]}`);
    api
      .appSetTeam(app.project, app.name, team)
      .then(() => {
        const resource = resources.app(`${app.project}/${app.name}`);
        // The POST confirmed these assignments. Keep them visible even
        // if the refresh fails, and invalidate any older in-flight read.
        resource.mutate(current => ({ ...current, team: assignments }));
        setDraft({});
        setNote("Team saved.");
        void resource.invalidate();
      })
      .catch((e: ApiError) => { setFailed(true); setNote(e.message ?? String(e)); })
      .finally(() => setBusy(false));
  };

  const addWorker = (role: string) => {
    if (adding || !canEdit) return;
    setAdding(role);
    setNote(null);
    setFailed(false);
    api
      .appAddWorker(app.project, app.name, role)
      .then((out) => {
        setNote(out.alias ? `Joined ${out.alias}.` : "Worker joined.");
        void resources.agents.invalidate();
        void resources.app(`${app.project}/${app.name}`).invalidate();
      })
      .catch((e: ApiError) => { setFailed(true); setNote(e.message ?? String(e)); })
      .finally(() => setAdding(null));
  };

  return (
    <section className="card px-4 py-3.5 min-w-0" aria-label="team">
      <h2 className="text-cardtitle font-medium text-ink-100 mb-2">Team</h2>
      <p className="text-label text-ink-400 mb-2">
        The default agents for new runs. Changing the team does not require app approval.
      </p>
      {!canEdit && <p className="text-label text-ink-500 mb-3">{viewer.readOnly ? "The team is read-only on this board." : "Sign in as the operator to change the team."}</p>}
      <ul className="space-y-3">
        {roles.map((role) => {
          const spec = inputs.find((i) => i.name === role);
          const value = valueOf(role);
          return (
            <li key={role} className="app-team-row">
              <div className="min-w-0">
                <label htmlFor={`team-${role}`} className="text-label font-medium text-ink-200">{appFieldLabel(role)}</label>
                {spec?.ask && <p id={`team-${role}-hint`} className="text-micro text-ink-500 mt-0.5 break-words">{spec.ask}</p>}
              </div>
              <div className="app-team-controls">
              <select
                id={`team-${role}`}
                value={value}
                onChange={(e) => {setDraft((cur) => ({ ...cur, [role]: e.target.value }));setNote(null);}}
                className="field flex-1 min-w-0"
                data-role={role}
                aria-describedby={spec?.ask ? `team-${role}-hint` : undefined}
                disabled={busy || adding !== null || !canEdit}
              >
                <option value="">Choose an agent</option>
                {candidates.map((a) => (
                  <option key={a.alias} value={a.alias}>
                    {a.alias} — {agentStateWord(a.state)}
                  </option>
                ))}
                {value && !candidates.some((a) => a.alias === value) && (
                  <option value={value}>{value} — saved</option>
                )}
              </select>
              <Button
                variant="ghost"
                onClick={() => addWorker(role)}
                disabled={busy || adding !== null || !canEdit}
                className="shrink-0"
                aria-label={`Add worker for ${appFieldLabel(role)}`}
                title={
                  !canEdit
                    ? "Joining a worker is the operator's."
                    : "join a new Devin worker for this role"
                }
              >
                {adding === role ? "Joining…" : "Add worker"}
              </Button>
              </div>
            </li>
          );
        })}
        {roles.length === 0 && (
          <li className="text-label text-ink-500">This workflow's steps name no team inputs.</li>
        )}
      </ul>
      {roles.length > 0 && (
        <div className="flex flex-wrap items-center gap-2 mt-3">
          <Button
            variant="primary"
            onClick={save}
            loading={busy}
            disabled={adding !== null || !dirty || !canEdit}
            title={!canEdit ? "Changing the team requires an editable operator session." : undefined}
          >
            {busy ? "Saving…" : "Save team"}
          </Button>
          {note && <span className={`text-label ${failed ? "text-fail" : "text-ink-400"} break-words`} role={failed ? "alert" : "status"}>{note}</span>}
        </div>
      )}
    </section>
  );
}

/** How it works — the steps in plain words, and the details folded away. */
function HowTab({ app, runs }: { app: AppDetailRow; runs: AppRun[] }) {
  const action = primaryAction(app);
  const wf = action?.wf ?? (app.workflows ?? [])[0];
  const team = wf ? teamFromLastRun(wf, runs) : {};
  const steps = wf ? stepRows(wf, team) : [];
  const slots = usedSlots(app);
  const unbound = new Set(unboundSlots(app));
  const findings = doctorFindings(app.doctor);
  const connections = connectionRows(app.doctor);
  const source = sourceLabel(app);
  const record = app.record as { source?: { path?: string } } | null | undefined;
  return (
    <div className="space-y-3 min-w-0">
      <section className="card px-4 py-3.5 min-w-0" aria-label="how it works">
        <p className="text-body text-ink-200 break-words">{appPurpose(app)}</p>
        {steps.length > 0 && (
          <ol className="mt-3 space-y-1.5">
            {steps.map((s, i) => (
              <li key={`${s.label}-${i}`} className="text-label min-w-0">
                <span className="num text-ink-500 mr-2">{i + 1}</span>
                <span className="text-ink-200">{s.label}</span>
                {s.who && <span className="text-ink-500"> — {s.who}</span>}
              </li>
            ))}
          </ol>
        )}
        <div className="mt-3 space-y-1">
          {slots.length > 0 && (
            <p className="text-label text-ink-400">
              Nothing is published without your approval.
            </p>
          )}
          {wf && distinctNote(wf) && (
            <p className="text-label text-ink-400">{distinctNote(wf)}</p>
          )}
        </div>
      </section>
      <details className="card px-4 py-3.5 min-w-0" aria-label="technical details">
        <summary className="slabel cursor-pointer select-none">technical details</summary>
        <div className="mt-3 space-y-3 min-w-0">
          <div className="num text-micro text-ink-500 space-y-0.5 break-all">
            {app.digest && <div>digest {app.digest}</div>}
            {source && <div>{source}</div>}
            {record?.source?.path && <div>{record.source.path}</div>}
            {app.installed_at && (
              <div>
                installed {app.installed_at}
                {app.installed_by ? ` by ${app.installed_by}` : ""}
              </div>
            )}
            <div>workflows: {(app.workflows ?? []).map((w) => w.name).join(", ")}</div>
          </div>
          {connections.length > 0 && (
            <div>
              <div className="slabel mb-1">connections</div>
              <ul className="space-y-0.5">
                {connections.map((c, i) => (
                  <li key={i} className={`num text-label ${c.cls} break-words`}>
                    {c.text}
                  </li>
                ))}
              </ul>
            </div>
          )}
          {(findings.length > 0 || unbound.size > 0) && (
            <div>
              <div className="slabel mb-1">doctor findings</div>
              <ul className="space-y-0.5">
                {findings.map((f, i) => (
                  <li key={i} className={`num text-label ${f.cls} break-words`}>
                    {f.text}
                  </li>
                ))}
                {findings.length === 0 && (
                  <li className="text-label text-ink-500">
                    {unbound.size > 0
                      ? `${unbound.size} unbound slot${unbound.size === 1 ? "" : "s"}`
                      : "No findings."}
                  </li>
                )}
              </ul>
            </div>
          )}
          {(app.rubrics ?? []).map((r) => (
            <div key={r.name}>
              <div className="slabel mb-1">rubric — {r.name}</div>
              <div className="issue-reader text-body text-ink-200">
                <Md text={r.body} />
              </div>
            </div>
          ))}
          {app.guide && (
            <div>
              <div className="slabel mb-1">agent guide — app.md</div>
              <div className="issue-reader text-body text-ink-200">
                <Md text={app.guide} />
              </div>
            </div>
          )}
        </div>
      </details>
    </div>
  );
}

/**
 * The New-run drawer (CAD-563): the same `RunForm` the Workflows screen
 * opens, in the app's variant — the topic first, the team from the last
 * run under "More options", the propose the board's OperatorOnly route.
 */
function RunDrawer({
  project,
  app,
  wf,
  runs,
  viewer,
  onClose,
  onOpenIssue,
  onHome,
}: {
  project: string;
  app: AppDetailRow;
  wf: string;
  runs: AppRun[];
  viewer: Viewer;
  onClose: () => void;
  onOpenIssue: (id: string) => void;
  onHome: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);
  const row = (app.workflows ?? []).find((w) => w.name === wf);
  if (!row) return null;
  const inputs = row.inputs ?? [];
  const primary = inputs[0]?.name ?? null;
  const slugInput = inputs.some((i) => i.name === "slug") ? "slug" : null;
  // The saved default team wins; the last run fills any role it left
  // unset (CAD-577).
  const team = { ...teamFromLastRun(row, runs), ...(app.team ?? {}) };
  // The run's plain name — the workflow's own `label:` ("New post"),
  // never the engine's "new run" (CAD-571).
  const title = row.label?.trim() || "New run";
  return (
    <>
      <div className="fixed inset-0 bg-scrim z-20" onClick={onClose} />
      <aside
        className="drawer fixed top-0 right-0 h-full w-full sm:w-[42rem] bg-ink-875 border-l border-ink-700 z-30 flex flex-col"
        aria-label={`new run of ${row.name}`}
      >
        <header className="px-5 pt-4 pb-4 border-b border-ink-700 flex items-start gap-3 shrink-0">
          <div className="min-w-0 flex-1">
            <div className="text-label text-ink-500">in {project} · {title}</div>
            <h2 className="text-drawer font-semibold text-ink-100 leading-tight mt-1">
              {title}
            </h2>
          </div>
          <button
            aria-label="Close"
            onClick={onClose}
            className="closebtn ml-auto shrink-0 w-8 h-8 grid place-items-center rounded border border-ink-600 text-ink-300 bg-ink-850"
          >
            <IconClose style={{ pointerEvents: "none" }} />
          </button>
        </header>
        <div className="overflow-y-auto min-w-0">
          <RunForm
            row={appWorkflowRow(project, app, row)}
            viewer={viewer}
            onOpenIssue={onOpenIssue}
            onHome={onHome}
            app={{
              primary,
              prefill: team,
              slugInput,
              title,
              start: startLabel(title),
              // The rules in plain words: the plan waits for the
              // operator, and nothing goes out without them.
              note: "It waits for your approval before anything runs. Nothing is published without your OK.",
              // Live checks: the folder name's shape, and a team that
              // breaks the workflow's kept-apart rule (shown only then).
              validate: (values) =>
                (slugInput ? slugProblem(values[slugInput] ?? "") : null) ??
                distinctProblem(row, values),
            }}
          />
        </div>
      </aside>
    </>
  );
}
