#!/usr/bin/env python3
"""CAD-1298 acceptance for the enqueue consumer of delivery requirements.

Expected outcomes come from the ticket-authored acceptance spec, not from the
consumer implementation. This drives the real enqueue-reviewed main/evaluate,
comment posting and final revalidation. The inherited Case only intercepts
external gh/git/cadence transports; its producer-shaped responses are
synthetic and do NOT prove daemon classification, native authority, operator
identity, GitHub, or merge-queue behavior.
"""
import json
import unittest

import test_enqueue_evidence as evidence


ISSUE = evidence.ISSUE
HEAD = evidence.HEAD
BASE = evidence.BASE
REPO = evidence.REPO


def producer_result(case, klass, *, readiness=None, reviews=None, review_kind=None,
                    security_capable=False, operator_approval=False,
                    browser_qa=False, full_checks=True, activation="active"):
    """Build only the RPC wire shape, bound to this test's synthetic PR."""
    digest = "sha256:" + "e" * 64
    defaults = {
        "routine": (0, "none", False, False, False),
        "consequential": (1, "combined", False, False, True),
        "strict": (2, "standards_and_spec", False, False, True),
    }
    count, kind, _security, _operator, checks = defaults[klass]
    req = {"class": klass, "reviews": count if reviews is None else reviews,
           "review_kind": kind if review_kind is None else review_kind,
           "security_capable": security_capable,
           "browser_qa": browser_qa,
           "operator_approval": operator_approval,
           "full_checks": checks if full_checks is None else full_checks,
           "reasons": ["fixture: synthetic requirements response"],
           "policy_digest": digest, "activation": activation}
    return {"issue": ISSUE, "project": case.project, "repo": REPO, "pr": 5,
            "head": HEAD, "base": BASE, "merge_base": "c" * 40,
            "policy_digest": digest, "requirements": req,
            "readiness": readiness}


def readiness():
    return {"repo": REPO, "pr": 5, "head": HEAD, "base": BASE,
            "policy_digest": "sha256:" + "e" * 64, "ci_run_id": evidence.RUN_ID,
            "ci_run_url": evidence.CI_URL,
            "outcome_report": f"{ISSUE}/reports/worker-done.md"}


class RequirementsConsumer(evidence.EvidencePosting):
    def setUp(self):
        super().setUp()

    def set_active(self, klass, **kwargs):
        self.c.requirements_override = producer_result(self.c, klass, **kwargs)

    def add_combined_review(self, reviewer="reviewer"):
        self.c.verdict_list = [self.c.mknote(
            "20261003-100000-cad1298-combined-verdict.md", "20261003-100000",
            reviewer, "combined", risk="auto")]

    def test_consequential_uses_one_independent_combined_review(self):
        self.c.paths = ["src/ordinary.rs"]
        self.set_active("consequential")
        self.add_combined_review()
        rc, out, err = self.run_main()
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(self.c.posted), 1)
        self.assertEqual(len(self.c.merges), 1)
        self.assertIn("Verdict notes: 20261003-100000-cad1298-combined-verdict.md",
                      self.c.posted[0])
        self.assertEqual(self.c.approval_calls, 0)

    def test_consequential_author_as_reviewer_is_refused(self):
        self.c.paths = ["src/ordinary.rs"]
        self.set_active("consequential")
        self.add_combined_review("cc")  # PR author from the fixture
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)
        self.assertIn("no pass verdict", err)

    def test_native_authority_binding_tampering_fails_closed(self):
        mutations = {
            "foreign issue": {"issue": "CAD-9999"},
            "foreign project": {"project": "other-project"},
            "foreign repo": {"repo": "evil/repo"},
            "foreign PR": {"pr": 6},
            "foreign head": {"head": "f" * 40},
            "foreign base": {"base": "f" * 40},
            "malformed digest": {"policy_digest": "bad"},
        }
        for label, changes in mutations.items():
            with self.subTest(binding=label):
                self.fresh()
                self.c.paths = ["README.md"]
                self.c.verdict_list = []
                result = producer_result(self.c, "routine", readiness=readiness())
                result.update(changes)
                self.c.requirements_override = result
                rc, out, err = self.run_main()
                self.assert_refused_clean(rc, err)
                self.assertIn("authoritative", err)

    def test_malformed_mandatory_review_cardinality_is_not_trusted(self):
        self.c.paths = ["src/ordinary.rs"]
        self.add_combined_review()
        self.set_active("consequential", reviews=0)
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)
        self.assertIn("cannot use authoritative", err)

    def test_inspector_failure_or_malformed_reply_never_falls_back_to_legacy(self):
        for mode in ("failed", "malformed"):
            with self.subTest(mode=mode):
                self.fresh()
                self.c.paths = ["README.md"]
                self.c.verdict_list = []
                original = self.c.run

                def broken(argv, input_text=None):
                    if argv[:3] == ["cadence", "delivery", "requirements"]:
                        if mode == "failed":
                            return 1, "{}", "native inspector unavailable"
                        return 0, "{not-json", ""
                    return original(argv, input_text)

                self.c.run = broken
                rc, out, err = self.run_main()
                self.assert_refused_clean(rc, err)
                self.assertIn("authoritative", err)

    def test_policy_binding_change_after_comment_refuses_enqueue(self):
        self.c.paths = ["src/ordinary.rs"]
        self.add_combined_review()
        first = producer_result(self.c, "consequential")
        changed = producer_result(self.c, "consequential")
        changed["policy_digest"] = "sha256:" + "f" * 64
        changed["requirements"]["policy_digest"] = "sha256:" + "f" * 64
        self.c.requirements_override = first
        self.c.requirements_after_at = 1
        self.c.requirements_after = changed
        rc, out, err = self.run_main()
        self.assertEqual(rc, 1, (out, err))
        self.assertEqual(len(self.c.posted), 1)
        self.assertEqual(self.c.merges, [])
        self.assertIn("no ticket comment names head", err)
        self.assertIn(changed["policy_digest"], err)

    def test_strict_native_result_cannot_downgrade_legacy_review_floor(self):
        self.c.paths = ["src/daemon/caller_rule.rs"]
        self.c.verdict_list = [self.c.mknote(
            "20261003-100000-cad1298-combined-verdict.md", "20261003-100000",
            "reviewer", "combined", risk="human (4, 7)")]
        self.c.per_head = True
        self.c.requirements_override = producer_result(
            self.c, "strict", activation="no_profile", reviews=1,
            review_kind="combined", operator_approval=True)
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)
        self.assertIn("Standards", err)

    def test_strict_native_two_review_floor_cannot_be_downgraded_by_local_one_review_path(self):
        self.c.paths = ["src/ordinary.rs"]
        self.c.verdict_list = [self.c.mknote(
            "20261003-100000-cad1298-combined-verdict.md", "20261003-100000",
            "reviewer", "combined", risk="auto")]
        self.c.requirements_override = producer_result(
            self.c, "strict", activation="no_profile", reviews=2,
            review_kind="standards_and_spec", operator_approval=False)
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)
        self.assertIn("Standards", err)

    def test_protected_auth_uses_legacy_two_reviews_and_operator_approval(self):
        self.c.paths = ["src/daemon/caller_rule.rs"]
        self.c.verdict_list = [
            self.c.mknote("20261003-100000-std-verdict.md", "20261003-100000",
                          "rev-std", "standards", risk="human (4, 7)"),
            self.c.mknote("20261003-100100-spec-verdict.md", "20261003-100100",
                          "rev-spec", "spec-security", risk="human (4, 7)"),
        ]
        self.set_active("strict", reviews=2, review_kind="standards_and_spec",
                        operator_approval=True, full_checks=True)
        rc, out, err = self.run_main()
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(self.c.posted), 1)
        self.assertEqual(len(self.c.merges), 1)
        self.assertEqual(self.c.approval_calls, 2,
                         "protected auth keeps exact-head operator approval at preflight and revalidation")

    def test_protected_auth_single_combined_review_is_refused(self):
        self.c.paths = ["src/daemon/caller_rule.rs"]
        self.add_combined_review()
        self.set_active("strict", reviews=2, review_kind="standards_and_spec",
                        operator_approval=True, full_checks=True)
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)
        self.assertIn("Standards", err)


if __name__ == "__main__":
    unittest.main()
