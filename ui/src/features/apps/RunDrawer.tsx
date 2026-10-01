import { useEffect } from "react";
import type { AppDetail as AppDetailRow, AppRun } from "../../lib/types";
import { IconClose } from "../../ui/icons";
import RunForm from "../projects/RunForm";
import type { Viewer } from "../projects/work";
import {
  appWorkflowRow,
  distinctProblem,
  slugProblem,
  startLabel,
  teamFromLastRun,
} from "./appViewModel";

/**
 * The New-run drawer (CAD-563): the same `RunForm` the Workflows screen
 * opens, in the app's variant — the topic first, the team from the last
 * run under "More options", the propose the board's OperatorOnly route.
 */
export default function RunDrawer({
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
