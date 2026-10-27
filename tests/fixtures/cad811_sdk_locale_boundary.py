#!/usr/bin/env python3
"""Independent CAD-811 negative acceptance; never qualifies a positive SDK build."""
import argparse
import hashlib
import io
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile

PACKAGER = "ui/scripts/package-screen-ui.mjs"
CONSUMER = "ui/screen-ui/src/index.ts"
DENIED = "ui/src/lib/locale.tsx"
INJECTION = b'import "../../src/lib/locale"; // independent forbidden host boundary edge\n'
REASON = "Error: Authority-free entry escaped allowlist: src/lib/locale.tsx"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host-source", required=True, type=Path)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--toolchain-root", required=True, type=Path)
    parser.add_argument("--scratch-root", required=True, type=Path)
    args = parser.parse_args()
    require(re.fullmatch(r"[0-9a-f]{40}", args.source_sha), "A full committed SHA is required")
    host = args.host_source.resolve(strict=True)
    toolchain = args.toolchain_root.resolve(strict=True)
    require(host.is_dir() and toolchain.is_dir(), "Source and toolchain must be directories")
    require((toolchain / "node_modules").is_dir(), "Supply the already-installed pinned toolchain; no installs")
    scratch = args.scratch_root.absolute()
    scratch_stat = scratch.lstat()
    require(stat.S_ISDIR(scratch_stat.st_mode), "Scratch root must be a real non-symlink directory")
    require(scratch == scratch.resolve(strict=True), "Scratch path must contain no symlink components or parent traversal")
    require(scratch_stat.st_uid == os.getuid(), "Scratch root must be owned by the calling UID")
    require(stat.S_IMODE(scratch_stat.st_mode) == 0o700, "Scratch root must have mode 0700")
    require(stat.S_ISREG(Path(__file__).lstat().st_mode), "Authored script must be a regular non-symlink file")
    require(shutil.which("node") and shutil.which("cadence"), "Installed node and native cadence admission are required")

    # Read-only Git calls. No checkout, index, ref, config or repository writes.
    # Keep the caller's complete environment and native identity bindings.
    def git(*argv):
        return subprocess.run(
            ["git", "-C", str(host), *argv], check=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60,
        ).stdout

    git("cat-file", "-e", args.source_sha + "^{commit}")
    require(git("rev-parse", "HEAD").decode().strip() == args.source_sha,
            "HOST HEAD must equal the requested committed source SHA")
    gitdir = git("rev-parse", "--absolute-git-dir").decode().strip()
    original_script = git("show", args.source_sha + ":" + PACKAGER)
    original_consumer = git("show", args.source_sha + ":" + CONSUMER)
    original_locale = git("show", args.source_sha + ":" + DENIED)
    archive = git("archive", "--format=tar", args.source_sha, "ui")

    # Retain this unique run on both PASS and failure for separate QA inspection.
    run = Path(tempfile.mkdtemp(prefix="negative-", dir=scratch))
    copy = run / "counterexample"
    copy.mkdir()
    temp = run / "packager-tmp"
    temp.mkdir()
    print("Counterexample/evidence directory: " + str(run), flush=True)
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as exported:
        # Materialize only regular committed files/directories, never links into HOST.
        for member in exported.getmembers():
            path = PurePosixPath(member.name)
            require(not path.is_absolute() and ".." not in path.parts
                    and path.parts and path.parts[0] == "ui",
                    "Unsafe committed archive path")
            destination = copy.joinpath(*path.parts)
            if member.isdir():
                destination.mkdir(parents=True, exist_ok=True)
            else:
                require(member.isfile(), "Non-regular committed UI path: " + member.name)
                destination.parent.mkdir(parents=True, exist_ok=True)
                with exported.extractfile(member) as source:
                    destination.write_bytes(source.read())
                destination.chmod(member.mode & 0o777)

    require((copy / PACKAGER).read_bytes() == original_script,
            "Official packager copy differs from its committed Git blob")
    require((copy / CONSUMER).read_bytes() == original_consumer,
            "SDK consumer copy differs from its committed Git blob")
    require((copy / DENIED).read_bytes() == original_locale,
            "Forbidden host module copy differs from its committed Git blob")
    require((copy / "ui/src/lib/localeText.tsx").is_file(),
            "Run only after the pure locale boundary extraction is committed")
    output = copy / "ui/dist"
    require(not output.exists(), "Fresh counterexample must contain no inherited SDK output/receipt")
    # One intentional bad edge, first in an otherwise unchanged admitted SDK entry.
    (copy / CONSUMER).write_bytes(INJECTION + original_consumer)

    env = os.environ.copy()
    env.update({
        "GIT_DIR": gitdir,
        "GIT_WORK_TREE": str(copy),
        "TMPDIR": str(temp),
        "CARGO_BUILD_JOBS": "4",
        "CARGO_NET_OFFLINE": "true",
        "COREPACK_ENABLE_NETWORK": "0",
        "NODE_DISABLE_COMPILE_CACHE": "1",
    })
    # Do not manufacture slot markers, replace RUSTC_WRAPPER, or clear HOME,
    # CADENCE_ALIAS, CADENCE_SOCKET or any identity/credential environment.
    command = [
        "cadence", "--org", "local", "build-slot", "run", "build", "--wait-secs", "900", "--",
        "timeout", "--signal=TERM", "1800s",
        "node", str(copy / PACKAGER),
        "--source-tree", str(copy), "--source-sha", args.source_sha,
        "--toolchain-root", str(toolchain), "--working-tree",
    ]
    # This is the only packager invocation. Admission failures remain failures.
    with (run / "stdout.log").open("xb") as stdout, (run / "stderr.log").open("xb") as stderr:
        result = subprocess.run(command, cwd=copy, env=env, stdout=stdout, stderr=stderr)
    stdout = (run / "stdout.log").read_text(errors="replace")
    stderr = (run / "stderr.log").read_text(errors="replace")
    metadata = (
        "sourceSha=" + args.source_sha + "\n"
        "hostSource=" + str(host) + "\n"
        "gitDir=" + gitdir + "\n"
        "toolchainRoot=" + str(toolchain) + "\n"
        "officialPackagerSha256=" + digest(original_script) + "\n"
        "originalConsumerSha256=" + digest(original_consumer) + "\n"
        "badConsumerSha256=" + digest((copy / CONSUMER).read_bytes()) + "\n"
        "admittedCommand=" + repr(command) + "\n"
        "nativeExit=" + str(result.returncode) + "\n"
    )
    (run / "result.txt").write_text(metadata)
    require(result.returncode == 1,
            "Unexpected native exit (including admission/timeout/signal/success): " + str(result.returncode))
    require(stderr.splitlines().count(REASON) == 1,
            "Expected exact official source-guard refusal missing/ambiguous; inspect stderr.log")
    require(not output.exists(), "SDK output directory/receipt/emission exists after refusal")
    require(not any(temp.iterdir()), "Packager created temporary emission before refusal")
    require("Kit typecheck: passed" not in stdout and "Host typecheck: passed" not in stdout,
            "Packager progressed beyond the pre-emission source guard")
    require((copy / PACKAGER).read_bytes() == original_script,
            "Official packager changed during invocation")
    require(git("rev-parse", "HEAD").decode().strip() == args.source_sha,
            "HOST HEAD moved during acceptance; evidence requires revalidation")
    print("PASS: native exit 1; " + REASON + "; no SDK receipt or emission")
    print(metadata, end="")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, OSError, subprocess.SubprocessError, tarfile.TarError) as error:
        print("FAIL (not accepted as boundary refusal): " + str(error), file=sys.stderr)
        sys.exit(1)
