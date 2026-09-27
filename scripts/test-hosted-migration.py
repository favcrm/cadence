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


class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='cad529-')
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
        self.tracker.mkdir()
        self.git('init', '-q')
        (self.tracker / 'README.md').write_text('isolated rehearsal tracker\n')
        self.git('add', 'README.md')
        self.git('-c', 'user.name=Rehearsal', '-c', 'user.email=rehearsal@example.invalid',
                 '-c', 'core.hooksPath=/dev/null', 'commit', '-qm', 'fixture')

    def git(self, *args):
        return subprocess.run(['git', '-C', str(self.tracker), *args], check=True,
                              capture_output=True, text=True).stdout.strip()

    def bundle(self):
        bundle = self.root / 'bundle'
        bundle.mkdir()
        data = self.db.read_bytes()
        (bundle / 'cadence.sqlite3').write_bytes(data)
        (bundle / 'manifest.json').write_text(json.dumps({
            'format': 'cadence.backup/1', 'kind': 'export', 'db_file': 'cadence.sqlite3',
            'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data),
            'schema_version': 1, 'export': {'contains': ['store']},
        }))
        return bundle

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
            migration.clean_tracker(self.tracker)
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
        self.assertFalse((self.root / 'out').exists())

    def test_refuses_same_count_source_mutation_during_export(self):
        real_run = migration.run
        def mutate(*args):
            if 'export' in args:
                bundle = self.bundle()
                bundle.rename(self.root / 'out/bundle')
                with sqlite3.connect(self.db) as db:
                    db.execute("UPDATE messages SET body='changed during export'")
                return ''
            return real_run(*args)
        with patch.object(migration, 'run', side_effect=mutate):
            with self.assertRaisesRegex(ValueError, 'source inventory changed'):
                migration.rehearse('/usr/bin/true', self.source, self.tracker, self.root / 'out')
        self.assertFalse((self.root / 'out/receipt.json').exists())

    @unittest.skipUnless(os.environ.get('CADENCE_REHEARSAL_BINARY'), 'real CLI binary not supplied')
    def test_real_export_restore_and_tracker_snapshot(self):
        receipt = migration.rehearse(os.environ['CADENCE_REHEARSAL_BINARY'], self.source,
                                    self.tracker, self.root / 'out')
        self.assertEqual(receipt['inventory']['message_states'], {'queued': 1})
        self.assertEqual(receipt['tracker_sha'], self.git('rev-parse', 'HEAD'))
        self.assertTrue(receipt['restored_inventory_matches'])
        self.assertFalse(receipt['production_cutover_authorized'])
        self.assertIn('cross-host single writer lease', receipt['not_proven'])
        self.assertEqual((self.root / 'out/receipt.json').stat().st_mode & 0o777, 0o600)
        # Original queued work is still present; the tool never dispatches it.
        self.assertEqual(migration.inspect_db(self.db)['message_states'], {'queued': 1})


if __name__ == '__main__':
    unittest.main()
