# Connected-platform contract fixture — v1

The machine-readable copy of [ADR 0006](../../../docs/adr/0006-connected-platform-contract.md)
§5.2/§5.4 — the contract two codebases implement: **Cadence** (CAD-366
credential proxy, CAD-506 effect gate) and **AgenticOS v2** (AOS-49
credential exchange, AOS-52 tool manifest). Both test suites consume
this directory so neither side drifts (§5.6).

## Contents

| File | What it is |
|---|---|
| `tool-table.schema.json` | JSON Schema (draft 2020-12) for an adapter's declared tool table: `{manifest_version, platform, tools: [{tool, effect, scopes, label?}]}`. `effect` is exactly `read`/`draft`/`send` (C1); the table is pinned to a manifest version (§5.2). |
| `pending-effect.schema.json` | JSON Schema for the durable pending-effect record (§5.4), including `preview`, `source_hash`, `label`, the `declined` terminal, `outcome.verified`, and the lifecycle's conditional fields. |
| `vector.schema.json` | JSON Schema for `vectors.json` — the shape of the worked vectors themselves. |
| `fake-tool-table.json` | The fake adapter's declared table — a concrete valid table both repos' fakes use. In Cadence it is embedded in `src/contract_fixture.rs` via `include_str!`, so the tested table is this file byte-for-byte. |
| `vectors.json` | The worked vectors: scenario scripts (`given` + `steps`) naming inputs and expected gate decisions / pending-effect states. |

## Versioning

The path is the version: `contracts/connected-platform/v<N>/`.

- Within a `vN` directory, changes are **additive or clarifying only**:
  new vectors, new optional fields, wording fixes. Anything already
  published keeps its meaning.
- A breaking change — a renamed field, a tightened enum, a changed
  semantic — ships as `v<N+1>/` alongside, so consumers pin a version
  and migrate deliberately.
- Consumers pin by path **and** by the `version` field inside
  `vectors.json` (currently `1`), which mirrors the directory number.

## How consumers use it

**Cadence** — `tests/contract_fixture.rs` loads this directory
(`CARGO_MANIFEST_DIR/contracts/connected-platform/v1/`), validates the
table and every vector against the schemas, asserts malformed tables
are really rejected, and checks each vector's expectations against the
contract's classification rules (C1–C3) and the C6 press-authority
invariant. A broken vector fails CI. Executing the vectors through the
real gate is CAD-506's job; the seam is the `step`/`expect` vocabulary
plus the fake platform (`src/contract_fixture.rs`,
`FakePlatform::standard()`).

**AgenticOS** (AOS-49/52) — vendor or fetch this directory at a pinned
cadence commit; treat it as immutable. Validate `vectors.json` and
`fake-tool-table.json` with any draft-2020-12 validator (e.g. Ajv), then
drive your own fake adapter through the same steps and assert the same
`expect` vocabulary. Your tool manifest's classes map onto this
vocabulary per §5.2 (`read`→`read`; `generate`/reversible `operate`→
`draft`; `send`/`spend`/`deploy`→`send`); keep the platform class in
`label`.

## Vector semantics

Each vector is self-contained: `given` describes the world, `steps` are
the actions, every step's `expect` asserts what an observer sees after
it. The DSL, in full:

### `given`

- `tool_table` — `"default"` for `fake-tool-table.json`, or an inline
  table object (which may be deliberately malformed).
- `table_valid` — default `true`. When `true` the table must validate
  against `tool-table.schema.json`; when `false` it must fail. The
  harness asserts the direction both ways, so a vector's claimed
  malformation is itself checked.
- `reported_manifest_version` — the manifest version the *platform*
  reports now. `null`/absent means the platform declares none. This is
  never a call argument — the call cannot assert the version, only the
  platform can report it (an agent input would always claim the match).
  When omitted entirely the table's pinned `manifest_version` is
  reported.
- `agent`, `account` — the calling alias and enrolled account handle
  the record carries.
- `sources` — reviewed source artifacts (name → content) the platform
  holds; a send pins one via `arguments.source` and its sha256 becomes
  the row's `source_hash`.
- `adapter` — fake-adapter knobs: `read_back` (`verify` | `mismatch` |
  `unknown`) and `fail` (`{tool, error}` — the platform call errors).

### `steps[].action`

- `call {tool, arguments?, handle?}` — an agent tool call. `handle` is
  the caller-named request handle; a repeat dedupes (`existing`).
- `press {decision: accept|decline, by: {member, role, rule},
  reason?, crash_after_decision?}` — a human decision on the pending
  effect. `crash_after_decision: true` records the decision durably
  then simulates the daemon dying before execution/outcome — the
  restart-inside-the-window case of §5.4 step 8.
- `edit_source {source, content, uncaught?}` — edit a reviewed source
  artifact. `uncaught: true` means the waiting-row cancel scan missed
  it, so only the re-check inside Execute can still catch it.
- `restart` — daemon restart: `waiting` rows re-park; a
  `decided`(accept) row without a proven outcome reconciles — never
  re-fires automatically.
- `caller_deadline` — the caller's wait deadline fires; it ends only
  the wait, never the durable row.

### `steps[].expect` (all keys optional)

- `result` — `executed` | `staged` | `existing` | `error` (call steps).
- `press` — `applied` | `refused` (press steps).
- `effect_id` — the staged result carried an `effect_id`.
- `state` — the pending-effect row's state after this step, or `null`
  for "this step produced no row".
- `close_reason` — the reason named on a `closed` row
  (`source_changed`, …).
- `fired` — this step caused platform traffic for the staged call.
- `executions` — cumulative platform executions for the effect; the
  exactly-once counter.
- `verified` — `outcome.verified`: `true` | `false` | `"unknown"`.
- `needs_you` — a Needs-you item was raised.
- `delivered` — `"message"`: the outcome arrived as a message to the
  task or PM, whether or not anyone was waiting.
- `wait` — `"ended"`: a caller's wait ended (row persists).
- `record` — a pending-effect record specimen; consumers validate it
  against `pending-effect.schema.json`.

The behavioural `expect` keys — `result`, `press`, `fired`,
`executions`, `verified`, `needs_you`, `delivered`, `wait`, `state`,
`close_reason`, `effect_id` — are asserted by CAD-506 when it executes
the vectors against the real gate. This directory's own check
(`tests/contract_fixture.rs`) proves only what data can prove: schema
conformance, classification coherence (C1–C3, C6), and that specimen
hashes match the declared source bytes.

### The scenario list and what each proves

| vector | contract |
|---|---|
| `declared-read-executes` | a `read` executes at call time; no row (§5.2) |
| `declared-draft-executes` | a `draft` executes at call time; no row (§5.2, Q3) |
| `undeclared-tool-parks-as-send` | absent from the table ⇒ gates as send (C3) |
| `unknown-effect-parks-as-send` | effect outside the vocabulary ⇒ send (C1, C3) |
| `missing-effect-parks-as-send` | absent `effect` field ⇒ send (C3) |
| `manifest-version-mismatch-parks` | platform reports a different manifest ⇒ even a read parks (§5.2) |
| `manifest-version-undeclared-parks` | platform reports no manifest ⇒ send (§5.2) |
| `effect-argument-ignored` | `arguments.effect` is ignored in both directions (C2) |
| `send-stages-executes-on-accept` | `staged` + `effect_id`, handle dedupe, accept executes once, `verified:true`, outcome as message, second press refused (C5, C6, C9, C10) |
| `non-operator-accept-refused` | release is operator-only: PM and requesting-agent accepts are refused and leave the row `waiting`; the operator's accept still executes once (C6, §5.4 step 4) |
| `agent-decline-allowed` | a PM that is itself an agent may decline — decline is not release (C6, §5.4 step 4) |
| `decline-never-fires` | decline parks the reason, nothing executes; `declined` is terminal (§5.4) |
| `source-edit-cancels` | editing a hashed source while waiting closes the row `source_changed` (§5.4 step 3) |
| `press-rechecks-source` | Execute re-verifies `source_hash`; a missed edit still cancels on press (§5.4 step 3) |
| `verified-false-raises-needs-you` | read-back mismatch ⇒ `verified:false` + Needs-you; row is `done` (C10) |
| `execute-fails-failed` | platform error on the accepted send ⇒ `failed`, distinct from `declined`/`closed` (§5.4 step 6) |
| `restart-reconciles-accepted-never-refires` | restart with accept-but-no-outcome ⇒ `reconcile`, never re-fires (C7, §5.4 step 8) |
| `restart-reparks-waiting` | `waiting` rows re-park; handle dedupe survives restart (§5.4 step 8) |
| `caller-deadline-ends-only-wait` | a caller deadline ends only the wait; the row stays `waiting` and a later press still executes (§5.4 step 7) |

## The fake adapter

`src/contract_fixture.rs` ships `FakePlatform`: the deterministic
platform-side double these vectors describe — this directory's
`fake-tool-table.json` as its declared table, a recorded execution log
("did it fire?"), idempotency-key dedupe (C9), source artifacts with
sha256 hashes, and read-back/fault knobs matching `given.adapter`. No
network, no credentials. Cadence's proxy and effect-gate tests
(CAD-366/506) drive it; AgenticOS implements the same knobs.
