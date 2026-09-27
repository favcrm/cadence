#!/usr/bin/env python3
"""Exercise feedback with an actual fake runner, without compiling Rust."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("feedback", ROOT / "scripts/test-feedback.py")
feedback = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(feedback)
CONTROL_SHA = "a" * 40
CANDIDATE_SHA = "b" * 40


class FeedbackTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="feedback-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.fixture_count = 0
        # Fake source/toolchain metadata only; the runner process itself is real.
        self.source = patch.object(feedback, "source_sha")
        self.source_mock = self.source.start()
        self.addCleanup(self.source.stop)
        self.version = patch.object(feedback.subprocess, "run")
        self.version.start().return_value.stdout = "rustc fake-test-version\n"
        self.addCleanup(self.version.stop)
        self.new_fixture()

    def new_fixture(self):
        self.fixture_count += 1
        fixture = self.root / str(self.fixture_count)
        self.control = fixture / "control"
        self.candidate = fixture / "candidate"
        (self.control / "scripts").mkdir(parents=True)
        self.candidate.mkdir()
        self.output = fixture / "evidence"
        self.args = argparse.Namespace(
            command="run", revision=CANDIDATE_SHA, control_sha=CONTROL_SHA,
            case="cad663-stale-turn", candidate=str(self.candidate), output=str(self.output),
        )
        target, name = feedback.CASES[self.args.case]
        self.report = f'<testsuites><testsuite name="cadence-agent::{target}"><testcase name="{name}">{{body}}</testcase></testsuite></testsuites>'
        self.source_mock.side_effect = [CONTROL_SHA, CANDIDATE_SHA]

    def fake_runner(self, report=None, code=0, stale=False, sleep=0, noise=0):
        runner = self.control / "scripts/cadence-nextest"
        runner.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, pathlib, sys, time\n"
            "root = pathlib.Path(os.environ['CARGO_TARGET_DIR'])\n"
            "root.mkdir(parents=True, exist_ok=True)\n"
            "capture = root / 'capture.json'\n"
            "assert not capture.exists(), 'runner was invoked twice'\n"
            "capture.write_text(json.dumps({'argv': sys.argv[1:], 'cwd': os.getcwd(), 'env': dict(os.environ)}))\n"
            f"report = {report!r}\n"
            "if report is not None:\n"
            "    path = root / 'nextest/cadence/junit.xml'\n"
            "    path.parent.mkdir(parents=True, exist_ok=True)\n"
            "    path.write_text(report)\n"
            f"    if {stale!r}: os.utime(path, (1, 1))\n"
            f"print('x' * {noise}, flush=True)\n"
            f"time.sleep({sleep})\n"
            f"sys.exit({code})\n"
        )
        runner.chmod(0o755)

    def execute(self, **kwargs):
        code = feedback.execute(self.args, self.control, **kwargs)
        return code, json.loads((self.output / "outcome.json").read_text())

    def test_pass_and_assertion_failure_are_ordinary_opposite_job_results(self):
        for code, body, outcome, expected in (
            (0, "", "pass", 0), (100, '<failure message="assertion failed"/>', "test_failure", 1),
        ):
            with self.subTest(code=code):
                self.new_fixture()
                self.fake_runner(self.report.format(body=body), code)
                result, receipt = self.execute()
                self.assertEqual((result, receipt["outcome"], receipt["returncode"]), (expected, outcome, code))
                self.assertEqual((self.output / "junit.xml").read_text(), self.report.format(body=body))

    def test_invalid_reports_and_exit_combinations_stay_red(self):
        valid = self.report.format(body="")
        failure = self.report.format(body="<failure/>")
        samples = (
            (None, 0), ("not xml", 0), ("<testsuites/>", 0),
            (valid.replace("</testsuite>", '<testcase name="extra"/></testsuite>'), 0),
            (valid.replace("when_idle_refuses", "wrong_name"), 0),
            (valid.replace("cadence-agent::backup_rollout", "cadence-agent::threads"), 0),
            (self.report.format(body="<skipped/>"), 0),
            (self.report.format(body="<error/>"), 100),
            (valid.replace("<testsuites>", '<testsuites skipped="1">'), 0),
            (valid.replace("<testsuites>", '<testsuites errors="1">'), 0),
            (valid, 100), (failure, 0), (failure, 103), (valid, 1),
        )
        for report, code in samples:
            with self.subTest(report=report, code=code):
                self.new_fixture()
                self.fake_runner(report, code)
                result, receipt = self.execute()
                self.assertEqual((result, receipt["outcome"], receipt["returncode"]), (1, "invalid_feedback", code))
                if report is not None:
                    self.assertEqual((self.output / "junit.xml").read_text(), report)

    def test_stale_report_and_preexisting_report_cannot_pass(self):
        for rewrite in (False, True):
            with self.subTest(rewrite=rewrite):
                self.new_fixture()
                report = self.candidate / "target/nextest/cadence/junit.xml"
                report.parent.mkdir(parents=True)
                report.write_text(self.report.format(body=""))
                self.fake_runner(self.report.format(body="") if rewrite else None, stale=True)
                result, receipt = self.execute()
                self.assertEqual((result, receipt["outcome"]), (1, "invalid_feedback"))
                self.assertIn("stale" if rewrite else "missing", receipt["reason"])

    def test_input_allowlist_and_exact_sha_checks_prevent_execution(self):
        for revision, case, control in (
            ("main", self.args.case, CONTROL_SHA), ("B" * 40, self.args.case, CONTROL_SHA),
            (CANDIDATE_SHA + "\n", self.args.case, CONTROL_SHA),
            ("$(touch bad)", self.args.case, CONTROL_SHA),
            (CANDIDATE_SHA, "threads; echo bad", CONTROL_SHA),
            (CANDIDATE_SHA, self.args.case, "main"),
        ):
            with self.subTest(revision=revision, case=case):
                self.args.revision, self.args.case, self.args.control_sha = revision, case, control
                result, receipt = self.execute()
                self.assertEqual((result, receipt["outcome"], receipt["returncode"]), (1, "invalid_feedback", None))
                self.assertFalse((self.candidate / "target/capture.json").exists())
        self.args.revision, self.args.case, self.args.control_sha = CANDIDATE_SHA, "cad663-stale-turn", CONTROL_SHA
        for shas in ((CANDIDATE_SHA,), (CONTROL_SHA, CONTROL_SHA)):
            with patch.object(feedback, "source_sha", side_effect=shas):
                result, receipt = self.execute()
                self.assertEqual((result, receipt["returncode"]), (1, None))
                self.assertIn("checkout differs", receipt["reason"])

    def test_exact_forwarding_and_child_environment_from_trusted_control(self):
        self.fake_runner(self.report.format(body=""))
        # A candidate config does not change the trusted wrapper's cwd or flags.
        (self.candidate / ".cargo").mkdir()
        (self.candidate / ".cargo/config.toml").write_text('[build]\ntarget-dir = "wrong"\n')
        with patch.dict(os.environ, {
            "HOME": "/original", "CARGO_HOME": "/cargo", "RUSTUP_HOME": "/rustup",
            "RUSTC_WRAPPER": "/retained-wrapper", "CARGO_BUILD_JOBS": "99",
            "RUSTFLAGS": "-D warnings",
            "CADENCE_REVIEW_SUITE_LOCK_HELD": "1", "CADENCE_REVIEW_HEAD": "forged",
            "CADENCE_REVIEW_PR": "forged", "CADENCE_SUITE_LOCK": "/foreign-lock",
        }):
            result, receipt = self.execute()
        self.assertEqual(result, 0)
        capture = json.loads((self.candidate / "target/capture.json").read_text())
        self.assertEqual(capture["argv"], ["--manifest-path", str(self.candidate / "Cargo.toml"), "--locked", "--features", "test-seam", "--test", "backup_rollout", "--", feedback.CASES[self.args.case][1], "--exact"])
        self.assertEqual(capture["cwd"], str(self.control))
        env = capture["env"]
        self.assertEqual((env["CARGO_HOME"], env["RUSTUP_HOME"], env["RUSTC_WRAPPER"]), ("/cargo", "/rustup", "/retained-wrapper"))
        self.assertEqual((env["CARGO_TARGET_DIR"], env["CARGO_BUILD_JOBS"]), (str(self.candidate / "target"), "4"))
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMPDIR", "CADENCE_SUITE_LOCK"):
            self.assertTrue(env[key].startswith("/tmp/cfb-"), (key, env[key]))
        self.assertNotIn("CADENCE_REVIEW_SUITE_LOCK_HELD", env)
        self.assertNotIn("CADENCE_REVIEW_HEAD", env)
        self.assertNotIn("CADENCE_REVIEW_PR", env)
        self.assertEqual((receipt["control_sha"], receipt["candidate_sha"]), (CONTROL_SHA, CANDIDATE_SHA))
        self.assertEqual(receipt["build_context"]["rustflags"], "-D warnings")

    def test_original_home_toolchain_fallback_and_second_allowlisted_case(self):
        self.args.case = "cad650-master-interrupt"
        target, name = feedback.CASES[self.args.case]
        report = f'<testsuite name="cadence-agent::{target}"><testcase name="{name}"/></testsuite>'
        self.fake_runner(report)
        with patch.dict(os.environ, {"HOME": "/original", "PATH": os.environ["PATH"]}, clear=True):
            result, _ = self.execute()
        self.assertEqual(result, 0)
        capture = json.loads((self.candidate / "target/capture.json").read_text())
        self.assertEqual((capture["env"]["CARGO_HOME"], capture["env"]["RUSTUP_HOME"]), ("/original/.cargo", "/original/.rustup"))
        self.assertEqual(capture["argv"][-4:], ["threads", "--", name, "--exact"])

    def test_unchanged_control_wrapper_pins_config_and_retries(self):
        self.fake_runner(self.report.format(body=""))
        runner = self.control / "scripts/cadence-nextest"
        binary = self.control / "cargo-nextest"
        runner.rename(binary)
        binary.write_text(binary.read_text().replace(
            "root = pathlib.Path", "if sys.argv[1:] == ['--version']:\n    print('cargo-nextest 0.9.145')\n    sys.exit(0)\nroot = pathlib.Path",
        ))
        shutil.copyfile(ROOT / "scripts/cadence-nextest", runner)
        runner.chmod(0o755)
        config = self.control / ".config"
        config.mkdir()
        shutil.copyfile(ROOT / ".config/nextest.toml", config / "nextest.toml")
        (config / "cargo-nextest.sha256").write_text(
            f"{hashlib.sha256(binary.read_bytes()).hexdigest()}  cargo-nextest\n",
        )
        with patch.dict(os.environ, {"CADENCE_NEXTTEST_BIN": str(binary), "NEXTEST_RETRIES": "9", "NEXTEST_PROFILE": "wrong"}):
            result, _ = self.execute()
        self.assertEqual(result, 0)
        capture = json.loads((self.candidate / "target/capture.json").read_text())
        self.assertEqual(capture["argv"][:10], ["nextest", "run", "--config-file", str(config / "nextest.toml"), "--profile", "cadence", "--retries", "0", "--no-tests", "fail"])
        self.assertEqual(capture["env"]["CADENCE_REVIEW_SUITE_LOCK_HELD"], "1")
        self.assertEqual(capture["env"]["CADENCE_SUITE_LOCK"], "")
        self.assertNotIn("NEXTEST_RETRIES", capture["env"])
        self.assertNotIn("NEXTEST_PROFILE", capture["env"])

    def test_timeout_and_log_bound_preserve_process_result(self):
        self.fake_runner(self.report.format(body=""), sleep=10, noise=feedback.LOG_LIMIT * 2)
        result, receipt = self.execute(timeout=0.5)
        self.assertEqual((result, receipt["outcome"], receipt["timed_out"]), (1, "invalid_feedback", True))
        self.assertLess(receipt["returncode"], 0)
        self.assertLessEqual((self.output / "runner.log").stat().st_size, feedback.LOG_LIMIT)
        self.assertTrue((self.output / "junit.xml").is_file())

    def test_missing_runner_and_linked_report_are_invalid(self):
        result, receipt = self.execute()
        self.assertEqual((result, receipt["returncode"]), (1, None))
        self.new_fixture()
        external = self.root / "external.xml"
        external.write_text(self.report.format(body=""))
        report = self.candidate / "target/nextest/cadence/junit.xml"
        report.parent.mkdir(parents=True)
        report.symlink_to(external)
        result, receipt = self.execute()
        self.assertEqual((result, receipt["returncode"]), (1, None))
        self.assertIn("symlink", receipt["reason"])
        self.assertEqual(external.read_text(), self.report.format(body=""))

    def test_toolchain_setup_failure_does_not_execute_runner(self):
        self.fake_runner(self.report.format(body=""))
        with patch.object(feedback.subprocess, "run", side_effect=subprocess.CalledProcessError(1, ["rustc", "--version"])):
            result, receipt = self.execute()
        self.assertEqual((result, receipt["outcome"], receipt["returncode"]), (1, "invalid_feedback", None))
        self.assertFalse((self.candidate / "target/capture.json").exists())

    def test_linked_report_parent_is_not_followed_for_preservation(self):
        external = self.root / "external"
        external.mkdir()
        (external / "junit.xml").write_text(self.report.format(body=""))
        parent = self.candidate / "target/nextest"
        parent.mkdir(parents=True)
        (parent / "cadence").symlink_to(external, target_is_directory=True)
        result, receipt = self.execute()
        self.assertEqual((result, receipt["outcome"], receipt["returncode"]), (1, "invalid_feedback", None))
        self.assertFalse((self.output / "junit.xml").exists())

    def test_preflight_preserves_invalid_until_execution_receipt(self):
        self.args.command = "validate"
        result, receipt = self.execute()
        self.assertEqual((result, receipt["outcome"], receipt["candidate_sha"], receipt["returncode"]), (0, "invalid_feedback", None, None))
        self.assertIn("execution not reached", receipt["reason"])


if __name__ == "__main__":
    unittest.main()
