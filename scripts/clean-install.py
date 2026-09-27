#!/usr/bin/env python3
"""CAD-317 clean-machine install-only smoke, pre- and post-publication.

Three commands; only candidate acquisition uses a read-only token.

  acquire-candidate  verify an exact tag-push ci.yml run (canonical repo,
                     head_branch tag, peeled tag ref, source on main, all
                     five gates + release-gate + the three release-build
                     legs successful via the attempt jobs endpoint), pick
                     the unique release-<tag>-<target> artifact by id,
                     verify its ZIP, the tarball's outer/inner sha256 and
                     a strict manifest, then fetch install.sh by GET at
                     the exact source sha. Missing/pending inventory is
                     polled for at most 1200s every 30s; any terminal
                     failure fails immediately — it never waits, skips or
                     retries past it. Evidence only: nothing here proves
                     public availability or publisher authenticity.

  acquire-published  anonymously GET install.sh, the tarball and its
                     .sha256 from the real github.com release download
                     URLs — no token, no gh, no mirror/base-url flag.

  harness            run install.sh on a restricted PATH (allowlisted
                     POSIX tools + git/curl/tar/sha256 only; cargo/
                     rustup/node/npm/gh provably absent) inside an owned
                     0700 /tmp root with an explicit-allowlist env, in a
                     default install and a custom --prefix under a
                     separate fresh root each, then a rerun. Binary
                     digest, installed manifest, --version and the link
                     are checked exactly; inode/mtime/content/link must be
                     stable across the rerun. The first failed step stops
                     the harness and is recorded with its exit and logs.

Setup/wizard and backup->restore are explicitly pending in this bounded
increment (full_cad317_acceptance: false); on macOS the daemon is
blocked by CAD-315. No repository binary executes in unit tests.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import zipfile

REPO = "favcrm/cadence"
REPO_RE = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
SHA_RE = re.compile(r"[0-9a-f]{40}")
SHA256_RE = re.compile(r"[0-9a-f]{64}")
# install.sh refuses tags outside [0-9A-Za-z.+_-] or containing "..".
TAG_RE = re.compile(r"v[0-9][0-9A-Za-z.+_-]*")
TARGETS = {
    "x86_64-linux": "ubuntu-22.04",
    "aarch64-linux": "ubuntu-22.04-arm",
    "aarch64-macos": "macos-14",
}
GATE_JOBS = ["fmt", "clippy", "test", "build", "ui"]
TAR_MEMBERS = {"cadence", "cadence.sha256", "manifest.json"}
TIMEOUT = 120
WAIT_LIMIT = 1200
WAIT_POLL = 30
PAGE_LIMIT = 50
BLOB_LIMIT = 200 * 1024 * 1024
# Tools the installed product may see on PATH; never a whole /usr/bin,
# which on hosted runners also carries node and gh.
PATH_ALLOWLIST = [
    "sh", "uname", "sysctl", "mktemp", "mkdir", "cp", "mv", "rm", "rmdir",
    "ln", "readlink", "chmod", "cat", "sed", "cut", "tr", "head", "tail",
    "sort", "pwd", "dirname", "basename", "env", "id", "test", "sleep", "cmp",
    "grep", "find", "xargs", "git", "curl", "tar", "gzip",
    "sha256sum", "shasum", "openssl", "date", "ps", "kill", "chown",
    "ls", "wc", "awk", "diff", "touch", "true", "false", "hostname",
    "whoami", "df", "du",
]
FORBIDDEN_ON_PATH = ["rustc", "cargo", "rustup", "node", "npm", "pnpm", "gh"]


# --- strict metadata -------------------------------------------------------


def require(cond, message):
    if not cond:
        raise ValueError(message)


def strict_int(value, label):
    require(type(value) is int and value >= 1, f"{label} must be a positive int")
    return value


def strict_str(value, label):
    require(type(value) is str and value, f"{label} must be a non-empty string")
    return value


def valid_inputs(repo, tag, sha, target):
    for value, name in ((repo, "repository"), (tag, "tag"),
                        (sha, "source"), (target, "target")):
        strict_str(value, name)
    require(REPO_RE.fullmatch(repo or ""), "invalid repository")
    require(repo == REPO, f"canonical repository is {REPO}, not {repo!r}")
    require(TAG_RE.fullmatch(tag or "") and ".." not in tag,
            f"not a release tag: {tag!r}")
    require(SHA_RE.fullmatch(sha or ""), "source must be a full 40-hex SHA")
    require(target in TARGETS, f"unsupported target: {target!r}")


def validate_run(run, repo, run_id, sha, tag, attempt):
    """An exact tag-push ci.yml run. Tag pushes carry head_branch and a
    null ref, so the tag binds through head_branch plus the peeled tag
    ref (peel_tag_ref), not `ref`."""
    valid_inputs(repo, tag, sha, "x86_64-linux")
    strict_int(run_id, "run id")
    strict_int(attempt, "attempt")
    require(type(run) is dict, "run metadata is not an object")
    require(type(run.get("id")) is int and run["id"] == run_id,
            "run id does not match the request")
    for key in ("repository", "head_repository"):
        repo_meta = run.get(key)
        require(type(repo_meta) is dict and repo_meta.get("full_name") == repo,
                "candidate belongs to another repository or fork")
    require(run.get("path") == ".github/workflows/ci.yml",
            "candidate is not a ci.yml run")
    require(run.get("event") == "push", "candidate is not a push run")
    require(run.get("head_sha") == sha,
            "run head SHA differs from the requested source")
    require(run.get("head_branch") == tag,
            "run head_branch differs from the requested tag")
    require(run.get("run_attempt") == attempt
            and type(run.get("run_attempt")) is int,
            "run attempt differs from the requested attempt")
    status = run.get("status")
    require(status in ("queued", "in_progress", "waiting", "completed"),
            f"unexpected run status {status!r}")
    # A terminal run that did not succeed fails immediately; it is never
    # waited on, skipped or treated as pending.
    if status == "completed":
        require(run.get("conclusion") == "success",
                f"run completed with {run.get('conclusion')!r}")
    else:
        require(run.get("conclusion") is None, "contradictory pending run")


def peel_tag_ref(ref_obj, tag_object):
    """Exact peeled tag: the immutable refs/tags/<tag> object resolves to
    the source commit, through one annotated-tag hop when needed."""
    require(type(ref_obj) is dict and type(ref_obj.get("object")) is dict,
            "tag ref payload malformed")
    obj = ref_obj["object"]
    require(SHA_RE.fullmatch(strict_str(obj.get("sha"), "tag object sha")),
            "tag object sha malformed")
    if obj.get("type") == "commit":
        return obj["sha"]
    require(obj.get("type") == "tag", f"tag ref points at {obj.get('type')!r}")
    require(type(tag_object) is dict and tag_object.get("sha") == obj["sha"],
            "annotated tag object does not match the ref")
    target = tag_object.get("object") or {}
    require(target.get("type") == "commit"
            and SHA_RE.fullmatch(strict_str(target.get("sha"), "peeled sha")),
            "annotated tag does not peel to a commit")
    return target["sha"]


def jobs_status(jobs):
    """Classify the attempt's complete job inventory.

    -> ("ok", jobs)        every required job exists once and succeeded
    -> ("pending", names)  required jobs missing or not yet terminal
    -> ("failed", names)   any required job reached a terminal
                           non-success; callers fail immediately
    """
    require(type(jobs) is list, "malformed job inventory")
    for job in jobs:
        require(type(job) is dict and type(job.get("name")) is str,
                "job entry malformed")
        if job["name"] in GATE_JOBS + ["release-gate"] or job["name"].startswith("release-build"):
            require(job.get("status") in ("queued", "in_progress", "completed"),
                    "unrecognized required job status")
            if job.get("status") != "completed":
                require(job.get("conclusion") is None, "contradictory pending job")
    require(not any(j["name"].startswith("release-build")
                    and not re.fullmatch(r"release-build \([^)]*\)", j["name"])
                    for j in jobs), "unrecognized release-build identity")
    required = GATE_JOBS + ["release-gate"]
    failed, pending = [], []

    def one(name):
        matches = [j for j in jobs if j["name"] == name]
        require(len(matches) <= 1, f"job {name!r}: found {len(matches)}")
        return matches[0] if matches else None

    for name in required:
        job = one(name)
        if job is None:
            pending.append(name)
        elif job.get("status") != "completed":
            pending.append(name)
        elif job.get("conclusion") != "success":
            failed.append(name)
    matrix = [j for j in jobs
              if re.fullmatch(r"release-build \([^)]*\)", j["name"])]
    seen = set()
    for job in matrix:
        inner = re.fullmatch(r"release-build \(([^)]*)\)", job["name"]).group(1)
        parts = [p.strip() for p in inner.split(",")]
        require(len(parts) == 2,
                f"ambiguous release-build job name {job['name']!r}")
        target, runner = parts
        require(target in TARGETS and TARGETS[target] == runner,
                f"unexpected release-build matrix leg {job['name']!r}")
        require(target not in seen,
                f"duplicate release-build leg for {target}")
        seen.add(target)
        if job.get("status") != "completed":
            pending.append(job["name"])
        elif job.get("conclusion") != "success":
            failed.append(job["name"])
    for target in set(TARGETS) - seen:
        pending.append(f"release-build ({target}, {TARGETS[target]})")
    if failed:
        return "failed", failed
    if pending:
        return "pending", pending
    return "ok", jobs


def validate_jobs(jobs):
    state, detail = jobs_status(jobs)
    require(state == "ok", f"required jobs not successful: {detail}")


def find_artifact(artifacts, name):
    """Exactly one live artifact with the exact name."""
    require(type(artifacts) is list, "artifact inventory must be a list")
    matches = [a for a in artifacts
               if type(a) is dict and a.get("name") == name]
    live = [a for a in matches if a.get("expired") is False]
    require(len(matches) == len(live), f"artifact {name!r} is expired")
    require(len(live) == 1,
            f"artifact {name!r}: expected 1, found {len(live)}")
    strict_int(live[0].get("id"), "artifact id")
    return live[0]


def recorded_sha256(text, label, filename=None):
    """First field of a sha256sum line; when a filename is given the
    hash line must name exactly that file."""
    fields = text.split()
    require(len(fields) == 2 and SHA256_RE.fullmatch(fields[0].lower()),
            f"{label} does not hold a sha256")
    if filename is not None:
        require(fields[1] == filename,
                f"{label} does not name {filename!r}")
    return fields[0].lower()


def zip_members(zip_path, expected_names):
    """An artifact ZIP with exactly the expected safe regular entries:
    no duplicates, no traversal, no directories-as-files, bounded size."""
    require(zipfile.is_zipfile(zip_path), f"{zip_path} is not a ZIP")
    require(Path(zip_path).stat().st_size <= BLOB_LIMIT, "ZIP too large")
    with zipfile.ZipFile(zip_path) as zf:
        infos = zf.infolist()
        names = [i.filename for i in infos]
        require(len(names) == len(set(names)), "duplicate ZIP entries")
        require(set(names) == set(expected_names),
                f"unexpected ZIP entries: {sorted(names)}")
        require(sum(i.file_size for i in infos) <= BLOB_LIMIT,
                "ZIP contents too large")
        out = {}
        for info in infos:
            require(not info.is_dir()
                    and stat.S_IFMT(info.external_attr >> 16) in (0, stat.S_IFREG)
                    and not info.flag_bits & 1, "unsafe ZIP member type")
            require(not info.filename.startswith("/")
                    and ".." not in info.filename.split("/"),
                    f"unsafe ZIP entry {info.filename!r}")
            require(info.file_size <= BLOB_LIMIT,
                    f"ZIP entry {info.filename!r} too large")
            out[info.filename] = zf.read(info)
    return out


def tar_members(blob, expected_names):
    """Exactly the expected unique regular members, bounded size."""
    require(len(blob) <= BLOB_LIMIT, "tarball too large")
    with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as tar:
        members = tar.getmembers()
        names = [m.name for m in members]
        require(len(names) == len(set(names)), "duplicate tar members")
        require(set(names) == set(expected_names),
                f"unexpected tar members: {sorted(names)}")
        require(sum(m.size for m in members) <= BLOB_LIMIT,
                "tar contents too large")
        extracted = {}
        for member in members:
            require(member.isreg(), f"{member.name} is not a regular file")
            require(not member.name.startswith("/")
                    and ".." not in member.name.split("/"),
                    f"unsafe tar member {member.name!r}")
            require(member.size <= BLOB_LIMIT, f"{member.name} too large")
            extracted[member.name] = tar.extractfile(member).read()
    return extracted


def check_manifest(manifest, tag, sha, target, run_id=None, attempt=None):
    """Strict manifest: every scalar the release job writes, plus run
    identity when the producing run is known (candidate mode)."""
    require(type(manifest) is dict, "manifest is not an object")
    valid_inputs(REPO, tag, sha, target)
    strict_int(manifest.get("run_id"), "manifest run id")
    strict_int(manifest.get("run_attempt"), "manifest attempt")
    require(manifest.get("version") == tag, "manifest tag mismatch")
    require(manifest.get("source_sha") == sha, "manifest source SHA mismatch")
    require(manifest.get("target") == target, "manifest target mismatch")
    require(manifest.get("runner") == TARGETS[target],
            "manifest runner mismatch")
    require(manifest.get("features") == ["ui"],
            "manifest must carry the ui feature")
    require(manifest.get("checks") == GATE_JOBS, "manifest checks mismatch")
    require(SHA256_RE.fullmatch(strict_str(manifest.get("sha256"),
                                           "manifest sha256")),
            "manifest sha256 malformed")
    if run_id is not None:
        strict_int(run_id, "run id")
        strict_int(attempt, "attempt")
        require(manifest.get("run_id") == run_id and type(
            manifest.get("run_id")) is int, "manifest run id mismatch")
        require(manifest.get("run_attempt") == attempt and type(
            manifest.get("run_attempt")) is int,
            "manifest attempt mismatch")
    for field in ("rustc", "cargo", "built_at"):
        strict_str(manifest.get(field), f"manifest {field}")


def verify_tarball(tar_bytes, sha_text, tag, target, sha,
                   run_id=None, attempt=None, zip_label=None):
    """Outer checksum (bound to the asset filename), safe member set,
    inner checksum, strict manifest — all before anything is executed."""
    asset = f"cadence-{tag}-{target}.tar.gz"
    if isinstance(tar_bytes, Path):
        tar_bytes = tar_bytes.read_bytes()
    if isinstance(sha_text, Path):
        sha_text = sha_text.read_text()
    outer_expected = recorded_sha256(sha_text, f"{asset}.sha256", asset)
    outer_actual = hashlib.sha256(tar_bytes).hexdigest()
    require(outer_actual == outer_expected,
            f"outer checksum mismatch for {asset}")
    extracted = tar_members(tar_bytes, TAR_MEMBERS)
    inner_expected = recorded_sha256(extracted["cadence.sha256"].decode(),
                                     "cadence.sha256", "cadence")
    digest = hashlib.sha256(extracted["cadence"]).hexdigest()
    require(digest == inner_expected, "inner cadence.sha256 mismatch")
    manifest = json.loads(extracted["manifest.json"])
    check_manifest(manifest, tag, sha, target, run_id, attempt)
    require(manifest["sha256"] == digest, "manifest sha256 mismatch")
    return {"sha256": outer_actual, "binary_sha256": digest,
            "manifest": manifest, "files": extracted}


# --- clean-machine environment --------------------------------------------


def restricted_path(root):
    """A bin dir holding symlinks to only the allowlisted tools, resolved
    in the controller's environment."""
    bin_dir = Path(root) / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    linked = []
    for tool in sorted(set(PATH_ALLOWLIST)):
        found = shutil.which(tool)
        if found:
            target = bin_dir / tool
            if not target.exists():
                target.symlink_to(found)
            linked.append(tool)
    require("sh" in linked and "tar" in linked,
            "restricted PATH lacks sh/tar")
    require(any(t in linked for t in ("sha256sum", "shasum", "openssl")),
            "restricted PATH lacks a sha256 tool")
    path = str(bin_dir)
    for tool in FORBIDDEN_ON_PATH:
        require(shutil.which(tool, path=path) is None,
                f"forbidden tool {tool} leaked into restricted PATH")
    return path, linked


def clean_env(root, restricted):
    """Explicit allowlist env: HOME/XDG/TMPDIR under the owned root,
    PATH restricted, nothing inherited — no LD_*, BASH_ENV, proxies,
    provider credentials or token-shaped variables can leak."""
    root = Path(root)
    env = {"PATH": restricted, "LANG": "C", "LC_ALL": "C", "TZ": "UTC"}
    dirs = {"HOME": "home", "XDG_STATE_HOME": "state",
            "XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "data",
            "XDG_CACHE_HOME": "cache", "TMPDIR": "tmp",
            "XDG_RUNTIME_DIR": "run"}
    for key, name in dirs.items():
        target = root / name
        target.mkdir(parents=True, exist_ok=True)
        target.chmod(0o700)
        env[key] = str(target)
    leaked = [k for k in env if k.startswith(("CADENCE_", "GITHUB_",
              "GH_", "TMUX", "PI_")) or "TOKEN" in k or "SECRET" in k]
    require(not leaked, f"environment still carries {leaked}")
    return env


def run(argv, env=None, cwd=None, timeout=TIMEOUT):
    """Record argv, exit, stdout, stderr. Never raises on nonzero exit —
    the caller decides; nothing is ignored."""
    require(argv and all(type(a) is str and a for a in argv),
            "argv must be non-empty strings")
    try:
        result = subprocess.run(argv, capture_output=True, text=True,
                                env=env, cwd=cwd, timeout=timeout, check=False)
    except subprocess.TimeoutExpired as error:
        def decoded(value):
            return value.decode(errors="replace") if isinstance(value, bytes) else value or ""
        return {"argv": argv, "exit": None, "outcome": "timeout",
                "stdout": decoded(error.stdout), "stderr": decoded(error.stderr)}
    return {"argv": argv, "exit": result.returncode,
            "stdout": result.stdout, "stderr": result.stderr}


# --- acquisition -----------------------------------------------------------


def gh(*args, binary=False):
    out = subprocess.check_output(["gh", *map(str, args)], timeout=60)
    return out if binary else out


def api(endpoint):
    return json.loads(gh("api", "--method", "GET", endpoint))


def paged(endpoint, key):
    """Complete paginated inventory: total_count is authoritative, every
    page must be a list, a short page before total is reached is an
    error, never a silent empty fallback."""
    items = []
    total = None
    page = 1
    identities = set()
    sep = "&" if "?" in endpoint else "?"
    while True:
        data = api(f"{endpoint}{sep}per_page=100&page={page}")
        require(type(data) is dict, f"{endpoint}: page not an object")
        chunk = data.get(key)
        require(type(chunk) is list, f"{endpoint}: {key} not a list")
        if total is None:
            total = data.get("total_count")
            require(type(total) is int and total >= 0,
                    f"{endpoint}: malformed total_count")
        require(type(data.get("total_count")) is int
                and data["total_count"] == total, "inventory changed during pagination")
        require(len(chunk) <= 100, "oversized inventory page")
        for item in chunk:
            require(type(item) is dict, "malformed inventory entry")
            identity = strict_int(item.get("id"), "inventory id")
            require(identity not in identities, "duplicate inventory id")
            identities.add(identity)
        items.extend(chunk)
        require(len(items) <= total,
                f"{endpoint}: inventory exceeded total_count")
        if len(items) == total:
            return items
        require(len(chunk) == 100,
                f"{endpoint}: short page with items outstanding")
        page += 1
        require(page <= PAGE_LIMIT, f"{endpoint}: pagination cap exceeded")


def acquire_candidate(tag, sha, target, run_id, attempt, out):
    valid_inputs(REPO, tag, sha, target)
    strict_int(run_id, "run id")
    strict_int(attempt, "attempt")
    out = Path(out)
    out.mkdir(parents=True, exist_ok=False)

    def fresh_identity():
        metadata = api(f"repos/{REPO}/actions/runs/{run_id}")
        validate_run(metadata, REPO, run_id, sha, tag, attempt)
        ref = api(f"repos/{REPO}/git/ref/tags/{tag}")
        require(type(ref) is dict and ref.get("ref") == f"refs/tags/{tag}",
                "tag ref name mismatch")
        obj = ref.get("object")
        require(type(obj) is dict, "tag object missing")
        tag_object = None
        if obj.get("type") == "tag":
            require(SHA_RE.fullmatch(strict_str(obj.get("sha"), "tag object SHA")),
                    "invalid annotated tag identity")
            tag_object = api(f"repos/{REPO}/git/tags/{obj['sha']}")
        peeled_sha = peel_tag_ref(ref, tag_object)
        require(peeled_sha == sha, "tag moved or source mismatched")
        return metadata, peeled_sha

    run_meta, peeled = fresh_identity()
    comparison = api(f"repos/{REPO}/compare/{sha}...main")
    require(comparison.get("status") in ("identical", "ahead"),
            f"source is not on main (compare: {comparison.get('status')})")

    # Bounded wait for pending jobs/artifacts only; a terminal failure
    # anywhere in the required set fails immediately.
    deadline = time.monotonic() + WAIT_LIMIT
    wait = {"polled": [], "limit_seconds": WAIT_LIMIT,
            "poll_seconds": WAIT_POLL}
    while True:
        run_meta, peeled = fresh_identity()
        jobs = paged(
            f"repos/{REPO}/actions/runs/{run_id}/attempts/{attempt}/jobs",
            "jobs")
        state, detail = jobs_status(jobs)
        wait["polled"].append({"kind": "jobs", "state": state,
                               "detail": detail})
        (out / "wait.json").write_text(json.dumps(wait, indent=2) + "\n")
        require(state != "failed",
                f"terminal job failure, refusing to wait: {detail}")
        if state == "ok":
            break
        require(run_meta["status"] != "completed",
                "completed run has missing required jobs")
        require(time.monotonic() + WAIT_POLL <= deadline,
                f"jobs still pending after {WAIT_LIMIT}s: {detail}")
        time.sleep(WAIT_POLL)
    name = f"release-{tag}-{target}"
    while True:
        run_meta, peeled = fresh_identity()
        jobs = paged(f"repos/{REPO}/actions/runs/{run_id}/attempts/{attempt}/jobs",
                     "jobs")
        validate_jobs(jobs)
        artifacts = paged(
            f"repos/{REPO}/actions/runs/{run_id}/artifacts", "artifacts")
        matches = [a for a in artifacts if type(a) is dict
                   and a.get("name") == name]
        require(len(matches) <= 1, "ambiguous exact-name artifact inventory")
        if len(matches) == 1 and matches[0].get("expired") is False:
            artifact = find_artifact(artifacts, name)
            break
        require(not any(a.get("expired") for a in matches),
                f"artifact {name!r} is expired")
        require(time.monotonic() + WAIT_POLL <= deadline,
                f"artifact {name!r} not uploaded within {WAIT_LIMIT}s")
        time.sleep(WAIT_POLL)
    artifact_id = strict_int(artifact["id"], "artifact id")
    zip_path = out / "artifact.zip"
    zip_path.write_bytes(
        gh("api", f"repos/{REPO}/actions/artifacts/{artifact_id}/zip",
           binary=True))
    asset = f"cadence-{tag}-{target}.tar.gz"
    files = zip_members(zip_path, [asset, f"{asset}.sha256"])
    verified = verify_tarball(files[asset], files[f"{asset}.sha256"].decode(),
                              tag, target, sha, run_id, attempt)
    for filename, blob in files.items():
        (out / filename).write_bytes(blob)

    # install.sh from the exact source revision via GET (?ref= query),
    # never -f ref (that POSTs) and never a moving ref.
    installer = out / "install.sh"
    installer.write_bytes(gh(
        "api", f"repos/{REPO}/contents/scripts/install.sh?ref={sha}",
        "-H", "Accept: application/vnd.github.raw+json"))
    installer.chmod(0o755)
    run_meta, peeled = fresh_identity()
    jobs = paged(f"repos/{REPO}/actions/runs/{run_id}/attempts/{attempt}/jobs", "jobs")
    validate_jobs(jobs)

    receipt = {
        "schema": 1, "mode": "candidate", "repo": REPO, "tag": tag,
        "source_sha": sha, "target": target,
        "run": {"id": run_id, "attempt": attempt,
                "head_branch": run_meta["head_branch"],
                "tag_ref_peeled": peeled,
                "status": run_meta["status"],
                "conclusion": run_meta.get("conclusion")},
        "jobs": jobs,
        "artifact": {"name": name, "id": artifact_id,
                     "zip_entries": sorted(files),
                     "outer_sha256": verified["sha256"],
                     "binary_sha256": verified["binary_sha256"]},
        "installer": {"source": f"repos/{REPO}/contents/scripts/install.sh?ref={sha}",
                      "sha256": hashlib.sha256(installer.read_bytes()).hexdigest()},
        "wait": wait,
        "attestation": "none-claimed: emitted by protected release-publish",
        "public_url_proven": False,
    }
    (out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    (out / "manifest.json").write_bytes(verified["files"]["manifest.json"])
    return receipt


def acquire_published(tag, sha, target, out):
    """Anonymous public fetch: plain urllib GETs of the real release
    URLs — no authorization header, token, gh or mirror flag anywhere."""
    valid_inputs(REPO, tag, sha, target)
    out = Path(out)
    out.mkdir(parents=True, exist_ok=False)
    base = f"https://github.com/{REPO}/releases/download/{tag}"
    asset = f"cadence-{tag}-{target}.tar.gz"
    fetched = {}
    blobs = {}
    for label, url in [("installer", f"{base}/install.sh"),
                       ("tarball", f"{base}/{asset}"),
                       ("sha256", f"{base}/{asset}.sha256")]:
        request = urllib.request.Request(url)  # urllib's own headers only
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(request, timeout=60) as response:
            data = response.read(BLOB_LIMIT + 1)
        require(len(data) <= BLOB_LIMIT, f"{label} download too large")
        fetched[label] = {"url": url, "bytes": len(data),
                          "sha256": hashlib.sha256(data).hexdigest()}
        blobs[label] = data
    (out / "install.sh").write_bytes(blobs["installer"])
    (out / "install.sh").chmod(0o755)
    (out / asset).write_bytes(blobs["tarball"])
    (out / f"{asset}.sha256").write_bytes(blobs["sha256"])
    verified = verify_tarball(blobs["tarball"], blobs["sha256"].decode(),
                              tag, target, sha)
    (out / "manifest.json").write_bytes(
        json.dumps(verified["manifest"], indent=2).encode())
    receipt = {
        "schema": 1, "mode": "published", "repo": REPO, "tag": tag,
        "source_sha": sha, "target": target, "fetched": fetched,
        "binary_sha256": verified["binary_sha256"], "anonymous": True,
        "note": "anonymous checksums prove byte integrity of the public "
                "download; publisher authenticity is the build-provenance "
                "attestation, checked control-side, not by these bytes.",
        "public_url_proven": True,
    }
    (out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    return receipt


# --- install-only smoke harness -------------------------------------------


def _file_state(path):
    st = path.lstat()
    return {"inode": st.st_ino, "mtime_ns": st.st_mtime_ns,
            "mode": stat.S_IMODE(st.st_mode),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest()
            if stat.S_ISREG(st.st_mode) else None}


def harness(installer, tag, sha, target, expect_sha256, mode,
            expected_manifest, base_url=None, root=None, evidence_path=None):
    """Install-only smoke: default prefix, a rerun, and a custom --prefix
    under its own fresh root. The first failed step stops everything;
    every step's argv/exit/stdout/stderr is recorded."""
    valid_inputs(REPO, tag, sha, target)
    require(mode in ("candidate", "published"), "unknown acquisition mode")
    strict_str(expect_sha256, "expected digest")
    require(SHA256_RE.fullmatch(expect_sha256 or ""),
            "expected binary sha256 must be 64 hex")
    if mode == "published":
        require(base_url is None,
                "published mode refuses any base-url, including file://")
    else:
        require(type(base_url) is str and base_url.startswith("file://"),
                "candidate mode allows a file:// mirror only")
    package_manifest = json.loads(Path(expected_manifest).read_text())
    check_manifest(package_manifest, tag, sha, target)
    require(package_manifest["sha256"] == expect_sha256,
            "expected package manifest binary digest mismatch")
    installer = Path(installer).resolve()
    require(installer.is_file(), "installer missing")
    parent = Path(root or "/tmp").resolve()
    require(parent.is_relative_to(Path("/tmp").resolve()) and parent.is_dir(),
            "harness root must be inside an existing /tmp directory")
    root = Path(tempfile.mkdtemp(prefix="c317-", dir=parent))
    root.chmod(0o700)
    restricted, linked = restricted_path(root)
    evidence = {"schema": 1, "mode": mode, "tag": tag, "source_sha": sha,
                "target": target, "root": str(root),
                "path_tools": linked, "forbidden_absent": FORBIDDEN_ON_PATH,
                "expect_binary_sha256": expect_sha256,
                "expected_package_manifest": package_manifest,
                "steps": [], "installs": {}, "pending": {
                    "setup": "setup --json not exercised in this increment",
                    "headless-wizard": "not exercised in this install-only increment",
                    "backup-restore": "not exercised in this bounded "
                                      "install-only increment"},
                "full_cad317_acceptance": False, "ok": False}
    if target.endswith("macos"):
        evidence["pending"]["macos-daemon"] = (
            "daemon intentionally unsupported on macOS until CAD-315")
    ok = True

    def persist():
        encoded = json.dumps(evidence, indent=2) + "\n"
        (root / "harness.json").write_text(encoded)
        if evidence_path is not None:
            destination = Path(evidence_path)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(encoded)

    persist()

    def abort(step, record, detail):
        evidence["failure"] = {"step": step, "detail": detail}
        evidence["ok"] = False
        persist()
        return evidence

    def install_root(label, prefix_args):
        """One fresh owned root: env, install, verify. Returns evidence."""
        sub = Path(tempfile.mkdtemp(prefix=f"{label}-", dir=root))
        sub.chmod(0o700)
        env = clean_env(sub, restricted)
        home = Path(env["HOME"])
        argv = ["sh", str(installer), "--version", tag, *prefix_args]
        if base_url:
            argv += ["--base-url", base_url]
        record = run(argv, env=env, cwd=str(home))
        evidence["steps"].append({"name": f"install-{label}", **record})
        persist()
        if record["exit"] != 0:
            return None, record, f"install exit {record['exit']}"
        link = home / ".local/bin/cadence"
        prefix = (Path(prefix_args[1]).resolve() if prefix_args
                  else Path(env["XDG_DATA_HOME"]) / "cadence")
        release = prefix / "releases" / tag
        if not link.is_symlink() or not release.is_dir():
            return None, record, "missing link or release dir"
        if (os.readlink(link) != str(release / "cadence")
                or Path(os.path.realpath(link)) != (release / "cadence").resolve()):
            return None, record, "installed link does not target the exact release"
        names = sorted(p.name for p in release.iterdir())
        if names != sorted(TAR_MEMBERS):
            return None, record, f"release dir holds {names}"
        if any(not (release / n).is_file() or (release / n).is_symlink() for n in names):
            return None, record, "installed members must be regular files"
        if stat.S_IMODE((release / "cadence").stat().st_mode) != 0o755:
            return None, record, "installed executable mode must be 0755"
        binary_sha = hashlib.sha256((release / "cadence").read_bytes()) \
            .hexdigest()
        if binary_sha != expect_sha256:
            return None, record, "installed binary digest mismatch"
        manifest = json.loads((release / "manifest.json").read_text())
        try:
            require(recorded_sha256((release / "cadence.sha256").read_text(),
                                    "installed inner checksum", "cadence") == expect_sha256,
                    "installed inner checksum mismatch")
            check_manifest(manifest, tag, sha, target)
            require(manifest == package_manifest,
                    "installed manifest differs from verified package manifest")
            require(manifest["sha256"] == expect_sha256,
                    "installed manifest sha256 mismatch")
        except ValueError as error:
            return None, record, f"installed manifest: {error}"
        version = run([str(link), "--version"], env=env, cwd=str(home))
        evidence["steps"].append({"name": f"{label}-version", **version})
        persist()
        expected = f"cadence {tag[1:]}+{sha}"
        if version["exit"] != 0 or version["stdout"].strip() != expected:
            return None, record, (
                f"--version: exit {version['exit']}, "
                f"{version['stdout'].strip()!r} != {expected!r}")
        state = {"link": os.readlink(link),
                 "link_target": os.path.realpath(link),
                 "link_file": _file_state(link),
                 "files": {n: _file_state(release / n) for n in names},
                 "argv": argv}
        return {"root": str(sub), "env_home": str(home), "state": state,
                "env": env, "argv": argv, "record": record}, record, None

    first, record, error = install_root("default", [])
    if error:
        return abort("install-default", record or {}, error)
    evidence["installs"]["default"] = {k: v for k, v in first.items()
                                       if k != "env"}

    # Rerun the identical install in the same root: inode/mtime/content
    # and the link must be stable (install.sh keeps a same-bytes release).
    record = run(first["argv"], env=first["env"],
                 cwd=str(Path(first["env"]["HOME"])))
    evidence["steps"].append({"name": "rerun-same-version", **record})
    persist()
    if record["exit"] != 0:
        return abort("rerun-same-version", record,
                     f"rerun exit {record['exit']}")
    home = Path(first["env"]["HOME"])
    link = home / ".local/bin/cadence"
    prefix = Path(first["env"]["XDG_DATA_HOME"]) / "cadence"
    release = prefix / "releases" / tag
    after = {"link": os.readlink(link) if link.is_symlink() else None,
             "link_target": os.path.realpath(link),
             "link_file": _file_state(link),
             "files": {n: _file_state(release / n)
                       for n in sorted(TAR_MEMBERS)}}
    if after["link"] != first["state"]["link"] or \
            after["link_target"] != first["state"]["link_target"] or \
            after["link_file"] != first["state"]["link_file"] or \
            after["files"] != first["state"]["files"]:
        return abort("rerun-stability", {"argv": first["argv"], "exit": 0,
                                         "stdout": "", "stderr": ""},
                     f"rerun changed installed state: {after}")

    custom_prefix = root / "custom-prefix"
    custom, record, error = install_root(
        "custom", ["--prefix", str(custom_prefix)])
    if error:
        return abort("install-custom-prefix", record or {}, error)
    evidence["installs"]["custom-prefix"] = {
        k: v for k, v in custom.items() if k != "env"}
    record = run(custom["argv"], env=custom["env"],
                 cwd=str(Path(custom["env"]["HOME"])))
    evidence["steps"].append({"name": "rerun-custom-prefix", **record})
    persist()
    if record["exit"] != 0:
        return abort("rerun-custom-prefix", record, "custom rerun failed")
    link = Path(custom["env"]["HOME"]) / ".local/bin/cadence"
    release = custom_prefix / "releases" / tag
    after = {"link": os.readlink(link) if link.is_symlink() else None,
             "link_target": os.path.realpath(link), "link_file": _file_state(link),
             "files": {n: _file_state(release / n) for n in sorted(TAR_MEMBERS)}}
    before = custom["state"]
    if any(after[k] != before[k] for k in after):
        return abort("custom-rerun-stability", record,
                     "custom rerun changed installed files/link")

    evidence["ok"] = bool(ok)
    persist()
    return evidence


# --- cli -------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("acquire-candidate", "acquire-published"):
        sub = commands.add_parser(name)
        sub.add_argument("--tag", required=True)
        sub.add_argument("--sha", required=True)
        sub.add_argument("--target", required=True, choices=sorted(TARGETS))
        sub.add_argument("--out", required=True)
    commands.choices["acquire-candidate"].add_argument(
        "--run-id", required=True, type=int)
    commands.choices["acquire-candidate"].add_argument(
        "--attempt", required=True, type=int)
    harness_parser = commands.add_parser("harness")
    harness_parser.add_argument("--installer", required=True)
    harness_parser.add_argument("--tag", required=True)
    harness_parser.add_argument("--sha", required=True)
    harness_parser.add_argument("--target", required=True,
                                choices=sorted(TARGETS))
    harness_parser.add_argument("--expect-sha256", required=True)
    harness_parser.add_argument("--expected-manifest", required=True,
                                help="Verified acquisition manifest JSON")
    harness_parser.add_argument("--mode", default="candidate",
                                choices=["candidate", "published"])
    harness_parser.add_argument("--base-url", default=None)
    harness_parser.add_argument("--root", default=None)
    harness_parser.add_argument("--out", default=None,
                                help="Persist partial/final smoke evidence here")
    args = parser.parse_args()
    if args.command == "acquire-candidate":
        receipt = acquire_candidate(args.tag, args.sha, args.target,
                                    args.run_id, args.attempt, args.out)
        print(json.dumps(receipt))
        return 0
    if args.command == "acquire-published":
        receipt = acquire_published(args.tag, args.sha, args.target, args.out)
        print(json.dumps(receipt))
        return 0
    evidence = harness(args.installer, args.tag, args.sha, args.target,
                       args.expect_sha256, args.mode,
                       expected_manifest=args.expected_manifest,
                       base_url=args.base_url, root=args.root,
                       evidence_path=args.out)
    print(json.dumps({"ok": evidence["ok"], "root": evidence["root"],
                      "full_cad317_acceptance":
                      evidence["full_cad317_acceptance"]}))
    return 0 if evidence["ok"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, subprocess.SubprocessError,
            tarfile.TarError, zipfile.BadZipFile,
            json.JSONDecodeError) as error:
        print(f"clean-install: {error}", file=sys.stderr)
        sys.exit(1)
