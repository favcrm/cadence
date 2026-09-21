# CAD-173-E4a routed PTY phase evidence

**Date:** 2026-09-21 (UTC)
**Implementation commits:** `ec3c107504aea712d3cd321194c0c277cf57f25f`, `19f7ce746a8437ff856dc925fd683348aeae3acd`
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
`CAD173_E4A_PHASE` JSON record per test. `submitting` is the durable attempt
start, and `paste_not_rendered` is its completion. The next `submitting` event
is the observable retry wake; there is no separate wake event in the
production stream. This separates enqueue-to-start, render duration, retry
wait, park, and failed-state phases without charging the test's 50ms RPC
polling to them. Missing timestamps become `null` in the record rather than
causing the regression to pass or fail for telemetry alone.

## Repeated measurements

| test | test wall time | enqueue → attempt 1 start | render attempts 1–4 | retry waits 1–3 | enqueue → failed state | park → failed state |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `pty_unrendered_worker_result_requeues_then_parks` (5 runs) | 40.43–40.63s | 4.861–4.872s | 4.655–4.674s | 5.004–5.011s | 38.537–38.574s | 0.001–0.004s |
| `job_event_parks_on_unrendered_pty_pm` (5 runs) | 68.71–69.00s | 0.143–0.144s | 4.656–4.672s | 5.009–5.012s | 33.808–33.850s | 0.002–0.005s |

Every run reported four `submitting` starts and four `paste_not_rendered`
events, with attempts 1–3 marked `retry: true` and attempt 4 marked
`retry: false`. Every run reported one `delivery_parked` event with
`attempts=4` and a final `failed` message with `via=pty_render_miss`. The PM
remained alive in both tests; the worker-result test then completed its
existing successful follow-up send, and the job-event test reached its
existing idle assertion.

## Interpretation

The 4.655–4.674s render durations are consistent with the existing 4s render
deadline plus 300ms paste settle and capture/process overhead. The 5.004–5.012s
retry waits match the existing nominal 5s routed retry wait. The worker-result
path spends 4.861–4.872s between durable enqueue and its first `submitting`
start, while the job-event path starts within 0.143–0.144s. The source's actor
loop has a real 5s empty-queue wake fallback; the first-start difference is
evidence to inspect whether worker-result enqueue wakes an idle recipient
differently from job-event enqueue. It is a candidate for a separate
event-driven wake study, not a production change in E4a.

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
and the subsequent successful delivery. The measured 5.004–5.012s retry waits
do not justify shortening production timing; fast feedback should address
state-machine/test-budget work through the separately reviewed changes
described in the CAD-173 timer investigation.

No full suite, release build, workflow change, runner activation, merge, or
production state change was performed for this evidence.
