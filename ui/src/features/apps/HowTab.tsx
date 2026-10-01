import type { AppDetail as AppDetailRow, AppRun } from "../../lib/types";
import Md from "../../ui/Md";
import {
  appPurpose,
  connectionRows,
  distinctNote,
  doctorFindings,
  primaryAction,
  sourceLabel,
  stepRows,
  teamFromLastRun,
  unboundSlots,
  usedSlots,
} from "./appViewModel";

/** How it works — the steps in plain words, and the details folded away. */
export default function HowTab({ app, runs }: { app: AppDetailRow; runs: AppRun[] }) {
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
