import { useState } from "react";
import Link from "../../ui/Link";
import { StaleChip, ResourceGate } from "../../ui/ResourceStatus";
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
  type SetupCheck,
  type SetupReport,
  type Tone,
} from "./checks";
import { recheckSetup, setupResource, SETUP_ON_HOST } from "./setupApi";

/**
 * First-run setup (CAD-327, MVP). Shows `cadence setup`'s checks as the
 * board ran them — detect only: nothing here installs, signs in, starts
 * or writes. Each check that needs work carries the command to run;
 * "Re-check" runs the probes again. Platforms and import come later.
 */

type StepId = "environment" | "agents" | "master" | "project" | "home";

const STEPS: { id: StepId; title: string; hint: string }[] = [
  { id: "environment", title: "Environment", hint: "state, tracker, daemon" },
  { id: "agents", title: "Agent CLIs", hint: "installed and signed in" },
  { id: "master", title: "Master agent", hint: "plans and delegates" },
  { id: "project", title: "First project", hint: "a repo to work on" },
  { id: "home", title: "Go to Home", hint: "ask for work" },
];

const LATER = [
  { title: "Platforms", hint: "managed services" },
  { title: "Import", hint: "restore, tracker, repos" },
];

const TONE: Record<Tone, string> = {
  ok: "bg-ok/15 text-ok",
  warn: "bg-warn/10 text-warn",
  fail: "bg-fail/10 text-fail",
  muted: "bg-ink-800 text-ink-400",
};

function stepDone(id: StepId, report: SetupReport | null, projectCount: number): boolean {
  if (!report) return false;
  switch (id) {
    case "environment":
      return inGroup(report, "environment")
        .filter((c) => ["state_dir", "tracker", "daemon", "ui"].includes(c.check))
        .every(isReady);
    case "agents":
      return inGroup(report, "provider").some(isReady);
    case "master":
      return inGroup(report, "master").every(isReady);
    case "project":
      return projectCount > 0;
    case "home":
      return missingRequired(report).length === 0 && projectCount > 0;
  }
}

function CopyFix({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // No clipboard (plain http off loopback): the command stays selectable.
    }
  };
  return (
    <div className="mt-2 flex items-start gap-2 min-w-0">
      <code className="num min-w-0 flex-1 rounded border border-ink-700 bg-ink-900 px-2 py-1.5 text-label text-ink-200 break-all select-all">
        {command}
      </code>
      <button
        type="button"
        onClick={() => void copy()}
        className="chip shrink-0 py-1 bg-ink-800 text-ink-300 hover:text-accent transition-colors"
        aria-label={`copy: ${command}`}
      >
        {copied ? "copied" : "copy"}
      </button>
    </div>
  );
}

function CheckRow({ c }: { c: SetupCheck }) {
  const chip = statusChip(c);
  return (
    <li className="px-4 py-3 border-t border-ink-700 first:border-t-0 min-w-0">
      <div className="flex items-start gap-3 min-w-0">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-baseline gap-x-2">
            <span className="text-body font-medium text-ink-100">{checkLabel(c.check)}</span>
            <span className="num text-micro text-ink-500">{c.check}</span>
          </div>
          <p className="text-secondary text-ink-400 mt-0.5 break-words [overflow-wrap:anywhere]">
            {c.detail}
          </p>
        </div>
        <span className={`chip shrink-0 ${TONE[chip.tone]}`}>{chip.label}</span>
      </div>
      {!isReady(c) && c.fix && <CopyFix command={c.fix} />}
    </li>
  );
}

function CheckList({ checks }: { checks: SetupCheck[] }) {
  return (
    <ul className="card mt-4 overflow-hidden" aria-live="polite">
      {checks.map((c) => (
        <CheckRow key={c.check} c={c} />
      ))}
    </ul>
  );
}

/**
 * The master step's provider choice (CAD-448): every CLI `master start`
 * accepts, each ready one with its exact start command — a not-ready
 * one shows its own install/sign-in fix instead, and an `--unconfined`
 * command carries its risk warning. Hidden once the master's files
 * exist (`master start` already ran or planted them), while the
 * `master` check waits on its own prerequisites, and on a board built
 * before CAD-448. Detect only: the command is the operator's to paste,
 * never run from the board.
 */
function MasterProviders({ report }: { report: SetupReport }) {
  const offers = report.master?.providers ?? [];
  const files = report.checks.find((c) => c.check === "master");
  if (offers.length === 0 || (files && isReady(files)) || masterNeedsUnmet(report)) return null;
  return (
    <div className="card mt-4 px-4 py-3">
      <p className="text-secondary text-ink-400">
        <span className="font-medium text-ink-200">Choose the CLI it runs on.</span> Its command
        installs the agent files and starts the master.
      </p>
      <ul className="mt-3 space-y-3">
        {offers.map((o) => {
          const provider = report.checks.find((c) => c.check === o.bin);
          const chip = provider
            ? statusChip(provider)
            : { label: "not found", tone: "muted" as Tone };
          const command = o.start ?? (!o.ready ? provider?.fix : null) ?? null;
          return (
            <li key={o.bin} className="min-w-0">
              <div className="flex items-baseline gap-2">
                <span className="text-body font-medium text-ink-100">{checkLabel(o.bin)}</span>
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
}: {
  step: StepId;
  report: SetupReport | null;
  projects: string[];
}) {
  switch (step) {
    case "environment":
      return (
        <>
          <h2 className="text-section font-semibold text-ink-100">Environment</h2>
          <p className="text-body text-ink-400 mt-1 max-w-[68ch]">
            Where Cadence keeps its state, the tracker that holds plans and issues, and the daemon
            that runs agents.
          </p>
          <CheckList checks={inGroup(report, "environment")} />
          <div className="card mt-3 px-4 py-3">
            <p className="text-secondary text-ink-400">
              <span className="font-medium text-ink-200">One command for all of these:</span>{" "}
              <span className="num">cadence setup</span> creates what is missing and never
              re-initialises what exists.
            </p>
            <CopyFix command="cadence setup" />
          </div>
        </>
      );
    case "agents":
      return (
        <>
          <h2 className="text-section font-semibold text-ink-100">Agent CLIs</h2>
          <p className="text-body text-ink-400 mt-1 max-w-[68ch]">
            The coding agents already on this machine, with their version and whether each is
            signed in. A signed-in one that can run the master is offered in the next step; the
            others join as workers. Sign-in is read from an exit code or whether a credentials
            file exists — never its contents.
          </p>
          <CheckList checks={inGroup(report, "provider")} />
        </>
      );
    case "master":
      return (
        <>
          <h2 className="text-section font-semibold text-ink-100">Master agent</h2>
          <p className="text-body text-ink-400 mt-1 max-w-[68ch]">
            The master plans, creates projects and hands work to the team. Pick the provider CLI
            it runs on — its command installs the agent files and starts it. The master signs in
            with its own login, separate from yours.
          </p>
          {report && <MasterProviders report={report} />}
          <CheckList checks={inGroup(report, "master")} />
        </>
      );
    case "project":
      return (
        <>
          <h2 className="text-section font-semibold text-ink-100">First project</h2>
          <p className="text-body text-ink-400 mt-1 max-w-[68ch]">
            Point Cadence at a repository to work on. Registering a repo seeds its project with a
            goal, staffing and stages.
          </p>
          <div className="card mt-4 px-4 py-3">
            {projects.length > 0 ? (
              <>
                <div className="flex items-center gap-2">
                  <span className="text-body font-medium text-ink-100">
                    {projects.length} project{projects.length === 1 ? "" : "s"} in the tracker
                  </span>
                  <span className={`chip ml-auto ${TONE.ok}`}>ready</span>
                </div>
                <p className="num text-label text-ink-400 mt-1 break-words">
                  {projects.slice(0, 8).join(" · ")}
                  {projects.length > 8 ? " …" : ""}
                </p>
                <p className="text-secondary text-ink-500 mt-2">Another one:</p>
              </>
            ) : (
              <div className="flex items-center gap-2">
                <span className="text-body font-medium text-ink-100">No project yet</span>
                <span className={`chip ml-auto ${TONE.warn}`}>missing</span>
              </div>
            )}
            <CopyFix command="cadence project new <key> --repo <path>" />
            <p className="text-label text-ink-500 mt-2">
              <span className="num">project new</span> is being built (CAD-358); older builds do
              not have it yet.
            </p>
          </div>
        </>
      );
    case "home":
      return null;
  }
}

/** Shown instead of the wizard to a read-only or tailnet viewer. */
function OnHost() {
  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 max-w-[62rem] w-full min-w-0">
      <h1 className="text-section font-semibold text-ink-100">Setup</h1>
      <div className="card mt-4 px-4 py-4 max-w-[68ch]" role="status">
        <p className="text-body font-medium text-ink-100">Setup runs on the host.</p>
        <p className="text-secondary text-ink-400 mt-1">
          Its checks read the machine the board runs on, so they are only shown to the operator
          there — open the board on <span className="num">127.0.0.1</span> on that machine, or run{" "}
          <span className="num">cadence setup</span> in its terminal.
        </p>
      </div>
    </main>
  );
}

function secs(ms: number): string {
  return `${Math.max(1, Math.ceil(ms / 1000))}s`;
}

export default function Setup({
  settingsHref,
  readOnly,
}: {
  settingsHref: string;
  /** null until `/api/meta` answers — nothing is fetched before then. */
  readOnly: boolean | null;
}) {
  if (readOnly === null) return null;
  return readOnly ? <OnHost /> : <SetupWizard settingsHref={settingsHref} />;
}

function SetupWizard({ settingsHref }: { settingsHref: string }) {
  const state = useQuery(setupResource);
  const projectsState = useQuery(resources.projects);
  const [step, setStep] = useState<StepId>("environment");
  const [checking, setChecking] = useState(false);
  const [recheckError, setRecheckError] = useState<string | null>(null);
  /** Set when a re-check was answered from the last run (under 5 s old). */
  const [reused, setReused] = useState<{ age: number; wait: number } | null>(null);
  const report = state.data;
  const projects = (projectsState.data ?? []).map((p) => p.key);
  const index = STEPS.findIndex((s) => s.id === step);
  const missing = report ? missingRequired(report) : [];
  const checkedAt = report ? new Date(report.checked_at).toLocaleTimeString() : null;

  const recheck = async () => {
    setChecking(true);
    setRecheckError(null);
    try {
      const r = await recheckSetup();
      setReused(r.ran_now ? null : { age: r.age_ms, wait: r.recheck_in_ms });
    } catch (e) {
      setRecheckError(e instanceof Error ? e.message : String(e));
    } finally {
      setChecking(false);
    }
  };

  if (state.error === SETUP_ON_HOST || recheckError === SETUP_ON_HOST) return <OnHost />;

  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 max-w-[62rem] w-full min-w-0">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <h1 className="text-section font-semibold text-ink-100">Setup</h1>
        <StaleChip state={state} />
        <div className="ml-auto flex items-center gap-2">
          {checkedAt && <span className="text-label text-ink-500">checked {checkedAt}</span>}
          <button
            type="button"
            onClick={() => void recheck()}
            disabled={checking || state.inFlight}
            className="chip py-1 bg-ink-800 text-ink-300 hover:text-accent disabled:opacity-60 transition-colors"
          >
            {checking || state.inFlight ? "checking…" : "re-check"}
          </button>
        </div>
      </div>
      {reused && (
        <p className="text-label text-ink-500 mt-1" role="status">
          checked {secs(reused.age)} ago — re-check available in {secs(reused.wait)}
        </p>
      )}
      {recheckError && (
        <p className="text-label text-fail mt-1" role="alert">
          re-check failed — {recheckError}
        </p>
      )}
      <p className="text-body text-ink-400 mt-2 max-w-[68ch]">
        These checks only look — nothing is installed, signed in or started for you. Each item
        that needs work shows the command to run; run it, then re-check.
      </p>

      <div className="mt-5 lg:grid lg:grid-cols-[13rem_minmax(0,1fr)] lg:gap-8">
        <nav aria-label="Setup steps" className="mb-5 lg:mb-0">
          <ol className="flex flex-wrap gap-2 lg:flex-col lg:gap-1">
            {STEPS.map((s, i) => {
              const done = stepDone(s.id, report, projects.length);
              const current = s.id === step;
              return (
                <li key={s.id}>
                  <button
                    type="button"
                    onClick={() => setStep(s.id)}
                    aria-current={current ? "step" : undefined}
                    className={`flex items-center gap-2 rounded px-2 py-1.5 text-left w-full transition-colors ${
                      current ? "bg-accent/10 text-accent" : "text-ink-400 hover:bg-ink-800"
                    }`}
                  >
                    <span
                      className={`num inline-flex size-5 shrink-0 items-center justify-center rounded-full text-micro ${
                        done ? "bg-ok/15 text-ok" : "bg-ink-800 text-ink-400"
                      }`}
                      aria-hidden="true"
                    >
                      {done ? "✓" : i + 1}
                    </span>
                    <span className="min-w-0">
                      <span className="block text-secondary font-medium">{s.title}</span>
                      <span className="hidden lg:block text-micro text-ink-500">{s.hint}</span>
                    </span>
                  </button>
                </li>
              );
            })}
            {LATER.map((s) => (
              <li key={s.title}>
                <span
                  className="flex items-center gap-2 rounded px-2 py-1.5 text-ink-500"
                  aria-disabled="true"
                  title={`${s.title} — ${s.hint}: later`}
                >
                  <span className="text-secondary">{s.title}</span>
                  <span className="chip bg-ink-800 text-ink-500">later</span>
                </span>
              </li>
            ))}
          </ol>
        </nav>

        <section className="min-w-0">
          <ResourceGate
            state={state}
            loading="checking this machine… (agent CLIs can take a few seconds)"
            failed="could not run the setup checks"
            onRetry={() => void recheck()}
          />
          {step === "home" ? (
            <>
              <h2 className="text-section font-semibold text-ink-100">Go to Home</h2>
              <p className="text-body text-ink-400 mt-1 max-w-[68ch]">
                {missing.length === 0
                  ? "Every required check passes. Ask the master for work from Home."
                  : `Still to do: ${missing.map(checkLabel).join(", ")}. You can go to Home now and finish later — Home links back here until they pass.`}
              </p>
              <p className="text-secondary text-ink-500 mt-2">
                Agents and model defaults are under{" "}
                <Link href={settingsHref} className="lnk">
                  Settings
                </Link>
                .
              </p>
              <Link
                href={routePath({ screen: "home" })}
                className="mt-4 inline-flex items-center rounded bg-accent px-3 py-2 text-body font-medium text-on-accent hover:opacity-90"
              >
                Go to Home →
              </Link>
            </>
          ) : (
            (report || step === "project") && (
              <StepBody step={step} report={report} projects={projects} />
            )
          )}
          <div className="mt-6 flex items-center justify-between border-t border-ink-700 pt-4">
            <button
              type="button"
              onClick={() => setStep(STEPS[Math.max(0, index - 1)].id)}
              disabled={index === 0}
              className="chip py-1 bg-ink-800 text-ink-300 disabled:opacity-40"
            >
              ← back
            </button>
            {index < STEPS.length - 1 && (
              <button
                type="button"
                onClick={() => setStep(STEPS[index + 1].id)}
                className="chip py-1 bg-accent/10 text-accent hover:bg-accent/20"
              >
                continue →
              </button>
            )}
          </div>
        </section>
      </div>
    </main>
  );
}
