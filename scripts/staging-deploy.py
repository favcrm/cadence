#!/usr/bin/env python3
"""Deploy the latest green main build to the `staging` sandbox, one tick.

One idempotent tick: resolve the newest successful ci.yml run on main
(or a pinned `--run-id`), verify the artifact through
`delivery-candidate.py prepare` — a binary that never passed prepare is
never executed — then `sandbox down`/`sandbox up` it on 127.0.0.1:3020.
First tick seeds the tracker and registers two inbox agents; the tailnet
publish is attempted under the sandbox opt-in and a refusal is recorded,
not fatal. Health is checked on loopback with the board's own Host; a
failed check rolls back to the previous release. Safe under a systemd
timer: a held deploy.lock exits the tick, and the tick itself never
invokes `tailscale` or sudo — it only drives the cadence CLI.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import urllib.request

REPO = "favcrm/cadence"
NAME = "staging"
PORT = 3020
TS_PORT = 9460
KEEP = 5
BASE = Path.home() / ".local/share/cadence-staging"
SCRIPT_DIR = Path(__file__).resolve().parent
URL_LOOPBACK = f"http://cadence-{PORT}.localhost:{PORT}"
URL_TAILNET = f"https://ip-172-31-1-32.tail9fcf30.ts.net:{TS_PORT}"
# These must never reach a sandbox child — they would alias it to the
# caller's identity, profile or tracker.
SCRUB_ENV = ["CADENCE_ALIAS", "CADENCE_PROFILE", "CADENCE_PM_DIR"]


class Refused(Exception):
    pass


def real_run(argv, env=None, cwd=None):
    proc = subprocess.run(
        [str(a) for a in argv],
        env=env,
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=900,
    )
    return proc.returncode, proc.stdout, proc.stderr


def real_http(url, host):
    req = urllib.request.Request(url, headers={"Host": host})
    with urllib.request.urlopen(req, timeout=10) as res:
        return res.status, res.read().decode()


def log_line(base, message):
    stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    with open(base / "deploy.log", "a") as f:
        f.write(f"{stamp} {message}\n")


def child_env(extra=None):
    env = {k: v for k, v in os.environ.items() if k not in SCRUB_ENV}
    env["CADENCE_SANDBOX_ROOT"] = str(BASE / "sandbox")
    env["CADENCE_SANDBOX_ALLOW_GLOBAL"] = "1"
    if extra:
        env.update(extra)
    return env


def run_ok(run, argv, env, cwd=None):
    code, out, err = run(argv, env=env, cwd=cwd)
    if code != 0:
        raise Refused(
            f"`{' '.join(map(str, argv))}` failed ({code}): "
            f"{(err or out).strip()[:500]}"
        )
    return out


def sandbox_env(run, cadence):
    """Parse `cadence sandbox env` export lines into an env dict."""
    out = run_ok(run, [cadence, "sandbox", "env", NAME], child_env())
    env = child_env()
    for line in out.splitlines():
        m = re.fullmatch(r"export (\w+)='(.*)'", line.strip())
        if m:
            env[m.group(1)] = m.group(2).replace("'\\''", "'")
    for key in ("CADENCE_STATE_DIR", "CADENCE_PM_DIR", "CADENCE_PROFILE"):
        if key not in env:
            raise Refused(f"sandbox env is missing {key}")
    return env


def sandbox_down(run, cadence):
    code, out, err = run(
        [cadence, "sandbox", "down", NAME], env=child_env()
    )
    if code != 0 and "no sandbox" not in (err + out):
        raise Refused(
            f"sandbox down failed ({code}): {(err or out).strip()[:500]}"
        )


def health_ok(http_get):
    try:
        status, _ = http_get(
            f"http://127.0.0.1:{PORT}/api/health",
            f"cadence-{PORT}.localhost:{PORT}",
        )
        if status != 200:
            return False
        status, body = http_get(
            f"http://127.0.0.1:{PORT}/api/meta",
            f"cadence-{PORT}.localhost:{PORT}",
        )
        if status != 200:
            return False
        return json.loads(body).get("build_commit")
    except Exception:
        return False


def ensure_release(run, sha, run_id, base):
    dest = base / "releases" / sha
    if (dest / "candidate.json").is_file():
        return dest
    partial = base / "releases" / f"{sha}.partial-{os.getpid()}"
    try:
        run_ok(
            run,
            [
                sys.executable,
                SCRIPT_DIR / "delivery-candidate.py",
                "prepare",
                "--repo", REPO,
                "--run-id", run_id,
                "--dest", partial,
            ],
            os.environ.copy(),
        )
        partial.rename(dest)
    finally:
        if partial.exists():
            import shutil
            shutil.rmtree(partial, ignore_errors=True)
    return dest


def seed(run, cadence, env, base):
    """First tick only: two scratch repos, the seed script against the
    sandbox's own pm dir, two inbox agents, then the marker."""
    for repo in ("cadence", "sportslog"):
        path = base / "repos" / repo
        if not (path / ".git").exists():
            run_ok(run, ["git", "init", path], env)
            run_ok(
                run,
                ["git", "-C", path, "commit", "--allow-empty", "-m", "seed"],
                env,
            )
    pm_dir = Path(env["CADENCE_PM_DIR"]).resolve()
    if not str(pm_dir).startswith(str(base.resolve()) + "/"):
        raise Refused(f"sandbox pm dir {pm_dir} escapes {base} — refusing to seed")
    run_ok(
        run,
        [SCRIPT_DIR / "seed-pm.sh", pm_dir],
        {
            **env,
            "CADENCE": cadence,
            "SEED_ALLOW_INITIALIZED": "1",
            "SEED_REPO_ROOT": str(base / "repos"),
        },
        # `issue new` resolves its project from the cwd's repo — run the
        # seed inside the seeded cadence checkout so its issues land on
        # `cadence`, not on an unmatched caller worktree.
        cwd=base / "repos" / "cadence",
    )
    for alias in ("staging-a", "staging-b"):
        run_ok(run, [cadence, "agent", "register", alias, "--provider", "inbox"], env)
    (base / "seeded").touch()


def tailnet(run, cadence, env):
    """Best-effort publish on the tailnet. Returns the status.json
    `tailnet` field text."""
    try:
        _, out, _ = run([cadence, "ui", "tailscale", "status"], env=env)
        sharing = "sharing: on" in out
        if not sharing:
            code, _, err = run(
                [cadence, "ui", "tailscale", "start", "--port", str(TS_PORT)],
                env=env,
            )
            if code != 0:
                return f"refused: {err.strip()[:300]}"
        return f"sharing at {URL_TAILNET}"
    except Exception as e:  # never fatal
        return f"error: {e}"


def prune(base, keep_shas):
    releases = sorted(
        (p for p in (base / "releases").iterdir()
         if p.is_dir() and re.fullmatch(r"[0-9a-f]{40}", p.name)),
        key=lambda p: p.stat().st_mtime,
    )
    for old in releases[:-KEEP]:
        if old.name not in keep_shas:
            shutil.rmtree(old)


def tick(run, http_get, base=BASE, run_id=None):
    if PORT == 3010:
        raise Refused("PORT 3010 is production's — staging must never take it")
    base.mkdir(parents=True, exist_ok=True)
    (base / "releases").mkdir(exist_ok=True)
    lock = open(base / "deploy.lock", "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        lock.close()
        log_line(base, "tick skipped: deploy.lock held")
        return 0
    try:
        return _tick_locked(run, http_get, base, run_id)
    finally:
        lock.close()


def _tick_locked(run, http_get, base, run_id):
    status_path = base / "status.json"
    status = json.loads(status_path.read_text()) if status_path.exists() else {}

    pinned = run_id is not None
    if not pinned:
        out = run_ok(
            run,
            ["gh", "run", "list", "-R", REPO, "--workflow", "ci.yml",
             "--branch", "main", "--event", "push", "--status", "success",
             "--limit", "1", "--json", "databaseId,headSha"],
            os.environ.copy(),
        )
        candidate = json.loads(out)[0]
        run_id, sha = candidate["databaseId"], candidate["headSha"]
    else:
        out = run_ok(
            run,
            ["gh", "api", f"repos/{REPO}/actions/runs/{run_id}"],
            os.environ.copy(),
        )
        sha = json.loads(out)["head_sha"]

    # An auto-selected candidate that already failed once is never
    # retried — each attempt bounces the live board. `--run-id` pins
    # are the operator's call and bypass the skip.
    if not pinned and sha == status.get("failed_sha"):
        log_line(base, f"tick: skipping known-bad {sha[:12]}")
        return 0

    deployed = health_ok(http_get)
    if sha == status.get("deployed_sha") and deployed == sha:
        log_line(base, f"tick: no-op — {sha[:12]} already live")
        return 0

    release = ensure_release(run, sha, run_id, base)
    cadence = str(release / "cadence")
    env = child_env()

    previous_sha = status.get("deployed_sha")
    log_line(base, f"deploying {sha} (run {run_id}, previous {previous_sha})")
    sandbox_down(run, cadence)

    try:
        run_ok(run, [cadence, "sandbox", "up", NAME, "--port", str(PORT)], env)
        if not (base / "seeded").exists():
            seed(run, cadence, sandbox_env(run, cadence), base)
        tailnet_state = tailnet(run, cadence, sandbox_env(run, cadence))
        healthy = health_ok(http_get)
        if healthy != sha:
            raise Refused(
                f"health check failed: /api/meta build_commit={healthy!r}, want {sha}"
            )
    except Exception as e:
        last_error = str(e)
        log_line(base, f"deploy {sha[:12]} failed: {last_error} — rolling back")
        rolled_back = False
        if previous_sha:
            prev = base / "releases" / previous_sha / "cadence"
            if prev.exists():
                try:
                    sandbox_down(run, str(prev))
                    run_ok(
                        run,
                        [str(prev), "sandbox", "up", NAME, "--port", str(PORT)],
                        env,
                    )
                    rolled_back = True
                except Exception as rb:
                    last_error += f"; rollback also failed: {rb}"
        status.update({
            "last_error": last_error,
            "failed_sha": sha,
            "previous_sha": previous_sha,
            "tailnet": status.get("tailnet"),
        })
        if not rolled_back:
            # Nothing verifiably live: the no-op check must not claim
            # the old sha is still deployed.
            status["deployed_sha"] = None
        status_path.write_text(json.dumps(status, indent=2) + "\n")
        return 1

    status = {
        "deployed_sha": sha,
        "ci_run_id": run_id,
        "deployed_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "previous_sha": previous_sha,
        "tailnet": tailnet_state,
        "last_error": None,
        "url_loopback": URL_LOOPBACK,
        "url_tailnet": URL_TAILNET,
    }
    status_path.write_text(json.dumps(status, indent=2) + "\n")
    prune(base, {sha, previous_sha})
    log_line(base, f"deployed {sha} on :{PORT}; tailnet: {tailnet_state}")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-id", type=int, default=None,
                        help="pin a specific ci.yml run (manual deploy/rollback)")
    args = parser.parse_args()
    try:
        return tick(real_run, real_http, run_id=args.run_id)
    except Refused as e:
        try:
            log_line(BASE, f"tick failed: {e}")
        except OSError:
            pass
        print(f"staging-deploy: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
