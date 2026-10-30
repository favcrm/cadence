#!/usr/bin/env python3
"""CAD-1946 v0: enqueue-reviewed runs with no Cadence daemon.

Independent of the daemon, the script must still refuse a PR that lacks the
review floor, the operator approval or a green CI run, and it must never turn
an unavailable verb into a pass. These cases model the hosted org the local
daemon is retired from:

  * `cadence delivery requirements` refuses (the verb is not available on a
    remote org). The legacy strict floor applies — one review off the
    one-review list read at the base, else two; the operator check on a
    human-class diff; the full CI floor — and the run says so.
  * `cadence audit approval` refuses too, so the operator approval is the
    signed PR comment verified against the allowed_signers read at the base
    (`scripts/delivery-gate approve`). A missing, unsigned, edited-Source or
    foreign-head approval refuses.
  * `cadence audit scope` is never consulted: the scope fallback needed the
    daemon's store, so v0 always requires the per-head approval.

Only the external transports (gh, git diff evidence, cadence) are faked. The
real main/evaluate/check_approval/check_signed_approval and the real
scripts/delivery-gate sign/verify run.

The scope fixture is imported so both suites share one transport model; this
module only adds the daemonless endpoints.
"""
import importlib.machinery
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
scope = importlib.machinery.SourceFileLoader(
    "scope_fixture", str(ROOT / "scripts" / "test_enqueue_scope_approval.py")
).load_module()
enq = scope.enq

HEAD = scope.HEAD
BASE = scope.BASE
ISSUE = scope.ISSUE
NOTE = scope.NOTE
CI_URL = scope.CI_URL

# The refusal a hosted org answers; stdout carries no JSON document.
UNAVAILABLE = "cmd: options.a: --json requires local state; not available on a remote org"
SOURCE = "chris in chat 2026-10-30T03:10Z"


class Daemonless(scope.Case):
    """A hosted org: no daemon verbs, and the approval is a signed comment."""

    def __init__(self, tmp):
        super().__init__(tmp)
        self.per_head = False
        self.approval_comments = []
        self.no_verdicts = False
        self.risk_rc = 0
        self.signers = ""
        self.scope_calls = 0
        # One combined PASS pinned to HEAD, on the one-review list.
        self.paths = ["scripts/enqueue-reviewed"]
        self.note_text = (
            f"# Verdict: {ISSUE} Review (standards+spec) — pass\n> Issue: {ISSUE}\n> From: rev\n\n"
            f"## Verdict\npass — PR #5, head {HEAD}\n\nRisk: human (4, 7) — scripts\n"
            f"Scope: in-scope {ISSUE}\n")

    def signed_approval(self, head=HEAD, source=SOURCE):
        """A real signature from scripts/delivery-gate approve."""
        return subprocess.run(
            [sys.executable, str(enq.GATE_SCRIPT), "approve", "--repo", "o/r", "--pr", "5",
             "--head", head, "--key", self.key, "--source", source],
            capture_output=True, text=True)

    def run(self, argv, input_text=None):
        # The nested verify must reach the real script, not this fake.
        if enq.GATE_SCRIPT in argv:
            p = subprocess.run(argv, capture_output=True, text=True)
            return p.returncode, p.stdout, p.stderr
        if argv[:3] == ["cadence", "delivery", "requirements"]:
            return 3, "", UNAVAILABLE
        if argv[:3] == ["cadence", "audit", "approval"]:
            return 3, "", UNAVAILABLE
        if argv[0] == "cadence" and "audit" in argv and "verdicts" in argv:
            # A saved remote org routes `audit` to the hosted transport, which
            # has no such verb; `--org local` is the same file read.
            if "--org" not in argv:
                return 3, "", UNAVAILABLE
            return 0, json.dumps({"verdicts": [] if self.no_verdicts else [self.note()],
                                  "skipped": []}), ""
        if argv[:3] == ["cadence", "audit", "scope"]:
            self.scope_calls += 1
            return 3, "", UNAVAILABLE
        if argv[:2] == ["gh", "api"]:
            url = next((a for a in argv if a.startswith("repos/")), "")
            if f"/contents/{enq.SIGNERS_FILE}?ref={BASE}" in url:
                return 0, self.signers, ""
            if "/issues/5/comments" in url:
                return 0, json.dumps([self.approval_comments]), ""
            if f"/contents/{enq.RISK_PATHS_FILE}?ref=" in url and self.risk_rc != 0:
                return self.risk_rc, "", "Not Found"
            if "/issues/5/comments" in url:
                return 0, json.dumps([self.approval_comments]), ""
        return super().run(argv)

    def evaluate(self, require_comment=False, daemonless=True):
        """The pre-post pass `main()` runs; the comment gate is its own phase."""
        args = types.SimpleNamespace(pr=5, head=HEAD, author=[], notes_dir=str(self.tmp),
                                     daemonless=daemonless)
        saved = enq.run, enq.pr_changes
        enq.run = self.run
        enq.pr_changes = lambda *a: ([("M", "100644", "100644", p) for p in self.paths], 40)
        try:
            return enq.evaluate(args, "o/r", require_comment=require_comment)
        finally:
            enq.run, enq.pr_changes = saved


class DaemonlessMerge(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="c1946v0")
        self.addCleanup(self._tmp.cleanup)
        self.c = Daemonless(self._tmp.name)
        # One real key, and the signers file that publishes it — the same shape
        # the queue reads from the base commit.
        key = os.path.join(self._tmp.name, "key")
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", key], check=True)
        self.c.key = key
        pub = Path(key + ".pub").read_text().split()[1]
        self.c.signers = f'cadence-delivery-gate namespaces="cadence-delivery-gate" ssh-ed25519 {pub}\n'

    def approval(self, head=HEAD, source=SOURCE):
        out = self.c.signed_approval(head=head, source=source)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.c.approval_comments = [{"id": 900, "body": out.stdout}]

    def refused(self, needle):
        reasons, report = self.c.evaluate()
        self.assertTrue(any(needle in r for r in reasons), (reasons, report))
        return reasons, report

    # -- the requirements verb is gone, the floor is not --------------------

    def test_the_unavailable_verb_uses_the_legacy_strict_floor(self):
        self.approval()
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)
        self.assertTrue(any("no daemon answer" in r and "legacy strict floor" in r
                            for r in report), report)
        self.assertFalse(any("authoritative" in r for r in reasons), reasons)

    def test_the_one_review_list_still_applies_at_the_base(self):
        """One combined PASS covers a one-review path; it does not cover two."""
        self.approval()
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)
        self.assertTrue(any(r.startswith("reviews required: one") for r in report), report)
        self.c.paths = ["src/peer.rs"]  # trigger 1: two reviews, never one
        reasons, _ = self.c.evaluate()
        self.assertTrue(any("two independent reviews" in r or "Spec/security" in r
                            for r in reasons), reasons)

    def test_a_non_default_base_is_refused_without_a_daemon(self):
        """The daemon also refused a base that was not the default or an approved target."""
        self.approval()
        self.c.base_ref = "release/next"
        reasons, _ = self.c.evaluate()
        self.assertTrue(any("is not the repository default" in r for r in reasons), reasons)

    # -- the operator approval is the signed comment ------------------------

    def test_a_signed_approval_for_this_head_is_accepted(self):
        self.approval()
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)
        self.assertTrue(any(r.startswith("approval in force: signed operator approval")
                            and HEAD in r for r in report), report)

    def test_no_approval_comment_refuses(self):
        self.c.approval_comments = []
        self.refused("no operator approval")

    def test_an_unsigned_approval_comment_refuses(self):
        self.c.approval_comments = [{"id": 900,
                                     "body": f"<!-- cadence-operator-approval v1 -->\nSource: {SOURCE}\n"}]
        self.refused("carried no signature that verifies")

    def test_an_edited_source_line_refuses(self):
        """Source is inside the signed payload, so editing it voids the approval."""
        self.approval()
        body = self.c.approval_comments[0]["body"]
        self.c.approval_comments = [{"id": 900,
                                     "body": body.replace(SOURCE, "someone else in chat")}]
        self.refused("carried no signature that verifies")

    def test_an_approval_for_another_head_refuses(self):
        self.approval(head="c" * 40)
        self.refused("no operator approval")

    def test_the_queue_signature_never_stands_in_for_an_approval(self):
        """A queue signature signs different bytes; the approval marker does not change that."""
        sign = subprocess.run([sys.executable, str(enq.GATE_SCRIPT), "sign", "--repo", "o/r",
                               "--pr", "5", "--head", HEAD, "--key", self.c.key],
                              capture_output=True, text=True)
        self.assertEqual(sign.returncode, 0, sign.stderr)
        # Same comment shape the approval has, so the marker filter lets it through
        # and only the cryptography decides.
        body = (f"<!-- cadence-operator-approval v1 -->\nSource: {SOURCE}\n\n"
                + "\n".join(sign.stdout.splitlines()[1:]))
        self.c.approval_comments = [{"id": 901, "body": body}]
        self.refused("carried no signature that verifies")

    # -- the fallback is opt-in, and keeps the native floors ----------------

    def test_without_the_flag_an_unanswerable_verb_refuses(self):
        """A working daemon's refusal must never become a silent downgrade."""
        self.approval()
        reasons, _ = self.c.evaluate(daemonless=False)
        self.assertTrue(any("did not answer here" in r for r in reasons), reasons)

    def test_an_unreadable_risk_manifest_keeps_two_reviews(self):
        """Native parsed both lists together; an unreadable one keeps two reviews."""
        self.approval()
        self.c.risk_rc = 1
        reasons, report = self.c.evaluate()
        self.assertTrue(any("Spec/security" in r for r in reasons), (reasons, report))

    def test_the_verdict_read_retries_the_local_org(self):
        self.approval()
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)
        self.assertTrue(any(r.startswith("counted verdicts: rev") for r in report), report)

    def test_a_missing_review_refuses(self):
        self.approval()
        self.c.no_verdicts = True
        reasons, _ = self.c.evaluate()
        self.assertTrue(any("verdict" in r for r in reasons), reasons)

    def test_an_absent_or_red_ci_run_refuses(self):
        self.approval()
        self.c.ci_runs = []
        reasons, _ = self.c.evaluate()
        self.assertTrue(any("ci.yml" in r or "not a completed success" in r for r in reasons), reasons)

    # -- the scope fallback is gone without a daemon ------------------------

    def test_scope_approval_is_never_consulted_without_a_daemon(self):
        self.approval()
        self.c.evaluate()
        self.assertEqual(self.c.scope_calls, 0)
        self.c.approval_comments = []
        self.refused("no operator approval")
        self.assertEqual(self.c.scope_calls, 0)


if __name__ == "__main__":
    unittest.main()
