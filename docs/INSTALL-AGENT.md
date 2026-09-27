# Native installation

As of 2026-09-27, no GitHub Release has been published. The commands below
describe installation **after publication**; they are not working download
links yet. Check the [release list](https://github.com/favcrm/cadence/releases)
for an available tag and its limitations before installing.

The immutable `v0.1.0-beta.1` tag failed its macOS UI build and has no
published installation assets. Replacement source is prepared for
**0.1.0-beta.2**, a local CLI pilot. Its matching planned tag is
**v0.1.0-beta.2**; no source SHA has been selected for publication. The
planned version is not an available download or a supported production rollout.

## Platforms and prerequisites

The pipeline produces `x86_64-linux`, `aarch64-linux` and `aarch64-macos`
assets. Linux builds use Ubuntu 22.04 (glibc 2.35 baseline); these are not
Alpine/musl builds. macOS packaging is for Apple Silicon, not Intel.
Windows is not packaged. Clean installation evidence for each platform is
tracked by CAD-317; packaging alone does not establish complete provider support.
Daemon and board evaluation is limited to Linux with fresh, separate state.
Apple Silicon macOS supports CLI packaging, installation and version checks
only; setup, daemon and board use is unsupported pending CAD-315.

The installer needs a POSIX shell, curl (or wget), tar, and one of sha256sum,
shasum or openssl. Installing the binary does not require Node, Rust, a GitHub
login or an AgenticOS login. Provider CLIs, their credentials and, for native
terminal providers, tmux are separate runtime requirements.

## Install an available version

For a pilot prerelease, copy its exact tag from the release list. GitHub's
`latest` endpoint excludes prereleases. Set `release_tag` to the published
tag before running these commands:

```sh
release_tag=v0.1.0-beta.2 # planned pilot; use only once this tag is published
curl -fsSL "https://github.com/favcrm/cadence/releases/download/$release_tag/install.sh" -o install.sh
sh install.sh --version "$release_tag"
"$HOME/.local/bin/cadence" --version
```

Only after a stable release exists, the convenience path is:

```sh
curl -fsSL https://github.com/favcrm/cadence/releases/latest/download/install.sh -o install.sh
sh install.sh
```

The default release root is `${XDG_DATA_HOME:-$HOME/.local/share}/cadence`.
`--prefix /absolute/path` selects another root; the executable link remains
`$HOME/.local/bin/cadence`. Add that directory to your shell's PATH if needed:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Installation verifies the archive and binary SHA256 digests and requested
version before activating it. Rerunning the same version is safe; different
bytes under an existing version and ordinary files at the executable link are
refused. The installer does not start a daemon or configure a provider. On Linux,
run `cadence setup` separately on a fresh installation when ready to create local
state. Do not run setup, daemon or board commands on macOS pending CAD-315.

## Verify provenance

Checksums detect corruption; obtaining checksums and files from the same
server does not independently establish authenticity. The release workflow
attests each tarball and the installer. For optional stronger verification,
download them without executing the installer, then use GitHub CLI:

```sh
gh attestation verify install.sh --repo favcrm/cadence \
  --source-ref "refs/tags/$release_tag" \
  --signer-workflow favcrm/cadence/.github/workflows/ci.yml
```

Apply the same command to the selected tarball. For an approved exact commit,
also pass `--source-digest "$release_sha"`, using the SHA from the release
evidence. GitHub CLI/API access requirements apply to this optional verification;
the basic installation path does not depend on `gh`.

## Updates and current limits

The public stable-release updater is not implemented. Current `cadence upgrade`
and `cadence update` use the authenticated, attested internal CI channel; do not
present them as anonymous public-release upgrades (CAD-661/CAD-561).

The installer can select another published version explicitly, but replacing
an executable while a daemon runs is not a coordinated upgrade or rollback.
Existing installations must use their rollout owner's backup, compatibility,
drain and restart procedure. An older binary may be incompatible with newer
state. No running daemon is restarted by this installer.

Browser/token login code is issuer-client preparation, not a working public
cloud login, enrollment or assignment flow. Local worker teams receiving cloud
assignments, sleep-safe accepted-to-applied result delivery and production
migration remain separate acceptance work under CAD-525/CAD-539/CAD-529.
The local offline outbox foundation does not establish a working remote
transport or an applied receipt. A native CLI pilot does not establish cloud
readiness, compatibility with existing production state or every provider.
