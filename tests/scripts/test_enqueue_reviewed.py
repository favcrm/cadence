"""scripts/enqueue-reviewed against a fake `gh` and a fake `cadence` on PATH,
and a real throwaway git repo (so changed paths and modes come from git).

Nothing reaches GitHub or a daemon. Each scenario is a JSON file the fakes
read; every call they receive is appended to a log the tests assert on.
One test per refusal reason, plus the happy path and the not-queued rollback.
"""
import copy
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / 'scripts' / 'enqueue-reviewed'
OLD = '1234567' + 'b' * 33
REPO = 'favcrm/cadence'
PR = 77
ISSUE = 'CAD-959'
TOML = '''one_review_include = ["tests/**", "docs/guides/**", "docs/ui/**"]
one_review_exclude = ["docs/roles/**", "docs/TEAM.md", "docs/CHARTER.md", "AGENTS.md"]
'''

FAKE = r'''#!/usr/bin/env python3
import json, os, sys
sc = json.load(open(os.environ["FAKE_SCENARIO"]))
argv = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write(json.dumps([os.path.basename(sys.argv[0])] + argv) + "\n")
tool = os.path.basename(sys.argv[0])

def out(v, rc=0, err=""):
    if v is not None:
        sys.stdout.write(v if isinstance(v, str) else json.dumps(v))
    sys.stderr.write(err)
    sys.exit(rc)

if tool == "gh":
    a = " ".join(argv)
    if argv[:2] == ["repo", "view"]:
        out({"nameWithOwner": sc["repo"]})
    if argv[:2] == ["pr", "view"]:
        out(sc["pr_view"])
    if argv[:2] == ["pr", "merge"]:
        if "--disable-auto" in argv:
            out("", sc.get("disable_rc", 0))
        out("", sc.get("merge_rc", 0), sc.get("merge_err", ""))
    if argv[0] == "api" and "graphql" in argv:
        seq = sc["graphql"]
        n = int(open(os.environ["FAKE_LOG"] + ".gq").read() or 0) if os.path.exists(os.environ["FAKE_LOG"] + ".gq") else 0
        open(os.environ["FAKE_LOG"] + ".gq", "w").write(str(n + 1))
        out({"data": {"repository": {"pullRequest": seq[min(n, len(seq) - 1)]}}})
    if argv[0] == "api" and "/rules/branches/" in a:
        r = sc["rulesets"]
        out(None if r is None else r, 1 if r is None else 0, "HTTP 500" if r is None else "")
    if argv[0] == "api" and a.endswith("/protection"):
        r = sc["protection"]
        if r is None:
            out("", 1, "gh: Branch not protected (HTTP 404)")
        out(r)
    if argv[0] == "api" and "/contents/" in a:
        t = sc.get("toml")
        if t is None:
            out("", 1, "gh: Not Found (HTTP 404)")
        out(t)
    out("", 64, "fake gh: unexpected call: " + a)
if tool == "cadence":
    if argv[:2] == ["issue", "show"]:
        out(sc["ticket"], sc.get("ticket_rc", 0))
    if argv[:2] == ["audit", "approval"]:
        out(sc["approval"], sc.get("approval_rc", 0))
    out("", 64, "fake cadence: unexpected call: " + " ".join(argv))
'''

CHECKS = ['test', 'fmt', 'clippy', 'build', 'ui']


def check_run(name, conclusion='SUCCESS', status='COMPLETED'):
    return {'__typename': 'CheckRun', 'name': name, 'status': status, 'conclusion': conclusion}


def base_scenario(head, base_oid):
    return {
        'repo': REPO,
        'pr_view': {
            'number': PR, 'state': 'OPEN', 'title': f'{ISSUE}: demo', 'headRefOid': head,
            'baseRefName': 'main', 'baseRefOid': base_oid, 'mergeStateStatus': 'CLEAN', 'autoMergeRequest': None,
            'author': {'login': 'cc-syntax'},
            'statusCheckRollup': [check_run(c) for c in CHECKS],
        },
        'rulesets': [{'type': 'merge_queue', 'parameters': {}}],
        'protection': {'required_status_checks': {
            'contexts': CHECKS, 'checks': [{'context': c, 'app_id': 1} for c in CHECKS]}},
        'toml': TOML,
        'ticket': {'id': ISSUE, 'owner': 'lane-author', 'claim': {'by': 'cc13-pm'}, 'comments': []},
        'approval': {'state': 'missing', 'reason': 'no merge approval'}, 'approval_rc': 1,
        'graphql': [{'isInMergeQueue': True, 'state': 'OPEN'}],
    }


def note_text(kind, frm, head, result='pass', section=None, risk='auto', pr=PR, issue=ISSUE):
    return (f'# Verdict: {issue} {kind} review — {result}\n> Issue: {issue}\n> From: {frm}\n\n'
            f'## Verdict\n{section or result} — PR #{pr}, head {head}\n\nRisk: {risk}\n\n## Gates\n- x\n')


GIT = ['git', '-c', 'user.name=t', '-c', 'user.email=t@t', '-c', 'commit.gpgsign=false']


class EnqueueReviewedTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        bindir = self.root / 'bin'
        bindir.mkdir()
        for tool in ('gh', 'cadence'):
            f = bindir / tool
            f.write_text(FAKE)
            f.chmod(0o755)
        self.notes = self.root / 'notes'
        self.notes.mkdir()
        self.log = self.root / 'calls.log'
        self.log.touch()
        self.scenario_path = self.root / 'scenario.json'
        self.env = {**os.environ, 'PATH': f'{bindir}{os.pathsep}{os.environ["PATH"]}',
                    'FAKE_SCENARIO': str(self.scenario_path), 'FAKE_LOG': str(self.log),
                    'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_CONFIG_SYSTEM': os.devnull}
        self.origin = self.root / 'origin.git'
        self.author = self.root / 'author'
        self.work = self.root / 'work'
        self.git('init', '-q', '--bare', '-b', 'main', str(self.origin), cwd=self.root)
        self.git('init', '-q', '-b', 'main', str(self.author), cwd=self.root)
        self.git('remote', 'add', 'origin', str(self.origin))
        for rel, body in {'docs/guides/guide.md': 'g\n', 'docs/other.md': 'o\n', 'src/foo.rs': 'f\n', 'AGENTS.md': 'a\n',
                          'docs/roles/risk-classes.md': 'r\n', 'tests/old.py': 'o\n'}.items():
            self.put(rel, body)
        self.git('add', '-A')
        self.git('commit', '-q', '-m', 'base')
        self.base = self.git('rev-parse', 'HEAD').strip()
        self.git('push', '-q', 'origin', 'main')
        # The script runs in a separate checkout that has never seen the PR head.
        self.git('clone', '-q', str(self.origin), str(self.work), cwd=self.root)
        self.counter = 0
        self.build_head([('add', 'src/new.rs')])

    def tearDown(self):
        self.temp.cleanup()

    # ---- a real PR head ---------------------------------------------------

    def git(self, *args, cwd=None):
        r = subprocess.run([*GIT, *args], cwd=cwd or self.author, text=True,
                           capture_output=True, env=self.env)
        self.assertEqual(r.returncode, 0, f'git {args}: {r.stderr}')
        return r.stdout

    def put(self, rel, body='x\n'):
        path = self.author / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)

    def build_head(self, ops):
        """Commit `ops` on top of the base as the PR head and publish it as
        refs/pull/<PR>/head; then reset the scenario, verdicts and comment."""
        self.git('checkout', '-q', '--detach', self.base)
        self.git('clean', '-fdxq')
        self.git('reset', '-q', '--hard', self.base)
        for op in ops:
            kind, path = op[0], op[1]
            if kind == 'add':
                self.put(path)
            elif kind == 'mod':
                self.put(path, 'changed\n')
            elif kind == 'del':
                (self.author / path).unlink()
            elif kind == 'rename':
                (self.author / op[2]).parent.mkdir(parents=True, exist_ok=True)
                (self.author / path).rename(self.author / op[2])
            elif kind == 'symlink':
                (self.author / path).parent.mkdir(parents=True, exist_ok=True)
                (self.author / path).symlink_to('target')
            elif kind == 'chmod':
                (self.author / path).chmod(0o755)
            elif kind == 'typechange':
                (self.author / path).unlink()
                (self.author / path).symlink_to('target')
            elif kind == 'submodule':
                self.git('update-index', '--add', '--cacheinfo', f'160000,{self.base},{path}')
        self.git('add', '-A')
        self.git('commit', '-q', '--allow-empty', '-m', 'pr head')
        self.head = self.git('rev-parse', 'HEAD').strip()
        self.git('push', '-q', '--force', 'origin', f'{self.head}:refs/pull/{PR}/head')
        for f in self.notes.iterdir():
            f.unlink()
        self.sc = base_scenario(self.head, self.base)
        self.standards = self.add_note('Standards', 'rev-std')
        self.spec = self.add_note('Spec/security', 'rev-spec')
        self.comment()

    def add_note(self, kind, frm, **kw):
        self.counter += 1
        kw.setdefault('head', self.head)
        p = self.notes / f'2026100107{self.counter:04d}-{frm}-verdict.md'
        p.write_text(note_text(kind, frm, **kw))
        os.utime(p, (1000 + self.counter, 1000 + self.counter))
        return p

    def comment(self, notes=None, head=None):
        names = [p.name for p in (notes if notes is not None else [self.standards, self.spec])]
        self.sc['ticket']['comments'] = [{
            'author': 'cc13-pm', 'at': 'x', 'body': f'Enqueue head {head or self.head}: ' + '; '.join(names)}]

    def run_script(self, *extra, head=None, pr=str(PR)):
        self.scenario_path.write_text(json.dumps(self.sc))
        self.log.write_text('')
        gq = Path(str(self.log) + '.gq')
        if gq.exists():
            gq.unlink()
        return subprocess.run(
            [str(SCRIPT), pr, '--head', head or self.head, '--notes-dir', str(self.notes),
             '--poll-secs', '0.3', '--poll-interval', '0.05', *extra],
            text=True, capture_output=True, env=self.env, cwd=self.work)

    def calls(self):
        return [json.loads(l) for l in self.log.read_text().splitlines()]

    def merges(self):
        return [c for c in self.calls() if c[:3] == ['gh', 'pr', 'merge'] and '--auto' in c]

    def disables(self):
        return [c for c in self.calls() if c[:3] == ['gh', 'pr', 'merge'] and '--disable-auto' in c]

    def refused(self, *needles, extra=()):
        r = self.run_script(*extra)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        for n in needles:
            self.assertIn(n, r.stderr)
        self.assertEqual(self.merges(), [], 'a refused PR must never reach gh pr merge --auto')
        return r

    # ---- happy path and rollback -------------------------------------

    def test_happy_path_enqueues_pinned_head_and_verifies_queue(self):
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.merges(), [[
            'gh', 'pr', 'merge', str(PR), '-R', REPO, '--auto', '--squash',
            '--match-head-commit', self.head]])
        self.assertEqual(self.disables(), [])
        self.assertIn('merge queue', r.stdout)
        # It never records an approval, whatever the class.
        self.assertFalse([c for c in self.calls() if c[:3] == ['cadence', 'audit', 'approve']])

    def test_not_queued_disables_auto_and_fails(self):
        self.sc['graphql'] = [{'isInMergeQueue': False, 'state': 'OPEN'}]
        r = self.run_script()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertEqual(len(self.merges()), 1)
        self.assertEqual(len(self.disables()), 1)
        self.assertIn('did not enter the merge queue', r.stderr)

    def test_queue_entry_after_a_poll_is_accepted(self):
        self.sc['graphql'] = [{'isInMergeQueue': False, 'state': 'OPEN'},
                              {'isInMergeQueue': True, 'state': 'OPEN'}]
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.disables(), [])

    def test_failed_disable_auto_is_reported_loudly(self):
        self.sc['graphql'] = [{'isInMergeQueue': False, 'state': 'OPEN'}]
        self.sc['disable_rc'] = 1
        r = self.run_script()
        self.assertEqual(r.returncode, 1)
        self.assertIn('auto-merge may still be on', r.stderr)

    def test_merge_command_failure_rolls_back(self):
        self.sc['merge_rc'] = 1
        self.sc['merge_err'] = 'head changed'
        r = self.run_script()
        self.assertEqual(r.returncode, 1)
        self.assertIn('gh pr merge failed', r.stderr)
        self.assertEqual(len(self.disables()), 1)

    def test_dry_run_checks_everything_and_enqueues_nothing(self):
        r = self.run_script('--dry-run')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.merges(), [])
        self.assertIn('dry run', r.stdout)
        # A failing rule still refuses under --dry-run.
        self.sc['pr_view']['state'] = 'CLOSED'
        r = self.run_script('--dry-run')
        self.assertEqual(r.returncode, 1)

    # ---- PR state -----------------------------------------------------

    def test_refuses_pr_not_open(self):
        self.sc['pr_view']['state'] = 'MERGED'
        self.refused('is MERGED, not OPEN')

    def test_refuses_head_that_is_not_the_pin(self):
        self.sc['pr_view']['headRefOid'] = OLD
        self.refused(f'head is {OLD}, not the pinned {self.head}')

    def test_refuses_dirty_merge_state(self):
        self.sc['pr_view']['mergeStateStatus'] = 'DIRTY'
        self.refused('DIRTY')

    def test_refuses_auto_merge_already_on(self):
        self.sc['pr_view']['autoMergeRequest'] = {'enabledAt': 'now'}
        self.refused('auto-merge is already enabled')

    # ---- required checks ------------------------------------------------

    def test_refuses_missing_required_check(self):
        # The 10-01 bug: an invalid workflow yields no check at all.
        self.sc['pr_view']['statusCheckRollup'] = [check_run(c) for c in CHECKS if c != 'clippy']
        self.refused("required check 'clippy' is missing")

    def test_refuses_all_checks_missing(self):
        self.sc['pr_view']['statusCheckRollup'] = []
        r = self.refused("required check 'test' is missing")
        for c in CHECKS:
            self.assertIn(f"'{c}' is missing", r.stderr)

    def test_refuses_pending_required_check(self):
        self.sc['pr_view']['statusCheckRollup'][1] = check_run('fmt', conclusion=None, status='IN_PROGRESS')
        self.refused("required check 'fmt' is pending")

    def test_refuses_failed_required_check(self):
        self.sc['pr_view']['statusCheckRollup'][0] = check_run('test', conclusion='FAILURE')
        self.refused("required check 'test' is not successful (FAILURE)")

    def test_a_failed_rerun_beside_a_success_still_refuses(self):
        self.sc['pr_view']['statusCheckRollup'].append(check_run('build', conclusion='CANCELLED'))
        self.refused("required check 'build' is not successful")

    def test_status_context_success_satisfies_a_required_check(self):
        roll = [check_run(c) for c in CHECKS if c != 'ui']
        roll.append({'__typename': 'StatusContext', 'context': 'ui', 'state': 'SUCCESS'})
        self.sc['pr_view']['statusCheckRollup'] = roll
        self.assertEqual(self.run_script('--dry-run').returncode, 0)

    def test_refuses_when_no_required_check_can_be_found(self):
        self.sc['protection'] = None
        self.refused('no required checks found')

    def test_refuses_when_rulesets_cannot_be_read(self):
        self.sc['rulesets'] = None
        self.refused('cannot read the rulesets')

    def test_ruleset_required_checks_are_enforced_too(self):
        self.sc['protection'] = None
        self.sc['rulesets'] = [{'type': 'required_status_checks', 'parameters': {
            'required_status_checks': [{'context': 'extra-gate'}]}}]
        self.refused("required check 'extra-gate' is missing")

    # ---- verdict notes --------------------------------------------------

    def test_refuses_with_a_single_review_on_a_two_review_diff(self):
        self.spec.unlink()
        self.comment([self.standards])
        r = self.refused('no pass Spec/security verdict', 'two distinct reviewers')

    def test_refuses_without_a_standards_verdict(self):
        self.standards.unlink()
        self.comment([self.spec])
        self.refused('no pass Standards verdict')

    def test_refuses_the_same_reviewer_for_both_kinds(self):
        self.spec.unlink()
        self.add_note('Spec/security', 'rev-std')
        self.comment()
        self.refused('two distinct reviewers')

    def test_refuses_reviewer_who_is_the_github_author(self):
        self.sc['pr_view']['author']['login'] = 'rev-spec'
        self.refused('rev-spec is not independent')

    def test_refuses_reviewer_who_owns_the_ticket(self):
        self.sc['ticket']['owner'] = 'rev-std'
        self.refused('rev-std is not independent')

    def test_refuses_reviewer_named_by_author_flag(self):
        self.refused('rev-spec is not independent', extra=('--author', 'Rev-Spec'))

    def test_refuses_verdicts_pinned_to_an_older_head(self):
        for p in (self.standards, self.spec):
            p.unlink()
        self.standards = self.add_note('Standards', 'rev-std', head=OLD)
        self.spec = self.add_note('Spec/security', 'rev-spec', head=OLD)
        self.refused('no pass Standards verdict', 'no pass Spec/security verdict')

    def test_refuses_a_short_sha_in_the_verdict(self):
        self.spec.write_text(note_text('Spec/security', 'rev-spec', self.head).replace(f'head {self.head}', f'head {self.head[:7]}'))
        self.refused('no pass Spec/security verdict')

    def test_refuses_a_verdict_for_another_pr(self):
        self.spec.write_text(note_text('Spec/security', 'rev-spec', self.head, pr=PR + 1))
        self.refused('no pass Spec/security verdict')

    def test_refuses_a_verdict_for_another_issue(self):
        self.spec.write_text(note_text('Spec/security', 'rev-spec', self.head, issue='CAD-1'))
        self.refused('no pass Spec/security verdict')

    def test_refuses_a_blocking_verdict_on_this_head(self):
        self.spec.write_text(note_text('Spec/security', 'rev-spec', self.head, result='revise'))
        self.refused("verdict by rev-spec on this head is not a pass")

    def test_refuses_a_note_whose_title_and_section_disagree(self):
        self.spec.write_text(note_text('Spec/security', 'rev-spec', self.head, result='pass', section='revise'))
        self.refused("title 'pass', section 'revise'")

    def test_a_later_pass_by_the_same_reviewer_replaces_an_earlier_revise(self):
        self.spec.write_text(note_text('Spec/security', 'rev-spec', self.head, result='revise'))
        os.utime(self.spec, (500, 500))
        again = self.add_note('Spec/security', 'rev-spec')
        self.comment([self.standards, again])
        self.assertEqual(self.run_script('--dry-run').returncode, 0)

    def test_refuses_an_unreadable_notes_dir(self):
        self.notes.rename(self.root / 'moved')
        self.refused('cannot read the notes dir')

    # ---- ticket comment -------------------------------------------------

    def test_refuses_without_a_ticket_comment(self):
        self.sc['ticket']['comments'] = []
        self.refused('no ticket comment names head')

    def test_refuses_a_comment_for_another_head(self):
        self.comment(head=OLD)
        self.refused('no ticket comment names head')

    def test_refuses_a_comment_that_omits_a_verdict(self):
        self.comment([self.standards])
        self.refused('no ticket comment names head', self.spec.name)

    def test_refuses_when_the_ticket_cannot_be_read(self):
        self.sc['ticket'] = 'not json'
        self.refused('cannot read ticket CAD-959')

    def test_refuses_a_title_without_an_issue_id(self):
        self.sc['pr_view']['title'] = 'no ticket here'
        self.refused('no issue id')

    # ---- human class ------------------------------------------------------

    def human(self):
        self.build_head([('add', 'scripts/enqueue-reviewed')])

    def test_human_diff_without_approval_is_refused(self):
        self.human()
        self.refused('no operator approval recorded for exactly head')
        self.assertTrue([c for c in self.calls() if c[:3] == ['cadence', 'audit', 'approval']])

    def test_human_diff_with_approval_for_another_head_is_refused(self):
        self.human()
        self.sc['approval'] = {'state': 'missing', 'reason': f'no merge approval names head {self.head[:9]}'}
        self.refused('no operator approval recorded')

    def test_human_diff_with_revoked_approval_is_refused(self):
        self.human()
        self.sc['approval'] = {'state': 'revoked'}
        self.refused('no operator approval recorded')

    def test_human_diff_with_unreadable_approval_is_refused(self):
        self.human()
        self.sc['approval'] = 'garbage'
        self.refused('cannot read the operator approval')

    def test_approval_state_must_be_in_force_even_with_rc_zero(self):
        self.human()
        self.sc['approval'] = {'state': 'unknown'}
        self.sc['approval_rc'] = 0
        self.refused('no operator approval recorded')

    def test_human_diff_with_approval_in_force_enqueues(self):
        self.human()
        self.sc['approval'] = {'state': 'in-force', 'approval_id': 'merge-pr77-feae08caaaaa'}
        self.sc['approval_rc'] = 0
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        approval = [c for c in self.calls() if c[:3] == ['cadence', 'audit', 'approval']]
        self.assertEqual(approval, [['cadence', 'audit', 'approval', '--pr', str(PR),
                                     '--head', self.head, '--repo', REPO]])
        self.assertFalse([c for c in self.calls() if c[:3] == ['cadence', 'audit', 'approve']])

    def test_each_human_trigger_path_demands_approval(self):
        for path in ('.github/workflows/ci.yml', 'Cargo.toml', 'Cargo.lock', 'ui/package.json',
                     'cadence-review.toml', 'src/review.rs', 'docs/roles/new.md',
                     'docs/TEAM.md', 'docs/CHARTER.md', 'AGENTS.md', 'scripts/pre-push',
                     'crates/x/Cargo.toml'):
            self.build_head([('add', path) if not (self.author / path).exists() else ('mod', path)])
            r = self.run_script()
            self.assertEqual(r.returncode, 1, path)
            self.assertIn('no operator approval recorded', r.stderr, path)

    def test_a_human_risk_verdict_demands_approval_on_a_non_human_path(self):
        self.spec.write_text(note_text('Spec/security', 'rev-spec', self.head, risk='human (1) — trust boundary'))
        self.refused('no operator approval recorded')

    def test_a_non_human_diff_never_reads_approvals(self):
        self.assertEqual(self.run_script('--dry-run').returncode, 0)
        self.assertFalse([c for c in self.calls() if c[:3] == ['cadence', 'audit', 'approval']])

    # ---- one-review path (CAD-957) ---------------------------------------

    def one_review(self, ops, toml=TOML, note_kind='Review (standards+spec)'):
        self.build_head(ops)
        self.sc['toml'] = toml
        self.standards.unlink()
        self.spec.unlink()
        self.single = self.add_note(note_kind, 'rev-one')
        self.comment([self.single])

    def assert_two_reviews(self, ops, toml=TOML):
        self.one_review(ops, toml)
        self.refused('no pass Standards verdict', 'no pass Spec/security verdict')

    def test_one_review_suffices_when_every_change_is_an_allowed_add_or_modify(self):
        self.one_review([('add', 'tests/scripts/test_x.py'), ('add', 'docs/guides/g2.md'),
                         ('mod', 'docs/guides/guide.md'), ('mod', 'tests/old.py'),
                         ('add', 'docs/ui/a/b.md')])
        r = self.run_script('--dry-run')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn('reviews required: one', r.stdout)

    def test_a_combined_note_does_not_satisfy_a_two_review_diff(self):
        self.one_review([('add', 'src/new.rs')])
        self.refused('no pass Standards verdict', 'no pass Spec/security verdict')

    def test_one_review_still_needs_one_independent_pass(self):
        self.one_review([('add', 'docs/guides/g2.md')])
        self.single.unlink()
        self.comment([])
        self.refused('no pass verdict for CAD-959 pinned to this head (one review is required)')

    def test_one_review_reviewer_must_not_be_the_author(self):
        self.one_review([('add', 'docs/guides/g2.md')])
        self.sc['ticket']['owner'] = 'rev-one'
        self.refused('rev-one is not independent')

    def test_one_review_list_is_read_from_the_base_branch(self):
        self.one_review([('add', 'docs/guides/g2.md')])
        self.run_script('--dry-run')
        [c] = [c for c in self.calls() if any('contents/docs/roles/one-review-paths.toml' in a for a in c)]
        self.assertIn('ref=main', c[-1])

    def test_one_path_off_the_list_needs_two_reviews(self):
        self.assert_two_reviews([('add', 'docs/guides/g2.md'), ('add', 'src/lib.rs')])

    def test_exclude_wins_over_include(self):
        self.assert_two_reviews([('mod', 'docs/roles/risk-classes.md')])
        self.assert_two_reviews([('mod', 'AGENTS.md')])

    def test_docs_outside_the_allowlist_need_two_reviews(self):
        self.assert_two_reviews([('mod', 'docs/other.md')])

    def test_base_advancing_after_the_branch_point_does_not_widen_the_diff(self):
        self.one_review([('add', 'docs/guides/g2.md')])
        # main moves on with a source change the PR does not contain.
        self.git('checkout', '-q', '-B', 'main', self.base)
        self.put('src/moved.rs')
        self.git('add', '-A')
        self.git('commit', '-q', '-m', 'main moves')
        self.git('push', '-q', 'origin', 'main')
        self.sc['pr_view']['baseRefOid'] = self.git('rev-parse', 'HEAD').strip()
        r = self.run_script('--dry-run')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn('reviews required: one', r.stdout)

    def test_a_delete_needs_two_reviews(self):
        self.assert_two_reviews([('del', 'docs/guides/guide.md')])

    def test_a_rename_needs_two_reviews(self):
        self.assert_two_reviews([('rename', 'docs/guides/guide.md', 'docs/guides/guide-renamed.md')])

    def test_a_rename_out_of_the_list_needs_two_reviews(self):
        self.assert_two_reviews([('rename', 'docs/guides/guide.md', 'src/guide.md')])

    def test_a_mode_change_needs_two_reviews(self):
        self.assert_two_reviews([('chmod', 'docs/guides/guide.md')])

    def test_a_type_change_needs_two_reviews(self):
        self.assert_two_reviews([('typechange', 'docs/guides/guide.md')])

    def test_an_added_symlink_needs_two_reviews(self):
        self.assert_two_reviews([('symlink', 'docs/guides/link.md')])

    def test_an_added_submodule_needs_two_reviews(self):
        self.assert_two_reviews([('submodule', 'docs/guides/sub')])

    def test_missing_one_review_file_means_two_reviews(self):
        self.assert_two_reviews([('add', 'docs/guides/g2.md')], toml=None)

    def test_unreadable_one_review_file_means_two_reviews(self):
        self.assert_two_reviews([('add', 'docs/guides/g2.md')], toml='one_review_include = [')

    def test_an_empty_diff_is_refused(self):
        self.build_head([])
        self.refused('empty diff')

    def test_a_pr_head_that_cannot_be_fetched_is_refused(self):
        self.git('push', '-q', 'origin', f':refs/pull/{PR}/head')
        self.refused('cannot list the changed files')

    def test_a_dot_slash_or_backslash_path_needs_two_reviews(self):
        toml = 'one_review_include = ["**"]\n'
        self.assert_two_reviews([('add', 'tests/a\\b.py')], toml=toml)

    # ---- usage ------------------------------------------------------------

    def test_head_must_be_the_full_sha(self):
        self.scenario_path.write_text(json.dumps(self.sc))
        for bad in (self.head[:7], self.head.upper()[:39], 'x' * 40):
            r = subprocess.run([str(SCRIPT), str(PR), '--head', bad], text=True,
                               capture_output=True, env=self.env, cwd=self.work)
            self.assertEqual(r.returncode, 2, (bad, r.stderr))

    def test_every_failing_rule_is_listed_one_per_line(self):
        self.sc['pr_view']['state'] = 'CLOSED'
        self.sc['pr_view']['mergeStateStatus'] = 'DIRTY'
        self.sc['pr_view']['statusCheckRollup'] = []
        r = self.refused()
        lines = [l for l in r.stderr.splitlines() if 'refused:' in l]
        self.assertGreaterEqual(len(lines), 7, r.stderr)


class OneReviewGlobTest(unittest.TestCase):
    """The pure path-list semantics, without the fakes."""

    @classmethod
    def setUpClass(cls):
        loader = importlib.machinery.SourceFileLoader('enqueue_reviewed', str(SCRIPT))
        spec = importlib.util.spec_from_loader('enqueue_reviewed', loader)
        cls.mod = importlib.util.module_from_spec(spec)
        loader.exec_module(cls.mod)

    def q(self, paths, toml=TOML, status='M', old='100644', new='100644'):
        return self.mod.one_review_qualifies([(status, old, new, p) for p in paths], toml)

    def test_star_stays_within_a_segment_and_double_star_crosses(self):
        toml = 'one_review_include = ["docs/*.md", "tests/**"]\n'
        self.assertTrue(self.q(['docs/a.md'], toml))
        self.assertFalse(self.q(['docs/sub/a.md'], toml))
        self.assertTrue(self.q(['tests/a/b/c.py'], toml))
        self.assertTrue(self.q(['tests/c.py'], toml))
        self.assertFalse(self.q(['testsx/c.py'], toml))

    def test_no_include_list_qualifies_nothing(self):
        self.assertFalse(self.q(['docs/a.md'], 'one_review_exclude = []\n'))

    def test_globs_are_anchored_at_the_repo_root(self):
        self.assertFalse(self.q(['src/tests/a.py']))
        self.assertFalse(self.q(['x/docs/a.md']))

    def test_matching_is_case_sensitive(self):
        self.assertFalse(self.q(['Docs/a.md']))
        self.assertFalse(self.q(['TESTS/a.py']))

    def test_odd_paths_disqualify_even_under_a_catch_all_include(self):
        toml = 'one_review_include = ["**"]\n'
        for bad in ('./tests/a.py', '../tests/a.py', 'tests/../src/a.py', 'tests//a.py',
                    '/tests/a.py', 'tests\\a.py', 'tests/./a.py', ''):
            self.assertFalse(self.q([bad], toml), repr(bad))
        self.assertTrue(self.q(['tests/a.py'], toml))

    def test_only_plain_adds_and_modifies_of_regular_files_qualify(self):
        self.assertTrue(self.q(['tests/a.py']))
        self.assertTrue(self.q(['tests/a.py'], status='A', old='000000', new='100755'))
        for status in ('D', 'R', 'T', 'C', 'U', 'X'):
            self.assertFalse(self.q(['tests/a.py'], status=status), status)
        self.assertFalse(self.q(['tests/a.py'], old='100644', new='100755'))  # mode change
        self.assertFalse(self.q(['tests/a.py'], old='120000', new='120000'))  # symlink
        self.assertFalse(self.q(['tests/a.py'], status='A', old='000000', new='120000'))
        self.assertFalse(self.q(['tests/a.py'], status='A', old='000000', new='160000'))  # submodule
        self.assertFalse(self.q(['tests/a.py'], status='A', old='100644', new='100644'))

    def test_one_bad_change_among_good_ones_disqualifies(self):
        mixed = [('M', '100644', '100644', 'tests/a.py'), ('D', '100644', '000000', 'tests/b.py')]
        self.assertFalse(self.mod.one_review_qualifies(mixed, TOML))

    def test_empty_diff_and_missing_list_do_not_qualify(self):
        self.assertFalse(self.mod.one_review_qualifies([], TOML))
        self.assertFalse(self.q(['tests/a.py'], None))

    def test_raw_diff_parser_reads_modes_and_nul_separated_paths(self):
        raw = ':000000 100644 0000 1111 A\0tests/a b.py\0:100644 120000 2222 3333 T\0docs/x.md\0'
        self.assertEqual(self.mod.parse_raw_diff(raw), [
            ('A', '000000', '100644', 'tests/a b.py'), ('T', '100644', '120000', 'docs/x.md')])


if __name__ == '__main__':
    unittest.main()
