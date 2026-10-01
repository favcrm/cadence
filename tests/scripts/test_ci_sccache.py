#!/usr/bin/env python3
"""CAD-904: the shared sccache is write-gated and a strict no-op without secrets.

Stdlib only, like test_ci_shared_checks.py. It inspects the checked-in
workflow and executes scripts/ci-sccache against stub tools: no network, no
real credential, no R2.
"""
from pathlib import Path
import hashlib
import io
import os
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
SCRIPT = ROOT / "scripts/ci-sccache"
RUST_JOBS = ("clippy", "test-shard", "test-once", "build", "ui")
RW_SECRET = re.compile(r"secrets\.SCCACHE_R2_RW_")
ENV_NAME = "sccache-writer"


def job_body(workflow, name):
    body = workflow.split(f"\n  {name}:\n", 1)[1]
    return re.split(r"\n  [a-z][a-z-]*:\n", body, maxsplit=1)[0]


def job_names(workflow):
    jobs = workflow.split("\njobs:\n", 1)[1]
    return re.findall(r"(?m)^  ([a-z][a-z-]*):$", jobs)


def guard_allows(expr, event, ref):
    """Evaluate the ${{ ... }} guard in front of `&& secret/'sccache-writer'`."""
    py = expr.replace("&&", " and ").replace("||", " or ")
    py = py.replace("github.event_name", "EV").replace("github.ref", "REF")
    # Test-only: the input is the checked-in workflow text, and eval runs
    # only after it is reduced to the known comparison grammar.
    rest = re.sub(r"EV|REF|and|or|==|[()\s]|'[A-Za-z_/.:*-]+'", "", py)
    if rest:
        raise AssertionError(f"unsupported guard expression: {expr!r}")
    return bool(eval(py, {"__builtins__": {}}, {"EV": event, "REF": ref}))


EVENTS = [
    ("pull_request", "refs/pull/1/merge"),
    ("workflow_dispatch", "refs/heads/main"),
    ("workflow_dispatch", "refs/heads/feature"),
    ("push", "refs/heads/feature"),
    ("push", "refs/tags/v1.0.0"),
    ("merge_group", "refs/heads/gh-readonly-queue/main/pr-1-abc"),
    ("push", "refs/heads/main"),
]
WRITERS = {EVENTS[5], EVENTS[6]}


def guarded_expressions(line):
    """The guard of every `${{ (guard) && X || '' }}` on a line."""
    return re.findall(r"\$\{\{\s*(\(.+?\))\s*&&\s*(?:secrets\.\w+|'sccache-writer')\s*\|\|\s*''\s*\}\}", line)


def assert_rw_gated(case, workflow):
    case.assertNotIn("pull_request_target", workflow)
    case.assertNotRegex(workflow, r"secrets:\s*inherit")
    users = [j for j in job_names(workflow) if RW_SECRET.search(job_body(workflow, j))]
    case.assertEqual(sorted(users), sorted(RUST_JOBS))
    # Never at workflow level or outside a job.
    head = workflow.split("\njobs:\n", 1)[0]
    case.assertNotRegex(head, r"SCCACHE_R2_RW")
    for job in users:
        body = job_body(workflow, job)
        env_lines = re.findall(r"(?m)^    environment: (.+)$", body)
        case.assertEqual(len(env_lines), 1, f"{job} needs one environment")
        exprs = guarded_expressions(env_lines[0])
        case.assertEqual(len(exprs), 1, f"{job} environment must be guarded: {env_lines[0]}")
        case.assertIn(f"'{ENV_NAME}'", env_lines[0])
        for event, ref in EVENTS:
            case.assertEqual(guard_allows(exprs[0], event, ref), (event, ref) in WRITERS,
                             f"{job} environment for {event} {ref}")
        for line in body.splitlines():
            if not RW_SECRET.search(line):
                continue
            gs = guarded_expressions(line)
            case.assertEqual(len(gs), 1, f"unguarded RW secret in {job}: {line.strip()}")
            for event, ref in EVENTS:
                case.assertEqual(guard_allows(gs[0], event, ref), (event, ref) in WRITERS,
                                 f"{job} RW secret visible to {event} {ref}")
    # The environment name appears only on those guarded job lines.
    for job in job_names(workflow):
        has = ENV_NAME in job_body(workflow, job)
        case.assertEqual(has, job in users, f"{job}: {ENV_NAME} placement")


def assert_no_secret_path_has_no_wrapper(case, workflow):
    # Only the script may set the wrapper, and only after credentials resolve.
    case.assertNotRegex(workflow, r"(?m)^\s*RUSTC_WRAPPER:|RUSTC_WRAPPER=sccache|export RUSTC_WRAPPER")
    for job in RUST_JOBS:
        body = job_body(workflow, job)
        case.assertEqual(body.count("scripts/ci-sccache enable"), 1, job)
        case.assertEqual(body.count("scripts/ci-sccache stats"), 1, job)
    # release-artifact is attested: it never uses the shared cache.
    for job in ("release-artifact", "release-gate", "release-publish", "fmt", "test", "queue-evidence"):
        body = job_body(workflow, job)
        case.assertNotIn("ci-sccache", body, job)
        case.assertNotRegex(body, r"SCCACHE_R2_R[WO]_")


class WorkflowContract(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text()

    def test_rw_secrets_only_in_writer_environment_jobs(self):
        assert_rw_gated(self, self.workflow)

    def test_no_pull_request_path_sees_rw_secrets(self):
        # The same guard is evaluated for every event; also the RO pair is
        # the only secret a PR job is ever handed.
        assert_rw_gated(self, self.workflow)
        for job in RUST_JOBS:
            for line in job_body(self.workflow, job).splitlines():
                if "secrets." in line and not RW_SECRET.search(line):
                    self.assertRegex(line, r"secrets\.SCCACHE_R2_RO_(ACCESS_KEY_ID|SECRET_ACCESS_KEY) \}\}$")

    def test_no_secret_path_does_not_set_wrapper(self):
        assert_no_secret_path_has_no_wrapper(self, self.workflow)

    def test_unguarded_rw_secret_is_rejected(self):
        guard = "(github.event_name == 'merge_group' || (github.event_name == 'push' && github.ref == 'refs/heads/main')) && secrets.SCCACHE_R2_RW_ACCESS_KEY_ID || ''"
        self.assertIn(guard, self.workflow)
        mutated = self.workflow.replace(guard, "secrets.SCCACHE_R2_RW_ACCESS_KEY_ID", 1)
        with self.assertRaises(AssertionError):
            assert_rw_gated(self, mutated)

    def test_pull_request_in_guard_is_rejected(self):
        old = "(github.event_name == 'merge_group' || (github.event_name == 'push' && github.ref == 'refs/heads/main')) && secrets.SCCACHE_R2_RW_SECRET_ACCESS_KEY"
        new = "(github.event_name == 'merge_group' || github.event_name == 'pull_request') && secrets.SCCACHE_R2_RW_SECRET_ACCESS_KEY"
        self.assertIn(old, self.workflow)
        with self.assertRaises(AssertionError):
            assert_rw_gated(self, self.workflow.replace(old, new, 1))

    def test_push_without_main_ref_is_rejected(self):
        old = "(github.event_name == 'push' && github.ref == 'refs/heads/main')"
        for job in RUST_JOBS:
            body = job_body(self.workflow, job)
            self.assertIn(old, body)
        mutated = self.workflow.replace(old, "github.event_name == 'push'")
        with self.assertRaises(AssertionError):
            assert_rw_gated(self, mutated)

    def test_missing_environment_is_rejected(self):
        env = re.search(r"(?m)^    environment: .*sccache-writer.*\n", job_body(self.workflow, "clippy"))
        mutated = self.workflow.replace(env[0], "", 1)
        with self.assertRaises(AssertionError):
            assert_rw_gated(self, mutated)

    def test_rw_secret_in_extra_job_is_rejected(self):
        marker = "  fmt:\n    needs: [queue-evidence]\n"
        self.assertIn(marker, self.workflow)
        mutated = self.workflow.replace(marker, marker + "    env:\n      X: ${{ secrets.SCCACHE_R2_RW_ACCESS_KEY_ID }}\n", 1)
        with self.assertRaises(AssertionError):
            assert_rw_gated(self, mutated)

    def test_pull_request_target_is_rejected(self):
        with self.assertRaises(AssertionError):
            assert_rw_gated(self, self.workflow.replace("  pull_request:\n", "  pull_request_target:\n", 1))

    def test_workflow_level_wrapper_is_rejected(self):
        mutated = self.workflow.replace("  CARGO_TERM_COLOR: always\n", "  CARGO_TERM_COLOR: always\n  RUSTC_WRAPPER: sccache\n", 1)
        with self.assertRaises(AssertionError):
            assert_no_secret_path_has_no_wrapper(self, mutated)

    def test_attested_release_build_never_uses_the_cache(self):
        mutated = self.workflow.replace("  release-artifact:\n", "  release-artifact:\n    # scripts/ci-sccache enable\n", 1)
        with self.assertRaises(AssertionError):
            assert_no_secret_path_has_no_wrapper(self, mutated)


class Harness:
    """Run a copy of scripts/ci-sccache against stub curl and stub sccache."""

    def __init__(self, case):
        self.dir = Path(tempfile.mkdtemp(prefix="sc904-", dir="/tmp"))
        case.addCleanup(shutil.rmtree, self.dir, True)
        (self.dir / "repo/scripts").mkdir(parents=True)
        (self.dir / "repo/.config").mkdir()
        shutil.copy(SCRIPT, self.dir / "repo/scripts/ci-sccache")
        self.bin = self.dir / "bin"
        self.bin.mkdir()
        self.github_env = self.dir / "github_env"
        self.github_path = self.dir / "github_path"
        self.summary = self.dir / "summary"
        for p in (self.github_env, self.github_path, self.summary):
            p.write_text("")
        self.runner_temp = self.dir / "tmp"
        self.runner_temp.mkdir()
        self.archive = "sccache-v0.18.0-x86_64-unknown-linux-musl.tar.gz"

    def write_manifest(self, sha):
        (self.dir / "repo/.config/sccache.sha256").write_text(f"{sha}  {self.archive}\n")

    def stub_curl(self, payload):
        """curl stub: copy `payload` bytes to --output, or fail when None."""
        script = self.bin / "curl"
        if payload is None:
            script.write_text("#!/bin/sh\nexit 22\n")
        else:
            blob = self.dir / "payload"
            blob.write_bytes(payload)
            script.write_text(
                '#!/bin/sh\nwhile [ $# -gt 0 ]; do [ "$1" = --output ] && out=$2; shift; done\n'
                f'cp {blob} "$out"\n')
        script.chmod(script.stat().st_mode | stat.S_IXUSR)

    def fake_archive(self, start_rc=0):
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as tf:
            body = f"#!/bin/sh\nexit {start_rc}\n".encode()
            info = tarfile.TarInfo("sccache-v0.18.0-x86_64-unknown-linux-musl/sccache")
            info.size = len(body)
            info.mode = 0o755
            tf.addfile(info, io.BytesIO(body))
        return buf.getvalue()

    def run(self, command="enable", **env):
        base = {
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "HOME": str(self.dir),
            "GITHUB_ENV": str(self.github_env),
            "GITHUB_PATH": str(self.github_path),
            "GITHUB_STEP_SUMMARY": str(self.summary),
            "RUNNER_TEMP": str(self.runner_temp),
        }
        base.update(env)
        return subprocess.run(["sh", str(self.dir / "repo/scripts/ci-sccache"), command],
                              env=base, capture_output=True, text=True)

    def exported(self):
        return self.github_env.read_text()


ENDPOINT = "https://0123456789abcdef.r2.cloudflarestorage.com"


def good_install(h, start_rc=0):
    blob = h.fake_archive(start_rc)
    h.write_manifest(hashlib.sha256(blob).hexdigest())
    h.stub_curl(blob)


class ScriptBehaviour(unittest.TestCase):
    def assertDisabled(self, h, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("RUSTC_WRAPPER", h.exported())
        self.assertEqual(h.exported(), "")
        self.assertIn("disabled", result.stdout)

    def test_no_credentials_is_a_noop(self):
        h = Harness(self)
        good_install(h)
        self.assertDisabled(h, h.run())
        self.assertDisabled(h, h.run(SCCACHE_CI_ENDPOINT=ENDPOINT))
        self.assertFalse((h.runner_temp / "sccache-0.18.0").exists(), "nothing downloaded without credentials")

    def test_credentials_without_endpoint_is_a_noop(self):
        h = Harness(self)
        good_install(h)
        self.assertDisabled(h, h.run(SCCACHE_CI_RO_ACCESS_KEY_ID="id", SCCACHE_CI_RO_SECRET_ACCESS_KEY="s"))

    def test_half_a_pair_is_a_noop(self):
        h = Harness(self)
        good_install(h)
        self.assertDisabled(h, h.run(SCCACHE_CI_ENDPOINT=ENDPOINT, SCCACHE_CI_RW_ACCESS_KEY_ID="id",
                                     SCCACHE_CI_RO_SECRET_ACCESS_KEY="s"))

    def test_non_https_endpoint_is_a_noop(self):
        h = Harness(self)
        good_install(h)
        self.assertDisabled(h, h.run(SCCACHE_CI_ENDPOINT="http://x.example", SCCACHE_CI_RO_ACCESS_KEY_ID="id",
                                     SCCACHE_CI_RO_SECRET_ACCESS_KEY="s"))

    def test_failed_download_fails_open(self):
        h = Harness(self)
        h.write_manifest("0" * 64)
        h.stub_curl(None)
        self.assertDisabled(h, h.run(SCCACHE_CI_ENDPOINT=ENDPOINT, SCCACHE_CI_RO_ACCESS_KEY_ID="id",
                                     SCCACHE_CI_RO_SECRET_ACCESS_KEY="s"))

    def test_checksum_mismatch_fails_open_and_installs_nothing(self):
        h = Harness(self)
        h.write_manifest("0" * 64)
        h.stub_curl(h.fake_archive())
        result = h.run(SCCACHE_CI_ENDPOINT=ENDPOINT, SCCACHE_CI_RO_ACCESS_KEY_ID="id",
                       SCCACHE_CI_RO_SECRET_ACCESS_KEY="s")
        self.assertDisabled(h, result)
        self.assertIn("checksum", result.stdout)
        self.assertEqual(h.github_path.read_text(), "")

    def test_unreachable_backend_fails_open(self):
        h = Harness(self)
        good_install(h, start_rc=1)
        result = h.run(SCCACHE_CI_ENDPOINT=ENDPOINT, SCCACHE_CI_RO_ACCESS_KEY_ID="id",
                       SCCACHE_CI_RO_SECRET_ACCESS_KEY="s")
        self.assertDisabled(h, result)
        self.assertIn("not reachable", result.stdout)

    def test_read_only_pair_gives_read_only_mode(self):
        h = Harness(self)
        good_install(h)
        result = h.run(SCCACHE_CI_ENDPOINT=ENDPOINT, SCCACHE_CI_RO_ACCESS_KEY_ID="roid",
                       SCCACHE_CI_RO_SECRET_ACCESS_KEY="rosecret")
        self.assertEqual(result.returncode, 0, result.stderr)
        out = h.exported()
        self.assertIn("RUSTC_WRAPPER=sccache\n", out)
        self.assertIn("SCCACHE_S3_RW_MODE=READ_ONLY\n", out)
        self.assertIn("SCCACHE_BUCKET=cadence-ci-sccache\n", out)
        self.assertIn("SCCACHE_REGION=auto\n", out)
        self.assertIn("AWS_ACCESS_KEY_ID=roid\n", out)
        self.assertIn(f"SCCACHE_ENDPOINT={ENDPOINT}\n", out)
        self.assertIn("CARGO_INCREMENTAL=0\n", out)
        self.assertIn("::add-mask::rosecret", result.stdout)
        self.assertNotIn("rosecret", result.stderr)
        self.assertIn("sccache-0.18.0", h.github_path.read_text())

    def test_read_write_pair_wins_only_when_supplied(self):
        h = Harness(self)
        good_install(h)
        result = h.run(SCCACHE_CI_ENDPOINT=ENDPOINT, SCCACHE_CI_RW_ACCESS_KEY_ID="rwid",
                       SCCACHE_CI_RW_SECRET_ACCESS_KEY="rwsecret",
                       SCCACHE_CI_RO_ACCESS_KEY_ID="roid", SCCACHE_CI_RO_SECRET_ACCESS_KEY="rosecret")
        self.assertEqual(result.returncode, 0, result.stderr)
        out = h.exported()
        self.assertIn("SCCACHE_S3_RW_MODE=READ_WRITE\n", out)
        self.assertIn("AWS_ACCESS_KEY_ID=rwid\n", out)
        self.assertNotIn("roid", out)

    def test_stats_is_a_noop_when_disabled(self):
        h = Harness(self)
        result = h.run("stats")
        self.assertEqual(result.returncode, 0)
        self.assertIn("not enabled", result.stdout)

    def test_unknown_command_is_rejected(self):
        h = Harness(self)
        self.assertEqual(h.run("bogus").returncode, 2)


class PinnedTool(unittest.TestCase):
    def test_manifest_pins_the_script_version(self):
        text = SCRIPT.read_text()
        version = re.search(r"(?m)^VERSION=(\S+)$", text)[1]
        manifest = (ROOT / ".config/sccache.sha256").read_text()
        entry = re.search(rf"(?m)^([0-9a-f]{{64}})  sccache-v{re.escape(version)}-x86_64-unknown-linux-musl\.tar\.gz$", manifest)
        self.assertIsNotNone(entry)

    def test_script_is_executable(self):
        self.assertTrue(os.access(SCRIPT, os.X_OK))


if __name__ == "__main__":
    unittest.main()
