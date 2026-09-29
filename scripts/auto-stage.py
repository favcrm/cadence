#!/usr/bin/env python3
"""Bounded automatic selector for attested main staging candidates (CAD-770).

The selector chooses only a successful, attested main CI artifact on a
bounded release train: it scans recent completed ci.yml push runs on main,
picks the newest eligible one, and pins source SHA, CI run ID and attempt,
and binary digest before staging. A moving main cannot silently change an
in-flight candidate because every downstream step consumes the pins.

The previous production release baseline comes from an explicit
operator-maintained pinned file (see config/production-baseline.json.example
for the exact fields). The selector refuses automatic staging when the
baseline is absent, ambiguous, or unverified; it never guesses the baseline
from a previous main CI run.

Decisions (selection receipt `decision` field):
  stage     -- a new eligible candidate with a verified baseline; pins set.
  duplicate -- the candidate already has a successful staging receipt.
  skip      -- a staging run is already in flight (coalesce to next tick).
  refuse    -- no eligible candidate, no usable baseline, or a pin failed.

Business refusals exit 0 with a machine-readable receipt. Only operational
failures (GitHub API errors, unwritable output) exit nonzero.

Staging itself has no production access or credentials and never claims a
rollout lease, installs, or restarts. Production approval stays separate.
"""
import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

SCHEMA = 1
SHA_RE = re.compile(r"[0-9a-f]{40}")
DIGEST_RE = re.compile(r"[0-9a-f]{64}")
RECEIPT_PREFIX = "staging-receipt-"


class BaselineError(ValueError):
    pass


class NoCandidateError(ValueError):
    pass


def utcnow():
    return dt.datetime.now(dt.timezone.utc)


def parse_time(value):
    return dt.datetime.fromisoformat(str(value).replace("Z", "+00:00"))


def check_run(run, repo, run_id):
    """Mirror of delivery-candidate.validate_ci so selection refuses the
    same forged identities (failed runs, forks, wrong workflow/branch)."""
    expected = {
        "id": run_id, "path": ".github/workflows/ci.yml", "head_branch": "main",
        "event": "push", "status": "completed", "conclusion": "success",
    }
    if any(run.get(k) != v for k, v in expected.items()):
        raise ValueError("candidate must be a successful ci.yml push run on main")
    if any(run.get(k, {}).get("full_name") != repo for k in ("repository", "head_repository")):
        raise ValueError("candidate belongs to another repository or fork")
    if not SHA_RE.fullmatch(run.get("head_sha", "")):
        raise ValueError("candidate has no full source SHA")
    if type(run.get("run_attempt")) is not int or run["run_attempt"] < 1:
        raise ValueError("candidate has no run attempt")


def load_baseline(path):
    """Read the operator-pinned production baseline or fail closed."""
    try:
        data = json.loads(Path(path).read_text())
    except FileNotFoundError:
        raise BaselineError("missing-baseline: no operator-pinned production baseline at " + str(path))
    except (OSError, ValueError) as error:
        raise BaselineError(f"unreadable-baseline: {error}")
    if not isinstance(data, dict):
        raise BaselineError("ambiguous-baseline: baseline file must hold one object")
    entries = data.get("baselines", [data])
    if not isinstance(entries, list) or len(entries) != 1 or not isinstance(entries[0], dict):
        raise BaselineError("ambiguous-baseline: baseline file must pin exactly one production release")
    entry = entries[0]
    for key in ("ci_run_id", "source_sha", "sha256", "verified_by", "verified_at", "evidence"):
        if not entry.get(key):
            raise BaselineError(f"unverified-baseline: baseline is missing {key}")
    if type(entry["ci_run_id"]) is not int or entry["ci_run_id"] < 1:
        raise BaselineError("unverified-baseline: baseline ci_run_id is not a positive run id")
    if not SHA_RE.fullmatch(entry["source_sha"]):
        raise BaselineError("unverified-baseline: baseline source_sha is not a full commit SHA")
    if not DIGEST_RE.fullmatch(entry["sha256"]):
        raise BaselineError("unverified-baseline: baseline sha256 is not a binary digest")
    return entry


def verify_baseline_run(entry, run):
    """The pinned baseline run must itself be a successful main CI run for
    the pinned SHA. Anything else refuses: never inherit a main run."""
    if run is None:
        raise BaselineError("unverified-baseline: pinned baseline run was not found via the API")
    try:
        check_run(run, run["repository"]["full_name"], entry["ci_run_id"])
    except (ValueError, KeyError, TypeError) as error:
        raise BaselineError(f"unverified-baseline: pinned run failed identity checks: {error}")
    if run["head_sha"] != entry["source_sha"]:
        raise BaselineError("unverified-baseline: baseline source_sha does not match its pinned run")
    return True


def select_candidate(runs, repo, now, window_hours):
    """Pick the newest eligible run. Returns (run, refused) where refused
    lists (run_id, reason) for visibly refused runs: failed CI, stale runs
    outside the train window, forks, and wrong workflow identity."""
    refused = []
    cutoff = now - dt.timedelta(hours=window_hours)
    for run in runs:
        run_id = run.get("id")
        try:
            created = parse_time(run.get("created_at", ""))
        except (ValueError, TypeError):
            refused.append((run_id, "missing-timestamp"))
            continue
        if created < cutoff:
            refused.append((run_id, "stale-outside-train-window"))
            continue
        try:
            check_run(run, repo, run_id)
        except ValueError as error:
            refused.append((run_id, str(error)))
            continue
        return run, refused
    raise NoCandidateError(
        "no eligible attested main candidate in the train window; refused: "
        + json.dumps(refused))


def check_on_main(compare_status):
    if compare_status not in ("identical", "ahead"):
        raise NoCandidateError("candidate is not on main (compare status: %s)" % compare_status)


def check_digest_pin(candidate_receipt, expected_digest):
    """The staged bytes must equal the digest pinned at selection time, so
    a moving main or swapped artifact cannot silently change the candidate."""
    actual = (candidate_receipt or {}).get("sha256")
    if not actual or actual != expected_digest:
        raise ValueError("candidate digest does not match the selection pin")


def classify(candidate, baseline, staging_state, current_run_id):
    """Decide stage/duplicate/skip. staging_state holds inflight run ids
    and prior staging receipts (newest first). Superseded candidates are
    recorded, never promoted implicitly."""
    inflight = [r for r in staging_state.get("inflight", []) if r != current_run_id]
    if inflight:
        return ("skip", f"inflight-staging-run-{inflight[0]}", [], inflight[0])
    supersedes = []
    for receipt in staging_state.get("receipts", []):
        prior = (receipt or {}).get("candidate") or {}
        if (prior.get("ci_run_id") == candidate["ci_run_id"]
                and prior.get("ci_run_attempt") == candidate["ci_run_attempt"]):
            if (receipt or {}).get("decision") == "staged":
                return ("duplicate",
                        "candidate already staged by run %s" % receipt.get("staging_run_id"),
                        [], None)
            continue
        if prior.get("ci_run_id"):
            supersedes.append({k: prior.get(k) for k in
                               ("ci_run_id", "ci_run_attempt", "source_sha", "sha256")})
    return ("stage", "new eligible candidate with verified baseline", supersedes, None)


def candidate_identity(run, digest=None):
    return {"ci_run_id": run["id"], "ci_run_attempt": run["run_attempt"],
            "source_sha": run["head_sha"], "sha256": digest}


def selection_receipt(decision, reason, candidate=None, baseline=None,
                      main_head_sha=None, supersedes=None, deferred_to_run=None,
                      refused=None, repo=None, window_hours=None):
    key = None
    if candidate:
        key = "candidate:%s:%s:%s" % (candidate["ci_run_id"],
                                      candidate["ci_run_attempt"],
                                      (candidate.get("sha256") or "unpinned")[:12])
    return {"schema": SCHEMA, "kind": "selection",
            "decided_at": utcnow().isoformat(), "repo": repo,
            "train_window_hours": window_hours, "candidate": candidate,
            "baseline": baseline, "main_head_sha": main_head_sha,
            "decision": decision, "reason": reason,
            "supersedes": supersedes or [], "deferred_to_run": deferred_to_run,
            "refused_runs": refused or [], "idempotency_key": key}


def staging_receipt(trigger, staging_run_id, staging_run_attempt, candidate,
                    baseline, gates, supersedes=None, run_head_sha=None):
    order = ["mvp_journey", "migration_rehearsal", "digest_recheck"]
    failed = [name for name in order if gates.get(name) != "pass"]
    unknown = [name for name in order if gates.get(name) not in ("pass", "fail", "skip")]
    if unknown:
        decision, reason = "failed", "unknown gate outcome: %s" % ",".join(unknown)
    elif failed:
        decision, reason = "failed", "failing gate: %s" % ",".join(failed)
    elif not candidate or not candidate.get("sha256"):
        decision, reason = "failed", "candidate identity or digest pin missing"
    else:
        decision, reason = "staged", "all staging gates passed on the pinned bytes"
    key = None
    if candidate and candidate.get("ci_run_id"):
        key = "candidate:%s:%s:%s" % (candidate["ci_run_id"],
                                      candidate.get("ci_run_attempt"),
                                      str(candidate.get("sha256") or "unpinned")[:12])
    return {"schema": SCHEMA, "kind": "staging", "finished_at": utcnow().isoformat(),
            "trigger": trigger, "staging_run_id": staging_run_id,
            "staging_run_attempt": staging_run_attempt, "candidate": candidate,
            "baseline": baseline, "gates": gates, "decision": decision,
            "reason": reason, "supersedes": supersedes or [],
            "run_head_sha": run_head_sha, "idempotency_key": key}


def gh_api(*args):
    return subprocess.check_output(["gh", "api", *args], timeout=120)


def fetch_json(path, field=None):
    out = gh_api(path, "--paginate" if field else "--jq", "." if field else ".")
    data = json.loads(out)
    return data if field is None else data


CI_PATH = ".github/workflows/ci.yml"
STAGING_PATH = ".github/workflows/staging.yml"


def fetch_main_runs(repo, limit):
    # The per-workflow runs endpoint is not used: list runs generically
    # and filter to ci.yml client-side so a crowded page cannot hide the
    # candidate behind unrelated workflows' runs.
    data = json.loads(gh_api(
        f"repos/{repo}/actions/runs?branch=main&event=push&status=completed&per_page={limit * 3}"))
    runs = [r for r in data.get("workflow_runs", []) if r.get("path") == CI_PATH]
    return runs[:limit]


def fetch_run(repo, run_id):
    try:
        return json.loads(gh_api(f"repos/{repo}/actions/runs/{run_id}"))
    except subprocess.SubprocessError:
        return None


def fetch_staging_state(repo, limit_runs, limit_receipts, current_run_id):
    """List recent staging.yml runs; collect inflight ids and download up
    to limit_receipts prior staging receipts (newest first)."""
    data = json.loads(gh_api(
        f"repos/{repo}/actions/runs?per_page={limit_runs * 3}"))
    state = {"inflight": [], "receipts": []}
    completed = []
    for run in data.get("workflow_runs", []):
        if run.get("path") != STAGING_PATH:
            continue
        if run.get("id") == current_run_id:
            continue
        if run.get("status") in ("queued", "in_progress"):
            state["inflight"].append(run["id"])
        elif run.get("status") == "completed":
            completed.append(run)
    for run in completed:
        if len(state["receipts"]) >= limit_receipts:
            break
        try:
            artifacts = json.loads(gh_api(f"repos/{repo}/actions/runs/{run['id']}/artifacts"))
        except subprocess.SubprocessError:
            continue
        names = [a for a in artifacts.get("artifacts", [])
                 if a.get("name", "").startswith(RECEIPT_PREFIX) and not a.get("expired")]
        if not names:
            continue
        names.sort(key=lambda a: a.get("created_at", ""), reverse=True)
        try:
            raw = subprocess.check_output(
                ["gh", "api", f"repos/{repo}/actions/artifacts/{names[0]['id']}/zip"],
                timeout=120)
        except subprocess.SubprocessError:
            continue
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "receipt.zip"
            archive.write_bytes(raw)
            result = subprocess.run(["unzip", "-p", str(archive)], capture_output=True, timeout=60)
            if result.returncode != 0:
                continue
            try:
                state["receipts"].append(json.loads(result.stdout))
            except ValueError:
                continue
    return state


def pin_digest(repo, run_id, script_dir):
    """Download and verify the candidate via delivery-candidate.py so the
    selection receipt pins the exact binary digest before staging."""
    with tempfile.TemporaryDirectory(prefix="autostage-") as directory:
        dest = Path(directory) / "candidate"
        script = Path(script_dir) / "delivery-candidate.py"
        subprocess.check_output(
            [sys.executable, str(script), "prepare", "--repo", repo,
             "--run-id", str(run_id), "--dest", str(dest)], timeout=600)
        return json.loads((dest / "candidate.json").read_text())


def write_outputs(values):
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a") as handle:
            for key, value in values.items():
                handle.write(f"{key}={value}\n")


def cmd_select(args):
    refused, baseline, candidate, supersedes = [], None, None, []
    main_head = None
    try:
        entry = load_baseline(args.baseline_file)
        run = fetch_run(args.repo, entry["ci_run_id"])
        verify_baseline_run(entry, run)
        baseline = {"ci_run_id": entry["ci_run_id"], "source_sha": entry["source_sha"],
                    "sha256": entry["sha256"], "verified_by": entry["verified_by"],
                    "verified_at": entry["verified_at"], "evidence": entry.get("evidence")}
    except BaselineError as error:
        receipt = selection_receipt("refuse", str(error), repo=args.repo,
                                    window_hours=args.train_window_hours)
        return finish_select(args, receipt)
    try:
        runs = fetch_main_runs(args.repo, args.max_runs)
        selected, refused = select_candidate(runs, args.repo, utcnow(), args.train_window_hours)
        try:
            raw_head = gh_api(f"repos/{args.repo}/commits/main", "--jq", ".sha")
            # gh --jq prints a bare string without JSON quotes.
            main_head = raw_head.decode().strip().strip('"') or None
        except subprocess.SubprocessError:
            main_head = None
    except (NoCandidateError, subprocess.SubprocessError) as error:
        reason = str(error) if isinstance(error, NoCandidateError) else f"ops-error: {error}"
        receipt = selection_receipt("refuse", reason, baseline=baseline, repo=args.repo,
                                    window_hours=args.train_window_hours, refused=refused)
        return finish_select(args, receipt)
    try:
        compare = json.loads(gh_api(
            f"repos/{args.repo}/compare/{selected['head_sha']}...main"))
        check_on_main(compare.get("status"))
    except (NoCandidateError, subprocess.SubprocessError) as error:
        reason = str(error) if isinstance(error, NoCandidateError) else f"ops-error: {error}"
        receipt = selection_receipt(
            "refuse", reason,
            candidate=candidate_identity(selected), baseline=baseline,
            main_head_sha=main_head, repo=args.repo,
            window_hours=args.train_window_hours, refused=refused)
        return finish_select(args, receipt)
    state = fetch_staging_state(args.repo, args.max_staging_runs,
                                args.max_receipts, args.current_run_id)
    decision, reason, supersedes, deferred = classify(
        {"ci_run_id": selected["id"], "ci_run_attempt": selected["run_attempt"]},
        baseline, state, args.current_run_id)
    digest = None
    if decision == "stage":
        try:
            pinned = pin_digest(args.repo, selected["id"], Path(__file__).resolve().parent)
            check_digest_pin(pinned, pinned["sha256"])
            if (pinned["source_sha"] != selected["head_sha"]
                    or pinned["ci_run_id"] != selected["id"]
                    or pinned["ci_run_attempt"] != selected["run_attempt"]):
                raise ValueError("verified artifact does not match the selected run")
            digest = pinned["sha256"]
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            decision, reason = "refuse", f"digest-pin-failed: {error}"
    candidate = candidate_identity(selected, digest)
    receipt = selection_receipt(decision, reason, candidate=candidate, baseline=baseline,
                                main_head_sha=main_head, supersedes=supersedes,
                                deferred_to_run=deferred, refused=refused,
                                repo=args.repo, window_hours=args.train_window_hours)
    return finish_select(args, receipt)


def finish_select(args, receipt):
    Path(args.out).write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))
    candidate = receipt.get("candidate") or {}
    baseline = receipt.get("baseline") or {}
    write_outputs({
        "decision": receipt["decision"],
        "reason": receipt["reason"],
        "source_sha": candidate.get("source_sha") or "",
        "ci_run_id": candidate.get("ci_run_id") or "",
        "ci_run_attempt": candidate.get("ci_run_attempt") or "",
        "sha256": candidate.get("sha256") or "",
        "baseline_ci_run_id": baseline.get("ci_run_id") or "",
        "supersedes": json.dumps(receipt.get("supersedes") or []),
        "receipt": str(args.out),
    })


def read_json(path):
    try:
        data = json.loads(Path(path).read_text())
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def gate_outcome(value):
    """Translate Actions step outcomes to the receipt's gate vocabulary.

    Unknown outcomes remain unknown so staging_receipt fails closed while
    still writing a useful failure receipt.
    """
    return {"success": "pass", "failure": "fail", "skipped": "skip"}.get(value, value)


def cmd_staging_receipt(args):
    candidate = read_json(args.candidate_json)
    baseline = read_json(args.baseline_json)
    if args.expected_digest:
        try:
            check_digest_pin(candidate, args.expected_digest)
            digest_gate = gate_outcome(args.digest_recheck)
        except ValueError as error:
            print(f"auto-stage: {error}", file=sys.stderr)
            digest_gate = "fail"
    else:
        digest_gate = gate_outcome(args.digest_recheck)
    gates = {"mvp_journey": gate_outcome(args.mvp),
             "migration_rehearsal": gate_outcome(args.rehearsal),
             "digest_recheck": digest_gate}
    try:
        supersedes = json.loads(args.supersedes_json) if args.supersedes_json else []
        if not isinstance(supersedes, list):
            supersedes = []
    except ValueError:
        supersedes = []
    receipt = staging_receipt(args.trigger, args.staging_run_id, args.staging_run_attempt,
                              candidate or None,
                              baseline or {"unverified": True, "reason": "baseline receipt missing"},
                              gates, supersedes, args.run_head_sha)
    Path(args.out).write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    select = commands.add_parser("select")
    select.add_argument("--repo", required=True)
    select.add_argument("--baseline-file", required=True)
    select.add_argument("--out", required=True)
    select.add_argument("--train-window-hours", type=float, default=24)
    select.add_argument("--max-runs", type=int, default=20)
    select.add_argument("--max-staging-runs", type=int, default=10)
    select.add_argument("--max-receipts", type=int, default=5)
    select.add_argument("--current-run-id", type=int, default=0)
    receipt = commands.add_parser("staging-receipt")
    receipt.add_argument("--trigger", required=True)
    receipt.add_argument("--staging-run-id", type=int, required=True)
    receipt.add_argument("--staging-run-attempt", type=int, required=True)
    receipt.add_argument("--candidate-json", required=True)
    receipt.add_argument("--baseline-json", required=True)
    receipt.add_argument("--mvp", required=True)
    receipt.add_argument("--rehearsal", required=True)
    receipt.add_argument("--digest-recheck", required=True)
    receipt.add_argument("--expected-digest", default="")
    receipt.add_argument("--supersedes-json", default="")
    receipt.add_argument("--run-head-sha", default="")
    receipt.add_argument("--out", required=True)
    args = parser.parse_args(argv)
    if args.command == "select":
        cmd_select(args)
    else:
        cmd_staging_receipt(args)


if __name__ == "__main__":
    try:
        main()
    except subprocess.SubprocessError as error:
        print(f"auto-stage: GitHub API failure: {error}", file=sys.stderr)
        sys.exit(2)
