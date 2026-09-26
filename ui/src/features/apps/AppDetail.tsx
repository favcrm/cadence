import { useEffect, useState } from "react";
import { fmtTime } from "../../lib/fmt";
import { resources } from "../../lib/resources";
import { useQuery, useResource } from "../../lib/useResource";
import type { AppDetail as AppDetailRow, AppRun, AppWorkflow, OutboxItem } from "../../lib/types";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { homeNeeds, type HomeNeed } from "../home/needs";
import RunForm from "../projects/RunForm";
import { progressView } from "../projects/work";
import { ProgressBar } from "../projects/HealthBadge";
import ApproveApp from "./ApproveApp";
import {
  appApprovalChip,
  appWorkflowRow,
  doctorFindings,
  outboxHref,
  runState,
  sourceLabel,
  unboundSlots,
} from "./apps";
import type { Viewer } from "../projects/work";

/**
 * `/apps/<project>/<name>` (CAD-557) — one installed app's detail, the
 * daemon's `app show` plus its `app doctor` row: the agent guide, each
 * workflow's checked summary (with Run opening the CAD-496 form in a
 * drawer here, so the operator never leaves the app), the app's runs
 * and their outputs (CAD-563), the rubrics, the slot bindings and the
 * doctor's slot findings. The operator's Approve sits next to the state
 * while approval is pending.
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
  const state = useQuery(resources.app(`${project}/${name}`));
  const app = state.data;
  const approval = app ? appApprovalChip(app) : null;
  // The workflow whose run form is open in the drawer, by its
  // app-qualified name (`<app>/<wf>`).
  const [running, setRunning] = useState<string | null>(null);
  return (
    <>
      <main className="px-4 lg:px-8 pt-4 pb-9 min-w-0 max-w-3xl" aria-label={`app ${project}/${name}`}>
        <Link href="/apps" className="lnk text-label">
          ← all apps
        </Link>
        <div className="flex flex-wrap items-center gap-2 mt-2 mb-3">
          <h1 className="text-section font-semibold text-ink-100 num">
            {project}/{name}
          </h1>
          {approval && <span className={`chip ${approval.cls}`}>{approval.text}</span>}
          <StaleChip state={state} />
          {app && <ApproveApp row={app} viewer={viewer} />}
        </div>
        <ResourceGate
          state={state}
          loading={`loading ${project}/${name}…`}
          failed={`could not load ${project}/${name}`}
          onRetry={() => void resources.app(`${project}/${name}`).invalidate()}
        />
        {app && <AppBody project={project} app={app} onRun={setRunning} onOpenIssue={onOpenIssue} />}
      </main>
      {running && app && (
        <RunDrawer
          project={project}
          app={app}
          wf={running}
          viewer={viewer}
          onClose={() => setRunning(null)}
          onOpenIssue={onOpenIssue}
          onHome={onHome}
        />
      )}
    </>
  );
}

function AppBody({
  project,
  app,
  onRun,
  onOpenIssue,
}: {
  project: string;
  app: AppDetailRow;
  onRun: (wf: string) => void;
  onOpenIssue: (id: string) => void;
}) {
  const source = sourceLabel(app);
  const unbound = new Set(unboundSlots(app));
  const findings = doctorFindings(app.doctor);
  return (
    <div className="space-y-4">
      <section className="card px-4 py-3.5 min-w-0">
        <div className="flex flex-wrap items-center gap-1.5">
          {app.version && <span className="chip bg-ink-800 text-ink-300">v{app.version}</span>}
          {app.title && <span className="text-cardtitle text-ink-100">{app.title}</span>}
        </div>
        <div className="num text-micro text-ink-500 mt-2 space-y-0.5 break-all">
          {app.digest && <div>digest {app.digest}</div>}
          {source && <div>{source}</div>}
          {app.installed_at && (
            <div>
              installed {app.installed_at}
              {app.installed_by ? ` by ${app.installed_by}` : ""}
            </div>
          )}
        </div>
      </section>

      {app.guide && (
        <section className="card px-4 py-3.5 min-w-0" aria-label="guide">
          <div className="slabel mb-2">agent guide — app.md</div>
          <div className="text-body text-ink-200">
            <Md text={app.guide} />
          </div>
        </section>
      )}

      <section className="card px-4 py-3.5 min-w-0" aria-label="workflows">
        <div className="slabel mb-2">workflows</div>
        {(app.workflows ?? []).length === 0 && (
          <p className="text-label text-ink-500">This app declares no workflows.</p>
        )}
        <ul className="space-y-2">
          {(app.workflows ?? []).map((wf) => (
            <WorkflowRowView key={wf.name} wf={wf} onRun={() => onRun(wf.name)} />
          ))}
        </ul>
      </section>

      <RunsSection project={project} name={app.name ?? ""} onOpenIssue={onOpenIssue} />

      <OutputsSection project={project} name={app.name ?? ""} />

      {(app.rubrics ?? []).length > 0 && (
        <section className="card px-4 py-3.5 min-w-0" aria-label="rubrics">
          <div className="slabel mb-2">rubrics</div>
          <ul className="space-y-3">
            {(app.rubrics ?? []).map((r) => (
              <li key={r.name}>
                <div className="num text-label text-accent mb-1">{r.name}</div>
                <div className="text-body text-ink-200">
                  <Md text={r.body} />
                </div>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="card px-4 py-3.5 min-w-0" aria-label="connection slots">
        <div className="slabel mb-2">connection slots</div>
        {(app.connections ?? []).length === 0 && (
          <p className="text-label text-ink-500">This app declares no connection slots.</p>
        )}
        <ul className="space-y-1">
          {(app.connections ?? []).map((c) => (
            <li key={c.slot} className="num text-label flex items-baseline gap-2 min-w-0">
              <span className="text-ink-200">{c.slot}</span>
              {c.bound == null ? (
                <span className="text-warn">— unbound</span>
              ) : (
                <span className="text-ink-400">→ {c.bound}</span>
              )}
            </li>
          ))}
        </ul>
        {unbound.size > 0 && (
          <p className="text-micro text-warn mt-2" role="note">
            {unbound.size} unbound slot{unbound.size === 1 ? "" : "s"} — bind with{" "}
            <code className="num">cadence app set {project}/{app.name} &lt;slot&gt; &lt;connection&gt;</code>
          </p>
        )}
      </section>

      <section className="card px-4 py-3.5 min-w-0" aria-label="doctor findings">
        <div className="slabel mb-2">doctor findings</div>
        {findings.length === 0 ? (
          <p className="text-label text-ink-500">
            {app.doctor ? "No findings — every slot resolves." : "Not in the doctor scan."}
          </p>
        ) : (
          <ul className="space-y-1">
            {findings.map((f, i) => (
              <li key={i} className={`num text-label ${f.cls} break-words`}>
                {f.text}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

function WorkflowRowView({ wf, onRun }: { wf: AppWorkflow; onRun: () => void }) {
  return (
    <li className="min-w-0" data-workflow={wf.name}>
      <div className="flex flex-wrap items-center gap-2 min-w-0">
        <span className="num text-label text-accent break-all">{wf.name}</span>
        {wf.title && <span className="text-label text-ink-200 min-w-0">{wf.title}</span>}
        {wf.ok === false && <span className="chip bg-fail/10 text-fail">errors</span>}
        {typeof wf.tickets === "number" && (
          <span className="chip bg-ink-800 text-ink-500">
            {wf.tickets} ticket{wf.tickets === 1 ? "" : "s"}
          </span>
        )}
        <button
          type="button"
          onClick={onRun}
          className="chip bg-accent/15 text-accent hover:bg-accent/25 transition-colors"
          title={`open ${wf.name} in the New run form here`}
          aria-label={`run ${wf.name}`}
        >
          run →
        </button>
      </div>
      {(wf.errors ?? []).map((e, i) => (
        <p key={i} className="text-micro text-fail mt-1 break-words" role="note">
          {e}
        </p>
      ))}
      {(wf.inputs ?? []).length > 0 && (
        <p className="text-micro text-ink-500 mt-1 break-words">
          inputs: {(wf.inputs ?? []).map((i) => i.name).join(", ")}
        </p>
      )}
    </li>
  );
}

/**
 * The app's runs — the plans/epics proposed from its workflows, by the
 * recorded `plan.workflow` provenance (CAD-563). The needs-you marker
 * reads the same overview the Home rail does; until it loads, a run
 * still shows its own state (waiting approval, running, done).
 */
function RunsSection({
  project,
  name,
  onOpenIssue,
}: {
  project: string;
  name: string;
  onOpenIssue: (id: string) => void;
}) {
  const state = useQuery(resources.appRuns(`${project}/${name}`));
  // Passive: the app detail screen asks for the overview (App.tsx), so
  // the rail's data lands without a second fetch from here.
  const overview = useResource(resources.overview);
  const needs = homeNeeds(overview.data?.needs_me);
  const runs = state.data ?? [];
  return (
    <section className="card px-4 py-3.5 min-w-0" aria-label="runs">
      <div className="flex flex-wrap items-baseline gap-2 mb-2">
        <div className="slabel">runs</div>
        <StaleChip state={state} />
      </div>
      <ResourceGate
        state={state}
        loading="loading runs…"
        failed="could not load runs"
        onRetry={() => void resources.appRuns(`${project}/${name}`).invalidate()}
      />
      {state.data && runs.length === 0 && (
        <p className="text-label text-ink-500">
          No runs yet — Run a workflow above to propose one.
        </p>
      )}
      <ul className="space-y-2.5">
        {runs.map((run) => (
          <RunRow key={run.epic} run={run} needs={needs} onOpenIssue={onOpenIssue} />
        ))}
      </ul>
    </section>
  );
}

function RunRow({
  run,
  needs,
  onOpenIssue,
}: {
  run: AppRun;
  needs: HomeNeed[];
  onOpenIssue: (id: string) => void;
}) {
  const state = runState(run, needs);
  const progress = progressView(run.plan.progress);
  const tone = state.text === "done" ? "ok" : state.text === "running" ? "ok" : "warn";
  return (
    <li className="min-w-0" data-run={run.epic}>
      <div className="flex flex-wrap items-center gap-2 min-w-0">
        <button
          type="button"
          onClick={() => onOpenIssue(run.epic)}
          className="lnk num text-label shrink-0"
          title={`open ${run.epic}`}
        >
          {run.epic}
        </button>
        <span className="text-label text-ink-100 min-w-0 break-words flex-1">{run.title}</span>
        <span className={`chip ${state.cls}`} data-state={state.text}>
          {state.text}
        </span>
        {state.needsYou && (
          <Link href="/" className="chip bg-warn/10 text-warn hover:bg-warn/20 transition-colors">
            needs you →
          </Link>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-2 mt-1.5">
        <span className="num text-micro text-ink-500">{run.workflow}</span>
        <div className="min-w-[12rem] flex-1">
          <ProgressBar view={progress} tone={tone} />
        </div>
      </div>
    </li>
  );
}

/**
 * The app's outputs — the outbox items its runs produced (CAD-563),
 * read through the operator-gated route; on a board that cannot prove
 * the operator the section says so, like the Outbox screen, and never
 * pretends the ledger is empty.
 */
function OutputsSection({ project, name }: { project: string; name: string }) {
  const state = useQuery(resources.appOutputs(`${project}/${name}`));
  const items = state.data ?? [];
  return (
    <section className="card px-4 py-3.5 min-w-0" aria-label="outputs">
      <div className="flex flex-wrap items-baseline gap-2 mb-2">
        <div className="slabel">outputs</div>
        <StaleChip state={state} />
        <Link href="/outbox" className="lnk text-label ml-auto">
          Outbox →
        </Link>
      </div>
      <ResourceGate
        state={state}
        loading="loading outputs…"
        failed="could not load outputs"
        onRetry={() => void resources.appOutputs(`${project}/${name}`).invalidate()}
      />
      {state.status === "failed" && (
        <p className="text-label text-ink-400">
          the outbox is the operator's view — sign in with{" "}
          <code className="text-ink-300">cadence ui login</code> to read it
        </p>
      )}
      {state.data && items.length === 0 && (
        <p className="text-label text-ink-500">
          Nothing published yet — a released publish from this app's runs lands here.
        </p>
      )}
      <ul className="space-y-2">
        {items.map((it) => (
          <OutputRow key={it.effect_id} item={it} />
        ))}
      </ul>
    </section>
  );
}

function OutputRow({ item }: { item: OutboxItem }) {
  return (
    <li className="min-w-0">
      <Link
        href={outboxHref(item.effect_id)}
        className="block rounded px-2 py-1.5 -mx-2 hover:bg-ink-800/60 transition-colors"
      >
        <div className="flex items-baseline gap-3 min-w-0">
          <span className="text-label text-ink-100 truncate">{item.title}</span>
          <span className="num text-micro text-ink-500 shrink-0">
            {item.project} · {fmtTime(item.published_at)}
          </span>
        </div>
        {item.preview && (
          <p className="text-micro text-ink-400 mt-0.5 whitespace-pre-wrap break-words line-clamp-2">
            {item.preview}
          </p>
        )}
      </Link>
    </li>
  );
}

/**
 * The run form in a drawer over the app page (CAD-563) — the same
 * `RunForm` the Workflows screen opens, so the app's workflows run
 * without leaving the app. Escape or the backdrop closes it; the
 * propose stays the board's OperatorOnly route.
 */
function RunDrawer({
  project,
  app,
  wf,
  viewer,
  onClose,
  onOpenIssue,
  onHome,
}: {
  project: string;
  app: AppDetailRow;
  wf: string;
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
              {row.title ?? row.name}
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
          />
        </div>
      </aside>
    </>
  );
}
