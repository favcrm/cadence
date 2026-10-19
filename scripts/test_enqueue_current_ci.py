#!/usr/bin/env python3
"""CAD-1598: the guarded enqueue judges the CURRENT authoritative required CI
attempt of every required context, for the exact reviewed head, and the ticket
comment must name the CI run that was actually evaluated.

Acceptance (ticket items 1 and 2):
 * a superseded cancelled/failed attempt must not poison a newer successful
   attempt, and a superseded success must not rescue a newer failure;
 * a current failed, pending, missing, malformed, untrusted, ambiguous,
   mis-bound or truncated required check refuses even when an older attempt
   succeeded;
 * each required context is proven by its own workflow's current pull_request
   attempt (two-workflow provenance); a push-event run is never this PR's proof;
 * the comment names the evaluated green ci.yml run, and a re-evaluation after
   the evidence post refuses when the CI run changes.

Independent acceptance check: the expected outcomes come from the CAD-1598 ticket
and the observed GitHub metadata of AgenticOS-v2 PR 747, not from
scripts/enqueue-reviewed. The real main/evaluate/run path runs; only the external
transports are faked: gh (pr view, pr merge, api, api graphql, repo view), cadence
(issue show/comment, audit verdicts/approval, delivery requirements) and git diff
via pr_changes. Any unserved command or API endpoint raises. Nothing is enqueued,
posted or merged for real. Refusal cases assert outcome and zero writes/merges,
never reason text.

Transport shapes served (producer-shaped fakes, not native GitHub qualification):
 * rulesets `required_status_checks[].integration_id` bind each required name to
   the GitHub Actions app (15368);
 * `repos/R/commits/SHA/check-runs?check_name=NAME&filter=all&per_page=100` returns
   {total_count, check_runs:[{id,name,head_sha,status,conclusion,started_at,
   completed_at,details_url,check_suite:{id},app:{id,slug}}]}; total_count above the
   listed length is a truncated listing;
 * `repos/R/actions/runs?head_sha=SHA&per_page=100` returns {total_count,
   workflow_runs:[{id,run_number,run_attempt,workflow_id,path,head_sha,event,
   status,conclusion,created_at,updated_at,run_started_at,check_suite_id,html_url}]},
   one latest attempt per run, every event (an event filter fails the test).

Observed values (ids, numbers, times, app, workflow ids and paths) come from the
ticket evidence: PR 747 ready ci.yml run 39231256712 / gate job 117992467504 /
suite 106304880676 SUCCESS; cancelled draft run 39231020958 / gate job
117989954486 / suite 106304242044 FAILURE; ci.yml workflow 362543015; promotion
workflow 368088362 (.github/workflows/promotion-policy.yml); observed head
81a4f55531064168f8490f41d2758a53cf318a6a runs 39209012730 (run_number 230),
39208284745 (run_number 229), ci.yml 39208284741 (run_number 3238), push ci.yml
39207701467 (run_number 3237). Those head-81a4 records are re-homed to HEAD here.
Every other id, time, digest, job id and the untrusted app id is invented.
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
BASE = "b" * 40  # invented base commit oid
BASE_REF = "staging"
BRANCH = "cadence/aos-293-pr747"  # invented lane branch
ISSUE = "AOS-293"
PROJECT = "agenticos-v2"
AUTHOR = "aos-author"
GATE = "Required result gate"
PROMO_GATE = "Main promotion policy"
CI_WORKFLOW_ID = 362543015
CI_PATH = ".github/workflows/ci.yml"
PROMO_WORKFLOW_ID = 368088362
PROMO_PATH = ".github/workflows/promotion-policy.yml"
OTHER_WORKFLOW_ID = 362543999  # invented
OTHER_PATH = ".github/workflows/other.yml"  # invented
TRUSTED_APP = {"id": 15368, "slug": "github-actions"}
UNTRUSTED_APP = {"id": 99999, "slug": "some-other-app"}  # invented
APPROVAL_ID = "merge-pr747-3d1023c036c1"  # invented
NOTE_STD = "20261019-093000-aos293-pr747-standards-verdict.md"  # invented
NOTE_SPEC = "20261019-094000-aos293-pr747-spec-security-verdict.md"  # invented
OPERATOR = "operator-connection"
POLICY = "sha256:" + "ab" * 32  # invented
ORDERS = (("stored", False), ("reversed", True))

# PR 747 ready and cancelled-draft ci.yml attempts (observed).
OLD_RUN, OLD_JOB, OLD_SUITE = 39231020958, 117989954486, 106304242044
NEW_RUN, NEW_JOB, NEW_SUITE = 39231256712, 117992467504, 106304880676
NEW_URL = f"https://github.com/{REPO}/actions/runs/{NEW_RUN}"
OLD_URL = f"https://github.com/{REPO}/actions/runs/{OLD_RUN}"

# Two-workflow fixture (observed head 81a4..., re-homed to HEAD).
CI_RUN, CI_SUITE, CI_NUMBER, CI_JOB = 39208284741, 106243344257, 3238, 117993000010  # job invented
CI_URL = f"https://github.com/{REPO}/actions/runs/{CI_RUN}"
PROMO_RUN, PROMO_SUITE, PROMO_NUMBER, PROMO_JOB = 39209012730, 106245240703, 230, 117993000011
PROMO_URL = f"https://github.com/{REPO}/actions/runs/{PROMO_RUN}"
PROMO_OLD_RUN, PROMO_OLD_SUITE, PROMO_OLD_NUMBER, PROMO_OLD_JOB = 39208284745, 106243344273, 229, 117993000012
PROMO_OLD_URL = f"https://github.com/{REPO}/actions/runs/{PROMO_OLD_RUN}"
PUSH_RUN, PUSH_SUITE, PUSH_NUMBER, PUSH_JOB = 39207701467, 106241800480, 3237, 117993000013
CI_NEWER_RUN, CI_NEWER_SUITE, CI_NEWER_NUMBER, CI_NEWER_JOB = 39208290000, 106243399999, 3239, 117993000014  # invented
CI_NEWER_URL = f"https://github.com/{REPO}/actions/runs/{CI_NEWER_RUN}"
OLD_FIRST_JOB, OLD_RETRY_JOB = 117993400001, 117993400002  # invented
OLD_RUN_NUMBER = 44437  # run_rec's run_number for OLD_RUN created 09:33:57
OTHER_RUN, OTHER_SUITE, OTHER_JOB, OTHER_NUMBER = 39231400000, 106306000000, 117994000000, 3240  # invented

# Trusted base copies of the producer workflows, served by the contents endpoint.
CI_WORKFLOW_TEXT = """name: CI
on:
  pull_request:

jobs:
  gate:
    name: Required result gate
    runs-on: ubuntu-latest
    steps:
      - run: echo gate
"""
PROMO_WORKFLOW_TEXT = """name: Promotion policy
on:
  pull_request:

jobs:
  promotion:
    name: Main promotion policy
    runs-on: ubuntu-latest
    steps:
      - run: echo promotion
"""
WORKFLOW_TEXT = {CI_PATH: CI_WORKFLOW_TEXT, PROMO_PATH: PROMO_WORKFLOW_TEXT}


def ts(hms):
    return f"2026-10-19T{hms}Z"


def run_rec(run_id, suite, created, *, status="completed", conclusion="success", updated=None,
            head=HEAD, workflow_id=CI_WORKFLOW_ID, path=CI_PATH, event="pull_request", run_number=None):
    """One entry of GET repos/R/actions/runs?head_sha=SHA (latest attempt)."""
    number = run_number or 10000 + int(created[11:13]) * 3600 + int(created[14:16]) * 60 + int(created[17:19])
    return {
        "id": run_id, "run_number": number, "run_attempt": 1, "workflow_id": workflow_id,
        "path": path, "head_sha": head, "event": event, "status": status, "conclusion": conclusion,
        "created_at": created, "updated_at": updated or created, "run_started_at": created,
        "html_url": f"https://github.com/{REPO}/actions/runs/{run_id}", "check_suite_id": suite,
    }


def gate_rec(job_id, suite, *, started, completed, status="completed", conclusion="success",
             head=HEAD, app=TRUSTED_APP, name=GATE):
    """A check run as GET repos/R/commits/SHA/check-runs lists it."""
    return {"id": job_id, "name": name, "head_sha": head, "status": status, "conclusion": conclusion,
            "started_at": started, "completed_at": completed,
            "details_url": f"https://github.com/{REPO}/actions/runs/{suite}/job/{job_id}",
            "check_suite": {"id": suite}, "app": app}


def attempt(run_id, suite, number, created, job, *, name=GATE, workflow_id=CI_WORKFLOW_ID, path=CI_PATH,
            event="pull_request", conclusion="success", started, completed):
    """A run and its gate check run. conclusion None means still pending."""
    pending = conclusion is None
    run = run_rec(run_id, suite, created, status="in_progress" if pending else "completed",
                  conclusion=conclusion, updated=completed or created, workflow_id=workflow_id,
                  path=path, event=event, run_number=number)
    gate = gate_rec(job, suite, started=started, completed=None if pending else completed,
                    status="in_progress" if pending else "completed", conclusion=conclusion, name=name)
    return run, gate


def rollup_entry(c):
    """gh pr view --json statusCheckRollup CheckRun shape."""
    return {"__typename": "CheckRun", "name": c["name"], "status": c["status"].upper(),
            "conclusion": c["conclusion"].upper() if c["conclusion"] else None,
            "startedAt": c["started_at"], "completedAt": c["completed_at"],
            "detailsUrl": c["details_url"]}


def observed_draft():
    """Superseded cancelled draft attempt; its same-head gate is FAILURE."""
    return (run_rec(OLD_RUN, OLD_SUITE, ts("09:33:57"), conclusion="cancelled", updated=ts("09:37:10")),
            gate_rec(OLD_JOB, OLD_SUITE, started=ts("09:37:02"), completed=ts("09:37:09"),
                     conclusion="failure"))


def observed_ready():
    """Current ready attempt at the same head; its gate is SUCCESS."""
    return (run_rec(NEW_RUN, NEW_SUITE, ts("09:35:50"), updated=ts("09:43:23")),
            gate_rec(NEW_JOB, NEW_SUITE, started=ts("09:43:14"), completed=ts("09:43:22")))


def newer_failure():
    return (run_rec(39231300000, 106305000000, ts("09:52:00"), conclusion="failure", updated=ts("09:52:40")),
            gate_rec(117993000000, 106305000000, started=ts("09:52:10"), completed=ts("09:52:40"),
                     conclusion="failure"))


def newer_pending():
    return (run_rec(39231300500, 106305100000, ts("09:53:00"), status="in_progress", conclusion=None),
            gate_rec(117993100000, 106305100000, started=ts("09:53:05"), completed=None,
                     status="in_progress", conclusion=None))


def newest_without_gate():
    """A newer ci.yml attempt that completed successfully but has no required gate."""
    return (run_rec(39231301000, 106305200000, ts("09:54:00"), updated=ts("09:58:00")), None)


def newer_untrusted_app():
    return (run_rec(39231302000, 106305300000, ts("09:55:00"), updated=ts("09:55:30")),
            gate_rec(117993300000, 106305300000, started=ts("09:55:10"), completed=ts("09:55:30"),
                     app=UNTRUSTED_APP))


def newer_malformed():
    """Newest gate with no check_suite and no app: cannot bind to a run or to Actions."""
    run, gate = run_rec(39231305000, 106305600000, ts("09:59:00"), updated=ts("09:59:30")), gate_rec(
        117993600000, 106305600000, started=ts("09:59:10"), completed=ts("09:59:30"))
    del gate["check_suite"]
    gate["app"] = None
    return run, gate


def newer_gate_bound_to_stale_head_run():
    """A newer HEAD gate whose check_suite belongs to a run at another head."""
    return (run_rec(39231304000, 106305500000, ts("09:57:00"), head=STALE_HEAD, updated=ts("09:57:30")),
            gate_rec(117993500000, 106305500000, started=ts("09:57:10"), completed=ts("09:57:30")))


def status_context_success():
    """A same-named commit status (StatusContext) from no Actions app."""
    return {"__typename": "StatusContext", "context": GATE, "state": "SUCCESS",
            "startedAt": ts("09:58:00"), "targetUrl": "https://status.example.invalid/gate"}


def ci_current(conclusion="success"):
    """Current ci.yml pull_request attempt at HEAD (observed run 39208284741, run_number 3238)."""
    return attempt(CI_RUN, CI_SUITE, CI_NUMBER, "2026-10-19T06:09:23Z", CI_JOB, conclusion=conclusion,
                   started="2026-10-19T06:09:30Z", completed="2026-10-19T06:10:00Z")


def ci_newer(conclusion="success"):
    """A newer ci.yml pull_request attempt at HEAD that replaces CI_RUN as current."""
    return attempt(CI_NEWER_RUN, CI_NEWER_SUITE, CI_NEWER_NUMBER, "2026-10-19T06:30:00Z", CI_NEWER_JOB,
                   conclusion=conclusion, started="2026-10-19T06:30:05Z", completed="2026-10-19T06:31:00Z")


def promo_current(conclusion="success"):
    """Current promotion-policy pull_request attempt (run_number 230, observed started 06:16:59Z)."""
    return attempt(PROMO_RUN, PROMO_SUITE, PROMO_NUMBER, "2026-10-19T06:16:59Z", PROMO_JOB, name=PROMO_GATE,
                   workflow_id=PROMO_WORKFLOW_ID, path=PROMO_PATH, conclusion=conclusion,
                   started="2026-10-19T06:17:05Z", completed="2026-10-19T06:18:00Z")


def promo_old(conclusion="success"):
    """Superseded promotion-policy pull_request attempt (run_number 229, observed)."""
    return attempt(PROMO_OLD_RUN, PROMO_OLD_SUITE, PROMO_OLD_NUMBER, "2026-10-19T06:09:23Z", PROMO_OLD_JOB,
                   name=PROMO_GATE, workflow_id=PROMO_WORKFLOW_ID, path=PROMO_PATH, conclusion=conclusion,
                   started="2026-10-19T06:09:30Z", completed="2026-10-19T06:10:00Z")


def promo_push_success():
    """A 'Main promotion policy' gate carried by the PUSH-event ci.yml run 39207701467 (observed)."""
    return attempt(PUSH_RUN, PUSH_SUITE, PUSH_NUMBER, "2026-10-19T06:00:00Z", PUSH_JOB, name=PROMO_GATE,
                   workflow_id=CI_WORKFLOW_ID, path=CI_PATH, event="push", conclusion="success",
                   started="2026-10-19T06:00:10Z", completed="2026-10-19T06:01:00Z")


def promo_orphan_gate():
    """A 'Main promotion policy' gate whose check_suite matches no run at this head."""
    return (None, gate_rec(117993000015, 106249999999, name=PROMO_GATE,
                           started="2026-10-19T06:20:00Z", completed="2026-10-19T06:20:30Z"))


def first_attempt_gate():
    """OLD_RUN's gate from its first attempt, in the suite the re-run keeps (started before the retry)."""
    return gate_rec(OLD_FIRST_JOB, OLD_SUITE, started=ts("09:34:05"), completed=ts("09:34:30"))


def retried_older_run(*, conclusion="failure"):
    """OLD_RUN re-run as attempt 2, started after NEW_RUN; GitHub keeps run_number and suite."""
    run, gate = attempt(OLD_RUN, OLD_SUITE, OLD_RUN_NUMBER, ts("09:40:00"), OLD_RETRY_JOB,
                        conclusion=conclusion, started=ts("09:40:10"),
                        completed=None if conclusion is None else ts("09:40:40"))
    run["run_attempt"] = 2
    return run, gate


def other_workflow_gate():
    """The only producer of the required context: a green run of other.yml, which the base does not carry."""
    return attempt(OTHER_RUN, OTHER_SUITE, OTHER_NUMBER, ts("09:50:00"), OTHER_JOB,
                   workflow_id=OTHER_WORKFLOW_ID, path=OTHER_PATH, conclusion="success",
                   started=ts("09:50:05"), completed=ts("09:50:30"))


class Case:
    """One PR 747 scenario over the guard's external transport."""

    def __init__(self, tmp):
        self.tmp = Path(tmp)
        self.paths = ["config/aos293-fixture.toml"]
        self.title = f"{ISSUE}: resume PR {PR} after the CI selection repair"
        self.required = [GATE]
        self.runs = []
        self.checks = []
        self.statuses = []
        self.rollup_reversed = False
        self.hidden_checks = 0  # same-name check runs the endpoint counts but does not list
        self.after_post = None  # (run, gate) that appears once the evidence comment is posted
        self.comments = []
        self.posted = []
        self.merges = []
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
        if run_ is not None:
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

    def head_runs(self, query):
        if "event" in query:
            raise AssertionError("the producer must list runs of every event, not filter by event")
        runs = [copy.deepcopy(r) for r in self.runs if r["head_sha"] == query.get("head_sha")]
        return 0, json.dumps({"total_count": len(runs), "workflow_runs": runs}), ""

    def api(self, url):
        parts = urlsplit(url)
        path = parts.path
        query = {k: v[-1] for k, v in parse_qs(parts.query).items()}
        if path == f"repos/{REPO}/rules/branches/{BASE_REF}":
            binding = [{"context": name, "integration_id": TRUSTED_APP["id"]} for name in self.required]
            rules = [{"type": "required_status_checks", "parameters": {"required_status_checks": binding}}
                     for _ in range(2)]
            return 0, json.dumps(rules), ""
        if path == f"repos/{REPO}/branches/{BASE_REF}/protection":
            return 1, "", "Branch not protected"
        for f in (enq.ONE_REVIEW_FILE, enq.RISK_PATHS_FILE):
            if path == f"repos/{REPO}/contents/{f}" and query.get("ref") == BASE:
                return 0, (ROOT / f).read_text(), ""
        if path.startswith(f"repos/{REPO}/contents/.github/workflows/"):
            if query.get("ref") != BASE:
                raise AssertionError(f"workflow read at an unserved ref: {url}")
            text = WORKFLOW_TEXT.get(path[len(f"repos/{REPO}/contents/"):])
            if text is None:
                return 1, "", "Not Found"
            return 0, text, ""
        prefix = f"repos/{REPO}/commits/"
        if path.startswith(prefix) and path.endswith("/check-runs"):
            return self.check_runs(path[len(prefix):-len("/check-runs")], query.get("check_name"))
        if path == f"repos/{REPO}/actions/runs":
            return self.head_runs(query)
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
            return 0, "", ""
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


def ticket_comment(green, *, proofs=(), checks=(GATE,)):
    """A ticket comment naming the head, notes, base, approval, the green CI run and any per-check proofs."""
    lines = [f"Delivery evidence: {REPO}#{PR}", f"Head: {HEAD}", f"Base: {BASE}",
             f"Base branch: {BASE_REF}", f"Green CI run: {green}", "Required checks: " + ", ".join(checks)]
    lines += [f"Required check run: {n} = {u}" for n, u in proofs]
    lines += [f"Verdict notes: {NOTE_STD}, {NOTE_SPEC}", f"Operator approval id: {APPROVAL_ID}"]
    return {"body": "\n".join(lines)}


def base_case(tmp, *, seeded=True):
    """The single-context control: only the current ready success exists."""
    c = Case(tmp)
    c.add(*observed_ready())
    if seeded:
        c.comments = [ticket_comment(NEW_URL)]
    return c


def multi_case(tmp, *, ci=ci_current, promo=promo_current, seeded=True, extra=()):
    """Two required contexts from two workflows, both on app 15368. Each factory returns (run, gate);
    promo=None means no promotion-policy pull_request run exists."""
    c = Case(tmp)
    c.required = [GATE, PROMO_GATE]
    for factory in (ci, promo):
        if factory is not None:
            c.add(*factory())
    for run_, gate_ in extra:
        c.add(run_, gate_)
    if seeded:
        c.comments = [ticket_comment(CI_URL, proofs=[(PROMO_GATE, PROMO_URL)], checks=(GATE, PROMO_GATE))]
    return c


class CurrentAuthoritativeCi(unittest.TestCase):

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

    def refuse_in_both_modes(self, mutate, make=base_case):
        """Each scenario refused by dry-run and by the enqueue path, in both rollup orders."""
        for order, reversed_rollup in ORDERS:
            for dry in (True, False):
                with self.subTest(order=order, dry_run=dry):
                    c = make(self._tmp.name)
                    c.rollup_reversed = reversed_rollup
                    mutate(c)
                    rc, out, err = self.run_main(c, *(["--dry-run"] if dry else []))
                    self.assertEqual(rc, 1, (out, err))
                    self.assertEqual(c.posted, [], "an evidence comment was written")
                    self.assertEqual(c.merges, [], "a merge command ran")

    def admit(self, c, *, expect_posted_has=()):
        """Enqueue path admitted: one comment, one merge, and the comment names what was evaluated."""
        rc, out, err = self.run_main(c)
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(c.posted), 1, c.posted)
        for needle in expect_posted_has:
            self.assertIn(needle, c.posted[0], needle)
        self.assertEqual(len(c.merges), 1, c.merges)
        self.assertIn("--match-head-commit", c.merges[0])
        self.assertIn(HEAD, c.merges[0])

    # ---- the stale draft must not poison the current success -------------

    def test_current_success_admitted_despite_older_cancelled_draft_failure(self):
        for order, reversed_rollup in ORDERS:
            with self.subTest(order=order):
                c = Case(self._tmp.name)
                c.add(*observed_draft())
                c.add(*observed_ready())
                c.rollup_reversed = reversed_rollup
                self.admit(c, expect_posted_has=(REPO, f"#{PR}", HEAD, BASE_REF, NEW_URL, APPROVAL_ID,
                                                 NOTE_STD, NOTE_SPEC))
                self.assertNotIn(OLD_URL, c.posted[0])

    def test_current_success_admitted_in_dry_run_with_ticket_comment(self):
        for order, reversed_rollup in ORDERS:
            with self.subTest(order=order):
                c = base_case(self._tmp.name)
                c.add(*observed_draft())
                c.rollup_reversed = reversed_rollup
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

    def test_newer_success_from_untrusted_app_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: c.add(*newer_untrusted_app()))

    def test_status_context_success_with_newest_attempt_missing_gate_refuses(self):
        def status_only(c):
            c.add(*newest_without_gate())
            c.statuses = [status_context_success()]
        self.refuse_in_both_modes(status_only)

    def test_ambiguous_gates_in_newest_attempt_refuse(self):
        def tied(c):
            c.add(*newer_failure())
            c.checks.append(gate_rec(117993000001, 106305000000, started=ts("09:52:10"),
                                     completed=ts("09:52:20"), conclusion="success"))
        self.refuse_in_both_modes(tied)

    def test_malformed_newest_gate_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: c.add(*newer_malformed()))

    def test_newer_gate_bound_to_stale_head_run_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: c.add(*newer_gate_bound_to_stale_head_run()))

    def test_truncated_check_run_listing_refuses_despite_older_success(self):
        self.refuse_in_both_modes(lambda c: setattr(c, "hidden_checks", 1))

    def test_newer_failure_appearing_after_evidence_post_refuses_enqueue(self):
        c = base_case(self._tmp.name, seeded=False)
        c.after_post = newer_failure()
        rc, out, err = self.run_main(c)
        self.assertEqual(rc, 1, (out, err))
        self.assertEqual(c.merges, [], "a merge command ran after a newer failure")

    # ---- (1) two-workflow provenance: each required context proven by its own workflow ----

    def test_both_required_contexts_current_and_green_admitted_naming_both_runs(self):
        for order, reversed_rollup in ORDERS:
            with self.subTest(order=order):
                c = multi_case(self._tmp.name, seeded=False)
                c.rollup_reversed = reversed_rollup
                self.admit(c, expect_posted_has=(f"Green CI run: {CI_URL}",
                                                 f"Required check run: {PROMO_GATE} = {PROMO_URL}"))

    def test_promotion_context_current_failed_refuses_though_ci_green(self):
        self.refuse_in_both_modes(lambda c: None,
                                  make=lambda tmp: multi_case(tmp, promo=lambda: promo_current("failure")))

    def test_promotion_context_current_pending_refuses_though_ci_green(self):
        self.refuse_in_both_modes(lambda c: None,
                                  make=lambda tmp: multi_case(tmp, promo=lambda: promo_current(None)))

    def test_ci_context_current_failed_refuses_though_promotion_green(self):
        self.refuse_in_both_modes(lambda c: None,
                                  make=lambda tmp: multi_case(tmp, ci=lambda: ci_current("failure")))

    def test_ci_context_current_pending_refuses_though_promotion_green(self):
        self.refuse_in_both_modes(lambda c: None,
                                  make=lambda tmp: multi_case(tmp, ci=lambda: ci_current(None)))

    def test_superseded_promotion_success_does_not_rescue_current_failure(self):
        self.refuse_in_both_modes(lambda c: c.add(*promo_old("success")),
                                  make=lambda tmp: multi_case(tmp, promo=lambda: promo_current("failure")))

    def test_superseded_promotion_failure_does_not_block_current_success(self):
        for order, reversed_rollup in ORDERS:
            with self.subTest(order=order):
                c = multi_case(self._tmp.name, seeded=False)
                c.add(*promo_old("failure"))
                c.rollup_reversed = reversed_rollup
                self.admit(c, expect_posted_has=(f"Required check run: {PROMO_GATE} = {PROMO_URL}",))
                self.assertNotIn(PROMO_OLD_URL, c.posted[0])

    def test_push_event_promotion_gate_alone_is_no_proof(self):
        self.refuse_in_both_modes(lambda c: None,
                                  make=lambda tmp: multi_case(tmp, promo=None, extra=[promo_push_success()]))

    def test_push_event_promotion_gate_never_stands_in_for_failed_pull_request_proof(self):
        self.refuse_in_both_modes(lambda c: None,
                                  make=lambda tmp: multi_case(tmp, promo=lambda: promo_current("failure"),
                                                              extra=[promo_push_success()]))

    def test_promotion_gate_bound_to_no_run_at_head_is_misbound_and_refuses(self):
        self.refuse_in_both_modes(lambda c: None,
                                  make=lambda tmp: multi_case(tmp, promo=None, extra=[promo_orphan_gate()]))

    # ---- (2) comment / evaluated-CI coherence ----

    def test_stale_comment_naming_a_different_run_cannot_satisfy_dry_run(self):
        for order, reversed_rollup in ORDERS:
            with self.subTest(order=order):
                c = Case(self._tmp.name)
                c.add(*observed_draft())
                c.add(*observed_ready())
                c.rollup_reversed = reversed_rollup
                c.comments = [ticket_comment(OLD_URL)]
                rc, out, err = self.run_main(c, "--dry-run")
                self.assertEqual(rc, 1, (out, err))
                self.assertEqual(c.posted, [])
                self.assertEqual(c.merges, [])

    def test_enqueue_posts_canonical_comment_naming_current_run_and_merges_once(self):
        for order, reversed_rollup in ORDERS:
            with self.subTest(order=order):
                c = multi_case(self._tmp.name, seeded=False)
                c.rollup_reversed = reversed_rollup
                c.comments = [ticket_comment(OLD_URL)]
                rc, out, err = self.run_main(c)
                self.assertEqual(rc, 0, (out, err))
                self.assertEqual(len(c.posted), 1, c.posted)
                body = c.posted[0]
                self.assertIn(f"Green CI run: {CI_URL}", body)
                self.assertIn(f"Required check run: {PROMO_GATE} = {PROMO_URL}", body)
                self.assertNotIn(OLD_URL, body)
                self.assertEqual(len(c.merges), 1, c.merges)
                self.assertIn("--match-head-commit", c.merges[0])

    def test_enqueue_with_seeded_current_comment_still_posts_canonical_and_merges_once(self):
        c = multi_case(self._tmp.name)
        rc, out, err = self.run_main(c)
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(c.merges), 1, c.merges)
        self.assertEqual(len(c.posted), 1, c.posted)
        self.assertIn(f"Green CI run: {CI_URL}", c.posted[0])

    def test_ci_run_changed_after_evidence_post_refuses_with_zero_merges(self):
        c = multi_case(self._tmp.name, seeded=False)
        c.after_post = ci_newer()
        rc, out, err = self.run_main(c)
        self.assertEqual(rc, 1, (out, err))
        self.assertEqual(len(c.posted), 1, c.posted)
        self.assertIn(f"Green CI run: {CI_URL}", c.posted[0])
        self.assertEqual(c.merges, [], "a merge command ran after the evaluated CI run changed")

    # ---- (3) later attempts of an older run, undeclared producers, separate producers ----

    def test_later_failed_attempt_of_older_run_refuses_despite_newer_success(self):
        def retried(c):
            c.add(None, first_attempt_gate())
            c.add(*observed_ready())
            c.add(*retried_older_run())
        self.refuse_in_both_modes(retried)

    def test_later_pending_attempt_of_older_run_refuses_despite_newer_success(self):
        def retried(c):
            c.add(None, first_attempt_gate())
            c.add(*observed_ready())
            c.add(*retried_older_run(conclusion=None))
        self.refuse_in_both_modes(retried)

    def test_same_named_check_from_undeclared_workflow_refuses_despite_gateless_green_ci(self):
        def undeclared(c):
            c.add(*newest_without_gate())
            c.add(*other_workflow_gate())
        self.refuse_in_both_modes(undeclared)

    def test_separate_declared_producers_each_prove_their_context_and_both_runs_are_named(self):
        for order, reversed_rollup in ORDERS:
            with self.subTest(order=order, dry_run=True):
                c = multi_case(self._tmp.name)
                c.rollup_reversed = reversed_rollup
                rc, out, err = self.run_main(c, "--dry-run")
                self.assertEqual(rc, 0, (out, err))
                self.assertEqual(c.merges, [])
            with self.subTest(order=order, dry_run=False):
                c = multi_case(self._tmp.name, seeded=False)
                c.rollup_reversed = reversed_rollup
                self.admit(c, expect_posted_has=(f"Green CI run: {CI_URL}",
                                                 f"Required check run: {PROMO_GATE} = {PROMO_URL}"))

    def test_required_context_with_no_producer_at_all_refuses(self):
        self.refuse_in_both_modes(lambda c: None, make=lambda tmp: multi_case(tmp, promo=None))


if __name__ == "__main__":
    unittest.main()
