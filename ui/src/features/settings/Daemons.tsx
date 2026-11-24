import { useCallback, useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type {
  HostedLocalAgentActivity,
  HostedLocalDaemonAgent,
  HostedLocalDaemonRunner,
  HostedLocalDaemonsResponse,
} from "../../lib/types";
import Button from "../../ui/Button";

function statusTone(status: HostedLocalAgentActivity["status"], presence: "online" | "offline"): string {
  if (presence === "offline" || status === "offline") return "bg-ink-700/60 text-ink-400";
  if (status === "busy") return "bg-accent/15 text-accent";
  if (status === "blocked") return "bg-warn/15 text-warn";
  return "bg-ok/15 text-ok";
}

function registrationLabel(state: HostedLocalDaemonRunner["registration_state"]): string {
  if (state === "current") return "Credential current";
  if (state === "expired") return "Credential expired — reconnect on host";
  return "Credential revoked — reconnect on host";
}

function AgentRow({
  agent,
  onOpenIssue,
}: {
  agent: HostedLocalDaemonAgent;
  onOpenIssue: (id: string) => void;
}) {
  const t = useTranslate();
  const status = agent.presence === "offline" ? "offline" : agent.activity.status;
  return (
    <div className="settings-row items-start">
      <div className="settings-row-text min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <b>{agent.label}</b>
          <span className="chip text-micro bg-ink-800 text-ink-300 num">{agent.adapter}</span>
          {agent.default_assignment_alias && (
            <span className="chip text-micro bg-info/15 text-info">{t("Default seat")}</span>
          )}
          <span className={`chip text-micro ${statusTone(agent.activity.status, agent.presence)}`}>
            {t(status)}
          </span>
        </div>
        <small className="block mt-1">
          {agent.activity.summary ? (
            <span>{agent.activity.summary}</span>
          ) : (
            <span className="text-ink-500">
              {status === "idle" ? t("Ready for queued ticket work.") : t("No active task reported.")}
            </span>
          )}
          {agent.activity.issue_id && (
            <>
              {" · "}
              <button
                type="button"
                className="lnk num inline"
                onClick={() => onOpenIssue(agent.activity.issue_id!)}
              >
                {agent.activity.issue_id}
              </button>
            </>
          )}
          {agent.activity.updated_at && (
            <span className="text-ink-500 num"> · {agent.activity.updated_at}</span>
          )}
        </small>
      </div>
    </div>
  );
}

/**
 * CAD-2746: Owner-only `/settings/daemons` directory view.
 * Intentionally observational — ticket dispatch lives on Issue Detail.
 */
export default function Daemons({
  onOpenIssue,
  orgSlug,
}: {
  onOpenIssue: (id: string) => void;
  orgSlug?: string | null;
}) {
  const t = useTranslate();
  const [data, setData] = useState<HostedLocalDaemonsResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showConnect, setShowConnect] = useState(false);

  const load = useCallback((signal?: AbortSignal) => {
    setLoading(true);
    setError(null);
    api
      .localDaemons(signal)
      .then((res) => {
        if (!signal?.aborted) setData(res);
      })
      .catch((cause: unknown) => {
        if (signal?.aborted) return;
        const err = cause as ApiError;
        setError(err.message || "Could not load connected daemons.");
      })
      .finally(() => {
        if (!signal?.aborted) setLoading(false);
      });
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    load(controller.signal);
    return () => controller.abort();
  }, [load]);

  const orgFlag = orgSlug ? `--org ${orgSlug} ` : "--org <org> ";
  const runners = data?.runners ?? [];

  return (
    <main className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="daemons-title">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 id="daemons-title" className="text-section font-semibold text-ink-100">
            {t("Daemons")}
          </h1>
          <p className="mt-1 mb-4 text-secondary text-ink-400">
            {t("Connected local Cadence daemons, seats, spawn capacity, and live agent status.")}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button variant="secondary" disabled={loading} onClick={() => load()}>
            {loading ? t("Refreshing…") : t("Refresh")}
          </Button>
          <Button variant="primary" onClick={() => setShowConnect((open) => !open)}>
            {showConnect ? t("Hide instructions") : t("+ Connect daemon")}
          </Button>
        </div>
      </div>

      {showConnect && (
        <section className="card p-4 mb-4 grid gap-2.5 bg-ink-875" aria-label={t("Connect daemon")}>
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            {t("Connect a local daemon")}
          </h2>
          <p className="text-secondary text-ink-300 m-0">
            {t("Standing seat pickup:")}
          </p>
          <pre className="p-2.5 rounded bg-ink-900 border border-ink-700 text-micro num overflow-x-auto m-0">
            {`cadence ${orgFlag}agent connect --alias <seat> --repo <path>`}
          </pre>
          <p className="text-secondary text-ink-300 m-0">
            {t("With opt-in isolated worker spawning for hosted Issue Dispatch:")}
          </p>
          <pre className="p-2.5 rounded bg-ink-900 border border-ink-700 text-micro num overflow-x-auto m-0">
            {`cadence ${orgFlag}agent connect --alias <seat> --allow-spawn --spawn-repo <name>=<path> --harnesses omp,pi,claude,codex --max-concurrent 2`}
          </pre>
        </section>
      )}

      {error && (
        <div className="card p-4 mb-4 border-l-[3px] border-l-fail bg-fail/10 flex items-center justify-between gap-3" role="alert">
          <span className="text-secondary text-ink-100">{error}</span>
          <Button variant="secondary" onClick={() => load()}>{t("Retry")}</Button>
        </div>
      )}

      {!loading && !error && runners.length === 0 && (
        <div className="card p-5 text-secondary text-ink-300">
          {t("No local daemons are registered in this workspace yet. Run + Connect daemon to link a workstation.")}
        </div>
      )}

      <div className="grid gap-4">
        {runners.map((runner) => {
          const online = runner.presence === "online" && runner.registration_state === "current";
          const { spawn_manifest: manifest } = runner;
          return (
            <section
              key={runner.runner_id}
              className="card p-4 grid gap-3 bg-ink-875"
              aria-label={runner.host_label ?? runner.runner_id}
            >
              <header className="flex flex-wrap items-center justify-between gap-2 border-b border-ink-700 pb-3">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
                      {runner.host_label ?? runner.runner_id}
                    </h2>
                    <span className={`chip text-micro ${online ? "bg-ok/15 text-ok" : "bg-ink-700/60 text-ink-400"}`}>
                      {online ? t("Online") : t("Offline")}
                    </span>
                    {runner.platform && (
                      <span className="chip text-micro bg-ink-800 text-ink-300 num">
                        {runner.platform}
                      </span>
                    )}
                  </div>
                  <p className="text-micro text-ink-500 num m-0 mt-1">
                    {runner.runner_id}
                    {runner.registration_state !== "current" && ` · ${registrationLabel(runner.registration_state)}`}
                  </p>
                </div>
                <div className="text-right text-micro text-ink-300">
                  {manifest.spawn_allowed ? (
                    <span className="chip bg-info/15 text-info num">
                      {`Spawn enabled · ${manifest.active_lanes}/${manifest.max_concurrent} lanes`}
                    </span>
                  ) : (
                    <span className="chip bg-ink-800 text-ink-400">
                      {t("Existing seats only")}
                    </span>
                  )}
                </div>
              </header>

              {manifest.spawn_allowed && (
                <div className="flex flex-wrap gap-3 text-micro text-ink-400">
                  <span>
                    <strong className="text-ink-200">{t("Harnesses:")}</strong>{" "}
                    <span className="num">{manifest.harnesses.join(", ") || "—"}</span>
                  </span>
                  <span>
                    <strong className="text-ink-200">{t("Repos:")}</strong>{" "}
                    <span className="num">{manifest.repos.join(", ") || "—"}</span>
                  </span>
                </div>
              )}

              <div className="settings-rows">
                {runner.agents.map((agent) => (
                  <AgentRow key={agent.agent_id} agent={agent} onOpenIssue={onOpenIssue} />
                ))}
              </div>
            </section>
          );
        })}
      </div>
    </main>
  );
}
