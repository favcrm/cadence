# Cadence release and production handoff

Two different publication paths exist. A native host can take an approved
main-SHA build without a new public version tag. A numbered GitHub Release is
for public installation; it does not replace production-candidate promotion.
Neither path authorizes an agent to approve or restart production. Follow
[AGENTS.md](../AGENTS.md#production-safety) and the
[artifact checklist](guides/rollout-checklist.md); image-managed hosts use the runtime
image path instead of self-update.

## Stable v0.2.0 (published, immutable)

[Cadence 0.2.0](https://github.com/favcrm/cadence/releases/tag/v0.2.0) was
published as the stable GitHub Release on 2026-10-15 at 10:07:27 UTC
(`prerelease=false`, `latest=true`, `draft=false`, immutable). Its annotated tag
object is `fd4a5491775c3cc8f0ba9c4c542a540bbee885e1`, peeled to source
`361c118f76449317ad8edcc71126ed6572e00366`. The producing
[tag workflow run 38708389618, attempt 2](https://github.com/favcrm/cadence/actions/runs/38708389618)
completed successfully: all five tag gates, `release-gate`, three native release
builds and three exact-candidate native installation legs passed. Attempt 1
timed out after 15 minutes and is not success evidence; the single full guarded
retry made no source, tag, timeout or gate changes.

All nine published HTTPS assets were anonymously downloaded and their bytes
matched the exact candidate assets. GitHub attestation verification passed for
the installer, three platform archives and app-toolchain archive, bound to
repository `favcrm/cadence`,
`refs/tags/v0.2.0`, source digest above, and
`favcrm/cadence/.github/workflows/ci.yml`. The installer SHA-256 is
`c335171994e89da9d8d86b3cece9a7de8899a52056b6c42904a5f63dfa33c7e5`.
The public native clean-install workflow also passed on its first attempt in
[consumer run 38718119770](https://github.com/favcrm/cadence/actions/runs/38718119770),
from reviewed main source `d990f570d236b8b7643ae67c80b2cef83df0def7`. All three
actual published-URL legs (`x86_64-linux`, `aarch64-linux`, `aarch64-macos`)
validated the release tag, source, producing run/attempt, success receipt and
binary digests. Default/custom prefixes and reruns passed through the existing
harness. This proves anonymous public download and CLI installation only; it
does not complete CAD-317 setup/wizard/backup acceptance, claim a macOS daemon
or board, or establish production-host promotion or rollout.

The public release and its notes/assets are immutable. This record documents
the delivery without modifying those bytes or notes. Public release publication
is separate from native-host candidate staging, approval, promotion, installation
and operational verification; none follows implicitly from this release.

## Native host: checkpoints, not one “shipped” status

| State | Evidence to check before reporting it |
| --- | --- |
| Merged | PR merge commit is on main; queue acceptance is not merge proof. |
| Artifact ready | Exact main `ci.yml` push run and attested `cadence-<sha>-x86_64-linux` artifact; verify source SHA/run attempt/digest, not just a green job name. |
| Staged | Successful `stage` job and staging evidence for the selected artifact and installed baseline. |
| Promoted | Successful manual `staging.yml` dispatch, successful `promote` job after GitHub `production` approval, and matching `production-candidate.json` in the `production-candidate` artifact. |
| Installed | Operator update result and installed/running build match the intended SHA. |
| Operationally verified | Read-only health/status and affected behavior checked on that build; fixture QA is not live verification. |

Record existing run/receipt links and actual results on the ticket; no new
ledger is required. Reconcile the ticket after merge as usual, but do not
reinterpret `done` from merge evidence as installed or operationally verified.
Report any remaining operator rollout work explicitly.

### Prepare and promote the exact candidate

1. Select a successful **main push** CI run containing the intended merged
   changes and its attested artifact. A manual `ci.yml` dispatch is a benchmark
   and does not run the main-push artifact/attestation jobs.
2. Identify the installed baseline from operator-provided update/version
   evidence and its approved candidate receipt's `ci_run_id`. Do not assume
   the preceding main commit or the public `latest` release is that baseline.
3. Dispatch `staging.yml` from `main`, supplying `ci_run_id` and
   `baseline_ci_run_id`. Example syntax (replace both with verified run IDs):

   ```sh
   gh workflow run staging.yml -R favcrm/cadence --ref main \
     -f ci_run_id=<CANDIDATE_MAIN_CI_RUN> \
     -f baseline_ci_run_id=<INSTALLED_BASELINE_MAIN_CI_RUN>
   ```

4. Check the selected SHA/digest and staging outcome on that run. The operator
   approves the GitHub `production` job; agents may prepare/dispatch on request,
   but never record approval. Wait for `promote` to succeed and verify the
   receipt matches the candidate, CI run/attempt and promotion run/attempt.
   PR merge approval and GitHub production approval are different decisions.
5. Only then relay the two supported operator update commands from AGENTS.md.
   The operator runs the check first and proceeds only if it permits the
   intended target. Installation, drain, backup and health checks remain in
   the supported update flow, not a hand-built restart recipe.

### Bundled guidance on native update (CAD-1472)

`cadence update` is the normal path. It synchronizes bundled public guidance
for the build it leaves active, including an already-current binary and both
explicit and automatic rollback. The sync is non-force: only missing or
stale managed files and repairable links are written from that build's
embedded bytes, so a repeat run writes nothing and matching files keep
their timestamps. A build that predates `cadence skill sync` exposes only a
forced manual install, so its bytes are staged outside the home and applied
with those same non-force rules. `--check` and `update status` do not write,
and they do not claim the target build's guidance is verified. `cadence skill
install [name]` is only the manual repair. Updating the files does not reload
an agent session that is already open; start a fresh session or reread the
skill at a task boundary. Client-only update does not install guidance or
start a daemon. There is no setup refresh flag. A guidance failure does not
roll the binary back and does not restart a healthy daemon just to refresh
skills.

### Owned release source (CAD-1455; default under CAD-1479)

`staging.yml` retains the promoted release's evidence package
(`production-evidence-<sha>`) and packages the flat retained bundle as
`release-package-<sha>`; `promotion-completion.yml` re-proves the completed
promotion, signs the publication-time source facts (observed main head,
bounded ancestor list including the candidate, `SCHEMA_VERSION` at the
candidate) and attests a closed completion receipt binding them. When the
operator provisions the release bucket (endpoint/bucket as
`vars.RELEASES_R2_ENDPOINT`/`vars.RELEASES_R2_BUCKET`, access keys as
`secrets.RELEASES_R2_ACCESS_KEY_ID`/`secrets.RELEASES_R2_SECRET_ACCESS_KEY`
— the hosted jobs export them as `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`
environment fields; no AWS profile is created and no credential is an
argument), the same workflow publishes the merged package into the owned R2
catalogue with a conditional, immutable write — the catalogue record last.
A configured job that lacks the secrets fails closed; absent endpoint/bucket
configuration leaves the job inactive. The publisher is an automatic
immutable writer: an identical replay (same identity, facts and artifact
digests, differing only in its fresh write-time stamp) reuses the first
recorded catalogue entry untouched; anything else conflicts. Withdrawal is
not implemented by the publisher — a withdrawn record is an operator-owned
lifecycle action through the data plane's own reviewed path.

#### Making a published operator-host record public (CAD-1475)

The publisher defaults every `operator-host` catalogue record to
`visibility: private`. To expose an already-published record publicly
without minting a new release, the operator dispatches
`.github/workflows/release-visibility.yml` from `main` with the record's
full source SHA, then approves the `production` environment. The job calls
`scripts/make-release-public.py`, which reads the exact
`catalogue/cadence/operator-host/<source-sha>.json` record, validates its
closed schema, identity and `status: published`, flips ONLY `visibility`
to `public` and writes the record back with `put-object --if-match
<captured-ETag>` — a compare-and-swap that fails closed if anything
changed the record between read and write. Already-public is an honest
no-op; missing/malformed/withdrawn/foreign records and concurrent
conflicts refuse with no write. Artifact objects are never touched — the
operation changes the catalogue record's visibility only, and the
publisher's immutable conditional writes are unchanged. The endpoint and
bucket are the dedicated-store literals `87b59a40e130c425e35474687ad98a4f.r2.cloudflarestorage.com`
/ `agenticos-v2-release-artifacts`, pinned in both
the workflow and the helper. The exact copyable endpoint literal is
`https://87b59a40e130c425e35474687ad98a4f.r2.cloudflarestorage.com`;
configured `vars.RELEASES_R2_*` must equal the store values before any
mutation, and the helper refuses any other store.
Credentials stay in the job environment (`secrets.RELEASES_R2_*` as
`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`); a missing credential fails
closed.

The operator-host visibility path is operationally proven. On 2026-10-16
the operator dispatched `release-visibility.yml` (run 38862158365, main
`c03c08a3`, `production` environment approval by the operator) for the
approved staging candidate `84de8c0b5df9ddeac81b4e8bbe7e888b6494d36d`
(staging promotion run 38824325073). The job flipped only
`visibility: private -> public`. Anonymous HTTPS verification the same
day showed detail, list and latest all resolving that one record, and the
full archive GET returning exactly 20,474,099 bytes with SHA-256
`7b25bf163cdb7bbe7699316c76bfd23889574e747775ab6a5439200448392c3f`,
byte-for-byte identical to the retained original candidate bundle. Public
URLs on the owned endpoint:

```text
https://api-v2.agenticos.hk/v1/cadence/releases/cadence/operator-host/84de8c0b5df9ddeac81b4e8bbe7e888b6494d36d
https://api-v2.agenticos.hk/v1/cadence/artifacts/cadence/operator-host/84de8c0b5df9ddeac81b4e8bbe7e888b6494d36d/cadence-release.tar.gz
```

This proves anonymous public read of a published operator-host record and
bundle only: no native updater consumed the endpoint, this verification
performed no install or restart, and a public record grants no
update/install/rollout authority by itself.

Under CAD-1479, `cadence update`/`upgrade` built with the new default
consumes the published release from `https://api-v2.agenticos.hk` without
a source flag and without any GitHub API call: the catalogue selects the
subject (discovery only, never authority); the binary, receipt,
build/promotion/completion Sigstore bundles and the signed source-facts are
verified offline against the embedded trusted-root snapshot, and the
snapshot — never a live main lookup — answers on-main, ancestry and schema
checks, and ancestry is proven only relative to the snapshot's own
candidate (membership in its signed ancestor list), never between two
sibling commits. Installed-profile detection is unchanged: a client-only
receipt selects the `public-client` profile, an ordinary daemon
installation the `operator-host` profile. The existing operator identity,
lease, backup, schema, no-backward, atomic install, drain, health and
rollback rules are unchanged. An explicit source failure refuses; there
is no silent fallback to the GitHub API and no GitHub login is needed —
offline verification needs only the `gh` binary for
`attestation verify --bundle`. `--release-source https://<host>` remains
as the advanced override to consume a different owned endpoint; it adds
no routine knob. A pinned `--to`/`--sha` resolves the published release
named by that exact source SHA; a sha the catalogue does not publish
refuses. Binaries installed before this default shipped still select the
GitHub route and keep working; they pick up the backend default only by
updating to a build that carries it.

The publication-time snapshot is an authenticated record of eligibility
*observed at promotion*; it cannot know a later main rewrite. Offline
verification needs `gh` installed (for `attestation verify --bundle`) but
no GitHub login and no API access. Operator-host catalogue visibility
defaults `private`; each record's public serving stays closed until the
operator-owned visibility job above authorizes that exact record.
Public-client release authority is distinct (below) — a `public-client` record never stands in for an
operator-host promotion, and the shared catalogue grants no install or
rollout authority by itself.

Remaining qualification boundaries: the `operator-host` record above is
live and publicly readable, but the `public-client` catalogue still has
no published record — its `latest` endpoint returned HTTP 200 with an
empty entry on 2026-10-16. No native updater has yet consumed the
endpoint and no install against it is claimed; a default that points
installed consumers at an empty profile must wait for a genuine
public-client publication. The operator-host record never stands in for
public-client authority. Publication, visibility, withdrawal, install
and rollout stay separately gated operator actions; anonymous read opens
none of them.

**Scheduled and `workflow_run` staging cannot promote.** Their successful
runs may have `promote: skipped`; that means staged, not production-approved.
The default updater selects the last successful approved manual promotion,
not the newest green main artifact.

### Stop conditions

- `current = available` at an older SHA means the default selected candidate
  did not advance to the intended build. Inspect the promotion run/receipt;
  do not repeatedly update, label it deployed, or silently add `--to`.
- If `--check` reports a refusal or an in-flight-turn drain blocker, report it
  and stop. Do not interrupt another lane, force a switch or bypass a lease.
- A later promotion can change the default candidate. If the reported target
  differs from the agreed SHA, stop and resolve it with the operator. A
  specific `--to` pin is permitted only when explicitly requested by the
  operator; it uses its own main/CI/digest/manifest/attestation checks rather
  than the production-candidate receipt.
- On update failure/regression, the operator decides rollback through supported
  recovery procedures. Do not improvise a symlink/store rollback or relay
  internal rollout/upgrade recipes.

## Numbered public releases

A tag is a deliberate operator-owned release batch, not a tag per PR. Cadence
has no automatic patch/minor policy or anonymous stable-release updater;
see [public installation](INSTALL-AGENT.md). Do not substitute the public
`latest` tag for the native host's approved main-SHA candidate.

Version-selection guidance for the operator's release decision:

| Change | Suggested version choice |
| --- | --- |
| Backward-compatible fixes or packaging corrections | Patch; documentation-only changes need no automatic binary release. |
| Additive user-facing features | Minor, with a clear release summary. |
| Compatibility-breaking CLI/API/data changes | Explicit compatibility/migration decision; major after 1.0. While 0.x, explicitly disclose breaks rather than imply a minor bump guarantees compatibility. |
| Preview not ready for stable users | Prerelease such as `-beta.N`; prereleases are not GitHub `latest`. |

These are selection guidance, not new runtime enforcement. The operator owns
release timing/version and approves compatibility exceptions. Reviewed release
scope and notes must describe actual delivered changes and known limitations.
Do not batch an unverified breaking change into a “routine patch.”

1. Put the intended `Cargo.toml` version bump and any corresponding lockfile
   update through the normal ticket/PR/review/approval/merge path. Do not edit
   main directly. Installed app-bundle versions are independent of the host
   version. The app-toolchain archive published by this tag workflow is explicitly
   bound to the host tag; do not infer version coupling for other packages.
2. An authorized operator creates a **new** protected `v<version>` tag on the
   selected merged main commit. The tag must exactly match Cargo's version;
   existing tags/releases are immutable, including a failed historical tag.
3. The tag-push `ci.yml` run requires all five gates (`fmt`, `clippy`, `test`,
   `build`, `ui`), main ancestry/version checks and full-gate eligibility.
   It builds x86_64 Linux, aarch64 Linux and aarch64 macOS packages, then runs
   native `release-install` checks on all three before publication.
4. The operator approves the separate protected GitHub **`release`** environment
   job. It verifies and attests the assets, then publishes the GitHub Release;
   this is not the `production` approval for a main-SHA host candidate.
5. Verify the published tag, source SHA, assets/checksums/attestations and release
   classification. Stable releases become `latest`; prereleases do not. Never
   move a published tag or replace published bytes: fix forward with a new
   reviewed version. A draft left by a failed publish was never public and
   may be retried only through the workflow's own guarded retry behavior.

Source of enforced behavior: `.github/workflows/ci.yml`,
`.github/workflows/staging.yml`, `src/upgrade.rs` and `src/update.rs`.
This guide does not grant approval, override GitHub protections or change
native authorization, release gates or production safety.

## Historical first public release handoff (CAD-661)

The records and detailed publication checks below retain their original scope;
placeholder annexes are historical templates, not fresh release evidence.

[Cadence 0.1.0-beta.2](https://github.com/favcrm/cadence/releases/tag/v0.1.0-beta.2)
is published as a local CLI prerelease (`prerelease=true`, `latest=false`). Its
source is `181bf4dfff466b02310a4d9339a3045b61dc5705`. The release notes record the
exact tag-run/attempt, gate/build results, digests and prepublication native
candidate installation receipts. Anonymous published-mode installation evidence
is recorded separately in the main [installation guide](INSTALL-AGENT.md);
immutable release notes are not appended after publication. This record is not
authorization for a production rollout, stable promotion, rerun or replacement
release.

## Stable 0.1.0 (published, immutable)

[Cadence 0.1.0](https://github.com/favcrm/cadence/releases/tag/v0.1.0)
is published as the stable release (`prerelease=false`, `latest=true`) from
source `4066ddab844a318a85c3bc286392ec927a600a5c`, tag run `38201476703`
attempt 1. Its nine published assets are `install.sh`, the three targets'
`cadence-v0.1.0-<target>.tar.gz` plus matching `.sha256`, and the x86 leg's
versioned `cadence-app-toolchain-v0.1.0.tar.gz` pair (CAD-1272). Its release
notes were `--generate-notes` output only and are immutable with the release;
this file, not those notes, records corrections. At v0.1.0 publication the
public stable updater was not implemented; the client-only `cadence update`
updater (CAD-1425/CAD-1437) and the owned release source (CAD-1455; default
under CAD-1479, above) supersede that note.

For a client-only installation, `cadence update` (CAD-1455; backend
default under CAD-1479) fetches the same tag's exact published archive,
checksum file and retained `ci.yml` build-provenance bundle through the
owned `public-client` catalogue profile at the default owned endpoint
(`https://api-v2.agenticos.hk`; `--release-source` overrides it) and
verifies the signature offline against the embedded trusted root —
the same repo/workflow/tag/digest/runner
policy as the live `gh` lookup. Because a build bundle attests the archive
BEFORE `Publish the GitHub Release` ran, the publisher also retains a small
closed post-publication receipt (`publication-receipt.json` +
`publication-attestation.jsonl`, platform `publication`): minted and
attested by `ci.yml`@tag only after the release is confirmed public, and
binding the repository, tag, source commit and every published archive's
sha256. The client verifies that receipt offline with the embedded root and
binds it to the verified archive and manifest before the candidate binary
runs; a missing, forged or unproven receipt refuses even when the build
bundle and the catalogue's `status:published` are fine. It does not use the
operator-host promotion authority, never substitutes a daemon for the client
archive, and preserves the tag/manifest/platform/version checks and the
forward-only rule. A configured source failure refuses rather than falling
back to the GitHub release API. Publication of the public-client record runs
only after the existing `release-publish` job publishes the GitHub Release,
and only when the operator provisioned the release bucket
(`vars.RELEASES_R2_ENDPOINT`/`vars.RELEASES_R2_BUCKET`, credentials
`secrets.RELEASES_R2_ACCESS_KEY_ID`/`secrets.RELEASES_R2_SECRET_ACCESS_KEY`);
its `publishedAt` is the release's genuine GitHub `publishedAt`, never a
mirror run's clock. Under CAD-1479 the published client record is public
by construction: the same upstream protected `release` approval that
publishes the GitHub Release authorizes public-client publicity — the
R2 publish job adds no second environment approval and there is no
separate client visibility workflow. Operator-host records stay
private-by-default with their own visibility lifecycle (above), and a
valid client publication receipt remains distinct from operator-host
promotion authority.

The first publication exposed an x86 artifact/verifier gap fixed under
CAD-1400: `release-build` zips the toolchain pair into the x86 artifact but
the acquisition verifier's exact inventory refused it. Tag-run candidate
`38203278770` installed successfully on aarch64-linux and aarch64-macos; the
x86_64-linux leg's verifier refused the artifact before installation. From
CAD-1400 on, the tag run's own `release-install` matrix installs the exact
candidate on all three targets automatically and `release-publish` cannot
reach the `release` environment approval until every leg succeeds. This new
tag-only gate awaits its first future authorized release tag; it did not run
retroactively on immutable v0.1.0.

Anonymous public v0.1.0 installation passed on all three native targets in
[run 38213520059, attempt 1](https://github.com/favcrm/cadence/actions/runs/38213520059),
using consumer source `b27a8ead58d471db6a88efeea03bba8433b0f700` and package
source/run `4066ddab844a318a85c3bc286392ec927a600a5c` / `38201476703`, attempt 1.
Default/custom prefixes, exact version, checksums, installed manifests and
same-version rerun stability passed. This proves public download and CLI
installation only: full CAD-317 setup/wizard/backup acceptance, macOS
daemon/board support and production activation are not claimed.

The immutable `v0.1.0-beta.1` tag failed its macOS UI build and retains a private
draft; it has no published installation assets. The beta.2 publication record
above is complete. The stable 0.1.0 annex below is an intentionally pending
future-candidate template; its placeholders are not release evidence or a claim
that v0.1.0 is published.

## Historical preparation: beta.2 candidate selection

The original pilot preparation set Cargo.toml and Cargo.lock to
**0.1.0-beta.2**, with matching tag **v0.1.0-beta.2** and title
**Cadence 0.1.0-beta.2 — local CLI pilot**. That historical preparation is
complete: the immutable beta.2 release is published from
`181bf4dfff466b02310a4d9339a3045b61dc5705` as recorded above. The placeholder
annex below describes the prepublication checklist, not current beta.2 status.
For a new release, select a fresh reviewed exact main commit only after the
version/source prerequisites land and release evidence is accepted. A
preparation PR head is not the publication candidate. Unmerged PRs and issuer
contracts are not shipped capabilities.

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

## Historical publication checklist; remaining acceptance

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

## Pilot scope and historical proposed notes

Cadence 0.1.0-beta.2 is a local CLI pilot: a native controller and embedded
browser board for local project and coding-agent coordination. Evaluate on
fresh, separate local state; existing production-state compatibility and schema
rollback are not established by a clean install.

Planned packages are Linux x86_64, Linux ARM64 (glibc 2.35 baseline) and Apple
Silicon macOS. Intel macOS, Windows and Alpine/musl packages are not provided.
Daemon and board evaluation is Linux-only with fresh, separate state. Apple
Silicon macOS is limited to CLI packaging, installation and version checks;
setup, daemon and board use is unsupported pending CAD-315.
Anonymous install-only native results are linked in the main installation guide;
full CAD-317 setup/wizard/backup acceptance remains pending. Provider
CLIs/authentication and tmux for terminal providers are separate prerequisites;
list only combinations actually verified for the eventual candidate.

Browser/token support is issuer-client preparation, not working public cloud
login, enrollment or assignment delivery. Local offline outbox persistence is
a foundation, not proof of remote transport, applied receipts or sleep-safe
delivery. Hosted remote teams and local-to-cloud production migration remain
separate acceptance work. At pilot time the public stable-release updater was
not implemented and update/upgrade used the authenticated internal CI channel;
the client-only updater and owned release source above supersede this.

Stable v0.1.0 is now published, so `releases/latest` resolves to it; the beta
prereleases remain installable only by explicit tag. Selecting another
installer version is not a coordinated running daemon upgrade or compatible
state rollback.

## Stable 0.1.0 evidence annex to complete for the approved candidate

Use the following content for the v0.1.0 release draft, replacing placeholders
with evidence from the exact reviewed candidate and successful tag workflow
before the environment approval. The workflow can reuse an existing draft,
upload the artifacts and publish after approval.

```text
Cadence 0.1.0 — stable local CLI release

Native CLI and embedded board for local agent coordination. Supported assets:
Linux x86_64, Linux ARM64 (glibc 2.35 baseline), Apple Silicon macOS.
No Intel macOS, Windows or musl package.
Daemon and board evaluation: Linux only, using fresh, separate state.
Apple Silicon macOS: CLI packaging, installation and version checks only;
setup, daemon and board use unsupported pending CAD-315.

Provider CLIs/authentication and tmux for terminal providers are separate.
List only provider/platform combinations actually verified for this candidate.
Hosted browser login, remote local worker teams and local-to-cloud migration
are not claimed operational. Public stable-release upgrades are not implemented.

Source SHA: <copied exact SHA>
Tag: v0.1.0 (planned, not created)
Classification: prerelease=false; latest=true (verify actual release metadata)
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
   Assets must be exactly `install.sh`, each target's
   `cadence-<tag>-<target>.tar.gz` plus matching `.tar.gz.sha256`, and (since
   CAD-1272) the x86-produced `cadence-app-toolchain-<tag>.tar.gz` plus
   `.tar.gz.sha256` — nine assets total. Each target archive contains
   `cadence`, `cadence.sha256`, `manifest.json`. The workflow re-checks every
   tarball and the toolchain manifest before attesting; record archive and
   installer digests and provenance verification results.
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

## Dev-to-release flow (CAD-1400)

One ticket, one PR. The lane worktree passes `scripts/pre-push` locally, then
exact-head CI and independent review on the PR; operator scope or per-head
approval (recorded via `cadence audit approve`) precedes
`scripts/enqueue-reviewed` into the merge queue, which retests on newest
main. The merged head is the only release candidate: a `v*` tag matching
Cargo.toml may be created only by the authorized operator under the tag
ruleset. The tag's own ci.yml run reruns all five gates plus release-gate,
builds the three native packages (`release-build`), then installs that exact
candidate natively on all three targets (`release-install`, candidate mode of
clean-install.py against the same run and attempt — no separate dispatch).
Only when every leg succeeds may `release-publish` request the `release`
environment approval, which a human grants. After publication, the published
mode of clean-install.py verifies the anonymous public URLs on the native
targets and its results are recorded here and in INSTALL-AGENT.md. No agent
approves release environments; published bytes and release notes are
immutable.
