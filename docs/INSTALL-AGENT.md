# Install Cadence with a coding agent

Paste the prompt below into Claude Code, Codex, Cursor or any coding agent
that can run shell commands. It installs the latest release with
[`install.sh`](../scripts/install.sh) (published as the release asset
`install.sh`), checks where the build came from, and reports each step. It
only writes under `~/.local/share/cadence` (or `--prefix`) and
`~/.local/bin/cadence`, and it never uses `sudo`.

To pin a version, replace `latest` in the prompt with a tag such as `v0.1.0`.

---

```text
Install the Cadence CLI on this machine and verify it. Work step by step,
run each command yourself, and stop at the first step that fails: report
the command, its exact output, and the fix it names. Do not use sudo, do
not edit shell profiles unless I say so, and do not start a daemon.

1. Platform. Run `uname -s` and `uname -m`. Supported: Linux x86_64,
   Linux aarch64 (arm64), macOS arm64 (Apple silicon). Anything else:
   stop and tell me it is unsupported. Target names: x86_64-linux,
   aarch64-linux, aarch64-macos.

2. Tools. Confirm `curl` (or `wget`), `tar`, and one of `sha256sum`,
   `shasum` or `openssl` are on PATH. Name any that are missing. Check
   whether `gh` (GitHub CLI) is installed and logged in (`gh auth status`).

3. Existing install. Run `ls -l ~/.local/bin/cadence 2>&1`. If it exists
   and is NOT a symlink, stop: the installer refuses to replace it, and I
   must move it aside myself.

4. Download into a fresh directory (do not pipe into sh), and read
   install.sh before running it:
     d=$(mktemp -d) && cd "$d"
     curl -fsSLO https://github.com/favcrm/cadence/releases/latest/download/install.sh
   With gh, also fetch the tarball for this machine and verify where both
   were built. Use the tag the release reports (`gh release view -R
   favcrm/cadence --json tagName --jq .tagName`, or the one I gave you):
     gh release download <tag> -R favcrm/cadence -p 'cadence-<tag>-<target>.tar.gz'
     for f in install.sh cadence-<tag>-<target>.tar.gz; do
       gh attestation verify "$f" --repo favcrm/cadence \
         --source-ref refs/tags/<tag> \
         --signer-workflow favcrm/cadence/.github/workflows/ci.yml
     done
   Both must verify. If either fails, stop and report it — do not install.
   Without gh, say plainly that authenticity was NOT verified: the
   installer's checksums come from the same release as the tarball, so
   they catch corruption and truncation, not a forged release.

5. Install:
     sh ./install.sh --version <tag>
   It must print `checksum ok: <sha256>` and end with
   `ok: cadence <version>+<commit>`. `checksum mismatch` means nothing was
   installed — report it, do not retry from another source. A `source:`
   line means it downloaded from somewhere other than GitHub Releases;
   report that too.

6. Verify:
     readlink ~/.local/bin/cadence
       -> must point into ~/.local/share/cadence/releases/<tag>/cadence
     ~/.local/bin/cadence --version
       -> `cadence <version>+<40-hex commit>`
     (cd "$(dirname "$(readlink ~/.local/bin/cadence)")" && \
       (sha256sum -c cadence.sha256 2>/dev/null || shasum -a 256 -c cadence.sha256))
       -> `cadence: OK`

7. PATH. If `command -v cadence` does not print ~/.local/bin/cadence,
   tell me to add `export PATH="$HOME/.local/bin:$PATH"` to my shell
   profile. Do not add it yourself.

8. Setup. If `cadence setup --help` succeeds, run
   `cadence setup --json --no-open` and report every check whose status is
   not ok with its `fix`. If `setup` does not exist in this version, say
   so and stop here.

9. Remove the temp directory and report: the installed version and
   commit, the release directory, the link, whether the attestation was
   verified, and the result of each check.
```

---

## What the installer guarantees

- It refuses to install unless the tarball matches its published
  `.sha256`, and the binary inside matches its own `cadence.sha256`.
  Those checksums are served next to the tarball, so they catch corruption
  and truncation only. Authenticity is the build-provenance attestation:
  `gh attestation verify <file> --repo favcrm/cadence --source-ref
  refs/tags/<tag> --signer-workflow favcrm/cadence/.github/workflows/ci.yml`
  proves the file was built by this repo's `ci.yml` from that tag.
- A cut-short download of `install.sh` (for example `curl | sh` over a
  dropped connection) installs nothing: all of it runs from one call on
  the last line.
- Releases live side by side in `<prefix>/releases/<tag>/` (`cadence`,
  `cadence.sha256`, `manifest.json`), the same layout `cadence upgrade`
  uses. The default prefix is `${XDG_DATA_HOME:-~/.local/share}/cadence`.
- `~/.local/bin/cadence` is a symlink, replaced atomically (a new link
  renamed over the old one). A regular file there is never replaced.
- Rerunning with the same version changes nothing; a different version
  installs beside the old one and moves the link. Roll back by rerunning
  with the older `--version`.
- The UI is embedded in the binary, so an installed release serves it
  without network access.

Limits for now: `cadence upgrade` (installing main builds) is
x86_64-linux only (`upgrade::TARGET`); on the other targets, update by
rerunning `install.sh` with a newer tag. On macOS the CLI installs and
runs, but `cadence daemon run` refuses to start until the macOS port
(CAD-315) lands.
