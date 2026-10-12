# CAD-1159 fixture-admission acceptance — independently authored SPEC (not a verdict)

Status: SPEC ONLY. No build, run, key, credential, resource or provider call is
authorized or performed by this document. Frozen inspection head: lane
`/home/ubuntu/Project/cadence/.cadence/wt/cad-1159-aos159-paired-real-root-construc`
@ `4c67d334bdd82eedccfd45e28ec7d6f160b6f569`. Implementer must not edit or
weaken this spec. The S-VIEW-1 oracle at
`/home/ubuntu/Project/agenticos-stack/.tmp/hosted-collab-01a1161e/s-view-1-independent-check/S-VIEW-1-acceptance-recipe.md`
is preserved unchanged; this spec complements it and replaces neither it nor
full custody/family qualification.

## One outcome

A seam-free real release constructor refuses any fixture-selected proof/key and
any wrong artifact/root/caller input BEFORE effects, while the separately
labelled debug fixture build CAN authenticate an owner-elected fixture
descriptor through the SAME real `entry()` path — a derivative baseline, never
production qualification.

## Source anchors (as inspected)

- `src/installer_bundle/constructor/mod.rs`: `entry()` — `fixed_arguments()` →
  FIFO check on fds 0,1,2 (`S_IFIFO`) → `channel::Duplex` → `wire.receive()` →
  `authenticate_bootstrap` → `runtime::run`. `:61`
  `IMAGE_AUTHORITY_KEYS: &[ImageAuthorityKey] = &[]`; attestation lookup
  `:347`-ish rejects with "constructor image authority unconfigured or
  untrusted" before Ed25519 verify and before `runtime::run`.
- `src/daemon/installer_enrollment_wire.rs`: `PRODUCTION_TRUST_KEYS` empty
  (`:64`); `production_trust_set()` (`:127`) → `constructor::receipt_keys()`;
  `keys_from_qualified`; `TrustedKey::capture_test` is `#[cfg(test)]`-only.
- `src/daemon/supervisor_grant.rs`: `SUPERVISOR_KEYRING` empty (`:70`),
  `INSTALLER_EXE_DIGEST`/`INSTALLER_GENERATION_PIN` `None` (`:76,:80`),
  `enroll_self()` `Err`, `capture_test` `#[cfg(test)]`-only.
- `src/test_seam.rs:50` existing `compile_error!` for release+test-seam
  (unchanged). `src/bin/cadence-root-constructor/main.rs` →
  `lib.rs:87 root_constructor_entry()` (Linux x86_64 only).

## Cases

C1 compile exclusion (authorized isolated runner, after implementation):
- `--release --features native-fixture-admission` → FAIL, stderr contains
  `native-fixture-admission-requires-debug-test-seam`.
- `--release --features test-seam,native-fixture-admission` → FAIL, same marker
  present (existing test-seam error may also appear).
- debug `--features native-fixture-admission` without `test-seam` → FAIL,
  same marker.
- plain `--release` → builds; binary/strings contain no fixture-adapter marker
  or fixture root material (canary-style absence check).
- `--features native-fixture-admission` before declaration → Cargo
  unknown-feature error; never reportable as pass.

C2 runtime refusal (same runner; real `cadence-root-constructor` over actual
FIFO stdin/stdout carrying the full Configure/Duplex protocol; outside-guest
owner signer produces attestations):
- fixture-descriptor-signed attestation vs seam-free release binary → exit 1,
  refusal at the compiled-root lookup;
- unknown/random signer vs the fixture build → refused;
- tampered artifact digest/`source` pin/`manifest.image ≠ launch.request.image`
  → refused;
- attempted caller key injection via env, argv, extra JSON field
  (`deny_unknown_fields`), frame-embedded key, stdout text, PID or
  manufactured-rights proof → refused;
- non-FIFO stdio or argv → refused before any frame read.
For EACH: assert zero constructor effect — no layout/file creation, no child
process, no socket, no key operation — refusal precedes `runtime::run`.

C3 positive control: debug build with all of `debug_assertions`, `test-seam`,
`native-fixture-admission` and the owner-elected PUBLIC descriptor compiled
via `build.rs` → the SAME `entry()` authenticates a fixture-signed bootstrap
and reaches real post-auth staging. Records authentic owned baseline; NOT a
production/frozen-release claim (empty-roots release refusal stays expected).

C4 HTTP peer: none exists — no HTTP route carries constructor authority
(FIFO binary at `files.rs:158`). Do not invent one.

## BLOCKED / prerequisites

Currently `fixture_admission.rs`, `native_fixture_trust.rs`, the
`native-fixture-admission` feature, its `compile_error!`, and the `build.rs`
descriptor intake are absent → C1–C3 are BLOCKED, never fake-executable.

Evidence: exact head, commands, exit codes, marker text, absence-of-effects
listing. Execution limits, key owner, named authority: pending CON-002/DEP-003.
