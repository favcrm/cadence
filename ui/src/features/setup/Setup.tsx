import { useEffect, useId, useRef, useState } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { IconCheck, IconRefresh } from "../../ui/icons";
import { useQuery } from "../../lib/useResource";
import { resources } from "../../lib/resources";
import { routePath } from "../../lib/router";
import {
  checkLabel,
  inGroup,
  isReady,
  masterNeedsUnmet,
  missingRequired,
  statusChip,
  MASTER_CLIS,
  type SetupCheck,
  type SetupReport,
  type Tone,
} from "./checks";
import { recheckSetup, setupResource, SETUP_ON_HOST } from "./setupApi";
import "./setup.css";

type StepId = "environment" | "agents" | "master" | "project";
const STEPS: { id: StepId; title: string; hint: string }[] = [
  {
    id: "environment",
    title: "Environment",
    hint: "State, tracker and daemon",
  },
  { id: "agents", title: "Agent CLIs", hint: "A supported CLI for the master" },
  { id: "master", title: "Master agent", hint: "Provider and login" },
  { id: "project", title: "Projects", hint: "Repositories to work on" },
];
const TONE: Record<Tone, string> = {
  ok: "bg-ok/15 text-ok",
  warn: "bg-warn/10 text-warn",
  fail: "bg-fail/10 text-fail",
  muted: "bg-ink-800 text-ink-400",
};
type Readiness = "ready" | "attention" | "unknown";
const LABEL: Record<Readiness, string> = {
  ready: "Ready",
  attention: "Needs attention",
  unknown: "Not checked",
};

/** Required names must be present: an empty group cannot pass vacuously. */
function readiness(
  id: StepId,
  report: SetupReport | null,
  projectCount: number | null,
): Readiness {
  if (id === "project")
    return projectCount === null
      ? "unknown"
      : projectCount > 0
        ? "ready"
        : "attention";
  if (!report) return "unknown";
  const names =
    id === "environment"
      ? ["state_dir", "tracker", "daemon", "ui"]
      : id === "master"
        ? ["master", "master_login"]
        : (report.master?.providers.map((provider) => provider.bin) ?? [
            ...MASTER_CLIS,
          ]);
  const checks = names.map((name) =>
    report.checks.find((check) => check.check === name),
  );
  if (id === "agents" && checks.some((check) => check && isReady(check)))
    return "ready";
  if (
    checks.length === 0 ||
    checks.some((check) => !check || check.status === "unknown")
  )
    return "unknown";
  return id !== "agents" && checks.every((check) => check && isReady(check))
    ? "ready"
    : "attention";
}

function CopyFix({ command }: { command: string }) {
  const [result, setResult] = useState<{
    command: string;
    copied: boolean;
  } | null>(null);
  const outcome = result?.command === command ? result : null;
  return (
    <div className="setup-command-wrap">
      <div className="setup-command">
        <code className="num">{command}</code>
        <Button
          size="sm"
          aria-label={`Copy command: ${command}`}
          onClick={async () => {
            try {
              if (!navigator.clipboard)
                throw new Error("Clipboard unavailable");
              await navigator.clipboard.writeText(command);
              setResult({ command, copied: true });
            } catch {
              setResult({ command, copied: false });
            }
          }}
        >
          {outcome?.copied ? "Copied" : "Copy"}
        </Button>
      </div>
      <p className="setup-copy-feedback" role="status">
        {outcome
          ? outcome.copied
            ? "Command copied."
            : "Copy unavailable. Select and copy the command."
          : null}
      </p>
    </div>
  );
}

function CheckList({ checks }: { checks: SetupCheck[] }) {
  if (checks.length === 0)
    return (
      <div className="card setup-empty" role="status">
        No checks were reported for this section. Check again to refresh the
        host report.
      </div>
    );
  return (
    <ul className="card setup-checks">
      {checks.map((check) => {
        const chip = statusChip(check);
        return (
          <li key={check.check}>
            <div className="setup-check-heading">
              <span>{checkLabel(check.check)}</span>
              <span className={`chip ${TONE[chip.tone]}`}>{chip.label}</span>
            </div>
            <p className="setup-check-detail">{check.detail}</p>
            {!isReady(check) && check.fix && <CopyFix command={check.fix} />}
          </li>
        );
      })}
    </ul>
  );
}

function MasterProviders({ report }: { report: SetupReport }) {
  const offers = report.master?.providers ?? [];
  const files = report.checks.find((c) => c.check === "master");
  if (
    offers.length === 0 ||
    (files && isReady(files)) ||
    masterNeedsUnmet(report)
  )
    return null;
  return (
    <div className="card setup-provider-offers">
      <p className="text-secondary text-ink-400">
        <span className="font-medium text-ink-200">Choose a provider.</span> Run
        its command on the host to prepare and start the master.
      </p>
      <ul className="setup-offers">
        {offers.map((o) => {
          const provider = report.checks.find((c) => c.check === o.bin);
          const chip = provider
            ? statusChip(provider)
            : { label: "not found", tone: "muted" as Tone };
          const command = o.start ?? (!o.ready ? provider?.fix : null) ?? null;
          return (
            <li key={o.bin} className="setup-offer">
              <div className="flex items-baseline gap-2">
                <span className="text-body font-medium text-ink-100">
                  {checkLabel(o.bin)}
                </span>
                <span className={`chip ${TONE[chip.tone]}`}>{chip.label}</span>
              </div>
              {provider && (
                <p className="text-secondary text-ink-500 mt-0.5 break-words [overflow-wrap:anywhere]">
                  {provider.detail}
                </p>
              )}
              {o.warning && (
                <p className="mt-1.5 rounded bg-warn/10 px-3 py-2 text-secondary font-medium text-warn break-words [overflow-wrap:anywhere]">
                  {o.warning}
                </p>
              )}
              {command && <CopyFix command={command} />}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function StepBody({
  step,
  report,
  projects,
  projectError,
  projectsLoading,
  retryProjects,
}: {
  step: StepId;
  report: SetupReport | null;
  projects: string[] | null;
  projectError: string | null;
  projectsLoading: boolean;
  retryProjects: () => void;
}) {
  switch (step) {
    case "environment": {
      const checks = inGroup(report, "environment");
      return (
        <>
          <h2>Environment</h2>
          <p className="setup-intro">
            The host's state, tracker and daemon. Follow a command below for any
            check that needs attention.
          </p>
          <CheckList checks={checks} />
          {checks.some((check) => !isReady(check)) &&
            !checks.some((check) => check.fix === "cadence setup") && (
              <div className="card setup-help">
                <p>
                  Prepare missing essentials with{" "}
                  <code className="num">cadence setup</code>, then check again.
                </p>
                <CopyFix command="cadence setup" />
              </div>
            )}
        </>
      );
    }
    case "agents":
      return (
        <>
          <h2>Agent CLIs</h2>
          <p className="setup-intro">
            Installed and signed-in CLIs. The master needs one of its supported
            providers; other CLIs can be used as workers.
          </p>
          {report && (
            <p className="setup-provider-note">
              Master providers:{" "}
              {(
                report.master?.providers.map((provider) => provider.bin) ?? [
                  ...MASTER_CLIS,
                ]
              )
                .map(checkLabel)
                .join(", ") || "None reported"}
              .
            </p>
          )}
          <CheckList checks={inGroup(report, "provider")} />
        </>
      );
    case "master":
      return (
        <>
          <h2>Master agent</h2>
          <p className="setup-intro">
            Choose a supported provider and follow the host's commands to
            prepare the master.
          </p>
          {report && <MasterProviders report={report} />}
          <CheckList checks={inGroup(report, "master")} />
        </>
      );
    case "project":
      return (
        <>
          <h2>Projects</h2>
          <p className="setup-intro">
            Register a repository so the master has a project to work on.
          </p>
          {projectsLoading && (
            <p className="setup-inline-status" role="status">
              Checking projects…
            </p>
          )}
          {projectError && (
            <div className="setup-error" role="alert">
              <span>
                Could not refresh projects — {projectError}.{" "}
                {projects
                  ? "Showing the last known list."
                  : "Project readiness is unknown."}
              </span>
              <Button onClick={retryProjects} disabled={projectsLoading}>
                Retry projects
              </Button>
            </div>
          )}
          {projects !== null && (
            <div className="card setup-projects">
              {projects.length > 0 ? (
                <>
                  <div className="setup-check-heading">
                    <span>
                      {projects.length} registered project
                      {projects.length === 1 ? "" : "s"}
                    </span>
                    <Link
                      className="lnk"
                      href={routePath({
                        screen: "projects",
                        slug: null,
                        section: "overview",
                      })}
                    >
                      Open projects
                    </Link>
                  </div>
                  <ul className="setup-project-list">
                    {projects.map((project) => (
                      <li key={project}>
                        <Link
                          className="lnk"
                          href={routePath({
                            screen: "projects",
                            slug: project,
                            section: "overview",
                          })}
                        >
                          {project}
                        </Link>
                      </li>
                    ))}
                  </ul>
                  <p>Add another repository:</p>
                </>
              ) : (
                <>
                  <div className="setup-check-heading">
                    <span>No projects yet</span>
                  </div>
                  <p>
                    Run this command on the host with your project key and
                    repository path:
                  </p>
                </>
              )}
              <CopyFix command="cadence project new <key> --repo <path>" />
              <p className="setup-project-hint">
                Then choose Check again to refresh the project list.
              </p>
            </div>
          )}
        </>
      );
  }
}

function OnHost() {
  return (
    <main className="setup-page">
      <h1>Setup</h1>
      <div className="card setup-host" role="status">
        <h2>Continue on the host</h2>
        <p>
          Setup checks inspect the machine running Cadence. Open the board at{" "}
          <code className="num">127.0.0.1</code> on that machine, or run this
          command in its terminal.
        </p>
        <CopyFix command="cadence setup" />
      </div>
    </main>
  );
}

/** Metadata and the existing HTTP host-only proof remain authoritative. */
export default function Setup({
  settingsHref,
  readOnly,
}: {
  settingsHref: string;
  readOnly: boolean | null;
}) {
  if (readOnly === null)
    return (
      <main className="setup-page">
        <h1>Setup</h1>
        <p className="setup-inline-status" role="status">
          Checking access…
        </p>
      </main>
    );
  return readOnly ? <OnHost /> : <SetupWorkspace settingsHref={settingsHref} />;
}

function SetupWorkspace({ settingsHref }: { settingsHref: string }) {
  const state = useQuery(setupResource),
    projectsState = useQuery(resources.projects);
  const [step, setStep] = useState<StepId>("environment");
  const [checking, setChecking] = useState(false),
    [recheckError, setRecheckError] = useState<string | null>(null);
  const [reused, setReused] = useState<{ age: number; wait: number } | null>(
    null,
  );
  const selected = useRef(false),
    flight = useRef(false);
  const panelId = useId();
  const report = state.data;
  const projects = projectsState.data?.map((project) => project.key) ?? null;
  const index = STEPS.findIndex((section) => section.id === step);
  const missing = report ? missingRequired(report) : [];
  const busy = checking || state.inFlight || projectsState.inFlight;
  const stale = !!(state.error || recheckError || projectsState.error);
  const current = report !== null && projects !== null && !busy && !stale;
  const date = report ? new Date(report.checked_at) : null;
  const checkedAt =
    date && !Number.isNaN(date.getTime()) ? date.toLocaleTimeString() : null;
  const sectionReady = (id: StepId) =>
    readiness(id, report, projects?.length ?? null);
  useEffect(() => {
    if (
      selected.current ||
      !report ||
      projectsState.data === null ||
      busy ||
      stale
    )
      return;
    selected.current = true;
    setStep(
      STEPS.find(
        (section) =>
          readiness(section.id, report, projectsState.data!.length) !== "ready",
      )?.id ?? "environment",
    );
  }, [report, projectsState.data, busy, stale]);
  const choose = (id: StepId) => {
    selected.current = true;
    setStep(id);
  };
  const recheck = async () => {
    if (flight.current || busy) return;
    flight.current = true;
    setChecking(true);
    setRecheckError(null);
    setReused(null);
    // Project registration happens on the host; refresh that observation too.
    void resources.projects.invalidate();
    try {
      const result = await recheckSetup();
      setReused(
        result.ran_now
          ? null
          : { age: result.age_ms, wait: result.recheck_in_ms },
      );
    } catch (error) {
      setRecheckError(error instanceof Error ? error.message : String(error));
    } finally {
      flight.current = false;
      setChecking(false);
    }
  };
  if (state.error === SETUP_ON_HOST || recheckError === SETUP_ON_HOST)
    return <OnHost />;
  const summary = !report
    ? state.error
      ? "Setup checks unavailable"
      : "Checking setup…"
    : stale
      ? "Checks need refreshing"
      : busy
        ? "Checking readiness…"
        : projects === null
          ? "Project readiness is unknown"
          : missing.length > 0
            ? `${missing.length} required item${missing.length === 1 ? " needs" : "s need"} attention`
            : projects.length === 0
              ? "Add your first project"
              : "Required checks passed";
  const summaryTone: Tone = !current
    ? "muted"
    : missing.length > 0 || projects?.length === 0
      ? "warn"
      : "ok";
  return (
    <main className="setup-page">
      <header className="setup-header">
        <div>
          <h1>Setup</h1>
          <p>Check readiness, follow the host's commands, then check again.</p>
        </div>
        <div className="setup-actions">
          <Button
            icon={<IconRefresh />}
            loading={busy}
            onClick={() => void recheck()}
          >
            Check again
          </Button>
          <Button href={routePath({ screen: "home" })}>Go to Home</Button>
        </div>
      </header>
      <div className="setup-summary" role="status">
        <span className={`chip ${TONE[summaryTone]}`}>{summary}</span>
        {checkedAt && <span>Last checked {checkedAt}</span>}
        {current && (
          <span>
            {
              STEPS.filter((section) => sectionReady(section.id) === "ready")
                .length
            }{" "}
            of {STEPS.length} sections ready
          </span>
        )}
      </div>
      {reused && (
        <p className="setup-inline-status" role="status">
          Recent report reused ({Math.max(1, Math.ceil(reused.age / 1000))}s
          old). The host can check again after its{" "}
          {Math.max(1, Math.ceil(reused.wait / 1000))}s cooldown.
        </p>
      )}
      {stale && (
        <div className="setup-error" role="alert">
          <span>
            {report
              ? "Showing last known observations."
              : "Could not load readiness observations."}{" "}
            {recheckError ?? state.error ?? projectsState.error}
          </span>
        </div>
      )}
      <div className="setup-layout">
        <nav aria-label="Setup sections">
          <ol>
            {STEPS.map((section, position) => {
              const status = sectionReady(section.id);
              return (
                <li key={section.id}>
                  <button
                    type="button"
                    aria-current={step === section.id ? "step" : undefined}
                    aria-controls={panelId}
                    onClick={() => choose(section.id)}
                    className="setup-section-button"
                  >
                    <span className="setup-section-index num" aria-hidden>
                      {current && status === "ready" ? (
                        <IconCheck />
                      ) : (
                        position + 1
                      )}
                    </span>
                    <span className="setup-section-body">
                      <span className="setup-section-title">
                        {section.title}
                      </span>
                      <span className="setup-section-hint">{section.hint}</span>
                      <span
                        className="setup-section-status"
                        data-state={current ? status : "unknown"}
                      >
                        {busy
                          ? "Checking…"
                          : stale
                            ? "Last known"
                            : LABEL[status]}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ol>
          <p className="setup-detect-note">
            Checks only inspect the host. Commands run in your terminal.
          </p>
        </nav>
        <section id={panelId} className="setup-panel">
          {!report && (
            <div
              className="card setup-empty"
              role={state.error ? "alert" : "status"}
            >
              {state.error
                ? `Could not run setup checks — ${state.error}. Choose Check again to retry.`
                : "Checking this machine… Agent CLIs can take a few seconds."}
            </div>
          )}
          {(report || step === "project") && (
            <StepBody
              step={step}
              report={report}
              projects={projects}
              projectError={projectsState.error}
              projectsLoading={
                projectsState.inFlight ||
                (projectsState.data === null && !projectsState.error)
              }
              retryProjects={() => void resources.projects.invalidate()}
            />
          )}
          <footer className="setup-footer">
            <Button
              disabled={index === 0}
              onClick={() => choose(STEPS[index - 1].id)}
            >
              Back
            </Button>
            {index < STEPS.length - 1 ? (
              <Button
                onClick={() => choose(STEPS[index + 1].id)}
              >{`Next: ${STEPS[index + 1].title}`}</Button>
            ) : (
              <Button variant="primary" href={routePath({ screen: "home" })}>
                Go to Home
              </Button>
            )}
          </footer>
          <p className="setup-settings-note">
            Provider and model defaults are in{" "}
            <Link className="lnk" href={settingsHref}>
              Settings
            </Link>
            .
          </p>
        </section>
      </div>
    </main>
  );
}
