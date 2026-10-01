import { useState } from "react";
import { api, type ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useResource } from "../../lib/useResource";
import type { AppDetail as AppDetailRow, AppRun, AppWorkflow } from "../../lib/types";
import Button from "../../ui/Button";
import type { Viewer } from "../projects/work";
import ApproveApp from "./ApproveApp";
import {
  agentStateWord,
  appFieldLabel,
  approvalPending,
  primaryAction,
  publishTarget,
  teamCandidates,
  teamFromLastRun,
  teamInputs,
  usedSlots,
} from "./appViewModel";

/** Settings — where it publishes, the team, and the app's version. */
export default function SettingsTab({
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
          <p className="text-micro text-ink-500 mt-2 break-words">
            Slot binding lives on the installed app — open it under Workspace apps on the
            Apps screen, then bind a slot in its Settings.
          </p>
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
