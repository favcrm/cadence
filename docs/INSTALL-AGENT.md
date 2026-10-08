# Native installation

[Cadence 0.1.0-beta.2](https://github.com/favcrm/cadence/releases/tag/v0.1.0-beta.2)
is the published local CLI pilot. Anonymous installation, exact version,
default/custom prefixes and same-version reruns were verified on native Ubuntu
22.04 x86_64/ARM64 and macOS 14 Apple Silicon in the
[anonymous published-mode validation](https://github.com/favcrm/cadence/actions/runs/36337031674).
The immutable release notes separately record source/build and prepublication
candidate installation evidence.
This install-only proof does not complete CAD-317's setup, headless wizard or
backup/restore acceptance, or establish compatibility with existing production
state. It is a prerelease, not a stable `latest` release or production rollout.

The immutable `v0.1.0-beta.1` tag failed its macOS UI build and has no
published installation assets. Use **v0.1.0-beta.2** explicitly.

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

### Managed Codex on Ubuntu 24.04

Managed Codex runs its own confined command sandbox. Cadence checks that
`codex sandbox` can run a read command, and for workspace-write agents can
write and read a temporary workspace file, before it opens app-server. The
10-second check runs at registration and resume; a failure puts the agent in
`attention` with the provider error in `cadence agent show`, before a message
can be assigned to it.

On this Ubuntu 24.04 host (2026-09-28), `bubblewrap` 0.9.0 is installed at
`/usr/bin/bwrap`, but `codex sandbox -- /bin/true` fails before the command
starts with `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`.
The AppArmor unprivileged-user-namespace restriction is enabled and the
`bwrap-userns-restrict` profile is absent. This is a high-confidence host
prerequisite diagnosis pending a successful post-change retest.

[OpenAI's Ubuntu 24.04 prerequisites](https://developers.openai.com/codex/concepts/sandboxing#prerequisites)
recommend that the operator install and load the extra `bwrap` profile:

```sh
sudo apt update
sudo apt install apparmor-profiles apparmor-utils
sudo install -m 0644 /usr/share/apparmor/extra-profiles/bwrap-userns-restrict /etc/apparmor.d/bwrap-userns-restrict
sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
```

Inspect the profile source after package installation and schedule this as a
host security change. Keep `kernel.apparmor_restrict_unprivileged_userns=1`;
do not use `danger-full-access`, make `bwrap` setuid, or restart a production
daemon as a shortcut. From a temporary work directory, verify `codex sandbox
-c 'sandbox_mode="workspace-write"' -- /bin/true` with the daemon's OS user
and PATH. Then, in a separate Cadence state directory and lane, verify
a managed workspace-write Codex worker reaches `idle`, reads/writes a small
file, reports a result, and retains its configured sandbox in `agent show`.
The direct CLI check establishes the host prerequisite; the managed test
establishes the app-server path.

If the profile causes a regression, unload it with `sudo apparmor_parser -R
/etc/apparmor.d/bwrap-userns-restrict` and remove the copied file. Retest
affected `bwrap` consumers; leave the global restriction enabled. Package
removal is not required for rollback.

## Install an available version

For a pilot prerelease, copy its exact tag from the release list. GitHub's
`latest` endpoint excludes prereleases. Set `release_tag` to the published
tag before running these commands:

```sh
release_tag=v0.1.0-beta.2 # published local CLI pilot
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

The public stable-release updater is not implemented. `cadence update
[--to <sha>]` is the operator path and `cadence upgrade` is recovery only.
Both use the authenticated, attested internal CI channel; do not present them as anonymous public-release upgrades (CAD-661/CAD-561).

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
