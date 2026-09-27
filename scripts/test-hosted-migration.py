#!/usr/bin/env python3
"""Run with python3; optional CADENCE_REHEARSAL_BINARY exercises the real CLI."""
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('migration', Path(__file__).with_name('rehearse-hosted-migration.py'))
migration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(migration)

TOKEN_REMOTE = 'https://user:ghp_exampletoken0000000000000000000000@host.invalid/x'


def index_fingerprint(repo):
    """The bytes and the exact mtime of a repository's index."""
    index = Path(repo) / '.git' / 'index'
    if not index.exists():
        return (None, None)
    info = index.stat()
    return (hashlib.sha256(index.read_bytes()).hexdigest(), info.st_mtime_ns)


class MigrationTests(unittest.TestCase):
    def setUp(self):
        # Always a real /tmp root: the tool refuses state outside /tmp, and CI
        # may point TMPDIR elsewhere.
        self.temp = tempfile.TemporaryDirectory(prefix='cad529-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / 'source'
        self.source.mkdir()
        (self.source / 'cadence.lock').touch()
        self.db = self.source / 'cadence.sqlite3'
        with sqlite3.connect(self.db) as db:
            db.executescript('''
                CREATE TABLE schema_version(version INTEGER NOT NULL);
                INSERT INTO schema_version VALUES(1);
                CREATE TABLE agents(alias TEXT PRIMARY KEY, cwd TEXT, generation TEXT, pid INTEGER, pid_start REAL);
                CREATE TABLE messages(id TEXT PRIMARY KEY, state TEXT, turn_id TEXT, body TEXT);
                INSERT INTO messages VALUES('queued-example', 'queued', NULL, 'perform local work');
            ''')
        self.tracker = self.root / 'tracker'
        self.make_repo(self.tracker)
        self.iso_temp = tempfile.TemporaryDirectory(prefix='cad529-iso-', dir='/tmp')
        self.addCleanup(self.iso_temp.cleanup)
        self.iso = migration.Isolation(self.iso_temp.name)

    def make_repo(self, path, remote=None):
        path.mkdir(parents=True, exist_ok=True)
        self.git(path, 'init', '-q')
        # Distinct content per repo: two fixtures must never share a commit sha,
        # or a test that proves a decoy was ignored would pass by coincidence.
        (path / 'README.md').write_text(f'isolated rehearsal tracker {path.name}\n')
        (path / 'work.txt').write_text(f'tracked working file {path.name}\n')
        self.git(path, 'add', 'README.md', 'work.txt')
        self.git(path, '-c', 'user.name=Rehearsal', '-c', 'user.email=rehearsal@example.invalid',
                 '-c', 'core.hooksPath=/dev/null', 'commit', '-qm', f'fixture {path.name}')
        if remote:
            self.git(path, 'remote', 'add', 'origin', remote)
        return path

    def live_shaped(self, repo=None):
        """Make the index's cached stat data stale, as in a live checkout.

        git only rewrites `.git/index` when a read has something to refresh;
        a fixture that never refreshes could not detect an unguarded write.
        """
        repo = repo or self.tracker
        for name in ('README.md', 'work.txt'):
            target = repo / name
            stamp = target.stat().st_mtime_ns + 5_000_000_000
            os.utime(target, ns=(stamp, stamp))

    def git(self, repo, *args):
        return subprocess.run(['git', '-C', str(repo), *args], check=True,
                              capture_output=True, text=True).stdout.strip()

    def bundle(self, repos=None):
        bundle = self.root / 'bundle'
        bundle.mkdir()
        data = self.db.read_bytes()
        (bundle / 'cadence.sqlite3').write_bytes(data)
        manifest = {
            'format': 'cadence.backup/1', 'kind': 'export', 'db_file': 'cadence.sqlite3',
            'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data),
            'schema_version': 1, 'export': {'contains': ['store'], 'excludes': ['runtime files']},
        }
        if repos is not None:
            manifest['repos'] = repos
        (bundle / 'manifest.json').write_text(json.dumps(manifest))
        return bundle

    def fake_cli(self, repos=None):
        """Stand in for export/restore so CLI-free tests reach the later checks."""
        real_run = migration.Isolation.run

        def dispatch(iso, *args, index=None):
            if 'export' in args:
                out = Path(args[args.index('--out') + 1])
                self.bundle(repos=repos() if callable(repos) else repos).rename(out)
                return ''
            if 'restore' in args:
                state = Path(args[args.index('--state-dir') + 1])
                state.mkdir(parents=True)
                source = Path(args[-1])
                (state / 'cadence.sqlite3').write_bytes((source / 'cadence.sqlite3').read_bytes())
                return ''
            return real_run(iso, *args, index=index)
        return patch.object(migration.Isolation, 'run', autospec=True, side_effect=dispatch)

    # ---------- inventory and bundle ----------

    def test_preserves_queued_inventory_without_claiming_delivery(self):
        inventory, _ = migration.verify_bundle(self.bundle())
        self.assertEqual(inventory['message_states'], {'queued': 1})

    def test_refuses_inflight_and_uncertain_effects(self):
        for state in ['running', 'submitting', 'unknown']:
            with self.subTest(state=state), sqlite3.connect(self.db) as db:
                db.execute('UPDATE messages SET state=?', [state])
            with self.assertRaisesRegex(ValueError, 'reconciliation'):
                migration.inspect_db(self.db)

    def test_refuses_live_actor_credentials(self):
        with sqlite3.connect(self.db) as db:
            db.execute("UPDATE messages SET turn_id='live-token'")
        with self.assertRaisesRegex(ValueError, 'messages.turn_id'):
            migration.verify_bundle(self.bundle())

    def test_refuses_corruption(self):
        bundle = self.bundle()
        manifest = json.loads((bundle / 'manifest.json').read_text())
        manifest['sha256'] = '0' * 64
        (bundle / 'manifest.json').write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, 'size/hash'):
            migration.verify_bundle(bundle)

    def test_refuses_dirty_tracker_and_non_temporary_state(self):
        (self.tracker / 'untracked').write_text('must not disappear')
        with self.assertRaisesRegex(ValueError, 'clean'):
            migration.clean_tracker(self.iso, self.tracker)
        with self.assertRaisesRegex(ValueError, '/tmp'):
            migration.temporary(Path.home() / '.local/state/cadence')

    def test_refuses_foreign_owned_inputs(self):
        with patch.object(migration.os, 'getuid', return_value=os.getuid() + 1):
            with self.assertRaisesRegex(ValueError, 'owned by the current user'):
                migration.rehearse('/usr/bin/true', self.source, self.tracker, self.root / 'out')
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_active_source_daemon_before_creating_output(self):
        with (self.source / 'cadence.lock').open('rb') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(ValueError, 'daemon is active'):
                migration.rehearse('/usr/bin/true', self.source, self.tracker, self.root / 'out')
            with self.assertRaisesRegex(ValueError, 'daemon is active'):
                migration.rehearse('/usr/bin/true', self.source, self.tracker,
                                   self.root / 'out', dry_run=True)
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_same_count_source_mutation_during_export(self):
        real_run = migration.Isolation.run

        def mutate(iso, *args, index=None):
            if 'export' in args:
                self.bundle().rename(self.root / 'out/bundle')
                with sqlite3.connect(self.db) as db:
                    db.execute("UPDATE messages SET body='changed during export'")
                return ''
            return real_run(iso, *args, index=index)
        with patch.object(migration.Isolation, 'run', autospec=True, side_effect=mutate):
            with self.assertRaisesRegex(ValueError, 'source inventory changed'):
                migration.rehearse('/usr/bin/true', self.source, self.tracker, self.root / 'out')
        self.assertFalse((self.root / 'out/receipt.json').exists())

    # ---------- 1: the source repository's index is never written ----------

    def test_reading_a_live_tracker_never_writes_its_index(self):
        self.live_shaped()
        before = index_fingerprint(self.tracker)
        self.assertIsNotNone(before[0])
        sha = migration.clean_tracker(self.iso, self.tracker)
        self.assertEqual(sha, self.git(self.tracker, 'rev-parse', 'HEAD'))
        self.assertEqual(index_fingerprint(self.tracker), before,
                         'clean_tracker rewrote the live index')
        # The fixture is live-shaped: the unguarded command this replaced does
        # rewrite the index, so the assertion above is load-bearing.
        subprocess.run(['git', '-C', str(self.tracker), 'status', '--porcelain'],
                       check=True, capture_output=True)
        self.assertNotEqual(index_fingerprint(self.tracker), before,
                            'fixture never refreshes its index; the guard is untested')

    def test_full_rehearsal_leaves_the_source_index_byte_identical(self):
        self.live_shaped()
        before = index_fingerprint(self.tracker)
        with self.fake_cli():
            receipt = migration.rehearse('/usr/bin/true', self.source, self.tracker,
                                         self.root / 'out')
        self.assertEqual(receipt['mode'], 'offline-rehearsal')
        self.assertEqual(index_fingerprint(self.tracker), before,
                         'the rehearsal rewrote the tracker index')

    def test_borrowed_index_absorbs_the_write_instead_of_the_repository(self):
        self.live_shaped()
        before = index_fingerprint(self.tracker)
        with self.iso.borrowed_index(self.tracker) as copy:
            self.assertEqual(copy.read_bytes(), (self.tracker / '.git/index').read_bytes())
            self.iso.run('git', '-C', str(self.tracker), 'status', '--porcelain', index=copy)
            borrowed_after = hashlib.sha256(copy.read_bytes()).hexdigest()
        self.assertNotEqual(borrowed_after, before[0], 'the copy absorbed no write')
        self.assertEqual(index_fingerprint(self.tracker), before)

    # ---------- 2: children never inherit the caller's git/cadence env ----------

    def poisoned(self):
        decoys = self.root / 'decoys'
        self.make_repo(decoys / 'other-repo')
        (decoys / 'pm').mkdir(parents=True, exist_ok=True)
        (decoys / 'state').mkdir(parents=True, exist_ok=True)
        return {
            'GIT_DIR': str(decoys / 'other-repo/.git'),
            'GIT_WORK_TREE': str(decoys / 'other-repo'),
            'GIT_INDEX_FILE': str(decoys / 'poisoned-index'),
            'GIT_OBJECT_DIRECTORY': str(decoys / 'other-repo/.git/objects'),
            'GIT_CONFIG_GLOBAL': str(decoys / 'gitconfig'),
            'CADENCE_STATE_DIR': str(decoys / 'state'),
            'CADENCE_PM_DIR': str(decoys / 'pm'),
            'CADENCE_ALIAS': 'not-the-rehearsal',
        }

    def test_child_environment_drops_git_and_cadence_variables(self):
        poison = self.poisoned()
        with patch.dict(os.environ, poison):
            env = self.iso.env()
        self.assertNotIn('GIT_DIR', env)
        self.assertNotIn('GIT_INDEX_FILE', env)
        self.assertNotIn('CADENCE_STATE_DIR', env)
        self.assertNotIn('CADENCE_PM_DIR', env)
        self.assertEqual(env['GIT_CONFIG_GLOBAL'], '/dev/null')
        self.assertNotEqual(env['HOME'], os.environ.get('HOME'))
        self.assertTrue(set(env) - {'PATH'} <= {
            'HOME', 'TMPDIR', 'LC_ALL', 'GIT_CONFIG_NOSYSTEM', 'GIT_CONFIG_GLOBAL',
            'GIT_CONFIG_SYSTEM', 'GIT_TERMINAL_PROMPT', 'GIT_ASKPASS'})

    def test_poisoned_parent_environment_cannot_redirect_the_rehearsal(self):
        self.live_shaped()
        poison = self.poisoned()
        truth = self.git(self.tracker, 'rev-parse', 'HEAD')
        before = index_fingerprint(self.tracker)
        with patch.dict(os.environ, poison):
            self.assertEqual(migration.clean_tracker(self.iso, self.tracker), truth)
            with self.fake_cli():
                receipt = migration.rehearse('/usr/bin/true', self.source, self.tracker,
                                             self.root / 'out')
        self.assertEqual(receipt['tracker_sha'], truth)
        self.assertEqual(index_fingerprint(self.tracker), before)
        self.assertFalse(Path(poison['GIT_INDEX_FILE']).exists(),
                         'a child wrote the inherited GIT_INDEX_FILE decoy')
        self.assertEqual(list((self.root / 'decoys/pm').iterdir()), [])
        self.assertEqual(list((self.root / 'decoys/state').iterdir()), [])
        # Without the scrub the decoy GIT_DIR wins over `-C`: prove it would.
        answer = subprocess.run(['git', '-C', str(self.tracker), 'rev-parse', 'HEAD'],
                                capture_output=True, text=True,
                                env={**os.environ, **poison})
        self.assertNotEqual(answer.stdout.strip(), truth,
                            'the poison is inert; the env guard is untested')

    # ---------- 3: no credentialed remote reaches any output ----------

    def test_strip_credentials_matches_the_rust_rule(self):
        self.assertEqual(migration.strip_credentials(TOKEN_REMOTE), 'https://host.invalid/x')
        self.assertEqual(migration.strip_credentials('git@host.invalid:o/r.git'),
                         'git@host.invalid:o/r.git')
        self.assertEqual(migration.strip_credentials('https://host.invalid/x'),
                         'https://host.invalid/x')

    def test_refuses_a_manifest_or_receipt_carrying_userinfo(self):
        with self.assertRaisesRegex(ValueError, 'credentialed URL'):
            migration.verify_bundle(self.bundle(repos=[{'path': '/x', 'remote': TOKEN_REMOTE}]))
        with self.assertRaisesRegex(ValueError, 'credentialed URL'):
            migration.assert_no_credentials('receipt', json.dumps({'remote': TOKEN_REMOTE}))
        migration.assert_no_credentials('receipt', json.dumps({'remote': 'https://host.invalid/x'}))

    def test_token_remote_never_reaches_stdout_or_the_receipt(self):
        secret = TOKEN_REMOTE.split('@')[0].split(':')[-1]
        self.git(self.tracker, 'remote', 'add', 'origin', TOKEN_REMOTE)
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['tracked', str(self.tracker)])

        def recorded():
            # What the CLI records: discover_repos strips the userinfo.
            remote = migration.strip_credentials(
                subprocess.run(['git', '-C', str(self.tracker), 'remote', 'get-url', 'origin'],
                               check=True, capture_output=True, text=True).stdout)
            self.assertEqual(remote, 'https://host.invalid/x')
            return [{'path': str(self.tracker), 'remote': remote}]
        with self.fake_cli(repos=recorded):
            receipt = migration.rehearse('/usr/bin/true', self.source, self.tracker,
                                         self.root / 'out')
        text = json.dumps(receipt) + (self.root / 'out/receipt.json').read_text() \
            + (self.root / 'out/bundle/manifest.json').read_text()
        self.assertNotIn(secret, text)
        self.assertNotIn('user:', text)
        self.assertEqual(receipt['recorded_repos'], [{'path': str(self.tracker),
                                                      'remote': 'https://host.invalid/x'}])
        migration.assert_no_credentials('receipt', text)

    def test_refuses_a_source_naming_a_checkout_outside_the_rehearsal(self):
        outside = self.make_repo(self.root / 'live-checkout', remote=TOKEN_REMOTE)
        before = index_fingerprint(outside)
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)', ['live', str(outside)])
        for dry_run in (False, True):
            with self.subTest(dry_run=dry_run):
                with self.assertRaisesRegex(ValueError, 'outside the rehearsal'):
                    migration.rehearse('/usr/bin/true', self.source, self.tracker,
                                       self.root / 'out', dry_run=dry_run)
        self.assertFalse((self.root / 'out').exists())
        self.assertEqual(index_fingerprint(outside), before)

    def test_refuses_a_missing_spec_path_whose_parent_is_a_live_checkout(self):
        """`discover_repos` falls back to the parent directory, so this reads it."""
        outside = self.make_repo(self.root / 'live-checkout', remote=TOKEN_REMOTE)
        with sqlite3.connect(self.db) as db:
            db.execute('CREATE TABLE tasks(id TEXT PRIMARY KEY, worktree TEXT, spec_path TEXT)')
            db.execute('INSERT INTO tasks VALUES(?,NULL,?)',
                       ['t', str(outside / 'deleted-spec.md')])
        with self.assertRaisesRegex(ValueError, 'outside the rehearsal'):
            migration.rehearse('/usr/bin/true', self.source, self.tracker,
                               self.root / 'out', dry_run=True)

    def test_accepts_paths_whose_parent_is_gone_too(self):
        gone = self.root / 'was-here' / 'deeper' / 'spec.md'
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)', ['gone', str(gone)])
        plan = migration.rehearse('/usr/bin/true', self.source, self.tracker,
                                  self.root / 'out', dry_run=True)
        self.assertEqual(plan['recorded_checkout_paths'], [str(gone)])
        self.assertIsNone(migration.would_read(str(gone), self.root))

    def test_would_read_matches_the_rust_resolution_rule(self):
        scratch = self.root / 'scratch'
        scratch.mkdir()
        directory = self.make_repo(self.root / 'some-repo')
        inside = directory / 'README.md'
        self.assertEqual(migration.would_read(str(directory), scratch), directory.resolve())
        self.assertEqual(migration.would_read(str(inside), scratch), directory.resolve())
        self.assertIsNone(migration.would_read(str(self.root / 'x/y/z'), scratch))
        # A relative value belongs to the child's cwd, which is private scratch.
        (scratch / 'rel').mkdir()
        self.assertEqual(migration.would_read('rel', scratch), (scratch / 'rel').resolve())

    # ---------- 4: --dry-run mutates nothing ----------

    def test_dry_run_creates_no_output_and_writes_no_index(self):
        self.live_shaped()
        before = index_fingerprint(self.tracker)
        listing = sorted(p.name for p in self.source.iterdir())
        plan = migration.rehearse('/usr/bin/true', self.source, self.tracker,
                                  self.root / 'out', dry_run=True)
        self.assertEqual(plan['mode'], 'dry-run')
        self.assertEqual(plan['mutations_performed'], [])
        self.assertEqual(plan['tracker_sha'], self.git(self.tracker, 'rev-parse', 'HEAD'))
        self.assertEqual(plan['inventory']['message_states'], {'queued': 1})
        self.assertTrue(any('export --out' in step for step in plan['would_run']))
        self.assertIn(str(self.root / 'out'), plan['would_create'])
        self.assertFalse(plan['production_cutover_authorized'])
        self.assertFalse((self.root / 'out').exists(), 'dry run created the output directory')
        self.assertEqual(index_fingerprint(self.tracker), before,
                         'dry run wrote the tracker index')
        self.assertEqual(sorted(p.name for p in self.source.iterdir()), listing)

    def test_dry_run_holds_no_lock_while_it_reports(self):
        held = {}
        real_plan = migration.plan

        def probe(*args, **kwargs):
            with (self.source / 'cadence.lock').open('rb') as other:
                try:
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    held['free'] = True
                except BlockingIOError:
                    held['free'] = False
            return real_plan(*args, **kwargs)
        with patch.object(migration, 'plan', side_effect=probe):
            migration.rehearse('/usr/bin/true', self.source, self.tracker,
                               self.root / 'out', dry_run=True)
        self.assertTrue(held['free'], 'dry run kept the source flock')
        # The real run does hold it: the assertion above is load-bearing.
        real_execute = migration.execute

        def check(*args, **kwargs):
            with (self.source / 'cadence.lock').open('rb') as other:
                try:
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    held['run_free'] = True
                except BlockingIOError:
                    held['run_free'] = False
            return real_execute(*args, **kwargs)
        with self.fake_cli(), patch.object(migration, 'execute', side_effect=check):
            migration.rehearse('/usr/bin/true', self.source, self.tracker, self.root / 'out')
        self.assertFalse(held['run_free'])

    def test_dry_run_flag_is_reachable_from_the_command_line(self):
        result = subprocess.run(
            ['python3', str(Path(__file__).with_name('rehearse-hosted-migration.py')),
             '--cadence', '/usr/bin/true', '--source-state', str(self.source),
             '--tracker', str(self.tracker), '--out', str(self.root / 'out'), '--dry-run'],
            capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['mode'], 'dry-run')
        self.assertFalse((self.root / 'out').exists())

    # ---------- real CLI ----------

    @unittest.skipUnless(os.environ.get('CADENCE_REHEARSAL_BINARY'), 'real CLI binary not supplied')
    def test_real_export_restore_and_tracker_snapshot(self):
        self.live_shaped()
        before = index_fingerprint(self.tracker)
        binary = os.environ['CADENCE_REHEARSAL_BINARY']
        plan = migration.rehearse(binary, self.source, self.tracker, self.root / 'out',
                                  dry_run=True)
        self.assertEqual(plan['mode'], 'dry-run')
        self.assertFalse((self.root / 'out').exists())
        with patch.dict(os.environ, self.poisoned()):
            receipt = migration.rehearse(binary, self.source, self.tracker, self.root / 'out')
        self.assertEqual(receipt['inventory']['message_states'], {'queued': 1})
        self.assertEqual(receipt['tracker_sha'], self.git(self.tracker, 'rev-parse', 'HEAD'))
        self.assertTrue(receipt['restored_inventory_matches'])
        self.assertFalse(receipt['production_cutover_authorized'])
        self.assertIn('cross-host single writer lease', receipt['not_proven'])
        self.assertEqual((self.root / 'out/receipt.json').stat().st_mode & 0o777, 0o600)
        self.assertEqual(index_fingerprint(self.tracker), before,
                         'the real rehearsal rewrote the tracker index')
        # Original queued work is still present; the tool never dispatches it.
        self.assertEqual(migration.inspect_db(self.db)['message_states'], {'queued': 1})

    @unittest.skipUnless(os.environ.get('CADENCE_REHEARSAL_BINARY'), 'real CLI binary not supplied')
    def test_real_cli_records_the_rehearsal_repo_not_an_inherited_decoy(self):
        """`discover_repos` runs git in recorded paths; a leaked GIT_DIR wins."""
        binary = os.environ['CADENCE_REHEARSAL_BINARY']
        self.git(self.tracker, 'remote', 'add', 'origin', 'https://real.invalid/tracker')
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['tracked', str(self.tracker)])
        poison = self.poisoned()
        decoy = Path(poison['GIT_WORK_TREE'])
        self.git(decoy, 'remote', 'add', 'origin', TOKEN_REMOTE)
        with patch.dict(os.environ, poison):
            receipt = migration.rehearse(binary, self.source, self.tracker, self.root / 'out')
        self.assertEqual(receipt['recorded_repos'],
                         [{'path': str(self.tracker), 'remote': 'https://real.invalid/tracker'}])
        # Unscrubbed, the same binary records the decoy instead: the guard bites.
        leaked = self.root / 'leaked-bundle'
        subprocess.run([binary, '--state-dir', str(self.source), 'export', '--out', str(leaked)],
                       check=True, capture_output=True, env={**os.environ, **poison})
        recorded = json.loads((leaked / 'manifest.json').read_text()).get('repos')
        self.assertEqual([repo['path'] for repo in recorded], [str(decoy)],
                         'the poison is inert; the env guard is untested')

    @unittest.skipUnless(os.environ.get('CADENCE_REHEARSAL_BINARY'), 'real CLI binary not supplied')
    def test_real_cli_never_reads_a_credentialed_remote(self):
        secret = TOKEN_REMOTE.split('@')[0].split(':')[-1]
        self.git(self.tracker, 'remote', 'add', 'origin', TOKEN_REMOTE)
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['tracked', str(self.tracker)])
        receipt = migration.rehearse(os.environ['CADENCE_REHEARSAL_BINARY'], self.source,
                                     self.tracker, self.root / 'out')
        emitted = json.dumps(receipt) + '\n'.join(
            path.read_text(errors='replace')
            for path in (self.root / 'out').rglob('*.json'))
        self.assertNotIn(secret, emitted)
        migration.assert_no_credentials('rehearsal output', emitted)
        for repo in receipt['recorded_repos']:
            self.assertNotIn('@', repo['remote'].split('://', 1)[-1].split('/')[0])


if __name__ == '__main__':
    unittest.main()
