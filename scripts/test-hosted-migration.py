#!/usr/bin/env python3
"""Run with python3; optional CADENCE_REHEARSAL_BINARY exercises the real CLI."""
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import stat
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


def git_inventory(repo):
    """Whole metadata identity; never follow a symlink into a foreign store."""
    root = Path(repo) / '.git'
    result = {}
    pending = [root]
    while pending:
        path = pending.pop()
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            pending.extend(path.iterdir())
        digest = hashlib.sha256(path.read_bytes()).hexdigest() if stat.S_ISREG(info.st_mode) else None
        link = os.readlink(path) if stat.S_ISLNK(info.st_mode) else None
        result[str(path.relative_to(root))] = (info.st_mode, info.st_uid, info.st_nlink,
                                               info.st_mtime_ns, info.st_size, digest, link)
    return result


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

    def test_refuses_a_tracker_outside_the_temporary_roots(self):
        # Allowlist, not denylist: any owned git root beyond /tmp is refused,
        # not only ~/pm. /etc exists on every CI image and is outside /tmp.
        for dry_run in (False, True):
            with self.subTest(dry_run=dry_run):
                with self.assertRaisesRegex(ValueError, '/tmp'):
                    migration.rehearse('/usr/bin/true', self.source, '/etc',
                                       self.root / 'out', dry_run=dry_run)
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_a_gitfile_checkout_pointing_outside_the_rehearsal(self):
        # The column value resolves inside `source`, but its `.git` file
        # makes `git -C` land in a repository outside the rehearsal — whose
        # `remote get-url` would print the token remote.
        outside = self.make_repo(self.root / 'foreign-repo', remote=TOKEN_REMOTE)
        inner = self.source / 'nested'
        inner.mkdir()
        (inner / '.git').write_text(f'gitdir: {outside}/.git\n')
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['nested', str(inner)])
        with self.assertRaisesRegex(migration.UnsupportedGitLayout, 'gitfile or symlinked gitdir is unsupported'):
            migration.rehearse('/usr/bin/true', self.source, self.tracker,
                               self.root / 'out', dry_run=True)
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_a_column_dir_whose_repo_is_an_ancestor_of_the_root(self):
        # No `.git` anywhere below the value's directory — `git -C` walks
        # up and finds a repository that contains the root itself.
        self.make_repo(self.root)
        sub = self.source / 'plain-subdir'
        sub.mkdir()
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['ancestor', str(sub)])
        with self.assertRaisesRegex(migration.UnsupportedGitLayout, 'outside ancestor'):
            migration.rehearse('/usr/bin/true', self.source, self.tracker,
                               self.root / 'out', dry_run=True)
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_a_commondir_escape_to_a_foreign_repo(self):
        # `.git` is a real worktree gitdir inside `source`, but its
        # `commondir` file sends every object/config read to a repo
        # outside the rehearsal — `--absolute-git-dir` alone accepts it.
        outside = self.make_repo(self.root / 'foreign-repo', remote=TOKEN_REMOTE)
        inner = self.source / 'nested'
        gitdir = inner / '.git'
        gitdir.mkdir(parents=True)
        (gitdir / 'commondir').write_text(str(outside / '.git'))
        (gitdir / 'gitdir').write_text(str(gitdir))
        (gitdir / 'HEAD').write_text('ref: refs/heads/main\n')
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['nested', str(inner)])
        # The escape is real: unguarded, the remote read returns the token.
        self.assertEqual(self.git(inner, 'remote', 'get-url', 'origin'), TOKEN_REMOTE)
        with self.assertRaisesRegex(migration.UnsupportedGitLayout, 'common directories and object alternates are unsupported'):
            migration.rehearse('/usr/bin/true', self.source, self.tracker,
                               self.root / 'out', dry_run=True)
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_alternates_reaching_a_foreign_object_store(self):
        # The repo and its gitdir are inside `source`, but
        # objects/info/alternates names an object store outside — a clone
        # or cat-file would serve foreign objects.
        outside = self.make_repo(self.root / 'foreign-store')
        inner = self.make_repo(self.source / 'nested-repo')
        info = inner / '.git' / 'objects' / 'info'
        info.mkdir(exist_ok=True)
        (info / 'alternates').write_text(f'{outside}/.git/objects\n')
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['nested', str(inner)])
        with self.assertRaisesRegex(migration.UnsupportedGitLayout, 'common directories and object alternates are unsupported'):
            migration.rehearse('/usr/bin/true', self.source, self.tracker,
                               self.root / 'out', dry_run=True)
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_a_tracker_gitfile_pointing_outside_its_root(self):
        # `temporary()` only confines the tracker *path*; a `.git` file
        # underneath still lands `clean_tracker` and `git clone` in a
        # repository the rehearsal does not own.
        outside = self.make_repo(self.root / 'foreign-tracker', remote=TOKEN_REMOTE)
        fake = self.root / 'fake-tracker'
        fake.mkdir()
        (fake / '.git').write_text(f'gitdir: {outside}/.git\n')
        with self.assertRaisesRegex(migration.UnsupportedGitLayout, 'gitfile or symlinked gitdir is unsupported'):
            migration.rehearse('/usr/bin/true', self.source, fake,
                               self.root / 'out', dry_run=True)
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_a_symlinked_object_store_escaping_the_root(self):
        # `objects` lexically inside a root but physically a symlink to a
        # foreign store: only resolving before comparing catches it.
        outside = self.make_repo(self.root / 'foreign-store')
        inner = self.make_repo(self.source / 'nested-repo')
        (inner / '.git' / 'objects').rename(inner / '.git' / 'objects-real')
        os.symlink(outside / '.git' / 'objects', inner / '.git' / 'objects')
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['nested', str(inner)])
        with self.assertRaisesRegex(migration.UnsupportedGitLayout, 'symlinked, linked or special repository metadata is unsupported'):
            migration.rehearse('/usr/bin/true', self.source, self.tracker,
                               self.root / 'out', dry_run=True)
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_transitive_alternates_reaching_outside(self):
        # inner -> middle (inside source) -> outside. The inside hop must
        # be traversed and the outside hop must still refuse.
        outside = self.make_repo(self.root / 'foreign-store')
        middle = self.make_repo(self.source / 'middle-repo')
        inner = self.make_repo(self.source / 'nested-repo')
        for repo, target in ((inner, middle), (middle, outside)):
            info = repo / '.git' / 'objects' / 'info'
            info.mkdir(exist_ok=True)
            (info / 'alternates').write_text(f'{target}/.git/objects\n')
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['nested', str(inner)])
        with self.assertRaisesRegex(migration.UnsupportedGitLayout, 'common directories and object alternates are unsupported'):
            migration.rehearse('/usr/bin/true', self.source, self.tracker,
                               self.root / 'out', dry_run=True)
        self.assertFalse((self.root / 'out').exists())

    def test_clean_tracker_refuses_a_split_index_instead_of_rewriting_it(self):
        self.git(self.tracker, 'update-index', '--split-index')
        live = sorted((self.tracker / '.git').glob('sharedindex.*'))
        self.assertTrue(live, 'fixture never created a shared index')
        self.live_shaped()

        def fingerprint():
            fp = {'index': index_fingerprint(self.tracker)}
            for p in (self.tracker / '.git').glob('sharedindex.*'):
                fp[p.name] = (hashlib.sha256(p.read_bytes()).hexdigest(),
                              p.stat().st_mtime_ns)
            return fp

        before = fingerprint()
        with self.assertRaisesRegex(ValueError, 'split index'):
            migration.clean_tracker(self.iso, self.tracker)
        self.assertEqual(before, fingerprint(),
                         'the refusal still touched the live shared index')
        # Non-vacuous: the unguarded read does refresh the shared index —
        # same file, new mtime — proving the refusal is load-bearing.
        subprocess.run(['git', '-C', str(self.tracker), 'status', '--porcelain'],
                       check=True, capture_output=True)
        self.assertNotEqual(before, fingerprint(),
                            'fixture never refreshes a split index; guard untested')

    def test_accepts_a_checkout_repo_inside_the_source_root(self):
        # Positive control: a real repository inside a root stays allowed —
        # its own gitdir resolves inside the rehearsal.
        inner = self.make_repo(self.source / 'nested-repo')
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)',
                       ['nested', str(inner)])
        plan = migration.rehearse('/usr/bin/true', self.source, self.tracker,
                                  self.root / 'out', dry_run=True)
        self.assertIn(str(inner), plan['recorded_checkout_paths'])

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

    # ---------- pre-Git storage/config admission ----------

    def test_config_only_split_index_does_not_create_live_sharedindex(self):
        self.git(self.tracker, 'config', 'core.splitIndex', 'true')
        self.assertFalse(list((self.tracker / '.git').glob('sharedindex.*')))
        self.live_shaped()
        before = git_inventory(self.tracker)
        migration.clean_tracker(self.iso, self.tracker)
        self.assertEqual(git_inventory(self.tracker), before)
        # Counterfactual: real ordinary Git creates the live shared index.
        self.git(self.tracker, 'status', '--porcelain')
        self.assertTrue(list((self.tracker / '.git').glob('sharedindex.*')))
        self.assertNotEqual(git_inventory(self.tracker), before)

    def test_config_and_pack_escapes_refuse_before_repository_git(self):
        for context in ('tracker', 'recorded_path'):
            for layout in ('config_symlink', 'config_include', 'pack_directory', 'pack_file'):
                with self.subTest(context=context, layout=layout):
                    f = MigrationTests()
                    f.setUp()
                    self.addCleanup(f.doCleanups)
                    repo = f.tracker if context == 'tracker' else f.make_repo(f.source / 'inner')
                    gitdir = repo / '.git'
                    outside = f.root / 'outside-metadata'
                    if layout.startswith('config'):
                        outside.write_text((gitdir / 'config').read_text() +
                                           f'\n[remote "origin"]\n url = {TOKEN_REMOTE}\n')
                        if layout == 'config_symlink':
                            (gitdir / 'config').unlink()
                            (gitdir / 'config').symlink_to(outside)
                        else:
                            with (gitdir / 'config').open('a') as config:
                                config.write(f'\n[include]\n path = {outside}\n')
                        # Actual Git resolves the outside credentialed config.
                        self.assertEqual(f.git(repo, 'remote', 'get-url', 'origin'), TOKEN_REMOTE)
                    else:
                        sha = f.git(repo, 'rev-parse', 'HEAD')
                        f.git(repo, 'gc', '--prune=now')
                        pack = gitdir / 'objects' / 'pack'
                        if layout == 'pack_directory':
                            shutil.move(str(pack), outside)
                            pack.symlink_to(outside, target_is_directory=True)
                        else:
                            packed = next(pack.glob('*.pack'))
                            shutil.move(str(packed), outside)
                            packed.symlink_to(outside)
                        # Object reach witness, separate from monitored rehearsal.
                        self.assertEqual(f.git(repo, 'cat-file', '-t', sha), 'commit')
                    if context == 'recorded_path':
                        with sqlite3.connect(f.db) as db:
                            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)', ('bad', str(repo)))
                    before = git_inventory(repo)
                    calls = []
                    real = subprocess.run
                    def observed(args, *a, **kw):
                        calls.append(tuple(str(arg) for arg in args))
                        return real(args, *a, **kw)
                    with patch.object(migration.subprocess, 'run', side_effect=observed):
                        with self.assertRaisesRegex(ValueError, 'unsupported'):
                            migration.rehearse('/usr/bin/true', f.source, f.tracker,
                                               f.root / 'out', dry_run=True)
                    self.assertFalse(any('-C' in call and call[call.index('-C') + 1] == str(repo)
                                         for call in calls), calls)
                    self.assertEqual(git_inventory(repo), before)
                    self.assertFalse((f.root / 'out').exists())

    def test_quoted_and_symlinked_alternates_refuse_before_foreign_python_read(self):
        for layout in ('quoted', 'alternates_symlink', 'info_symlink'):
            with self.subTest(layout=layout):
                f = MigrationTests()
                f.setUp()
                self.addCleanup(f.doCleanups)
                outside = f.make_repo(f.root / 'foreign-store')
                blob = f.git(outside, 'rev-parse', 'HEAD:README.md')
                inner = f.make_repo(f.source / 'inner')
                absent = subprocess.run(['git', '-C', str(inner), 'cat-file', '-t', blob], capture_output=True)
                self.assertNotEqual(absent.returncode, 0, 'foreign-only witness already local')
                info = inner / '.git/objects/info'
                alternate = info / 'alternates'
                canary = f.root / 'outside-alternates'
                if layout == 'quoted':
                    alternate.write_text(json.dumps(str(outside / '.git/objects')) + '\n')
                elif layout == 'alternates_symlink':
                    canary.write_text(str(outside / '.git/objects') + '\n')
                    alternate.symlink_to(canary)
                else:
                    shutil.move(str(info), canary)
                    (canary / 'alternates').write_text(str(outside / '.git/objects') + '\n')
                    info.symlink_to(canary, target_is_directory=True)
                self.assertEqual(f.git(inner, 'cat-file', '-t', blob), 'blob')
                with sqlite3.connect(f.db) as db:
                    db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)', ('bad', str(inner)))
                before = git_inventory(inner)
                reads = []
                real = Path.read_text
                def read(path, *a, **kw):
                    if path.resolve().is_relative_to(canary.resolve()):
                        reads.append(str(path))
                    return real(path, *a, **kw)
                with patch.object(Path, 'read_text', read):
                    with self.assertRaisesRegex(ValueError, 'unsupported'):
                        migration.rehearse('/usr/bin/true', f.source, f.tracker,
                                           f.root / 'out', dry_run=True)
                self.assertEqual(reads, [], 'outside Python canary was read before refusal')
                self.assertEqual(git_inventory(inner), before)

    def test_central_git_and_clone_admit_before_spawn(self):
        config = self.tracker / '.git/config'
        outside = self.root / 'outside-config'
        outside.write_text(config.read_text())
        config.unlink()
        config.symlink_to(outside)
        with patch.object(migration.subprocess, 'run', side_effect=AssertionError('Git must not spawn')):
            with self.assertRaisesRegex(ValueError, 'unsupported'):
                self.iso.run('git', '-C', str(self.tracker), 'rev-parse', 'HEAD')
            with self.assertRaisesRegex(ValueError, 'unsupported'):
                self.iso.run('git', 'clone', str(self.tracker), str(self.root / 'clone'))

    def test_path_command_and_includeif_config_refuse_before_repository_git(self):
        config = self.tracker / '.git/config'
        initial = config.read_text()
        settings = [f'[core]\n {key} = /external/never-read-or-execute\n'
                    for key in ('worktree', 'hooksPath', 'attributesFile', 'fsmonitor')]
        settings.append('[includeIf "gitdir:/tmp/"]\n path = /external/never-read\n')
        for setting in settings:
            with self.subTest(setting=setting):
                config.write_text(initial + '\n' + setting)
                calls = []
                real = subprocess.run
                def observed(args, *a, **kw):
                    calls.append(tuple(args))
                    return real(args, *a, **kw)
                with patch.object(migration.subprocess, 'run', side_effect=observed):
                    with self.assertRaisesRegex(ValueError, 'unsupported|allowlist'):
                        self.iso.run('git', '-C', str(self.tracker), 'rev-parse', 'HEAD')
                self.assertEqual(len(calls), 1)
                self.assertIn('--no-includes', calls[0])
                self.assertNotIn('-C', calls[0])

    def test_missing_recorded_file_cannot_discover_an_outside_ancestor(self):
        self.make_repo(self.root, remote=TOKEN_REMOTE)
        plain = self.source / 'plain'
        plain.mkdir()
        missing = plain / 'deleted-spec.md'
        self.assertEqual(self.git(plain, 'rev-parse', '--absolute-git-dir'), str(self.root / '.git'))
        self.assertEqual(self.git(plain, 'remote', 'get-url', 'origin'), TOKEN_REMOTE)
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO agents VALUES(?,?,NULL,NULL,NULL)', ('bad', str(missing)))
        with patch.object(migration.subprocess, 'run', side_effect=AssertionError('Git must not spawn')):
            with self.assertRaisesRegex(ValueError, 'outside ancestor'):
                migration.refuse_foreign_checkouts(self.db, (self.source, self.tracker), self.iso)

    def test_metadata_hooks_hardlinks_and_inventory_bound_refuse_before_git(self):
        gitdir = self.tracker / '.git'
        hook = gitdir / 'hooks/post-checkout'
        hook.write_text('#!/bin/sh\nexit 71\n')
        with patch.object(migration.subprocess, 'run', side_effect=AssertionError('Git must not spawn')):
            with self.assertRaisesRegex(ValueError, 'unsupported'):
                migration.clean_tracker(self.iso, self.tracker)
        hook.unlink()
        hardlink = self.root / 'config-hardlink'
        os.link(gitdir / 'config', hardlink)
        with patch.object(migration.subprocess, 'run', side_effect=AssertionError('Git must not spawn')):
            with self.assertRaisesRegex(ValueError, 'unsupported'):
                migration.clean_tracker(self.iso, self.tracker)
        hardlink.unlink()
        bounded = gitdir / 'bounded'
        bounded.mkdir()
        for number in range(10001):
            (bounded / str(number)).touch()
        with patch.object(migration.subprocess, 'run', side_effect=AssertionError('Git must not spawn')):
            with self.assertRaisesRegex(ValueError, 'too large'):
                migration.clean_tracker(self.iso, self.tracker)

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
