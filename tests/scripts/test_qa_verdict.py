"""qa-verdict.sh regressions run against a fake `gh` on PATH; nothing reaches GitHub."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / 'scripts' / 'qa-verdict.sh'
HEAD = 'feae08c' + 'a' * 33
OTHER = '1234567' + 'b' * 33

FAKE_GH = r'''#!/usr/bin/env bash
# Records each call as one tab-joined line, then answers from the environment.
(IFS=$'\t'; printf '%s\n' "$*") >> "$FAKE_GH_LOG"
case "$1 ${2:-}" in
  "pr view") printf '%s\n' "$FAKE_GH_HEAD" ;;
  "repo view") echo "favcrm/cadence" ;;
  "api --method") echo '{}' ;;
  "api "*) printf '%s\n' "$FAKE_GH_STATE" ;;
  *) echo "fake gh: unexpected call: $*" >&2; exit 64 ;;
esac
'''


class QaVerdictTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        bindir = self.root / 'bin'
        bindir.mkdir()
        fake = bindir / 'gh'
        fake.write_text(FAKE_GH)
        fake.chmod(0o755)
        self.log = self.root / 'gh.log'
        self.log.touch()
        self.env = {**os.environ, 'PATH': f"{bindir}{os.pathsep}{os.environ['PATH']}",
                    'FAKE_GH_LOG': str(self.log), 'FAKE_GH_HEAD': HEAD,
                    'FAKE_GH_STATE': 'none'}

    def tearDown(self):
        self.temp.cleanup()

    def run_script(self, *args, **env):
        return subprocess.run([str(SCRIPT), *args], text=True, capture_output=True,
                              env={**self.env, **env})

    def calls(self):
        return [line.split('\t') for line in self.log.read_text().splitlines()]

    def posts(self):
        return [call for call in self.calls() if call[:2] == ['api', '--method']]

    def note(self, name='20260918-095830-319f2-demo-verdict.md',
             text=f'# Verdict: demo (PR #30 @ {HEAD[:7]}) — pass\n\n## Verdict\npass\n'):
        path = self.root / name
        path.write_text(text)
        return str(path)

    def assert_refused(self, result, message):
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(message, result.stderr)
        self.assertEqual(self.posts(), [])

    def test_pass_posts_success_to_the_head_sha(self):
        note = self.note()
        result = self.run_script('30', 'pass', '--note', note, '--repo', 'favcrm/cadence')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.posts(), [[
            'api', '--method', 'POST', f'repos/favcrm/cadence/statuses/{HEAD}',
            '-f', 'context=qa-verdict', '-f', 'state=success',
            '-f', f'description=pass — {Path(note).name}']])
        self.assertNotIn('target_url', self.log.read_text())

    def test_blocked_posts_failure(self):
        result = self.run_script('30', 'blocked', '--note', self.note())
        self.assertEqual(result.returncode, 0, result.stderr)
        [post] = self.posts()
        self.assertIn(f'repos/favcrm/cadence/statuses/{HEAD}', post)
        self.assertIn('state=failure', post)

    def test_matching_sha_is_accepted_short_or_full(self):
        for sha in (HEAD, HEAD[:7], HEAD[:7].upper()):
            result = self.run_script('30', 'pass', '--note', self.note(), '--sha', sha)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.posts()), 3)

    def test_stale_sha_is_refused(self):
        result = self.run_script('30', 'pass', '--note', self.note(), '--sha', OTHER)
        self.assert_refused(result, 'stale review')

    def test_head_moved_after_the_note_is_refused(self):
        result = self.run_script('30', 'pass', '--note', self.note(), FAKE_GH_HEAD=OTHER)
        self.assert_refused(result, 'does not name head 1234567')

    def test_missing_note_is_refused(self):
        result = self.run_script('30', 'pass', '--note', str(self.root / 'gone-verdict.md'))
        self.assert_refused(result, 'verdict note not found')

    def test_relative_note_is_refused(self):
        result = self.run_script('30', 'pass', '--note', 'demo-verdict.md')
        self.assert_refused(result, 'absolute path')

    def test_wrong_note_type_is_refused(self):
        qa = self.note(name='20260918-095830-319f2-demo-qa.md')
        self.assert_refused(self.run_script('30', 'pass', '--note', qa), 'not a verdict note')
        untitled = self.note(text=f'# QA: demo (PR #30 @ {HEAD[:7]})\n')
        self.assert_refused(self.run_script('30', 'pass', '--note', untitled), 'not a verdict note')

    def test_note_naming_another_pr_is_refused(self):
        note = self.note(text=f'# Verdict: demo (PR #301 @ {HEAD[:7]}) — pass\n')
        self.assert_refused(self.run_script('30', 'pass', '--note', note), 'does not name PR #30')

    def test_pr_named_by_url_is_accepted(self):
        note = self.note(text=f'# Verdict: demo — pass\nhttps://github.com/favcrm/cadence/pull/30 at `{HEAD[:7]}`\n')
        result = self.run_script('30', 'pass', '--note', note)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_description_is_capped_at_140_characters(self):
        note = self.note(name='x' * 200 + '-verdict.md')
        result = self.run_script('30', 'pass', '--note', note)
        self.assertEqual(result.returncode, 0, result.stderr)
        [post] = self.posts()
        description = post[-1].removeprefix('description=')
        self.assertLessEqual(len(description), 140)
        self.assertTrue(description.startswith('pass — xxx'))

    def test_unresolvable_head_is_refused(self):
        result = self.run_script('30', 'pass', '--note', self.note(), FAKE_GH_HEAD='')
        self.assert_refused(result, 'could not resolve the head SHA')

    def test_usage_errors_exit_2_without_posting(self):
        for args in (['30'], ['30', 'maybe', '--note', self.note()], ['30', 'pass'],
                     ['abc', 'pass', '--note', self.note()], ['--check'],
                     ['--check', '30', '--note', self.note()]):
            result = self.run_script(*args)
            self.assertEqual(result.returncode, 2, args)
        self.assertEqual(self.posts(), [])

    def test_check_exits_0_only_on_success(self):
        for state, code in (('success', 0), ('failure', 1), ('pending', 1), ('none', 1)):
            result = self.run_script('--check', '30', FAKE_GH_STATE=state)
            self.assertEqual(result.returncode, code, state)
            self.assertIn(f'qa-verdict: {state}', result.stdout)
            self.assertIn(HEAD[:7], result.stdout)
        self.assertEqual(self.posts(), [])

    def test_check_reads_the_status_of_the_current_head(self):
        self.run_script('--check', '30', '--repo', 'favcrm/cadence', FAKE_GH_HEAD=OTHER)
        reads = [call for call in self.calls() if call[0] == 'api']
        self.assertEqual(len(reads), 1)
        self.assertEqual(reads[0][1], f'repos/favcrm/cadence/commits/{OTHER}/status?per_page=100')


if __name__ == '__main__':
    unittest.main()
