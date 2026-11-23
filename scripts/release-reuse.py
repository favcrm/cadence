#!/usr/bin/env python3
"""Build once per git tree, deploy the exact tested binary (CAD-1131).

`pack` (merge_group run): assemble the queue-built release pair with a
manifest that records their git tree hash and both executable digests.
`reuse` (push to main): fetch that artifact from the queue run that tested
this exact sha, verify it, and write the manifest main attests. Every doubt
raises ValueError and exits 1; the caller then rebuilds (fail closed).
The artifact is never executed here.
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

FULL = re.compile(r"[0-9a-f]{40}")
QUEUE_JOB = "release-queue-build"
FILES = {"cadence", "cadence.sha256", "manifest.json", "cadence-runtime-owner", "cadence-runtime-owner.sha256"}


def sha256_of(path):
    with Path(path).open("rb") as binary:
        return hashlib.file_digest(binary, "sha256").hexdigest()


def git_tree(checkout, sha):
    """Tree hash of `sha`, only if the checkout's HEAD is exactly `sha`."""
    def git(*args):
        return subprocess.check_output(["git", "-C", str(checkout), *args], text=True).strip()
    if not FULL.fullmatch(sha) or git("rev-parse", "--verify", "HEAD") != sha:
        raise ValueError("checkout does not match the source sha")
    tree = git("rev-parse", "--verify", f"{sha}^{{tree}}")
    if not FULL.fullmatch(tree):
        raise ValueError("no tree hash for the source sha")
    return tree


def verify_candidate(dest, sha, tree):
    """Return both digests if `dest` holds the tested build of `tree`."""
    dest = Path(dest)
    if {p.name for p in dest.iterdir()} != FILES or any(p.is_symlink() for p in dest.iterdir()):
        raise ValueError("artifact does not hold exactly the release pair, their checksums and manifest")
    manifest = json.loads((dest / "manifest.json").read_text())
    digest = sha256_of(dest / "cadence")
    owner_digest = sha256_of(dest / "cadence-runtime-owner")
    # The tree is the key: a different tree never reuses an artifact.
    if manifest.get("tree") != tree or not FULL.fullmatch(tree):
        raise ValueError("artifact was built from a different tree")
    # The binary bakes its commit in; another commit with the same tree
    # would misreport the deployed revision, so it is rebuilt.
    if manifest.get("source_sha") != sha:
        raise ValueError("artifact was built from a different commit")
    if (manifest.get("sha256") != digest
            or (dest / "cadence.sha256").read_text().split() != [digest, "cadence"]):
        raise ValueError("artifact digest does not match its manifest")
    if (manifest.get("owner_sha256") != owner_digest
            or (dest / "cadence-runtime-owner.sha256").read_text().split() != [owner_digest, "cadence-runtime-owner"]):
        raise ValueError("runtime owner digest does not match its manifest")
    if (manifest.get("target") != "x86_64-linux" or manifest.get("features") != ["ui"]
            or manifest.get("profile") != "release"):
        raise ValueError("artifact is not the release x86_64-linux ui build")
    if sha.encode() not in (dest / "cadence").read_bytes():
        raise ValueError("binary does not carry the source commit")
    return digest, owner_digest


def pack(args):
    tree = git_tree(".", args.sha)
    dest = Path(args.dest)
    dest.mkdir(parents=True, exist_ok=True)
    binary = dest / "cadence"
    shutil.copyfile(args.binary, binary)
    binary.chmod(0o755)
    digest = sha256_of(binary)
    (dest / "cadence.sha256").write_text(f"{digest}  cadence\n")
    owner = dest / "cadence-runtime-owner"
    shutil.copyfile(args.owner, owner)
    owner.chmod(0o755)
    owner_digest = sha256_of(owner)
    (dest / "cadence-runtime-owner.sha256").write_text(f"{owner_digest}  cadence-runtime-owner\n")
    manifest = {
        "source_sha": args.sha, "tree": tree, "profile": "release", "features": ["ui"],
        "target": "x86_64-linux", "runner": "ubuntu-24.04", "rustc": args.rustc,
        "cargo": args.cargo, "built_run_id": args.run_id, "sha256": digest,
        "owner_sha256": owner_digest,
        "built_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    (dest / "manifest.json").write_text(json.dumps(manifest) + "\n")
    verify_candidate(dest, args.sha, tree)
    print(json.dumps(manifest))


def gh_json(*args):
    return json.loads(subprocess.check_output(["gh", "api", *args], timeout=120))


def queue_job(repo, run_id):
    jobs = gh_json(f"repos/{repo}/actions/runs/{run_id}/jobs?filter=latest&per_page=100")["jobs"]
    return [j for j in jobs if j["name"] == QUEUE_JOB]


def reuse(args):
    repo, sha = args.repo, args.sha
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) or not FULL.fullmatch(sha):
        raise ValueError("invalid repository or sha")
    tree = git_tree(".", sha)
    run = gh_json(f"repos/{repo}/actions/runs/{args.queue_run_id}")
    expected = {"id": args.queue_run_id, "path": ".github/workflows/ci.yml",
                "event": "merge_group", "head_sha": sha}
    if (any(run.get(k) != v for k, v in expected.items())
            or not str(run.get("head_branch", "")).startswith("gh-readonly-queue/main/")
            or any(run.get(k, {}).get("full_name") != repo for k in ("repository", "head_repository"))):
        raise ValueError("queue run is not this repository's merge_group run of this sha")
    deadline = time.monotonic() + args.wait_seconds
    while True:
        jobs = queue_job(repo, args.queue_run_id)
        if len(jobs) != 1:
            raise ValueError("queue run has no single release-queue-build job")
        if jobs[0]["status"] == "completed":
            break
        if time.monotonic() >= deadline:
            raise ValueError("release-queue-build did not finish in time")
        time.sleep(20)
    if jobs[0]["conclusion"] != "success":
        raise ValueError("release-queue-build did not succeed")
    dest = Path(args.dest)
    subprocess.check_call(["gh", "run", "download", str(args.queue_run_id), "--repo", repo,
                           "--name", f"release-tree-{tree}", "--dir", str(dest)], timeout=300)
    digest, owner_digest = verify_candidate(dest, sha, tree)
    queued = json.loads((dest / "manifest.json").read_text())
    final = {
        "source_sha": sha, "tree": tree, "run_id": args.run_id, "run_attempt": args.run_attempt,
        "rustc": queued["rustc"], "cargo": queued["cargo"], "features": ["ui"],
        "target": "x86_64-linux", "runner": "ubuntu-24.04",
        "checks": ["fmt", "clippy", "build", "ui"],
        "checks_event": "merge_group", "checks_run_id": args.queue_run_id,
        "reused_from_run_id": args.queue_run_id, "sha256": digest,
        "built_at": queued["built_at"],
    }
    (dest / "manifest.json").write_text(json.dumps(final) + "\n")
    print(json.dumps(final))
    return digest, owner_digest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("pack")
    p.add_argument("--run-id", required=True, type=int)
    for name in ("binary", "owner", "sha", "dest", "rustc", "cargo"):
        p.add_argument(f"--{name}", required=True)
    r = sub.add_parser("reuse")
    for name in ("repo", "sha", "dest"):
        r.add_argument(f"--{name}", required=True)
    for name in ("queue-run-id", "run-id", "run-attempt"):
        r.add_argument(f"--{name}", required=True, type=int)
    r.add_argument("--wait-seconds", type=int, default=900)
    args = parser.parse_args()
    if args.command == "pack":
        pack(args)
    else:
        digest, owner_digest = reuse(args)
        out = os.environ.get("GITHUB_OUTPUT")
        if out:
            with open(out, "a") as f:
                f.write(f"sha256={digest}\n")
                f.write(f"owner_sha256={owner_digest}\n")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        print(f"release-reuse: {error}", file=sys.stderr)
        sys.exit(1)
