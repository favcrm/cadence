#!/usr/bin/env python3
"""Executable workflow boundary checks plus one required-gate wiring smoke test."""
from pathlib import Path
import json
import os
import re
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parent.parent


def workflow(name):
    return (ROOT / ".github/workflows" / name).read_text()


def job(text, name):
    match = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:\n|\Z)",
                      text, re.M | re.S)
    if not match:
        raise AssertionError(f"missing job {name}")
    return match.group(1)


class CleanupTests(unittest.TestCase):
    def test_required_gates_still_have_real_checks_and_scope_wiring(self):
        ci = workflow("ci.yml")
        self.assertIn("  merge_group:", ci)
        for name in ("fmt", "clippy", "test", "build", "ui"):
            body = job(ci, name)
            self.assertIn("change-scope", body)
            self.assertIn("needs.queue-evidence.outputs.tested", body)
            commands = [step for step in re.split(r"(?=^      - )", body, flags=re.M)
                        if re.search(r"(?:run:|uses:).*(?:cargo |pnpm |rust-cache@|ci-rust-toolchain)", step)]
            self.assertTrue(commands, name)
            for step in commands:
                category = "ui" if name == "ui" else "rust"
                self.assertIn(f"needs.change-scope.outputs.{category} != 'false'", step)
        test = job(ci, "test")
        # CAD-1124: retiring the empty doctest invocation must retain BOTH
        # default-feature compilation gates, not just feature-on tests.
        rust_scope = "        if: ${{ needs.change-scope.outputs.rust != 'false' }}"
        for name, command in (
            ("clippy", "cargo clippy --all-targets --locked -- -D warnings"),
            ("build", "cargo build --profile ci --locked"),
        ):
            steps = re.split(r"(?=^      - )", job(ci, name), flags=re.M)
            matching = [step for step in steps if re.search(
                rf"^      (?:- |  )run: {re.escape(command)}$", step, re.M)]
            self.assertEqual(len(matching), 1, (name, command))
            self.assertIn(rust_scope, matching[0])
            self.assertNotIn("continue-on-error:", matching[0])
        build_steps = re.split(r"(?=^      - )", job(ci, "build"), flags=re.M)
        for name in ("Assert the test seam refuses a release build",
                     "Assert the ci-profile gate binary carries no test seam"):
            matching = [step for step in build_steps
                        if step.startswith(f"      - name: {name}\n")]
            self.assertEqual(len(matching), 1, name)
            step = matching[0]
            self.assertIn(rust_scope, step)
            self.assertNotIn("continue-on-error:", step)
            if name == "Assert the test seam refuses a release build":
                self.assertRegex(step, r"(?m)^        run: sh scripts/check-release-test-seam$")
            else:
                self.assertRegex(
                    step,
                    r"(?m)^        run: \|\n"
                    r"          if grep -aq 'cadence-test-seam-v1' target/ci/cadence; then\n"
                    r"            echo [^\n]+\n"
                    r"            exit 1\n"
                    r"          fi$")
        # CAD-1090: every tests/*.rs target, via the shared selection script.
        self.assertIn("scripts/run-result-tests", test)
        runner = (ROOT / "scripts/run-result-tests").read_text()
        self.assertIn("scripts/result-test-args", runner)
        # CAD-1105: the lib unit tests run too, in the same runner.
        for needle in ("--features test-seam", "--no-fail-fast", "--test-threads 2",
                       "running 0 tests", "HOME=", "XDG_CONFIG_HOME=",
                       "--features test-seam --lib --no-fail-fast",
                       "CARGO_BUILD_TARGET_DIR", "unset CARGO_TARGET_DIR"):
            self.assertIn(needle, runner)
        selected = subprocess.run([str(ROOT / "scripts/result-test-args")],
                                  capture_output=True, text=True, check=True).stdout.split()
        self.assertIn("safety_floor", selected)
        meta = json.loads(subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
            cwd=ROOT, capture_output=True, text=True, check=True).stdout)
        cargo_tests = sorted({t["name"] for p in meta["packages"]
                              for t in p["targets"] if "test" in t["kind"]})
        self.assertTrue(cargo_tests)
        self.assertEqual(cargo_tests, sorted(selected[1::2]))
        self.assertEqual(set(selected[0::2]), {"--test"})
        self.assertIn("scripts/split-doctor-host --check", job(ci, "fmt"))

    def test_scope_shell_defaults_full_on_missing_or_forged_policy(self):
        scope = job(workflow("ci.yml"), "change-scope")
        block = scope.split("        run: |\n", 1)[1]
        script = textwrap.dedent(block)
        with tempfile.TemporaryDirectory() as root:
            attacker = Path(root) / "scripts/ci-change-scope.py"
            attacker.parent.mkdir()
            attacker.write_text("from pathlib import Path\nPath('forged-policy-executed').touch()\n"
                                "print('{\"scope\":\"ui\",\"rust\":\"false\",\"ui\":\"true\"}')\n")
            git = Path(root) / "git"
            git.write_text("#!/bin/sh\n[ \"$1\" = show ] || exit 1\n"
                           "[ \"$2\" = \"$BASE:scripts/ci-change-scope.py\" ] || exit 1\n"
                           "[ \"$TEST_MISSING\" = yes ] && exit 1\n"
                           "printf '%s\\n' 'import os' 'print(os.environ[\"TEST_RESULT\"])'\n")
            git.chmod(0o755)
            for event, missing, result, expected in (
                ("pull_request", "yes", '{}', "full"),
                ("pull_request", "no", '{"scope":"docs","rust":"true","ui":"false"}', "full"),
                ("pull_request", "no", '{"scope":"docs","rust":"false","ui":"false"}', "docs"),
                ("merge_group", "no", '{"scope":"docs","rust":"false","ui":"false"}', "full"),
            ):
                with self.subTest(event=event, missing=missing, result=result):
                    output = Path(root) / "output"
                    output.write_text("")
                    env = dict(os.environ, EVENT=event, BASE="0" * 40, HEAD="1" * 40,
                               RUNNER_TEMP=root, GITHUB_WORKSPACE=root,
                               GITHUB_OUTPUT=str(output), GITHUB_STEP_SUMMARY=str(Path(root) / "summary"),
                               TEST_MISSING=missing, TEST_RESULT=result,
                               PATH=root + os.pathsep + os.environ["PATH"])
                    run = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script],
                                         env=env, cwd=root, capture_output=True, text=True)
                    self.assertEqual(run.returncode, 0, run.stderr)
                    self.assertIn(f"scope={expected}\n", output.read_text())
                    self.assertFalse((Path(root) / 'forged-policy-executed').exists())

    def test_seam_probe_rejects_success_and_unrelated_build_failure(self):
        e2e = workflow("e2e.yml")
        block = re.search(r"      - name: MVP journey retired[^\n]*\n        run: \|\n(.*?)(?=      - if:)",
                          e2e, re.S).group(1)
        script = textwrap.dedent(block)
        with tempfile.TemporaryDirectory() as root:
            cargo = Path(root) / "cargo"
            for code, message, expected in (
                (0, "", 1),
                (1, "toolchain unavailable", 1),
                (1, "must never be compiled into a release build", 0),
            ):
                with self.subTest(code=code, message=message):
                    cargo.write_text(f"#!/bin/sh\necho '{message}' >&2\nexit {code}\n")
                    cargo.chmod(0o755)
                    env = dict(os.environ, RUNNER_TEMP=root,
                               PATH=root + os.pathsep + os.environ["PATH"])
                    result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script],
                                            env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)


# CAD-1105: scripts/run-result-tests against a stub cargo. The stub answers
# `cargo metadata` with one test target and logs every `cargo test` call
# with the environment the test binaries would inherit.
STUB_CARGO = r"""#!/bin/sh
case "$1" in
metadata)
  printf '%s' '{"packages":[{"targets":[{"name":"floor","kind":["test"]}]}]}'
  exit 0 ;;
test)
  printf '%s|PATH=%s|CTD=%s|CBTD=%s|HOME=%s\n' "$*" "$PATH" \
    "${CARGO_TARGET_DIR-unset}" "${CARGO_BUILD_TARGET_DIR-unset}" "$HOME" >> "$STUB_LOG"
  case " $* " in
  *" --lib "*)
    [ -n "${STUB_LIB_SILENT:-}" ] || echo "running ${STUB_LIB_N:-3} tests"
    exit "${STUB_LIB_RC:-0}" ;;
  *)
    echo "running 2 tests"
    [ -z "${STUB_INT_ZERO:-}" ] || echo "running 0 tests"
    exit "${STUB_INT_RC:-0}" ;;
  esac ;;
esac
exit 1
"""
SYSTEM_PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"


class RunResultTestsRunner(unittest.TestCase):
    def run_runner(self, root, toolchain_extra=(), **env_extra):
        toolchain = Path(root) / "toolchain"
        host = Path(root) / "host-bin"
        for d in (toolchain, host):
            d.mkdir(exist_ok=True)
        (toolchain / "cargo").write_text(STUB_CARGO)
        (toolchain / "cargo").chmod(0o755)
        # Host provider CLIs outside the toolchain dir, as on a dev box.
        for name in ("claude", "codex", "cursor-agent", "pi"):
            (host / name).write_text("#!/bin/sh\nexit 0\n")
            (host / name).chmod(0o755)
        for name in toolchain_extra:
            (toolchain / name).write_text("#!/bin/sh\nexit 0\n")
            (toolchain / name).chmod(0o755)
        log = Path(root) / "cargo-test.log"
        env = {"PATH": f"{toolchain}:{host}:/usr/bin:/bin", "RUNNER_TEMP": root,
               "STUB_LOG": str(log), "CARGO_HOME": str(Path(root) / "cargo-home"),
               "RUSTUP_HOME": str(Path(root) / "rustup-home"), "HOME": root}
        env.update(env_extra)
        result = subprocess.run([str(ROOT / "scripts/run-result-tests")], cwd=root,
                                env=env, capture_output=True, text=True)
        calls = log.read_text().splitlines() if log.exists() else []
        return result, calls, toolchain

    def test_lib_suite_runs_isolated_after_the_integration_targets(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as root:
            result, calls, toolchain = self.run_runner(root, CARGO_TARGET_DIR="rel/tgt")
            out = result.stdout + result.stderr
            self.assertEqual(result.returncode, 0, out)
            self.assertEqual(len(calls), 2, calls)
            integration, lib = calls
            self.assertIn("--test floor", integration)
            self.assertNotIn("--lib", integration)
            for needle in ("test --locked --features test-seam --lib --no-fail-fast",
                           "-- --test-threads 2"):
                self.assertIn(needle, lib)
            # The lib suite's PATH is the toolchain dir plus system dirs only.
            self.assertIn(f"|PATH={toolchain}:{SYSTEM_PATH}|", lib)
            self.assertNotIn("host-bin", lib)
            for call in calls:
                self.assertIn("|CTD=unset|", call)
                self.assertIn(f"|CBTD={root}/rel/tgt|", call)
                self.assertIn(f"|HOME={root}/result-tests.", call)

    def test_provider_cli_beside_cargo_refuses_before_any_test_runs(self):
        for cli in ("claude", "codex", "cursor-agent", "pi", "devin"):
            with self.subTest(cli=cli), tempfile.TemporaryDirectory(dir="/tmp") as root:
                result, calls, _ = self.run_runner(root, toolchain_extra=(cli,))
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(f"provider CLI {cli} resolves", result.stderr)
                self.assertEqual(calls, [])

    def test_red_lib_or_red_integration_fails_and_both_still_run(self):
        for env in ({"STUB_LIB_RC": "101"}, {"STUB_INT_RC": "101"}):
            with self.subTest(env=env), tempfile.TemporaryDirectory(dir="/tmp") as root:
                result, calls, _ = self.run_runner(root, **env)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual(len(calls), 2, calls)
                self.assertIn("--lib", calls[1])

    def test_target_that_runs_no_tests_fails(self):
        # One of several targets at 0 still fails; so does a lib with none.
        for env in ({"STUB_LIB_N": "0"}, {"STUB_LIB_SILENT": "1"}, {"STUB_INT_ZERO": "1"}):
            with self.subTest(env=env), tempfile.TemporaryDirectory(dir="/tmp") as root:
                result, calls, _ = self.run_runner(root, **env)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("ran 0 tests", result.stderr)
                self.assertEqual(len(calls), 2, calls)


if __name__ == "__main__":
    unittest.main()
