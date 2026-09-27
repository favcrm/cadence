# First public release handoff (CAD-661)

This is preparation, not publication authorization. The immutable
`v0.1.0-beta.1` tag failed its macOS UI build and retains a private draft;
it has no published installation assets. This replacement prepares beta.2.
Source, review, CI, installation evidence and release approval must be
recorded before publication.

## Prepared version; candidate selection pending

Cargo.toml and Cargo.lock are prepared as **0.1.0-beta.2**, with matching planned
tag **v0.1.0-beta.2** and release title **Cadence 0.1.0-beta.2 — local CLI pilot**.
No beta.2 release candidate SHA is selected and no beta.2 tag or release is
created. Select a fresh reviewed exact main commit only after the version/source
prerequisites
land and the release evidence is accepted. A preparation PR head is not the
publication candidate. Unmerged PRs and issuer contracts are not shipped
capabilities.

The tag gate requires `v` plus Cargo.toml's exact version. The publisher sets
`prerelease=true` and `latest=false` for the pilot on both new and reused drafts;
stable versions use explicit false/true respectively. Classification reads the
prerelease portion before any build metadata, so a build-metadata hyphen alone
does not imply prerelease. Already published releases are never edited on a
rerun: differing prerelease classification or asset bytes refuse the rerun.
The workflow does not promote an existing published version to latest on a
rerun. See the GitHub CLI [create](https://cli.github.com/manual/gh_release_create)
and [edit](https://cli.github.com/manual/gh_release_edit) flags. A beta label
does not advertise cloud production readiness.

## Outstanding publication evidence

| Requirement | Current owner / next evidence |
|---|---|
| Exact reviewed main SHA and version | Release PM selects after blockers; copy SHA from git/gh |
| Explicit required gate results | CAD-420 source merged; actual candidate/tag run evidence still required |
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

The existing macOS cross-build job also installs frozen UI dependencies and
builds the UI on PRs and merge groups. This job is advisory under current branch
protection; beta.2 publication additionally requires its actual macOS UI build
to pass. The static module-name check runs in the existing required UI build.

## Proposed local pilot notes

Cadence 0.1.0-beta.2 is a local CLI pilot: a native controller and embedded
browser board for local project and coding-agent coordination. Evaluate on
fresh, separate local state; existing production-state compatibility and schema
rollback are not established by a clean install.

Planned packages are Linux x86_64, Linux ARM64 (glibc 2.35 baseline) and Apple
Silicon macOS. Intel macOS, Windows and Alpine/musl packages are not provided.
Daemon and board evaluation is Linux-only with fresh, separate state. Apple
Silicon macOS is limited to CLI packaging, installation and version checks;
setup, daemon and board use is unsupported pending CAD-315.
Real tagged-archive/platform installation results remain pending. Provider
CLIs/authentication and tmux for terminal providers are separate prerequisites;
list only combinations actually verified for the eventual candidate.

Browser/token support is issuer-client preparation, not working public cloud
login, enrollment or assignment delivery. Local offline outbox persistence is
a foundation, not proof of remote transport, applied receipts or sleep-safe
delivery. Hosted remote teams and local-to-cloud production migration remain
separate acceptance work. The public stable-release updater is not implemented;
current update/upgrade commands use the authenticated internal CI channel.

For the pilot use its explicit published tag, never stable `latest` convenience
installation. Selecting another installer version is not a coordinated running
daemon upgrade or compatible state rollback. No download link below is claimed
available until the release exists and public installation is verified.

## Evidence annex to complete for the approved candidate

Use the following content in the release draft, replacing placeholders with
verified evidence before the environment approval. The workflow can reuse an
existing draft, upload the artifacts and publish after approval.

```text
Cadence 0.1.0-beta.2 — local CLI pilot

Native CLI and embedded board for local agent coordination. Supported assets:
Linux x86_64, Linux ARM64 (glibc 2.35 baseline), Apple Silicon macOS.
No Intel macOS, Windows or musl package.
Daemon and board evaluation: Linux only, using fresh, separate state.
Apple Silicon macOS: CLI packaging, installation and version checks only;
setup, daemon and board use unsupported pending CAD-315.

Provider CLIs/authentication and tmux for terminal providers are separate.
List only provider/platform combinations actually verified for this candidate.
Hosted browser login, remote local worker teams and local-to-cloud migration
are not claimed operational. Public stable-release updater is not implemented.

Source SHA: <copied exact SHA>
Tag: v0.1.0-beta.2 (planned, not created)
Classification: prerelease=true; latest=false (must verify actual release metadata)
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
   The final approver verifies the draft notes, prerelease/latest classification
   and actual gate/build results. Synthetic workflow-policy tests do not prove
   the real tag event, API side effects or release metadata.
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
