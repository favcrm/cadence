import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { fmtTime } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import { fmtInZone, sameDayClock, useTimeZones } from "../../lib/timeZone";
import type {
  AgentEndpoint,
  Connection,
  Routine,
  RoutineRun,
  StagedRoutineProposal,
} from "../../lib/types";
import Button from "../../ui/Button";
import DrawerShell, { type DrawerMenuItem } from "../app-shell/shared/DrawerShell";
import type { TabContext } from "./AgentTabs";
import { IncomingEndpointsSection, formatEndpointEmail } from "./IncomingEndpointsSection";
import { routinesApi } from "./routinesApi";
import "./routines.css";

type FrequencyPreset = "daily" | "weekdays" | "weekly" | "hourly" | "custom";

const PRESET_CRON: Record<Exclude<FrequencyPreset, "hourly" | "custom">, string> = {
  daily: "0 9 * * *",
  weekdays: "0 9 * * 1-5",
  weekly: "0 9 * * 1",
};

function statusMeta(r: Routine): { label: string; tone: "ok" | "warn" | "ink" } {
  switch (r.status) {
    case "on":
      return { label: "On", tone: "ok" };
    case "waiting_approval":
      return { label: "Waiting for you", tone: "warn" };
    case "repoint_required":
      return { label: "Choose thread", tone: "warn" };
    default:
      return { label: "Paused", tone: "ink" };
  }
}

/** A preset's text with its 09:00 replaced by the viewer's clock, when the run keeps its day. */
function presetSummary(r: Routine, text: string, tz: string, viewer: string): string {
  if (viewer !== tz && r.next_due_at) {
    const clock = sameDayClock(r.next_due_at, tz, viewer);
    if (clock) return `${text.replace("09:00", clock.time)} ${clock.label}`;
  }
  return `${text} (${tz})`;
}

/** The schedule in the viewer's zone, labelled; a stored zone is never rewritten. */
function scheduleSummary(r: Routine, t: (s: string) => string, viewer: string): string {
  if (r.trigger_kind === "event") {
    return `${t("On event:")} ${r.event_source ?? t("event")}`;
  }
  const tz = r.timezone ?? "UTC";
  if (r.cron === "0 9 * * *") return presetSummary(r, t("Every day · 09:00"), tz, viewer);
  if (r.cron === "0 9 * * 1-5") return presetSummary(r, t("Weekdays · 09:00"), tz, viewer);
  if (r.cron === "0 9 * * 1") return presetSummary(r, t("Weekly · Mon 09:00"), tz, viewer);
  if (r.cron) return `${r.cron} (${tz})`;
  if (r.interval_mins) return `${t("Every")} ${r.interval_mins}m (${tz})`;
  return tz;
}

function targetSummary(r: Routine, t: (s: string) => string): string {
  if (r.target_kind === "script_then_markdown") {
    return `${t("Custom script")} → ${t("Playbook (.md)")}`;
  }
  if (r.target_kind === "script") {
    return t("Custom script");
  }
  return t("Playbook (.md)");
}

function targetPathLabel(r: Routine): string {
  if (r.target_kind === "script_then_markdown") {
    return `${r.script_path ?? ""} → ${r.markdown_path ?? ""}`;
  }
  return r.markdown_path ?? r.script_path ?? "";
}

export default function RoutinesTab({ c }: { c: TabContext }) {
  const t = useTranslate();
  const zones = useTimeZones();
  const viewerZone = zones.effective;
  const companyZone = zones.company ?? "UTC";
  const [routines, setRoutines] = useState<Routine[] | null>(null);
  const [proposals, setProposals] = useState<StagedRoutineProposal[]>([]);
  const [waitingRuns, setWaitingRuns] = useState<RoutineRun[]>([]);
  const [connections, setConnections] = useState<Connection[]>([]);
  const [endpoints, setEndpoints] = useState<AgentEndpoint[]>([]);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [notServed, setNotServed] = useState(false);

  const [creating, setCreating] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const [detail, setDetail] = useState<Routine | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionErr, setActionErr] = useState<string | null>(null);
  const [repointThread, setRepointThread] = useState("");
  const [editConnAccount, setEditConnAccount] = useState("");

  // New routine drawer state
  const [newName, setNewName] = useState("");
  const [newTrigger, setNewTrigger] = useState<"schedule" | "event">("schedule");
  const [preset, setPreset] = useState<FrequencyPreset>("weekdays");
  const [customCron, setCustomCron] = useState("0 9 * * 1-5");
  const [customInterval, setCustomInterval] = useState("60");
  // null follows the company zone: a new routine runs in it, never in the browser's.
  const [timezoneEdit, setTimezone] = useState<string | null>(null);
  const timezone = timezoneEdit ?? companyZone;
  const [eventSource, setEventSource] = useState("github.pull_request");
  const [whatMode, setWhatMode] = useState<"playbook" | "script">("playbook");
  const [wakeWithPlaybook, setWakeWithPlaybook] = useState(true);
  const [markdownPath, setMarkdownPath] = useState("wiki:global/playbooks/triage.md");
  const [scriptPath, setScriptPath] = useState("scripts/check.sh");
  const [destThread, setDestThread] = useState("");
  const [newConnAccount, setNewConnAccount] = useState("");
  const [createErr, setCreateErr] = useState<string | null>(null);

  const load = useCallback(() => {
    if (!c.operator) return;
    setLoadErr(null);
    routinesApi
      .list(c.alias)
      .then((res) => {
        setNotServed(false);
        setRoutines(res.routines);
        setProposals(res.staged_proposals ?? []);
        setWaitingRuns(res.waiting_runs ?? []);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && (e.status === 501 || e.status === 404)) {
          setNotServed(true);
          setRoutines([]);
          return;
        }
        setLoadErr(e instanceof ApiError ? e.message : t("Could not load routines."));
      });
    api
      .connections()
      .then((res) => {
        setConnections((res.connections ?? []).filter((conn) => conn.kind === "enrolled"));
      })
      .catch(() => {});
  }, [c.alias, c.operator, t]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (!openId || !c.operator) {
      setDetail(null);
      return;
    }
    let live = true;
    setActionErr(null);
    routinesApi
      .show(openId)
      .then((res) => {
        if (!live) return;
        setDetail(res.routine);
        setRepointThread(res.routine.destination_thread ?? "");
        setEditConnAccount(res.routine.connection_account ?? "");
      })
      .catch(() => {
        if (!live) return;
        const fallback = routines?.find((item) => item.id === openId) ?? null;
        setDetail(fallback);
      });
    return () => {
      live = false;
    };
  }, [openId, c.operator, routines]);

  const scheduled = useMemo(
    () => (routines ?? []).filter((r) => r.trigger_kind === "schedule"),
    [routines],
  );
  const events = useMemo(
    () => (routines ?? []).filter((r) => r.trigger_kind === "event"),
    [routines],
  );
  const agentProposals = useMemo(
    () => proposals.filter((p) => {
      if (p.state !== "pending") return false;
      const alias = p.payload?.agent_alias;
      if (!c.scopedRoster) return alias === undefined || alias === c.alias;
      return alias == null || (typeof alias === "string" && alias.trim() === c.alias);
    }),
    [proposals, c.alias, c.scopedRoster],
  );

  if (!c.operator) {
    return (
      <div className="rtn-wrap" data-routines-tab>
        <p className="text-label text-ink-500">
          {t("Routines and script approvals are managed by the workspace owner.")}
        </p>
      </div>
    );
  }

  if (notServed) {
    return (
      <div className="rtn-wrap" data-routines-tab>
        <p className="text-label text-ink-500">
          {t("This deployment does not host local routines on the board.")}
        </p>
      </div>
    );
  }

  const resetCreateForm = () => {
    setNewName("");
    setNewTrigger("schedule");
    setPreset("weekdays");
    setCustomCron("0 9 * * 1-5");
    setCustomInterval("60");
    setTimezone(null);
    setEventSource("github.pull_request");
    setWhatMode("playbook");
    setWakeWithPlaybook(true);
    setMarkdownPath("wiki:global/playbooks/triage.md");
    setScriptPath("scripts/check.sh");
    setDestThread("");
    setNewConnAccount("");
    setCreateErr(null);
  };

  const submitCreate = async () => {
    if (busy) return;
    const trimmedName = newName.trim();
    if (!trimmedName) {
      setCreateErr(t("Enter a routine name."));
      return;
    }
    const targetKind =
      whatMode === "playbook"
        ? "markdown"
        : wakeWithPlaybook
          ? "script_then_markdown"
          : "script";
    const body: Record<string, unknown> = {
      name: trimmedName,
      agent_alias: c.alias,
      trigger_kind: newTrigger,
      target_kind: targetKind,
    };
    if (newTrigger === "schedule") {
      const tz = timezone.trim();
      if (!tz) {
        setCreateErr(t("Enter an explicit IANA timezone (for example UTC or Asia/Hong_Kong)."));
        return;
      }
      body.timezone = tz;
      if (preset === "hourly") {
        body.interval_mins = 60;
      } else if (preset === "custom") {
        if (customCron.trim()) {
          body.cron = customCron.trim();
        } else {
          const mins = Number.parseInt(customInterval, 10);
          if (!Number.isFinite(mins) || mins < 15) {
            setCreateErr(t("Recurring interval must be at least 15 minutes."));
            return;
          }
          body.interval_mins = mins;
        }
      } else {
        body.cron = PRESET_CRON[preset];
      }
    } else {
      if (!eventSource.trim()) {
        setCreateErr(t("Enter an event source."));
        return;
      }
      body.event_source = eventSource.trim();
    }
    if (targetKind === "markdown" || targetKind === "script_then_markdown") {
      if (!markdownPath.trim()) {
        setCreateErr(t("Enter a Playbook (.md) path."));
        return;
      }
      body.markdown_path = markdownPath.trim();
    }
    if (targetKind === "script" || targetKind === "script_then_markdown") {
      if (!scriptPath.trim()) {
        setCreateErr(t("Enter a script path."));
        return;
      }
      body.script_path = scriptPath.trim();
    }
    if (destThread.trim()) {
      body.destination_thread = destThread.trim();
    }
    if (newConnAccount.trim()) {
      body.connection_account = newConnAccount.trim();
    }

    setBusy(true);
    setCreateErr(null);
    try {
      const res = await routinesApi.create(body);
      setCreating(false);
      resetCreateForm();
      load();
      setOpenId(res.routine.id);
      c.toast("ok", t("Routine created — review and approve to turn on."));
    } catch (e: unknown) {
      setCreateErr(e instanceof ApiError ? e.message : t("Could not create routine."));
    } finally {
      setBusy(false);
    }
  };

  const activeRoutine = detail ?? routines?.find((item) => item.id === openId) ?? null;

  const approveRoutine = async (r: Routine) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      const payload: { expect_rev?: string | null; digest?: string | null } = {};
      if (r.target_kind === "markdown" || r.target_kind === "script_then_markdown") {
        payload.expect_rev = r.current_rev;
      }
      if (r.target_kind === "script" || r.target_kind === "script_then_markdown") {
        payload.digest = r.current_sha256;
      }
      const res = await routinesApi.approve(r.id, payload);
      setDetail(res.routine);
      load();
      c.toast("ok", t("Routine approved and active."));
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Approval failed."));
    } finally {
      setBusy(false);
    }
  };

  const runRoutineNow = async (r: Routine) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      const res = await routinesApi.run(r.id, { trigger_source: "manual" });
      const refreshed = await routinesApi.show(r.id);
      setDetail(refreshed.routine);
      load();
      c.toast(
        "ok",
        res.run.state === "skipped_empty"
          ? t("Script found 0 items — agent was not woken.")
          : t("Routine started."),
      );
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Run refused."));
      if (openId) {
        routinesApi.show(openId).then((res) => setDetail(res.routine)).catch(() => {});
      }
      load();
    } finally {
      setBusy(false);
    }
  };

  const togglePause = async (r: Routine) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      const res =
        r.status === "paused" ? await routinesApi.resume(r.id) : await routinesApi.pause(r.id);
      setDetail(res.routine);
      load();
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Could not update routine status."));
    } finally {
      setBusy(false);
    }
  };

  const saveRepointThread = async (r: Routine) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      const res = await routinesApi.update(r.id, {
        destination_thread: repointThread.trim() ? repointThread.trim() : null,
        connection_account: editConnAccount.trim() ? editConnAccount.trim() : null,
      });
      setDetail(res.routine);
      load();
      c.toast("ok", t("Delivery settings updated."));
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Could not update delivery settings."));
    } finally {
      setBusy(false);
    }
  };

  const decideProposal = async (id: string, decision: "approve" | "dismiss") => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      const res = await routinesApi.proposalDecide(id, decision);
      load();
      if (decision === "approve" && res.routine) {
        setOpenId(res.routine.id);
      }
      c.toast("ok", decision === "approve" ? t("Proposal approved.") : t("Proposal dismissed."));
    } catch (e: unknown) {
      setLoadErr(e instanceof ApiError ? e.message : t("Could not record routine proposal decision."));
    } finally {
      setBusy(false);
    }
  };

  const decideRun = async (runId: string, decision: "approve" | "dismiss") => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      await routinesApi.runDecide(runId, decision);
      if (openId) {
        routinesApi.show(openId).then((res) => setDetail(res.routine)).catch(() => {});
      }
      load();
      c.toast("ok", decision === "approve" ? t("Step approved.") : t("Step dismissed."));
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Could not record step decision."));
    } finally {
      setBusy(false);
    }
  };

  const removeRoutine = async (r: Routine) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      await routinesApi.remove(r.id);
      setOpenId(null);
      setDetail(null);
      load();
      c.toast("ok", t("Routine removed."));
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Could not remove routine."));
    } finally {
      setBusy(false);
    }
  };

  const renderRow = (r: Routine) => {
    const meta = statusMeta(r);
    return (
      <button
        key={r.id}
        type="button"
        className="rtn-row"
        data-record-id={r.id}
        onClick={() => setOpenId(r.id)}
      >
        <span className="rtn-row-main">
          <b>{r.name}</b>
          <small>
            {scheduleSummary(r, t, viewerZone)} · {targetSummary(r, t)}
          </small>
          <span className="rtn-row-path">{targetPathLabel(r)}</span>
        </span>
        <span className="rtn-pill" data-tone={meta.tone}>
          <span className="rtn-pill-dot" aria-hidden />
          {t(meta.label)}
        </span>
      </button>
    );
  };

  const drawerMenu: DrawerMenuItem[] =
    activeRoutine && c.writable
      ? [
          {
            key: "delete",
            label: t("Delete routine"),
            destructive: true,
            disabled: busy,
            onSelect: () => {
              void removeRoutine(activeRoutine);
            },
          },
        ]
      : [];

  return (
    <div className="rtn-wrap" data-routines-tab>
      <div className="rtn-head">
        <div className="rtn-head-copy">
          <p className="text-label text-ink-400">
            {t(
              "Run an owner-approved Playbook (.md) or SHA-256 pinned script on a recurring schedule or verified event.",
            )}
          </p>
        </div>
        {c.writable && (
          <Button
            size="sm"
            variant="primary"
            onClick={() => {
              resetCreateForm();
              setCreating(true);
            }}
          >
            {t("+ New routine")}
          </Button>
        )}
      </div>

      <IncomingEndpointsSection
        c={c}
        eventRoutines={events}
        onEndpointsChange={setEndpoints}
      />

      {loadErr && (
        <p className="text-label text-fail" role="alert">
          {loadErr}{" "}
          <button type="button" className="lnk" onClick={load}>
            {t("Retry")}
          </button>
        </p>
      )}

      {routines === null && !loadErr ? (
        <p className="text-label text-ink-500">{t("Loading routines…")}</p>
      ) : (
        <>
          {waitingRuns.length > 0 && (
            <section data-work-panel="waiting-routine-steps">
              <div className="ap-panel-h">
                <h3>{t("Waiting step approvals")}</h3>
              </div>
              <div className="ap-rows">
                {waitingRuns.map((run) => {
                  const waitingStep =
                    (run.checklist_state ?? run.checklist ?? []).find(
                      (s) => s.state === "waiting_approval",
                    )?.label ?? t("Ask for approval first");
                  return (
                    <div key={run.id} className="ap-row flex-wrap gap-2">
                      <span className="min-w-0">
                        <b className="text-ink-100">{run.routine_name ?? run.routine_id}</b>{" "}
                        <span className="text-warn text-micro">({waitingStep})</span>
                      </span>
                      {c.writable ? (
                        <span className="flex flex-wrap items-center gap-1.5">
                          <Button
                            size="sm"
                            variant="primary"
                            onClick={() => void decideRun(run.id, "approve")}
                            disabled={busy}
                          >
                            {t("Approve & send")}
                          </Button>
                          <Button
                            size="sm"
                            variant="secondary"
                            onClick={() => void decideRun(run.id, "dismiss")}
                            disabled={busy}
                          >
                            {t("Dismiss")}
                          </Button>
                        </span>
                      ) : (
                        <small className="num text-warn">{t("Waiting for you")}</small>
                      )}
                    </div>
                  );
                })}
              </div>
            </section>
          )}

          {agentProposals.length > 0 && (
            <section data-work-panel="staged-routines">
              <div className="ap-panel-h">
                <h3>{t("Proposed by agents")}</h3>
              </div>
              <div className="ap-rows">
                {agentProposals.map((p) => (
                  <div key={p.id} className="ap-row flex-wrap gap-2">
                    <span className="min-w-0">
                      {p.title}{" "}
                      <span className="text-ink-500 text-micro">
                        ({p.kind === "wiki_propose" ? t("Playbook draft") : t("Routine draft")})
                      </span>
                    </span>
                    {c.writable ? (
                      <span className="flex flex-wrap items-center gap-1.5">
                        <Button
                          size="sm"
                          variant="primary"
                          onClick={() => void decideProposal(p.id, "approve")}
                          disabled={busy}
                        >
                          {t("Approve")}
                        </Button>
                        <Button
                          size="sm"
                          variant="secondary"
                          onClick={() => void decideProposal(p.id, "dismiss")}
                          disabled={busy}
                        >
                          {t("Dismiss")}
                        </Button>
                      </span>
                    ) : (
                      <small className="num text-warn">{t("Waiting for you")}</small>
                    )}
                  </div>
                ))}
              </div>
            </section>
          )}

          <section data-work-panel="scheduled-routines">
            <div className="ap-panel-h">
              <h3>{t("Scheduled")}</h3>
              <span className="text-micro text-ink-500 num">{scheduled.length}</span>
            </div>
            {scheduled.length === 0 ? (
              <p className="text-label text-ink-500">
                {t("No scheduled routines yet. Minimum interval is 15 minutes with an explicit IANA timezone.")}
              </p>
            ) : (
              <div className="ap-rows">{scheduled.map(renderRow)}</div>
            )}
          </section>

          <section data-work-panel="event-routines">
            <div className="ap-panel-h">
              <h3>{t("When something happens")}</h3>
              <span className="text-micro text-ink-500 num">{events.length}</span>
            </div>
            {events.length === 0 ? (
              <p className="text-label text-ink-500">
                {t("No event-triggered routines configured for this agent.")}
              </p>
            ) : (
              <div className="ap-rows">{events.map(renderRow)}</div>
            )}
          </section>
        </>
      )}

      {creating && (
        <DrawerShell
          kind="routine-new"
          scrim="board"
          modal
          trapFocus
          size="md"
          preventClose={busy}
          label={t("New routine")}
          title={t("New routine")}
          subtitle={`${t("Agent")}: ${c.alias}`}
          onClose={() => setCreating(false)}
          secondary={
            <Button size="sm" variant="ghost" onClick={() => setCreating(false)} disabled={busy}>
              {t("Cancel")}
            </Button>
          }
          primary={
            <Button size="sm" variant="primary" onClick={() => void submitCreate()} loading={busy}>
              {t("Create routine")}
            </Button>
          }
        >
          <div className="rtn-drawer-form">
                  <div className="rtn-field">
                    <label htmlFor="rtn-name">{t("Name")}</label>
                    <input
                      id="rtn-name"
                      className="rtn-input"
                      value={newName}
                      onChange={(e) => setNewName(e.target.value)}
                      placeholder={t("Morning PR & support triage")}
                    />
                  </div>

                  <div className="rtn-field">
                    <span className="rtn-field-label">{t("When it runs")}</span>
                    <div className="rtn-seg" role="radiogroup" aria-label={t("When it runs")}>
                      <button
                        type="button"
                        role="radio"
                        aria-checked={newTrigger === "schedule"}
                        className="rtn-seg-btn"
                        onClick={() => setNewTrigger("schedule")}
                      >
                        <b>{t("Scheduled")}</b>
                        <small>{t("Fixed time or >= 15m interval")}</small>
                      </button>
                      <button
                        type="button"
                        role="radio"
                        aria-checked={newTrigger === "event"}
                        className="rtn-seg-btn"
                        onClick={() => setNewTrigger("event")}
                      >
                        <b>{t("When something happens")}</b>
                        <small>{t("Verified webhook or event")}</small>
                      </button>
                    </div>
                  </div>

                  {newTrigger === "schedule" ? (
                    <>
                      <div className="rtn-field">
                        <span className="rtn-field-label">{t("Frequency")}</span>
                        <div className="rtn-presets">
                          {(
                            [
                              ["weekdays", "Weekdays"],
                              ["daily", "Every day"],
                              ["weekly", "Weekly"],
                              ["hourly", "Every hour"],
                              ["custom", "Custom"],
                            ] as const
                          ).map(([id, label]) => (
                            <button
                              key={id}
                              type="button"
                              className="rtn-preset"
                              aria-pressed={preset === id}
                              onClick={() => setPreset(id)}
                            >
                              {t(label)}
                            </button>
                          ))}
                        </div>
                      </div>
                      {preset === "custom" && (
                        <div className="rtn-field">
                          <label htmlFor="rtn-cron">{t("Cron expression (or leave empty for interval)")}</label>
                          <input
                            id="rtn-cron"
                            className="rtn-input num"
                            value={customCron}
                            onChange={(e) => setCustomCron(e.target.value)}
                            placeholder="*/30 * * * *"
                          />
                          {!customCron.trim() && (
                            <>
                              <label htmlFor="rtn-interval">{t("Interval (minutes, >= 15)")}</label>
                              <input
                                id="rtn-interval"
                                className="rtn-input num"
                                type="number"
                                min={15}
                                value={customInterval}
                                onChange={(e) => setCustomInterval(e.target.value)}
                              />
                            </>
                          )}
                        </div>
                      )}
                      <div className="rtn-field">
                        <label htmlFor="rtn-tz">{t("Timezone (IANA)")}</label>
                        <input
                          id="rtn-tz"
                          className="rtn-input num"
                          value={timezone}
                          onChange={(e) => setTimezone(e.target.value)}
                          placeholder="Asia/Hong_Kong"
                        />
                        <div className="rtn-presets">
                          {[...new Set([companyZone, "UTC", "Asia/Hong_Kong", "America/New_York", "Europe/London"])].map((tz) => (
                            <button
                              key={tz}
                              type="button"
                              className="rtn-preset num"
                              aria-pressed={timezone === tz}
                              onClick={() => setTimezone(tz)}
                            >
                              {tz}
                            </button>
                          ))}
                        </div>
                      </div>
                    </>
                  ) : (
                    <>
                      {endpoints.length > 0 && (
                        <div className="rtn-field">
                          <label htmlFor="rtn-endpoint-bind">{t("Incoming endpoint (optional)")}</label>
                          <select
                            id="rtn-endpoint-bind"
                            className="rtn-input"
                            value={
                              endpoints.find((ep) => {
                                const src =
                                  ep.kind === "email"
                                    ? `email:${formatEndpointEmail(ep.slug)}`
                                    : `webhook:${ep.id}`;
                                return eventSource.trim() === src;
                              })?.id ?? ""
                            }
                            onChange={(e) => {
                              const picked = endpoints.find((ep) => ep.id === e.target.value);
                              if (!picked) return;
                              setEventSource(
                                picked.kind === "email"
                                  ? `email:${formatEndpointEmail(picked.slug)}`
                                  : `webhook:${picked.id}`,
                              );
                            }}
                          >
                            <option value="">{t("Custom event source below…")}</option>
                            {endpoints.map((ep) => (
                              <option key={ep.id} value={ep.id}>
                                {ep.kind === "email"
                                  ? `${ep.slug} · ${formatEndpointEmail(ep.slug)}`
                                  : `${ep.slug} · webhook (${ep.id})`}
                              </option>
                            ))}
                          </select>
                        </div>
                      )}
                      <div className="rtn-field">
                        <label htmlFor="rtn-event">{t("Event source")}</label>
                        <input
                          id="rtn-event"
                          className="rtn-input num"
                          value={eventSource}
                          onChange={(e) => setEventSource(e.target.value)}
                          placeholder="github.pull_request"
                        />
                      </div>
                    </>
                  )}

                  <div className="rtn-field">
                    <span className="rtn-field-label">{t("What runs")}</span>
                    <div className="rtn-seg" role="radiogroup" aria-label={t("What runs")}>
                      <button
                        type="button"
                        role="radio"
                        aria-checked={whatMode === "playbook"}
                        className="rtn-seg-btn"
                        onClick={() => setWhatMode("playbook")}
                      >
                        <b>{t("Playbook (.md)")}</b>
                        <small>{t("Context Markdown instructions")}</small>
                      </button>
                      <button
                        type="button"
                        role="radio"
                        aria-checked={whatMode === "script"}
                        className="rtn-seg-btn"
                        onClick={() => setWhatMode("script")}
                      >
                        <b>{t("Custom script")}</b>
                        <small>{t("SHA-256 pinned pre-check (0 LLM tokens when empty)")}</small>
                      </button>
                    </div>
                  </div>

                  {whatMode === "script" && (
                    <>
                      <div className="rtn-field">
                        <label htmlFor="rtn-script">{t("Script path")}</label>
                        <input
                          id="rtn-script"
                          className="rtn-input num"
                          value={scriptPath}
                          onChange={(e) => setScriptPath(e.target.value)}
                          placeholder="scripts/check.sh"
                        />
                      </div>
                      <label className="rtn-check-row">
                        <input
                          type="checkbox"
                          checked={wakeWithPlaybook}
                          onChange={(e) => setWakeWithPlaybook(e.target.checked)}
                        />
                        <span>{t("Wake agent with Playbook (.md) only when script finds items")}</span>
                      </label>
                    </>
                  )}

                  {(whatMode === "playbook" || wakeWithPlaybook) && (
                    <div className="rtn-field">
                      <label htmlFor="rtn-md">{t("Playbook (.md) path")}</label>
                      <input
                        id="rtn-md"
                        className="rtn-input num"
                        value={markdownPath}
                        onChange={(e) => setMarkdownPath(e.target.value)}
                        placeholder="wiki:global/playbooks/triage.md"
                      />
                      <span className="text-micro text-ink-500">
                        {t("Accepts wiki:<path>.md or workflows/<name>.md. Pinned by revision on owner approval.")}
                      </span>
                    </div>
                  )}

                  <div className="rtn-field">
                    <label htmlFor="rtn-conn-account">{t("Connection account")}</label>
                    <select
                      id="rtn-conn-account"
                      className="rtn-input"
                      value={newConnAccount}
                      onChange={(e) => setNewConnAccount(e.target.value)}
                    >
                      <option value="">{t("Default account")}</option>
                      {connections.map((conn) => (
                        <option key={conn.id} value={conn.id}>
                          {conn.provider} · {conn.account}
                          {conn.policy?.visibility === "personal" ? ` (${t("Just me (Personal)")})` : ""}
                          {conn.policy?.is_default ? ` · ${t("Default")}` : ""}
                        </option>
                      ))}
                    </select>
                    {newTrigger === "event" &&
                      newConnAccount &&
                      connections.find((conn) => conn.id === newConnAccount)?.policy
                        ?.allow_external_triggers === false && (
                        <p className="rtn-ep-warn" role="status">
                          {t(
                            "Selected connection blocks external triggers (allow_external_triggers: false). Enable external triggers in Connections before external events can invoke it.",
                          )}
                        </p>
                      )}
                  </div>

                  <div className="rtn-field">
                    <label htmlFor="rtn-thread">{t("Destination thread (optional)")}</label>
                    <input
                      id="rtn-thread"
                      className="rtn-input num"
                      value={destThread}
                      onChange={(e) => setDestThread(e.target.value)}
                      placeholder="thr_..."
                    />
                  </div>

                  {createErr && (
                    <p className="text-label text-fail" role="alert">
                      {createErr}
                    </p>
                  )}
                </div>
        </DrawerShell>
      )}

      {activeRoutine && (
        <DrawerShell
          key={activeRoutine.id}
          kind="routine"
          scrim="board"
          modal
          trapFocus
          size="md"
          preventClose={busy}
          label={activeRoutine.name}
          title={activeRoutine.name}
          subtitle={`${scheduleSummary(activeRoutine, t, viewerZone)} · ${targetSummary(activeRoutine, t)}`}
          pills={
            <span className="rtn-pill" data-tone={statusMeta(activeRoutine).tone}>
              <span className="rtn-pill-dot" aria-hidden />
              {t(statusMeta(activeRoutine).label)}
            </span>
          }
          warning={
            activeRoutine.status === "waiting_approval"
              ? activeRoutine.approval_drift &&
                (activeRoutine.approved_rev !== null || activeRoutine.approved_sha256 !== null)
                ? t("Target file changed since approval — review the updated revision/digest and approve to resume.")
                : t("Owner approval of the live revision/digest is required before this routine runs.")
              : activeRoutine.status === "repoint_required"
                ? t("Destination thread was archived or deleted. Choose a live thread below to resume.")
                : activeRoutine.pause_reason === "auto_paused_failures"
                  ? t("Auto-paused after 10 consecutive failures. Inspect recent runs before resuming.")
                  : undefined
          }
          onClose={() => {
            setOpenId(null);
            setDetail(null);
          }}
          menu={drawerMenu}
          secondary={
            c.writable && activeRoutine.status === "on" ? (
              <Button
                size="sm"
                variant="secondary"
                onClick={() => void togglePause(activeRoutine)}
                disabled={busy}
              >
                {t("Pause")}
              </Button>
            ) : undefined
          }
          primary={
            c.writable ? (
              activeRoutine.status === "waiting_approval" ? (
                <Button
                  size="sm"
                  variant="primary"
                  onClick={() => void approveRoutine(activeRoutine)}
                  loading={busy}
                >
                  {t("Approve")}
                </Button>
              ) : activeRoutine.status === "paused" ? (
                <Button
                  size="sm"
                  variant="primary"
                  onClick={() => void togglePause(activeRoutine)}
                  loading={busy}
                >
                  {t("Resume")}
                </Button>
              ) : activeRoutine.status === "on" ? (
                <Button
                  size="sm"
                  variant="primary"
                  onClick={() => void runRoutineNow(activeRoutine)}
                  loading={busy}
                >
                  {t("Run now")}
                </Button>
              ) : undefined
            ) : undefined
          }
          tabs={[
            {
              id: "overview",
              label: t("Overview"),
              panel: (
                <div className="rtn-drawer-form">
                  {actionErr && (
                    <p className="text-label text-fail" role="alert">
                      {actionErr}
                    </p>
                  )}

                  <div className="rtn-card-box">
                    <span className="slabel">{t("What runs")}</span>
                    {activeRoutine.markdown_path && (
                      <dl className="space-y-1">
                        <div className="rtn-kv">
                          <dt>{t("Playbook (.md)")}</dt>
                          <dd className="num">{activeRoutine.markdown_path}</dd>
                        </div>
                        <div className="rtn-kv">
                          <dt>{t("Live revision")}</dt>
                          <dd className="num">{activeRoutine.current_rev ?? t("missing")}</dd>
                        </div>
                        <div className="rtn-kv">
                          <dt>{t("Approved revision")}</dt>
                          <dd className="num">{activeRoutine.approved_rev ?? t("not approved")}</dd>
                        </div>
                      </dl>
                    )}
                    {activeRoutine.script_path && (
                      <dl className="space-y-1">
                        <div className="rtn-kv">
                          <dt>{t("Custom script")}</dt>
                          <dd className="num">{activeRoutine.script_path}</dd>
                        </div>
                        <div className="rtn-kv">
                          <dt>{t("Live SHA-256")}</dt>
                          <dd className="num break-all">
                            {activeRoutine.current_sha256
                              ? `${activeRoutine.current_sha256.slice(0, 16)}…`
                              : t("missing")}
                          </dd>
                        </div>
                        <div className="rtn-kv">
                          <dt>{t("Approved SHA-256")}</dt>
                          <dd className="num break-all">
                            {activeRoutine.approved_sha256
                              ? `${activeRoutine.approved_sha256.slice(0, 16)}…`
                              : t("not approved")}
                          </dd>
                        </div>
                      </dl>
                    )}
                  </div>

                  <div className="rtn-card-box">
                    <span className="slabel">{t("Schedule & delivery")}</span>
                    <dl className="space-y-1">
                      <div className="rtn-kv">
                        <dt>{t("Trigger")}</dt>
                        <dd>{scheduleSummary(activeRoutine, t, viewerZone)}</dd>
                      </div>
                      {activeRoutine.next_due_at && (
                        <div className="rtn-kv">
                          <dt>{t("Next slot")}</dt>
                          <dd className="num">{fmtInZone(activeRoutine.next_due_at, viewerZone) ?? fmtTime(activeRoutine.next_due_at)}</dd>
                        </div>
                      )}
                      {activeRoutine.connection_account && (
                        <div className="rtn-kv">
                          <dt>{t("Pinned account")}</dt>
                          <dd className="num">{activeRoutine.connection_account}</dd>
                        </div>
                      )}
                    </dl>
                    {c.writable && (
                      <div className="rtn-field mt-2">
                        <label htmlFor="rtn-edit-conn">{t("Connection account")}</label>
                        <select
                          id="rtn-edit-conn"
                          className="rtn-input"
                          value={editConnAccount}
                          onChange={(e) => setEditConnAccount(e.target.value)}
                        >
                          <option value="">{t("Default account")}</option>
                          {editConnAccount &&
                            !connections.some((conn) => conn.id === editConnAccount) && (
                              <option value={editConnAccount}>{editConnAccount}</option>
                            )}
                          {connections.map((conn) => (
                            <option key={conn.id} value={conn.id}>
                              {conn.provider} · {conn.account}
                              {conn.policy?.visibility === "personal" ? ` (${t("Just me (Personal)")})` : ""}
                              {conn.policy?.is_default ? ` · ${t("Default")}` : ""}
                            </option>
                          ))}
                        </select>
                        {activeRoutine.trigger_kind === "event" &&
                          editConnAccount &&
                          connections.find((conn) => conn.id === editConnAccount)?.policy
                            ?.allow_external_triggers === false && (
                            <p className="rtn-ep-warn" role="status">
                              {t(
                                "Selected connection blocks external triggers (allow_external_triggers: false). Enable external triggers in Connections before external events can invoke it.",
                              )}
                            </p>
                          )}
                        <label htmlFor="rtn-repoint">{t("Destination thread")}</label>
                        <div className="flex flex-wrap gap-2">
                          <input
                            id="rtn-repoint"
                            className="rtn-input num flex-1"
                            value={repointThread}
                            onChange={(e) => setRepointThread(e.target.value)}
                            placeholder={t("Default agent inbox")}
                          />
                          <Button
                            size="sm"
                            variant="secondary"
                            onClick={() => void saveRepointThread(activeRoutine)}
                            disabled={busy}
                          >
                            {t("Save delivery")}
                          </Button>
                        </div>
                      </div>
                    )}
                  </div>

                  {activeRoutine.checklist.length > 0 && (
                    <div className="rtn-card-box">
                      <span className="slabel">{t("Checklist")}</span>
                      <ul className="space-y-1.5 text-label">
                        {activeRoutine.checklist.map((step) => (
                          <li key={step.id} className="flex flex-wrap items-center justify-between gap-2">
                            <span className="text-ink-200">{step.label}</span>
                            {step.requires_approval && (
                              <span className="chip bg-warn/10 text-warn">
                                {t("Ask for approval first")}
                              </span>
                            )}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                </div>
              ),
            },
            {
              id: "runs",
              label: t("Recent runs"),
              panel: (
                <div className="rtn-drawer-form">
                  {!activeRoutine.runs || activeRoutine.runs.length === 0 ? (
                    <p className="text-label text-ink-500">{t("No runs recorded yet.")}</p>
                  ) : (
                    <div className="ap-rows">
                      {activeRoutine.runs.map((run) => (
                        <div key={run.id} className="ap-row flex-wrap gap-2">
                          <span className="min-w-0">
                            <b className="text-ink-200">{run.state}</b>{" "}
                            <span className="text-ink-500 text-micro">({run.trigger_source})</span>
                            {run.state === "skipped_empty" && (
                              <span className="block text-micro text-ink-500">
                                {t("Script found 0 items — 0 LLM tokens used")}
                              </span>
                            )}
                            {run.error && (
                              <span className="block text-micro text-fail break-words">{run.error}</span>
                            )}
                          </span>
                          <span className="flex flex-wrap items-center gap-1.5">
                            {c.writable && run.state === "waiting_approval" && (
                              <>
                                <Button
                                  size="sm"
                                  variant="primary"
                                  onClick={() => void decideRun(run.id, "approve")}
                                  disabled={busy}
                                >
                                  {t("Approve & send")}
                                </Button>
                                <Button
                                  size="sm"
                                  variant="secondary"
                                  onClick={() => void decideRun(run.id, "dismiss")}
                                  disabled={busy}
                                >
                                  {t("Dismiss")}
                                </Button>
                              </>
                            )}
                            <small className="num">{fmtInZone(run.started_at, viewerZone) ?? fmtTime(run.started_at)}</small>
                          </span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ),
            },
          ]}
        />
      )}
    </div>
  );
}
