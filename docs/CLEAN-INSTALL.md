# Bounded clean-install validation (CAD-317)

`clean-install.yml` validates installation of an exact release package on native
Ubuntu 22.04 x86-64, Ubuntu 22.04 ARM64 and macOS 14 ARM64 runners. This increment
covers the actual installer, default and custom prefixes, package/installed
digests and manifest, exact `--version`, exact symlink destination, and unchanged
file and symlink inode/mtime/content on both same-version reruns.

CAD-317 remains open: setup JSON, headless wizard, fake-provider operation and
backup/restore acceptance are pending. No daemon, board, provider or agent is
started here. macOS daemon support additionally depends on CAD-315. Every receipt
sets `full_cad317_acceptance: false`; a successful install-only job cannot complete
the original setup and backup/restore criteria.

PR checks run standard-library tests with the real `scripts/install.sh` and a
harmless shell executable that only implements fixture `--version`. The simulated
macOS fixture tests orchestration, not native macOS or a Cadence binary. Actual
package proof comes only from the three native dispatch jobs.

Dispatch **from main**, supplying `mode`, exact `tag`, full package `source_sha`,
producing `ci_run_id` and `ci_run_attempt`. The workflow revision and package
revision are recorded separately. Non-main dispatches do not execute native
installation. Inputs are validated before acquisition or application execution.

In `candidate` mode the helper uses a read-only token only in the acquisition
step. It verifies the canonical repository, tag-push `ci.yml` identity, peeled
tag, source ancestry, exact attempt, all five required checks, `release-gate`
and all three successful native release builds. A protected publisher waiting
for approval is supported; a failed, cancelled or skipped required job is refused.
Complete paginated inventories and a unique live artifact are required. Pending
inventory is bounded to 20 minutes; failure is never retried into success.
Verified archive bytes are materialized into an owned `file://` mirror because
the installer intentionally refuses an HTTP mirror. This is pre-publication
artifact proof, not public-download or publisher-authenticity proof.

In `published` mode acquisition uses anonymous actual tag-specific GitHub release
URLs, without `gh`, a bearer token, inherited proxies or a mirror. The supplied
source/run/attempt are checked against the package manifest. Checksums detect byte
changes; this helper does not claim cryptographic publisher attestation. Run this
mode after publication to establish real public URL availability. Use explicit
beta tags: GitHub's [latest release endpoint](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)
excludes prereleases.

Application execution uses fresh short owned `/tmp` roots, separate HOME/XDG/TMP
directories and an explicit environment allowlist. Its PATH contains only selected
system tools; Cargo, Rust, Node, npm, pnpm and `gh` are absent. Neither acquisition
tokens nor arbitrary inherited provider credentials reach the installer or binary.
The only Cadence invocation is `--version`. No production home/state, port 3010,
tailnet or installed operator CLI is used. Hosted runner teardown removes these
owned roots; there are no background processes to signal.

Artifacts retain acquisition identity, manifest, polling decisions, exact producer
revision and actual command exits/stdout/stderr, including partial failed smoke
receipts. No release, tag, deployment, protection rule or production state is
changed by this workflow. Native installation proof supplements the existing
full CI and merge queue; it does not replace either gate or authorize publication.
