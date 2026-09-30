#!/usr/bin/env python3
"""Exercise test-suite sharding: partition args and phase gating.

The partition travels as an explicit CLI argument, never ambient env:
exact-command contract tests broke twice on env leaking across steps.
"""
import hashlib
import importlib.machinery
import importlib.util
import json
import subprocess
import sys
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


runner = load("runner", "ci-rust-tests.py")

FULL = {"schema": 1, "mode": "full", "targets": [], "reason": "t"}


class StandaloneInventoryCLI(unittest.TestCase):
    def test_copied_inventory_runner_does_not_require_archive_sibling(self):
        with tempfile.TemporaryDirectory(prefix='standalone-inventory.') as temporary:
            root = Path(temporary)
            copied = root / 'ci-rust-tests.py'
            copied.write_bytes((ROOT / 'scripts/ci-rust-tests.py').read_bytes())
            scripts = root / 'scripts'
            scripts.mkdir()
            inventory = scripts / 'nextest-inventory'
            inventory.write_text('#!/bin/sh\n[ "$*" = "all-targets --features test-seam" ]\n')
            inventory.chmod(0o755)
            plan = root / 'plan.json'
            plan.write_text(json.dumps(FULL))
            result = subprocess.run([sys.executable, str(copied), '--root', str(root), '--plan', str(plan),
                                     '--phase', 'inventory'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)


class PartitionArgs(unittest.TestCase):
    def test_empty_means_whole_suite(self):
        self.assertIsNone(runner.partition_args(""))
        self.assertIsNone(runner.partition_args("   "))

    def test_valid_shard(self):
        self.assertEqual(runner.partition_args("2/8"), (2, 8))
        self.assertEqual(runner.partition_args(" 1/4 "), (1, 4))

    def test_invalid_shapes_refused(self):
        for raw in ("0/8", "9/8", "2/0", "a/b", "1-8", "1/2/3", "--partition", "hash:1/8"):
            with self.assertRaises(ValueError, msg=raw):
                runner.partition_args(raw)


class PhaseGating(unittest.TestCase):
    def run_phase(self, phase, partition=""):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.object(runner.subprocess, "run") as run:
                runner.run(root, FULL, phase, partition)
        return run.call_args.args[0]

    def test_tests_phase_partitions_via_filterset(self):
        # A partitioned run lists the scope first (the hash partition is
        # gone — see PartitionedRun for the full contract).
        with tempfile.TemporaryDirectory() as tmp:
            calls = []

            def fake(cmd, **kw):
                calls.append(cmd)
                doc = LIST_DOC
                if "-E" in cmd:
                    names = set(re.findall(r'test\(=([A-Za-z0-9_:\-./]+)\)', cmd[cmd.index("-E") + 1]))
                    doc = {"rust-suites": {b: {"testcases": {
                        n: c for n, c in s["testcases"].items() if n in names}}
                        for b, s in doc["rust-suites"].items()}}

                class Out:
                    stdout = json.dumps(doc)

                return Out()

            with patch.object(runner.subprocess, "run", side_effect=fake):
                runner.run(Path(tmp), FULL, "tests", "1/2")
        self.assertEqual(calls[0][1:2], ["list"])
        self.assertIn("-E", calls[-1])

    def test_tests_phase_without_partition(self):
        command = self.run_phase("tests")
        self.assertNotIn("--partition", command)

    def test_inventory_rejects_partition(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(runner.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    runner.run(Path(tmp), FULL, "inventory", "3/8")
        run.assert_not_called()

    def test_bad_partition_fails_before_running(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(runner.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    runner.run(Path(tmp), FULL, "tests", "bogus")
        run.assert_not_called()




class Assign(unittest.TestCase):
    TESTS = [("b", f"t{i}") for i in range(6)] + [("a", "heavy")]

    def test_deterministic_regardless_of_order(self):
        weights = {"a heavy": 50.0}
        forward = runner.assign(self.TESTS, weights, 4)
        shuffled = runner.assign(list(reversed(self.TESTS)), weights, 4)
        self.assertEqual(forward, shuffled)

    def test_complete_and_disjoint(self):
        shards = runner.assign(self.TESTS, {}, 3)
        flat = [t for s in shards for t in s]
        self.assertEqual(sorted(flat), sorted(f"{b} {n}" for b, n in self.TESTS))
        self.assertEqual(len(flat), len(set(flat)))

    def test_balances_around_optimum(self):
        tests = [("b", "heavy")] + [("b", f"t{i}") for i in range(20)]
        weights = {"b heavy": 50.0, **{f"b t{i}": 1.0 for i in range(20)}}
        shards = runner.assign(tests, weights, 4)
        # Optimum is 70/4 = 17.5s but the 50s test can't split: the best
        # possible max load is 50s. LPT must stay under 1.5x that.
        loads = [sum(weights.get(t, 0.1) for t in s) for s in shards]
        self.assertLessEqual(max(loads), 50 * 1.5)
        others = [l for i, l in enumerate(loads) if l < 50]
        self.assertTrue(all(l >= 3.0 for l in others), loads)

    def test_unknown_tests_get_default_weight(self):
        shards = runner.assign([("b", "u1"), ("b", "u2")], {}, 2)
        self.assertEqual([len(s) for s in shards], [1, 1])

    def test_more_shards_than_tests_leaves_empties(self):
        shards = runner.assign([("b", "only")], {}, 8)
        self.assertEqual(len([s for s in shards if s]), 1)

    def test_bad_weight_refused_directly(self):
        for bad in [-1.0, float("nan"), float("inf")]:
            with self.assertRaises(ValueError, msg=bad):
                runner.assign([("b", "t")], {"b t": bad}, 1)


class Filterset(unittest.TestCase):
    def test_ids_with_special_characters(self):
        expr = runner.filterset(
            ["cadence-agent::pty_interact slow", "my-bin/x::y a::b::c"]
        )
        self.assertIn('binary_id(=cadence-agent::pty_interact)', expr)
        self.assertIn('test(=slow)', expr)
        self.assertIn('binary_id(=my-bin/x::y)', expr)
        self.assertIn('test(=a::b::c)', expr)
        self.assertEqual(expr.count(" | "), 1)

    def test_bare_charset_violation_refused(self):
        for bad in ['b "quoted" n', 'b n"ame', "b a'b"]:
            with self.assertRaises(ValueError, msg=bad):
                runner.filterset([bad])


class ScopeArgsSchema(unittest.TestCase):
    def test_duplicate_targets_refused(self):
        for targets in [["a", "a"], ["a", "b", "a"]]:
            with self.assertRaises(ValueError, msg=targets):
                runner.scope_args({"schema": 1, "mode": "selected",
                                   "targets": targets, "reason": "t"})

    def test_schema_must_be_integer_one(self):
        for bad_schema in (True, "1", 1.5):
            with self.assertRaises(ValueError, msg=repr(bad_schema)):
                runner.scope_args({"schema": bad_schema, "mode": "full",
                                   "targets": [], "reason": "t"})


class LoadWeights(unittest.TestCase):
    def write(self, tmp, doc):
        path = Path(tmp) / "w.json"
        path.write_text(json.dumps(doc) if not isinstance(doc, str) else doc)
        return path

    def test_valid(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = self.write(tmp, {"schema": 1, "weights": {"a t": 5.0}})
            self.assertEqual(runner.load_weights(path), {"a t": 5.0})

    def test_bad_schema(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError):
                runner.load_weights(self.write(tmp, {"schema": 2, "weights": {}}))

    def test_key_must_have_exactly_one_space(self):
        for key in ["nospace", "a  b", "a b c"]:
            with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError, msg=key):
                runner.load_weights(self.write(tmp, {"schema": 1, "weights": {key: 1.0}}))

    def test_non_numeric_weight(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError):
                runner.load_weights(self.write(tmp, {"schema": 1, "weights": {"a t": "x"}}))
            with self.assertRaises(ValueError):
                runner.load_weights(self.write(tmp, {"schema": 1, "weights": {"a t": True}}))

    def test_non_finite_or_negative_weight(self):
        # json.loads accepts NaN/Infinity; the gate must not.
        for bad in [-1, float("nan"), float("inf"), float("-inf")]:
            with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError, msg=bad):
                runner.load_weights(self.write(tmp, {"schema": 1, "weights": {"a t": bad}}))

    def test_zero_and_positive_weights_accepted(self):
        with tempfile.TemporaryDirectory() as tmp:
            w = runner.load_weights(self.write(tmp, {"schema": 1, "weights": {"a t": 0, "b u": 3.5}}))
            self.assertEqual(w, {"a t": 0.0, "b u": 3.5})


def case(ignored=False, status="matches"):
    return {"kind": "test", "ignored": ignored,
            "filter-match": {"status": status}}


LIST_DOC = {
    "rust-suites": {
        "b::one": {
            "testcases": {
                "t1": case(),
                "skip_me": case(ignored=True),
            }
        },
        "b::two": {"testcases": {"t2": case(), "filtered": case(status="mismatch")}},
    }
}


class PartitionedRun(unittest.TestCase):
    """End to end with a fake `cadence-nextest` serving list JSON."""

    def run_phase(self, root, partition, weights_doc=None, list_override=None, honor_filter=True):
        calls = []

        def served(cmd):
            doc = list_override if list_override is not None else LIST_DOC
            # Honour -E so the runner's filterset self-check is real:
            # extract the test(="name") clauses and filter the doc.
            if "-E" in cmd and honor_filter:
                names = set(re.findall(r'test\(=([A-Za-z0-9_:\-./]+)\)', cmd[cmd.index("-E") + 1]))
                doc = {"rust-suites": {b: {"testcases": {
                    n: c for n, c in s["testcases"].items() if n in names}}
                    for b, s in doc["rust-suites"].items()}}
            return doc

        def fake(cmd, **kw):
            calls.append(cmd)

            class Out:
                stdout = json.dumps(served(cmd))

            return Out()
        weights = None
        if weights_doc is not None:
            weights = Path(root) / "w.json"
            weights.write_text(json.dumps(weights_doc))
        out = Path(root) / "assignment.json"
        with patch.object(runner.subprocess, "run", side_effect=fake):
            runner.run(Path(root), FULL, "tests", partition,
                       weights=weights, assignment_out=out)
        return calls, out

    def test_list_then_filtered_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            calls, out = self.run_phase(tmp, "1/4", {"schema": 1, "weights": {"b::one t1": 5.0}})
            assignment = json.loads(out.read_text())
        # list (inventory), list -E (self-check), run -E.
        self.assertEqual(len(calls), 3)
        self.assertEqual(calls[0][1:2], ["list"])
        self.assertEqual(calls[1][1:2], ["list"])
        self.assertIn("-E", calls[1])
        self.assertNotIn("--partition", calls[2])
        self.assertEqual(calls[2][-2], "-E")
        self.assertEqual(calls[2][-1], calls[1][-1])  # same filterset
        self.assertEqual(assignment["schema"], 1)
        self.assertEqual(assignment["shard"], 1)
        self.assertEqual(assignment["total"], 4)
        self.assertEqual(sorted(assignment["inventory"]), ["b::one t1", "b::two t2"])
        self.assertEqual(sorted(assignment["tests"]), assignment["tests"])
        self.assertTrue(set(assignment["tests"]) <= set(assignment["inventory"]))
        import hashlib
        self.assertEqual(
            assignment["inventory_sha256"],
            hashlib.sha256("\n".join(assignment["inventory"]).encode()).hexdigest(),
        )

    def test_self_check_mismatch_raises(self):
        doc = {"rust-suites": {"b::one": {"testcases": {
            "t1": case(),
            "intruder": case()}}}}
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            self.run_phase(tmp, "1/4", list_override=doc, honor_filter=False)

    def test_empty_shard_skips_nextest_but_writes_assignment(self):
        with tempfile.TemporaryDirectory() as tmp:
            calls, out = self.run_phase(tmp, "8/8")
            assignment = json.loads(out.read_text())
        self.assertEqual(len(calls), 1)  # only the inventory list
        self.assertEqual(assignment["tests"], [])

    def test_docs_mode_writes_empty_assignment(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "a.json"
            with patch.object(runner.subprocess, "run") as run:
                runner.run(Path(tmp), {"schema": 1, "mode": "docs", "targets": [], "reason": "r"},
                           "tests", "1/8", assignment_out=out)
            run.assert_not_called()
            assignment = json.loads(out.read_text())
            self.assertEqual(assignment["inventory"], [])
            self.assertEqual(assignment["tests"], [])


def load_script(name, filename):
    # Extensionless scripts need an explicit SourceFileLoader.
    loader = importlib.machinery.SourceFileLoader(name, str(ROOT / "scripts" / filename))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


weights_gen = load_script("shard_weights", "shard-weights")


def costs_dir(root, attempt, shard, tests):
    d = Path(root) / f"nextest-costs-42-{attempt}-shard-{shard}"
    d.mkdir(parents=True)
    (d / "nextest-costs.json").write_text(json.dumps({
        "schema": "cadence.nextest-costs/1",
        "slowest": [{"suite": s, "name": n, "status": "passed", "duration_s": dsec}
                    for s, n, dsec in tests],
    }))


class BundleReuse(unittest.TestCase):
    """CAD-858: shards consume the verified producer archive instead of
    compiling. Only verify_bundle/check_archive/subprocess.run are
    patched — read_context and the bundle files stay real, so byte/hash
    comparisons genuinely bind the runner to the producer's outputs."""

    SHA = "a" * 40
    BIN_DIR = "target/nextest"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "ws"
        self.root.mkdir()
        self.dir = self.base / "bundle"
        self.dir.mkdir()
        # Producer-pinned plan and inventory travel inside the bundle.
        (self.dir / "ci-test-plan.json").write_text(json.dumps(FULL, indent=2))
        (self.dir / "inventory.json").write_text(json.dumps({
            "rust-suites": {
                "b::one": {"testcases": {"t1": case(), "skip_me": case(ignored=True)}},
                "b::two": {"testcases": {"t2": case(), "filtered": case(status="mismatch")}},
            },
            "test-count": 4,
        }))
        (self.dir / "nextest.tar.zst").write_bytes(b"synthetic archive")
        self.expected = {
            "schema": 1,
            "source_sha": self.SHA,
            "run_id": "12345",
            "producer_attempt": "1",
            "nextest_version": "0.9.145",
            "features": ["test-seam"],
            "workspace_root": str(self.root),
            "build": {"target_root": str(self.root / "target")},
            "plan_sha256": hashlib.sha256(
                (self.dir / "ci-test-plan.json").read_bytes()).hexdigest(),
            "inventory_sha256": hashlib.sha256(
                (self.dir / "inventory.json").read_bytes()).hexdigest(),
            "archive_sha256": hashlib.sha256(
                (self.dir / "nextest.tar.zst").read_bytes()).hexdigest(),
        }
        self.authority = self.base / "expected.json"
        self.authority.write_text(json.dumps(self.expected))
        self.plan = self.base / "cli-plan.json"
        self.plan.write_bytes((self.dir / "ci-test-plan.json").read_bytes())
        self.calls = []
        self.real_run = subprocess.run

    def served(self, cmd):
        """The extracted list honours -E so the self-check stays real."""
        doc = LIST_DOC
        if "-E" in cmd:
            names = set(re.findall(r'test\(=([A-Za-z0-9_:\-./]+)\)',
                                   cmd[cmd.index("-E") + 1]))
            doc = {"rust-suites": {b: {"testcases": {
                n: c for n, c in s["testcases"].items() if n in names}}
                for b, s in doc["rust-suites"].items()}}
        return doc

    def fake(self, cmd, **kw):
        self.calls.append(cmd)
        if "--extract-to" in cmd and self.stub_version is not None:
            # The extraction list is what materializes target/; the
            # version stub only exists once it has run.
            self.install_version_stub(self.stub_version)
        if cmd[0] == str(self.root / "target/debug/cadence"):
            # The version probe is a real executable stub, not a nextest
            # JSON endpoint.
            return self.real_run(cmd, **kw)

        class Out:
            stdout = json.dumps(self.served(cmd))

        return Out()

    def install_version_stub(self, version=None):
        """Executable `target/debug/cadence` reporting a source-suffixed build."""
        binary = self.root / "target/debug/cadence"
        binary.parent.mkdir(parents=True, exist_ok=True)
        version = version or f"cadence 0.1.2+{self.SHA}"
        binary.write_text(
            "#!/bin/sh\nprintf '%s\\n' " + version + "\n")
        binary.chmod(0o755)

    def reused(self, partition="1/8", plan_path=None, install_stub=True,
               version=None, expected_patch=None, **overrides):
        self.stub_version = (
            (version or f"cadence 0.1.2+{self.SHA}") if install_stub else None
        )
        if expected_patch is not None:
            patched = dict(self.expected, **expected_patch)
            self.authority.write_text(json.dumps(patched))
        kw = dict(partition=partition, assignment_out=self.base / "assignment.json")
        kw.update(overrides)
        bdir = kw.pop("bundle_dir", self.dir)
        epath = kw.pop("expected_path", self.authority)
        with patch.object(runner.bundle, "verify_bundle",
                          return_value=Path(bdir) / "nextest.tar.zst") as verify, \
             patch.object(runner.bundle, "check_archive",
                          # The preflight callable lands with the main
                          # patch; mock/patch only, never implemented here.
                          create=True,
                          return_value=Path(bdir) / "nextest.tar.zst") as check, \
             patch.object(runner.subprocess, "run", side_effect=self.fake):
            runner.run(self.root, plan_path or self.plan,
                       "tests", bundle_dir=bdir,
                       expected_path=epath, **kw)
        return verify, check

    def test_reuse_extracts_verified_archive_then_runs_metadata_only(self):
        verify, check = self.reused()
        verify.assert_called_once_with(self.dir, self.expected)
        check.assert_called_once_with(self.dir / "nextest.tar.zst")
        extract, probe, fetch, selfcheck, run = self.calls
        self.assertEqual(probe,
                         [str(self.root / "target/debug/cadence"), "--version"])
        self.assertEqual(extract[0], str(self.root / "scripts/cadence-nextest"))
        # Extraction list: archive flags only — never Cargo selectors,
        # features, --locked or the reuse metadata flags.
        self.assertEqual(extract[1:3], ["list", "--archive-file"])
        self.assertEqual(extract[3], str(self.dir / "nextest.tar.zst"))
        self.assertEqual(extract[4:6], ["--extract-to", str(self.root)])
        self.assertEqual(extract[6:], ["--message-format", "json"])
        for banned in ("--extract-overwrite", "--locked", "--features",
                       "--all-targets", "--lib", "--bins", "--test",
                       "--cargo-metadata", "--binaries-metadata",
                       "--workspace-remap", "--target-dir-remap", "-E"):
            self.assertNotIn(banned, extract)
        # Self-check list and run: metadata reuse flags, never archive or
        # Cargo invocation flags. The run's only selector is -E, and the
        # run keeps the wrapper's default `run` subcommand (no positional).
        self.assertEqual(selfcheck[:2],
                         [str(self.root / "scripts/cadence-nextest"), "list"])
        self.assertEqual(run[0], str(self.root / "scripts/cadence-nextest"))
        self.assertTrue(run[1].startswith("--"))
        for cmd in (selfcheck, run):
            for flag, value in (
                ("--cargo-metadata", str(self.root / self.BIN_DIR
                                         / "cargo-metadata.json")),
                ("--binaries-metadata", str(self.root / self.BIN_DIR
                                            / "binaries-metadata.json")),
                ("--workspace-remap", str(self.root)),
                ("--target-dir-remap", str(self.root / "target")),
            ):
                self.assertIn(flag, cmd)
                self.assertEqual(cmd[cmd.index(flag) + 1], value)
            for banned in ("--archive-file", "--extract-to", "--locked",
                           "--features", "--all-targets", "--lib", "--bins",
                           "--test", "--partition"):
                self.assertNotIn(banned, cmd)
        # The version probe is followed by the dependency fetch: unit
        # tests invoke `cargo` directly (cargo tree --locked --offline),
        # which needs the registry the compile-once archive never carries.
        # Fetch runs real Cargo with --locked on the checkout — never a
        # nextest or metadata flag.
        self.assertEqual(fetch, ["cargo", "fetch", "--locked"])
        self.assertIn("-E", selfcheck)
        self.assertIn("-E", run)
        self.assertEqual(run[-1], selfcheck[-1])  # identical filterset
        assignment = json.loads((self.base / "assignment.json").read_text())
        self.assertEqual(sorted(assignment["inventory"]),
                         ["b::one t1", "b::two t2"])

    def test_verify_precedes_every_nextest_call(self):
        # A tampered bundle must refuse before the extraction list: the
        # verifier's ValueError propagates and no subprocess runs.
        with patch.object(runner.bundle, "verify_bundle",
                          side_effect=ValueError("archive_sha256 mismatch")), \
             patch.object(runner.subprocess, "run", side_effect=self.fake):
            with self.assertRaises(ValueError):
                runner.run(self.root, self.plan, "tests", partition="1/8",
                           assignment_out=self.base / "a.json",
                           bundle_dir=self.dir, expected_path=self.authority)
        self.assertEqual(self.calls, [])

    def test_expected_context_cannot_be_supplied_by_the_bundle(self):
        inner = self.dir / 'expected.json'
        inner.write_bytes(self.authority.read_bytes())
        with self.assertRaises(ValueError):
            self.reused(expected_path=inner)
        self.assertEqual(self.calls, [])

    def test_same_ids_with_changed_ignored_status_cannot_shrink_coverage(self):
        changed = json.loads(json.dumps(LIST_DOC))
        changed['rust-suites']['b::one']['testcases']['t1']['ignored'] = True
        original = self.served
        self.served = lambda cmd: changed
        try:
            with self.assertRaises(ValueError):
                self.reused()
        finally:
            self.served = original
        self.assertEqual(len(self.calls), 1)  # extraction list only

    def test_cli_plan_must_equal_bundle_plan_bytes(self):
        self.plan.write_text(json.dumps(dict(FULL, reason="different")))
        with self.assertRaises(ValueError):
            self.reused()
        self.assertEqual(self.calls, [])  # refused before any nextest call

    def test_expected_workspace_root_mismatch_refuses_before_verify(self):
        with patch.object(runner.bundle, "verify_bundle") as verify, \
             patch.object(runner.subprocess, "run", side_effect=self.fake):
            with self.assertRaises(ValueError):
                self.reused(expected_patch={"workspace_root": str(self.base)})
        verify.assert_not_called()
        self.assertEqual(self.calls, [])

    def test_target_dir_must_be_absent_including_symlink(self):
        (self.root / "target").mkdir()
        with self.assertRaises(ValueError):
            self.reused()
        (self.root / "target").rmdir()
        (self.root / "target").symlink_to(self.base / "elsewhere")
        with self.assertRaises(ValueError):
            self.reused()
        self.assertEqual(self.calls, [])
        (self.root / "target").unlink()

    def test_cargo_target_dir_remap_env_refused(self):
        for value in (str(self.base / "alt"), str(self.root / "target")):
            self.calls.clear()
            with self.subTest(value=value), \
                 patch.dict("os.environ", {"CARGO_TARGET_DIR": value}):
                with self.assertRaises(ValueError):
                    self.reused()
            self.assertEqual(self.calls, [])

    def test_extracted_inventory_must_equal_producer_inventory(self):
        doc = {"rust-suites": {"b::one": {"testcases": {
            "t1": case(), "intruder": case()}}}}
        served = self.served
        self.served = lambda cmd: doc  # the extracted list lies
        try:
            with self.assertRaises(ValueError):
                self.reused()
        finally:
            self.served = served
        # Refusal lands before the run: only the extraction list happened.
        self.assertEqual(len(self.calls), 1)
        self.assertIn("--extract-to", self.calls[0])

    def test_compiled_binary_must_match_expected_source(self):
        # A binary whose embedded source SHA differs from the expected
        # context refuses before any assigned-tests run.
        with self.assertRaises(ValueError):
            self.reused(version="cadence 0.1.2+" + "b" * 40)
        for cmd in self.calls:  # the compiled-tests run never starts
            self.assertNotEqual(cmd[1:2], ["run"])

    def test_consumer_fetches_locked_deps_for_direct_cargo_calls(self):
        # CAD-858 follow-up: `cargo tree --locked --offline` inside the
        # suite resolves the lockfile against the registry cache, which
        # the compile-once archive does not carry. A real
        # `cargo fetch --locked` after extraction populates it; a missing
        # fetch leaves the suite's cargo calls dead — this test fails
        # pre-fix (4 calls, no fetch) and passes once fetch is wired.
        _, check = self.reused()
        check.assert_called_once()
        fetch_calls = [c for c in self.calls
                       if c[:3] == ["cargo", "fetch", "--locked"]]
        self.assertEqual(len(fetch_calls), 1, self.calls)
        # Fetch happens after extraction and the version probe, before
        # the self-check list and the run.
        self.assertLess(self.calls.index(fetch_calls[0]),
                        len(self.calls) - 2)

    def test_fetch_never_runs_for_docs(self):
        plan_doc = {"schema": 1, "mode": "docs", "targets": [],
                    "reason": "docs only"}
        (self.dir / "ci-test-plan.json").write_text(json.dumps(plan_doc))
        self.plan.write_bytes((self.dir / "ci-test-plan.json").read_bytes())
        (self.dir / "inventory.json").write_text(
            json.dumps({"rust-suites": {}, "test-count": 0}))
        (self.dir / "nextest.tar.zst").write_bytes(b"")
        self.reused(partition="1/8", install_stub=False)
        self.assertNotIn(["cargo", "fetch", "--locked"], self.calls)

    def test_bundle_dir_must_not_live_under_root(self):
        inner = self.root / "bundle"
        inner.mkdir()
        for name in ("ci-test-plan.json", "inventory.json", "nextest.tar.zst"):
            (inner / name).write_bytes((self.dir / name).read_bytes())
        with self.assertRaises(ValueError):
            self.reused(bundle_dir=inner)
        self.assertEqual(self.calls, [])

    def test_empty_shard_still_extracts_and_verifies_binary(self):
        _, check = self.reused(partition="8/8")
        check.assert_called_once()
        # Extraction list + version probe + dependency fetch happen, but
        # the empty assignment means no self-check list and no run.
        self.assertEqual(len(self.calls), 3)
        self.assertIn("--extract-to", self.calls[0])
        self.assertEqual(self.calls[2], ["cargo", "fetch", "--locked"])
        assignment = json.loads((self.base / "assignment.json").read_text())
        self.assertEqual(assignment["tests"], [])

    def test_reuse_without_partition_runs_all_selected_metadata_only(self):
        self.reused(partition="")
        extract, probe, fetch, run = self.calls
        self.assertIn("--extract-to", extract)
        self.assertEqual(probe,
                         [str(self.root / "target/debug/cadence"), "--version"])
        self.assertEqual(fetch, ["cargo", "fetch", "--locked"])
        self.assertIn("--workspace-remap", run)
        self.assertNotIn("-E", run)
        for banned in ("--locked", "--features", "--all-targets"):
            self.assertNotIn(banned, run)

    def test_reuse_requires_the_recorded_plan_file(self):
        with self.assertRaises(ValueError):
            runner.run(self.root, dict(FULL), "tests", partition="1/8",
                       assignment_out=self.base / "a.json",
                       bundle_dir=self.dir, expected_path=self.authority)

    def test_missing_version_probe_binary_fails(self):
        # Extraction that never produced target/debug/cadence must fail
        # loud at the version probe, not silently run unverified code.
        with self.assertRaises((ValueError, OSError)):
            self.reused(install_stub=False)

    def test_docs_bundle_verifies_markers_and_never_extracts(self):
        plan_doc = {"schema": 1, "mode": "docs", "targets": [],
                    "reason": "docs only"}
        (self.dir / "ci-test-plan.json").write_text(json.dumps(plan_doc))
        self.plan.write_bytes((self.dir / "ci-test-plan.json").read_bytes())
        (self.dir / "inventory.json").write_text(
            json.dumps({"rust-suites": {}, "test-count": 0}))
        (self.dir / "nextest.tar.zst").write_bytes(b"")
        out = self.base / "assignment.json"
        verify, check = self.reused(partition="1/8", install_stub=False,
                                    assignment_out=out)
        verify.assert_called_once()
        check.assert_not_called()
        # Docs never extracts, probes or fetches — nothing calls out.
        self.assertEqual(self.calls, [])
        self.assertFalse((self.root / "target").exists())
        assignment = json.loads(out.read_text())
        self.assertEqual(assignment["tests"], [])

    def test_docs_bundle_rejects_non_empty_markers(self):
        plan_doc = {"schema": 1, "mode": "docs", "targets": [],
                    "reason": "docs only"}
        (self.dir / "ci-test-plan.json").write_text(json.dumps(plan_doc))
        self.plan.write_bytes((self.dir / "ci-test-plan.json").read_bytes())
        (self.dir / "inventory.json").write_text(
            json.dumps({"rust-suites": {}, "test-count": 0}))
        # A non-empty archive marker refuses before extraction.
        with self.assertRaises(ValueError):
            self.reused(partition="1/8", install_stub=False)
        self.assertEqual(self.calls, [])
        self.assertFalse((self.root / "target").exists())

    def test_reuse_flags_rejected_without_bundle_pair(self):
        for kw in ({"bundle_dir": self.dir}, {"expected_path": self.authority}):
            with self.subTest(kw=kw), self.assertRaises(ValueError):
                runner.run(self.root, self.plan, "tests", **kw)
        with self.assertRaises(ValueError):
            runner.run(self.root, self.plan, "inventory",
                       bundle_dir=self.dir, expected_path=self.authority)

    def test_archive_protocol_marker_is_a_literal(self):
        # The producer's AST capability probe needs an exact literal.
        self.assertEqual(runner.ARCHIVE_PROTOCOL, 1)


class ShardWeightsGen(unittest.TestCase):
    def test_highest_attempt_wins_numerically(self):
        with tempfile.TemporaryDirectory() as tmp:
            for attempt, secs in [(1, 5.0), (9, 6.0), (10, 7.0)]:
                costs_dir(tmp, attempt, 1, [("b", "t", secs)])
            weights = weights_gen.weights_from(tmp)
            self.assertEqual(weights, {"b t": 7.0})

    def test_unparseable_dir_fails_loud(self):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "nextest-costs-42-shard-1").mkdir()
            with self.assertRaises(ValueError):
                weights_gen.weights_from(tmp)


class ShardWeightsGenExtended(unittest.TestCase):
    """Generator contract through the real script entry point."""

    def run_gen(self, *dirs):
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "shard-weights"), *dirs],
            capture_output=True, text=True,
        )

    def test_newest_input_dir_wins(self):
        with tempfile.TemporaryDirectory() as tmp:
            old = Path(tmp) / "costs-old"; new = Path(tmp) / "costs-new"
            costs_dir(old, 1, 1, [("b", "t", 9.9)])
            costs_dir(new, 1, 1, [("b", "t", 6.0)])
            r = self.run_gen(new, old)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(json.loads(r.stdout)["weights"]["b t"], 6.0)

    def test_min_seconds_threshold(self):
        with tempfile.TemporaryDirectory() as tmp:
            costs_dir(tmp, 1, 1, [("b", "slow", 5.0), ("b", "fast", 4.9)])
            r = self.run_gen(tmp)
            self.assertEqual(r.returncode, 0, r.stderr)
            weights = json.loads(r.stdout)["weights"]
            self.assertNotIn("b fast", weights)
            # Exactly at the threshold counts (>= min_seconds).
            self.assertIn("b slow", weights)

    def test_output_is_deterministic_and_sorted(self):
        with tempfile.TemporaryDirectory() as tmp:
            costs_dir(tmp, 1, 1, [("b", "zz", 9.0), ("b", "aa", 8.0), ("a", "t", 7.0)])
            one, two = self.run_gen(tmp).stdout, self.run_gen(tmp).stdout
            self.assertEqual(one, two)
            keys = list(json.loads(one)["weights"])
            self.assertEqual(keys, sorted(keys))

    def test_malformed_report_fails_loud(self):
        for entry in [
            {"suite": "b", "name": "t"},                       # missing duration
            {"suite": "b", "name": "t", "duration_s": "x"},    # non-numeric
            {"suite": "b b", "name": "t", "duration_s": 6.0},  # space in id
        ]:
            with tempfile.TemporaryDirectory() as tmp, self.subTest(entry=entry):
                report_dir = Path(tmp) / "nextest-costs-42-1-shard-1"
                report_dir.mkdir()
                report = report_dir / "nextest-costs.json"
                report.write_text(json.dumps({"slowest": [entry]}))
                r = self.run_gen(tmp)
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("nextest-costs.json", r.stderr)


if __name__ == "__main__":
    unittest.main()
