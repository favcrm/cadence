"""Regression checks use temporary notes; never prune the real note directory."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import unittest

SCRIPTS = Path(__file__).resolve().parents[1] / 'scripts'

class NotesTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.env = {**os.environ, 'AGENT_NOTES_DIR': str(self.root)}

    def tearDown(self):
        self.temp.cleanup()

    def run_script(self, name, *args, text=None):
        return subprocess.run([str(SCRIPTS / name), *args], input=text,
                              text=True, capture_output=True, check=True, env=self.env)

    def note(self, stamp, session, kind):
        path = self.root / f'{stamp}-{session}-test-{kind}.md'
        path.write_text('# Test note\n')
        old = time.time() - 40 * 86400
        os.utime(path, (old, old))
        return path

    def test_reopened_chain_is_not_pruned(self):
        closed = self.note('20260101-000000', 'abcde', 'verdict')
        reopened = self.note('20260102-000000', 'abcde', 'kickoff')
        self.run_script('notes-prune.sh')
        self.assertTrue(closed.exists())
        self.assertTrue(reopened.exists())
        self.assertRegex((self.root / 'index.html').read_text(), r'abcde</code>.*?open')

    def test_closed_old_chain_is_pruned(self):
        first = self.note('20260101-000000', 'abcde', 'kickoff')
        last = self.note('20260102-000000', 'abcde', 'verdict')
        self.run_script('notes-prune.sh')
        self.assertFalse(first.exists())
        self.assertFalse(last.exists())

    def test_same_second_legacy_ambiguity_is_retained(self):
        first = self.note('20260101-000000', 'abcde', 'kickoff')
        last = self.note('20260101-000000', 'abcde', 'verdict')
        self.run_script('notes-prune.sh')
        self.assertTrue(first.exists())
        self.assertTrue(last.exists())

    def test_concurrent_publications_remain_indexable(self):
        processes = [subprocess.Popen([str(SCRIPTS / 'note-publish.sh'), 'abcde', 'same', 'qa', '-'],
                      stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                      text=True, env=self.env) for _ in range(2)]
        for process in processes:
            process.stdin.write('# Concurrent note\n')
            process.stdin.close()
            process.stdin = None
        paths = []
        for process in processes:
            out, err = process.communicate(timeout=15)
            self.assertEqual(process.returncode, 0, err)
            paths.append(Path(out.strip()))
        self.assertEqual(len(set(paths)), 2)
        index = (self.root / 'index.html').read_text()
        for path in paths:
            self.assertRegex(path.name, r'^\d{8}-\d{6}-abcde-same-qa\.md$')
            self.assertIn(path.name, index)

if __name__ == '__main__':
    unittest.main()
