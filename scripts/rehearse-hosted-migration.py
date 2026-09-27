#!/usr/bin/env python3
"""Offline CAD-529 rehearsal only. Never freezes, starts or contacts production."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import subprocess
import sys


def run(*args):
    result = subprocess.run(args, capture_output=True, text=True, check=False)
    if result.returncode:
        # Do not echo arbitrary export/remote/credential output into evidence.
        raise ValueError(f"{Path(args[0]).name} command failed (exit {result.returncode})")
    return result.stdout.strip()


def temporary(path):
    path = Path(path).resolve()
    if not path.is_relative_to(Path('/tmp')) or path == Path('/tmp'):
        raise ValueError('rehearsal state/output must be inside an owned /tmp directory')
    return path


def owned(path, directory=False):
    info = path.lstat()
    expected_type = stat.S_ISDIR if directory else stat.S_ISREG
    if info.st_uid != os.getuid() or not expected_type(info.st_mode):
        raise ValueError('rehearsal inputs and output parent must be owned by the current user and have the expected type')


def inspect_db(path, portable=False):
    if path.is_symlink() or not path.is_file():
        raise ValueError('database must be a regular file')
    with sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True) as db:
        if db.execute('PRAGMA integrity_check').fetchall() != [('ok',)]:
            raise ValueError('database integrity check failed')
        schema = db.execute('SELECT version FROM schema_version').fetchall()
        if len(schema) != 1:
            raise ValueError('schema version must have exactly one row')
        names = [row[0] for row in db.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )]
        counts = {}
        for name in names:
            escaped = name.replace('"', '""')
            counts[name] = db.execute(f'SELECT count(*) FROM "{escaped}"').fetchone()[0]
        if not {'agents', 'messages'}.issubset(names):
            raise ValueError('agents/messages inventory is required')
        states = dict(db.execute('SELECT state,count(*) FROM messages GROUP BY state'))
        if any(states.get(state, 0) for state in ('running', 'submitting', 'unknown')):
            raise ValueError('running/submitting/unknown messages require reconciliation before export')
        if portable:
            for table, column in [('agents', 'generation'), ('agents', 'pid'),
                                  ('agents', 'pid_start'), ('messages', 'turn_id')]:
                columns = {row[1] for row in db.execute(f'PRAGMA table_info({table})')}
                if column in columns and db.execute(
                    f'SELECT count(*) FROM {table} WHERE {column} IS NOT NULL'
                ).fetchone()[0]:
                    raise ValueError(f'portable export retained {table}.{column}')
        return {'schema_version': schema[0][0], 'table_counts': counts, 'message_states': states}


def content_digest(path):
    """Compare offline source content without printing rows or credentials."""
    digest = hashlib.sha256()
    with sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True) as db:
        for statement in db.iterdump():
            digest.update(statement.encode())
            digest.update(b'\n')
    return digest.hexdigest()


def verify_bundle(bundle):
    manifest_path = bundle / 'manifest.json'
    if manifest_path.is_symlink():
        raise ValueError('manifest must not be a symlink')
    manifest = json.loads(manifest_path.read_text())
    if (manifest.get('format') != 'cadence.backup/1' or manifest.get('kind') != 'export'
            or manifest.get('db_file') != 'cadence.sqlite3' or not manifest.get('export')):
        raise ValueError('a portable cadence export is required')
    db_path = bundle / 'cadence.sqlite3'
    inventory = inspect_db(db_path, portable=True)
    digest = hashlib.sha256(db_path.read_bytes()).hexdigest()
    if digest != manifest.get('sha256') or db_path.stat().st_size != manifest.get('bytes'):
        raise ValueError('export size/hash mismatch')
    if inventory['schema_version'] != manifest.get('schema_version'):
        raise ValueError('export schema mismatch')
    return inventory, digest


def clean_tracker(tracker):
    if run('git', '-C', str(tracker), 'status', '--porcelain'):
        raise ValueError('tracker must be clean, including untracked files')
    if run('git', '-C', str(tracker), 'rev-parse', '--show-toplevel') != str(tracker):
        raise ValueError('tracker must name the repository root')
    return run('git', '-C', str(tracker), 'rev-parse', 'HEAD')


def rehearse(binary, source, tracker, output):
    source, output = temporary(source), temporary(output)
    tracker = Path(tracker).resolve()
    if tracker == (Path.home() / 'pm').resolve():
        raise ValueError('use an isolated tracker clone, never the production PM directory')
    if output.exists() or output.is_relative_to(source) or source.is_relative_to(output):
        raise ValueError('output must be a new directory separate from source state')
    owned(source, directory=True)
    owned(source / 'cadence.sqlite3')
    owned(source / 'cadence.lock')
    owned(tracker, directory=True)
    owned(output.parent, directory=True)
    if not Path(binary).is_absolute() or not Path(binary).is_file():
        raise ValueError('pass an explicit existing cadence binary path')
    sha = clean_tracker(tracker)
    # Same singleton flock as cadence daemon/restore. This protects against a
    # daemon starting during rehearsal, but is NOT a freeze of standalone CLI writers.
    lock_fd = os.open(source / 'cadence.lock', os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(lock_fd, 'rb') as lock:
        if not stat.S_ISREG(os.fstat(lock.fileno()).st_mode):
            raise ValueError('cadence.lock must be a regular file')
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError('source daemon is active; use an offline rehearsal copy') from error
        before = inspect_db(source / 'cadence.sqlite3')
        before_digest = content_digest(source / 'cadence.sqlite3')
        output.mkdir(mode=0o700, parents=False)
        bundle = output / 'bundle'
        run(binary, '--state-dir', str(source), 'export', '--out', str(bundle))
        exported, digest = verify_bundle(bundle)
        export_exclusions = json.loads((bundle / 'manifest.json').read_text())['export'].get('excludes', [])
        if not isinstance(export_exclusions, list) or any(not isinstance(item, str) for item in export_exclusions):
            raise ValueError('invalid export exclusions inventory')
        if (before != exported or inspect_db(source / 'cadence.sqlite3') != before
                or content_digest(source / 'cadence.sqlite3') != before_digest):
            raise ValueError('source inventory changed; freeze every rehearsal writer and retry')
        restored_dir = output / 'restored-state'
        run(binary, '--state-dir', str(restored_dir), 'restore', str(bundle))
        restored = inspect_db(restored_dir / 'cadence.sqlite3', portable=True)
        if restored != exported:
            raise ValueError('restored schema/counts/delivery states differ from export')
        if content_digest(restored_dir / 'cadence.sqlite3') != content_digest(bundle / 'cadence.sqlite3'):
            raise ValueError('restored row content differs from export')
        tracker_copy = output / 'tracker'
        run('git', 'clone', '--quiet', '--no-hardlinks', '--no-checkout', str(tracker), str(tracker_copy))
        run('git', '-C', str(tracker_copy), 'checkout', '--quiet', '--detach', sha)
        if clean_tracker(tracker) != sha or clean_tracker(tracker_copy) != sha:
            raise ValueError('tracker changed during rehearsal')
        receipt = {
            'format': 'cadence.migration-rehearsal/1', 'mode': 'offline-rehearsal',
            'tracker_sha': sha, 'export_sha256': digest, 'inventory': exported,
            'restored_inventory_matches': True,
            'restored_content_matches': True,
            'export_exclusions': export_exclusions,
            'separate_inventory_required': ['tracker configuration and hooks',
                'roles', 'briefings', 'review evidence', 'private runtime files',
                'local checkouts and worktrees', 'provider and connection credentials'],
            'excluded_runtime_authority': ['old agent generations', 'turn tokens',
                'source process identities', 'local sockets and locks'],
            'requires_fresh_agent_enrollment': True,
            'production_cutover_authorized': False,
            'not_proven': ['all-writer freeze', 'cross-host single writer lease',
                           'cloud durable storage', 'remote worker authentication',
                           'sleep-safe inbound queue', 'post-write rollback'],
        }
        receipt_path = output / 'receipt.json'
        with receipt_path.open('x') as stream:
            os.chmod(receipt_path, 0o600)
            json.dump(receipt, stream, indent=2)
            stream.write('\n')
        return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cadence', required=True, help='explicit binary, never installed/replaced')
    parser.add_argument('--source-state', required=True, help='offline owned /tmp state with cadence.lock')
    parser.add_argument('--tracker', required=True, help='clean isolated tracker clone')
    parser.add_argument('--out', required=True, help='new owned /tmp output directory')
    args = parser.parse_args()
    try:
        receipt = rehearse(args.cadence, args.source_state, args.tracker, args.out)
    except (ValueError, OSError, sqlite3.Error, json.JSONDecodeError) as error:
        print(f'rehearsal refused: {error}', file=sys.stderr)
        return 1
    print(json.dumps(receipt, indent=2))
    return 0


if __name__ == '__main__':
    sys.exit(main())
