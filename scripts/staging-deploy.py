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
import hashlib
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
# Sandbox children get nothing CADENCE_* the caller exported — an
# allowlist, not a denylist. CADENCE_ALIAS/PROFILE/PM_DIR would alias
# the child to the caller's identity or tracker, and client::rpc
# honours CADENCE_SOCKET over CADENCE_STATE_DIR, so an inherited one
# would route `sandbox down`/`issue agent` at the production daemon.
# The two below are the only ones the deploy adds itself; sandbox env
# lines (STATE_DIR/PM_DIR/PROFILE) arrive via `extra`.


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


def child_env(extra=None, base=BASE):
    env = {k: v for k, v in os.environ.items() if not k.startswith("CADENCE_")}
    env["CADENCE_SANDBOX_ROOT"] = str(base / "sandbox")
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


def sandbox_env(run, cadence, base=BASE):
    """Parse `cadence sandbox env` export lines into an env dict."""
    out = run_ok(run, [cadence, "sandbox", "env", NAME], child_env(base=base))
    env = child_env(base=base)
    for line in out.splitlines():
        m = re.fullmatch(r"export (\w+)='(.*)'", line.strip())
        if m:
            env[m.group(1)] = m.group(2).replace("'\\''", "'")
    for key in ("CADENCE_STATE_DIR", "CADENCE_PM_DIR", "CADENCE_PROFILE"):
        if key not in env:
            raise Refused(f"sandbox env is missing {key}")
    return env


def sandbox_down(run, cadence, base=BASE):
    code, out, err = run(
        [cadence, "sandbox", "down", NAME], env=child_env(base=base)
    )
    if code != 0 and "no sandbox" not in (err + out):
        raise Refused(
            f"sandbox down failed ({code}): {(err or out).strip()[:500]}"
        )


def health_ok(http_get):
    """The live build commit, or False. /api/health is 200 even with a
    dead daemon ({"ok": false, "daemon": "unreachable"}), so the body —
    not the status — decides."""
    try:
        status, body = http_get(
            f"http://127.0.0.1:{PORT}/api/health",
            f"cadence-{PORT}.localhost:{PORT}",
        )
        if status != 200:
            return False
        health = json.loads(body)
        if not health.get("ok") or health.get("daemon") != "reachable":
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


def verified_binary(release_dir):
    """The release's cadence binary, only while it still proves what
    prepare attested: candidate.json identifies the source/run/attempt
    and the file's live sha256 matches it. Re-hashed on every call —
    before every execution, including rollback."""
    try:
        candidate = json.loads((release_dir / "candidate.json").read_text())
        binary = release_dir / "cadence"
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    except (OSError, ValueError):
        return None
    if not all(candidate.get(k) for k in ("source_sha", "ci_run_id", "ci_run_attempt")):
        return None
    if candidate.get("sha256") != digest:
        return None
    return str(binary)


def release_identity(dest):
    """The receipt's `<sha>-<run>-<attempt>` — what prepare actually
    fetched, or None when unreadable."""
    try:
        c = json.loads((dest / "candidate.json").read_text())
        return f"{c['source_sha']}-{c['ci_run_id']}-{c['ci_run_attempt']}"
    except Exception:
        return None


def ensure_release(run, rel_id, run_id, base):
    """releases/<sha>-<run_id>-<attempt>: reuse only a still-verified
    directory, otherwise run `prepare` — a CI rerun's artifacts land in
    a new dir."""
    dest = base / "releases" / rel_id
    if dest.is_dir():
        if verified_binary(dest) and release_identity(dest) == rel_id:
            return dest
        raise Refused(
            f"cached release {rel_id} failed verification — not executing it"
        )
    partial = base / "releases" / f"{rel_id}.partial-{os.getpid()}"
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
            shutil.rmtree(partial, ignore_errors=True)
    if not verified_binary(dest):
        raise Refused(f"prepared release {rel_id} failed verification")
    # A rerun between run selection and prepare resolves a newer
    # attempt than rel_id names — refuse rather than record wrong.
    if release_identity(dest) != rel_id:
        raise Refused(
            f"prepare returned {release_identity(dest)}, not the "
            f"selected {rel_id} — the run's attempt moved mid-tick"
        )
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
    `tailnet` field text.

    The board's own status — not the stored status.json string — says
    whether the serve mapping is live: a mapping the operator removed
    (or tailscaled dropped) shows `absent` even while the persisted
    board options still say sharing. An `absent` mapping is republished
    through `ui tailscale start`; a foreign one on the port is named
    and left alone — that command refuses to overwrite it anyway."""
    try:
        code, out, _ = run([cadence, "ui", "tailscale", "status"], env=env)
        if code != 0:
            return f"refused: tailscale status exited {code}"
        sharing = "sharing: on" in out
        mapping_live = "(live)" in out
        foreign = "NOT ours" in out
        needs_publish = not sharing or (sharing and not mapping_live and not foreign)
        if needs_publish:
            code, _, err = run(
                [cadence, "ui", "tailscale", "start", "--port", str(TS_PORT)],
                env=env,
            )
            if code != 0:
                return f"refused: {err.strip()[:300]}"
        if foreign and not mapping_live:
            return f"refused: foreign mapping on :{TS_PORT} — left alone"
        return f"sharing at {URL_TAILNET}"
    except Exception as e:  # never fatal
        return f"error: {e}"


def probe_tailnet(http_get):
    """Non-fatal reachability probe through the tailnet URL itself."""
    try:
        code, _ = http_get(
            f"{URL_TAILNET}/api/health",
            "ip-172-31-1-32.tail9fcf30.ts.net:9460",
        )
        return "ok" if code == 200 else f"http {code}"
    except Exception as e:
        return str(e)[:200]


def refresh_tailnet(run, http_get, cadence, base, expected_sha):
    """Reconcile sharing without leaving a previously healthy board down.

    Tailnet publication and sandbox environment lookup are best-effort, but
    `ui tailscale start` may restart the board. Verify the loopback build
    afterward and recover the same attested release if that restart failed.
    """
    try:
        env = sandbox_env(run, cadence, base)
        tailnet_state = tailnet(run, cadence, env)
    except Exception as e:
        tailnet_state = f"error: sandbox env: {str(e)[:200]}"

    live = health_ok(http_get)
    if live != expected_sha:
        try:
            sandbox_down(run, cadence, base)
            run_ok(
                run,
                [cadence, "sandbox", "up", NAME, "--port", str(PORT)],
                child_env(base=base),
            )
        except Exception as e:
            raise Refused(
                f"tailnet check left staging unhealthy ({live!r}); "
                f"same-release recovery failed: {e}"
            ) from e
        live = health_ok(http_get)
        if live != expected_sha:
            raise Refused(
                f"tailnet check left staging unhealthy ({live!r}); "
                f"recovery expected {expected_sha}"
            )
        tailnet_state = f"refused: board recovered after tailnet check ({tailnet_state})"

    tailnet_health = (
        probe_tailnet(http_get)
        if tailnet_state.startswith("sharing")
        else None
    )
    return tailnet_state, tailnet_health


def prune(base, keep_shas):
    releases = sorted(
        (p for p in (base / "releases").iterdir()
         # New ids carry run+attempt; a bare-sha dir is a pre-migration
        # release and prunes the same way.
        if p.is_dir() and re.fullmatch(r"[0-9a-f]{40}(-\d+-\d+)?", p.name)),
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
             "--limit", "1", "--json", "databaseId,headSha,attempt"],
            os.environ.copy(),
        )
        candidate = json.loads(out)[0]
        run_id, sha = candidate["databaseId"], candidate["headSha"]
        attempt = candidate["attempt"]
    else:
        out = run_ok(
            run,
            ["gh", "api", f"repos/{REPO}/actions/runs/{run_id}"],
            os.environ.copy(),
        )
        run_info = json.loads(out)
        sha, attempt = run_info["head_sha"], run_info["run_attempt"]
    # A rerun for the same source is a different release — the dir id
    # carries run and attempt, so a rerun never reuses stale artifacts.
    rel_id = f"{sha}-{run_id}-{attempt}"

    # An auto-selected release that already failed once is skipped —
    # each attempt bounces the live board. `--run-id` pins are the
    # operator's call and bypass this. Skip only while staging is
    # verifiably alive: if the board is down or on the wrong build,
    # recover the previous verified release when there is one, else
    # retry the candidate (staging is down anyway — nothing to bounce).
    if not pinned and rel_id == status.get("failed_release"):
        live = health_ok(http_get)
        if status.get("deployed_release") and live == status.get("deployed_sha"):
            deployed_bin = verified_binary(
                base / "releases" / status["deployed_release"]
            )
            if deployed_bin:
                try:
                    status["tailnet"], status["tailnet_health"] = refresh_tailnet(
                        run, http_get, deployed_bin, base, status["deployed_sha"]
                    )
                except Refused as e:
                    status["last_error"] = str(e)
                    status_path.write_text(json.dumps(status, indent=2) + "\n")
                    log_line(base, f"tick: tailnet recovery failed: {e}")
                    return 1
                status_path.write_text(json.dumps(status, indent=2) + "\n")
            log_line(base, f"tick: skipping known-bad {rel_id[:12]}")
            return 0
        prev_rel = status.get("previous_release")
        prev_bin = (
            verified_binary(base / "releases" / prev_rel) if prev_rel else None
        )
        if prev_rel and prev_rel != rel_id and prev_bin:
            log_line(base, f"tick: known-bad {rel_id[:12]} — recovering {prev_rel[:12]}")
            try:
                sandbox_down(run, prev_bin, base)
                run_ok(
                    run,
                    [prev_bin, "sandbox", "up", NAME, "--port", str(PORT)],
                    child_env(base=base),
                )
                live = health_ok(http_get)
                prev_sha = status.get("previous_sha")
                if live != prev_sha:
                    raise Refused(
                        f"recovered board reports build_commit={live!r}, want {prev_sha}"
                    )
            except Exception as e:
                status["last_error"] = f"recovery to {prev_rel} failed: {e}"
                status_path.write_text(json.dumps(status, indent=2) + "\n")
                log_line(base, f"tick: recovery failed: {e}")
                return 1
            status.update({
                "deployed_release": prev_rel,
                "deployed_sha": prev_sha,
                "last_error": status.get("last_error"),
            })
            status_path.write_text(json.dumps(status, indent=2) + "\n")
            log_line(base, f"tick: recovered on {prev_rel[:12]}")
            return 0
        # Nothing verified to recover: fall through to a normal retry.

    deployed = health_ok(http_get)
    if rel_id == status.get("deployed_release") and deployed == sha:
        # Already live. The tailnet mapping is still revalidated against
        # the board every tick — a stored "sharing" is only what the
        # last tick saw, and a removed/foreign mapping while staging was
        # live never heals itself. `tailnet()` republishes an absent
        # mapping and probes reachability; the CLI refuses to overwrite
        # a foreign route, and a refusal is recorded, not fatal.
        cadence = verified_binary(base / "releases" / rel_id)
        if cadence:
            try:
                status["tailnet"], status["tailnet_health"] = refresh_tailnet(
                    run, http_get, cadence, base, sha
                )
            except Refused as e:
                status["last_error"] = str(e)
                status_path.write_text(json.dumps(status, indent=2) + "\n")
                log_line(base, f"tick: tailnet recovery failed: {e}")
                return 1
            status_path.write_text(json.dumps(status, indent=2) + "\n")
        log_line(base, f"tick: no-op — {rel_id[:12]} already live")
        return 0

    release = ensure_release(run, rel_id, run_id, base)
    cadence = verified_binary(release)
    if cadence is None:  # cannot happen after ensure_release — belt and braces
        raise Refused(f"release {rel_id} produced no verified binary")
    env = child_env(base=base)

    previous_release = status.get("deployed_release")
    previous_sha = status.get("deployed_sha")
    if rel_id == previous_release:
        # Repairing the live release: its own rollback target is the
        # one that must survive — keep the recorded distinct fallback
        # or a failed restart would leave nothing else to try.
        previous_release = status.get("previous_release")
        previous_sha = status.get("previous_sha")
    log_line(base, f"deploying {rel_id} (previous {previous_release})")
    sandbox_down(run, cadence, base)

    try:
        run_ok(run, [cadence, "sandbox", "up", NAME, "--port", str(PORT)], env)
        try:
            if not (base / "seeded").exists():
                seed(run, cadence, sandbox_env(run, cadence, base), base)
        except Exception as e:
            # A partial seed leaves projects in the tracker and the seed
            # then refuses every retry — reset the sandbox (never when
            # the seeded marker exists) so the next tick starts clean.
            # A failed reset means the tracker is in an unknown state.
            rc, _, reset_err = run(
                [cadence, "sandbox", "reset", NAME], env=child_env(base=base)
            )
            if rc != 0:
                raise Refused(
                    f"seed failed ({e}); sandbox reset ALSO failed ({rc}): "
                    f"{reset_err.strip()[:300]} — manual repair needed"
                )
            raise Refused(f"seed failed ({e}); staging sandbox reset")
        tailnet_state = tailnet(run, cadence, sandbox_env(run, cadence, base))
        tailnet_health = (
            probe_tailnet(http_get)
            if tailnet_state.startswith("sharing")
            else None
        )
        healthy = health_ok(http_get)
        if healthy != sha:
            raise Refused(
                f"health check failed: /api/meta build_commit={healthy!r}, want {sha}"
            )
    except Exception as e:
        last_error = str(e)
        log_line(base, f"deploy {rel_id[:12]} failed: {last_error} — rolling back")
        rolled_back = False
        if previous_release:
            prev_bin = verified_binary(base / "releases" / previous_release)
            if prev_bin:
                try:
                    sandbox_down(run, prev_bin, base)
                    run_ok(
                        run,
                        [prev_bin, "sandbox", "up", NAME, "--port", str(PORT)],
                        env,
                    )
                    rolled_back = True
                except Exception as rb:
                    last_error += f"; rollback also failed: {rb}"
        status.update({
            "last_error": last_error,
            "failed_release": rel_id,
            "previous_release": previous_release,
            "previous_sha": previous_sha,
            "tailnet": status.get("tailnet"),
        })
        if rolled_back:
            # The fallback's `sandbox up` only returns 0 once its board
            # answers health, and verified_binary bound that release dir
            # to previous_sha — the fallback is what is verifiably live.
            # Record it, or the next tick reads the healthy fallback as
            # a wrong-build mismatch and bounces it again.
            status["deployed_release"] = previous_release
            status["deployed_sha"] = previous_sha
        else:
            # Nothing verifiably live: the no-op check must not claim
            # the old release is still deployed.
            status["deployed_release"] = None
            status["deployed_sha"] = None
        status_path.write_text(json.dumps(status, indent=2) + "\n")
        return 1

    status = {
        "deployed_release": rel_id,
        "deployed_sha": sha,
        "ci_run_id": run_id,
        "ci_run_attempt": attempt,
        "deployed_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "previous_release": previous_release,
        "previous_sha": previous_sha,
        "tailnet": tailnet_state,
        "tailnet_health": tailnet_health,
        "last_error": None,
        "url_loopback": URL_LOOPBACK,
        "url_tailnet": URL_TAILNET,
    }
    status_path.write_text(json.dumps(status, indent=2) + "\n")
    prune(base, {rel_id, previous_release})
    log_line(base, f"deployed {rel_id} on :{PORT}; tailnet: {tailnet_state}")
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
