# First public release handoff (CAD-661)

This is preparation, not publication authorization. No tag or release was
created for this increment. Source, review, CI, installation evidence and
release approval must be recorded before publication.

## Candidate assessment

The lane started from main `4db1359b1f2d2adab790694818c8fb9fcbe7a7b4` with
Cargo version `0.1.0`. This is an assessment baseline, **not the approved release
candidate**. Select a fresh reviewed exact main commit after release blockers
are resolved. Unmerged PRs and draft issuer contracts are not shipped capabilities.

Recommend an explicitly labeled pilot prerelease for the first public install.
The existing gate requires the tag to equal `v` plus Cargo.toml's exact version:
`v0.1.0-beta.1` therefore needs a reviewed Cargo.toml/Cargo.lock version change
before tagging. The current Cargo version permits only `v0.1.0`, which GitHub
would treat as stable. Do not create a prerelease tag against mismatching source
or advertise cloud production readiness based on a local CLI pilot.

## Outstanding publication evidence

| Requirement | Current owner / next evidence |
|---|---|
| Exact reviewed main SHA and version | Release PM selects after blockers; copy SHA from git/gh |
| Explicit required gate results | CAD-420; skipped/failed jobs cannot count as passing |
| Clean supported-platform installs | CAD-317, using actual published assets after publication |
| Customer public upgrade channel | CAD-661/CAD-561 coordination; currently internal CI channel |
| Final release approval | `release` environment currently names `cc-syntax` |

On 2026-09-27, the live `release` environment required approval by `cc-syntax`;
the active “release tags” ruleset (23893821) restricted creation/update/deletion
of `v*` tags to its administrator bypass role. Re-read these settings before
publication and use the configured approval gate. Do not bypass it. The
workflow also expects immutable published releases; verify the repository's
immutability setting before the first tag rather than relying on a workflow comment.

CAD-420 hardens evidence reuse: the latest-attempt jobs inventory is paginated,
each required name must exist, and every job with that name must be completed
and successful. Successful matrix/name collisions remain valid; any failed,
cancelled, skipped or unfinished collision refuses reuse and runs the gates here.
API/pagination errors also fall back to running the gates.

The tag-only release-gate uses `always()` to evaluate upstream results instead
of inheriting the default success/skip behavior. It records each result and
refuses release builds unless fmt, clippy, test, build and ui all succeeded.
This is an explicit scheduling/result policy, not a guarantee that GitHub can
start a job after an entire run is externally cancelled. See GitHub's
[status check functions](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions#status-check-functions).

Required check names do not establish that the workflow itself is trustworthy.
Review changes to this workflow and its evidence predicates independently;
these checks do not implement CAD-120's trusted external QA boundary or prevent
an authorized workflow change from replacing a gate with a weaker job.

## Release notes to complete for the approved candidate

Use the following content in the release draft, replacing placeholders with
verified evidence before the environment approval. The workflow can reuse an
existing draft, upload the artifacts and publish after approval.

```text
Cadence <version> — local CLI pilot

Native CLI and embedded board for local agent coordination. Supported assets:
Linux x86_64, Linux ARM64 (glibc 2.35 baseline), Apple Silicon macOS.
No Intel macOS, Windows or musl package.

Provider CLIs/authentication and tmux for terminal providers are separate.
List only provider/platform combinations actually verified for this candidate.
Hosted browser login, remote local worker teams and local-to-cloud migration
are not claimed operational by this release unless exact-candidate acceptance
evidence establishes them. Public stable automatic upgrades are not yet verified.

Source SHA: <copied exact SHA>
Tag: <exact Cargo version prefixed with v>
CI run and all required gate conclusions: <links/results>
Clean installation evidence: <platform links; distinguish pre/post-publication>
Installer and provenance guide: <tag-pinned docs/INSTALL-AGENT.md URL>
Known limitations: <candidate-specific issues>
Publisher: <identity>; release environment approval: <receipt>
```

Do not approve with placeholders remaining. Generated GitHub changelogs alone
are not sufficient capability or compatibility notes.

## Publication and verification

1. Record the reviewed candidate SHA, matching version/tag, relevant reviews
   and successful gates; inspect the exact tagged workflow.
2. Assemble candidate notes and prepublication install evidence, confirm tag
   protection, environment approval and release immutability. The authorized
   publisher creates the tag; no production daemon rollout follows implicitly.
3. The tag workflow reruns gates, builds the UI-embedded binaries on each target,
   checks reported version/SHA and publishes through the `release` environment.
   The final approver verifies the draft notes and actual gate/build results.
4. Record tag, source SHA, run URL/attempt, release URL, publisher and approval.
   Assets must be exactly `install.sh` and each target's
   `cadence-<tag>-<target>.tar.gz` plus matching `.tar.gz.sha256`.
   Each archive contains `cadence`, `cadence.sha256`, `manifest.json`.
   Record archive and installer digests and provenance verification results.
5. CAD-317 verifies the **public URLs** on clean supported systems, without Rust,
   Node, GitHub login or AgenticOS login. Record `--version`, default/custom
   prefix, PATH, rerun and explicit version selection. Local mirror fixtures
   remain useful regression evidence, not public delivery proof.
6. Keep corruption/missing asset, unsupported platform and wrong-version refusal
   evidence. Never test against the production HOME or installed executable.
7. For a prerelease, share the explicit tag URL; `releases/latest` is valid only
   after a stable release exists. Update README's publication status only after
   actual delivery succeeds. Failed postpublication verification requires a
   documented withdrawal/replacement version, never mutating published bytes.

Release publication, installing a build, migrating organization state and
rolling out a production daemon are separate operations with separate evidence.
