"""Public-seam tests for scripts/native-review-observation.py (CAD-680)."""
import copy
import importlib.util
import os
import sys
import threading
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MODULE_PATH = os.path.join(ROOT, "scripts", "native-review-observation.py")


def _load_module():
    spec = importlib.util.spec_from_file_location("native_review_observation", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


SHA = "a" * 40
PR_URL = "https://github.com/favcrm/cadence/pull/42"
PR43_URL = "https://github.com/favcrm/cadence/pull/43"
PR1_URL = "https://github.com/favcrm/cadence/pull/1"
EXPECTED_BATCH = {"requests": [{"issue": "CAD-680", "pr": PR_URL, "sha": SHA}]}


class RecordingGitHub:
    def __init__(self, snapshots, trace=None):
        self._snapshots = list(snapshots)
        self.calls = 0
        self._trace = trace

    def snapshot(self):
        self.calls += 1
        if self._trace is not None:
            self._trace.append("github")
        snap = self._snapshots[min(self.calls - 1, len(self._snapshots) - 1)]
        return copy.deepcopy(snap)


class RecordingNative:
    def __init__(self, exports, trace=None):
        self._exports = list(exports)
        self.calls = 0
        self.batches = []
        self._trace = trace

    def export(self, batch):
        self.calls += 1
        if self._trace is not None:
            self._trace.append("native")
        self.batches.append(copy.deepcopy(batch))
        export = self._exports[min(self.calls - 1, len(self._exports) - 1)]
        return copy.deepcopy(export)


def snapshot(**pr_overrides):
    pr = {"number": 42, "pr": PR_URL, "sha": SHA, "base_ref": "main", "state": "OPEN"}
    pr.update(pr_overrides)
    return {"repository_id": 1372936414, "repository": "favcrm/cadence",
            "complete": True, "pull_requests": [pr]}


def review(**overrides):
    r = {"issue": "CAD-680", "project": "cadence", "repository": "favcrm/cadence",
         "number": 42, "pr": PR_URL, "sha": SHA, "worker": "worker",
         "reviewer": "reviewer", "report": "CAD-680/reports/pass.md", "reviewed_at": 990}
    r.update(overrides)
    return r


def export(*reviews, **overrides):
    e = {"schema": "cadence.review-evidence/1", "transport_only": True,
         "checked_at": 1000, "reviews": list(reviews)}
    e.update(overrides)
    return e


def scope(**overrides):
    s = {"kind": "pull_request", "issue": "CAD-680", "number": 42}
    s.update(overrides)
    return s


class HeldClock:
    # Yields each value once, then retains the final value forever.
    def __init__(self, values):
        self._values = list(values)
        self._last = self._values[-1]

    def __call__(self):
        if self._values:
            self._last = self._values.pop(0)
        return self._last


class ObserveTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mod = _load_module()

    def config(self):
        return self.mod.Config(1372936414, "favcrm/cadence", "cadence", "main")

    def observe(self, gh, native, scope_=None, **kw):
        args = dict(wall_clock=lambda: 1000, monotonic=lambda: 0)
        args.update(kw)
        return self.mod.observe(scope_ if scope_ is not None else scope(),
                                gh, native, self.config(), **args)

    def test_positive_exact_binding(self):
        trace = []
        gh = RecordingGitHub([snapshot(), snapshot()], trace=trace)
        native = RecordingNative([export(review()), export(review())], trace=trace)
        result = self.observe(gh, native)
        self.assertEqual(result["schema"], "cadence.review-observation/1")
        self.assertIs(result["observation_only"], True)
        self.assertIs(result["matched"], True)
        self.assertEqual(result["checked_at"], [1000, 1000])
        for key, want in [("repository_id", 1372936414), ("repository", "favcrm/cadence"),
                          ("project", "cadence"), ("base_ref", "main"), ("issue", "CAD-680"),
                          ("number", 42), ("pr", PR_URL), ("sha", SHA)]:
            self.assertEqual(result[key], want)
        self.assertEqual(result["review"], review())
        self.assertEqual(gh.calls, 2)
        self.assertEqual(native.calls, 2)
        self.assertEqual(native.batches, [EXPECTED_BATCH, EXPECTED_BATCH])
        self.assertEqual(trace, ["github", "native", "github", "native"])

    def test_unknown_scope_kind_and_caller_fields_refused_before_adapters(self):
        gh = RecordingGitHub([snapshot()])
        native = RecordingNative([export(review())])
        caller_fields = scope(file="x", evidence="y", transport="z", app="w")
        caller_fields["App"] = "w"
        scopes = [
            scope(kind="merge_group"),
            scope(extra_field="x"),
            {},
            {k: v for k, v in scope().items() if k != "issue"},
            caller_fields,
            json_like_native_export_scope(),
        ]
        for s in scopes:
            with self.subTest(scope=repr(s)[:60]):
                result = self.observe(gh, native, scope_=s)
                self.assertFalse(result["matched"])
                self.assertIn("reason", result)
        self.assertEqual(gh.calls, 0)
        self.assertEqual(native.calls, 0)

    def test_snapshot_mismatch_cases_refuse(self):
        cases = {
            "incomplete": dict(snapshot(), complete=False),
            "complete_int": dict(snapshot(), complete=1),
            "duplicate_pr": {**snapshot(), "pull_requests": [
                snapshot()["pull_requests"][0],
                dict(snapshot()["pull_requests"][0], sha="b" * 40)]},
            "wrong_repo": dict(snapshot(), repository="favcrm/other"),
            "wrong_repo_id": dict(snapshot(), repository_id=999),
            "closed": snapshot(state="CLOSED"),
            "moved_base": snapshot(base_ref="other"),
            "missing": dict(snapshot(), pull_requests=[]),
            "remote_top_extra_field": dict(snapshot(), forged=True),
            "remote_pr_extra_field": snapshot(forged=True),
            "partial_sha": snapshot(sha="a" * 7),
            "uppercase_sha": snapshot(sha="A" * 40),
        }
        for name, snap in cases.items():
            with self.subTest(case=name):
                gh = RecordingGitHub([snap, snap])
                native = RecordingNative([export(review()), export(review())])
                result = self.observe(gh, native)
                self.assertFalse(result["matched"])
                self.assertIn("reason", result)

    def test_same_sha_other_open_pr_different_base_is_not_match(self):
        # PR42 on main at SHA (valid) plus PR43 at the same SHA on another base.
        snap = dict(snapshot(), pull_requests=[
            snapshot()["pull_requests"][0],
            snapshot(number=43, pr=PR43_URL, base_ref="other")["pull_requests"][0],
        ])
        gh = RecordingGitHub([snap, snap])
        native = RecordingNative([export(review()), export(review())])
        self.assertFalse(self.observe(gh, native)["matched"])

    def test_native_binding_and_cardinality_refuse(self):
        cases = {
            "wrong_sha": export(review(sha="b" * 40)),
            "wrong_pr": export(review(pr="https://github.com/favcrm/cadence/pull/7")),
            "wrong_issue": export(review(issue="CAD-1")),
            "wrong_number": export(review(number=43)),
            "wrong_project": export(review(project="other")),
            "wrong_repository": export(review(repository="favcrm/other")),
            "missing": export(),
            "duplicate": export(review(), review()),
            "review_extra_field": export(dict(review(), forged=True)),
            "export_extra_field": dict(export(review()), forged=True),
            "wrong_schema": dict(export(review()), schema="other/1"),
            "transport_only_int": export(review(), transport_only=1),
        }
        for name, exp in cases.items():
            with self.subTest(case=name):
                gh = RecordingGitHub([snapshot(), snapshot()])
                native = RecordingNative([exp, export(review())])
                self.assertFalse(self.observe(gh, native)["matched"])

    def test_self_review_and_empty_identities_refuse(self):
        for name, rev in {
            "self_review": review(worker="same", reviewer="same"),
            "empty_worker": review(worker=""),
            "empty_reviewer": review(reviewer=""),
            "empty_report": review(report=""),
            "whitespace_worker": review(worker=" "),
            "whitespace_report": review(report="  "),
            "control_char_report": review(report="CAD-680/reports/\x00pass.md"),
        }.items():
            with self.subTest(case=name):
                gh = RecordingGitHub([snapshot(), snapshot()])
                native = RecordingNative([export(rev), export(rev)])
                self.assertFalse(self.observe(gh, native)["matched"])

    def test_checked_at_freshness(self):
        # No review-age expiry: an old reviewed_at is still standing evidence.
        # Freshness applies to checked_at vs wall_clock(1000): accepted at 970,
        # stale below, future above. reviewed_at must stay <= checked_at.
        cases = {
            "old_review_valid": (review(reviewed_at=1), 1000, True),
            "accepted_edge": (review(reviewed_at=1), 970, True),
            "stale": (review(reviewed_at=1), 969, False),
            "future": (review(reviewed_at=1), 1001, False),
            "reviewed_at_future": (review(reviewed_at=1001), 1000, False),
            "reviewed_at_string": (review(reviewed_at="now"), 1000, False),
            "reviewed_at_bool": (review(reviewed_at=True), 1000, False),
        }
        for name, (rev, checked_at, want) in cases.items():
            with self.subTest(case=name):
                gh = RecordingGitHub([snapshot(), snapshot()])
                exp = export(rev, checked_at=checked_at)
                native = RecordingNative([exp, exp])
                self.assertEqual(self.observe(gh, native)["matched"], want)

    def test_strict_scalar_types(self):
        cases = {
            "bool_number": ("snapshot", snapshot(number=True)),
            "str_number": ("snapshot", snapshot(number="42")),
            "bool_sha": ("export", export(review(sha=True))),
            "int_sha": ("export", export(review(sha=123))),
            "bool_checked_at": ("export", dict(export(review()), checked_at=True)),
            "str_checked_at": ("export", dict(export(review()), checked_at="1000")),
        }
        for name, (kind, obj) in cases.items():
            with self.subTest(case=name):
                gh = RecordingGitHub([snapshot(), snapshot()])
                exps = [export(review()), export(review())]
                if kind == "snapshot":
                    gh = RecordingGitHub([obj, obj])
                else:
                    exps = [obj, export(review())]
                result = self.observe(gh, RecordingNative(exps))
                self.assertFalse(result["matched"])

    def test_second_export_binding_change_detected(self):
        for name, second in {
            "sha_change": export(review(sha="b" * 40)),
            "report_change": export(review(report="CAD-680/reports/fail.md")),
            "worker_change": export(review(worker="worker2")),
            "reviewer_change": export(review(reviewer="reviewer2")),
            "reviewed_at_change": export(review(reviewed_at=980)),
        }.items():
            with self.subTest(case=name):
                gh = RecordingGitHub([snapshot(), snapshot()])
                native = RecordingNative([export(review()), second])
                result = self.observe(gh, native)
                self.assertFalse(result["matched"])

    def test_in_place_mutation_detected(self):
        pr = snapshot()["pull_requests"][0]
        rev = review()

        class MutatingGitHub:
            def __init__(self):
                self.calls = 0

            def snapshot(self):
                self.calls += 1
                if self.calls == 2:
                    pr["sha"] = "b" * 40
                return {"repository_id": 1372936414, "repository": "favcrm/cadence",
                        "complete": True, "pull_requests": [pr]}

        class AliasNative:
            # Returns the same export object twice; mutates it between calls.
            def __init__(self):
                self.calls = 0
                self.exp = export(rev)

            def export(self, batch):
                self.calls += 1
                if self.calls == 2:
                    rev["report"] = "CAD-680/reports/fail.md"
                return self.exp

        for name, (gh, native) in {
            "remote": (MutatingGitHub(), RecordingNative([export(review()), export(review())])),
            "native": (RecordingGitHub([snapshot(), snapshot()]), AliasNative()),
        }.items():
            with self.subTest(case=name):
                self.assertFalse(self.observe(gh, native)["matched"])

    def test_head_change_between_snapshots_detected(self):
        first_seen = threading.Event()
        release = threading.Event()
        errors = []
        state = {"snap": snapshot()}

        class SwitchingGitHub:
            def __init__(self):
                self.calls = 0

            def snapshot(self):
                self.calls += 1
                if self.calls == 1:
                    first = copy.deepcopy(state["snap"])
                    first_seen.set()
                    if not release.wait(timeout=5):
                        errors.append("release timeout")
                    return first
                return copy.deepcopy(state["snap"])

        gh = SwitchingGitHub()
        native = RecordingNative([export(review()), export(review())])
        holder = {}
        t = threading.Thread(target=lambda: holder.update(
            {"r": self.observe(gh, native)}))
        t.start()
        self.assertTrue(first_seen.wait(timeout=5))
        state["snap"] = snapshot(sha="b" * 40)
        release.set()
        t.join(timeout=10)
        self.assertFalse(t.is_alive())
        self.assertFalse(errors)
        self.assertEqual(gh.calls, 2)
        self.assertFalse(holder["r"]["matched"])

    def test_adapter_exceptions_sanitized_and_deadline(self):
        # All four adapter-call boundaries sanitize exception text.
        for ordinal in (1, 2, 3, 4):
            class Boom:
                def __init__(self):
                    self.calls = 0

                def snapshot(self):
                    self.calls += 1
                    if self.calls == ordinal:
                        raise RuntimeError("secret-token-xyz")
                    return snapshot()

                def export(self, batch):
                    self.calls += 1
                    if self.calls == ordinal:
                        raise RuntimeError("secret-token-xyz")
                    return export(review())

            with self.subTest(ordinal=ordinal):
                boom = Boom()
                result = self.observe(boom, boom)
                self.assertFalse(result["matched"])
                self.assertNotIn("secret-token-xyz", str(result))
                self.assertEqual(boom.calls, ordinal)

        # Cumulative monotonic budget (5s deadline) expires after 4 reads.
        self.assertFalse(self.observe(RecordingGitHub([snapshot(), snapshot()]),
                                      RecordingNative([export(review()), export(review())]),
                                      monotonic=HeldClock([0, 1.5, 3, 4.5, 6]))["matched"])
        # Nonfinite and bool clocks refuse.
        for clock in (float("nan"), float("inf"), True):
            with self.subTest(clock=clock):
                self.assertFalse(self.observe(RecordingGitHub([snapshot()]),
                                              RecordingNative([export(review())]),
                                              monotonic=lambda c=clock: c)["matched"])

        # Backwards clock must not hang; result is a refusal, not a type fluke.
        self.assertFalse(self.observe(RecordingGitHub([snapshot()]),
                                      RecordingNative([export(review())]),
                                      monotonic=HeldClock([0, -1]))["matched"])


class StrictEqualityTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mod = _load_module()

    def observe(self, gh, native, scope_=None, config=None):
        cfg = config if config is not None else self.mod.Config(
            1372936414, "favcrm/cadence", "cadence", "main")
        return self.mod.observe(scope_ if scope_ is not None else scope(),
                                gh, native, cfg,
                                wall_clock=lambda: 1000, monotonic=lambda: 0)

    def test_loose_scalar_equality_traps_refuse(self):
        # Numeric/string traps that Python == accepts must still refuse.
        float_repo = dict(snapshot(), repository_id=1372936414.0)
        cases = {
            "remote_repository_id_float": (
                RecordingGitHub([float_repo, float_repo]),
                RecordingNative([export(review()), export(review())]),
                scope(), None),
            "native_number_float": (
                RecordingGitHub([snapshot(), snapshot()]),
                RecordingNative([export(review(number=42.0)),
                                 export(review(number=42.0))]),
                scope(), None),
            "sha_trailing_newline": (
                RecordingGitHub([snapshot(sha=SHA + "\n"),
                                 snapshot(sha=SHA + "\n")]),
                RecordingNative([export(review(sha=SHA + "\n")),
                                 export(review(sha=SHA + "\n"))]),
                scope(), None),
            # bool == int must not satisfy equality either.
            "remote_repository_id_bool": (
                RecordingGitHub([dict(snapshot(), repository_id=True),
                                 dict(snapshot(), repository_id=True)]),
                RecordingNative([export(review()), export(review())]),
                scope(), self.mod.Config(1, "favcrm/cadence", "cadence", "main")),
            "native_number_bool": (
                RecordingGitHub([snapshot(number=1, pr=PR1_URL),
                                 snapshot(number=1, pr=PR1_URL)]),
                RecordingNative([export(review(number=True, pr=PR1_URL)),
                                 export(review(number=True, pr=PR1_URL))]),
                scope(number=1), None),
        }
        for name, (gh, native, sc, cfg) in cases.items():
            with self.subTest(case=name):
                self.assertFalse(self.observe(gh, native, scope_=sc,
                                              config=cfg)["matched"])

    def test_caller_scope_borrowed_mutation_refuses(self):
        # The adapter mutates the caller's scope dict and returns reviews
        # for the mutated issue; the originally requested binding must
        # not change and the observation must refuse.
        caller_scope = scope()

        class MutatingScopeNative:
            def __init__(self):
                self.calls = 0
                self.batches = []

            def export(self, batch):
                self.calls += 1
                self.batches.append(copy.deepcopy(batch))
                caller_scope["issue"] = "CAD-2"
                return copy.deepcopy(export(review(issue="CAD-2")))

        gh = RecordingGitHub([snapshot(), snapshot()])
        native = MutatingScopeNative()
        result = self.observe(gh, native, scope_=caller_scope)
        self.assertFalse(result["matched"])
        self.assertGreaterEqual(native.calls, 1)
        for batch in native.batches:
            self.assertEqual(batch, EXPECTED_BATCH)

    def test_export_copied_before_next_callback(self):
        # A shared mutable export aliased across native returns: the
        # monotonic clock callback right after the first native return
        # mutates it, so the second native read observes "new".
        # The first returned export must already have been copied, so
        # the observation must refuse with the changed review.
        shared = export(review(report="CAD-680/reports/old.md"))

        class SharedNative:
            def __init__(self):
                self.calls = 0

            def export(self, batch):
                self.calls += 1
                return shared

        mono_calls = [0]

        def mono():
            mono_calls[0] += 1
            if mono_calls[0] == 3:  # tick right after first native return
                shared["reviews"][0]["report"] = "CAD-680/reports/new.md"
            return 0

        gh = RecordingGitHub([snapshot(), snapshot()])
        native = SharedNative()
        result = self.mod.observe(scope(), gh, native, self.mod.Config(
            1372936414, "favcrm/cadence", "cadence", "main"),
            wall_clock=lambda: 1000, monotonic=mono)
        self.assertFalse(result["matched"])
        self.assertEqual(native.calls, 2)

    def test_wall_and_config_vectors(self):
        mod = self.mod
        cfg = mod.Config(1372936414, "favcrm/cadence", "cadence", "main")
        for kwargs in ({"repository_id": 0}, {"repository_id": True},
                       {"repository": "bad repo"}, {"project": ""},
                       {"base_ref": " "}):
            full = {"repository_id": 1372936414,
                    "repository": "favcrm/cadence",
                    "project": "cadence", "base_ref": "main"}
            full.update(kwargs)
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(ValueError):
                    mod.Config(**full)
        # Non-Config config refuses rather than raising.
        self.assertFalse(mod.observe(scope(), RecordingGitHub([snapshot()]),
                                     RecordingNative([export(review())]),
                                     {"repository_id": 1372936414},
                                     wall_clock=lambda: 1000,
                                     monotonic=lambda: 0)["matched"])
        # Backwards wall clock refuses.
        self.assertFalse(mod.observe(
            scope(), RecordingGitHub([snapshot(), snapshot()]),
            RecordingNative([export(review()), export(review())]), cfg,
            wall_clock=HeldClock([1000, 999]), monotonic=lambda: 0)["matched"])
        # Final wall ages the first export past 30s: refuse.
        self.assertFalse(mod.observe(
            scope(), RecordingGitHub([snapshot(), snapshot()]),
            RecordingNative([export(review()), export(review())]), cfg,
            wall_clock=HeldClock([1000, 1000, 1031]),
            monotonic=lambda: 0)["matched"])
        # Advancing checked_at within an advancing wall stays matched;
        # an old reviewed_at remains valid standing evidence.
        rev = review(reviewed_at=1)
        result = mod.observe(
            scope(), RecordingGitHub([snapshot(), snapshot()]),
            RecordingNative([export(rev, checked_at=1000),
                             export(rev, checked_at=1001)]), cfg,
            wall_clock=HeldClock([1000, 1001, 1001]),
            monotonic=lambda: 0)
        self.assertTrue(result["matched"])
        self.assertEqual(result["checked_at"], [1000, 1001])


def json_like_native_export_scope():
    # A native-export-shaped dict is not a valid caller scope either.
    return {"schema": "cadence.review-evidence/1", "transport_only": True,
            "checked_at": 1000, "reviews": [review()]}


if __name__ == "__main__":
    unittest.main()
