import { useEffect, useRef, useState } from "react";
import { fmtTime } from "../../lib/fmt";
import { resources } from "../../lib/resources";
import { useMaybeResource, useQuery, useResource } from "../../lib/useResource";
import { useHref } from "../../lib/useLocation";
import type {
  AppDetail as AppDetailRow,
  AppPendingSend,
  AppRun,
  AppRunOutput,
} from "../../lib/types";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { homeNeeds, type HomeNeed } from "../home/needs";
import RunForm from "../projects/RunForm";
import ApproveApp from "./ApproveApp";
import {
  appApprovalChip,
  appNeeds,
  appPurpose,
  appWorkflowRow,
  approvalPending,
  connectionRows,
  distinctNote,
  doctorFindings,
  outboxHref,
  outputsOf,
  primaryAction,
  publishTarget,
  runFilter,
  runStages,
  runState,
  runsSummary,
  sourceLabel,
  stepRows,
  teamFromLastRun,
  teamInputs,
  unboundSlots,
  usedSlots,
  type RunFilter,
} from "./apps";
import type { Viewer } from "../projects/work";

type Tab = "posts" | "settings" | "how";

/**
 * `/apps/<project>/<name>` (CAD-563 r2) — one installed app, built
 * around what a person does with it: what needs them (the strip), a new
 * run (the drawer), the runs in flight and what has been published
 * (Posts), the team and where it publishes (Settings), and how it works
 * (How it works, with the engine's details folded away). `?new=<wf>`
 * opens the New-run drawer — what an Apps card's primary action links
 * to.
 */
export default function AppDetail({
  project,
  name,
  viewer,
  onOpenIssue,
  onHome,
}: {
  project: string;
  name: string;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onHome: () => void;
}) {
  const key = `${project}/${name}`;
  const state = useQuery(resources.app(key));
  const app = state.data;
  const [tab, setTab] = useState<Tab>("posts");
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
  const want = new URLSearchParams(href.split("?")[1] ?? "").get("new");
  const consumed = useRef<string | null>(null);
  useEffect(() => {
    if (want && app && consumed.current !== want) {
      consumed.current = want;
      setRunning(want);
    }
  }, [want, app]);

  const action = app ? primaryAction(app) : null;
  const approval = app ? appApprovalChip(app) : null;
  const needsRows = app ? appNeeds(app, runs, needs, pending) : [];
  return (
    <>
      <main className="px-4 lg:px-8 pt-4 pb-9 min-w-0 max-w-3xl" aria-label={`app ${key}`}>
        <Link href="/apps" className="lnk text-label">
          ← all apps
        </Link>
        <div className="flex flex-wrap items-start gap-3 mt-2 mb-3">
          <div className="min-w-0 flex-1">
            <h1 className="text-section font-semibold text-ink-100">
              {app?.title?.trim() || name}
            </h1>
            {app && <p className="text-body text-ink-400 mt-0.5 break-words">{appPurpose(app)}</p>}
            <div className="flex flex-wrap items-center gap-2 mt-1.5">
              <span className="num text-label text-ink-500">{key}</span>
              {approval && <span className={`chip ${approval.cls}`}>{approval.text}</span>}
              <StaleChip state={state} />
            </div>
          </div>
          {action && (
            <button
              type="button"
              onClick={() => setRunning(action.wf.name)}
              className="shrink-0 h-8 px-3 rounded bg-accent text-on-accent text-label font-medium"
            >
              {action.label}
            </button>
          )}
        </div>
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
                  {n.kind === "approve" ? (
                    <ApproveApp row={app} viewer={viewer} />
                  ) : (
                    <Link href="/" className="lnk text-label shrink-0" title="open Needs you">
                      {n.kind === "release" ? "release →" : "answer →"}
                    </Link>
                  )}
                </li>
              ))}
            </ul>
          </section>
        )}

        {app && (
          <>
            <nav className="flex items-center gap-1 border-b border-ink-700 mb-3" role="tablist">
              {(
                [
                  ["posts", "Posts"],
                  ["settings", "Settings"],
                  ["how", "How it works"],
                ] as [Tab, string][]
              ).map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  role="tab"
                  aria-selected={tab === id}
                  onClick={() => setTab(id)}
                  className={`h-8 px-3 -mb-px border-b-2 text-label ${
                    tab === id
                      ? "border-accent text-ink-100 font-medium"
                      : "border-transparent text-ink-400 hover:text-ink-200"
                  }`}
                >
                  {label}
                </button>
              ))}
            </nav>
            {tab === "posts" && (
              <PostsTab
                runs={runs}
                runsState={runsState}
                needs={needs}
                items={items}
                pending={pending}
                operator={viewer.operator}
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
  operator,
  onOpenIssue,
  onRetry,
}: {
  runs: AppRun[];
  runsState: ReturnType<typeof useQuery<AppRun[]>>;
  needs: HomeNeed[];
  items: AppRunOutput[];
  pending: AppPendingSend[];
  operator: boolean;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
}) {
  const [filter, setFilter] = useState<RunFilter>("in_progress");
  const shown = runs.filter((run) => {
    const mine = outputsOf(run, items, pending);
    return runFilter(run, needs, mine.items.length > 0) === filter;
  });
  const counts: Record<RunFilter, number> = { in_progress: 0, needs_you: 0, published: 0 };
  for (const run of runs) {
    counts[runFilter(run, needs, outputsOf(run, items, pending).items.length > 0)] += 1;
  }
  return (
    <section className="min-w-0 space-y-2.5" aria-label="posts">
      <div className="flex flex-wrap items-center gap-1.5">
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
        <span className="text-micro text-ink-500 ml-auto">{runsSummary(runs, needs) ?? "Nothing yet"}</span>
      </div>
      <ResourceGate
        state={runsState}
        loading="loading posts…"
        failed="could not load posts"
        onRetry={onRetry}
      />
      {runsState.data && runs.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          No posts yet — start one above. Nothing is published without your approval.
        </div>
      )}
      {runsState.data && runs.length > 0 && shown.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          Nothing under this filter.
        </div>
      )}
      <ul className="space-y-2.5">
        {shown.map((run) => (
          <RunCard
            key={run.epic}
            run={run}
            needs={needs}
            mine={outputsOf(run, items, pending)}
            operator={operator}
            onOpenIssue={onOpenIssue}
          />
        ))}
      </ul>
    </section>
  );
}

const STAGE_CLS: Record<string, string> = {
  ok: "bg-ok/15 text-ok",
  run: "bg-accent/15 text-accent",
  wait: "bg-warn/10 text-warn",
  off: "bg-ink-800 text-ink-500",
};

function RunCard({
  run,
  needs,
  mine,
  operator,
  onOpenIssue,
}: {
  run: AppRun;
  needs: HomeNeed[];
  mine: { items: AppRunOutput[]; pending: AppPendingSend[] };
  operator: boolean;
  onOpenIssue: (id: string) => void;
}) {
  const state = runState(run, needs);
  const stages = runStages(run, mine.items, mine.pending);
  return (
    <li className="card px-3.5 py-3 min-w-0" data-run={run.epic}>
      <div className="flex flex-wrap items-center gap-2 min-w-0">
        <span className="text-cardtitle font-medium text-ink-100 min-w-0 break-words flex-1">
          {run.title}
        </span>
        <span className={`chip ${state.cls}`} data-state={state.text}>
          {state.text}
        </span>
      </div>
      <div className="flex flex-wrap items-center gap-1 mt-2 min-w-0">
        {stages.map((s, i) => (
          <span key={`${s.label}-${i}`} className="flex items-center gap-1 min-w-0">
            {i > 0 && <span className="text-ink-600" aria-hidden>→</span>}
            <span className={`chip ${STAGE_CLS[s.tone]}`}>{s.label}</span>
          </span>
        ))}
      </div>
      <div className="flex flex-wrap items-center gap-2 mt-1.5 text-micro text-ink-500 min-w-0">
        <button
          type="button"
          onClick={() => onOpenIssue(run.epic)}
          className="lnk num shrink-0"
          title={`open ${run.epic}`}
        >
          {run.epic}
        </button>
        {run.plan.proposed_at && <span className="num">{fmtTime(run.plan.proposed_at)}</span>}
        {state.needsYou && (
          <Link href="/" className="lnk text-warn shrink-0">
            needs you →
          </Link>
        )}
      </div>
      {mine.items.length > 0 && (
        <ul className="mt-2 space-y-1">
          {mine.items.map((it) => (
            <li key={it.effect_id} className="text-label min-w-0">
              <Link href={outboxHref(it.effect_id)} className="lnk break-words">
                published: {it.title}
              </Link>
              <span className="text-ink-500"> · {fmtTime(it.published_at)}</span>
            </li>
          ))}
        </ul>
      )}
      {operator && mine.pending.length > 0 && (
        <p className="text-label mt-2 min-w-0">
          {mine.pending.map((p) => (
            <Link key={p.effect_id} href="/" className="lnk text-warn break-words">
              {p.title?.trim() || "a post"} — ready to publish →
            </Link>
          ))}
        </p>
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
  const team = wf ? teamFromLastRun(wf, runs) : {};
  const inputs = wf ? (wf.inputs ?? []) : [];
  const slots = usedSlots(app);
  return (
    <div className="space-y-3 min-w-0">
      {slots.length > 0 && (
        <section className="card px-4 py-3.5 min-w-0" aria-label="publishing">
          <div className="slabel mb-2">publishing</div>
          <ul className="space-y-1">
            {slots.map((slot) => (
              <li key={slot} className="text-label text-ink-200">
                Publishes to: <span className="text-ink-100">{publishTarget(app, slot)}</span>
              </li>
            ))}
          </ul>
        </section>
      )}
      {wf && (
        <section className="card px-4 py-3.5 min-w-0" aria-label="team">
          <div className="slabel mb-2">team</div>
          <p className="text-label text-ink-400 mb-2">
            A new run starts with the team the last one used.
          </p>
          <ul className="space-y-1.5">
            {teamInputs(wf).map((input) => {
              const spec = inputs.find((i) => i.name === input);
              const value = team[input] ?? null;
              return (
                <li key={input} className="flex flex-wrap items-baseline gap-2 min-w-0 text-label">
                  <span className="text-ink-300 min-w-0 break-words">{spec?.ask ?? input}</span>
                  <span className={`num ${value ? "text-ink-100" : "text-ink-500"}`}>
                    {value ?? "not used yet"}
                  </span>
                  {value && <span className="chip bg-ink-800 text-ink-500">last used</span>}
                </li>
              );
            })}
            {teamInputs(wf).length === 0 && (
              <li className="text-label text-ink-500">This workflow's steps name no team inputs.</li>
            )}
          </ul>
        </section>
      )}
      <section className="card px-4 py-3.5 min-w-0" aria-label="app">
        <div className="slabel mb-2">app</div>
        <div className="flex flex-wrap items-center gap-2">
          {app.version && <span className="chip bg-ink-800 text-ink-300">v{app.version}</span>}
          {approvalPending(app) && <ApproveApp row={app} viewer={viewer} />}
        </div>
        {approvalPending(app) && (
          <p className="text-micro text-ink-500 mt-2 break-words">
            Approving records this exact bundle; `cadence app update` installs a newer one.
          </p>
        )}
        {!approvalPending(app) && (
          <p className="text-micro text-ink-500 mt-2 break-words">
            This bundle is approved as installed.
          </p>
        )}
        <button
          type="button"
          onClick={onApproveRetry}
          className="lnk text-micro mt-2"
        >
          refresh
        </button>
      </section>
    </div>
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
  const team = teamFromLastRun(row, runs);
  const action = primaryAction(app);
  return (
    <>
      <div className="fixed inset-0 bg-scrim z-20" onClick={onClose} />
      <aside
        className="drawer fixed top-0 right-0 h-full w-full sm:w-[42rem] bg-ink-875 border-l border-ink-700 z-30 flex flex-col"
        aria-label={`new run of ${row.name}`}
      >
        <header className="px-5 pt-4 pb-4 border-b border-ink-700 flex items-start gap-3 shrink-0">
          <div className="min-w-0 flex-1">
            <div className="num text-label text-ink-500">
              {project}/{app.name} · new run
            </div>
            <h2 className="text-drawer font-semibold text-ink-100 leading-tight mt-1">
              {action?.label ?? row.label ?? "New run"}
            </h2>
          </div>
          <button
            aria-label="Close"
            onClick={onClose}
            className="closebtn ml-auto shrink-0 w-8 h-8 grid place-items-center rounded border border-ink-600 text-ink-300 bg-ink-850"
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 12 12"
              stroke="currentColor"
              strokeWidth="1.5"
              fill="none"
              style={{ pointerEvents: "none" }}
            >
              <path d="M2 2l8 8M10 2l-8 8" />
            </svg>
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
              // The rules in plain words: who must be different people,
              // and that nothing goes out without the operator.
              note: [distinctNote(row), "Nothing is published without your approval."]
                .filter(Boolean)
                .join(" "),
            }}
          />
        </div>
      </aside>
    </>
  );
}
