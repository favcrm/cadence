import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import type { AppDetail as AppDetailRow, AppWorkflow } from "../../lib/types";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import ApproveApp from "./ApproveApp";
import { appApprovalChip, doctorFindings, runHref, sourceLabel, unboundSlots } from "./apps";
import type { Viewer } from "../projects/work";

/**
 * `/apps/<project>/<name>` (CAD-557) — one installed app's detail, the
 * daemon's `app show` plus its `app doctor` row: the agent guide, each
 * workflow's checked summary (with Run opening the project's existing
 * New run form on `<app>/<wf>`), the rubrics, the slot bindings and the
 * doctor's slot findings. The operator's Approve sits next to the state
 * while approval is pending.
 */
export default function AppDetail({
  project,
  name,
  viewer,
}: {
  project: string;
  name: string;
  viewer: Viewer;
}) {
  const state = useQuery(resources.app(`${project}/${name}`));
  const app = state.data;
  const approval = app ? appApprovalChip(app) : null;
  return (
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
      {app && <AppBody project={project} app={app} />}
    </main>
  );
}

function AppBody({ project, app }: { project: string; app: AppDetailRow }) {
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
            <WorkflowRowView key={wf.name} project={project} wf={wf} />
          ))}
        </ul>
      </section>

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

function WorkflowRowView({ project, wf }: { project: string; wf: AppWorkflow }) {
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
        <Link
          href={runHref(project, wf.name)}
          className="chip bg-accent/15 text-accent hover:bg-accent/25 transition-colors"
          title={`open ${wf.name} in the New run form`}
        >
          run →
        </Link>
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
