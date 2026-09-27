"""Observation-only native review binding check (CAD-680).

Reads the open-PR inventory from a trusted GitHub adapter and a fresh
review-evidence export from a trusted native adapter, twice each, and
reports whether a single exact native review still binds the requested
pull request head. It writes nothing: no GitHub check, no status, no
saved JSON, no daemon call. The result is an observation, not an
attestation or an authorization.

Trusted adapter contract (both are abstract duck-typed callables):

- ``github.snapshot()`` returns the authoritative, COMPLETE open-PR
  inventory for the configured repository (all pages): a dict with
  exactly ``repository_id`` (int), ``repository`` ("owner/name"),
  ``complete`` (True) and ``pull_requests`` (list of entries with
  exactly ``number``, ``pr``, ``sha``, ``base_ref``, ``state``).
- ``native.export(batch)`` takes ``{"requests": [{"issue", "pr", "sha"}]}``
  and returns a ``cadence.review-evidence/1`` transport_only export.
- Both adapters must bound their own calls; this module checks clocks
  only when a call returns and cannot preempt a blocked adapter.

Adapter identity and live provenance are NOT authenticated by this
policy: a malicious trusted adapter can forge plausible values. The
two-snapshot recheck narrows but does not eliminate the race between
the last read and any later use.
"""

import copy
import re
import time
from dataclasses import dataclass

SCHEMA = "cadence.review-observation/1"
EVIDENCE_SCHEMA = "cadence.review-evidence/1"
MAX_EXPORT_AGE_SECONDS = 30
MAX_DURATION_SECONDS = 5.0

_REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_CONTROL_RE = re.compile(r"[\x00-\x1f\x7f]")

_SCOPE_KEYS = {"kind", "issue", "number"}
_SNAPSHOT_KEYS = {"repository_id", "repository", "complete", "pull_requests"}
_PR_KEYS = {"number", "pr", "sha", "base_ref", "state"}
_EXPORT_KEYS = {"schema", "transport_only", "checked_at", "reviews"}
_REVIEW_KEYS = {"issue", "project", "repository", "number", "pr", "sha",
                "worker", "reviewer", "report", "reviewed_at"}


class _Refuse(Exception):
    def __init__(self, reason):
        super().__init__(reason)
        self.reason = reason


def _is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def _is_num(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def _clean_str(v):
    return (isinstance(v, str) and v and v == v.strip()
            and not _CONTROL_RE.search(v))


def _pr_url(repository, number):
    return "https://github.com/%s/pull/%d" % (repository, number)


@dataclass(frozen=True)
class Config:
    """Pinned service configuration; no operational/timing options."""
    repository_id: int
    repository: str
    project: str
    base_ref: str

    def __post_init__(self):
        if not _is_int(self.repository_id) or self.repository_id <= 0:
            raise ValueError("repository_id must be a positive int")
        if not _clean_str(self.repository) or not _REPO_RE.match(self.repository):
            raise ValueError("repository must be canonical 'owner/name'")
        if not _clean_str(self.project):
            raise ValueError("project must be a non-empty clean string")
        if not _clean_str(self.base_ref):
            raise ValueError("base_ref must be a non-empty clean string")


def _refusal(reason):
    return {"schema": SCHEMA, "observation_only": True,
            "matched": False, "reason": reason}


def _validate_config(config):
    if not isinstance(config, Config):
        raise _Refuse("config-malformed")
    try:
        Config(config.repository_id, config.repository,
               config.project, config.base_ref)
    except (ValueError, TypeError):
        raise _Refuse("config-malformed")


def _validate_scope(scope):
    if not isinstance(scope, dict) or set(scope) != _SCOPE_KEYS:
        raise _Refuse("scope-malformed")
    if scope["kind"] != "pull_request":
        raise _Refuse("scope-kind-unsupported")
    if not _clean_str(scope["issue"]):
        raise _Refuse("scope-malformed")
    if not _is_int(scope["number"]) or scope["number"] <= 0:
        raise _Refuse("scope-malformed")


def _check_pr_entry(entry, config):
    if not isinstance(entry, dict) or set(entry) != _PR_KEYS:
        raise _Refuse("snapshot-malformed")
    n = entry["number"]
    if not _is_int(n) or n <= 0:
        raise _Refuse("snapshot-malformed")
    if entry["pr"] != _pr_url(config.repository, n):
        raise _Refuse("snapshot-malformed")
    if not isinstance(entry["sha"], str) or not _SHA_RE.fullmatch(entry["sha"]):
        raise _Refuse("snapshot-malformed")
    if entry["state"] != "OPEN":
        raise _Refuse("snapshot-malformed")
    if not _clean_str(entry["base_ref"]):
        raise _Refuse("snapshot-malformed")


def _check_snapshot(snap, config):
    if not isinstance(snap, dict) or set(snap) != _SNAPSHOT_KEYS:
        raise _Refuse("snapshot-malformed")
    if (not _is_int(snap["repository_id"])
            or snap["repository_id"] != config.repository_id):
        raise _Refuse("snapshot-repository-mismatch")
    if snap["repository"] != config.repository:
        raise _Refuse("snapshot-repository-mismatch")
    if snap["complete"] is not True:
        raise _Refuse("snapshot-incomplete")
    prs = snap["pull_requests"]
    if not isinstance(prs, list):
        raise _Refuse("snapshot-malformed")
    numbers, urls = set(), set()
    for entry in prs:
        _check_pr_entry(entry, config)
        if entry["number"] in numbers or entry["pr"] in urls:
            raise _Refuse("snapshot-duplicate")
        numbers.add(entry["number"])
        urls.add(entry["pr"])
    return prs


def _find_target(prs, scope, config):
    target = None
    for entry in prs:
        if entry["number"] == scope["number"]:
            target = entry
    if target is None or target["base_ref"] != config.base_ref:
        raise _Refuse("target-not-found")
    # The same head on any other open PR makes the binding ambiguous.
    for entry in prs:
        if entry["number"] != scope["number"] and entry["sha"] == target["sha"]:
            raise _Refuse("head-ambiguous")
    return target


def _check_review(rev, scope, config, target, checked_at):
    if not isinstance(rev, dict) or set(rev) != _REVIEW_KEYS:
        raise _Refuse("review-malformed")
    if rev["issue"] != scope["issue"]:
        raise _Refuse("review-binding-mismatch")
    if not _is_int(rev["number"]):
        raise _Refuse("review-binding-mismatch")
    for key, want in (("project", config.project),
                      ("repository", config.repository),
                      ("number", scope["number"]),
                      ("pr", target["pr"]),
                      ("sha", target["sha"])):
        if rev[key] != want:
            raise _Refuse("review-binding-mismatch")
    for key in ("worker", "reviewer", "report"):
        if not _clean_str(rev[key]):
            raise _Refuse("review-identity-malformed")
    if rev["worker"] == rev["reviewer"]:
        raise _Refuse("review-not-independent")
    rat = rev["reviewed_at"]
    if not _is_int(rat) or rat <= 0 or rat > checked_at:
        raise _Refuse("review-timestamp-invalid")


def _check_export(exp, scope, config, target, wall):
    if not isinstance(exp, dict) or set(exp) != _EXPORT_KEYS:
        raise _Refuse("export-malformed")
    if exp["schema"] != EVIDENCE_SCHEMA:
        raise _Refuse("export-malformed")
    if exp["transport_only"] is not True:
        raise _Refuse("export-malformed")
    checked_at = exp["checked_at"]
    if not _is_int(checked_at) or checked_at <= 0:
        raise _Refuse("export-timestamp-invalid")
    if checked_at > wall or wall - checked_at > MAX_EXPORT_AGE_SECONDS:
        raise _Refuse("export-stale")
    reviews = exp["reviews"]
    if not isinstance(reviews, list) or len(reviews) != 1:
        raise _Refuse("export-cardinality")
    _check_review(reviews[0], scope, config, target, checked_at)
    return checked_at


def observe(scope, github, native, config, *,
            wall_clock=time.time, monotonic=time.monotonic):
    """Observe whether exact native review evidence binds the PR head.

    Read order is github, native, github, native. Returns a
    ``cadence.review-observation/1`` dict; ``matched`` is True only when
    every check passes on both reads. Refusal reasons are static
    strings; adapter exception content is never propagated.
    """
    try:
        return _observe(scope, github, native, config, wall_clock, monotonic)
    except _Refuse as r:
        return _refusal(r.reason)
    except Exception:
        return _refusal("observation-error")


def _observe(scope, github, native, config, wall_clock, monotonic):
    # Freeze the caller's scope before any validation or adapter call:
    # a mutable borrowed dict must not change the requested binding.
    if isinstance(scope, dict):
        scope = copy.deepcopy(scope)
    _validate_config(config)
    _validate_scope(scope)

    start = monotonic()
    if not _is_num(start) or not (start == start) or start in (float("inf"), float("-inf")):
        raise _Refuse("clock-invalid")
    prev_mono = [start]
    prev_wall = [None]

    def tick():
        m = monotonic()
        if not _is_num(m) or m != m or m in (float("inf"), float("-inf")):
            raise _Refuse("clock-invalid")
        if m < prev_mono[0] or m - start > MAX_DURATION_SECONDS:
            raise _Refuse("deadline-exceeded")
        prev_mono[0] = m

    def wall():
        w = wall_clock()
        if not _is_num(w) or w != w or w in (float("inf"), float("-inf")):
            raise _Refuse("clock-invalid")
        if prev_wall[0] is not None and w < prev_wall[0]:
            raise _Refuse("clock-invalid")
        prev_wall[0] = w
        return w

    def gh_read():
        try:
            snap = github.snapshot()
        except Exception:
            raise _Refuse("github-read-failed")
        snap = copy.deepcopy(snap)
        tick()
        return snap

    def native_read(batch):
        try:
            exp = native.export(copy.deepcopy(batch))
        except Exception:
            raise _Refuse("native-read-failed")
        exp = copy.deepcopy(exp)
        tick()
        return exp, wall()

    snap1 = _check_snapshot(gh_read(), config)
    target1 = _find_target(snap1, scope, config)
    batch = {"requests": [{"issue": scope["issue"],
                           "pr": target1["pr"], "sha": target1["sha"]}]}
    exp1, wall1 = native_read(batch)
    checked1 = _check_export(exp1, scope, config, target1, wall1)

    snap2 = _check_snapshot(gh_read(), config)
    target2 = _find_target(snap2, scope, config)
    if target2 != target1:
        raise _Refuse("head-moved")
    exp2, wall2 = native_read(batch)
    checked2 = _check_export(exp2, scope, config, target2, wall2)

    tick()
    # Re-validate both export timestamps against the final wall time:
    # the older batch may have aged during the second read.
    final_wall = wall()
    for checked in (checked1, checked2):
        if checked > final_wall or final_wall - checked > MAX_EXPORT_AGE_SECONDS:
            raise _Refuse("export-stale")
    if exp1["reviews"][0] != exp2["reviews"][0]:
        raise _Refuse("review-changed")

    return {"schema": SCHEMA, "observation_only": True, "matched": True,
            "repository_id": config.repository_id,
            "repository": config.repository, "project": config.project,
            "base_ref": config.base_ref, "issue": scope["issue"],
            "number": scope["number"], "pr": target2["pr"],
            "sha": target2["sha"], "review": exp2["reviews"][0],
            "checked_at": [checked1, checked2]}
