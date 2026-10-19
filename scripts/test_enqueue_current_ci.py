#!/usr/bin/env python3
"""CAD-1598: the guarded enqueue judges the CURRENT authoritative required CI
attempt for the exact reviewed head.

A superseded cancelled/failed draft attempt must not poison a newer successful
ready attempt. A newer genuine failure, pending, missing, untrusted (other
app), other-workflow, malformed, ambiguous, truncated or mis-bound attempt must
still refuse even when an older attempt succeeded. An older run that was
re-run after a newer success is judged by its current attempt, not its
creation time.

Independent acceptance check: the expected outcomes come from the CAD-1598
ticket and the observed GitHub metadata of AgenticOS-v2 PR 747, not from
scripts/enqueue-reviewed. The real main/evaluate/run path runs; only the
external transports are faked: gh (pr view, pr merge, api, api graphql, repo
view), cadence (issue show/comment, audit verdicts/approval, delivery
requirements) and git diff via pr_changes. Any unserved command or API
endpoint raises, so the test fails rather than passing silently. Nothing is
enqueued, posted or merged for real.

Transport shapes served (per the producer interface notice):
 * rules `required_status_checks[].integration_id` binds the required name to
   the GitHubActions app (15368) in both matching rulesets;
 * `repos/R/commits/SHA/check-runs?check_name=NAME&filter=all&per_page=100`
   returns {total_count, check_runs:[{id,name,head_sha,status,conclusion,
   started_at,completed_at,app:{id,slug},check_suite:{id},details_url}]};
   total_count greater than the returned list is served as truncated;
 * `repos/R/actions/workflows/ci.yml/runs?head_sha=SHA&event=pull_request`
   (no status filter) returns {total_count, workflow_runs:[{id,head_sha,event,
   status,conclusion,html_url,check_suite_id,run_attempt,run_started_at,...}]};
   like GitHub, one entry per run id carrying its LATEST attempt's status,
   and created_at is the run's original creation time; runs of other
   workflows never appear in it;
 * commit statuses are StatusContext rollup entries and never satisfy a
   required check-run name.

Re-run modelling (assumption to verify against Main's observed reruns): a
re-run keeps the run id, check_suite id and created_at, bumps run_attempt and
sets run_started_at to the re-run start. Its gate check run gets a new id once
emitted; until then the previous attempt's gate record remains listed.

Run, job, suite and app ids, times, the ci.yml workflow id and the required
gate name are the PR 747 values from the observed API metadata. Base oid,
notes, ticket and policy digest are synthetic. This is a producer-shaped fake,
not native GitHub qualification.

Set ENQ_SCRIPT to run the tests against a mutated copy of the script.
"""
import contextlib
import copy
import importlib.machinery
import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = os.environ.get("ENQ_SCRIPT") or str(ROOT / "scripts/enqueue-reviewed")
loader = importlib.machinery.SourceFileLoader("enq_current_ci", SCRIPT)
spec = importlib.util.spec_from_loader("enq_current_ci", loader)
enq = importlib.util.module_from_spec(spec)
loader.exec_module(enq)

REPO = "agenticos-stack/agenticos-v2"
PR = 747
HEAD = "3d1023c036c10276e2c0e7a10443a008d61afd61"
STALE_HEAD = "c" * 40
BASE = "b" * 40  # synthetic base commit oid
BASE_REF = "staging"
BRANCH = "cadence/aos-293-pr747"  # synthetic lane branch
ISSUE = "AOS-293"
PROJECT = "agenticos-v2"
AUTHOR = "aos-author"
GATE = "Required result gate"
CI_WORKFLOW_ID = 362543015
CI_PATH = ".github/workflows/ci.yml"
OTHER_PATH = ".github/workflows/other.yml"
TRUSTED_APP = {"id": 15368, "slug": "github-actions"}
UNTRUSTED_APP = {"id": 99999, "slug": "some-other-app"}
APPROVAL_ID = "merge-pr747-3d1023c036c1"
NOTE_STD = "20261019-093000-aos293-pr747-standards-verdict.md"
NOTE_SPEC = "20261019-094000-aos293-pr747-spec-security-verdict.md"
OPERATOR = "operator-connection"
POLICY = "sha256:" + "ab" * 32


def ts(hms):
    return f"2026-10-19T{hms}Z"


def run_rec(run_id, suite, created, *, status="completed", conclusion="success", updated=None,
            head=HEAD, workflow_id=CI_WORKFLOW_ID, path=CI_PATH, attempt=1, started=None):
    """A workflow run as GET repos/R/actions/workflows/ci.yml/runs lists it."""
    repo_url = f"https://api.github.com/repos/{REPO}"
    return {
        "id": run_id, "workflow_id": workflow_id, "head_sha": head, "event": "pull_request",
        "status": status, "conclusion": conclusion, "run_attempt": attempt,
        "created_at": created, "updated_at": updated or created, "run_started_at": started or created,
        "html_url": f"https://github.com/{REPO}/actions/runs/{run_id}", "path": path,
        "check_suite_id": suite,
        "pull_requests": [{"number": PR,
                           "head": {"sha": head, "ref": BRANCH, "repo": {"url": repo_url}},
                           "base": {"ref": BASE_REF, "sha": BASE, "repo": {"url": repo_url}}}],
    }


def rerun_rec(original, *, started, status, conclusion, updated=None):
    """The same run id after a re-run: latest attempt, original created_at kept."""
    rec = copy.deepcopy(original)
    rec.update({"run_attempt": original["run_attempt"] + 1, "run_started_at": started,
                "status": status, "conclusion": conclusion, "updated_at": updated or started})
    return rec


def gate_rec(job_id, run_id, suite, *, started, completed, status="completed",
             conclusion="success", head=HEAD, app=TRUSTED_APP, name=GATE):
    """A check run as GET repos/R/commits/SHA/check-runs lists it."""
    url = f"https://github.com/{REPO}/actions/runs/{run_id}/job/{job_id}"
    return {"id": job_id, "name": name, "head_sha": head, "status": status,
            "conclusion": conclusion, "started_at": started, "completed_at": completed,
            "details_url": url, "check_suite": {"id": suite}, "app": app}


def malformed(gate):
    """Same check run with no check_suite and no app: unbindable to a run or app."""
    gate = dict(gate)
    del gate["check_suite"]
    gate["app"] = None
    return gate


def status_success():
    """A same-named commit status (StatusContext) from no Actions app."""
    return {"__typename": "StatusContext", "context": GATE, "state": "SUCCESS",
            "startedAt": ts("09:58:00"), "targetUrl": "https://status.example.invalid/gate"}


def rollup_entry(c):
    """gh pr view --json statusCheckRollup CheckRun shape."""
    conclusion = c["conclusion"]
    return {"__typename": "CheckRun", "name": c["name"], "status": c["status"].upper(),
            "conclusion": conclusion.upper() if conclusion else None,
            "startedAt": c["started_at"], "completedAt": c["completed_at"],
            "detailsUrl": c["details_url"]}


# Observed PR 747 attempts (ids, times and app from the GitHub API metadata).
OLD_RUN, OLD_JOB, OLD_SUITE = 39231020958, 117989954486, 106304242044
NEW_RUN, NEW_JOB, NEW_SUITE = 39231256712, 117992467504, 106304880676
NEW_URL = f"https://github.com/{REPO}/actions/runs/{NEW_RUN}"
OLD_URL = f"https://github.com/{REPO}/actions/runs/{OLD_RUN}"


def observed_draft():
    """Superseded cancelled draft attempt; its same-head gate is FAILURE."""
    return (run_rec(OLD_RUN, OLD_SUITE, ts("09:33:57"), conclusion="cancelled", updated=ts("09:37:10")),
            gate_rec(OLD_JOB, OLD_RUN, OLD_SUITE, conclusion="failure",
                     started=ts("09:37:02"), completed=ts("09:37:09")))


def observed_ready():
    """Current ready attempt at the same head; its gate is SUCCESS."""
    return (run_rec(NEW_RUN, NEW_SUITE, ts("09:35:50"), updated=ts("09:43:23")),
            gate_rec(NEW_JOB, NEW_RUN, NEW_SUITE, started=ts("09:43:14"), completed=ts("09:43:22")))


def newer_failure():
    return (run_rec(39231300000, 106305000000, ts("09:52:00"), conclusion="failure", updated=ts("09:52:40")),
            gate_rec(117993000000, 39231300000, 106305000000, conclusion="failure",
                     started=ts("09:52:10"), completed=ts("09:52:40")))


def newer_pending():
    return (run_rec(39231300500, 106305100000, ts("09:53:00"), status="in_progress", conclusion=None),
            gate_rec(117993100000, 39231300500, 106305100000, status="in_progress", conclusion=None,
                     started=ts("09:53:05"), completed=None))


def newest_without_gate():
    """A newer ci.yml attempt that completed successfully but has no required gate."""
    return (run_rec(39231301000, 106305200000, ts("09:54:00"), updated=ts("09:58:00")), None)


def newer_untrusted_app():
    return (run_rec(39231302000, 106305300000, ts("09:55:00"), updated=ts("09:55:30")),
            gate_rec(117993300000, 39231302000, 106305300000, started=ts("09:55:10"),
                     completed=ts("09:55:30"), app=UNTRUSTED_APP))


def other_workflow_success():
    """A different workflow's same-named SUCCESS gate at the head; ci.yml never lists it."""
    return (run_rec(39231303000, 106305400000, ts("09:56:00"), updated=ts("09:56:30"),
                    path=OTHER_PATH, workflow_id=362543999),
            gate_rec(117993400000, 39231303000, 106305400000, started=ts("09:56:10"),
                     completed=ts("09:56:30")))


def newer_malformed():
    return (run_rec(39231305000, 106305600000, ts("09:59:00"), updated=ts("09:59:30")),
            malformed(gate_rec(117993600000, 39231305000, 106305600000, started=ts("09:59:10"),
                               completed=ts("09:59:30"))))


def newer_gate_bound_to_stale_head_run():
    """A newer HEAD gate whose check_suite belongs to a run at another head."""
    return (run_rec(39231304000, 106305500000, ts("09:57:00"), head=STALE_HEAD, updated=ts("09:57:30")),
            gate_rec(117993500000, 39231304000, 106305500000, started=ts("09:57:10"),
                     completed=ts("09:57:30")))


class Case:
    """One PR 747 scenario over the guard's external transport."""

    def __init__(self, tmp):
        self.tmp = Path(tmp)
        self.paths = ["config/aos293-fixture.toml"]
        self.title = f"{ISSUE}: resume PR {PR} after the CI selection repair"
        self.runs = []
        self.checks = []
        self.statuses = []
        self.rollup_reversed = False
        self.hidden_checks = 0  # extra same-name check runs the endpoint reports but does not list
        self.after_post = None  # attempt that appears once the evidence comment is posted
        self.comments = []
        self.posted = []
        self.merges = []
        self.merge_rc = 0
        self.verdicts = [self.verdict(NOTE_STD, "20261019-093000", "std-reviewer", "standards"),
                         self.verdict(NOTE_SPEC, "20261019-094000", "spec-reviewer", "spec-security")]

    def verdict(self, name, stamp, reviewer, kind):
        (self.tmp / name).write_text(
            f"# Verdict: {ISSUE} {kind} — pass\n> Issue: {ISSUE}\n> From: {reviewer}\n\n"
            f"## Verdict\npass — PR #{PR}, head {HEAD}\n")
        return {"name": name, "path": str(self.tmp / name), "stamp": stamp, "reviewer": reviewer,
                "kind": kind, "outcome": "pass", "title_outcome": "pass",
                "section_outcome": "pass", "risk": "auto", "from": reviewer}

    def add(self, run_, gate_=None):
        self.runs.append(run_)
        if gate_ is not None:
            self.checks.append(gate_)

    def rollup(self):
        entries = [rollup_entry(c) for c in self.checks if c["head_sha"] == HEAD]
        entries += [copy.deepcopy(s) for s in self.statuses]
        return list(reversed(entries)) if self.rollup_reversed else entries

    def pr_view(self):
        view = {"number": PR, "state": "OPEN", "title": self.title, "headRefName": BRANCH,
                "headRefOid": HEAD, "baseRefName": BASE_REF, "baseRefOid": BASE,
                "mergeStateStatus": "CLEAN", "autoMergeRequest": None,
                "author": {"login": AUTHOR}, "statusCheckRollup": self.rollup()}
        return 0, json.dumps(view), ""

    def check_runs(self, sha, name):
        runs = [copy.deepcopy(c) for c in self.checks if c["head_sha"] == sha and c["name"] == name]
        return 0, json.dumps({"total_count": len(runs) + self.hidden_checks, "check_runs": runs}), ""

    def ci_runs(self, query):
        runs = [copy.deepcopy(r) for r in self.runs
                if r["path"] == CI_PATH and r["head_sha"] == query.get("head_sha")
                and r["event"] == query.get("event")]
        return 0, json.dumps({"total_count": len(runs), "workflow_runs": runs}), ""

    def api(self, url):
        parts = urlsplit(url)
        path = parts.path
        query = {k: v[-1] for k, v in parse_qs(parts.query).items()}
        if path == f"repos/{REPO}/rules/branches/{BASE_REF}":
            binding = {"context": GATE, "integration_id": TRUSTED_APP["id"]}
            rules = [{"type": "required_status_checks", "parameters": {"required_status_checks": [binding]}}
                     for _ in range(2)]
            return 0, json.dumps(rules), ""
        if path == f"repos/{REPO}/branches/{BASE_REF}/protection":
            return 1, "", "Branch not protected"
        for f in (enq.ONE_REVIEW_FILE, enq.RISK_PATHS_FILE):
            if path == f"repos/{REPO}/contents/{f}" and query.get("ref") == BASE:
                return 0, (ROOT / f).read_text(), ""
        prefix = f"repos/{REPO}/commits/"
        if path.startswith(prefix) and path.endswith("/check-runs"):
            return self.check_runs(path[len(prefix):-len("/check-runs")], query.get("check_name"))
        if path == f"repos/{REPO}/actions/workflows/ci.yml/runs":
            return self.ci_runs(query)
        raise AssertionError(f"unserved GitHub API call: {url}")

    def requirements(self, argv):
        issue = argv[argv.index("--issue") + 1]
        pr = int(argv[argv.index("--pr") + 1].rsplit("/", 1)[-1])
        head = argv[argv.index("--head") + 1]
        req = {"class": "strict", "reviews": 2, "review_kind": "standards_and_spec",
               "security_capable": False, "browser_qa": False, "operator_approval": True,
               "full_checks": True, "reasons": ["fixture: legacy project has no approved solo profile"],
               "policy_digest": POLICY, "activation": "no_profile"}
        resp = {"issue": issue, "project": PROJECT, "repo": REPO, "pr": pr, "head": head,
                "base": BASE, "base_ref": BASE_REF, "merge_base": "c" * 40,
                "policy_digest": POLICY, "requirements": req, "readiness": None}
        return 0, json.dumps(resp), ""

    def run(self, argv, input_text=None):
        if argv[:3] == ["gh", "pr", "view"]:
            return self.pr_view()
        if argv[:3] == ["gh", "pr", "merge"]:
            self.merges.append(list(argv))
            return self.merge_rc, "", "merge refused"
        if argv[:3] == ["gh", "repo", "view"]:
            return 0, json.dumps({"defaultBranchRef": {"name": "main"}}), ""
        if argv[:3] == ["gh", "api", "graphql"]:
            return 0, json.dumps({"data": {"repository": {"pullRequest": {
                "isInMergeQueue": True, "state": "OPEN"}}}}), ""
        if argv[:2] == ["gh", "api"]:
            return self.api(argv[-1])
        if argv[:3] == ["cadence", "issue", "show"]:
            return 0, json.dumps({"id": argv[3], "project": PROJECT, "owner": None, "claim": None,
                                  "body": "## Goal\nResume the AOS-293 delivery.\n",
                                  "status": "review", "refs": [{"kind": "branch", "path": BRANCH}],
                                  "comments": self.comments}), ""
        if argv[:3] == ["cadence", "issue", "comment"]:
            self.posted.append(input_text)
            self.comments.append({"body": input_text})
            if self.after_post is not None:
                self.add(*self.after_post)
                self.after_post = None
            return 0, "", ""
        if argv[:3] == ["cadence", "audit", "verdicts"]:
            return 0, json.dumps({"verdicts": self.verdicts, "skipped": []}), ""
        if argv[:3] == ["cadence", "audit", "approval"]:
            return 0, json.dumps({"state": "in-force", "approval_id": APPROVAL_ID,
                                  "source": "operator in chat", "recorded_via": OPERATOR}), ""
        if argv[:3] == ["cadence", "delivery", "requirements"]:
            return self.requirements(argv)
        raise AssertionError(f"unexpected command {argv}")


def base_case(tmp, *, seeded=True):
    """The superseded-success control: only the current ready success exists."""
    c = Case(tmp)
    c.add(*observed_ready())
    if seeded:
        c.comments = [{"body": f"{HEAD} {NOTE_STD} {NOTE_SPEC}\nBase branch: {BASE_REF}\n"
                               f"Operator approval id: {APPROVAL_ID}"}]
    return c


class CurrentAuthoritativeCi(unittest.TestCase):
    ORDERS = (("stored", False), ("reversed", True))

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="cad1598")
        self.addCleanup(self._tmp.cleanup)

    def run_main(self, c, *extra):
        argv = [str(PR), "--head", HEAD, "--repo", REPO, "--notes-dir", self._tmp.name,
                "--poll-secs", "0", *extra]
        saved = enq.run, enq.pr_changes
        enq.run = c.run
        enq.pr_changes = lambda *a: ([("M", "100644", "100644", p) for p in c.paths], 40)
        out, err = io.StringIO(), io.StringIO()
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                rc = enq.main(argv)
        finally:
            enq.run, enq.pr_changes = saved
        return rc, out.getvalue(), err.getvalue()

    def refuse_in_both_modes(self, mutate):
        """Each scenario refused by dry-run and by the enqueue path, in both rollup orders."""
        for order, reversed_rollup in self.ORDERS:
            for dry in (True, False):
                with self.subTest(order=order, dry_run=dry):
                    c = base_case(self._tmp.name)
                    c.rollup_reversed = reversed_rollup
                    mutate(c)
                    rc, out, err = self.run_main(c, *(["--dry-run"] if dry else []))
                    self.assertEqual(rc, 1, (out, err))
                    self.assertEqual(c.posted, [], "an evidence comment was written")
                    self.assertEqual(c.merges, [], "a merge command ran")

    # ---- the stale draft must not poison the current success -------------

    def test_current_success_admitted_despite_older_cancelled_draft_failure(self):
        for order, reversed_rollup in self.ORDERS:
            with self.subTest(order=order):
                c = Case(self._tmp.name)
                c.add(*observed_draft())
                c.add(*observed_ready())
                c.rollup_reversed = reversed_rollup
                rc, out, err = self.run_main(c)
                self.assertEqual(rc, 0, (out, err))
                self.assertEqual(len(c.posted), 1, c.posted)
                body = c.posted[0]
                for needle in (REPO, f"#{PR}", HEAD, BASE_REF, NEW_URL, APPROVAL_ID, NOTE_STD, NOTE_SPEC):
                    self.assertIn(needle, body, needle)
                self.assertNotIn(OLD_URL, body)
                self.assertEqual(len(c.merges), 1, c.merges)
                self.assertIn("--match-head-commit", c.merges[0])
                self.assertIn(HEAD, c.merges[0])

    def test_current_success_admitted_in_dry_run_once_evidence_posted(self):
        # Dry-run needs the evidence comment the guard itself posts; reuse that
        # body as the ticket comment so the fixture carries no invented format.
        posted = Case(self._tmp.name)
        posted.add(*observed_draft())
        posted.add(*observed_ready())
        rc, _, err = self.run_main(posted)
        self.assertEqual(rc, 0, err)
        c = Case(self._tmp.name)
        c.add(*observed_draft())
        c.add(*observed_ready())
        c.comments = [{"body": posted.posted[0]}]
        rc, out, err = self.run_main(c, "--dry-run")
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(c.merges, [])

    # ---- a newer non-success or unbindable attempt refuses despite the older success --

    def test_newer_failure_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: c.add(*newer_failure()))

    def test_newer_pending_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: c.add(*newer_pending()))

    def test_newest_attempt_without_required_gate_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: c.add(*newest_without_gate()))

    def test_newer_success_from_untrusted_app_refuses(self):
        self.refuse_in_both_modes(lambda c: c.add(*newer_untrusted_app()))

    def test_same_named_status_context_alone_is_no_evidence(self):
        def only_status(c):
            c.runs = []
            c.checks = []
            c.statuses = [status_success()]
        self.refuse_in_both_modes(only_status)

    def test_ambiguous_gate_in_newest_attempt_refuses(self):
        def ambiguous(c):
            c.checks.append(gate_rec(117992999999, NEW_RUN, NEW_SUITE, conclusion="failure",
                                     started=ts("09:43:15"), completed=ts("09:43:20")))
        self.refuse_in_both_modes(ambiguous)

    def test_malformed_newest_gate_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: c.add(*newer_malformed()))

    def test_newer_gate_bound_to_other_head_run_refuses(self):
        self.refuse_in_both_modes(lambda c: c.add(*newer_gate_bound_to_stale_head_run()))

    def test_truncated_check_run_listing_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: setattr(c, "hidden_checks", 1))

    def test_newer_failure_appearing_after_evidence_post_refuses_enqueue(self):
        c = base_case(self._tmp.name, seeded=False)
        c.after_post = newer_failure()
        rc, out, err = self.run_main(c)
        self.assertEqual(rc, 1, (out, err))
        self.assertEqual(c.merges, [], "a merge command ran after a newer failure")

    # ---- an older run re-run after the newer success is judged by its current attempt --

    def test_rerun_of_older_run_in_progress_refuses_despite_newer_success(self):
        # Run A created 09:33 (before ready B at 09:35) was re-run at 09:44:30 and is
        # still in progress; its re-run gate is not emitted yet. The old failed
        # gate record remains listed under A's suite.
        def rerun_pending(c):
            draft_run, draft_gate = observed_draft()
            c.runs.append(rerun_rec(draft_run, started=ts("09:44:30"), status="in_progress",
                                    conclusion=None))
            c.checks.append(draft_gate)
        self.refuse_in_both_modes(rerun_pending)

    def test_rerun_of_older_run_failed_refuses_despite_newer_success(self):
        def rerun_failed(c):
            draft_run, draft_gate = observed_draft()
            c.runs.append(rerun_rec(draft_run, started=ts("09:44:30"), status="completed",
                                    conclusion="failure", updated=ts("09:46:00")))
            c.checks.append(draft_gate)
            c.checks.append(gate_rec(117993700000, OLD_RUN, OLD_SUITE, conclusion="failure",
                                     started=ts("09:44:40"), completed=ts("09:45:50")))
        self.refuse_in_both_modes(rerun_failed)

    # ---- a same-named success from another workflow must not rescue ci.yml ----

    def test_other_workflow_success_does_not_rescue_newer_failure(self):
        def rescue(c):
            c.add(*newer_failure())
            c.add(*other_workflow_success())
        self.refuse_in_both_modes(rescue)

    def test_other_workflow_success_does_not_rescue_missing_ci_gate(self):
        def rescue(c):
            c.add(*newest_without_gate())
            c.add(*other_workflow_success())
        self.refuse_in_both_modes(rescue)


if __name__ == "__main__":
    unittest.main()
