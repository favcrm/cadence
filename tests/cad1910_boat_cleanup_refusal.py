#!/usr/bin/env python3
"""CAD-1910 INDEPENDENT acceptance — boat-build refuses a mismatched sandbox.

Authored independently of the implementer; the implementer may not edit or
weaken this file. It is a black-box check of the public ``scripts/boat-build``
entry point: no module of the helper is imported and no source text is
inspected.

Scenario (no real provider is contacted): the helper is run inside a
disposable, clean git repository whose PATH starts with an executable fake
``boat``. The fake ``boat new`` streams a direct JSONL ``created`` event
naming sandbox A and then a ``ready`` event naming a different sandbox B —
a creation whose readiness cannot be attributed to the sandbox this
invocation created.

What must hold:

1. The helper exits nonzero. A mismatched ``ready`` is a refusal, not a
   result to work around.
2. No ``exec`` or ``scp`` invocation happens at all: the refusal precedes
   any remote work, so B's identity can never be used and the command is
   never run in a sandbox whose readiness was not proven.
3. Sandbox B is never named by any ``boat`` invocation — no delete, stop,
   scp, exec or anything else may target the mismatched ID. Only the ID
   from this invocation's own direct ``created`` event (A) is ever eligible
   for cleanup.
4. If the helper cleans up, it deletes only A: every ``delete``/``stop``
   invocation names A and nothing else. Cleaning up A is optional here
   (leaving it for the operator is also a valid refusal), so this check
   does not require a deletion — it only constrains one.

Invocation data is recorded by the fake in this check's own temporary
directory. The helper is run with a scrubbed environment: no credentials,
tokens or provider configuration from the ambient environment are passed
through, and nothing from the environment is logged.

Interface assumptions (the public contract this check relies on):

* ``scripts/boat-build`` is executable (run directly), or is a Python
  program (run as ``python3 scripts/boat-build``). No arguments are
  required; the check passes ``-- <command>`` to exercise the documented
  separator, and the command must never run.
* It resolves its git root from the working directory and refuses when the
  worktree has untracked files, so the disposable repository is clean and
  committed, with both ``refs/remotes/origin/main`` and ``refs/heads/main``
  pointing at the candidate commit as the default base.
* ``boat`` is found through PATH. ``boat new ... --json`` prints one JSON
  object per line with ``event`` and ``id``; the owned ID is the ``id`` of
  the ``created`` event, and a ``ready`` event with a different ``id`` is
  the mismatch. Sandbox IDs are opaque ``bx_`` tokens.
* Cleanup, when attempted, is a ``boat`` subcommand naming the sandbox ID
  (``delete`` or ``stop``).
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


REPO = Path(__file__).resolve().parents[1]
HELPER = REPO / "scripts/boat-build"

# A: the ID of the helper's own direct `created` event.
CREATED_ID = "bx_abcd2345"
# B: a valid-looking sandbox ID that this invocation did not create.
FOREIGN_ID = "bx_efgh6789"

FAKE_BOAT = """#!/usr/bin/env python3
import json
import sys

LOG = %(log)r
CREATED = %(created)r
FOREIGN = %(foreign)r
COMMANDS = ("new", "delete", "deletion", "scp", "exec", "usage", "stop", "list")

argv = sys.argv[1:]
try:
    with open(LOG, "a", encoding="utf-8") as log:
        log.write(json.dumps({"argv": argv}) + "\\n")
except OSError:
    pass

command = next((token for token in argv if token in COMMANDS), "")
if command == "new":
    # The created event is this invocation's only owned ID; readiness then
    # names a different sandbox. The process still exits 0.
    print(json.dumps({"event": "created", "id": CREATED}), flush=True)
    print(json.dumps({"event": "ready", "id": FOREIGN}), flush=True)
elif command in ("delete", "deletion", "stop"):
    target = next((token for token in argv if token.startswith("bx_")), "")
    print(json.dumps({"operation": {"id": "op_cad1910", "targetId": target,
                                    "status": "completed"}}), flush=True)
else:
    print("{}", flush=True)
sys.exit(0)
"""


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def git(env, *args):
    return subprocess.run(["git", *args], env=env, capture_output=True, text=True,
                          timeout=120)


def flag_value(argv, flag):
    for index, token in enumerate(argv):
        if token == flag:
            return argv[index + 1] if index + 1 < len(argv) else None
        if token.startswith(flag + "="):
            return token.split("=", 1)[1]
    return None


def sandbox_tokens(argv):
    return [token for token in argv if token.startswith("bx_")]


def build_environment(base):
    """A scrubbed environment: no credentials or provider config inherited."""
    bin_dir = base / "bin"
    home = base / "home"
    tmp = base / "tmp"
    for directory in (bin_dir, home, tmp):
        directory.mkdir(parents=True, exist_ok=True)
    return {
        "PATH": str(bin_dir) + os.pathsep + os.environ.get(
            "PATH", "/usr/local/bin:/usr/bin:/bin"),
        "HOME": str(home),
        "TMPDIR": str(tmp),
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_AUTHOR_NAME": "CAD-1910 Acceptance",
        "GIT_AUTHOR_EMAIL": "acceptance@example.invalid",
        "GIT_COMMITTER_NAME": "CAD-1910 Acceptance",
        "GIT_COMMITTER_EMAIL": "acceptance@example.invalid",
    }


def prepare_repository(base, env):
    repo = base / "repo"
    repo.mkdir()
    for args in (
        ("init", "-q", str(repo)),
        ("-C", str(repo), "symbolic-ref", "HEAD", "refs/heads/main"),
    ):
        result = git(env, *args)
        require(result.returncode == 0, f"git {args} failed: {result.stderr}")
    (repo / "tracked.txt").write_text("candidate\n", encoding="utf-8")
    for args in (
        ("-C", str(repo), "add", "tracked.txt"),
        ("-C", str(repo), "commit", "-q", "-m", "candidate"),
        # A cloned-like base so either default base resolution is satisfied.
        ("-C", str(repo), "update-ref", "refs/remotes/origin/main", "HEAD"),
    ):
        result = git(env, *args)
        require(result.returncode == 0, f"git {args} failed: {result.stderr}")
    untracked = git(env, "-C", str(repo), "ls-files", "--others", "--exclude-standard")
    require(untracked.returncode == 0 and not untracked.stdout.strip(),
            f"disposable repository is not clean: {untracked.stdout!r}")
    return repo


def read_invocations(log):
    if not log.exists():
        return []
    invocations = []
    for line in log.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line:
            invocations.append(json.loads(line)["argv"])
    return invocations


def main():
    require(HELPER.is_file(), f"missing public entry point: {HELPER}")

    with tempfile.TemporaryDirectory(prefix="cad1910-") as temp:
        base = Path(temp)
        env = build_environment(base)
        log = base / "boat-invocations.jsonl"
        fake = base / "bin" / "boat"
        fake.write_text(
            FAKE_BOAT % {"log": str(log), "created": CREATED_ID,
                         "foreign": FOREIGN_ID},
            encoding="utf-8",
        )
        fake.chmod(0o755)
        repo = prepare_repository(base, env)

        if os.access(HELPER, os.X_OK):
            command = [str(HELPER)]
        else:
            command = [sys.executable, str(HELPER)]
        # The documented `--` separator; this command must never run.
        command += ["--", sys.executable, "-c", "raise SystemExit(0)"]

        result = subprocess.run(command, cwd=repo, env=env, capture_output=True,
                                text=True, timeout=300)
        output = (result.stdout + result.stderr)[-2000:]

        require(result.returncode != 0,
                "boat-build accepted a mismatched ready event; "
                f"exit={result.returncode} output={output!r}")

        invocations = read_invocations(log)
        require(invocations, f"boat-build never invoked the fake boat; output={output!r}")

        news = [argv for argv in invocations
                if any(token == "new" for token in argv)]
        require(len(news) == 1,
                f"expected exactly one fresh `boat new` invocation, saw {len(news)}: "
                f"{invocations!r}")
        new_argv = news[0]
        for flag in ("--no-env", "--no-snapshots", "--json"):
            require(flag in new_argv, f"`boat new` omitted {flag}: {new_argv!r}")
        require(flag_value(new_argv, "--ttl") == "3600",
                f"`boat new` did not request a one-hour TTL: {new_argv!r}")
        require(flag_value(new_argv, "--type") == "default",
                f"`boat new` did not request the default type: {new_argv!r}")
        name = flag_value(new_argv, "--name")
        require(bool(name and name.strip()),
                f"`boat new` did not name the sandbox: {new_argv!r}")

        for argv in invocations:
            require(not any(FOREIGN_ID in token for token in argv),
                    "boat-build named the mismatched ready sandbox B: "
                    f"argv={argv!r}")

        for argv in invocations:
            command_name = next((token for token in argv
                                 if token in ("exec", "scp")), None)
            require(command_name is None,
                    f"boat-build ran `{command_name}` despite the refusal: {argv!r}")

        for argv in invocations:
            command_name = next((token for token in argv
                                 if token in ("delete", "stop")), None)
            if command_name is None:
                continue
            targets = sandbox_tokens(argv)
            require(targets == [CREATED_ID],
                    f"cleanup `{command_name}` must target only the created sandbox "
                    f"{CREATED_ID}, saw {targets!r} in {argv!r}")

        deleted = [argv for argv in invocations
                   if any(token in ("delete", "stop") for token in argv)]
        print(f"PASS: boat-build refused the mismatched ready event (exit "
              f"{result.returncode}); 1 fresh sandbox requested, no exec/scp, "
              f"no reference to {FOREIGN_ID}, cleanup targets only {CREATED_ID} "
              f"({len(deleted)} cleanup invocation(s))")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
