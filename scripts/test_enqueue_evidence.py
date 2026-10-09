#!/usr/bin/env python3
"""CAD-1298: enqueue-reviewed posts the ticket evidence comment itself, once,
only after every preflight gate holds, then re-checks every gate before the
merge command runs.

Independent acceptance check — the author is NOT the implementer. The
expected outcomes below are fixed from the ticket's posting outcome, not
from the implementation:

  * --dry-run performs every check and writes nothing: no tracker comment,
    no merge command;
  * any failed gate — missing verdict, a non-green required check, a forged
    or absent operator approval, a missing/foreign-head/malformed or
    unreadable green ci.yml run — refuses and writes nothing;
  * a stale head, base, state or auto-merge flag — whether it appears
    before the evidence post or after it — refuses the enqueue;
  * a valid proof posts ONE canonical comment naming the head, the base,
    the PR, every counted verdict note, the actual successful ci.yml run
    URL (github.com/<repo>/actions/runs/<id>) and, for a human-class diff,
    the real operator approval id; a retry posts no duplicate;
  * a failed comment write refuses; nothing merges;
  * after the post every gate is re-evaluated against fresh reads: a
    verdict, an approval or a green check that disappears between preflight
    and the final evaluation still refuses;
  * zero counted verdicts can never satisfy the review floor — no PASS is
    invented and nothing posts.

The real main/evaluate/post_evidence/check_evidence_head/green_ci_run/
evidence_body of scripts/enqueue-reviewed run; only the external transports
(gh, git diff evidence via pr_changes, cadence) are faked, by extending the
CAD-1106 fixture in test_enqueue_scope_approval.py. An unexpected command
aborts the test.

The positive routine case uses a producer-shaped, explicitly synthetic
transport response, not a claimed daemon/native observation. The separate
CAD-1298 acceptance module exercises the adapter's fail-closed bindings and
policy classes through this same transport boundary.
"""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import tempfile
import types
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Reuse the CAD-1106 transport fixture (real TOML lists, verdict-note shape,
# gh/cadence reply shapes) instead of duplicating it.
_loader = importlib.machinery.SourceFileLoader(
    "enq_scope_fixture", str(ROOT / "scripts/test_enqueue_scope_approval.py"))
_spec = importlib.util.spec_from_loader("enq_scope_fixture", _loader)
scope = importlib.util.module_from_spec(_spec)
_loader.exec_module(scope)
enq = scope.enq  # the real scripts/enqueue-reviewed module under test

HEAD = "a" * 40
BASE = "b" * 40
REPO = "o/r"
ISSUE = "CAD-1298"
NOTE = "20261003-100000-cad1298-pr5-review-verdict.md"
RUN_ID = 12345
CI_URL = f"https://github.com/{REPO}/actions/runs/{RUN_ID}"
CI_RUN = {"id": RUN_ID, "head_sha": HEAD, "event": "pull_request",
          "status": "completed", "conclusion": "success", "html_url": CI_URL}
OPERATOR = "operator-connection"
APPROVAL_ID = "merge-pr5-aaaa"


class Case(scope.Case):
    """CAD-1106 fixture extended with the evidence-posting endpoints:
    ci.yml workflow runs, `cadence issue comment` (stdin captured), ticket
    comments that reflect posted bodies on later `issue show` reads, the
    merge command and the merge-queue GraphQL read. Every gate result can
    flip after a chosen read count to model staleness."""

    def __init__(self, tmp):
        super().__init__(tmp)
        self.issue = ISSUE
        self.title = f"{ISSUE}: post the enqueue evidence comment"
        self.branch = "cadence/cad-1298-solo-operator-delivery"
        self.refs = [{"kind": "branch", "path": self.branch}]
        self.store = []  # no ticket scope approval; the proof is per-head
        self.per_head = True
        self.approval_id = APPROVAL_ID
        self.approval_via = OPERATOR
        self.approval_missing_after = None   # return "missing" once calls exceed this
        self.approval_calls = 0
        self.note_text = (f"# Verdict: {ISSUE} Review (standards+spec) — pass\n> Issue: {ISSUE}\n> From: rev\n\n"
                          f"## Verdict\npass — PR #5, head {HEAD}\n\nRisk: human (4, 7) — scripts\n"
                          f"Scope: in-scope {ISSUE}\n")
        self.verdict_list = None             # replace the default single combined note
        self.verdicts_after = None           # raw verdicts reply once calls exceed the count
        self.verdicts_after_at = None
        self.verdict_calls = 0
        self.comments = []                   # ticket comments `issue show` returns
        self.posted = []                     # stdin bodies of `issue comment`
        self.comment_rc = 0
        self.merges = []                     # `gh pr merge` argv, in order
        self.merge_rc = 0
        self.ci_rc = 0
        self.ci_runs = [CI_RUN]
        self.rollup = [{"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}]
        self.view_calls = 0
        self.stale_at = None                 # apply stale_fields once views reach this
        self.stale_fields = {"headRefOid": "c" * 40}
        self.rollup_flip_at = None           # required check turns FAILURE at this view count

    def active_routine_authority(self, *, readiness=True):
        digest = "sha256:" + "e" * 64
        run_id = 12345
        result = {"issue": ISSUE, "project": self.project, "repo": REPO, "pr": 5,
                  "head": HEAD, "base": BASE, "merge_base": "c" * 40,
                  "policy_digest": digest,
                  "requirements": {"class": "routine", "reviews": 0, "review_kind": "none",
                                   "security_capable": False, "browser_qa": False,
                                   "operator_approval": False, "full_checks": False,
                                   "reasons": ["fixture: synthetic approved routine profile"],
                                   "policy_digest": digest, "activation": "active"},
                  "readiness": ({"repo": REPO, "pr": 5, "head": HEAD, "base": BASE,
                                 "policy_digest": digest, "ci_run_id": run_id,
                                 "ci_run_url": CI_URL,
                                 "outcome_report": f"{ISSUE}/reports/worker-done.md"}
                                if readiness else None)}
        return result

    def mknote(self, name, stamp, reviewer, kind, outcome="pass", risk="auto"):
        p = Path(self.tmp) / name
        p.write_text(f"# Verdict: {ISSUE} {kind} — {outcome}\n> Issue: {ISSUE}\n> From: {reviewer}\n\n"
                     f"## Verdict\n{outcome} — PR #5, head {HEAD}\n\nRisk: {risk}\n")
        return {"name": name, "path": str(p), "stamp": stamp, "reviewer": reviewer,
                "kind": kind, "outcome": outcome, "title_outcome": outcome,
                "section_outcome": outcome, "risk": risk, "from": reviewer}

    def note(self):
        # The parent fixture binds module-level NOTE/ISSUE; write this
        # issue's note text under this check's note name instead.
        p = Path(self.tmp) / NOTE
        if self.note_text is not None:
            p.write_text(self.note_text)
        return {"name": NOTE, "path": str(p), "stamp": "20261003-100000",
                "reviewer": "rev", "kind": "combined", "outcome": "pass",
                "title_outcome": "pass", "section_outcome": "pass",
                "risk": "human", "from": "rev"}

    def pr_view(self):
        self.view_calls += 1
        rollup = self.rollup
        if self.rollup_flip_at is not None and self.view_calls >= self.rollup_flip_at:
            rollup = [{"name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
        view = {"number": 5, "state": "OPEN", "title": self.title, "headRefName": self.branch,
                "headRefOid": HEAD, "baseRefName": "main", "baseRefOid": BASE,
                "mergeStateStatus": "CLEAN", "autoMergeRequest": None,
                "author": {"login": "cc"}, "statusCheckRollup": rollup}
        if self.stale_at is not None and self.view_calls >= self.stale_at:
            view.update(self.stale_fields)
        return 0, json.dumps(view), ""

    def run(self, argv, input_text=None):
        if argv[:3] == ["gh", "pr", "view"]:
            return self.pr_view()
        if argv[:3] == ["gh", "pr", "merge"]:
            self.merges.append(list(argv))
            return self.merge_rc, "", "merge refused"
        if (argv[:2] == ["gh", "api"] and "/contents/" in argv[-1]
                and any(f in argv[-1] for f in (enq.ONE_REVIEW_FILE, enq.RISK_PATHS_FILE))):
            # The trusted-base lists are served at whatever ref the caller
            # pins (a stale-base probe reads them at the moved base too).
            for f in (enq.ONE_REVIEW_FILE, enq.RISK_PATHS_FILE):
                if f"/contents/{f}?" in argv[-1]:
                    return 0, (ROOT / f).read_text(), ""
        if argv[:3] == ["gh", "api", "graphql"]:
            return 0, json.dumps({"data": {"repository": {"pullRequest": {
                "isInMergeQueue": True, "state": "OPEN"}}}}), ""
        if argv[:2] == ["gh", "api"] and "actions/workflows/ci.yml/runs" in argv[-1]:
            if self.ci_rc != 0:
                return self.ci_rc, "", "actions api down"
            return 0, json.dumps({"workflow_runs": self.ci_runs}), ""
        if argv[:3] == ["cadence", "issue", "comment"]:
            if self.comment_rc != 0:
                return self.comment_rc, "", "tracker write failed"
            self.posted.append(input_text)
            self.comments.append({"body": input_text})
            return 0, "", ""
        if argv[:3] == ["cadence", "issue", "show"]:
            return 0, json.dumps({"id": argv[3], "project": self.project, "owner": None,
                                  "claim": None, "body": self.ticket_body, "status": self.status,
                                  "refs": self.refs, "comments": self.comments}), ""
        if argv[:3] == ["cadence", "audit", "verdicts"]:
            self.verdict_calls += 1
            if self.verdicts_after_at is not None and self.verdict_calls > self.verdicts_after_at:
                return 0, json.dumps(self.verdicts_after), ""
            vs = self.verdict_list if self.verdict_list is not None else [self.note()]
            return 0, json.dumps({"verdicts": vs, "skipped": []}), ""
        if argv[:3] == ["cadence", "audit", "approval"]:
            self.approval_calls += 1
            if not self.per_head:
                return 1, json.dumps({"state": "missing", "reason": "no merge approval"}), ""
            if (self.approval_missing_after is not None
                    and self.approval_calls > self.approval_missing_after):
                return 1, json.dumps({"state": "missing", "reason": "no merge approval"}), ""
            return 0, json.dumps({"state": "in-force", "approval_id": self.approval_id,
                                  "source": "op", "recorded_via": self.approval_via}), ""
        return super().run(argv)


class EvidencePosting(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="c1298")
        self.addCleanup(self._tmp.cleanup)
        self.fresh()

    def fresh(self):
        self.c = Case(self._tmp.name)

    def run_main(self, *extra):
        argv = ["5", "--head", HEAD, "--repo", REPO, "--notes-dir", self._tmp.name,
                "--poll-secs", "0", *extra]
        saved = enq.run, enq.pr_changes
        enq.run = self.c.run
        enq.pr_changes = lambda *a: ([("M", "100644", "100644", p) for p in self.c.paths], 40)
        out, err = io.StringIO(), io.StringIO()
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                rc = enq.main(argv)
        finally:
            enq.run, enq.pr_changes = saved
        return rc, out.getvalue(), err.getvalue()

    def assert_refused_clean(self, rc, err):
        self.assertEqual(rc, 1, err)
        self.assertEqual(self.c.posted, [], "a tracker comment was written")
        self.assertEqual(self.c.merges, [], "a merge command ran")

    # ---- dry run -----------------------------------------------------------

    def test_dry_run_checks_everything_and_writes_nothing(self):
        self.c.comments = [{"body": f"{HEAD} {NOTE}"}]
        rc, out, err = self.run_main("--dry-run")
        self.assertEqual(rc, 0, (out, err))
        self.assertIn("dry run", out)
        self.assertEqual(self.c.posted, [], "a tracker comment was written")
        self.assertEqual(self.c.merges, [], "a merge command ran")

    def test_dry_run_with_a_missing_comment_refuses_without_posting(self):
        rc, out, err = self.run_main("--dry-run")
        self.assertEqual(rc, 1, (out, err))
        self.assertIn("no ticket comment", err)
        self.assert_refused_clean(rc, err)

    # ---- the happy path ----------------------------------------------------

    def test_valid_human_proof_posts_one_evidence_comment_and_enqueues(self):
        rc, out, err = self.run_main()
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(self.c.posted), 1, self.c.posted)
        body = self.c.posted[0]
        for needle in (REPO, "#5", HEAD, BASE, NOTE, CI_URL, APPROVAL_ID):
            self.assertIn(needle, body, needle)
        self.assertEqual(len(self.c.merges), 1, self.c.merges)
        self.assertIn("--match-head-commit", self.c.merges[0])
        self.assertIn(HEAD, self.c.merges[0])

    def test_a_retry_posts_no_duplicate_comment(self):
        rc, out, err = self.run_main()
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(self.c.posted), 1)
        rc, out, err = self.run_main()  # same head: the canonical comment exists
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(self.c.posted), 1, self.c.posted)
        self.assertEqual(len(self.c.merges), 2)

    def test_auto_class_proof_posts_evidence_without_an_approval(self):
        self.c.paths = ["docs/x.md"]
        self.c.verdict_list = [
            self.c.mknote("20261003-100000-std-verdict.md", "20261003-100000", "rev-std", "standards"),
            self.c.mknote("20261003-100100-spec-verdict.md", "20261003-100100", "rev-spec",
                          "spec-security"),
        ]
        rc, out, err = self.run_main()
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(self.c.posted), 1)
        body = self.c.posted[0]
        for needle in (HEAD, BASE, CI_URL, "20261003-100000-std-verdict.md",
                       "20261003-100100-spec-verdict.md"):
            self.assertIn(needle, body, needle)
        self.assertNotIn(APPROVAL_ID, body)  # no approval required, none quoted
        self.assertEqual(self.c.approval_calls, 0, "an auto diff must not need approval")

    # ---- failed gates write nothing ---------------------------------------

    def test_missing_verdict_refuses_and_invents_no_pass(self):
        self.c.verdict_list = []
        rc, out, err = self.run_main()
        self.assertEqual(rc, 1, (out, err))
        self.assertIn("no pass verdict", err)
        self.assert_refused_clean(rc, err)

    def test_failed_required_check_refuses_without_writes(self):
        self.c.rollup = [{"name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)
        self.assertIn("required check", err)

    def test_forged_or_missing_approval_refuses_without_writes(self):
        for knob in ("forged", "missing"):
            with self.subTest(knob=knob):
                self.fresh()
                if knob == "forged":
                    self.c.approval_via = "agent:pm-x"
                else:
                    self.c.approval_missing_after = 0
                rc, out, err = self.run_main()
                self.assert_refused_clean(rc, err)
                self.assertIn("operator approval", err)

    def test_comment_write_failure_refuses_before_enqueue(self):
        self.c.comment_rc = 1
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)
        self.assertIn("evidence comment", err)

    # ---- the green CI run --------------------------------------------------

    def test_green_ci_run_missing_foreign_or_malformed_refuses(self):
        variants = {
            "empty": [],
            "foreign head": [dict(CI_RUN, head_sha="e" * 40,
                                  html_url=f"https://github.com/{REPO}/actions/runs/999")],
            "non-pull_request": [dict(CI_RUN, event="push")],
            "incomplete": [dict(CI_RUN, status="in_progress", conclusion=None)],
            "failed": [dict(CI_RUN, conclusion="failure")],
            "string id": [dict(CI_RUN, id="12345")],
            "url mismatch": [dict(CI_RUN, html_url="https://evil.example/runs/12345")],
            "no list": {"workflow_runs": "nope"},
        }
        for name, runs in variants.items():
            with self.subTest(variant=name):
                self.fresh()
                self.c.ci_runs = runs
                rc, out, err = self.run_main()
                self.assert_refused_clean(rc, err)
        self.fresh()
        self.c.ci_rc = 1
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)

    # ---- staleness ---------------------------------------------------------

    def test_stale_head_or_base_before_the_post_writes_nothing(self):
        for fields in ({"headRefOid": "c" * 40}, {"baseRefOid": "d" * 40},
                       {"state": "CLOSED"}, {"autoMergeRequest": {"enabledAt": "x"}}):
            with self.subTest(fields=fields):
                self.fresh()
                self.c.stale_at, self.c.stale_fields = 2, fields  # view 2 is the post check
                rc, out, err = self.run_main()
                self.assert_refused_clean(rc, err)
                self.assertIn("changed during evidence collection", err)

    def test_stale_head_or_base_after_the_post_refuses_the_enqueue(self):
        # View 3+ is the full re-evaluation after the comment was written.
        for fields in ({"headRefOid": "c" * 40}, {"baseRefOid": "d" * 40},
                       {"autoMergeRequest": {"enabledAt": "x"}}):
            with self.subTest(fields=fields):
                self.fresh()
                self.c.stale_at, self.c.stale_fields = 3, fields
                rc, out, err = self.run_main()
                self.assertEqual(rc, 1, (out, err))
                self.assertEqual(len(self.c.posted), 1)   # the write happened
                self.assertEqual(self.c.merges, [], "a stale head was enqueued")

    # ---- full re-evaluation after posting ----------------------------------

    def test_gates_are_reevaluated_after_the_post(self):
        for knob in ("verdicts", "approval", "rollup"):
            with self.subTest(knob=knob):
                self.fresh()
                if knob == "verdicts":
                    self.c.verdicts_after_at = 1
                    self.c.verdicts_after = {"verdicts": [], "skipped": []}
                elif knob == "approval":
                    self.c.approval_missing_after = 1
                else:
                    self.c.rollup_flip_at = 3
                rc, out, err = self.run_main()
                self.assertEqual(rc, 1, (out, err))
                self.assertEqual(len(self.c.posted), 1)
                self.assertEqual(self.c.merges, [], "a gate that failed late was enqueued")

    # ---- canonical body, zero-review floor ---------------------------------

    def test_active_routine_native_readiness_enqueues_without_a_verdict(self):
        self.c.paths = ["README.md"]
        self.c.verdict_list = []
        self.c.requirements_override = self.c.active_routine_authority()
        rc, out, err = self.run_main()
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(self.c.posted.__len__(), 1)
        self.assertIn("Outcome report: CAD-1298/reports/worker-done.md", self.c.posted[0])
        self.assertIn(f"Native CI run: {RUN_ID} {CI_URL}", self.c.posted[0])
        self.assertIn("Verdict notes: none required", self.c.posted[0])
        self.assertEqual(self.c.merges.__len__(), 1)
        self.assertEqual(self.c.approval_calls, 0)

    def test_routine_without_native_readiness_refuses_and_writes_nothing(self):
        self.c.paths = ["README.md"]
        self.c.requirements_override = self.c.active_routine_authority(readiness=False)
        rc, out, err = self.run_main()
        self.assert_refused_clean(rc, err)
        self.assertIn("no fresh native readiness proof", err)

    def test_evidence_body_with_no_counted_notes_fabricates_none(self):
        body = enq.evidence_body({"issue": ISSUE, "repo": REPO, "pr": 5, "head": HEAD,
                                  "base": BASE, "ci_run": CI_URL, "checks": ["ci"],
                                  "notes": [], "comments": []})
        self.assertIn(HEAD, body)
        self.assertIn(CI_URL, body)
        self.assertIn("none", body.lower())
        self.assertNotIn("pass", body.lower().replace("compass", ""))
        self.assertNotIn(APPROVAL_ID, body)


if __name__ == "__main__":
    unittest.main()
