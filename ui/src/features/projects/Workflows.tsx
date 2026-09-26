import { useEffect, useState } from "react";
import { resources } from "../../lib/resources";
import type { WorkflowRow } from "../../lib/types";
import { useQuery } from "../../lib/useResource";
import { useHref } from "../../lib/useLocation";
import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { appHref } from "../apps/apps";
import RunForm from "./RunForm";
import { approvalChip, gateBlock } from "./workflows";
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
 *
 * An installed app's workflow (`<app>/<wf>`) is listed, but it runs
 * from its app's page (CAD-563): the row reads "provided by <app>" and
 * links there, so a run always happens where the app lives. A stored
 * workflow is unchanged; `?run=<name>` still opens its form directly.
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
  // `?run=<name>` asks for that row's New run form to be open — the
  // stored workflow's own deep link. Follows the param so a second Run
  // click on this screen re-opens too; a name that resolves to no
  // stored row (an app's workflow, a typo) is ignored.
  const href = useHref();
  const run = new URLSearchParams(href.split("?")[1] ?? "").get("run");
  useEffect(() => {
    if (run && (state.data ?? []).some((r) => r.name === run && !r.app)) setOpen(run);
  }, [run, state.data]);

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
  const app = row.app ?? null;
  return (
    <li className="card min-w-0" data-workflow={row.name}>
      <div className="px-3.5 py-3 min-w-0">
        {app ? (
          <div className="w-full text-left flex items-start gap-2 min-w-0">
            <span className="num text-label text-accent shrink-0 pt-px">{row.name}</span>
            <span className="text-cardtitle font-medium text-ink-100 min-w-0 break-words flex-1">
              {row.title ?? row.name}
            </span>
          </div>
        ) : (
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
        )}
        <div className="flex flex-wrap items-center gap-1.5 mt-2">
          <span className={`chip ${approval.cls}`}>{approval.text}</span>
          {app && <span className="chip bg-accent/10 text-accent">provided by {app}</span>}
          {typeof row.tickets === "number" && (
            <span className="chip bg-ink-800 text-ink-300">
              {row.tickets} ticket{row.tickets === 1 ? "" : "s"}
            </span>
          )}
          <span className="chip bg-ink-800 text-ink-500">
            {(row.inputs ?? []).length} input{(row.inputs ?? []).length === 1 ? "" : "s"}
          </span>
          {app && (
            <Link
              href={appHref(row.project, app)}
              className="chip bg-accent/15 text-accent hover:bg-accent/25 transition-colors"
              title={`open ${app} — its workflows run from the app page`}
            >
              open app →
            </Link>
          )}
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
      {open && !app && (
        <RunForm row={row} viewer={viewer} onOpenIssue={onOpenIssue} onHome={onHome} />
      )}
    </li>
  );
}
