#!/usr/bin/env python3
"""Executable workflow boundary checks plus one required-gate wiring smoke test."""
from pathlib import Path
import json
import os
import re
import subprocess
import sys
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
    def test_pr_rehearsal_cache_is_read_only_and_never_replaces_real_checks(self):
        # CAD-1135: inspect the actual consumer configuration. This proves
        # wiring, not a remote cache hit or GitHub's runtime save refusal.
        staging = workflow("staging.yml")
        body = job(staging, "rehearsal-check")
        self.assertRegex(staging, r"(?m)^permissions:\n  contents: read\n  actions: read$")
        self.assertNotRegex(staging, r"(?m)^env:")
        self.assertRegex(body, r"(?m)^    if: github.event_name == 'pull_request'$")
        self.assertRegex(body, r"(?m)^    runs-on: ubuntu-24.04$")
        self.assertNotRegex(body, r"(?m)^    (?:permissions|defaults|environment|container):")
        self.assertNotIn("continue-on-error:", body)
        self.assertNotRegex(body, r"(?m)^      (?:- |  )shell:")
        self.assertNotIn("secrets.", body)
        self.assertNotIn("cache-hit", body)
        env = re.search(r"^    env:\n((?:      [^\n]*\n)+)", body, re.M)
        self.assertIsNotNone(env, "missing producer-matching pre-cache environment")
        self.assertEqual(env.group(1).splitlines(), [
            "      CARGO_TERM_COLOR: always", "      RUSTFLAGS: -D warnings",
        ])
        steps = [step for step in re.split(r"(?=^      - )", body, flags=re.M)
                 if step.startswith("      - ")]

        def step_index(field, value):
            matches = [i for i, step in enumerate(steps) if re.search(
                rf"^      (?:- |  ){field}: {re.escape(value)}(?: #.*)?$", step, re.M)]
            self.assertEqual(len(matches), 1, (field, value))
            return matches[0]

        install = step_index("run", "scripts/ci-rust-toolchain --profile minimal")
        cache = step_index("uses", "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6")
        export = step_index("run", "scripts/ci-rust-toolchain --export --profile minimal")
        ui_install = step_index("run", "pnpm install --frozen-lockfile")
        ui_build = step_index("run", "pnpm build")
        build = step_index("run", "cargo build --release --locked --features ui")
        rehearsal = step_index("name", "Real-binary migration and backup recovery rehearsal")
        self.assertLess(install, cache)
        self.assertLess(cache, export)
        self.assertLess(export, build)
        self.assertLess(ui_install, ui_build)
        self.assertLess(ui_build, build)
        self.assertLess(build, rehearsal)
        # No cache-dependent conditions on preparation, compile or proof:
        # a cold cache follows exactly the same required real-binary path.
        for step in steps[:rehearsal + 1]:
            self.assertNotRegex(step, r"(?m)^      (?:- |  )if:")
        for step in steps[:cache + 1]:
            self.assertNotRegex(step, r"(?m)^        env:")
            self.assertNotIn("GITHUB_ENV", step)
            self.assertNotIn("--export", step)
        cache_inputs = re.search(r"^        with:\n((?:          [^\n]*\n)+)", steps[cache], re.M)
        self.assertIsNotNone(cache_inputs)
        # Explicit literal false; no PR-writable expression, tracked-env
        # suppression or second cache action can grant cache-write authority.
        self.assertEqual(cache_inputs.group(1).splitlines(), [
            "          shared-key: gate-release-full", "          save-if: false",
        ])
        uses = re.findall(r"^      (?:- |  )uses: ([^ #\n]+)", body, re.M)
        self.assertCountEqual(uses, [
            "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683",
            "pnpm/action-setup@b906affcce14559ad1aafd4ab0e942779e9f58b1",
            "actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020",
            "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6",
            "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
        ])
        checkout = step_index("uses", "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683")
        self.assertRegex(steps[checkout], r"(?m)^          persist-credentials: false$")
        for i in (ui_install, ui_build):
            self.assertRegex(steps[i], r"(?m)^        working-directory: ui$")
        self.assertRegex(steps[build], r"(?m)^        env:\n          CARGO_BUILD_JOBS: 4$")
        # Exact baseline verification and local release candidate, not a
        # downloaded replacement, mock binary, or swallowed proof failure.
        proof = re.search(r"^        run: \|\n((?:          [^\n]*\n)+)", steps[rehearsal], re.M)
        self.assertIsNotNone(proof)
        self.assertEqual(textwrap.dedent(proof.group(1)),
                         'baseline=$(gh run list -R "$REPO" --workflow ci.yml --event push --branch main --status success --limit 1 --json databaseId --jq \'.[0].databaseId\')\n' +
                         'python3 scripts/delivery-candidate.py prepare --repo "$REPO" --run-id "$baseline" --dest "$RUNNER_TEMP/baseline"\n' +
                         'python3 scripts/staging-rehearsal.py --baseline "$RUNNER_TEMP/baseline/cadence" \\\n' +
                         '  --candidate "$GITHUB_WORKSPACE/target/release/cadence" --sha "$SOURCE_SHA" --out "$RUNNER_TEMP/rehearsal"\n')
        for key, value in (("GH_TOKEN", "${{ github.token }}"),
                           ("REPO", "${{ github.repository }}"),
                           ("SOURCE_SHA", "${{ github.sha }}")):
            self.assertRegex(steps[rehearsal], rf"(?m)^          {key}: {re.escape(value)}$")

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
        # CAD-1334: one pinned nextest invocation keeps every selected
        # integration target plus lib and bins, with bounded concurrency and
        # a result-level guard against empty suites.
        for needle in ('scripts/cadence-nextest" --locked --features test-seam',
                       '"${target_args[@]}" --lib --bins', '-j 2',
                       'no runnable tests', 'junit_report=', 'HOME=',
                       'XDG_CONFIG_HOME=', 'CARGO_BUILD_TARGET_DIR',
                       'unset CARGO_TARGET_DIR'):
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


# The provider-refusal case uses a stub cargo only to keep metadata lookup
# local; every runner behavior (selection, isolation, lock, and zero-test
# refusal) is exercised by the independent real-nextest acceptance below.
STUB_CARGO = r"""#!/bin/sh
case "$1" in
metadata)
  printf '%s' '{"packages":[{"targets":[{"name":"floor","kind":["test"]}]}]}'
  exit 0 ;;
esac
echo "unexpected cargo invocation: $*" >&2
exit 1
"""


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

    def test_real_nextest_runner_acceptance(self):
        check = ROOT / "scripts/acceptance/test-cad-1334-nextest-contract.py"
        result = subprocess.run([sys.executable, str(check)], cwd=ROOT,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_provider_cli_beside_cargo_refuses_before_any_test_runs(self):
        for cli in ("claude", "codex", "cursor-agent", "pi", "devin"):
            with self.subTest(cli=cli), tempfile.TemporaryDirectory(dir="/tmp") as root:
                result, calls, _ = self.run_runner(root, toolchain_extra=(cli,))
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(f"provider CLI {cli} resolves", result.stderr)
                self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
