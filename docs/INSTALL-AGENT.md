# Native installation

As of 2026-09-27, no GitHub Release has been published. The commands below
describe installation **after publication**; they are not working download
links yet. Check the [release list](https://github.com/favcrm/cadence/releases)
for an available tag and its limitations before installing.

## Platforms and prerequisites

The pipeline produces `x86_64-linux`, `aarch64-linux` and `aarch64-macos`
assets. Linux builds use Ubuntu 22.04 (glibc 2.35 baseline); these are not
Alpine/musl builds. macOS packaging is for Apple Silicon, not Intel.
Windows is not packaged. Clean installation evidence for each platform is
tracked by CAD-317; packaging alone does not establish complete provider support.

The installer needs a POSIX shell, curl (or wget), tar, and one of sha256sum,
shasum or openssl. Installing the binary does not require Node, Rust, a GitHub
login or an AgenticOS login. Provider CLIs, their credentials and, for native
terminal providers, tmux are separate runtime requirements.

## Install an available version

For a pilot prerelease, copy its exact tag from the release list. GitHub's
`latest` endpoint excludes prereleases. Set `release_tag` to the published
tag before running these commands:

```sh
release_tag=v0.1.0-beta.1 # example only; must be an actually published tag
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
refused. The installer does not start a daemon or configure a provider. Run
`cadence setup` separately on a new installation when ready to create local state.

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

Public stable automatic upgrades are not yet verified. Current `cadence upgrade`
and `cadence update` use the authenticated, attested internal CI channel; do not
present them as anonymous public-release upgrades (CAD-661/CAD-561).

The installer can select another published version explicitly, but replacing
an executable while a daemon runs is not a coordinated upgrade or rollback.
Existing installations must use their rollout owner's backup, compatibility,
drain and restart procedure. An older binary may be incompatible with newer
state. No running daemon is restarted by this installer.

Hosted sign-in, local worker cloud delivery and production migration remain
separate acceptance work under CAD-525/CAD-539/CAD-529. A native CLI release
does not imply those cloud paths are operational.
