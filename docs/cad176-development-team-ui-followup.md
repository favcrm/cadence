# CAD-176 development-team follow-up: accountable local monitor inbox

This increment closes the first visible part of the development-team loop:
the existing daemon monitor state and durable alerts are shown on the default
Overview page, with a guarded UI acknowledgement route. It does not change
approval policy, dispatch authority, external delivery, merge, deployment, or
the monitor worker's store contract.

## Acceptance checklist

The review starts with this checklist. A passing implementation must show all
of the following on a temporary state directory and after a board reload:

- a registered monitor's `active`, `degraded`, `stale`, `off`, or
  `unavailable` state, with the reason when it cannot reconcile;
- the latest completed scan from `last_success_at`, separately from the
  heartbeat and next scheduled check;
- an overdue active monitor marked `stale`, so a stopped coordinator cannot
  look healthy forever from its last heartbeat;
- mixed monitor health keeps the aggregate `stale`/`degraded` result
  regardless of row order, and malformed monitor RPC rows fail closed as
  `unavailable` rather than being skipped;
- the daemon lifecycle accepts only `active`, `degraded`, or `off`; every
  `monitor_alerts` response validates its monitor, cursor, alert array, alert
  scope, fields, and `open`/`acknowledged`/`resolved` state. RPC failure, a missing or
  malformed response, or an unsupported state is `unavailable`; `alerts: []`
  remains a valid empty history;
- a concrete alert with project, task, event sequence, fingerprint, age,
  evidence, next action, next owner, and authority;
- the rendered monitor row shows heartbeat, last check, completed scan, and
  explicit task coverage, while the alert shows its project;
- receipt-only events remaining absent from the alert inbox (the existing
  monitor store tests cover this contract);
- one alert remaining one alert across repeated reads and acknowledgement
  moving its durable state to `acknowledged`;
- the acknowledgement refused by the board's existing read-only and
  same-origin write guards;
- external push explicitly reported as unconfigured, with no claim that the
  UI can wake an idle chat session; and
- the event stream refreshing monitor changes while the 30-second/focus poll
  remains the reconciliation fallback.

## What the repository supports now

`Store::check_monitor` already advances a durable event cursor and inserts a
deduplicated `(monitor, fingerprint)` alert in one transaction. The daemon
watch loop periodically checks registrations and records `last_success_at` or
an explicit degraded error. The monitor RPC exposes registration, coverage,
health, alert history, and acknowledgement. Those facts are the source of the
Overview projection in `src/overview.rs`; the UI does not open the live
SQLite database.

The new board path is deliberately local:

```text
daemon monitor_list + monitor_alerts
             │
             ▼
GET /api/overview → Overview / coordinator panel
             │
POST /api/monitors/<monitor>/alerts/<seq>/ack
             │
             ▼
daemon monitor_alert_ack → durable alert state
```

The SSE watcher fingerprints `monitor_list` and emits `monitoring` when
health, cursor, alert counts, or acknowledgement state changes. A client that
misses that event re-reads the same projection on focus or the existing
30-second poll. `ui_local_only` is a visibility surface, not a push provider.

## Development-team contract and remaining gap

The Overview now presents the accountable shape needed for the next
increments: work evidence, current state, next action, next owner, authority,
and age. The monitor owner is used only as the owner of monitor recovery;
`operator` and `reviewer` labels indicate the authority required by the alert
kind. No label claims that an action has already been dispatched.

The following parts of the broader team promise remain explicitly outside
this bounded patch and need a separately owned coordinator change:

- worker result → independent reviewer routing with delivery acknowledgement;
- reviewer failure → author revision, or reviewer success → Ops merge-ready
  handoff, both bound to the exact head and invalidated on a head change;
- ready-worker admission with role, reviewer capacity, provider allowance,
  memory, and build/test slot guards;
- event-triggered transition processing with periodic reconciliation,
  durable cursor/deduplication, crash lease recovery, retry/backoff, and
  one bounded human escalation for approval, credentials, quota, or
  inactivity; and
- acceptance-first issue dispatch without repeated unchanged full suites.

Those gaps are product requirements, not hidden UI behavior. Until their
coordinator exists, the only safe UI next action is an explicit inspection or
operator handoff. No author can self-approve, and no settings or protected
gate is bypassed.

## Ownership and safe rollout

- Tracker owner remains `CAD-176` / `luna-watchdog` for monitor worker and
  store behavior. This branch owns the Overview projection, UI route, UI
  event refresh, and board regression only; it does not edit `src/store.rs`.
- Independent QA must review the exact branch head after the focused checks.
  Ops decides build admission, daemon/UI pairing, restart timing, and any
  later merge or deployment. No merge or activation is implied by this
  document.
- The currently observed live daemon is an older binary (PID 320856) using
  `/home/ubuntu/.local/state/cadence`; the current UI is also an older
  process. No new binary was pointed at that database. A deployable rollout
  must first build the exact reviewed UI and daemon, exercise the checklist
  against a temporary state directory, then obtain the existing Ops restart
  authority. Before that pairing, the board truthfully reports monitor RPC
  `unavailable` when an old daemon lacks these methods.

Focused evidence for this branch:

```text
CARGO_BUILD_JOBS=4 cargo test --locked --lib overview::tests
CARGO_BUILD_JOBS=4 cargo test --locked --test board ui_overview_surfaces_durable_monitor_alert_and_acknowledges_it -- --nocapture
pnpm --dir ui typecheck
pnpm --dir ui build
cargo fmt --all -- --check
```
