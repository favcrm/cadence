import { useEffect, useMemo, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import type { WorkspaceRun, AppEffect } from "./workspaceApps";
import {
  socialPublish,
  publishStateText,
  publishStateTone,
  type PublishIntent,
} from "./socialPublish";
import { HorizontalStrip } from "./HorizontalStrip";
import { plainTitle } from "./presentation";
import "./schedule-calendar.css";

/* CAD-980 — installed Schedule date-row calendar (host MVP).
 *
 * Replaces the Schedule placeholder with the operator-confirmed presentation:
 * one row per day, a fixed date column on the left, a fixed-width (240px,
 * gap 8) horizontal card strip per day that never wraps, plus an explicit
 * Unplanned row for in-scope runs carrying no valid matching intent. Data is
 * REAL and scoped — the verified `socialPublish.list` result joined to the
 * already-loaded `runs`/`effects` for this route-owned `installId` and the
 * selected `contextId`. No fixtures, no invented timestamps/destinations/
 * receipts.
 *
 * Honesty contract (mirrors the CAD-977 app-owned projection semantics):
 *  - A calendar DAY is anchored only by a real intent's `due_epoch`+`timezone`.
 *    A malformed `due_epoch`/`timezone` does not silently vanish — it renders
 *    as an explicit "unresolved plan" with its id, not a fabricated day.
 *  - Several historical intents per run are legitimate (cancelled then
 *    rescheduled, different destinations): independent cards keyed `intent_id`.
 *    A duplicated `intent_id` is a conflict — ALL conflicting cards are
 *    withheld, never a first-kept pretence.
 *  - `posted` shows only a real posted intent; a locally completed run is not
 *    an external post. `held`/`cancelled`/`refused` keep explicit labels.
 *  - Joins validate install + run_id + matching context: a cross-context or
 *    foreign-install intent/effect is excluded, never mis-attributed.
 *  - Effect linkage needs full install/context/run/effect identity; an absent
 *    or mismatched effect is flagged "linkage unavailable", never satisfied
 *    by a fabricated effect/receipt.
 *  - The list is capped at 100 records; at the cap the view says schedule
 *    history may be incomplete — never claims full coverage or "most recent"
 *    (the cap order is not chronology).
 *  - Read state is tagged with the scope it belongs to; only the exact current
 *    scope is rendered, so a slow reply can never flash a previous context.
 *  - The read is isolated to this component: a calendar load failure cannot
 *    break the existing Board/Library/source/new-run handlers.
 *  - Read-only for non-operators; every write stays in existing handlers.
 */

const LIST_CAP = 100; // daemon caps the list at 100, ORDER BY intent_id

interface DayGroup {
  date: string;
  timezone: string;
  intents: PublishIntent[];
}

/** Rows that cannot land on a day: a real intent row with a malformed
 *  `due_epoch`/`timezone` — shown honestly, never dropped. */
interface MalformedPlan {
  intent: PublishIntent;
  reason: string;
}

/** Local `YYYY-MM-DD` in the intent's own timezone, or null when the timezone
 *  is not a real IANA zone or the epoch is not a finite integer. */
function intentDayKey(dueEpoch: number, timezone: string): string | null {
  try {
    // due_epoch is a verified integer (toPublishIntent enforces it), but the
    // helper still validates honestly rather than asserting it.
    if (!Number.isInteger(dueEpoch)) return null;
    const parts = new Intl.DateTimeFormat("en-CA", {
      timeZone: timezone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).formatToParts(new Date(dueEpoch * 1000));
    const get = (t: string) => parts.find((p) => p.type === t)?.value ?? "";
    const key = `${get("year")}-${get("month")}-${get("day")}`;
    return /^\d{4}-\d{2}-\d{2}$/.test(key) ? key : null;
  } catch {
    return null;
  }
}

interface Projection {
  days: DayGroup[];
  malformed: MalformedPlan[];
  /** intent_ids withheld because a second row reused them (conflict). */
  conflictingIds: string[];
  /** in-scope runs that carry no valid matching intent → Unplanned. */
  unplanned: WorkspaceRun[];
  /** intents whose claimed effect is absent/mismatched under full identity. */
  unresolvedEffects: string[]; // intent_ids
  /** Exact in-scope runs resolved by full identity — for open-run-detail, so a
   *  foreign same-id run is never picked over the in-scope one. */
  runById: Map<string, WorkspaceRun>;
}

/** Same scope rule WorkspaceApp applies: "" matches context-less runs only. */
function inScope(contextId: string | null, selected: string): boolean {
  return (contextId ?? "") === selected;
}

/**
 * Project the read into day rows + an Unplanned row. Pure — testable without
 * the network. Every join validates install + run_id + matching context.
 */
export function projectSchedule(args: {
  installId: string;
  contextId: string;
  runs: WorkspaceRun[];
  effects: AppEffect[];
  intents: PublishIntent[];
}): Projection {
  const { installId, contextId, runs, effects, intents } = args;

  const runById = new Map<string, WorkspaceRun>();
  for (const run of runs) {
    if (run.install_id !== installId) continue;
    if (!inScope(run.context_id, contextId)) continue;
    if (!runById.has(run.id)) runById.set(run.id, run);
  }

  // In-scope effects keyed for full-identity linkage checks (install + the
  // effect's run being in scope + the effect's context matching that run's).
  const effectById = new Map<string, AppEffect>();
  for (const effect of effects) {
    if (effect.authority.install_id !== installId) continue;
    const run = runById.get(effect.authority.run_id);
    if (!run) continue;
    if ((run.context_id ?? null) !== (effect.authority.context?.id ?? null)) continue;
    effectById.set(effect.effect_id, effect);
  }

  const seenIds = new Set<string>();
  const conflictingIds: string[] = [];
  const malformed: MalformedPlan[] = [];
  const byDay = new Map<string, DayGroup>();
  const unresolvedEffects: string[] = [];
  const runsWithIntent = new Set<string>();
  // Runs resolved by exact identity (install+context+id) — used to open run
  // detail so a foreign same-id run is never picked over the in-scope one.
  const resolvedRunById = new Map<string, WorkspaceRun>();

  // Withhold a conflicting intent_id from EVERY surface — accepted day cards,
  // malformed rows, and effect-unresolved flags — not just the late row.
  const withhold = (id: string) => {
    for (const day of byDay.values())
      day.intents = day.intents.filter((i) => i.intent_id !== id);
    for (let i = malformed.length - 1; i >= 0; i--)
      if (malformed[i].intent.intent_id === id) malformed.splice(i, 1);
    for (let i = unresolvedEffects.length - 1; i >= 0; i--)
      if (unresolvedEffects[i] === id) unresolvedEffects.splice(i, 1);
  };

  for (const intent of intents) {
    if (intent.install_id !== installId) continue; // foreign install — drop
    const run = runById.get(intent.run_id);
    if (!run) continue; // run outside scope (other context / wrong install)
    if ((run.context_id ?? null) !== (intent.context_id ?? null)) continue; // cross-context
    if (seenIds.has(intent.intent_id) || conflictingIds.includes(intent.intent_id)) {
      // Same intent_id reused → withhold ALL rows under that id (a genuine
      // conflict): every card, malformed row and unresolved flag is removed.
      if (!conflictingIds.includes(intent.intent_id)) {
        conflictingIds.push(intent.intent_id);
        withhold(intent.intent_id);
      }
      continue;
    }
    seenIds.add(intent.intent_id);
    runsWithIntent.add(run.id);
    resolvedRunById.set(run.id, run); // exact in-scope run for this intent

    const linked = effectById.get(intent.effect_id);
    const effectUnresolved = !linked || linked.authority.run_id !== run.id;
    if (effectUnresolved) unresolvedEffects.push(intent.intent_id);

    const date = intentDayKey(intent.due_epoch, intent.timezone);
    if (date === null) {
      malformed.push({
        intent,
        reason: !Number.isInteger(intent.due_epoch)
          ? `unresolvable due_epoch ${String(intent.due_epoch)}`
          : `unresolvable timezone "${intent.timezone}"`,
      });
      continue;
    }
    const key = `${date}::${intent.timezone}`;
    if (!byDay.has(key)) byDay.set(key, { date, timezone: intent.timezone, intents: [] });
    byDay.get(key)!.intents.push(intent);
  }

  // Drop day buckets emptied by a conflict withhold.
  const days = [...byDay.values()]
    .filter((d) => d.intents.length > 0)
    .sort((a, b) =>
      a.date === b.date ? a.timezone.localeCompare(b.timezone) : a.date.localeCompare(b.date),
    );
  for (const day of days) day.intents.sort((a, b) => a.due_epoch - b.due_epoch);

  const unplanned = [...runById.values()].filter((run) => !runsWithIntent.has(run.id));

  return { days, malformed, conflictingIds, unplanned, unresolvedEffects, runById: resolvedRunById };
}

/** The read's scope tag — results render only while it still matches the
 *  current install/context, so a slow reply can never paint stale scope. */
interface ScopedRead {
  installId: string;
  contextId: string;
  intents: PublishIntent[] | null;
  loadError: string | null;
  loading: boolean;
  /** Raw count from the daemon reply BEFORE local filtering — the cap
   *  signal. `ORDER BY intent_id LIMIT 100` is id order, not chronology. */
  rawCount: number;
}

export default function ScheduleCalendar({
  installId,
  contextId,
  runs,
  effects,
  canWrite,
  onOpenRun,
  onDenied,
  refreshToken,
  client = socialPublish,
}: {
  installId: string;
  /** Selected context; "" selects the host's context-less (unscoped) rows. */
  contextId: string;
  runs: WorkspaceRun[];
  effects: AppEffect[];
  canWrite: boolean;
  onOpenRun: (run: WorkspaceRun) => void;
  onDenied: () => void;
  /** Stable reference that changes when the parent's scoped snapshot refreshes
   *  (e.g. WorkspaceApp's `data`). Used as the read's invalidation token so a
   *  parent refresh re-reads intents — never the per-render `runs`/`effects`
   *  arrays, which would loop. */
  refreshToken: unknown;
  client?: typeof socialPublish;
}) {
  const [read, setRead] = useState<ScopedRead>({
    installId,
    contextId,
    intents: null,
    loadError: null,
    loading: true,
    rawCount: 0,
  });
  const [refetchKey, setRefetchKey] = useState(0);
  const active = useRef<AbortController | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    active.current?.abort();
    active.current = controller;
    client
      .list(installId, contextId || null, controller.signal)
      .then((reply) => {
        if (controller.signal.aborted) return;
        setRead({
          installId, contextId,
          intents: reply.intents,
          loadError: null,
          loading: false,
          rawCount: reply.intents.length, // raw pre-filter count → cap signal
        });
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        if (error instanceof ApiError && [401, 403].includes(error.status)) onDenied();
        setRead({
          installId, contextId,
          intents: null,
          loadError: error instanceof Error ? error.message : String(error),
          loading: false,
          rawCount: 0,
        });
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- refreshToken is a
    // deliberate invalidation reference; runs/effects are read via props, not deps.
  }, [installId, contextId, client, onDenied, refetchKey, refreshToken]);

  // Render only the read that matches the CURRENT scope — a reply tagged for a
  // previous install/context is never projected (no stale-scope flash).
  const current = read.installId === installId && read.contextId === contextId;
  const scoped = current
    ? read
    : { installId, contextId, intents: null, loadError: null, loading: true, rawCount: 0 };
  const rawCount = scoped.rawCount;

  const projection = useMemo(() => {
    if (scoped.intents === null) return null;
    return projectSchedule({ installId, contextId, runs, effects, intents: scoped.intents });
  }, [scoped.intents, installId, contextId, runs, effects]);

  const unresolved = useMemo(
    () => new Set(projection?.unresolvedEffects ?? []),
    [projection],
  );
  const atCap = scoped.intents !== null && rawCount >= LIST_CAP;

  const dayLabel = (date: string) => {
    const [y, m, d] = date.split("-").map(Number);
    const day = new Date(Date.UTC(y, m - 1, d));
    return day.toLocaleDateString(undefined, { weekday: "short", timeZone: "UTC" });
  };

  const intentCard = (intent: PublishIntent) => {
    // Resolve via the projection's identity-checked map — never a raw
    // `runs.find` that could pick a foreign run sharing the id.
    const run = projection ? projection.runById.get(intent.run_id) ?? null : null;
    const unresolvedFlag = unresolved.has(intent.intent_id);
    return (
      <button
        key={intent.intent_id}
        type="button"
        className="wa-post-card wa-cal-card"
        data-tone={publishStateTone(intent.state)}
        onClick={() => run && onOpenRun(run)}
        aria-label={`${publishStateText(intent.state)} · ${plainTitle(run?.snapshot.inputs ?? {}, intent.run_id)}`}
      >
        <div className="wa-card-meta">
          <span className="wa-status" data-tone={publishStateTone(intent.state)}>
            {publishStateText(intent.state)}
          </span>
          <span className="wa-kicker">{intent.channel}</span>
        </div>
        <strong>{plainTitle(run?.snapshot.inputs ?? {}, intent.intent_id)}</strong>
        <span className="wa-kicker">
          {new Date(intent.due_epoch * 1000).toLocaleTimeString(undefined, {
            timeZone: intent.timezone,
            hour: "2-digit",
            minute: "2-digit",
            hour12: false,
          })}{" "}
          {intent.timezone}
          {unresolvedFlag ? " · linkage unavailable" : ""}
          {intent.refusal ? ` · ${intent.refusal.code || "refused"}` : ""}
        </span>
      </button>
    );
  };

  const runCard = (run: WorkspaceRun) => (
    <button
      key={run.id}
      type="button"
      className="wa-post-card wa-cal-card"
      data-tone="muted"
      onClick={() => onOpenRun(run)}
      aria-label={`Unplanned · ${plainTitle(run.snapshot.inputs, run.id)}`}
    >
      <div className="wa-card-meta">
        <span className="wa-status" data-tone="muted">Unplanned</span>
        <span className="wa-kicker">{run.state}</span>
      </div>
      <strong>{plainTitle(run.snapshot.inputs, run.snapshot.workflow.title)}</strong>
      <span className="wa-kicker">No publish intent — no planned date</span>
    </button>
  );

  if (scoped.loading) {
    return (
      <section className="wa-panel wa-stack">
        <h2>Schedule</h2>
        <p className="wa-empty" role="status">Loading planned posts…</p>
      </section>
    );
  }
  if (scoped.loadError !== null) {
    return (
      <section className="wa-panel wa-stack">
        <h2>Schedule</h2>
        <p className="wa-alert" data-tone="fail" role="alert">
          Could not load the schedule: {scoped.loadError}
        </p>
        <div>
          <Button onClick={() => setRefetchKey((k) => k + 1)}>Retry</Button>
        </div>
      </section>
    );
  }
  if (!projection) return null;

  return (
    <section className="wa-panel wa-stack">
      <div className="wa-row">
        <h2>Schedule</h2>
        <span className="wa-kicker">
          {contextId ? "Selected context" : "No brand context"} · local plans
          {canWrite ? "" : " · read-only"}
        </span>
      </div>
      {atCap && (
        <p className="wa-alert" data-tone="warn" role="status">
          Schedule history may be incomplete (up to {LIST_CAP} records shown).
        </p>
      )}
      {projection.conflictingIds.length > 0 && (
        <p className="wa-alert" data-tone="fail" role="alert">
          {projection.conflictingIds.length} publish intent id
          {projection.conflictingIds.length === 1 ? "" : "s"} appear more than
          once and were withheld — the schedule was not guessed.
        </p>
      )}
      {projection.days.length === 0 && projection.malformed.length === 0 && projection.unplanned.length === 0 && projection.conflictingIds.length === 0 ? (
        <p className="wa-empty" role="status">
          No posts are planned. Accepted text stays in Library until it is
          scheduled; runs without a publish intent have no planned date.
        </p>
      ) : (
        <div className="wa-cal-agenda" role="region" aria-label="Planned posts by day">
          {projection.days.map((day) => (
            <section className="wa-cal-day" key={`${day.date}::${day.timezone}`} data-date={day.date}>
              <header className="wa-cal-head">
                <span className="wa-kicker">{dayLabel(day.date)}</span>
                <strong>{day.date.slice(8)}</strong>
                <span className="wa-status">
                  {day.intents.length} {day.intents.length === 1 ? "post" : "posts"}
                </span>
              </header>
              <HorizontalStrip label={`Posts planned for ${day.date}`} className="wa-cal-posts">
                {day.intents.map(intentCard)}
              </HorizontalStrip>
            </section>
          ))}
          {projection.malformed.length > 0 && (
            <section className="wa-cal-day wa-cal-malformed">
              <header className="wa-cal-head">
                <span className="wa-kicker">Unresolved</span>
                <strong>!</strong>
                <span className="wa-status" data-tone="fail">{projection.malformed.length}</span>
              </header>
              <div className="wa-cal-posts wa-cal-malformed-list">
                {projection.malformed.map(({ intent, reason }) => (
                  <div key={intent.intent_id} className="wa-post-card wa-cal-card" data-tone="fail">
                    <div className="wa-card-meta">
                      <span className="wa-status" data-tone="fail">Unresolved plan</span>
                      <span className="wa-kicker">{intent.intent_id}</span>
                    </div>
                    <strong>{intent.run_id}</strong>
                    <span className="wa-kicker">{reason}</span>
                  </div>
                ))}
              </div>
            </section>
          )}
          {projection.unplanned.length > 0 && (
            <section className="wa-cal-day wa-cal-unplanned" data-unplanned="true">
              <header className="wa-cal-head">
                <span className="wa-kicker">—</span>
                <strong>Unplanned</strong>
                <span className="wa-status">{projection.unplanned.length}</span>
              </header>
              <HorizontalStrip label="Runs with no planned date" className="wa-cal-posts">
                {projection.unplanned.map(runCard)}
              </HorizontalStrip>
            </section>
          )}
        </div>
      )}
    </section>
  );
}
