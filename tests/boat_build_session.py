#!/usr/bin/env python3
"""Hermetic checks for the warm Boat session.

No real provider is contacted. A fake ``boat`` records every invocation and
copies uploaded files into a local directory. The embedded remote script then
runs against that directory, so the shallow snapshot, the tracked patch and
``target/`` preservation are real git operations.
"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile


REPO = Path(__file__).resolve().parents[1]
HELPER = REPO / "scripts" / "boat-build"
COMMANDS = ("new", "delete", "deletion", "scp", "exec", "usage", "stop",
            "list", "info", "extend")

FAKE_BOAT = r'''#!/usr/bin/env python3
import json
import shutil
import sys
from pathlib import Path

LOG = %(log)r
STATE = %(state)r
REMOTE = %(remote)r
COMMANDS = ("new", "delete", "deletion", "scp", "exec", "usage", "stop", "list", "info", "extend")
ALPHABET = "23456789abcdefghjkmnpqrstuvwxyz"

def sandbox_id(count):
    number = count
    chars = []
    for _ in range(8):
        chars.append(ALPHABET[number %% len(ALPHABET)])
        number //= len(ALPHABET)
    return "bx_" + "".join(chars)

argv = sys.argv[1:]
with open(LOG, "a", encoding="utf-8") as log:
    log.write(json.dumps({"argv": argv}) + "\n")
state = json.loads(Path(STATE).read_text(encoding="utf-8"))
command = next((token for token in argv if token in COMMANDS), "")

def save():
    Path(STATE).write_text(json.dumps(state), encoding="utf-8")

def target():
    return next((token for token in argv if token.startswith("bx_")), "")

if command == "new":
    count = state.get("creates", 0) + 1
    state["creates"] = count
    state["id"] = sandbox_id(count)
    state["alive"] = True
    save()
    print(json.dumps({"event": "created", "id": state["id"]}), flush=True)
    print(json.dumps({"event": "ready", "id": state["id"]}), flush=True)
elif command == "info":
    if state.get("alive") and target() == state.get("id"):
        print(json.dumps({"sandbox": {"id": target(), "state": "idle"}}), flush=True)
    else:
        print(json.dumps({"code": "not_found", "event": "error", "status": 404,
                          "error": "not found"}), flush=True)
        sys.exit(1)
elif command == "extend":
    if state.get("extend_fail"):
        print(json.dumps({"code": "not_found", "event": "error", "status": 404}), flush=True)
        sys.exit(1)
    print("{}", flush=True)
elif command == "scp":
    destination = argv[-1].split("/")[-1]
    Path(REMOTE).mkdir(parents=True, exist_ok=True)
    shutil.copy(argv[-2], Path(REMOTE) / destination)
elif command == "exec":
    if "-c" in argv:
        shas = [token for token in argv if len(token) in (40, 64) and all(
            char in "0123456789abcdef" for char in token)]
        have = set(state.get("have") or [])
        if not state.get("repo"):
            print("repo miss", flush=True)
            for sha in shas:
                print("miss " + sha, flush=True)
        else:
            print("repo have", flush=True)
            for sha in shas:
                print(("have " if sha in have else "miss ") + sha, flush=True)
    else:
        sys.exit(int(state.get("exec_rc", 0)))
elif command == "delete":
    mode = state.get("delete", "ok")
    ident = target()
    if mode == "missing":
        print(json.dumps({"code": "not_found", "event": "error", "status": 404}), flush=True)
        sys.exit(1)
    status = "blocked" if mode == "blocked" else "completed"
    print(json.dumps({"operation": {"id": "op_session", "targetId": ident, "status": status}}),
          flush=True)
    if status == "completed":
        state["alive"] = False
        save()
elif command == "deletion":
    print(json.dumps({"operation": {"id": "op_session", "targetId": state.get("id", ""),
                                    "status": "completed"}}), flush=True)
else:
    print("{}", flush=True)
sys.exit(0)
'''


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def git(env, *args):
    result = subprocess.run(["git", *args], env=env, capture_output=True, text=True, timeout=60)
    require(result.returncode == 0, f"git {args} failed: {result.stderr}")
    return result


def flag_value(argv, flag):
    for index, token in enumerate(argv):
        if token == flag and index + 1 < len(argv):
            return argv[index + 1]
    return None


def invocations(log):
    if not log.exists():
        return []
    rows = []
    for line in log.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        payload = json.loads(line)
        if "argv" in payload:
            rows.append(payload["argv"])
    return rows


def named(log, command):
    return [argv for argv in invocations(log) if command in argv]


def scp_names(log):
    names = []
    for argv in named(log, "scp"):
        names.append(argv[-1].split("/")[-1])
    return names


def build_environment(base):
    bin_dir = base / "bin"
    home = base / "home"
    tmp = base / "tmp"
    for directory in (bin_dir, home, tmp):
        directory.mkdir(parents=True, exist_ok=True)
    return {
        "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
        "HOME": str(home),
        "TMPDIR": str(tmp),
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_AUTHOR_NAME": "Boat Session",
        "GIT_AUTHOR_EMAIL": "boat-session@example.invalid",
        "GIT_COMMITTER_NAME": "Boat Session",
        "GIT_COMMITTER_EMAIL": "boat-session@example.invalid",
    }


def write_state(path, **overrides):
    payload = {"creates": 0, "alive": False, "repo": False, "have": [], "exec_rc": 0, "delete": "ok"}
    payload.update(overrides)
    path.write_text(json.dumps(payload), encoding="utf-8")


def load_state(path):
    return json.loads(path.read_text(encoding="utf-8"))


def save_state(path, payload):
    path.write_text(json.dumps(payload), encoding="utf-8")


def prepare_repository(base, env):
    repo = base / "repo"
    repo.mkdir()
    git(env, "init", "-q", str(repo))
    git(env, "-C", str(repo), "symbolic-ref", "HEAD", "refs/heads/main")
    (repo / ".gitignore").write_text("target/\n", encoding="utf-8")
    (repo / "tracked.txt").write_text("base\n", encoding="utf-8")
    git(env, "-C", str(repo), "add", ".gitignore", "tracked.txt")
    git(env, "-C", str(repo), "commit", "-q", "-m", "base")
    git(env, "-C", str(repo), "update-ref", "refs/remotes/origin/main", "HEAD")
    (repo / "tracked.txt").write_text("committed\n", encoding="utf-8")
    git(env, "-C", str(repo), "add", "tracked.txt")
    git(env, "-C", str(repo), "commit", "-q", "-m", "head")
    return repo


def run_helper(repo, env, *args, timeout=120):
    return subprocess.run([sys.executable, str(HELPER), *args], cwd=repo, env=env,
                          capture_output=True, text=True, timeout=timeout)


def run_remote(remote_root, env):
    child = env.copy()
    child["CADENCE_BOAT_REMOTE_ROOT"] = str(remote_root)
    return subprocess.run([sys.executable, str(remote_root / "boat-remote.py")],
                          cwd=remote_root, env=child, capture_output=True, text=True, timeout=120)


def shas(repo, env):
    head = git(env, "-C", str(repo), "rev-parse", "HEAD").stdout.strip()
    base = git(env, "-C", str(repo), "rev-parse", "refs/remotes/origin/main").stdout.strip()
    return head, base


def main():
    require(HELPER.is_file(), f"missing {HELPER}")
    with tempfile.TemporaryDirectory(prefix="boat-session-") as temp:
        base = Path(temp)
        env = build_environment(base)
        log = base / "boat.jsonl"
        state_path = base / "state.json"
        remote_root = base / "sandbox"
        write_state(state_path)
        fake = base / "bin" / "boat"
        fake.write_text(FAKE_BOAT % {"log": str(log), "state": str(state_path), "remote": str(remote_root)},
                        encoding="utf-8")
        fake.chmod(0o755)
        repo = prepare_repository(base, env)
        (repo / "tracked.txt").write_text("dirty\n", encoding="utf-8")

        first = run_helper(repo, env, "--receipt", "--", sys.executable, "-c",
                           "open('ran.txt','w').write('ok\\n')")
        require(first.returncode == 0, f"first run failed: {first.stderr}\n{first.stdout}")
        require(len(named(log, "new")) == 1, f"expected one create, saw {named(log, 'new')}")
        new_argv = named(log, "new")[0]
        for flag in ("--no-env", "--no-snapshots", "--json"):
            require(flag in new_argv, f"create omitted {flag}: {new_argv}")
        require(flag_value(new_argv, "--ttl") == "3600", f"create ttl: {new_argv}")
        require(flag_value(new_argv, "--type") == "default", f"create type: {new_argv}")
        require("boat-snapshot.tar" in scp_names(log), f"first run did not upload a snapshot: {scp_names(log)}")
        require(not named(log, "delete"), "first run deleted the sandbox")
        receipt = first.stdout.strip().splitlines()[-1]
        require("reused=0" in receipt and receipt.startswith("head="), f"bad receipt {receipt}")
        session = json.loads((repo / ".git" / "cadence-boat-session.json").read_text(encoding="utf-8"))
        require(session["sandbox"] == load_state(state_path)["id"], f"session id {session}")
        require("idle_seconds=1800" in first.stdout, "idle timer was not set")

        with tarfile.open(remote_root / "boat-snapshot.tar") as archive:
            names = archive.getnames()
            require(all(name == "snapshot.git" or name.startswith("snapshot.git/") for name in names),
                    f"snapshot escaped its directory: {names[:5]}")
            config = archive.extractfile("snapshot.git/config").read().decode()
            require(str(repo) not in config, f"snapshot config records the host path: {config}")
            inspect = base / "inspect"
            archive.extractall(inspect)
        side = base / "side"
        cloned = subprocess.run(["git", "clone", "--quiet", str(inspect / "snapshot.git"), str(side)],
                                env=env, capture_output=True, text=True, timeout=60)
        require(cloned.returncode == 0, f"snapshot clone failed: {cloned.stderr}")
        require((side / "tracked.txt").read_text(encoding="utf-8") == "committed\n",
                "snapshot included the uncommitted edit")
        merge_base = git(env, "-C", str(side), "merge-base", "HEAD", "origin/base").stdout.strip()
        require(merge_base == shas(repo, env)[1], f"merge-base {merge_base}")

        remote = run_remote(remote_root, env)
        require(remote.returncode == 0, f"remote clone failed: {remote.stderr}\n{remote.stdout}")
        work = remote_root / "boat-worktree"
        require((work / "tracked.txt").read_text(encoding="utf-8") == "dirty\n", "patch was not applied")
        require((work / "ran.txt").read_text(encoding="utf-8") == "ok\n", "command did not run")
        require("sync=clone" in remote.stdout, remote.stdout)
        (work / "target").mkdir()
        (work / "target" / "keep").write_text("warm\n", encoding="utf-8")
        (work / "junk.txt").write_text("drop me\n", encoding="utf-8")

        head, base_sha = shas(repo, env)
        state = load_state(state_path)
        state["repo"] = True
        state["have"] = [head, base_sha]
        save_state(state_path, state)
        log.write_text("", encoding="utf-8")
        (repo / "tracked.txt").write_text("dirtier\n", encoding="utf-8")
        second = run_helper(repo, env, "--receipt", "--", sys.executable, "-c",
                            "open('ran.txt','w').write('ok\\n')")
        require(second.returncode == 0, f"reuse run failed: {second.stderr}\n{second.stdout}")
        require(not named(log, "new"), "reuse run created a sandbox")
        require("boat-snapshot.tar" not in scp_names(log), f"reuse uploaded a snapshot: {scp_names(log)}")
        require(not named(log, "delete"), "reuse run deleted the sandbox")
        extends = [flag_value(argv, "--ttl") for argv in named(log, "extend")]
        require(extends == ["3600", "1800"], f"extend ttls {extends}")
        require("reused=1" in second.stdout.strip().splitlines()[-1], second.stdout)
        require("patch only" in second.stdout, second.stdout)
        remote = run_remote(remote_root, env)
        require(remote.returncode == 0, f"remote reuse failed: {remote.stderr}\n{remote.stdout}")
        require("sync=reuse" in remote.stdout, remote.stdout)
        require((work / "tracked.txt").read_text(encoding="utf-8") == "dirtier\n", "reused patch was wrong")
        require((work / "target" / "keep").read_text(encoding="utf-8") == "warm\n", "warm target/ was dropped")
        require(not (work / "junk.txt").exists(), "untracked junk survived the sync")

        git(env, "-C", str(repo), "add", "tracked.txt")
        git(env, "-C", str(repo), "commit", "-q", "-m", "again")
        new_head, _ = shas(repo, env)
        require(new_head not in state["have"], "new commit was already marked present")
        log.write_text("", encoding="utf-8")
        third = run_helper(repo, env, "--", sys.executable, "-c", "open('ran.txt','w').write('ok\\n')")
        require(third.returncode == 0, f"new-commit run failed: {third.stderr}\n{third.stdout}")
        require("boat-snapshot.tar" in scp_names(log), "a new commit did not upload a snapshot")
        require(not named(log, "new"), "a new commit created another sandbox")
        remote = run_remote(remote_root, env)
        require(remote.returncode == 0, f"remote refresh failed: {remote.stderr}\n{remote.stdout}")
        require("sync=clone" in remote.stdout, remote.stdout)
        require((work / "tracked.txt").read_text(encoding="utf-8") == "dirtier\n",
                "refreshed checkout did not match the new commit")
        require((work / "target" / "keep").read_text(encoding="utf-8") == "warm\n",
                "refresh dropped target/")

        log.write_text("", encoding="utf-8")
        down = run_helper(repo, env, "--down")
        require(down.returncode == 0, f"--down failed: {down.stderr}\n{down.stdout}")
        require(len(named(log, "delete")) == 1, "down did not delete")
        require(not (repo / ".git" / "cadence-boat-session.json").exists(), "session file survived --down")
        log.write_text("", encoding="utf-8")
        again = run_helper(repo, env, "--down")
        require(again.returncode == 0, again.stderr)
        require(not named(log, "delete"), "second --down deleted something")
        require("Boat session: none" in again.stdout, again.stdout)

        # Untracked files are refused before any provider call.
        stray = base / "stray"
        stray.mkdir()
        git(env, "init", "-q", str(stray))
        git(env, "-C", str(stray), "symbolic-ref", "HEAD", "refs/heads/main")
        (stray / "tracked.txt").write_text("x\n", encoding="utf-8")
        git(env, "-C", str(stray), "add", "tracked.txt")
        git(env, "-C", str(stray), "commit", "-q", "-m", "x")
        git(env, "-C", str(stray), "update-ref", "refs/remotes/origin/main", "HEAD")
        (stray / "new-file.txt").write_text("nope\n", encoding="utf-8")
        log.write_text("", encoding="utf-8")
        refused = run_helper(stray, env, "--", "true")
        require(refused.returncode != 0, "untracked files were accepted")
        require(not invocations(log), f"untracked refusal contacted boat: {invocations(log)}")

        # One-shot mode deletes even after success.
        log.write_text("", encoding="utf-8")
        once = run_helper(repo, env, "--once", "--", "true")
        require(once.returncode == 0, f"--once failed: {once.stderr}\n{once.stdout}")
        require(named(log, "delete"), "--once did not delete")
        require(not (repo / ".git" / "cadence-boat-session.json").exists(), "--once left a session")

        # A failed command keeps the sandbox.
        state = load_state(state_path)
        state["exec_rc"] = 1
        state["repo"] = True
        state["have"] = list(shas(repo, env))
        save_state(state_path, state)
        log.write_text("", encoding="utf-8")
        failed = run_helper(repo, env, "--", "false")
        require(failed.returncode == 1, f"failed command exit {failed.returncode}: {failed.stderr}")
        require(not named(log, "delete"), "a failed command deleted the sandbox")
        require((repo / ".git" / "cadence-boat-session.json").is_file(), "failed command dropped the session")
        state = load_state(state_path)
        state["exec_rc"] = 0
        save_state(state_path, state)

        # --fresh replaces the recorded sandbox.
        kept = json.loads((repo / ".git" / "cadence-boat-session.json").read_text(encoding="utf-8"))["sandbox"]
        log.write_text("", encoding="utf-8")
        fresh = run_helper(repo, env, "--fresh", "--", "true")
        require(fresh.returncode == 0, f"--fresh failed: {fresh.stderr}\n{fresh.stdout}")
        deleted = [token for argv in named(log, "delete") for token in argv if token.startswith("bx_")]
        require(deleted == [kept], f"--fresh deleted {deleted}, wanted {kept}")
        created = [argv for argv in named(log, "new")]
        require(len(created) == 1, created)
        replacement = json.loads((repo / ".git" / "cadence-boat-session.json").read_text(encoding="utf-8"))["sandbox"]
        require(replacement != kept, f"fresh sandbox id did not change ({kept})")
        require(replacement == load_state(state_path)["id"], "session id does not match the fresh sandbox")

        # A second caller loses the lock and does not create a sandbox.
        # It also must not truncate the log the active invocation still has open.
        import fcntl
        lock_path = repo / ".git" / "cadence-boat-session.lock"
        lock = open(lock_path, "a+")
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        run_log = repo / ".git" / "cadence-boat-build.log"
        run_log.write_bytes(b"active-run-log\n")
        active = open(run_log, "ab", buffering=0)
        active.write(b"before-refusal\n")
        log.write_text("", encoding="utf-8")
        busy = run_helper(repo, env, "--", "true")
        down = run_helper(repo, env, "--down")
        active.write(b"after-refusal\n")
        active.close()
        lock.close()
        body = run_log.read_bytes()
        require(busy.returncode != 0, "busy session was entered")
        require("already has a boat-build running" in busy.stderr, busy.stderr)
        require(down.returncode != 0, "busy --down was entered")
        require("already has a boat-build running" in down.stderr, down.stderr)
        require(not named(log, "new"), "busy run created a sandbox")
        require(b"\x00" not in body, f"busy refusal punched holes in the active log: {body!r}")
        require(body == b"active-run-log\nbefore-refusal\nafter-refusal\n", body)

        # An unconfirmed deletion does not forget the sandbox.
        state = load_state(state_path)
        state["delete"] = "blocked"
        state["alive"] = True
        state["id"] = replacement
        save_state(state_path, state)
        log.write_text("", encoding="utf-8")
        blocked = run_helper(repo, env, "--down")
        require(blocked.returncode != 0, "unconfirmed deletion exited 0")
        require((repo / ".git" / "cadence-boat-session.json").is_file(),
                "unconfirmed deletion dropped the session")
        print("PASS: warm session reuses one sandbox, sends a shallow snapshot, "
              "and keeps target/ across a patch and a new commit")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.TimeoutExpired, json.JSONDecodeError, tarfile.TarError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
