import { useEffect, useRef, useState } from "react";
import { api, type ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { WorkflowRow } from "../../lib/types";
import { useQuery } from "../../lib/useResource";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import {
  approvalChip,
  gateBlock,
  missingRequired,
  proposeBlock,
  proposedEpic,
  providedInputs,
  refusalText,
  runFields,
} from "./workflows";
import type { Viewer } from "./work";

/**
 * Projects → a project → Workflows (CAD-496): the project's stored
 * workflows — `<pm>/<key>/workflows/*.md`, next to PROJECT.md — each
 * with its declared inputs, ticket count and gate approval. "New run"
 * opens the workflow's form: one field per `inputs:` entry, a live
 * preview of the rendered plan file, and Propose — which calls the
 * board's relay of the daemon's `plan_propose`, the same path
 * `cadence plan propose --workflow` takes. The plan it lands waits in
 * Home's Needs you like any other proposal.
 */
export default function Workflows({
  project,
  viewer,
  onOpenIssue,
  onHome,
}: {
  project: string;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onHome: () => void;
}) {
  const state = useQuery(resources.workflows(project));
  const [open, setOpen] = useState<string | null>(null);
  const rows = state.data ?? [];

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="workflows">
      <div className="flex flex-wrap items-center gap-2 mb-3">
        <h1 className="text-section font-semibold text-ink-100">Workflows</h1>
        <StaleChip state={state} />
        <span className="kicker">
          {project}/workflows · next to PROJECT.md
        </span>
      </div>
      <ResourceGate state={state} loading="loading workflows…" failed="could not load workflows" onRetry={() => void resources.workflows(project).invalidate()} />
      {state.data && rows.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          No workflows in {project} yet —{" "}
          <span className="num">cadence workflow add</span> stores one beside PROJECT.md.
        </div>
      )}
      <ul className="space-y-2.5">
        {rows.map((row) => (
          <WorkflowCard
            key={row.name}
            row={row}
            viewer={viewer}
            open={open === row.name}
            onToggle={() => setOpen((cur) => (cur === row.name ? null : row.name))}
            onOpenIssue={onOpenIssue}
            onHome={onHome}
          />
        ))}
      </ul>
    </main>
  );
}

function WorkflowCard({
  row,
  viewer,
  open,
  onToggle,
  onOpenIssue,
  onHome,
}: {
  row: WorkflowRow;
  viewer: Viewer;
  open: boolean;
  onToggle: () => void;
  onOpenIssue: (id: string) => void;
  onHome: () => void;
}) {
  const approval = approvalChip(row);
  const blocked = gateBlock(row);
  return (
    <li className="card min-w-0" data-workflow={row.name}>
      <div className="px-3.5 py-3 min-w-0">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          aria-label={`${open ? "close" : "new run of"} ${row.name}`}
          className="w-full text-left flex items-start gap-2 min-w-0 rounded hover:text-accent"
        >
          <span className="num text-label text-accent shrink-0 pt-px">{row.name}</span>
          <span className="text-cardtitle font-medium text-ink-100 min-w-0 break-words flex-1">
            {row.title ?? row.name}
          </span>
          <svg
            width="10"
            height="10"
            viewBox="0 0 10 10"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            className={`shrink-0 mt-1.5 text-ink-500 transition-transform ${open ? "rotate-180" : ""}`}
            aria-hidden
          >
            <path d="M2 3.5l3 3 3-3" />
          </svg>
        </button>
        <div className="flex flex-wrap items-center gap-1.5 mt-2">
          <span className={`chip ${approval.cls}`}>{approval.text}</span>
          {typeof row.tickets === "number" && (
            <span className="chip bg-ink-800 text-ink-300">
              {row.tickets} ticket{row.tickets === 1 ? "" : "s"}
            </span>
          )}
          <span className="chip bg-ink-800 text-ink-500">
            {(row.inputs ?? []).length} input{(row.inputs ?? []).length === 1 ? "" : "s"}
          </span>
        </div>
        {(row.inputs ?? []).length > 0 && (
          <p className="text-micro text-ink-500 mt-2 break-words">
            inputs: {(row.inputs ?? []).map((i) => i.name).join(", ")}
          </p>
        )}
        {blocked && (
          <p className="text-label text-warn mt-2 break-words" role="note">
            {blocked}
          </p>
        )}
      </div>
      {open && (
        <RunForm row={row} viewer={viewer} onOpenIssue={onOpenIssue} onHome={onHome} />
      )}
    </li>
  );
}

/** The live preview's state — the rendered plan or the named render refusal. */
interface PreviewState {
  loading: boolean;
  rendered: string | null;
  error: string | null;
  /** The daemon's refusal code (one_line, not_distinct, render_diverged). */
  code: string | null;
}

/**
 * The run form for one workflow: a field per declared input (`ask` as
 * the label, optionals marked), a debounced live preview of the
 * rendered plan file, and Propose — disabled with the reason while the
 * board, the gate or the form is not ready.
 */
function RunForm({
  row,
  viewer,
  onOpenIssue,
  onHome,
}: {
  row: WorkflowRow;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onHome: () => void;
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
  const blocked = proposeBlock(row, viewer, values);

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
        // just marks invalid — no fetch while Home is off screen).
        void resources.issues.invalidate();
        void resources.overview.invalidate();
      })
      .catch((e: ApiError) =>
        setResult({ ok: false, text: refusalText(e.code, e.message ?? String(e)) }),
      )
      .finally(() => setBusy(false));
  };

  return (
    <div className="border-t border-ink-700 px-3.5 py-3 grid gap-4 lg:grid-cols-2 min-w-0">
      <section className="min-w-0" aria-label={`${row.name} inputs`}>
        <div className="slabel mb-1.5">new run</div>
        {fields.length === 0 && (
          <p className="text-label text-ink-500">This workflow declares no inputs — it proposes as written.</p>
        )}
        <div className="space-y-3">
          {fields.map((field) => (
            <div key={field.name}>
              <label
                htmlFor={`wf-${row.name}-${field.name}`}
                className="text-label text-ink-300 block mb-1"
              >
                {field.label}
                {field.optional && <span className="text-ink-500"> · optional</span>}
              </label>
              <input
                id={`wf-${row.name}-${field.name}`}
                value={values[field.name] ?? ""}
                onChange={(e) => setValues((cur) => ({ ...cur, [field.name]: e.target.value }))}
                className="field w-full"
                placeholder={field.name}
                aria-label={`${row.name} input ${field.name}`}
                data-input={field.name}
              />
            </div>
          ))}
        </div>
        {proposed ? (
          <div className="mt-3" role="status">
            <p className="text-label text-ok break-words">
              plan {proposed.epic} proposed — {proposed.tickets} ticket{proposed.tickets === 1 ? "" : "s"} · waiting in Needs you
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
                onClick={() => onOpenIssue(proposed.epic)}
                className="h-8 px-3 rounded border border-ink-600 text-label text-ink-300 hover:border-edge-hover"
              >
                open {proposed.epic}
              </button>
            </div>
          </div>
        ) : (
          <div className="mt-3">
            <button
              type="button"
              onClick={propose}
              disabled={busy || blocked !== null}
              className="h-8 px-3 rounded bg-accent text-on-accent text-label font-medium disabled:opacity-40"
              title={blocked ?? undefined}
            >
              {busy ? "Proposing…" : "Propose plan"}
            </button>
            {blocked && (
              <p className="text-micro text-ink-500 mt-1.5 break-words">{blocked}</p>
            )}
            {missing.length === 0 && viewer.operator && (
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
        )}
      </section>
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
    </div>
  );
}
