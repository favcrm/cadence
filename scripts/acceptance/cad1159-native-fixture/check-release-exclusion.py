#!/usr/bin/env python3
"""
CAD-1159 / SEC-002 independent C1 driver: compile-exclusion checks for the
`native-fixture-admission` feature on the real cadence-root-constructor.

SPEC ONLY companion: this file executes the C1 compile cases authored in
./ACCEPTANCE.md (independent spec; the implementer must not edit or weaken it).
It performs compile-only cargo builds and a fixed marker scan of the plain
release binary. It is NOT an admission mechanism, NOT an approval, and NOT a
runtime qualification: C2 (runtime refusal) and C3 (positive control) remain
BLOCKED per the spec and are intentionally not implemented here.

Hard rules (fixed, not configurable):
  * Run only on an admitted isolated build/CI runner, never the shared host.
    The caller must supply CAD1159_RUNNER_ADMISSION_ID; the value is recorded
    as caller-ASSERTED provenance only. This driver cannot verify admission.
  * Fixed cargo argv: `cargo build --bin cadence-root-constructor --locked`
    plus the per-case flags below. No dynamic args, no subcommand injection.
  * CARGO_BUILD_JOBS=2 is set; RUSTC_WRAPPER is inherited, never overridden.
  * The public fixture descriptor .cadence/native-fixture-public-root-v1.txt
    must be ABSENT in the repo. If present this driver STOPS; it never reads
    or deletes a foreign file.
  * Whole-run safety deadline: 20 minutes (local driver budget). The <=15 min
    cloud singleton/lifetime limit applies only to an externally allocated
    cloud run; this driver allocates no resources.
  * No source edits, no commits, no credential env printing, no unit tests,
    no restoration of legacy checks.
"""

from __future__ import annotations

import hashlib
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

# --- Fixed constants (not caller-tunable) -----------------------------------

BIN_NAME = "cadence-root-constructor"
BASE_ARGS = ["cargo", "build", "--bin", BIN_NAME, "--locked"]
MARKER = "native-fixture-admission-requires-debug-test-seam"
DESCRIPTOR_REL = ".cadence/native-fixture-public-root-v1.txt"
CONSTRUCTOR_MAIN_REL = "src/bin/cadence-root-constructor/main.rs"
ADMISSION_ENV = "CAD1159_RUNNER_ADMISSION_ID"
DEADLINE_S = 20 * 60  # 20-minute whole-run safety deadline.
DIRTY_CAP = 200

# (case_name, extra fixed argv, expectation)
#   refuse = must exit nonzero AND emit MARKER (unknown-feature / dependency /
#            build-script-panic failures are NOT a pass)
#   build  = must exit zero, produce the binary, and the binary must not
#            contain any fixed adapter/fixture marker bytes
CASES = [
    ("release-feature",
     ["--release", "--features", "native-fixture-admission"], "refuse"),
    ("release-testseam-feature",
     ["--release", "--features", "test-seam,native-fixture-admission"], "refuse"),
    ("debug-feature-no-testseam",
     ["--features", "native-fixture-admission"], "refuse"),
    ("plain-release",
     ["--release"], "build"),
]

# Fixed byte markers scanned in the plain-release binary. Absence of these is
# evidence the adapter did not leak in; it is NOT proof that owner-elected
# fixture root material is absent (those bytes are unknown until election).
BINARY_SCAN_MARKERS = [
    b"native-fixture-admission",
    b"native_fixture_admission",
    b"fixture_admission",
    b"native_fixture_trust",
    b"native-fixture-public-root-v1",
]

# Signatures that mean a refusal is a cargo/infra failure, not our marker.
NON_PASS_SIGNATURES = [
    "does not have these features",
    "does not contain these features",
    "unknown feature",
    "failed to select a version",
    "failed to run custom build command",
    "error: could not compile",
]

# Minimal fixed env allowlist for the build child. RUSTC_WRAPPER and the
# host's local sccache vars pass through so the wrapper still works; nothing
# in the child environment is ever printed or recorded.
ENV_ALLOWLIST = {
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "TMPDIR",
    "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "RUSTC_WRAPPER",
    "SSL_CERT_FILE", "SSL_CERT_DIR",
    "CARGO_NET_GIT_FETCH_WITH_CLI", "CARGO_HTTP_MULTIPLEXING",
    "SCCACHE_CONF", "SCCACHE_DIR", "SCCACHE_CACHE_SIZE",
    "SCCACHE_ERROR_LOG", "SCCACHE_IDLE_TIMEOUT", "SCCACHE_LOG",
}


def fail_stop(report: dict, outdir: Path | None, reason: str) -> int:
    """Record a preflight refusal and stop. Never reads/deletes foreign files."""
    report["overall"] = "stopped"
    report["stop_reason"] = reason
    sys.stderr.write("STOP: %s\n" % reason)
    if outdir is not None:
        _write_report(report, outdir)
    return 2


def _write_report(report: dict, outdir: Path) -> None:
    report_path = outdir / "report.json"
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


def _git(repo: Path, args: list[str], timeout: int = 30) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo)] + args,
        capture_output=True, text=True, timeout=timeout,
    )


def _build_env(outdir: Path) -> dict:
    env = {k: os.environ[k] for k in ENV_ALLOWLIST if k in os.environ}
    env["CARGO_BUILD_JOBS"] = "2"
    env["CARGO_TARGET_DIR"] = str(outdir / "target")
    return env


def _run_case(repo: Path, outdir: Path, env: dict, name: str,
              extra: list[str], budget_s: float) -> dict:
    logdir = outdir / "logs"
    argv = BASE_ARGS + extra  # fixed argv; no caller-controlled tokens
    rec = {
        "name": name,
        "argv": argv,
        "exit": None,
        "timeout": False,
        "marker_present": False,
        "classification": None,
        "pass": False,
        "logs": {
            "stdout": "logs/%s.stdout.log" % name,
            "stderr": "logs/%s.stderr.log" % name,
        },
    }
    with open(logdir / ("%s.stdout.log" % name), "wb") as so, \
         open(logdir / ("%s.stderr.log" % name), "wb") as se:
        try:
            proc = subprocess.Popen(
                argv, cwd=str(repo), env=env,
                stdout=so, stderr=se, start_new_session=True,
            )
            try:
                proc.wait(timeout=max(1.0, budget_s))
                rec["exit"] = proc.returncode
            except subprocess.TimeoutExpired:
                rec["timeout"] = True
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                proc.wait()
                rec["exit"] = "timeout"
        except FileNotFoundError:
            rec["exit"] = "spawn-failed"
            rec["classification"] = "cargo-not-found"
            return rec

    stderr_text = (logdir / ("%s.stderr.log" % name)).read_text(errors="replace")
    combined = stderr_text + "\n" + \
        (logdir / ("%s.stdout.log" % name)).read_text(errors="replace")
    rec["marker_present"] = MARKER in combined
    for sig in NON_PASS_SIGNATURES:
        if sig in combined:
            rec["classification"] = sig
            break
    return rec


def _scan_binary(binary: Path) -> dict:
    data = binary.read_bytes()
    found = []
    for m in BINARY_SCAN_MARKERS:
        if m in data:
            found.append(m.decode())
    return {
        "path": str(binary),
        "sha256": hashlib.sha256(data).hexdigest(),
        "size": len(data),
        "adapter_markers_found": found,
        "fixture_root_absence": "not-verifiable",
        "fixture_root_absence_note": (
            "Owner-elected public fixture bytes are unknown until election; "
            "this scan can only prove the fixed adapter markers are absent, "
            "not that all fixture root material is absent."
        ),
    }


def main(argv: list[str]) -> int:
    t0 = time.monotonic()
    deadline = t0 + DEADLINE_S

    usage = ("usage: check-release-exclusion.py <abs-repo-dir> "
             "<absent-abs-output-dir>\n"
             "requires env %s=<caller-asserted admission id>" % ADMISSION_ENV)
    if len(argv) != 3:
        sys.stderr.write(usage + "\n")
        return 2

    repo_arg = Path(argv[1])
    outdir_arg = Path(argv[2])
    repo = repo_arg.resolve()
    outdir = outdir_arg.resolve()

    report = {
        "driver": "check-release-exclusion.py (CAD-1159 C1, compile-only)",
        "spec": "fixture-admission-acceptance-author/ACCEPTANCE.md",
        "spec_sha256_recorded": None,
        "admission_id_caller_asserted": os.environ.get(ADMISSION_ENV),
        "admission_verified": False,
        "admission_note": (
            "The admission id is caller-asserted provenance. This standalone "
            "driver is not an admission mechanism and cannot verify runner "
            "admission, approval, election or CI authorization; genuine "
            "admission requires approved CI execution external to it."
        ),
        "repo": str(repo),
        "outdir": str(outdir),
        "head": None,
        "worktree_toplevel": None,
        "dirty": None,
        "dirty_entries": [],
        "descriptor_absent_confirmed": False,
        "deadline_s": DEADLINE_S,
        "elapsed_s": None,
        "env_facts": {
            "cargo_build_jobs": "2",
            "rustc_wrapper_present": "RUSTC_WRAPPER" in os.environ,
            "rustc_wrapper_value": "redacted (presence only; env values are never recorded)",
        },
        "limits": [
            "compile-only C1 driver; C2 runtime refusal and C3 positive "
            "control remain BLOCKED and are not attempted",
            "plain-release binary scan proves only absence of the fixed "
            "adapter markers, not absence of owner-elected fixture roots",
            "unknown-feature, dependency or build-script failure is never "
            "counted as the marker refusal",
            "no dynamic cargo args; fixed argv per case",
            "no resources allocated; 20-minute local safety deadline",
        ],
        "cases": [],
        "overall": "running",
    }

    # --- Preflight gates (any failure -> STOP, nonzero) --------------------

    admission = os.environ.get(ADMISSION_ENV)
    if not admission or not admission.strip():
        return fail_stop(report, None,
                         "%s unset: driver must run on an admitted isolated "
                         "runner (admission id is caller-asserted, not "
                         "verified); refusing on a shared host" % ADMISSION_ENV)

    # Absolute-path contract applies to the SUPPLIED arguments, checked
    # before resolve so .resolve() cannot silently canonicalize a relative
    # path into acceptance.
    if not repo_arg.is_absolute():
        return fail_stop(report, None,
                         "repo dir must be supplied as an absolute path: %r"
                         % argv[1])
    if not outdir_arg.is_absolute():
        return fail_stop(report, None,
                         "output dir must be supplied as an absolute path: %r"
                         % argv[2])
    # Reject non-canonical/symlink input: the supplied lexical path must
    # equal its canonical resolution.
    if repo_arg != repo:
        return fail_stop(report, None,
                         "repo path is non-canonical or traverses a symlink: "
                         "%r resolves to %r" % (argv[1], str(repo)))
    if outdir_arg != outdir:
        return fail_stop(report, None,
                         "output path is non-canonical or traverses a symlink: "
                         "%r resolves to %r" % (argv[2], str(outdir)))
    if not repo.is_dir():
        return fail_stop(report, None, "repo dir missing: %s" % repo)

    # Output dir must be absent; its parent must already exist, be canonical
    # and owner-only (private). The driver creates only outdir and logs at
    # 0700; it never creates or repairs foreign ancestor directories.
    if os.path.lexists(outdir):
        return fail_stop(report, None,
                         "output dir must be absent (private new dir): %s" % outdir)
    outdir_parent = outdir_arg.parent
    outdir_parent_real = outdir_parent.resolve()
    if not outdir_parent.is_dir() or outdir_parent != outdir_parent_real:
        return fail_stop(report, None,
                         "output parent must exist and be canonical (no "
                         "foreign ancestors created): %s" % outdir_parent)
    parent_st = os.stat(outdir_parent_real)
    if parent_st.st_uid != os.geteuid() or (parent_st.st_mode & 0o077):
        return fail_stop(report, None,
                         "output parent must be owned by this user with no "
                         "group/other permission bits: %s" % outdir_parent)
    os.mkdir(outdir, 0o700)
    os.mkdir(outdir / "logs", 0o700)

    # Bind the report to the spec version actually on disk, if present.
    spec_path = Path(__file__).resolve().parent / "ACCEPTANCE.md"
    if spec_path.is_file():
        report["spec_sha256_recorded"] = hashlib.sha256(
            spec_path.read_bytes()).hexdigest()

    # The public descriptor must be ABSENT. Never read or delete it.
    # lexists also catches a dangling symlink, which exists() would miss.
    if os.path.lexists(repo / DESCRIPTOR_REL):
        return fail_stop(report, outdir,
                         "foreign public fixture descriptor present at %s; "
                         "stopping without reading or deleting it"
                         % DESCRIPTOR_REL)
    report["descriptor_absent_confirmed"] = True

    if not (repo / CONSTRUCTOR_MAIN_REL).is_file():
        return fail_stop(report, outdir,
                         "%s absent; expected the real constructor source "
                         "from the #809 lane worktree, not main"
                         % CONSTRUCTOR_MAIN_REL)

    # Source provenance (recorded, never asserted as admission/approval).
    try:
        head = _git(repo, ["rev-parse", "HEAD"])
        if head.returncode == 0:
            report["head"] = head.stdout.strip()
        top = _git(repo, ["rev-parse", "--show-toplevel"])
        if top.returncode == 0:
            report["worktree_toplevel"] = top.stdout.strip()
        status = _git(repo, ["status", "--porcelain"])
        if status.returncode == 0:
            lines = [l for l in status.stdout.splitlines() if l.strip()]
            report["dirty"] = bool(lines)
            report["dirty_entries"] = lines[:DIRTY_CAP]
            if len(lines) > DIRTY_CAP:
                report["dirty_entries_truncated"] = len(lines) - DIRTY_CAP
    except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
        report["provenance_error"] = "git provenance unavailable: %s" % exc

    env = _build_env(outdir)

    # --- Fixed build matrix -------------------------------------------------

    overall_ok = True
    binary_path = outdir / "target" / "release" / BIN_NAME
    for name, extra, expect in CASES:
        budget = deadline - time.monotonic()
        if budget <= 0:
            report["cases"].append({
                "name": name, "argv": BASE_ARGS + extra, "exit": None,
                "timeout": True, "marker_present": False,
                "classification": "deadline-exhausted-before-start",
                "pass": False, "logs": None,
            })
            overall_ok = False
            break
        rec = _run_case(repo, outdir, env, name, extra, budget)
        rec["expectation"] = expect

        if expect == "refuse":
            refused = (rec["exit"] not in (0, "spawn-failed", None)
                       and not rec["timeout"])
            rec["pass"] = bool(refused and rec["marker_present"])
            if refused and not rec["marker_present"]:
                rec["note"] = (
                    "nonzero exit without marker %r is NOT a pass "
                    "(unknown-feature/dependency/build-script failures "
                    "do not satisfy C1)" % MARKER)
        else:  # build case
            if rec["exit"] == 0 and binary_path.is_file():
                scan = _scan_binary(binary_path)
                rec["binary_scan"] = scan
                rec["pass"] = not scan["adapter_markers_found"]
            else:
                rec["pass"] = False
                rec["binary_scan"] = {
                    "path": str(binary_path), "present": binary_path.is_file(),
                }
        if not rec["pass"]:
            overall_ok = False
        report["cases"].append(rec)

    report["elapsed_s"] = round(time.monotonic() - t0, 3)
    report["overall"] = "pass" if overall_ok else "fail"
    _write_report(report, outdir)

    sys.stderr.write(
        "C1 driver overall=%s head=%s report=%s\n"
        % (report["overall"], report["head"], outdir / "report.json"))
    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
