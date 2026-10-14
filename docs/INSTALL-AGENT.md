# Native installation

[Cadence 0.1.0](https://github.com/favcrm/cadence/releases/tag/v0.1.0)
is the published stable local CLI release (`latest=true`), built from source
`4066ddab844a318a85c3bc286392ec927a600a5c` by tag run `38201476703` attempt 1
with build-provenance attestations. Anonymous public downloads and native
installation passed on Linux x86_64, Linux ARM64 and Apple Silicon macOS in
[run 38213520059, attempt 1](https://github.com/favcrm/cadence/actions/runs/38213520059).
The consumer workflow used reviewed main source
`b27a8ead58d471db6a88efeea03bba8433b0f700`; the installed package remained
pinned to the v0.1.0 source and producing run above. On every target, default
and custom-prefix installation, exact version, archive/binary checksums,
installed manifest equality and same-version rerun stability passed without
Rust, Node or GitHub login in the installation environment.

CAD-1400 also added a three-target `release-install` gate before publication
approval. That tag-only gate awaits its first future authorized release tag;
it did not run retroactively on immutable v0.1.0.
This install-only proof does not complete CAD-317's setup, headless wizard or
backup/restore acceptance, or establish compatibility with existing production
state; it is not a production rollout.

The immutable `v0.1.0-beta.1` tag failed its macOS UI build and has no
published installation assets; `v0.1.0-beta.2` remains installable only by
explicit tag.

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

The published stable release installs through `latest`:

```sh
curl -fsSL https://github.com/favcrm/cadence/releases/latest/download/install.sh -o install.sh
sh install.sh
"$HOME/.local/bin/cadence" --version
```

For a prerelease or an older version, copy its exact tag from the release
list. GitHub's `latest` endpoint excludes prereleases. Set `release_tag` to
the published tag before running these commands:

```sh
release_tag=v0.1.0-beta.2 # published local CLI pilot
curl -fsSL "https://github.com/favcrm/cadence/releases/download/$release_tag/install.sh" -o install.sh
sh install.sh --version "$release_tag"
"$HOME/.local/bin/cadence" --version
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

## Client-only installation (CAD-1425)

A remote-org CLI user who has no local daemon can install a client-only copy
of a public release. It lives in its own prefix and is reached through a
separate link, so the ordinary `cadence` install is not changed:

```sh
curl -fsSL https://github.com/favcrm/cadence/releases/latest/download/install.sh -o install.sh
sh install.sh --client-only
"$HOME/.local/bin/cadence-client" --version
```

The default prefix is `${XDG_DATA_HOME:-$HOME/.local/share}/cadence-client`
and the link is `$HOME/.local/bin/cadence-client`. `--client-only` writes
`<prefix>/client-install.json` and fetches only the fixed public GitHub
release. It refuses `--base-url` and `CADENCE_INSTALL_*` overrides, requires
the GitHub CLI (`gh`) to verify the archive's build-provenance attestation
(repository `favcrm/cadence`, signer `ci.yml`, tag ref, manifest source
commit) before the candidate runs, and refuses without an insecure fallback.

A client-only installation upgrades itself with `cadence-client update`
(`--check` selects and reports the target only, with no download or
verification; `--version <tag>` pins a newer release, and the real upgrade
verifies it before any change). The old `client-update` command is removed.
The published v0.1.1 already has legacy client-only support, through
`client-update`; it cannot be upgraded to this unified `update` in place. The
unified update needs the next capable signed release and its `install.sh`:
reinstall once with that installer. It re-validates the existing receipt and
link against the same prefix and moves the link only under its guards; it
never adopts an ordinary installation. It never downgrades, changes
`$HOME/.local/bin/cadence`, or touches the remote credentials, registry or
daemon. A client-only executable refuses
local daemon and state commands with exit 3; bounded
login, auth, org and offline management stays available.

Client-only mode works on any published release whose `cadence update --help`
carries the client-only marker. The published v0.1.1 has the legacy
`client-update`; the unified `update` needs the next capable release. The
GitHub CLI used for the attestation check is the operator's own `gh` session,
not Cadence remote credentials.

## Updates and current limits

The native daemon updater is `cadence update [--to <sha>]`, the operator path
for a host that runs the local daemon. `cadence upgrade` is recovery only.
Both use the authenticated, attested internal CI channel. On macOS the native
daemon updater is unsupported and refuses (it cannot prove the operator there
without procfs); upgrade a macOS client with `install.sh --client-only` and
`cadence-client update`. Do not present the native updater as an
anonymous public-release upgrade (CAD-661/CAD-561).

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
