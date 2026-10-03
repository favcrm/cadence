# CAD-1015 runtime guard evidence — implementation still incomplete

## Verified Pi runtime, not Cadence end-to-end

Runtime: Pi 1.0.0; provider extension pi-devin 0.2.1; model `devin/swe-2-high`.
Only newly owned temporary processes, directories and sessions were used.

Commands:

```sh
node --test tests/manual/native_inbox/pi_guard.test.mjs
CAD1015_DOGFOOD=1 python3 tests/manual/native_inbox/pi_guard_probe.py
```

Results: 11 fixture tests pass. Real-runtime report:
`/tmp/c1015-guard-jdov_2gt/report.json` (`status: observed`, owned process exit 0).

Observed:
- An exact bound active run receives guidance; original output is `BASELINE AMENDED_CAD1015`.
- Idle, foreign-generation, stale-turn and closing-turn input is refused without starting work.
- A second, awaited `turn_end` extension listener creates the real closing race; the first-loaded guard closes admission before that listener yields.
- Same-id/same-body repetition queues once. Same-id/changed-body is rejected in fixtures.
- An unused binding can be abandoned; another binding or an active run cannot.
- Accepted guidance belonging to a cancelled run is excluded from a successor's model context. The successor makes one model request, with no old guidance, and returns only `FOLLOWUP_CAD1015`.
- Only the verified Pi version registers native controls; an unverified version gets no such capability.

### Red/green and limitations

The cancellation fixture failed before context scoping was added. A real probe also exposed old guidance in successor model context even when the final output looked clean (`/tmp/c1015-guard-40izz_li/report.json`). Output matching alone was insufficient evidence.

The fixed probe observes model-context projections, not just final text. It clears the native queue after settlement, mirroring the adapter's required cleanup. Accepted guidance may be applied before an abort is processed; the test does **not** claim abort retrospectively retracts already applied input. Acceptance still does not prove application.

Mutation evidence: `/tmp/c1015-mut-ru21l4en/report.json`. Removing the exact-turn predicate makes the foreign-generation regression fail; removing context scoping makes the cancellation regression fail (both exit 1). Original source was not modified by these mutation runs.

## Codex negotiation observation

A new app-server process with an empty, isolated `CODEX_HOME` was initialized without opening a thread or making a model call. Its response was:

```json
{"userAgent":"cad1015-version-probe/0.160.0 (Ubuntu 24.4.0; x86_64) Orca/0.0.0-dev (cad1015-version-probe; 0.1.0)"}
```

The client name prefixes the server version. `clientInfo.version` is not server-version evidence. The adapter now requests the experimental API and grants steering only for the verified server version; other versions retain normal queued work. Existing native expected-turn probes are documented in `EVIDENCE.md`.

## Outstanding gates — no finished-feature claim

- Rust compilation, filtered unit/integration tests and clippy have not run for these runtime changes.
- Build-slot admission refused this session: caller descends from no registered pane or enrolled managed endpoint. Do not bypass that guard or launch a private competing resource scheduler.
- Lane-binary end-to-end dogfood, task objective/criteria composition, caller/HTTP adversarial cases, reconnect/restart and approval cases remain required.
- Claude strict input remains disabled; stream-json busy input is not evidence of an atomic expected-turn guard. A hook-only gate is insufficient: documented UserPromptSubmit timeouts let the prompt proceed.
- Unexpected source edits appeared in the lane outside this session's issued writes; reconcile ownership before concurrent integration changes.
- No PR, exact-head verdict, CI pass, operator approval or merge is claimed.
