#!/usr/bin/env python3
"""Exercise test-suite sharding: partition args and phase gating.

The partition travels as an explicit CLI argument, never ambient env:
exact-command contract tests broke twice on env leaking across steps.
"""
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
