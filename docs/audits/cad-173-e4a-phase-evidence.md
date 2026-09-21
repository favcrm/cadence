# CAD-173-E4a routed PTY phase evidence

**Date:** 2026-09-21 (UTC)
**Implementation commit:** `ec3c107504aea712d3cd321194c0c277cf57f25f`
**Base:** `6279117b9378ba823ed4a0a6ef8ce3e6b052c156` (`origin/main`)
**Host:** `ip-172-31-1-32`
**Toolchain:** `rustc 1.98.1 (48a229cea 2026-09-01)`, `cargo 1.98.1 (797e8a9bc 2026-08-05)`

This evidence is for the test-only phase trace in `tests/integration.rs`. The
runtime, render deadline, retry count, retry wait, and all existing assertions
are unchanged. Each test was run five times in isolation, sequentially, with
`CARGO_BUILD_JOBS=4`. The commands were filtered `cargo test --test
integration <exact-test> -- --exact --nocapture` runs, so the integration
binary's full-suite host lock was not used and no full suite was started.

## Trace contract

The helper reads durable `messages.created`, `messages.completed`, and event
`at` values after the test's existing assertions. It emits one
`CAD173_E4A_PHASE` JSON record per test. `paste_not_rendered` is the completion
boundary of a render attempt. The first phase is routed-message enqueue to the
first attempt completion. Each later `retry_gap` is the interval from one miss
event to the next; it includes the retry wait and the following render attempt
because the production event stream has no attempt-start event. Park and
failed-state timestamps are separate. Missing timestamps become `null` in the
record rather than causing the regression to pass or fail for telemetry alone.

## Repeated measurements

| test | test wall time | enqueue → attempt 1 completion | attempt gaps 1–3 | enqueue → failed state | park → failed state |
| --- | ---: | ---: | ---: | ---: | ---: |
| `pty_unrendered_worker_result_requeues_then_parks` (5 runs) | 40.46–40.57s | 9.532–9.543s | 9.668–9.696s | 38.562–38.611s | 0.003–0.005s |
| `job_event_parks_on_unrendered_pty_pm` (5 runs) | 69.00–69.10s | 4.809–4.822s | 9.676–9.693s | 33.857–33.878s | 0.003–0.005s |

Every run reported four `paste_not_rendered` events, with attempts 1–3 marked
`retry: true` and attempt 4 marked `retry: false`; every run reported one
`delivery_parked` event and a final `failed` message. The PM remained alive in
both tests; the worker-result test then completed its existing successful
follow-up send, and the job-event test reached its existing idle assertion.

## Interpretation

The repeated 9.67–9.70s gaps are consistent with the existing 4s render
deadline, 300ms paste settle, and nominal 5s routed retry wait. This trace does
not claim to split those internal phases because no attempt-start event exists.
The worker-result path's first gap is consistently about 9.54s, while the
job-event path's first gap is about 4.82s. The source's actor loop has a real 5s
empty-queue wake fallback; the first-gap difference is evidence to inspect
whether the worker-result enqueue wakes an idle recipient differently from a
job-event enqueue. It is a candidate for a separate event-driven wake study,
not a production change in E4a.

The job-event test's 69s wall time versus 33.86s routed-delivery interval leaves
about 35s outside the traced delivery path. That remainder includes job/task
setup and final observation; this evidence does not assign all of it to one
phase. The worker-result test's 40.5s wall time versus 38.59s traced delivery
leaves about 2s for worker setup and the existing post-park successful send.

## Proposed next optimization

Instrument or review the worker-result enqueue wake path before changing any
deadline. A safe follow-up would prove whether an idle recipient is spending a
5s fallback wait after a durable routed message is created, then add an
explicit wake only if the existing queue/wake contract requires it. The next
review must preserve four attempts, three retry flags, parking, pane survival,
and the subsequent successful delivery. The stable 9.67–9.70s retry gaps do
not justify shortening production timing; fast feedback should address them
through the separately reviewed state-machine/test-budget work described in
the CAD-173 timer investigation.

No full suite, release build, workflow change, runner activation, merge, or
production state change was performed for this evidence.
