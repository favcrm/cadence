#!/usr/bin/env python3
"""CAD-1334 behavior acceptance for the real CI Rust test-runner boundary."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts" / "run-result-tests"
RESULT_ARGS = ROOT / "scripts" / "result-test-args"
INSTALLER = ROOT / "scripts" / "install-cadence-nextest"
PINNED_VERSION = "0.9.145"


def fail(message: str) -> "NoReturn":
    print(f"test-cad-1334-nextest-contract: {message}", file=sys.stderr)
    raise SystemExit(1)


def integration_targets() -> list[str]:
    output = subprocess.check_output([str(RESULT_ARGS)], cwd=ROOT, text=True)
    args = output.splitlines()
    if not args or len(args) % 2:
        fail("result-test-args returned an empty or malformed integration selection")
    names: list[str] = []
    for flag, name in zip(args[::2], args[1::2]):
        if flag != "--test" or not name:
            fail(f"unexpected result-test-args entry: {flag!r} {name!r}")
        names.append(name)
    if len(names) != len(set(names)):
        fail("result-test-args returned duplicate integration target names")
    return names


def find_nextest(temp: Path) -> Path:
    candidates: list[Path] = []
    configured = os.environ.get("CADENCE_NEXTTEST_TEST_BIN")
    if configured:
        candidates.append(Path(configured).expanduser())
    home = Path(os.environ.get("HOME", "")).expanduser()
    xdg_data = Path(os.environ.get("XDG_DATA_HOME", home / ".local" / "share"))
    candidates.append(
        xdg_data / "cadence" / "tools" / f"cadence-nextest-{PINNED_VERSION}" / "cargo-nextest"
    )
    found = shutil.which("cargo-nextest")
    if found:
        candidates.append(Path(found))
    for candidate in candidates:
        if candidate.is_file() and os.access(candidate, os.X_OK) and candidate.name == "cargo-nextest":
            return candidate.resolve()

    # Install with the repository's trusted checksum verifier. Both the archive
    # and executable stay beneath this acceptance run's temporary directory.
    tools = temp / "nextest-tools"
    install_env = os.environ.copy()
    install_env.update(TMPDIR=str(temp), XDG_DATA_HOME=str(temp / "install-xdg"),
                       CADENCE_NEXTTEST_INSTALL_DIR=str(tools))
    result = subprocess.run([str(INSTALLER)], cwd=ROOT, env=install_env,
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    binary = tools / "cargo-nextest"
    if result.returncode != 0 or not binary.is_file():
        fail(f"could not install trusted cargo-nextest {PINNED_VERSION}:\n{result.stdout}")
    return binary.resolve()


def rust_test_body(marker: str, category: str) -> str:
    return f'''#[test]
fn cadence_1334_runner_environment_lock_and_execution() {{
    use std::path::Path;
    use std::process::Command;

    let root = std::env::var("CAD1334_EXPECT_ROOT").unwrap();
    let original_home = std::env::var("CAD1334_ORIGINAL_HOME").unwrap();
    let home = std::env::var("HOME").unwrap();
    assert_ne!(home, original_home, "runner must isolate HOME");
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMPDIR"] {{
        let value = std::env::var(key).unwrap_or_else(|_| panic!("runner omitted {{key}}"));
        assert!(Path::new(&value).starts_with(&root), "{{key}} was not isolated: {{value}}");
    }}
    for key in ["CADENCE_PM_DIR", "CADENCE_STATE_DIR", "CADENCE_HOME"] {{
        assert!(std::env::var_os(key).is_none(), "{{key}} reached a test process");
    }}

    let lock = std::env::var("CAD1334_LOCK_PATH").unwrap();
    assert!(Path::new(&lock).exists(), "suite lock file was not created");
    let probe = Command::new("flock")
        .args(["-n", &lock, "-c", "true"])
        .status()
        .expect("flock must be available to verify suite lock ownership");
    assert!(!probe.success(), "suite lock was not held while the test ran");

    let marker = std::env::var("CAD1334_MARKER_DIR").unwrap();
    std::fs::write(Path::new(&marker).join("{marker}"), b"executed").unwrap();
    if std::env::var("CAD1334_FAIL_TEST").as_deref() == Ok("{marker}") {{
        panic!("intentional CAD-1334 failure in {category} target");
    }}
}}
'''


def write_fixture(directory: Path, names: list[str], *, zero_target: str | None = None,
                  lib_test: bool = True, bin_test: bool = True) -> Path:
    directory.mkdir(parents=True)
    (directory / "src").mkdir()
    (directory / "tests").mkdir()
    manifest = directory / "Cargo.toml"
    manifest.write_text(
        '''[package]\nname = "cadence-1334-runner-fixture"\nversion = "0.1.0"\nedition = "2021"\n\n[features]\ntest-seam = []\n\n[lib]\npath = "src/lib.rs"\n\n[[bin]]\nname = "cadence-1334-fixture-bin"\npath = "src/main.rs"\n''', encoding="utf-8")
    (directory / "src" / "lib.rs").write_text(
        "#[cfg(test)]\nmod tests {\n" + (rust_test_body("lib-test", "lib") if lib_test else "") + "}\n",
        encoding="utf-8")
    (directory / "src" / "main.rs").write_text(
        "fn main() {}\n#[cfg(test)]\nmod tests {\n"
        + (rust_test_body("bin-test", "bin") if bin_test else "") + "}\n",
        encoding="utf-8")
    for name in names:
        path = directory / "tests" / f"{name}.rs"
        if name == zero_target:
            path.write_text("// Deliberately selected integration target with zero tests.\n", encoding="utf-8")
        else:
            path.write_text(rust_test_body(f"integration-{name}", f"integration target {name}"),
                            encoding="utf-8")
        with manifest.open("a", encoding="utf-8") as cargo_toml:
            cargo_toml.write(f'\n[[test]]\nname = "{name}"\npath = "tests/{name}.rs"\n')
    lock_env = os.environ.copy()
    lock_env["TMPDIR"] = str(directory.parent)
    result = subprocess.run(
        ["cargo", "generate-lockfile", "--offline", "--manifest-path", str(manifest)],
        cwd=directory, env=lock_env, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, check=False)
    if result.returncode:
        fail(f"could not generate offline fixture Cargo.lock:\n{result.stdout}")
    return manifest


def invoke(manifest: Path, temp: Path, nextest: Path, *, fail_test: str | None = None
           ) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.pop("CADENCE_NEXTTEST_BIN", None)
    env["PATH"] = f"{nextest.parent}{os.pathsep}{env.get('PATH', '')}"
    env.update(TMPDIR=str(temp), RUNNER_TEMP=str(temp),
               CADENCE_SUITE_LOCK=str(temp / "suite.lock"),
               CADENCE_PM_DIR=str(temp / "must-be-scrubbed-pm"),
               CADENCE_STATE_DIR=str(temp / "must-be-scrubbed-state"),
               CADENCE_HOME=str(temp / "must-be-scrubbed-home"),
               CAD1334_EXPECT_ROOT=str(temp),
               CAD1334_ORIGINAL_HOME=os.environ.get("HOME", ""),
               CAD1334_LOCK_PATH=str(temp / "suite.lock"),
               CAD1334_MARKER_DIR=str(temp / "markers"),
               CARGO_TARGET_DIR=str(temp / "cargo-target"))
    if fail_test:
        env["CAD1334_FAIL_TEST"] = fail_test
    else:
        env.pop("CAD1334_FAIL_TEST", None)
    return subprocess.run([str(RUNNER), "--manifest-path", str(manifest)], cwd=ROOT,
                          env=env, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, check=False)


def expect_refusal(result: subprocess.CompletedProcess[str], category: str, detail: str) -> None:
    if result.returncode == 0:
        fail(f"runner accepted empty/invalid {category} selection")
    output = result.stdout.lower()
    if category.lower() not in output or detail.lower() not in output:
        fail(f"{category} refusal did not name its category and {detail!r}:\n{result.stdout}")


def main() -> None:
    if not RUNNER.is_file() or not os.access(RUNNER, os.X_OK):
        fail("scripts/run-result-tests is missing or not executable")
    if not INSTALLER.is_file() or not os.access(INSTALLER, os.X_OK):
        fail("scripts/install-cadence-nextest is missing or not executable")
    names = integration_targets()
    if not names:
        fail("exact integration target inventory is empty")

    with tempfile.TemporaryDirectory(prefix="cadence-cad-1334-") as temp_name:
        temp = Path(temp_name).resolve()
        nextest = find_nextest(temp)
        (temp / "markers").mkdir()
        manifest = write_fixture(temp / "fixture", names)
        expected = {f"integration-{name}" for name in names} | {"lib-test", "bin-test"}

        positive = invoke(manifest, temp, nextest)
        if positive.returncode != 0:
            fail("complete fixture failed through CI runner:\n" + positive.stdout)
        observed = {path.name for path in (temp / "markers").iterdir()}
        if observed != expected:
            fail(f"positive run did not execute exact integration/lib/bin coverage; missing={expected - observed}, extra={observed - expected}")

        # A test failure must fail the job without fail-fast suppressing other selections.
        failed_target = names[0]
        for marker in expected:
            (temp / "markers" / marker).unlink()
        failing = invoke(manifest, temp, nextest, fail_test=f"integration-{failed_target}")
        if failing.returncode == 0:
            fail(f"runner accepted failing integration target {failed_target!r}")
        if failed_target not in failing.stdout:
            fail(f"failure output did not name integration target {failed_target!r}:\n{failing.stdout}")
        observed = {path.name for path in (temp / "markers").iterdir()}
        if observed != expected:
            fail(f"failure stopped unrelated selected tests; missing={expected - observed}, extra={observed - expected}")

        missing_dir = temp / "missing-fixture"
        missing_manifest = write_fixture(missing_dir, names)
        (missing_dir / "tests" / f"{names[0]}.rs").unlink()
        missing = invoke(missing_manifest, temp, nextest)
        expect_refusal(missing, "integration target", names[0])

        zero_manifest = write_fixture(temp / "zero-integration-fixture", names, zero_target=names[0])
        zero = invoke(zero_manifest, temp, nextest)
        expect_refusal(zero, "integration target", names[0])

        no_lib_manifest = write_fixture(temp / "zero-lib-fixture", names, lib_test=False)
        no_lib = invoke(no_lib_manifest, temp, nextest)
        expect_refusal(no_lib, "lib selection", "lib")

        no_bins_manifest = write_fixture(temp / "zero-bins-fixture", names, bin_test=False)
        no_bins = invoke(no_bins_manifest, temp, nextest)
        expect_refusal(no_bins, "bins selection", "bin")

    print("CAD-1334 nextest runner acceptance passed")


if __name__ == "__main__":
    main()
