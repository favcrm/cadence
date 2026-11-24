import { useEffect, useMemo, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type {
  HostedLocalDaemonRunner,
  HostedLocalDispatchRequest,
  HostedLocalHarness,
} from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import {
  describeSpawnDisabledReason,
  onlineCurrentSeats,
  selectDefaultHostedDispatch,
  spawnableRunners,
} from "./hostedDispatchModel";

const EFFORTS = ["low", "medium", "high", "xhigh", "max"] as const;

export default function HostedDispatchDialog({
  id,
  title,
  daemonsHref,
  onClose,
  onDispatched,
  onToast,
}: {
  id: string;
  title: string;
  daemonsHref?: string;
  onClose: () => void;
  onDispatched: () => void;
  onToast: (text: string) => void;
}) {
  const t = useTranslate();
  const [runners, setRunners] = useState<HostedLocalDaemonRunner[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<"seat" | "spawn">("seat");
  const [targetAgentId, setTargetAgentId] = useState("");
  const [runnerId, setRunnerId] = useState("");
  const [provider, setProvider] = useState<HostedLocalHarness>("omp");
  const [repo, setRepo] = useState("");
  const [effort, setEffort] = useState<"" | (typeof EFFORTS)[number]>("");
  const [model, setModel] = useState("");
  const [note, setNote] = useState("");
  const [reasonNote, setReasonNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [idempotencyKey, setIdempotencyKey] = useState(() => crypto.randomUUID());

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    const controller = new AbortController();
    api
      .localDaemons(controller.signal)
      .then((res) => {
        if (controller.signal.aborted) return;
        setRunners(res.runners);
        const picked = selectDefaultHostedDispatch(res.runners, id);
        if (picked.mode === "spawn") {
          setMode("spawn");
          setRunnerId(picked.runnerId);
          setProvider(picked.provider);
          setRepo(picked.repo);
          setReasonNote(null);
        } else if (picked.mode === "seat") {
          setMode("seat");
          setTargetAgentId(picked.targetAgentId);
          setRunnerId(picked.runnerId);
          setReasonNote(
            picked.reason === "warm_lane"
              ? "Warm lane detected for this issue — continuing on existing seat."
              : picked.spawnDisabledReason,
          );
        } else {
          setReasonNote(picked.spawnDisabledReason);
        }
      })
      .catch((cause: unknown) => {
        if (controller.signal.aborted) return;
        const err = cause as ApiError;
        setLoadError(err.message || "Could not load connected daemons.");
      });
    return () => controller.abort();
  }, [id]);

  const seats = useMemo(() => (runners ? onlineCurrentSeats(runners) : []), [runners]);
  const spawnHosts = useMemo(() => (runners ? spawnableRunners(runners) : []), [runners]);
  const spawnDisabledReason = useMemo(
    () => (runners ? describeSpawnDisabledReason(runners) : null),
    [runners],
  );
  const activeSpawnHost = useMemo(
    () => spawnHosts.find((r) => r.runner_id === runnerId) ?? spawnHosts[0] ?? null,
    [spawnHosts, runnerId],
  );

  // Reset idempotency key whenever payload intent changes so edited parameters are not conflated.
  const resetKey = () => setIdempotencyKey(crypto.randomUUID());

  const canSubmit =
    !busy &&
    !loadError &&
    runners !== null &&
    (mode === "seat"
      ? seats.some((s) => s.agent.agent_id === targetAgentId)
      : activeSpawnHost !== null && Boolean(repo));

  const submit = () => {
    if (!canSubmit) return;
    const trimmedNote = note.trim();
    const request: HostedLocalDispatchRequest =
      mode === "seat"
        ? {
            idempotency_key: idempotencyKey,
            mode: "seat",
            target_agent_id: targetAgentId,
            ...(trimmedNote ? { note: trimmedNote } : {}),
          }
        : {
            idempotency_key: idempotencyKey,
            mode: "spawn",
            spawn: {
              runner_id: activeSpawnHost!.runner_id,
              provider,
              repo,
              ...(model.trim() ? { model: model.trim() } : {}),
              ...(effort ? { effort } : {}),
            },
            ...(trimmedNote ? { note: trimmedNote } : {}),
          };

    setBusy(true);
    setSubmitError(null);
    api
      .localDispatch(id, request)
      .then((resp) => {
        onDispatched();
        onToast(
          resp.replayed
            ? `${id} dispatch already queued (${resp.assignment.assignment_id})`
            : `Dispatched ${id} to connected daemon`,
        );
        onClose();
      })
      .catch((cause: unknown) => {
        const err = cause as ApiError;
        setSubmitError(err.message || "Local daemon dispatch failed.");
      })
      .finally(() => setBusy(false));
  };

  return (
    <div className="fixed inset-0 z-40 bg-scrim grid place-items-center p-4" onClick={onClose}>
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="hosted-dispatch-title"
        className="card w-full max-w-[640px] max-h-[min(840px,calc(100vh-48px))] flex flex-col bg-ink-875"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-baseline justify-between gap-3 px-4 py-3 border-b border-ink-700">
          <h2 id="hosted-dispatch-title" className="text-cardtitle font-semibold text-ink-100">
            {t("Dispatch to connected daemon")}
          </h2>
          <span className="kicker num">{id}</span>
        </header>

        <div className="overflow-auto px-4 py-4 grid gap-3">
          <p className="text-secondary text-ink-300 m-0 font-medium">{title}</p>

          {runners === null && !loadError && (
            <p className="text-secondary text-ink-400 m-0">{t("Loading connected daemons…")}</p>
          )}

          {loadError && (
            <p className="text-secondary text-fail m-0" role="alert">{loadError}</p>
          )}

          {runners !== null && !loadError && (
            <>
              <div className="grid gap-1.5">
                <span className="slabel">{t("Target mode")}</span>
                <div className="flex gap-1" role="group" aria-label={t("Target mode")}>
                  <Button
                    className="flex-1"
                    variant={mode === "spawn" ? "primary" : "secondary"}
                    disabled={spawnHosts.length === 0}
                    title={spawnDisabledReason ?? undefined}
                    onClick={() => {
                      const host = spawnHosts[0];
                      if (!host) return;
                      setMode("spawn");
                      setRunnerId(host.runner_id);
                      setProvider(host.spawn_manifest.harnesses.includes("omp") ? "omp" : host.spawn_manifest.harnesses[0]!);
                      setRepo(host.spawn_manifest.repos[0]!);
                      resetKey();
                    }}
                  >
                    {t("New isolated worker")}
                  </Button>
                  <Button
                    className="flex-1"
                    variant={mode === "seat" ? "primary" : "secondary"}
                    disabled={seats.length === 0}
                    onClick={() => {
                      setMode("seat");
                      if (!targetAgentId && seats[0]) setTargetAgentId(seats[0].agent.agent_id);
                      resetKey();
                    }}
                  >
                    {t("Continue existing seat")}
                  </Button>
                </div>
                {reasonNote && (
                  <p className="text-micro text-ink-400 m-0">{reasonNote}</p>
                )}
              </div>

              {mode === "seat" ? (
                <div className="grid gap-1.5">
                  <span className="slabel">{t("Connected seat")}</span>
                  {seats.length === 0 ? (
                    <p className="text-secondary text-ink-400 m-0">
                      {t("No online local seat is currently connected.")}{" "}
                      {daemonsHref && <Link href={daemonsHref} className="lnk">{t("View Daemons")}</Link>}
                    </p>
                  ) : (
                    <Select
                      full
                      value={targetAgentId}
                      aria-label={t("Connected seat")}
                      options={seats.map(({ runner, agent }) => ({
                        value: agent.agent_id,
                        label: `${agent.label} (${runner.host_label ?? runner.runner_id.slice(0, 12)})`,
                        badge: [agent.adapter, agent.activity.issue_id ?? agent.activity.status].join(" · "),
                      }))}
                      onChange={(val) => {
                        setTargetAgentId(val);
                        resetKey();
                      }}
                    />
                  )}
                </div>
              ) : (
                activeSpawnHost && (
                  <div className="grid gap-2.5">
                    <label className="grid gap-1.5">
                      <span className="slabel">{t("Daemon host")}</span>
                      <Select
                        full
                        value={activeSpawnHost.runner_id}
                        aria-label={t("Daemon host")}
                        options={spawnHosts.map((host) => ({
                          value: host.runner_id,
                          label: host.host_label ?? host.runner_id,
                          badge: `${host.spawn_manifest.active_lanes}/${host.spawn_manifest.max_concurrent} lanes`,
                        }))}
                        onChange={(nextRunnerId) => {
                          const next = spawnHosts.find((r) => r.runner_id === nextRunnerId);
                          if (!next) return;
                          setRunnerId(next.runner_id);
                          if (!next.spawn_manifest.harnesses.includes(provider)) {
                            setProvider(next.spawn_manifest.harnesses[0]!);
                          }
                          if (!next.spawn_manifest.repos.includes(repo)) {
                            setRepo(next.spawn_manifest.repos[0]!);
                          }
                          resetKey();
                        }}
                      />
                    </label>

                    <div className="grid sm:grid-cols-2 gap-2.5">
                      <label className="grid gap-1.5">
                        <span className="slabel">{t("Harness")}</span>
                        <Select
                          full
                          value={provider}
                          aria-label={t("Harness")}
                          options={activeSpawnHost.spawn_manifest.harnesses.map((h) => ({
                            value: h,
                            label: h,
                          }))}
                          onChange={(val) => {
                            setProvider(val as HostedLocalHarness);
                            resetKey();
                          }}
                        />
                      </label>
                      <label className="grid gap-1.5">
                        <span className="slabel">{t("Repository")}</span>
                        <Select
                          full
                          value={repo}
                          aria-label={t("Repository")}
                          options={activeSpawnHost.spawn_manifest.repos.map((r) => ({
                            value: r,
                            label: r,
                          }))}
                          onChange={(val) => {
                            setRepo(val);
                            resetKey();
                          }}
                        />
                      </label>
                    </div>

                    <div className="grid sm:grid-cols-2 gap-2.5">
                      <label className="grid gap-1.5">
                        <span className="slabel">{t("Effort (optional)")}</span>
                        <Select
                          full
                          value={effort}
                          aria-label={t("Effort")}
                          options={[
                            { value: "", label: "Default" },
                            ...EFFORTS.map((e) => ({ value: e, label: e })),
                          ]}
                          onChange={(val) => {
                            setEffort(val as "" | (typeof EFFORTS)[number]);
                            resetKey();
                          }}
                        />
                      </label>
                      <label className="grid gap-1.5">
                        <span className="slabel">{t("Model override (optional)")}</span>
                        <input
                          type="text"
                          className="input w-full num"
                          placeholder="inherit"
                          value={model}
                          onChange={(e) => {
                            setModel(e.target.value);
                            resetKey();
                          }}
                        />
                      </label>
                    </div>
                  </div>
                )
              )}

              <label className="grid gap-1.5">
                <span className="slabel">{t("Dispatch note (optional)")}</span>
                <textarea
                  rows={2}
                  className="input w-full"
                  placeholder={t("Guidance for the local worker…")}
                  value={note}
                  onChange={(e) => {
                    setNote(e.target.value);
                    resetKey();
                  }}
                />
              </label>
            </>
          )}

          {submitError && (
            <p className="text-secondary text-fail m-0" role="alert">{submitError}</p>
          )}
        </div>

        <footer className="flex items-center justify-between gap-2 px-4 py-3 border-t border-ink-700">
          <div>
            {daemonsHref && (
              <Link href={daemonsHref} className="lnk text-secondary" onClick={onClose}>
                {t("Manage daemons")}
              </Link>
            )}
          </div>
          <div className="flex items-center gap-2">
            <Button variant="secondary" onClick={onClose} disabled={busy}>
              {t("Cancel")}
            </Button>
            <Button variant="primary" onClick={submit} disabled={!canSubmit}>
              {busy ? t("Dispatching…") : t("Dispatch")}
            </Button>
          </div>
        </footer>
      </div>
    </div>
  );
}
