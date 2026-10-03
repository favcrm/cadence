#!/usr/bin/env python3
"""CAD-1106: enqueue-reviewed accepts a live ticket SCOPE approval in place of a
per-head merge approval for an in-scope human-class PR, and only then.

Each test drives `evaluate` end to end against a fake `run` that answers like
gh, git and cadence do. The fake `cadence audit scope` filters a fake approval
store the way the real verb does (per issue, `revoked` from the revocation
list, `recorded_via` as stored), and the digest is computed here, independently
of the script, as sha256 of the ticket body (the daemon's scope_digest).
Set ENQ_SCRIPT to run the tests against a mutated copy of the script."""
import hashlib
import importlib.machinery
import importlib.util
import json
import os
import tempfile
import types
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = os.environ.get("ENQ_SCRIPT") or str(ROOT / "scripts/enqueue-reviewed")
loader = importlib.machinery.SourceFileLoader("enq_scope", SCRIPT)
spec = importlib.util.spec_from_loader("enq_scope", loader)
enq = importlib.util.module_from_spec(spec)
loader.exec_module(enq)

HEAD = "a" * 40
BASE = "b" * 40
ISSUE = "CAD-1106"
BRANCH = "cc13/cad-1106-scope-approval"
BODY = "## Goal\nApprove once per ticket.\nRisk: human (4, 7)\n"
OPERATOR = "operator-connection"
NOTE = "20261003-100000-cad1106-pr5-review-verdict.md"


def digest(body):
    return hashlib.sha256(body.strip().encode()).hexdigest()


def scope_rec(issue=ISSUE, body=BODY, via=OPERATOR, revoked=False, aid="scope-cad-1106-x"):
    return {"issue": issue, "approval_id": aid, "digest": digest(body), "source": "operator in chat",
            "recorded_via": via, "revoked": revoked}


class Case:
    """One PR scenario; every knob defaults to the accepted case."""

    def __init__(self, tmp):
        self.tmp = Path(tmp)
        self.paths = ["scripts/enqueue-reviewed", ".github/workflows/ci.yml"]
        self.title = f"{ISSUE}: approve once per ticket"
        self.branch = BRANCH
        self.ticket_body = BODY
        self.refs = [{"kind": "branch", "path": BRANCH}]
        self.status = "doing"
        self.store = [scope_rec()]
        self.note_text = (f"# Verdict: {ISSUE} Review (standards+spec) — pass\n> Issue: {ISSUE}\n> From: rev\n\n"
                          f"## Verdict\npass — PR #5, head {HEAD}\n\nRisk: human (4, 7) — scripts\n"
                          f"Scope: in-scope {ISSUE}\n")
        self.per_head = False
        self.scope_rc = 0
        self.scope_override = None  # a raw reply, to model a lying or broken verb

    def note(self):
        p = self.tmp / NOTE
        if self.note_text is not None:
            p.write_text(self.note_text)
        verdict = {"name": NOTE, "path": str(p), "stamp": "20261003-100000", "reviewer": "rev",
                   "kind": "combined", "outcome": "pass", "title_outcome": "pass",
                   "section_outcome": "pass", "risk": "human", "from": "rev"}
        return verdict

    def run(self, argv):
        if argv[:3] == ["gh", "pr", "view"]:
            return 0, json.dumps({
                "number": 5, "state": "OPEN", "title": self.title, "headRefName": self.branch,
                "headRefOid": HEAD, "baseRefName": "main", "baseRefOid": BASE,
                "mergeStateStatus": "CLEAN", "autoMergeRequest": None,
                "author": {"login": "cc"},
                "statusCheckRollup": [{"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}]}), ""
        if argv[:3] == ["gh", "repo", "view"]:
            return 0, json.dumps({"defaultBranchRef": {"name": "main"}}), ""
        if argv[:2] == ["gh", "api"]:
            url = argv[-1]
            if "/rules/branches/" in url:
                return 0, json.dumps([{"type": "required_status_checks", "parameters": {
                    "required_status_checks": [{"context": "ci"}]}}]), ""
            if url.endswith("/protection"):
                return 1, "", "Branch not protected"
            for f in (enq.ONE_REVIEW_FILE, enq.RISK_PATHS_FILE):  # the REAL lists
                if f"/contents/{f}?ref={BASE}" in url:
                    return 0, (ROOT / f).read_text(), ""
        if argv[:3] == ["cadence", "issue", "show"]:
            return 0, json.dumps({"id": argv[3], "owner": None, "claim": None, "body": self.ticket_body,
                                  "status": self.status, "refs": self.refs,
                                  "comments": [{"body": f"{HEAD} {NOTE}"}]}), ""
        if argv[:3] == ["cadence", "audit", "verdicts"]:
            return 0, json.dumps({"verdicts": [self.note()], "skipped": []}), ""
        if argv[:3] == ["cadence", "audit", "approval"]:
            if self.per_head:
                return 0, json.dumps({"state": "in-force", "approval_id": "merge-pr5-aaaa",
                                      "source": "op", "recorded_via": OPERATOR}), ""
            return 1, json.dumps({"state": "missing", "reason": "no merge approval"}), ""
        if argv[:3] == ["cadence", "audit", "scope"]:
            if self.scope_override is not None:
                return self.scope_rc, self.scope_override, ""
            issue = argv[argv.index("--issue") + 1]
            rows = [{k: v for k, v in r.items() if k != "issue"} for r in self.store
                    if r["issue"].lower() == issue.lower()]
            return self.scope_rc, json.dumps({"state": "ok", "issue": issue, "approvals": rows}), ""
        raise AssertionError(f"unexpected command {argv}")

    def evaluate(self):
        args = types.SimpleNamespace(pr=5, head=HEAD, author=[], notes_dir=str(self.tmp))
        saved = enq.run, enq.pr_changes
        enq.run = self.run
        enq.pr_changes = lambda *a: ([("M", "100644", "100644", p) for p in self.paths], 40)
        try:
            return enq.evaluate(args, "o/r")
        finally:
            enq.run, enq.pr_changes = saved


class ScopeApproval(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="c1106")
        self.addCleanup(self._tmp.cleanup)
        self.c = Case(self._tmp.name)

    def refused(self, why=None):
        reasons, report = self.c.evaluate()
        self.assertTrue(any("no operator approval recorded" in r for r in reasons), (reasons, report))
        self.assertFalse(any("scope approval in force" in r for r in report), report)
        if why:
            self.assertTrue(any("scope approval not used" in r and why in r for r in report), (why, report))

    def test_in_scope_scripts_diff_is_accepted_and_reports_the_approval(self):
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)
        used = [r for r in report if r.startswith("scope approval in force")]
        self.assertEqual(len(used), 1, report)
        self.assertIn("scope-cad-1106-x", used[0])

    def test_per_head_approval_still_works_without_a_scope_approval(self):
        self.c.per_head, self.c.store = True, []
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)
        self.assertTrue(any(r.startswith("approval in force") for r in report), report)

    def test_trigger_1_and_3_and_two_review_paths_need_a_per_head_approval(self):
        for path in ("ui/src/App.svelte", "src/peer.rs", "src/secret/vault.rs", "src/cli/secret.rs",
                     "src/daemon/identity.rs", "src/store/events.rs"):
            with self.subTest(path=path):
                self.c.paths = ["scripts/enqueue-reviewed", path]
                self.refused("trigger 1/3")

    def test_trigger_lists_unreadable_at_the_base_fail_closed(self):
        orig = self.c.run
        for name in ("risk-paths", "one-review-paths"):
            with self.subTest(list=name):
                self.c.run = lambda a, n=name: (1, "", "404") if a[:2] == ["gh", "api"] and n in a[-1] else orig(a)
                self.refused("cannot read the trigger 1/3 lists")

    def test_trigger_1_or_3_named_by_any_verdict_risk_line_needs_a_per_head_approval(self):
        for risk in ("human (1, 4)", "human (3)", "human (4, 7, 3)", "human (trigger 1)"):
            with self.subTest(risk=risk):
                self.c.note_text = self.c.note_text.replace("human (4, 7) — scripts", f"{risk} — x")
                self.refused("Risk trigger")

    def test_unparseable_or_missing_risk_class_fails_closed(self):
        for risk in ("Risk: human", "Risk: human (see ticket)", "Risk: human (1-4)", "Risk: delegated (4)", ""):
            with self.subTest(risk=risk):
                self.c.note_text = self.c.note_text.replace("Risk: human (4, 7) — scripts", risk)
                self.refused("avoids Risk triggers 1 and 3")

    def test_stale_digest_after_a_ticket_edit_is_refused(self):
        self.c.ticket_body = BODY + "Also: ship the world.\n"
        self.refused("matches the ticket as it reads now")

    def test_digest_matches_the_daemon_trim_not_pythons(self):
        # Rust's trim() keeps U+001C; Python's str.strip() would drop it.
        body = BODY.rstrip("\n") + "\u001c"
        self.c.ticket_body = body
        self.c.store = [{**scope_rec(), "digest": hashlib.sha256(body.encode()).hexdigest()}]
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)
        # NBSP and trailing newlines ARE trimmed by both.
        self.c.ticket_body = BODY + "\u00a0\n"
        self.c.store = [{**scope_rec(), "digest": digest(BODY)}]
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)

    def test_scope_approval_of_another_ticket_is_refused(self):
        self.c.store = [scope_rec(issue="CAD-1102", body=BODY)]  # same digest, other ticket
        self.refused("no live (unrevoked) scope approval")
        self.c.store = [scope_rec()]
        self.c.title = "CAD-1107: another ticket"  # title names a ticket with no approval
        self.refused()

    def test_a_reply_for_another_issue_is_refused(self):
        self.c.scope_override = json.dumps({"state": "ok", "issue": "CAD-1102", "approvals": [
            {"approval_id": "s", "digest": digest(BODY), "recorded_via": OPERATOR, "revoked": False}]})
        self.refused("not " + ISSUE)

    def test_forged_or_agent_recorded_approval_is_refused(self):
        for via in (None, "agent:cc13-pm", "delegated:pm-d", "operator", "daemon"):
            with self.subTest(via=via):
                self.c.store = [scope_rec(via=via)]
                self.refused("no live scope approval")

    def test_revoked_approval_is_refused(self):
        self.c.store = [scope_rec(revoked=True)]
        self.refused("no live (unrevoked)")

    def test_unreadable_scope_store_fails_closed(self):
        self.c.scope_rc, self.c.scope_override = 1, json.dumps({"state": "unknown", "reason": "no store"})
        self.refused("cannot read scope approvals")
        # Even a reply that says `unknown` yet carries a matching row is no answer.
        self.c.scope_rc = 0
        self.c.scope_override = json.dumps({"state": "unknown", "issue": ISSUE, "approvals": [
            {"approval_id": "s", "digest": digest(BODY), "recorded_via": OPERATOR, "revoked": False}]})
        self.refused("cannot read scope approvals")
        self.c.scope_rc, self.c.scope_override = 127, "not json"
        self.refused("cannot read scope approvals")

    def test_unrecorded_or_closed_lane_branch_is_refused(self):
        self.c.branch = "someone/else"
        self.refused("not an open lane branch")
        self.c.branch, self.c.refs = BRANCH, [{"kind": "branch", "path": BRANCH, "closed": True}]
        self.refused("not an open lane branch")
        self.c.refs = []
        self.refused("not an open lane branch")

    def test_verdict_not_stating_in_scope_is_refused(self):
        for line in ("", f"Scope: out-of-scope {ISSUE}\n", "Scope: in-scope CAD-1\n"):
            with self.subTest(line=line):
                self.c.note_text = self.c.note_text.replace(f"Scope: in-scope {ISSUE}\n", line)
                self.refused("does not state")

    def set_risk(self, line):
        self.c.note_text = self.c.note_text.replace("Risk: human (4, 7) — scripts", line)

    def test_undeclared_path_triggers_need_a_per_head_approval(self):
        # A trigger-7 (docs) ticket must not cover trigger 4 (supply chain) or 6 (rollout) paths.
        self.c.ticket_body = "## Goal\ndocs\nRisk: human (7)\n"
        self.c.store = [scope_rec(body=self.c.ticket_body)]
        self.set_risk("Risk: human (7) — docs")
        self.c.paths = ["docs/AUDIT.md"]
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)  # sanity: in scope when only 7 is hit
        for paths, why in ((["docs/AUDIT.md", "Cargo.toml"], [4]), (["docs/AUDIT.md", "src/rollout.rs"], [6]),
                           (["docs/AUDIT.md", ".github/workflows/release.yml", "src/update.rs"], [4, 6]),
                           (["docs/AUDIT.md", "src/store/schema.rs"], [2])):
            with self.subTest(paths=paths):
                self.c.paths = paths
                self.refused(f"does not declare (declared [7])")
                self.assertTrue(any(f"hits trigger(s) {why}" in r for r in self.c.evaluate()[1]))

    def test_undeclared_verdict_trigger_needs_a_per_head_approval(self):
        self.c.ticket_body = "Risk: human (7)\n"
        self.c.store = [scope_rec(body=self.c.ticket_body)]
        self.c.paths = ["docs/AUDIT.md"]
        self.set_risk("Risk: human (7, 6) — x")
        self.refused("names Risk trigger(s) [6] the ticket does not declare")

    def test_ticket_without_a_parseable_risk_line_has_no_scope(self):
        for body in ("## Goal\nno risk line\n", "Risk: human\n", "Risk: human (see below)\n", "Risk: auto\n",
                     "Risk: human (4, 7, 1)\n", "Risk: human (3)\n"):
            with self.subTest(body=body):
                self.c.ticket_body = body
                self.c.store = [scope_rec(body=body)]
                self.refused()

    def test_human_path_in_no_risk_list_needs_a_per_head_approval(self):
        for path in ("config/production-baseline.json", "tests/safety_floor.rs", "apps/x/package-lock.json"):
            with self.subTest(path=path):
                self.assertTrue(enq.is_human_path(path, ()))
                self.c.paths = ["scripts/enqueue-reviewed", path]
                self.refused("in no risk trigger list")

    def test_trigger_1_or_3_anywhere_on_a_risk_line_is_refused(self):
        for risk in ("human (4, 7) and trigger 1 (identity)", "human (4, 7) (triggers 4 and 3)",
                     "human (4, 7) — see (3)", "human (4, 7) (identity 1)"):
            with self.subTest(risk=risk):
                self.set_risk("Risk: " + risk)
                self.refused("Risk trigger")
                self.c.note_text = self.c.note_text.replace("Risk: " + risk, "Risk: human (4, 7) — scripts")

    def test_risk_prose_without_trigger_numbers_is_fine(self):
        self.set_risk("Risk: human (4, 7) — scripts, 1500 lines (PR #773, CAD-1106)")
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)

    def test_only_ready_doing_review_tickets_have_scope(self):
        for status in ("dropped", "done", "backlog", None, "cancelled", "Doing", ""):
            with self.subTest(status=status):
                self.c.status = status
                self.refused("status is")
        for status in ("ready", "doing", "review"):
            with self.subTest(status=status):
                self.c.status = status
                reasons, report = self.c.evaluate()
                self.assertEqual(reasons, [], report)

    def test_ticket_risk_line_in_a_fence_is_ignored_and_two_lines_mean_no_scope(self):
        self.c.paths, self.c.ticket_body = ["src/rollout.rs"], "```\nRisk: human (6, 7)\n```\nRisk: human (7)\n"
        self.c.store = [scope_rec(body=self.c.ticket_body)]
        self.set_risk("Risk: human (7) — x")
        self.refused("does not declare (declared [7])")  # the fenced (6) is not declared
        self.c.paths = ["docs/AUDIT.md"]
        reasons, report = self.c.evaluate()
        self.assertEqual(reasons, [], report)
        for body in ("Risk: human (7)\nRisk: human (1, 4)\n", "Risk: human (7)\n```\nx\n```\nRisk: human (7)\n"):
            with self.subTest(body=body):
                self.c.ticket_body = body
                self.c.store = [scope_rec(body=body)]
                self.refused("declares no parseable")

    def test_verdict_with_two_risk_lines_needs_a_per_head_approval(self):
        for extra in ("Risk: human (1)\n", "Risk: auto\n", "Risk: human (4, 7)\n"):
            with self.subTest(extra=extra):
                self.c.note_text = "Risk: auto\n" + self.c.note_text.replace("Risk: human (4, 7) — scripts", extra)
                self.refused("avoids Risk triggers 1 and 3")

    def test_unreadable_verdict_note_fails_closed(self):
        self.c.note_text = None
        self.refused()


if __name__ == "__main__":
    unittest.main()
