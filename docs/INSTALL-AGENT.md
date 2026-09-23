# Install Cadence with a coding agent

Paste the prompt below into Claude Code, Codex, Cursor or any coding agent
that can run shell commands. It installs the latest release with
[`install.sh`](../install.sh), verifies it, and reports each step. It only
writes under `~/.local/share/cadence` (or `--prefix`) and `~/.local/bin/cadence`,
and it never uses `sudo`.

To pin a version, replace `latest` in the prompt with a tag such as `v0.1.0`.

---

```text
Install the Cadence CLI on this machine and verify it. Work step by step,
run each command yourself, and stop at the first step that fails: report
the command, its exact output, and the fix it names. Do not use sudo, do
not edit shell profiles unless I say so, and do not start a daemon.

1. Platform. Run `uname -s` and `uname -m`. Supported: Linux x86_64,
   Linux aarch64 (arm64), macOS arm64 (Apple silicon). Anything else:
   stop and tell me it is unsupported.

2. Tools. Confirm `curl` (or `wget`), `tar`, and one of `sha256sum`,
   `shasum` or `openssl` are on PATH. Name any that are missing.

3. Existing install. Run `ls -l ~/.local/bin/cadence 2>&1`. If it exists
   and is NOT a symlink, stop: the installer refuses to replace it, and I
   must move it aside myself.

4. Install. Download the installer to a file and read it before running
   it (do not pipe it into sh):
     curl -fsSL -o /tmp/cadence-install.sh \
       https://github.com/favcrm/cadence/releases/latest/download/install.sh
     sh /tmp/cadence-install.sh --version latest
   It must print `checksum ok: <sha256>` and finish with
   `ok: cadence <version>+<commit>`. A line with `checksum mismatch` means
   the download was refused and nothing was installed — report it, do not
   retry with another source.

5. Verify.
     readlink ~/.local/bin/cadence
       -> must point into ~/.local/share/cadence/releases/v<version>/cadence
     ~/.local/bin/cadence --version
       -> `cadence <version>+<40-hex commit>`
     cd "$(dirname "$(readlink ~/.local/bin/cadence)")" && \
       (sha256sum -c cadence.sha256 2>/dev/null || shasum -a 256 -c cadence.sha256)
       -> `cadence: OK`
   If `gh` is installed and logged in, also verify the build provenance of
   the release tarball (optional; skip if gh is missing):
     gh attestation verify <the downloaded .tar.gz> --repo favcrm/cadence
   (download it with `gh release download v<version> -R favcrm/cadence
   -p 'cadence-*-<target>.tar.gz'`, where <target> is x86_64-linux,
   aarch64-linux or aarch64-macos).

6. PATH. If `command -v cadence` does not print ~/.local/bin/cadence,
   tell me to add `export PATH="$HOME/.local/bin:$PATH"` to my shell
   profile. Do not add it yourself.

7. Setup. If `cadence setup --help` succeeds, run
   `cadence setup --json --no-open` and report every check whose status is
   not ok with its `fix`. If `setup` does not exist in this version, say
   so and stop here.

8. Clean up /tmp/cadence-install.sh and report: the installed version and
   commit, the release directory, the link, and the result of each check.
```

---

## What the installer guarantees

- It refuses to install unless the tarball matches its published
  `.sha256`, and the binary inside matches its own `cadence.sha256`.
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

On macOS the CLI installs and runs, but `cadence daemon run` refuses to
start until the macOS port (CAD-315) lands.
