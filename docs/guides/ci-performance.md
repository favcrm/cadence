# CI wall-time: the measured budget and the levers

Learned 2026-10-17 on `favcrm/cadence` (merge-group ~14.6m → ~4.5m) and
`agenticos-stack/agenticos-v2` (~7.5m → ~5m). Every number below is from a
real run log, not an estimate.

## 1. Measure before touching

Shard wall decomposes into three disjoint costs — the fix lives in exactly
one of them, and guessing wrong wastes a review cycle:

| Cost | Symptom | Tool |
|---|---|---|
| Dep compile | many `Compiling <dep>` lines despite a cache hit | job log grep `Compiling` |
| Workspace compile | `Compiling cadence-agent` once, multi-minute | same |
| Test exec | `Summary [ Ns ]` in the test line | same |

Real numbers on cadence's shard 2 (warm run 39014481055):

- workspace compile: **2m19s** (`Compiling cadence-agent` once)
- test exec: **73s** (599 tests)
- total shard wall: **~4min**

On the *stale-cache* run the same shard compiled **239 units** — the entire
dep graph rebuilt because restored artifacts fingerprint-missed.

Test duration includes fixture teardown. CAD-1556 found the CAD-1482 media
refusal fixture joining a provider thread that only stopped at its 60s safety
deadline. Connecting the existing teardown stop signal reduced the isolated
test from 60.35s to 0.85s, with all acceptance assertions unchanged. The 60s
deadline and 200ms receive bound remain. Maintenance of this protected check
was explicitly operator-authorized and still needs independent review.
This is test runtime evidence, not a measured whole-CI saving.

## 2. The rust-cache staleness trap (the real bug)

`Swatinem/rust-cache` **never overwrites an existing key**. The post-action
logs `Cache up-to-date` and moves on. Consequence:

> Flip a fingerprint-bearing flag (linker `-C link-arg`, `RUSTFLAGS`,
> toolchain bump, feature set) and the warm leg builds fresh artifacts that
> never save — every consumer then restores *stale* artifacts and rebuilds
> the dep graph from scratch. The cache-hit line lies.

The fix, shipped as CAD-1493: **version the shared key with an epoch
suffix** (`gate-test` → `gate-test-ld1`). Bump `-ldN` whenever a
fingerprint-bearing flag changes, so the warm leg writes a fresh entry
instead of skipping a stale one. The comment in `ci.yml` (around the
`shared-key` lines) states the rule.

To *force* a re-warm manually: delete the entry via
`gh api -X DELETE repos/<owner>/<repo>/actions/caches/<id>` — find the id
with `?key=v0-rust-<key>` — then let the next main push re-write it.

### Native cross-build: warm both profiles (CAD-1551)

`cache-warm-cross` prepares both the CAD-1425 acceptance with `test-seam`
and the default `cargo build --locked`. The test profile's dependency
optimization differs from dev, so a test-only warm cache does not cover the
second build. Producer and restore-only consumers share
`cross-dual1-${{ matrix.target }}`; bump that epoch together when changing
the warm inputs. Failed warm jobs do not save partial entries under the new
immutable key. Main remains the only writer; native tests and builds still
run on PRs and merge groups.

On 2026-10-18, 25 successful PR/merge-group runs showed the cross-build
aggregate finishing last in 22 cases. The macOS acceptance and default-build
steps had medians of 147s and 138s respectively. In
[run 39098521591](https://github.com/favcrm/cadence/actions/runs/39098521591),
the default build recompiled third-party dependencies despite a full cache
hit and took 2m10s. This is the baseline, not a claimed improvement.
The introducing PR uses a cold epoch; assess the benefit only after main
has saved both profiles and a later native consumer restores that entry.

## 3. sccache scope — build/ui only, not shards

`cache-workspace-crates: false` on rust-cache, because workspace-source
hashes change every merge (restoring them always misses). Workspace objects
go through sccache instead — **but only where the job enables it**:

- `cache-warm` legs run `READ_WRITE` under `sccache-writer` on main pushes.
- `build` and `ui` gate jobs run `READ_ONLY` via `ci-sccache enable`.
- **`test-shards` do not enable sccache** — they restore rust-cache, enable
  the linker, and invoke the test runner directly. Their ~2m19s
  `cadence-agent` compile is the raw workspace-crate codegen+link floor.

So the shard diagnostic is: if `cadence-agent` compiles once and takes
~2min on a warm dep cache, that is expected — sccache never entered the
shard path. Adding sccache to shards would help only for merges that don't
touch the crate (rare); not worth the credential surface.

Prefix is `v1/rustc-<toolchain>/` — a rustc bump re-zeros sccache too.

## 4. Shard count vs. isolate serialism

More shards help **only** when the suite is embarrassingly parallel at the
file level and the parallelism limit is the leg count, not per-process
constraint.

- **cadence**: 6 nextest shards — won ~4m per run (the shard already had
  `--test-threads`; more legs split the wall).
- **agenticos-v2 api**: vitest with `--maxWorkers=1 --no-file-parallelism`
  because each file owns a Miniflare isolate — the *files* are still
  parallelizable, so 4→8 shards halved the leg (405s → ~200s).
- **agenticos-v2 tooling**: 24 `node --test` files serially. Splitting into
  two concurrent processes gained **~15s** — the suite is serial-`spawn`-bound
  (each file waits on its own child), so process-count isn't the bound.
  Reverted. This is the failure mode to remember: *parallel processes only
  help when the work is CPU- or file-count-bound, not when each test blocks
  on its own child.*

## 5. Fixed waits vs. deadline bounds

A deadline is a failure bound, not a duration to spend. For example,
nextest's `slow-timeout = 60s` with `terminate-after = 3` permits up to
180s before termination; a test completing in 26s does not wait another 154s.

Synchronize on the operation being proved:

- Readiness and cleanup: wait for health, socket/pipe readiness, process exit,
  or the relevant durable state; short sleeps inside these loops only throttle
  observation. A fixed sleep before inspecting state is not completion evidence.
- Background work: use fixture-scoped completion events. The test-seam
  `daemon::test_events` subscriptions are registered before the triggering action;
  worker exit is observed after registry cleanup and the respawn decision.
  Monitor events follow completed revalidation, never merely a started pass.
  When proving fresh passes, discard one that might already have been in flight.
- Interleavings: park at an explicit barrier and release it after the competing
  operation, rather than hoping a 3s pause covers the race.
- Contention: wait for the first failed native flock/SQLite acquisition,
  not a contender thread starting. Unit-test observers are thread-local and
  compiled out of normal builds. The opt-in SQLite observer keeps the native
  lock and the existing busy bound; ordinary connections keep their handler.
- Shared fetches: park the chosen fetch, observe its actual joiners, perform
  the competing write/subscription, then release it. Fixture mtimes advance
  explicitly when the behavior depends on filesystem timestamp ordering.
- Logical age and timestamp boundaries: seed aged records or advance an
  isolated test clock. Review report naming has a `test-seam`-only
  `CADENCE_TEST_REVIEW_REPORT_EPOCH`; the publisher acceptance injects its clock
  only inside its isolated Python subprocess.
- Keep real elapsed/deadline assertions and bounded negative-observation
  windows. Late detached spawns, unsolicited worker restarts, held locks and
  slow socket traffic need an exclusion interval; deleting it would weaken
  the claim. A response channel can fail immediately if the forbidden event
  arrives. Never replace completion with a global counter from another fixture.

These are synchronization changes, not evidence of a whole-CI speedup.

## 6. agenticos-v2 specifics

- Merge protocol: staging → main via a promotion PR gated on `staging-proof`.
  Solo-operator: the gate is the merge requirement.
- `tools/ci/select.mjs` decides which jobs run per diff; `gate` needs the
  union, job names not counts. A matrix fan-out inside `api` is safe.
- `runtime-image-state-bridge.test.mjs` asserts a heartbeat *late* by
  ≥1000ms — flakes under shared-CPU. Not a port issue; a timing tolerance.
  Running it in a parallel half surfaced the flake.

## 7. The checklist

When a CI run is slow:

1. Pull the slowest job's log; split its wall into dep compile / workspace
   compile / exec / overhead. The fix lives in exactly one.
2. If deps recompile despite a cache hit → the cache is stale under a
   fingerprint flip → bump the `-ldN` epoch (or delete the key).
3. If the workspace crate is the wall → on `build`/`ui` check `Cache hits
   (Rust)`; on `test-shards` that metric doesn't exist — the job has no
   sccache, so ~2min workspace compile is the floor, not a cache miss.
4. If exec is the wall and the suite is per-file serial → more shards.
   If it's serial-*spawn*-bound → more processes don't help.
5. Never delete a check to speed up CI. The audit found zero true
   duplicates — every literal-assert was a contract string.

## 8. What's left (deliberately not pursued)

- cadence's crate split: ~30-90s/shard only on merges that don't touch
  `src/daemon/`. Most merges do, so the split pays little for its churn.
  Revisit if a clean boundary (platform/adapter out of daemon) emerges.
- agenticos-v2 `toolchain` (163s): one lint+format+tests step; a split
  buys ~30s net of a second pnpm install.
- agenticos-v2 `tooling` serial-spawn bound: parallelize *within* test
  files (faster spawn loops), not across processes.
