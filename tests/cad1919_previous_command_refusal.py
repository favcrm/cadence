#!/usr/bin/env python3
"""CAD-1919 acceptance: a warm sandbox must not reset a live command's tree.

Black-box check. It does not import the helper. A fake ``boat`` lets
``scripts/boat-build`` upload its real remote script, and this check then
runs that script against a checkout whose previous command is still alive.
No provider account is contacted.
"""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time


REPO = Path(__file__).resolve().parents[1]
HELPER = REPO / "scripts" / "boat-build"
SURVIVOR = "import signal,time\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)\nwhile True:\n time.sleep(3600)\n"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def git(env, *args):
    result = subprocess.run(["git", *args], env=env, capture_output=True, text=True, timeout=60)
    require(result.returncode == 0, f"git {args} failed: {result.stderr}")
    return result


def scrubbed_env(root):
    return {
        "PATH": str(root / "bin") + os.pathsep + os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(root / "home"),
        "TMPDIR": str(root / "tmp"),
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_AUTHOR_NAME": "Boat Refusal",
        "GIT_AUTHOR_EMAIL": "boat-refusal@example.invalid",
        "GIT_COMMITTER_NAME": "Boat Refusal",
        "GIT_COMMITTER_EMAIL": "boat-refusal@example.invalid",
    }


def write_fake_boat(path, remote):
    path.write_text(
        """#!/usr/bin/env python3
import json, shutil, sys
from pathlib import Path
remote = Path(%(remote)r)
argv = sys.argv[1:]
command = next((token for token in argv if token in {
    "new", "delete", "deletion", "scp", "exec", "usage", "stop", "list", "info", "extend"}), "")
if command == "new":
    print(json.dumps({"event": "created", "id": "bx_23456789"}), flush=True)
    print(json.dumps({"event": "ready", "id": "bx_23456789"}), flush=True)
elif command == "scp":
    remote.mkdir(parents=True, exist_ok=True)
    shutil.copy(argv[-2], remote / argv[-1].split("/")[-1])
elif command == "info":
    print(json.dumps({"sandbox": {"id": "bx_23456789", "state": "idle"}}), flush=True)
else:
    print("{}", flush=True)
""" % {"remote": str(remote)},
        encoding="utf-8",
    )
    path.chmod(0o755)


def prepare_repository(root, env):
    repo = root / "repo"
    repo.mkdir()
    git(env, "init", "-q", str(repo))
    git(env, "-C", str(repo), "symbolic-ref", "HEAD", "refs/heads/main")
    (repo / "tracked.txt").write_text("base\n", encoding="utf-8")
    git(env, "-C", str(repo), "add", "tracked.txt")
    git(env, "-C", str(repo), "commit", "-q", "-m", "base")
    git(env, "-C", str(repo), "update-ref", "refs/remotes/origin/main", "HEAD")
    return repo


def run_remote(remote_root, env):
    child = env.copy()
    child["CADENCE_BOAT_REMOTE_ROOT"] = str(remote_root)
    return subprocess.run(
        [sys.executable, str(remote_root / "boat-remote.py")],
        cwd=remote_root, env=child, capture_output=True, text=True, timeout=30)


def prepare_checkout(remote_root, env):
    work = remote_root / "boat-worktree"
    if work.exists():
        subprocess.run(["rm", "-rf", str(work)], check=True)
    work.mkdir(parents=True)
    git(env, "init", "-q", str(work))
    git(env, "-C", str(work), "symbolic-ref", "HEAD", "refs/heads/main")
    (work / "tracked.txt").write_text("kept\n", encoding="utf-8")
    git(env, "-C", str(work), "add", "tracked.txt")
    git(env, "-C", str(work), "commit", "-q", "-m", "kept")
    sha = git(env, "-C", str(work), "rev-parse", "HEAD").stdout.strip()
    (work / "marker.txt").write_text("live command owns this tree\n", encoding="utf-8")
    job = {
        "head": sha,
        "base": sha,
        "command": ["true"],
        "snapshot": False,
        "quarantine": False,
    }
    (remote_root / "boat-job.json").write_text(json.dumps(job), encoding="utf-8")
    (remote_root / "boat-changes.patch").write_bytes(b"")
    return work


def kernel_record(pid, argv):
    stat = Path(f"/proc/{pid}/stat").read_text()
    fields = stat.rpartition(")")[2].split()
    return {
        "pid": pid,
        "argv": argv,
        "starttime": int(fields[19]),
        "sid": int(fields[3]),
        "pgid": int(fields[2]),
    }


def write_record(remote_root, record):
    (remote_root / "boat-command.pid").write_text(json.dumps(record), encoding="utf-8")


def main():
    require(HELPER.is_file(), f"missing {HELPER}")
    owned = []
    try:
        with tempfile.TemporaryDirectory(prefix="boat-previous-") as temp:
            root = Path(temp)
            for name in ("bin", "home", "tmp", "sandbox"):
                (root / name).mkdir()
            env = scrubbed_env(root)
            remote_root = root / "sandbox"
            write_fake_boat(root / "bin" / "boat", remote_root)
            repo = prepare_repository(root, env)
            uploaded = subprocess.run(
                [sys.executable, str(HELPER), "--", "true"],
                cwd=repo, env=env, capture_output=True, text=True, timeout=120)
            require(uploaded.returncode == 0, f"helper did not upload the remote script: {uploaded.stderr}\n{uploaded.stdout}")
            require((remote_root / "boat-remote.py").is_file(), "helper did not upload boat-remote.py")

            def refuse(proc, work, detail):
                refused = run_remote(remote_root, env)
                require(refused.returncode != 0, f"{detail} was reused: {refused.stderr}\n{refused.stdout}")
                require("still running" in refused.stderr, refused.stderr)
                require("sync=" not in refused.stdout, refused.stdout)
                require((remote_root / "boat-command.pid").is_file(), f"{detail} deleted the command record")
                require((work / "marker.txt").is_file(), f"{detail} reset the live checkout")
                require(proc.poll() is None, f"{detail} killed the live command")

            work = prepare_checkout(remote_root, env)
            legacy = subprocess.Popen(
                [sys.executable, "-c", SURVIVOR],
                start_new_session=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL)
            owned.append(legacy)
            write_record(remote_root, {"pid": legacy.pid, "argv": ["/bin/not-the-launch-argv"]})
            refuse(legacy, work, "legacy argv mismatch")

            work = prepare_checkout(remote_root, env)
            survivor = subprocess.Popen(
                [sys.executable, "-c", SURVIVOR],
                start_new_session=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL)
            owned.append(survivor)
            write_record(remote_root, kernel_record(survivor.pid, ["/bin/not-the-launch-argv"]))
            refuse(survivor, work, "changed argv")

            ready = remote_root / "exec-ready"
            code = (
                "import signal,time,pathlib\n"
                "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                f"pathlib.Path({str(ready)!r}).write_text('execed\\n')\n"
                "while True:\n"
                "    time.sleep(3600)\n"
            )
            launch = ["/bin/bash", "-c", "exec " + shlex.quote(sys.executable) + " -c " + shlex.quote(code)]
            execed = subprocess.Popen(
                launch, start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            owned.append(execed)
            deadline = time.time() + 5
            while time.time() < deadline and not ready.exists():
                require(execed.poll() is None, "exec helper exited before replacing itself")
                time.sleep(0.05)
            require(ready.exists(), "exec did not replace the shell")
            cmdline = Path(f"/proc/{execed.pid}/cmdline").read_bytes().split(b"\0")
            require(cmdline[0] != b"/bin/bash", f"exec kept the shell argv: {cmdline!r}")
            work = prepare_checkout(remote_root, env)
            write_record(remote_root, kernel_record(execed.pid, launch))
            refuse(execed, work, "exec")

            for proc in owned:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait(timeout=5)
            owned.clear()
            released = run_remote(remote_root, env)
            require(released.returncode == 0, f"stopped command did not release the sandbox: {released.stderr}\n{released.stdout}")
            require("sync=reuse" in released.stdout, released.stdout)
            require(not (work / "marker.txt").exists(), "a stopped command left the checkout unsynchronized")
            require(not (remote_root / "boat-command.pid").exists(), "a finished command left its pid record")

            work = prepare_checkout(remote_root, env)
            reused = subprocess.Popen(["sleep", "30"], start_new_session=True)
            owned.append(reused)
            record = kernel_record(reused.pid, ["/bin/not-our-command"])
            record["starttime"] += 1
            write_record(remote_root, record)
            continued = run_remote(remote_root, env)
            require(continued.returncode == 0, f"reused pid blocked the sandbox: {continued.stderr}\n{continued.stdout}")
            require(reused.poll() is None, "a reused pid was signalled")
            require(not (work / "marker.txt").exists(), "a reused pid left the checkout unsynchronized")
            require(not (remote_root / "boat-command.pid").exists(), "a reused pid left the command record")
        print("PASS: exec keeps the checkout protected, and a reused pid is not signalled")
    finally:
        for proc in owned:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.TimeoutExpired, json.JSONDecodeError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
