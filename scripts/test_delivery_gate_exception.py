#!/usr/bin/env python3
"""CAD-1946: the temporary review exception cannot pass for anything else.

Run: python3 scripts/test_delivery_gate_exception.py
"""

import argparse
import datetime
import importlib.machinery
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    path = os.path.join(HERE, name)
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), path)
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


gate = load("delivery-gate")
enqueue = load("enqueue-reviewed")
REPO, PR, HEAD = "favcrm/cadence", 7, "a" * 40
OTHER = "b" * 40


def note(kind, outcome="pass", stamp="20261103-000000", who="usr_a", login="reviewer"):
    return {"name": f"{stamp}-{kind}-verdict.md", "stamp": stamp, "reviewer": who, "reviewer_id": who,
            "kind": kind, "outcome": outcome, "title_outcome": outcome, "section_outcome": outcome,
            "risk": "auto", "github_login": login, "repo": REPO, "project": "cadence"}


class Signatures(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.key = os.path.join(self.tmp.name, "key")
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", self.key], check=True)
        with open(self.key + ".pub", encoding="utf-8") as f:
            pub = f.read().strip()
        self.signers = os.path.join(self.tmp.name, "signers")
        with open(self.signers, "w", encoding="utf-8") as f:
            f.write(f'{gate.PRINCIPAL} namespaces="{gate.NAMESPACE}" {pub}\n')

    def signed(self, exception, head=HEAD):
        out = StringIO()
        args = argparse.Namespace(repo=REPO, pr=PR, head=head, key=self.key, exception=exception)
        with redirect_stdout(out):
            self.assertEqual(gate.sign(args), 0)
        return out.getvalue()

    def verify(self, bodies, head=HEAD):
        path = os.path.join(self.tmp.name, "comments.json")
        with open(path, "w", encoding="utf-8") as f:
            json.dump([{"id": i, "body": b} for i, b in enumerate(bodies)], f)
        args = argparse.Namespace(repo=REPO, pr=PR, head=head, signers=self.signers, comments=path)
        out = StringIO()
        with redirect_stdout(out), redirect_stderr(StringIO()):
            return gate.verify(args), out.getvalue()

    def closed(self):
        return mock.patch.object(gate, "exception_open", return_value=False)

    def opened(self):
        return mock.patch.object(gate, "exception_open", return_value=True)

    def test_a_normal_pass_is_unchanged(self):
        code, out = self.verify([self.signed(False)])
        self.assertEqual(code, 0)
        self.assertIn("as a pass", out)

    def test_an_exception_says_it_is_one_everywhere(self):
        with self.opened():
            body = self.signed(True)
            code, out = self.verify([body])
        self.assertEqual(code, 0)
        self.assertIn("EXCEPTION, not a pass", out)
        self.assertIn("EXCEPTION, not a pass", body)
        self.assertNotIn("Delivery gate passed", body)

    def test_an_exception_binds_the_exact_head(self):
        with self.opened():
            self.assertEqual(self.verify([self.signed(True)], head=OTHER)[0], 1)

    def test_an_exception_stops_at_the_end(self):
        with self.opened():
            body = self.signed(True)
        with self.closed():
            self.assertEqual(self.verify([body])[0], 1)

    def test_signing_refuses_after_the_end(self):
        args = argparse.Namespace(repo=REPO, pr=PR, head=HEAD, key=self.key, exception=True)
        with self.closed(), redirect_stderr(StringIO()):
            self.assertEqual(gate.sign(args), 1)

    def test_the_end_is_a_real_clock_check(self):
        real = datetime.datetime
        ends = real(2026, 11, 10, tzinfo=datetime.timezone.utc)

        class Fake(real):
            @classmethod
            def now(cls, tz=None):
                return Fake.at

        with mock.patch.object(gate.datetime, "datetime", Fake):
            Fake.at = ends - datetime.timedelta(seconds=1)
            self.assertTrue(gate.exception_open())
            Fake.at = ends
            self.assertFalse(gate.exception_open())

    def test_the_end_is_at_most_seven_days_from_activation(self):
        self.assertEqual(gate.EXCEPTION_ENDS, "2026-11-10T00:00:00Z")

    def test_one_kind_of_signature_cannot_stand_in_for_the_other(self):
        with self.opened():
            exception = self.signed(True)
            normal = self.signed(False)
            relabelled = exception.replace(gate.EXCEPTION_MARKER, gate.MARKER)
            self.assertEqual(self.verify([relabelled])[0], 1)
            upgraded = normal.replace(gate.MARKER, gate.EXCEPTION_MARKER)
            self.assertEqual(self.verify([upgraded])[0], 1)


class Reviews(unittest.TestCase):
    def check(self, rows, **kw):
        return enqueue.check_reviews(rows, False, True, {"author"}, "CAD-1", {"repo": REPO, "project": "cadence"}, **kw)

    def test_normal_mode_still_demands_the_verdicts(self):
        reasons, _ = self.check([])
        self.assertTrue(any("Standards" in r for r in reasons))
        self.assertTrue(any("Browser QA" in r for r in reasons))

    def test_the_exception_drops_only_the_missing_counts(self):
        self.assertEqual(self.check([], exception=True)[0], [])

    def test_a_filed_revise_still_refuses(self):
        reasons, _ = self.check([note("standards", outcome="revise")], exception=True)
        self.assertTrue(any("not a pass" in r for r in reasons), reasons)

    def test_a_verdict_by_the_author_still_refuses(self):
        reasons, _ = self.check([note("standards", login="author")], exception=True)
        self.assertTrue(any("not independent" in r for r in reasons), reasons)


class Advisory(unittest.TestCase):
    OK = {"standards": "https://x.test/1", "spec-security": "https://x.test/2"}

    def test_both_reviews_are_required(self):
        self.assertEqual(enqueue.advisory_reasons(self.OK, False), [])
        self.assertTrue(enqueue.advisory_reasons({"standards": "https://x.test/1"}, False))

    def test_a_ui_change_needs_real_browser_qa_evidence(self):
        self.assertTrue(enqueue.advisory_reasons(self.OK, True))
        self.assertEqual(enqueue.advisory_reasons({**self.OK, "browser-qa": "https://x.test/3"}, True), [])

    def test_one_link_cannot_be_two_reviews(self):
        same = {"standards": "https://x.test/1", "spec-security": "https://x.test/1"}
        self.assertTrue(enqueue.advisory_reasons(same, False))

    def test_a_link_must_be_https(self):
        self.assertTrue(enqueue.advisory_reasons({**self.OK, "standards": "looks fine"}, False))

    def test_evidence_says_it_is_not_a_normal_pass(self):
        body = enqueue.evidence_body({
            "repo": REPO, "pr": PR, "head": HEAD, "base": OTHER, "base_ref": "main", "ci_run": "https://ci",
            "checks": ["build"], "notes": [], "exception": {"ends": gate.EXCEPTION_ENDS, "advisory": self.OK}})
        self.assertIn("EXCEPTION, not a normal gate pass", body)
        self.assertIn(f"Review exception until: {gate.EXCEPTION_ENDS}", body)
        self.assertIn("Advisory review: standards = https://x.test/1", body)


class Routine(unittest.TestCase):
    PROOF = {"ci_run_url": "https://ci/1"}

    def test_only_a_hosted_org_may_lack_readiness_and_it_is_named(self):
        self.assertEqual(enqueue.exception_routine("routine", True, "hosted", None, "https://ci/1"), ([], True))
        reasons, waived = enqueue.exception_routine("routine", True, "local", None, "https://ci/1")
        self.assertTrue(reasons)
        self.assertFalse(waived)

    def test_a_proof_for_another_run_still_refuses(self):
        reasons, _ = enqueue.exception_routine("routine", True, "hosted", self.PROOF, "https://ci/2")
        self.assertTrue(reasons)
        self.assertEqual(enqueue.exception_routine("routine", True, "local", self.PROOF, "https://ci/1"), ([], False))

    def test_other_classes_are_not_touched(self):
        self.assertEqual(enqueue.exception_routine("strict", True, "hosted", None, None), ([], False))

    def test_the_waiver_is_on_the_record(self):
        body = enqueue.evidence_body({
            "repo": REPO, "pr": PR, "head": HEAD, "base": OTHER, "base_ref": "main", "ci_run": "https://ci",
            "checks": [], "notes": [],
            "exception": {"ends": gate.EXCEPTION_ENDS, "advisory": {}, "waives_readiness": True}})
        self.assertIn("routine readiness (hosted has none)", body)

    def test_an_unknown_advisory_kind_refuses(self):
        self.assertTrue(enqueue.advisory_reasons({**Advisory.OK, "junk": "https://x.test/9"}, False))


class CommandLine(unittest.TestCase):
    def test_advisory_alone_is_refused(self):
        with redirect_stderr(StringIO()):
            self.assertEqual(enqueue.main(["1", "--head", HEAD, "--advisory", "standards=https://x"]), 2)


if __name__ == "__main__":
    unittest.main()
