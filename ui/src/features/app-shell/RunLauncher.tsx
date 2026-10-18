import { useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { useTranslate } from "../../lib/locale";
import type { Viewer } from "../projects/work";
import type { Agent, AppContext, InstallTeam, Installation, WorkspaceRun } from "../workspace-apps/workspaceApps";
import { workspaceApps } from "../workspace-apps/workspaceApps";
import { retainedRequest, completeRequest } from "../workspace-apps/requests";
import { WorkspaceDialog } from "../workspace-apps/WorkspaceDialog";
import { TeamSettings } from "../workspace-apps/TeamSettings";
import "../workspace-apps/workspace-apps.css";

/** One declared installed workflow as the installation detail reports it. */
type WorkflowDecl = NonNullable<Installation["workflows"]>[number];
/** Team-covered roles are displayed from the configured team, then frozen
 * explicitly into the operator's draft before approval. */
const teamCovered = (team: InstallTeam, name: string) => name in team.roles;

/** Registered, live, managed local workers: the eligibility the social
 *  workspace applies and the daemon re-proves on run create. */
const localWorkers = (agents: Agent[]) =>
  agents.filter((agent) =>
    agent.role === "worker" &&
    !agent.dead &&
    !agent.fenced &&
    !agent.inbox &&
    ((agent.provider === "codex" && ["managed", "managed-ws"].includes(agent.endpoint_kind)) ||
      (["claude", "pi"].includes(agent.provider) && agent.endpoint_kind === "managed") ||
      (agent.provider === "fake" && agent.endpoint_kind === "fake")));

/**
 * CAD-1560: the host-drawn launcher for an installed app's declared
 * local workflow (e.g. Meeting minutes' `consolidate`). Everything the
 * form draws comes from the verified installation receipt: the workflow's
 * declared label, `inputs` (`ask`/`optional`/`default`/`context_default`)
 * and `distinct` worker-role names — never app-name branches.
 *
 * Preparing creates only a draft. The operator sees its exact frozen inputs
 * and assignments before approving and dispatching the snapshot. Changed
 * installation/context receipts refuse; default-team changes cannot silently
 * replace the assignments the operator saw.
 */
export default function RunLauncher({
  installation,
  workflow,
  contextId,
  context,
  viewer,
  stillCurrent,
  onClose,
}: {
  installation: Installation;
  workflow: WorkflowDecl;
  /** The shell's verified selected context the run binds to. */
  contextId: string;
  context: AppContext | null;
  viewer: Viewer;
  /** False once the install/context/viewer this dialog was opened under moved. */
  stillCurrent: () => boolean;
  onClose: () => void;
}) {
  const t = useTranslate();
  const installId = installation.install_id;
  const [team, setTeam] = useState<InstallTeam | null | undefined>(undefined);
  const [agents, setAgents] = useState<Agent[] | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [values, setValues] = useState<Record<string, string>>({});
  const [picked, setPicked] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{ id: string; state: string } | null>(null);
  const [prepared, setPrepared] = useState<{ run: WorkspaceRun; requestKey: string } | null>(null);
  const mounted = useRef(false);
  useLayoutEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  // The scope this dialog opened under: the run is pinned to exactly it.
  const scope = useRef({ installId, contextId });
  const denied = (error: unknown) => error instanceof ApiError && [401, 403].includes(error.status);

  useEffect(() => {
    const controller = new AbortController();
    Promise.all([
      workspaceApps.team(installId, controller.signal),
      workspaceApps.agents(controller.signal),
    ]).then(([nextTeam, nextAgents]) => {
      if (controller.signal.aborted || !ownsDialog()) return;
      setTeam(nextTeam);
      setAgents(nextAgents);
      setReadError(null);
    }).catch((reason: unknown) => {
      if (controller.signal.aborted || !ownsDialog()) return;
      if (denied(reason)) { onClose(); return; }
      setReadError(reason instanceof Error ? reason.message : "The app team could not be read.");
    });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [installId, attempt]);

  const roles = useMemo(() => Object.fromEntries((workflow.distinct ?? []).map((name) => [name, true])), [workflow]);
  const title = workflow.label?.trim() || `New ${workflow.name}`;
  const canWrite = viewer.operator === true && !viewer.readOnly;

  const current = stillCurrent();
  useEffect(() => {
    // The scope moved while the dialog was open: close rather than let a
    // form filled for one context submit under another.
    if (!current) onClose();
  }, [current, onClose]);
  if (!current) return null;

  const ownsDialog = () => mounted.current && stillCurrent();
  const showFailure = (reason: unknown) => {
    if (!ownsDialog()) return;
    if (denied(reason)) { onClose(); return; }
    setError(reason instanceof Error ? reason.message : "The run could not be confirmed.");
  };
  const matchesReceipts = (run: WorkspaceRun) =>
    run.install_id === installId && run.context_id === (scope.current.contextId || null) &&
    run.snapshot.bundle_digest === installation.digest &&
    (context === null
      ? run.snapshot.context == null
      : run.snapshot.context?.id === context.id &&
        run.snapshot.context.revision === context.revision && run.snapshot.context.digest === context.digest) &&
    Object.keys(run.snapshot.capabilities ?? {}).length === 0 &&
    Object.keys(run.snapshot.quotes ?? {}).length === 0;

  const contentInputs = workflow.inputs.filter((input) => roles[input.name] !== true);
  const roleInputs = workflow.inputs.filter((input) => roles[input.name] === true);
  const ownerPm = team?.owner_pm ?? "";
  const workers = agents === null ? [] : localWorkers(agents).filter((agent) => agent.group === ownerPm);
  const managers = agents === null ? [] : agents.filter((agent) => agent.role === "pm" && !agent.dead && !agent.fenced);

  /** Preserve declared input text; never send a role the app team injects. */
  const collectedInputs = () => {
    const inputs: Record<string, string> = {};
    for (const input of workflow.inputs) {
      if (team !== null && team !== undefined && teamCovered(team, input.name)) continue;
      const value = input.name in picked ? (picked[input.name] ?? "") : (values[input.name] ?? "");
      if (value !== "") inputs[input.name] = value;
    }
    return inputs;
  };

  const missingRequired = () =>
    workflow.inputs
      .filter((input) =>
        input.optional !== true &&
        input.default == null &&
        !(input.context_default === true && (context?.config.input_defaults[input.name] ?? "") !== "") &&
        !(team !== null && team !== undefined && teamCovered(team, input.name)) &&
        (input.name in picked ? (picked[input.name] ?? "") : (values[input.name] ?? "")) === "")
      .map((input) => input.name);

  const distinctError = () => {
    const declared = workflow.distinct ?? [];
    const seen = new Map<string, string>();
    for (const name of declared) {
      const alias = team !== null && team !== undefined && teamCovered(team, name)
        ? team.roles[name]
        : picked[name] ?? "";
      if (alias === "") continue;
      const first = seen.get(alias);
      if (first) return `${name} must be a different registered worker than ${first}.`;
      seen.set(alias, name);
    }
    return null;
  };

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (busy || team === null || team === undefined || agents === null || !canWrite) return;
    const missing = missingRequired();
    if (missing.length > 0) {
      setError(`Missing required input: ${missing.join(", ")}.`);
      return;
    }
    const distinct = distinctError();
    if (distinct !== null) {
      setError(distinct);
      return;
    }
    for (const input of roleInputs) {
      if (teamCovered(team, input.name)) continue;
      const alias = picked[input.name] ?? "";
      if (!workers.some((worker) => worker.alias === alias)) {
        setError(`Choose a registered worker in ${ownerPm}'s group for ${input.name.replace(/_/g, " ")}.`);
        return;
      }
    }
    if (!ownsDialog()) { onClose(); return; }
    setError(null);
    setBusy(true);
    // Preparing freezes the displayed team, not a newer default team that
    // another tab may have saved. No approval or dispatch occurs here.
    const inputs = collectedInputs();
    for (const input of workflow.inputs) {
      if (teamCovered(team, input.name)) inputs[input.name] = team.roles[input.name];
    }
    const requestKey = JSON.stringify({
      installId, digest: installation.digest, contextId: scope.current.contextId,
      contextRevision: context?.revision, contextDigest: context?.digest,
      workflow: workflow.name, team, inputs,
    });
    void workspaceApps.createRun({
      install_id: installId,
      workflow: workflow.name,
      owner_pm: team.owner_pm,
      inputs,
      request_id: retainedRequest(requestKey),
      ...(scope.current.contextId ? { context_id: scope.current.contextId } : {}),
    }).then((run) => {
      if (!ownsDialog()) return;
      if (!matchesReceipts(run) || run.snapshot.owner_pm !== team.owner_pm) {
        throw new Error("The frozen installation or context no longer matches this dialog. No approval or dispatch was sent; reload the app before continuing.");
      }
      setPrepared({ run, requestKey });
      if (run.state !== "awaiting_approval" && run.state !== "approved") {
        setResult({ id: run.id, state: run.state });
      }
    }).catch(showFailure).finally(() => { if (ownsDialog()) setBusy(false); });
  };

  const confirm = () => {
    if (busy || prepared === null || !canWrite || !ownsDialog()) return;
    const { run, requestKey } = prepared;
    setError(null);
    setBusy(true);
    void (async () => {
      // Read before retrying an uncertain approval/dispatch, never replay a
      // side effect merely because its response was not observed.
      let next = await workspaceApps.getRun(run.id);
      if (!ownsDialog()) return;
      if (next.snapshot_digest !== run.snapshot_digest || !matchesReceipts(next)) {
        throw new Error("The frozen plan no longer matches this dialog. No approval or dispatch was sent.");
      }
      if (next.state === "awaiting_approval") {
        next = await workspaceApps.approveRun(next.id, run.snapshot_digest);
        if (!ownsDialog()) return;
      }
      if (next.state === "approved") {
        next = await workspaceApps.dispatchRun(next.id);
        if (!ownsDialog()) return;
      }
      setPrepared({ run: next, requestKey });
      setResult({ id: next.id, state: next.state });
    })().catch(showFailure).finally(() => { if (ownsDialog()) setBusy(false); });
  };

  const close = () => {
    // Only an explicit acknowledgement of a displayed result retires replay
    // identity. Unmount/scope changes and unseen late responses retain it.
    if (result !== null && prepared !== null) completeRequest(prepared.requestKey);
    onClose();
  };

  return (
    <WorkspaceDialog title={title} onClose={close}>
      {error !== null && <p className="wa-alert" data-tone="fail" role="alert">{error}</p>}
      {team === undefined || agents === null ? (
        readError !== null
          ? <div className="wa-stack"><p className="wa-alert" data-tone="fail" role="alert">{readError}</p>
              <Button size="sm" onClick={() => { setReadError(null); setTeam(undefined); setAttempt((n) => n + 1); }}>{t("Retry")}</Button></div>
          : <p className="wa-muted" role="status">{t("Reading the app's team…")}</p>
      ) : team === null ? (
        // No configured team means no owner PM to pin — start is ABI-refused
        // without one, so the operator sets the team here first.
        <TeamSettings
          installation={installation}
          managers={managers}
          workers={localWorkers(agents)}
          canWrite={canWrite}
          busy={busy}
          mutate={async (work) => {
            setBusy(true);
            try {
              await work();
              if (!ownsDialog()) return;
              const next = await workspaceApps.team(installId);
              if (!ownsDialog()) return;
              setTeam(next);
            } catch (reason: unknown) {
              if (!ownsDialog()) return;
              if (denied(reason)) { onClose(); return; }
              setReadError(reason instanceof Error ? reason.message : "The app team could not be confirmed.");
              setTeam(undefined);
            } finally {
              setBusy(false);
            }
          }}
        />
      ) : result !== null ? (
        <div className="wa-stack">
          <p className="wa-muted" role="status">
            {result.state === "running"
              ? t("The run is started in {context} — the writer and reviewer pick it up; the screen lists it as it completes.").replace("{context}", () => context?.config.label ?? scope.current.contextId)
              : t("The run was created in {context} with state {state}.").replace("{context}", () => context?.config.label ?? scope.current.contextId).replace("{state}", () => result.state)}
          </p>
          <p className="wa-kicker">{t("Run")} {result.id}</p>
          {prepared?.run.failure && <p className="wa-alert" role="alert">{prepared.run.failure.reason}</p>}
          <Button onClick={close}>{t("Close")}</Button>
        </div>
      ) : prepared !== null ? (
        <div className="wa-stack">
          <h2>{t("Review frozen plan")}</h2>
          <p className="wa-muted">{prepared.run.state === "approved"
            ? t("Approval is already recorded for this frozen plan. Confirm to dispatch it.")
            : t("Nothing is dispatched until you approve this exact plan. Changed native receipts refuse approval or dispatch.")}</p>
          <p className="wa-muted">{t("Owner PM")}: {prepared.run.snapshot.owner_pm}</p>
          <p className="wa-kicker">{t("Context")}: {context?.config.label ?? t("No context")}</p>
          {Object.entries(prepared.run.snapshot.inputs).map(([name, value]) => (
            <div className="wa-field" key={name}>
              <span>{name.replace(/_/g, " ")}</span>
              <pre className="wa-preview">{value}</pre>
            </div>
          ))}
          {prepared.run.snapshot.workflow.steps.map((step) => (
            <div className="wa-step" key={step.id}>
              <span>{step.kind === "review_text" ? t("Independent review") : t("Write text")} · {step.assignee}</span>
            </div>
          ))}
          <details className="wa-details">
            <summary>{t("Inspect the exact plan")}</summary>
            <pre>{JSON.stringify(prepared.run.snapshot, null, 2)}</pre>
            <p className="wa-digest">{prepared.run.snapshot_digest}</p>
          </details>
          <div className="wa-row">
            <Button onClick={() => setPrepared(null)} disabled={busy}>{t("Edit inputs")}</Button>
            <Button variant="primary" loading={busy} disabled={!canWrite} onClick={confirm}>
              {prepared.run.state === "approved" ? t("Start approved plan") : t("Approve and start")}
            </Button>
          </div>
        </div>
      ) : (
        <form className="wa-stack" onSubmit={submit}>
          <p className="wa-muted">
            {t("Prepare a draft in {context}, then review its frozen inputs and assignments before starting it.").replace("{context}", () => context?.config.label ?? t("this scope"))}
            {" "}{t("The team below is the configured default read for this form; these displayed assignments are frozen into the draft.")}
          </p>
          {contentInputs.map((input) => {
            const id = `wa-run-${input.name}`;
            const label = input.ask?.trim() || input.name.replace(/_/g, " ");
            const required = input.optional !== true && input.default == null &&
              !(input.context_default === true && (context?.config.input_defaults[input.name] ?? "") !== "");
            const saved = input.context_default === true ? context?.config.input_defaults[input.name] : undefined;
            const hint = [
              input.example ? t("Example: {example}").replace("{example}", () => input.example!) : null,
              required ? null : input.default != null
                ? t("Leave empty to use the app default.")
                : saved ? t("Leave empty to use this context's saved default.") : t("Optional."),
            ].filter(Boolean).join(" ");
            return (
              <div className="wa-field" key={input.name}>
                <label htmlFor={id}>{label}{required ? "" : ` (${t("optional")})`}</label>
                <textarea
                  id={id}
                  name={input.name}
                  className="wa-input"
                  value={values[input.name] ?? ""}
                  onChange={(event) => setValues((previous) => ({ ...previous, [input.name]: event.target.value }))}
                  rows={3}
                  required={required}
                  disabled={busy}
                />
                {hint !== "" && <p className="wa-kicker">{hint}</p>}
              </div>
            );
          })}
          <div className="wa-field">
            <span className="wa-kicker">{t("Owner PM")}</span>
            <p className="wa-muted">{team.owner_pm}</p>
          </div>
          {roleInputs.map((input) => {
            const covered = teamCovered(team, input.name);
            return (
              <div className="wa-field" key={input.name}>
                <label htmlFor={`wa-run-${input.name}`}>
                  {input.ask?.trim() || input.name.replace(/_/g, " ")}
                </label>
                {covered ? (
                  <p className="wa-muted">{team.roles[input.name]} <span className="wa-kicker">{t("(app team)")}</span></p>
                ) : (
                  <Select
                    id={`wa-run-${input.name}`}
                    value={picked[input.name] ?? ""}
                    onChange={(value) => setPicked((previous) => ({ ...previous, [input.name]: value }))}
                    options={workers.map((worker) => ({ value: worker.alias, label: worker.alias, hint: worker.state_label || worker.state }))}
                    placeholder={t("Choose a registered worker")}
                    disabled={busy || workers.length === 0}
                    full
                  />
                )}
              </div>
            );
          })}
          {roleInputs.some((input) => !teamCovered(team, input.name)) && workers.length === 0 && (
            <p className="wa-alert">{t("No registered worker in this owner group can fill the role. Register one first.")}</p>
          )}
          <Button type="submit" variant="primary" loading={busy} disabled={!canWrite}>
            {t("Prepare {label}").replace("{label}", () => title)}
          </Button>
        </form>
      )}
    </WorkspaceDialog>
  );
}
