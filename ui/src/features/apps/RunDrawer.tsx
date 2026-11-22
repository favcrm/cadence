import type { AppDetail as AppDetailRow, AppRun } from "../../lib/types";
import { useTranslate } from "../../lib/locale";
import DrawerShell from "../app-shell/shared/DrawerShell";
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
  const t = useTranslate();
  const row = (app.workflows ?? []).find((w) => w.name === wf);
  if (!row) return null;
  const inputs = row.inputs ?? [];
  const primary = inputs[0]?.name ?? null;
  const slugInput = inputs.some((i) => i.name === "slug") ? "slug" : null;
  // The saved default team wins; the last run fills any role it left
  // unset (CAD-577).
  const team = { ...teamFromLastRun(row, runs), ...app.team };
  // The run's plain name — the workflow's own `label:` ("New post"),
  // never the engine's "new run" (CAD-571).
  const title = row.label?.trim() || "New run";
  return (
    <DrawerShell
      kind="app-run"
      scrim="board"
      modal
      trapFocus
      size="lg"
      label={t("new run of {name}").replace("{name}", () => row.name)}
      kicker={`${t("in")} ${project} · ${title}`}
      title={title}
      onClose={onClose}
    >
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
          note: t("It waits for your approval before anything runs. Nothing is published without your OK."),
          // Live checks: the folder name's shape, and a team that
          // breaks the workflow's kept-apart rule (shown only then).
          validate: (values) =>
            (slugInput ? slugProblem(values[slugInput] ?? "") : null) ??
            distinctProblem(row, values),
        }}
      />
    </DrawerShell>
  );
}
