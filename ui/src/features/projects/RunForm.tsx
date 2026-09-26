import { useEffect, useRef, useState } from "react";
import { api, type ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { WorkflowRow } from "../../lib/types";
import { appFieldLabel, slugFromTopic, slugProblem, teamRole } from "../apps/apps";
import {
  missingRequired,
  proposeBlock,
  proposedEpic,
  providedInputs,
  refusalText,
  runFields,
  type RunField,
} from "./workflows";
import type { Viewer } from "./work";

/** The live preview's state — the rendered plan or the named render refusal. */
interface PreviewState {
  loading: boolean;
  rendered: string | null;
  error: string | null;
  /** The daemon's refusal code (one_line, not_distinct, render_diverged). */
  code: string | null;
}

/**
 * The app page's variant (CAD-563 r2/r3): the input the run is about is
 * shown first and everything else folds under "More options" (the
 * folder name, the keyword, and the team by role); `prefill` seeds
 * values the operator should not have to retype (the team from the last
 * run), `slugInput` derives from the primary input until it is edited by
 * hand, `start` is the primary button's plain label, `note` the line
 * under it, and `validate` a live reason the run cannot start (the
 * folder name's shape, a team that violates the workflow's kept-apart
 * rule).
 */
export interface AppRunForm {
  primary: string | null;
  prefill: Record<string, string>;
  slugInput: string | null;
  start: string;
  note?: string;
  validate?: (values: Record<string, string>) => string | null;
}

/**
 * The run form for one workflow (CAD-496): a field per declared input
 * (`ask` as the label, optionals marked), a debounced live preview of
 * the rendered plan file, and Propose — disabled with the reason while
 * the board, the gate or the form is not ready. Extracted from the
 * Workflows screen (CAD-563) so an app's page opens the same form in a
 * drawer — one component, no second copy of the propose logic. The
 * row's `name` is a stored workflow's, or `<app>/<wf>` for an installed
 * app's; the preview and propose routes take both.
 */
export default function RunForm({
  row,
  viewer,
  onOpenIssue,
  onHome,
  app,
}: {
  row: WorkflowRow;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onHome: () => void;
  app?: AppRunForm;
}) {
  const fields = runFields(row);
  const [values, setValues] = useState<Record<string, string>>({});
  const [preview, setPreview] = useState<PreviewState>({
    loading: true,
    rendered: null,
    error: null,
    code: null,
  });
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [proposed, setProposed] = useState<{ epic: string; title: string; tickets: number } | null>(null);
  const request = useRef(0);
  const inputs = providedInputs(row, values);

  // The team (and anything else worth keeping) arrives with the app's
  // last run; a field the operator has touched is never overwritten.
  const prefill = app ? JSON.stringify(app.prefill) : "";
  useEffect(() => {
    if (!app) return;
    setValues((cur) => {
      let next = cur;
      for (const [name, value] of Object.entries(app.prefill)) {
        if (!value || (cur[name] ?? "").trim() !== "") continue;
        next = next === cur ? { ...cur } : next;
        next[name] = value;
      }
      return next;
    });
  }, [prefill]);

  // The slug a topic suggests, until the operator edits it by hand.
  const slug = app?.slugInput ?? null;
  const topic = app?.primary ?? null;
  const slugTouched = useRef(false);
  const topicValue = topic ? (values[topic] ?? "") : "";
  useEffect(() => {
    if (!slug || !topic || slugTouched.current) return;
    setValues((cur) => ({ ...cur, [slug]: slugFromTopic(topicValue) }));
  }, [slug, topic, topicValue]);

  // The rendered plan file, re-rendered shortly after the last keystroke.
  useEffect(() => {
    const seq = ++request.current;
    const timer = window.setTimeout(() => {
      api
        .workflowPreview(row.project, row.name, inputs)
        .then((next) => {
          if (request.current !== seq) return;
          setPreview({
            loading: false,
            rendered: next.rendered ?? null,
            error: next.error ?? null,
            code: next.code ?? null,
          });
        })
        .catch((e: ApiError) => {
          if (request.current !== seq) return;
          setPreview({ loading: false, rendered: null, error: e.message ?? String(e), code: e.code ?? null });
        });
    }, 250);
    return () => window.clearTimeout(timer);
  }, [row.project, row.name, JSON.stringify(inputs)]);

  const missing = missingRequired(row, values);
  const gate = proposeBlock(row, viewer, values);
  // The app variant's live validation (the folder name's shape, a team
  // that breaks the workflow's kept-apart rule) blocks like the gate.
  const invalid = app?.validate?.(values) ?? null;
  const blocked = gate ?? invalid;

  const propose = () => {
    if (blocked || busy) return;
    setBusy(true);
    setResult(null);
    api
      .workflowPropose(row.project, row.name, inputs)
      .then((out) => {
        const done = proposedEpic(out);
        setProposed(done);
        // The daemon emitted plan_proposed; the new epic is an issues
        // row, and Needs you reads the overview (an unobserved store
        // just marks invalid — no fetch while Home is off screen). An
        // app's runs list re-reads too.
        void resources.issues.invalidate();
        void resources.overview.invalidate();
        if (row.app) void resources.appRuns(`${row.project}/${row.app}`).invalidate();
      })
      .catch((e: ApiError) =>
        setResult({ ok: false, text: refusalText(e.code, e.message ?? String(e)) }),
      )
      .finally(() => setBusy(false));
  };

  const field = (f: RunField) => {
    const label = app ? appFieldLabel(f.name, f.label) : f.label;
    const problem = app && f.name === app.slugInput ? slugProblem(values[f.name] ?? "") : null;
    return (
      <div key={f.name}>
        <label htmlFor={`wf-${row.name}-${f.name}`} className="text-label text-ink-300 block mb-1">
          {label}
          {f.optional && <span className="text-ink-500"> · optional</span>}
        </label>
        <input
          id={`wf-${row.name}-${f.name}`}
          value={values[f.name] ?? ""}
          onChange={(e) => {
            if (f.name === slug) slugTouched.current = true;
            setValues((cur) => ({ ...cur, [f.name]: e.target.value }));
          }}
          className="field w-full"
          placeholder={f.name}
          aria-label={`${row.name} input ${f.name}`}
          aria-invalid={problem !== null || undefined}
          data-input={f.name}
        />
        {problem && (
          <p className="text-micro text-warn mt-1 break-words" role="note">
            {problem}
          </p>
        )}
      </div>
    );
  };

  const head = app && app.primary ? fields.filter((f) => f.name === app.primary) : fields;
  const more = fields.filter((f) => !head.includes(f));
  // The app variant groups the folded fields: the run's own options
  // first, the team under its own heading.
  const team = app ? more.filter((f) => teamRole(f.name) !== null) : [];
  const options = app ? more.filter((f) => teamRole(f.name) === null) : more;

  const previewBlock = (
    <section className="min-w-0" aria-label={`${row.name} plan preview`}>
      <div className="slabel mb-1.5">
        rendered plan{preview.loading ? " · rendering…" : ""}
      </div>
      {preview.error && (
        <p className="card px-3.5 py-2.5 text-label text-warn break-words" role="note">
          {refusalText(preview.code, preview.error)}
        </p>
      )}
      {preview.rendered && (
        <pre className="num text-micro text-ink-300 leading-relaxed whitespace-pre-wrap break-words rounded border border-ink-700 bg-ink-900 p-3 max-h-[26rem] overflow-auto">
          {preview.rendered}
        </pre>
      )}
      {!preview.loading && !preview.rendered && !preview.error && (
        <p className="text-label text-ink-500">No preview — the workflow file did not render.</p>
      )}
    </section>
  );

  const submit = (label: string) => (
    <div className="mt-3">
      <button
        type="button"
        onClick={propose}
        disabled={busy || blocked !== null}
        className="h-8 px-3 rounded bg-accent text-on-accent text-label font-medium disabled:opacity-40"
        title={blocked ?? undefined}
      >
        {busy ? "Starting…" : label}
      </button>
      {blocked && <p className="text-micro text-ink-500 mt-1.5 break-words">{blocked}</p>}
      {!blocked && missing.length === 0 && viewer.operator && !app && (
        <p className="text-micro text-ink-500 mt-1.5">
          Proposes via the daemon's plan_propose — the plan waits in Needs you.
        </p>
      )}
      {result && !result.ok && (
        <p className="text-label text-fail mt-1.5 break-words" role="alert">
          {result.text}
        </p>
      )}
    </div>
  );

  const done = (p: { epic: string; tickets: number }) => (
    <div className="mt-3" role="status">
      <p className="text-label text-ok break-words">
        plan {p.epic} proposed — {p.tickets} ticket{p.tickets === 1 ? "" : "s"} · waiting in Needs you
      </p>
      <div className="flex flex-wrap gap-2 mt-2">
        <button
          type="button"
          onClick={onHome}
          className="h-8 px-3 rounded bg-accent text-on-accent text-label font-medium"
        >
          Needs you →
        </button>
        <button
          type="button"
          onClick={() => onOpenIssue(p.epic)}
          className="h-8 px-3 rounded border border-ink-600 text-label text-ink-300 hover:border-edge-hover"
        >
          open {p.epic}
        </button>
      </div>
    </div>
  );

  return (
    <div className={app ? "px-3.5 py-3 min-w-0" : "border-t border-ink-700 px-3.5 py-3 grid gap-4 lg:grid-cols-2 min-w-0"}>
      <section className="min-w-0" aria-label={`${row.name} inputs`}>
        <div className="slabel mb-1.5">{app ? "the run" : "new run"}</div>
        {fields.length === 0 && (
          <p className="text-label text-ink-500">This workflow declares no inputs — it proposes as written.</p>
        )}
        <div className="space-y-3">
          {head.map(field)}
        </div>
        {more.length > 0 && (
          <details className="mt-3" open={!app ? true : undefined}>
            <summary className="slabel cursor-pointer select-none">
              {app ? "more options" : "all fields"}
            </summary>
            <div className="space-y-3 mt-2.5">
              {options.map(field)}
              {team.length > 0 && (
                <>
                  <div className="slabel pt-1">team</div>
                  {team.map(field)}
                </>
              )}
            </div>
          </details>
        )}
        {proposed ? done(proposed) : submit(app ? app.start : "Propose plan")}
        {app?.note && !proposed && (
          <p className="text-micro text-ink-400 mt-2 break-words">{app.note}</p>
        )}
      </section>
      {app ? (
        <details className="min-w-0 mt-4">
          <summary className="slabel cursor-pointer select-none">show plan</summary>
          <div className="mt-2.5">{previewBlock}</div>
        </details>
      ) : (
        previewBlock
      )}
    </div>
  );
}
