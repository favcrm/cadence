#!/usr/bin/env python3
"""CAD-927: the Rust toolchain is pinned in rust-toolchain.toml and nowhere else.

A floating `stable` let a new clippy lint turn every queue entry red at once.
Stdlib only, like test_ci_shared_checks.py: the checks are pure functions over
file text, so the mutation tests can feed them a re-broken workflow or pin.
"""
from pathlib import Path
import os
import re
import stat
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = sorted((ROOT / ".github/workflows").glob("*.yml"))
TOOLCHAIN = ROOT / "rust-toolchain.toml"
SCRIPT = ROOT / "scripts/ci-rust-toolchain"
EXACT = re.compile(r"\d+\.\d+\.\d+")
INSTALL = re.compile(r"^\s*(?:- )?(?:run: )?(?:\./|control/|\$GITHUB_WORKSPACE/)?scripts/ci-rust-toolchain(?: .*)?$")
CARGO_USE = re.compile(r"\bcargo (?:build|test|clippy|fmt|check|nextest)\b|scripts/cadence-nextest")
JOB = re.compile(r"(?m)^  ([A-Za-z0-9_-]+):\n")


def check_pin(text):
    """Problems in rust-toolchain.toml text."""
    try:
        tc = tomllib.loads(text).get("toolchain", {})
    except tomllib.TOMLDecodeError as e:
        return [f"rust-toolchain.toml does not parse: {e}"]
    problems = []
    channel = tc.get("channel", "")
    if not EXACT.fullmatch(channel):
        problems.append(f"channel must be an exact x.y.z, got {channel!r}")
    if tc.get("profile") != "minimal":
        problems.append("profile must be minimal")
    for c in ("rustfmt", "clippy"):
        if c not in tc.get("components", []):
            problems.append(f"component {c} missing")
    return problems


def jobs(text):
    """{job name: body} for the top-level jobs: block."""
    body = text.split("\njobs:\n", 1)[1] if "\njobs:\n" in text else ""
    parts = JOB.split(body)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts) - 1, 2)}


def check_workflow(name, text):
    """Problems in one workflow's Rust installs."""
    problems = []
    code = [l for l in text.splitlines() if not l.lstrip().startswith("#")]
    for line in code:
        if re.search(r"rustup (?:toolchain install|default|override)\b", line):
            problems.append(f"{name}: direct rustup install/select: {line.strip()}")
        if "dtolnay/rust-toolchain" in line or "actions-rs/toolchain" in line:
            problems.append(f"{name}: toolchain action instead of the pin: {line.strip()}")
        if re.search(r"RUSTUP_TOOLCHAIN\s*[:=]", line):
            problems.append(f"{name}: hard-coded RUSTUP_TOOLCHAIN: {line.strip()}")
        if re.search(r"(?:\+stable\b|toolchain:\s*['\"]?(?:stable|beta|nightly)|--default-toolchain)", line):
            problems.append(f"{name}: floating or literal toolchain: {line.strip()}")
    for job, body in jobs(text).items():
        lines = body.splitlines()
        uses_cargo = any(CARGO_USE.search(l) for l in lines if not l.lstrip().startswith("#"))
        installs = [i for i, l in enumerate(lines) if INSTALL.match(l)]
        if uses_cargo and not installs:
            problems.append(f"{name}:{job}: runs cargo without installing the pinned toolchain")
        for i in installs:
            if not any("actions/checkout" in l for l in lines[:i]):
                problems.append(f"{name}:{job}: toolchain install precedes checkout")
    return problems


def check_all(texts):
    problems = []
    for name, text in texts.items():
        problems += check_workflow(name, text)
    return problems


def workflow_texts():
    return {p.name: p.read_text() for p in WORKFLOWS}


class RealRepo(unittest.TestCase):
    def test_pin_file_exists_and_is_exact(self):
        self.assertTrue(TOOLCHAIN.is_file(), "rust-toolchain.toml missing")
        self.assertEqual(check_pin(TOOLCHAIN.read_text()), [])

    def test_every_workflow_install_uses_the_pin(self):
        self.assertEqual(check_all(workflow_texts()), [])

    def test_some_workflows_actually_install_through_the_script(self):
        n = sum(len(re.findall(r"scripts/ci-rust-toolchain", t)) for t in workflow_texts().values())
        self.assertGreaterEqual(n, 10)

    def test_no_stable_literal_in_a_toolchain_install(self):
        for name, text in workflow_texts().items():
            for line in text.splitlines():
                if "toolchain" in line and not line.lstrip().startswith("#"):
                    self.assertNotRegex(line, r"\bstable\b", f"{name}: {line}")

    def test_scripts_do_not_hard_code_stable(self):
        for p in sorted((ROOT / "scripts").iterdir()):
            if not p.is_file():
                continue
            try:
                text = p.read_text()
            except UnicodeDecodeError:
                continue
            for line in text.splitlines():
                self.assertNotRegex(line, r"\+stable\b|toolchain install stable|--default-toolchain", f"{p.name}: {line}")

    def test_ci_runs_this_contract(self):
        self.assertIn("python3 tests/scripts/test_ci_toolchain_pin.py",
                      (ROOT / ".github/workflows/ci.yml").read_text())


class Mutations(unittest.TestCase):
    """Each re-break of the pin must be caught."""

    def setUp(self):
        self.texts = workflow_texts()

    def mutate(self, name, old, new):
        text = self.texts[name]
        self.assertIn(old, text, f"fixture drift: {old!r} not in {name}")
        out = dict(self.texts)
        out[name] = text.replace(old, new, 1)
        return check_all(out)

    def test_reinserting_stable_is_caught(self):
        got = self.mutate("ci.yml", "scripts/ci-rust-toolchain --profile minimal --component clippy",
                          "rustup toolchain install stable --profile minimal --component clippy")
        self.assertTrue(any("direct rustup" in p for p in got), got)

    def test_literal_version_install_is_caught(self):
        got = self.mutate("staging.yml", "control/scripts/ci-rust-toolchain --export --profile minimal",
                          "rustup toolchain install 1.98.1 --profile minimal")
        self.assertTrue(got)

    def test_hard_coded_env_is_caught(self):
        got = self.mutate("e2e.yml", "  RUSTFLAGS: -D warnings\n", "  RUSTFLAGS: -D warnings\n  RUSTUP_TOOLCHAIN: 1.98.1\n")
        self.assertTrue(any("RUSTUP_TOOLCHAIN" in p for p in got), got)

    def test_toolchain_action_is_caught(self):
        got = self.mutate("mutation.yml", "scripts/ci-rust-toolchain --profile minimal",
                          "uses: dtolnay/rust-toolchain@stable")
        self.assertTrue(got)

    def test_dropping_the_install_step_is_caught(self):
        got = self.mutate("stress.yml", "      - run: scripts/ci-rust-toolchain --profile minimal\n", "")
        self.assertTrue(any("without installing" in p for p in got), got)

    def test_pin_file_set_to_stable_is_caught(self):
        for bad in ('channel = "stable"', 'channel = "1.99"', 'channel = "nightly"'):
            text = re.sub(r'channel = "[^"]*"', bad, TOOLCHAIN.read_text())
            self.assertTrue(check_pin(text), bad)

    def test_missing_components_are_caught(self):
        text = re.sub(r"components = .*", "components = []", TOOLCHAIN.read_text())
        self.assertTrue(check_pin(text))


class InstallScript(unittest.TestCase):
    """scripts/ci-rust-toolchain installs exactly the channel in the file."""

    def run_script(self, toml, *args, env_extra=None):
        with tempfile.TemporaryDirectory(prefix="c927") as d:
            d = Path(d)
            (d / "scripts").mkdir()
            script = d / "scripts/ci-rust-toolchain"
            script.write_text(SCRIPT.read_text())
            script.chmod(script.stat().st_mode | stat.S_IXUSR)
            (d / "rust-toolchain.toml").write_text(toml)
            bin_ = d / "bin"
            bin_.mkdir()
            stub = bin_ / "rustup"
            stub.write_text('#!/bin/sh\necho "$@" > "$STUB_OUT"\n')
            stub.chmod(0o755)
            out = d / "argv"
            ghenv = d / "ghenv"
            env = {"PATH": f"{bin_}:/usr/bin:/bin", "STUB_OUT": str(out), "GITHUB_ENV": str(ghenv)}
            r = subprocess.run([str(script), *args], env=env, capture_output=True, text=True)
            return (r, out.read_text() if out.exists() else None,
                    ghenv.read_text() if ghenv.exists() else None)

    def test_installs_the_file_channel_with_given_args(self):
        r, argv, _ = self.run_script(TOOLCHAIN.read_text(), "--profile", "minimal", "--component", "clippy")
        channel = tomllib.loads(TOOLCHAIN.read_text())["toolchain"]["channel"]
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(argv.strip(), f"toolchain install {channel} --profile minimal --component clippy")

    def test_export_writes_the_channel_to_github_env(self):
        r, _, ghenv = self.run_script(TOOLCHAIN.read_text(), "--export", "--profile", "minimal")
        channel = tomllib.loads(TOOLCHAIN.read_text())["toolchain"]["channel"]
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(ghenv.strip(), f"RUSTUP_TOOLCHAIN={channel}")

    def test_floating_channel_in_the_file_is_refused_without_installing(self):
        for bad in ("stable", "1.99", ""):
            toml = f'[toolchain]\nchannel = "{bad}"\n'
            r, argv, _ = self.run_script(toml, "--profile", "minimal")
            self.assertNotEqual(r.returncode, 0, bad)
            self.assertIsNone(argv, f"rustup ran for {bad!r}")


if __name__ == "__main__":
    unittest.main()
