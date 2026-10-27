#!/usr/bin/env python3
"""Independent CAD-1383 shell acceptance: executable probes, real flock, no Rust.

Expected outcomes come from CAD-1383, not the candidate implementation. The
copied runners retain their real validation, checksum, re-entry and acquisition
paths. Only Cargo/nextest/install effects are controlled executable fixtures.
"""
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
VERSION = "0.9.150"
PROBE = r'''#!/usr/bin/python3
import json, os, pathlib, sys, time
if sys.argv[1:] == ["--version"]:
    print("cargo-nextest 0.9.150")
    sys.exit(0)
root = pathlib.Path(os.environ["PROBE_ROOT"])
def record(kind):
    with (root / "events").open("a") as log:
        log.write(json.dumps({"kind": kind, "args": sys.argv[1:],
                              "env": dict(os.environ)}) + "\n")
record("start")
busy = root / "busy"
try:
    fd = os.open(busy, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
except FileExistsError:
    record("overlap")
    sys.exit(98)
os.close(fd)
try:
    print("probe-started", flush=True)
    if os.environ.get("PROBE_GATE"):
        deadline = time.monotonic() + 5
        while not (root / "release").exists():
            if time.monotonic() > deadline:
                sys.exit(97)
            time.sleep(0.01)
    if os.environ.get("PROBE_JUNIT"):
        report = pathlib.Path.cwd() / "target/nextest/cadence/junit.xml"
        report.parent.mkdir(parents=True, exist_ok=True)
        report.write_text('<testsuites>' + ''.join(
            '<testsuite name="' + name + '"><testcase name="probe"/></testsuite>'
            for name in ["fixture::floor", "fixture", "fixture::bin/fixture"]
        ) + '</testsuites>')
    record("end")
finally:
    busy.unlink()
sys.exit(int(os.environ.get("PROBE_EXIT", "0")))
'''
CARGO = r'''#!/usr/bin/python3
import json, os, pathlib, sys
with (pathlib.Path(os.environ["PROBE_ROOT"]) / "tools").open("a") as log:
    log.write(json.dumps({"tool": "cargo", "args": sys.argv[1:],
                          "env": dict(os.environ)}) + "\n")
if sys.argv[1:] == ["metadata", "--no-deps", "--format-version", "1", "--locked"]:
    print(json.dumps({"packages": [{"targets": [{"name": "floor", "kind": ["test"]}]}]}))
else:
    sys.exit(99)
'''


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Fixture:
    def __init__(self, directory):
        self.root = directory
        self.workspace = directory / "workspace"
        self.scripts = self.workspace / "scripts"
        self.scripts.mkdir(parents=True)
        config = self.workspace / ".config"
        config.mkdir()
        for name in ("cadence-nextest", "run-result-tests", "suite-lock-config",
                     "result-test-args", "check-test-junit.py"):
            source = ROOT / "scripts" / name
            shutil.copy2(source, self.scripts / name)
            print(f"source sha256 {name}: {hashlib.sha256(source.read_bytes()).hexdigest()}")
        self.bin = directory / "toolchain"
        self.bin.mkdir()
        data = directory / "data"
        self.nextest = data / "cadence/tools" / f"cadence-nextest-{VERSION}" / "cargo-nextest"
        self.nextest.parent.mkdir(parents=True)
        self.executable(self.nextest, PROBE)
        self.executable(self.bin / "cargo", CARGO)
        self.executable(self.scripts / "install-cadence-nextest",
                        '#!/bin/sh\nprintf "installer\\n" >> "$PROBE_ROOT/tools"\nexit 99\n')
        checksum = hashlib.sha256(self.nextest.read_bytes()).hexdigest()
        (config / "cargo-nextest.sha256").write_text(f"{checksum}  cargo-nextest\n")
        (config / "nextest.toml").write_text('[profile.cadence]\nretries = 0\nfail-fast = false\n')
        (self.workspace / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n')
        self.lock = directory / "shared lock with spaces"
        self.env = {"PATH": f"{self.bin}:/usr/bin:/bin", "HOME": str(directory / "home"),
                    "XDG_DATA_HOME": str(data), "TMPDIR": str(directory),
                    "RUNNER_TEMP": str(directory), "CARGO_HOME": str(directory / "cargo-home"),
                    "RUSTUP_HOME": str(directory / "rustup-home"), "PROBE_ROOT": str(directory),
                    "CADENCE_NEXTTEST_BIN": str(self.nextest), "CADENCE_SUITE_LOCK": str(self.lock),
                    "CADENCE_SUITE_LOCK_WAIT_SECS": "1", "CADENCE_PM_DIR": str(directory / "fake-pm"),
                    "CADENCE_STATE_DIR": str(directory / "fake-state"), "CADENCE_UNRELATED_ROUTING": "scrub-me"}
        Path(self.env["HOME"]).mkdir()

    @staticmethod
    def executable(path, content):
        path.write_text(content)
        path.chmod(0o755)

    def environment(self, changes):
        env = dict(self.env)
        for key, value in changes.items():
            if value is None:
                env.pop(key, None)
            else:
                env[key] = value
        return env

    def run(self, script="cadence-nextest", args=(), **changes):
        return subprocess.run([str(self.scripts / script), *args], cwd=self.workspace,
                              env=self.environment(changes), capture_output=True,
                              text=True, timeout=8)

    def records(self, name="events"):
        path = self.root / name
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def clear(self):
        for name in ("events", "tools", "release"):
            (self.root / name).unlink(missing_ok=True)

    @contextmanager
    def held(self):
        # Real util-linux flock owns the descriptor, not a stand-in executable.
        process = subprocess.Popen(["flock", "-o", str(self.lock), sys.executable, "-c",
                                    'import sys; print("locked", flush=True); sys.stdin.read()'],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
                                   env={"PATH": "/usr/bin:/bin"})
        try:
            require(process.stdout.readline() == "locked\n", "real flock holder failed")
            yield
        finally:
            process.stdin.close()
            process.wait(timeout=3)
            process.stdout.close()


def assert_scrubbed(records):
    require(records, "probe did not execute")
    for record in records:
        leaked = set(record["env"]) & {"CADENCE_SUITE_LOCK_WAIT_SECS", "CADENCE_PM_DIR",
                                        "CADENCE_STATE_DIR", "CADENCE_UNRELATED_ROUTING"}
        require(not leaked, f"routing/wait reached executable: {sorted(leaked)}")


def acceptance(f):
    invalid = [({"CADENCE_SUITE_LOCK": None}, "CADENCE_SUITE_LOCK"),
               ({"CADENCE_SUITE_LOCK": ""}, "CADENCE_SUITE_LOCK"),
               ({"CADENCE_SUITE_LOCK": "relative.lock"}, "CADENCE_SUITE_LOCK")]
    for wait in ("", "-1", "+1", "1.5", " 1", "1 ", "abc", "86401", "999999999999999999999999"):
        invalid.append(({"CADENCE_SUITE_LOCK_WAIT_SECS": wait}, "CADENCE_SUITE_LOCK_WAIT_SECS"))
    for script in ("cadence-nextest", "run-result-tests"):
        for changes, variable in invalid:
            f.clear()
            result = f.run(script, **changes)
            require(result.returncode != 0, f"{script} accepted invalid {changes}")
            require(variable in result.stderr, f"{script} refusal omitted {variable}: {result.stderr}")
            require(not f.records(), f"{script} launched a suite on invalid config")
            require(not (f.root / "tools").exists(), f"{script} ran metadata/install before refusal")
    print("PASS missing/relative lock and malformed/out-of-range wait refuse before effects")

    with f.held():
        for wait in ("1", "0"):
            f.clear()
            started = time.monotonic()
            result = f.run(CADENCE_SUITE_LOCK_WAIT_SECS=wait)
            elapsed = time.monotonic() - started
            require(result.returncode == 75, f"contention wait{wait} returned {result.returncode}: {result.stderr}")
            require(elapsed < 4, f"wait{wait} contention was not bounded: {elapsed:.3f}s")
            if wait == "1":
                require(elapsed >= 0.8, f"wait1 did not wait: {elapsed:.3f}s")
            else:
                require(elapsed < 0.8, f"wait0 was not immediate: {elapsed:.3f}s")
            require(not f.records(), "contention launched a probe")
            require(str(f.lock) in result.stderr and wait in result.stderr,
                    f"contention diagnostics omitted selected path/wait: {result.stderr}")
            print(f"PASS real flock contention wait{wait}: exit75, {elapsed:.3f}s, no probe")
        f.clear()
        started = time.monotonic()
        result = f.run("run-result-tests", CADENCE_SUITE_LOCK_WAIT_SECS="1")
        elapsed = time.monotonic() - started
        require(result.returncode != 0 and 0.8 <= elapsed < 4,
                f"result runner did not preserve wait1 on contention: exit{result.returncode}, {elapsed:.3f}s")
        require(not f.records(), "result runner launched a probe despite real contention")
        assert_scrubbed(f.records("tools"))
        print(f"PASS result runner forwards captured wait1: exit{result.returncode}, {elapsed:.3f}s, no probe")
        f.clear()
        result = f.run(args=("list",), CADENCE_SUITE_LOCK=None, CADENCE_SUITE_LOCK_WAIT_SECS="bad")
        require(result.returncode == 0, f"list attempted locking/config validation: {result.stderr}")
        require(f.records()[0]["args"][:2] == ["nextest", "list"], "list lost its subcommand")
        assert_scrubbed(f.records())
        f.clear()
        result = f.run(CADENCE_SUITE_LOCK="", CADENCE_REVIEW_SUITE_LOCK_HELD="1",
                       CADENCE_REVIEW_PR="1383", CADENCE_REVIEW_HEAD="a" * 40)
        require(result.returncode == 0, f"held review child re-locked: {result.stderr}")
        assert_scrubbed(f.records())
    print("PASS list lock-free and held-review child avoids nested real flock")

    held = {"CADENCE_SUITE_LOCK": "", "CADENCE_REVIEW_SUITE_LOCK_HELD": "1",
            "CADENCE_REVIEW_PR": "1383", "CADENCE_REVIEW_HEAD": "a" * 40}
    for changes in ({"CADENCE_REVIEW_PR": None}, {"CADENCE_REVIEW_HEAD": None},
                    {"CADENCE_SUITE_LOCK": str(f.lock)}):
        f.clear()
        result = f.run(**(held | changes))
        require(result.returncode != 0 and not f.records(), "held review context validation bypassed")
    print("PASS held-review still refuses missing context or nonempty lock")

    for args in ((), ("archive",)):
        for wait in (None, "0", "86400"):
            f.clear()
            result = f.run(args=args, CADENCE_SUITE_LOCK_WAIT_SECS=wait)
            require(result.returncode == 0, f"valid explicit path/wait failed: {result.stderr}")
            expected = "archive" if args else "run"
            require(f.records()[0]["args"][:2] == ["nextest", expected], "re-entry lost subcommand")
            selected_wait = "3600" if wait is None else wait
            require(str(f.lock) in result.stderr and selected_wait in result.stderr,
                    f"selected path/default/bound not reported: {result.stderr}")
            assert_scrubbed(f.records())
    print("PASS spaced absolute path, default3600, bounds0/86400 and archive re-entry")

    for code in (0, 7, 75):
        f.clear()
        result = f.run(PROBE_EXIT=str(code))
        require(result.returncode == code, f"child status{code} changed to {result.returncode}")
        require(len(f.records()) == 2, f"child status{code} did not execute exactly once")
        require("timed out" not in result.stderr.lower() and "timeout" not in result.stderr.lower(),
                f"child status{code} falsely labeled lock timeout: {result.stderr}")
    print("PASS child exit0/7/75 passes through without false timeout")

    f.clear()
    command = [str(f.scripts / "cadence-nextest")]
    first = subprocess.Popen(command, cwd=f.workspace,
                             env=f.environment({"PROBE_GATE": "1", "CADENCE_SUITE_LOCK_WAIT_SECS": "3"}),
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    second = None
    try:
        require(first.stdout.readline() == "probe-started\n", "first coordinated caller did not start")
        second = subprocess.Popen(command, cwd=f.workspace,
                                  env=f.environment({"CADENCE_SUITE_LOCK_WAIT_SECS": "3"}),
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        # Keep the first caller in its executable while the second enters the wrapper.
        time.sleep(0.2)
        require(second.poll() is None, "second caller exited before lock release")
        require([r["kind"] for r in f.records()] == ["start"], "second caller overlapped first")
        (f.root / "release").touch()
        first.communicate(timeout=8)
        second.communicate(timeout=8)
        require(first.returncode == second.returncode == 0, "coordinated caller failed")
        require([r["kind"] for r in f.records()] == ["start", "end", "start", "end"],
                "shared-lock callers overlapped or were not both executed")
    finally:
        (f.root / "release").touch()
        for process in (first, second):
            if process is not None and process.poll() is None:
                process.terminate()
                process.communicate(timeout=3)
    print("PASS two real shared-lock callers serialize without overlap")

    f.clear()
    result = f.run("run-result-tests", PROBE_JUNIT="1")
    require(result.returncode == 0, f"real result runner fixture failed: {result.stdout}\n{result.stderr}")
    require(len(f.records("tools")) == 1, "unexpected tool install/compile")
    assert_scrubbed(f.records("tools"))
    assert_scrubbed(f.records())
    require("validated 1 integration targets" in result.stdout, "result JUnit coverage guard not reached")
    print("PASS real result runner: Cargo metadata and child scrub wait/routing; JUnit guard retained")


def main():
    require(shutil.which("flock", path="/usr/bin:/bin"), "real util-linux flock is required")
    print(f"driver sha256: {hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}")
    with tempfile.TemporaryDirectory(prefix="c1383-", dir="/tmp") as temp:
        acceptance(Fixture(Path(temp)))
    print("CAD-1383 independent fake-executable/real-flock acceptance passed (no Rust compiled)")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, subprocess.TimeoutExpired, OSError) as error:
        print(f"CAD-1383 acceptance FAILED: {error}", file=sys.stderr)
        sys.exit(1)
