#!/usr/bin/env python3
"""Offline CAD-529 rehearsal only. Never freezes, starts or contacts production."""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import stat
import subprocess
import sys
import tempfile

# Child processes get exactly these variables from this process. An allowlist,
# not a denylist: a GIT_DIR, GIT_INDEX_FILE, GIT_WORK_TREE, CADENCE_STATE_DIR or
# CADENCE_PM_DIR exported into the rehearsal must never reach git or the CLI,
# and a variable added to the parent later cannot silently become inherited.
INHERITED_ENV = ('PATH',)

# Any `scheme://user[:secret]@host` URL. Nothing carrying userinfo may reach the
# receipt, stdout or the bundle; the scp form (`git@host:p`) has no secret.
CREDENTIAL_URL = re.compile(r"""[A-Za-z][A-Za-z0-9+.\-]*://[^/\s@'"]+@""")

# Columns `discover_repos`/`plan_remap` (src/backup/mod.rs) run git in, plus any
# same-named column a later schema adds. A path here that exists on this host is
# a checkout the export would read; the rehearsal refuses instead of touching it.
PATH_COLUMNS = (('agents', 'cwd'), ('jobs', 'repo'), ('jobs', 'spec_path'),
                ('tasks', 'worktree'), ('tasks', 'spec_path'))
PATH_COLUMN_NAMES = frozenset(
    {'cwd', 'repo', 'worktree', 'spec_path', 'path', 'root', 'dir', 'checkout'}
)


class Isolation:
    """Spawns children with a scrubbed environment and a throwaway git index.

    `git status` refreshes stat data and rewrites the index it opens, so a
    rehearsal that reads a repository must hand git a copy; the repository the
    operator points at can be live and must come back byte-identical.
    """

    def __init__(self, scratch):
        self.scratch = Path(scratch)
        self.home = self.scratch / 'home'
        self.tmp = self.scratch / 'tmp'
        for path in (self.home, self.tmp):
            path.mkdir(mode=0o700, exist_ok=True)
        self.borrowed = 0

    def env(self, index=None):
        env = {name: os.environ[name] for name in INHERITED_ENV if name in os.environ}
        env.setdefault('PATH', '/usr/bin:/bin')
        env.update({
            # Not the caller's home: no ~/.gitconfig, no ~/.local/state/cadence.
            'HOME': str(self.home), 'TMPDIR': str(self.tmp), 'LC_ALL': 'C',
            'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
            'GIT_CONFIG_SYSTEM': '/dev/null', 'GIT_TERMINAL_PROMPT': '0',
            'GIT_ASKPASS': '/bin/false',
        })
        if index is not None:
            env['GIT_INDEX_FILE'] = str(index)
        return env

    def run(self, *args, index=None):
        result = subprocess.run(args, capture_output=True, text=True, check=False,
                                env=self.env(index), stdin=subprocess.DEVNULL,
                                cwd=self.scratch)
        if result.returncode:
            # Do not echo arbitrary export/remote/credential output into evidence.
            raise ValueError(f'{Path(args[0]).name} command failed (exit {result.returncode})')
        return result.stdout.strip()

    @contextlib.contextmanager
    def borrowed_index(self, repo):
        """A throwaway copy of `repo`'s index for one read-only git command."""
        # Asked without GIT_INDEX_FILE, or git would just echo it back.
        live = Path(self.run('git', '-C', str(repo), 'rev-parse',
                             '--path-format=absolute', '--git-path', 'index'))
        self.borrowed += 1
        copy = self.scratch / f'borrowed-index-{self.borrowed}'
        if live.is_file() and not live.is_symlink():
            shutil.copyfile(live, copy)
        try:
            yield copy
        finally:
            copy.unlink(missing_ok=True)


def strip_credentials(url):
    """`https://user:token@host/p` becomes `https://host/p` (src/backup/mod.rs)."""
    url = url.strip()
    scheme, separator, rest = url.partition('://')
    if not separator:
        return url
    cut = rest.find('/')
    authority, path = (rest, '') if cut < 0 else (rest[:cut], rest[cut:])
    _, _, host = authority.rpartition('@')
    return f'{scheme}://{host}{path}'


def assert_no_credentials(label, text):
    if CREDENTIAL_URL.search(text):
        raise ValueError(f'{label} carries a credentialed URL; refusing to emit it')


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


def recorded_paths(path):
    """Every path column value the export/restore CLI would resolve."""
    found = set()
    with sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True) as db:
        tables = [row[0] for row in db.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
        )]
        for table in tables:
            escaped = table.replace('"', '""')
            for column in (row[1] for row in db.execute(f'PRAGMA table_info("{escaped}")')):
                if column not in PATH_COLUMN_NAMES and (table, column) not in PATH_COLUMNS:
                    continue
                quoted = column.replace('"', '""')
                for (value,) in db.execute(
                    f'SELECT DISTINCT "{quoted}" FROM "{escaped}" WHERE "{quoted}" IS NOT NULL'
                ):
                    if isinstance(value, str) and value:
                        found.add(value)
    return sorted(found)


def would_read(value, cwd):
    """The directory `discover_repos` would run git in for one column value.

    Exactly the Rust rule (`src/backup/mod.rs`): the value itself when it is a
    directory, else its parent when *that* is a directory, else nothing. A
    `spec_path` that no longer exists still names its live checkout. A relative
    value resolves against the child's cwd, which is the private scratch dir.
    """
    path = Path(value)
    if not path.is_absolute():
        path = Path(cwd) / path
    if path.is_dir():
        target = path
    elif path.parent.is_dir():
        target = path.parent
    else:
        return None  # Nothing to read: the CLI skips it too.
    return target.resolve()


def refuse_foreign_checkouts(db_path, roots, cwd):
    """Refuse a source whose path columns reach a directory outside the rehearsal.

    `cadence export` (`discover_repos`) and `cadence restore` (`plan_remap`) run
    `git -C <dir> remote get-url origin` on every recorded path that resolves to
    a directory. An offline copy of a real store still names live developer
    checkouts, so the rehearsal would read — and could print — a remote holding
    `user:token@`. The rehearsal reads no repository it does not own.
    """
    outside = []
    for value in recorded_paths(db_path):
        resolved = would_read(value, cwd)
        if resolved is None:
            continue
        if not any(resolved == root or resolved.is_relative_to(root) for root in roots):
            outside.append(str(resolved))
    if outside:
        outside = sorted(set(outside))
        shown = ', '.join(outside[:3]) + (' …' if len(outside) > 3 else '')
        raise ValueError(
            f'source path columns reach {len(outside)} director(y/ies) outside the '
            f'rehearsal ({shown}); export and restore run git in them, and such a '
            'remote may embed a token. NULL or remap those path columns in the '
            'offline copy first'
        )
    return recorded_paths(db_path)


def content_digest(path, forbid_credentials=False):
    """Compare offline source content without printing rows or credentials."""
    digest = hashlib.sha256()
    with sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True) as db:
        for statement in db.iterdump():
            if forbid_credentials:
                assert_no_credentials('exported row content', statement)
            digest.update(statement.encode())
            digest.update(b'\n')
    return digest.hexdigest()


def verify_bundle(bundle):
    manifest_path = bundle / 'manifest.json'
    if manifest_path.is_symlink():
        raise ValueError('manifest must not be a symlink')
    text = manifest_path.read_text()
    assert_no_credentials('export manifest', text)
    manifest = json.loads(text)
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


def recorded_repos(bundle):
    """The manifest's checkouts, with userinfo stripped from every remote."""
    manifest = json.loads((bundle / 'manifest.json').read_text())
    repos = []
    for repo in manifest.get('repos') or []:
        repos.append({'path': str(repo.get('path', '')),
                      'remote': strip_credentials(str(repo.get('remote', '')))})
    return repos


def clean_tracker(iso, tracker):
    """The tracker's commit, read without writing anything in it."""
    tracker = Path(tracker)
    with iso.borrowed_index(tracker) as index:
        # `--untracked-files=all` and the throwaway index beat a repository
        # config that would otherwise hide untracked files from this check.
        status = iso.run('git', '-C', str(tracker), '-c', 'core.fsmonitor=false',
                         '-c', 'gc.auto=0', 'status', '--porcelain',
                         '--untracked-files=all', index=index)
    if status:
        raise ValueError('tracker must be clean, including untracked files')
    if iso.run('git', '-C', str(tracker), 'rev-parse', '--show-toplevel') != str(tracker):
        raise ValueError('tracker must name the repository root')
    return iso.run('git', '-C', str(tracker), 'rev-parse', 'HEAD')


def preflight(binary, source, tracker, output):
    """Every check that needs no lock, no child write and no output directory."""
    source, output = temporary(source), temporary(output)
    tracker = Path(tracker).resolve()
    if tracker == (Path.home() / 'pm').resolve():
        raise ValueError('use an isolated tracker clone, never the production PM directory')
    if output.exists() or output.is_relative_to(source) or source.is_relative_to(output):
        raise ValueError('output must be a new directory separate from source state')
    owned(source, directory=True)
    owned(source / 'cadence.sqlite3', directory=False)
    owned(source / 'cadence.lock', directory=False)
    owned(tracker, directory=True)
    owned(output.parent, directory=True)
    if not Path(binary).is_absolute() or not Path(binary).is_file():
        raise ValueError('pass an explicit existing cadence binary path')
    return source, tracker, output


@contextlib.contextmanager
def source_lock(source, hold):
    """The daemon's singleton flock. `hold=False` probes and lets go at once.

    Taking and dropping an advisory lock leaves nothing on disk, so a dry run
    can report whether a daemon is active without a lasting side effect.
    """
    lock_fd = os.open(source / 'cadence.lock', os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(lock_fd, 'rb') as lock:
        if not stat.S_ISREG(os.fstat(lock.fileno()).st_mode):
            raise ValueError('cadence.lock must be a regular file')
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError('source daemon is active; use an offline rehearsal copy') from error
        if not hold:
            fcntl.flock(lock, fcntl.LOCK_UN)
        yield


def rehearse(binary, source, tracker, output, dry_run=False):
    source, tracker, output = preflight(binary, source, tracker, output)
    with tempfile.TemporaryDirectory(prefix='cad529-isolation-') as scratch:
        iso = Isolation(scratch)
        # Read-only: the tracker's own index is never the one git opens.
        sha = clean_tracker(iso, tracker)
        roots = (source, tracker, output)
        db = source / 'cadence.sqlite3'
        with source_lock(source, hold=not dry_run):
            paths = refuse_foreign_checkouts(db, roots, iso.scratch)
            before = inspect_db(db)
            if dry_run:
                return plan(binary, source, tracker, output, sha, before, paths)
            return execute(iso, binary, source, tracker, output, sha, before,
                           content_digest(db), paths)


def plan(binary, source, tracker, output, sha, inventory, paths):
    """What a real run would do. It has already done nothing."""
    bundle, restored_dir = output / 'bundle', output / 'restored-state'
    return {
        'format': 'cadence.migration-rehearsal/1', 'mode': 'dry-run',
        # Nothing outside this process survives it: no output directory, no
        # index write, no file in the source state, and the source flock was
        # probed and released. The private scratch dir deletes itself.
        'mutations_performed': [],
        'tracker_sha': sha, 'inventory': inventory,
        'source_lock_available': True,
        'recorded_checkout_paths': paths,
        'would_create': [str(output), str(bundle), str(restored_dir),
                         str(output / 'tracker'), str(output / 'receipt.json')],
        'would_run': [f'{binary} --state-dir {source} export --out {bundle}',
                      f'{binary} --state-dir {restored_dir} restore {bundle}',
                      f'git clone --no-hardlinks --no-checkout {tracker} {output}/tracker'],
        'would_hold_source_flock': str(source / 'cadence.lock'),
        'would_verify': ['portable export manifest', 'integrity/schema/counts',
                         'delivery states', 'restored row content',
                         'source content unchanged', 'tracker commit unchanged',
                         'no URL carrying userinfo in manifest, rows or receipt'],
        'production_cutover_authorized': False,
    }


def execute(iso, binary, source, tracker, output, sha, before, before_digest, paths):
    db = source / 'cadence.sqlite3'
    output.mkdir(mode=0o700, parents=False)
    bundle = output / 'bundle'
    iso.run(binary, '--state-dir', str(source), 'export', '--out', str(bundle))
    exported, digest = verify_bundle(bundle)
    content_digest(bundle / 'cadence.sqlite3', forbid_credentials=True)
    repos = recorded_repos(bundle)
    export_exclusions = json.loads((bundle / 'manifest.json').read_text())['export'].get('excludes', [])
    if not isinstance(export_exclusions, list) or any(not isinstance(item, str) for item in export_exclusions):
        raise ValueError('invalid export exclusions inventory')
    if (before != exported or inspect_db(db) != before
            or content_digest(db) != before_digest):
        raise ValueError('source inventory changed; freeze every rehearsal writer and retry')
    restored_dir = output / 'restored-state'
    iso.run(binary, '--state-dir', str(restored_dir), 'restore', str(bundle))
    restored = inspect_db(restored_dir / 'cadence.sqlite3', portable=True)
    if restored != exported:
        raise ValueError('restored schema/counts/delivery states differ from export')
    if content_digest(restored_dir / 'cadence.sqlite3') != content_digest(bundle / 'cadence.sqlite3'):
        raise ValueError('restored row content differs from export')
    tracker_copy = output / 'tracker'
    iso.run('git', '-c', 'gc.auto=0', 'clone', '--quiet', '--no-hardlinks',
            '--no-checkout', str(tracker), str(tracker_copy))
    iso.run('git', '-C', str(tracker_copy), 'checkout', '--quiet', '--detach', sha)
    if clean_tracker(iso, tracker) != sha or clean_tracker(iso, tracker_copy) != sha:
        raise ValueError('tracker changed during rehearsal')
    receipt = {
        'format': 'cadence.migration-rehearsal/1', 'mode': 'offline-rehearsal',
        'tracker_sha': sha, 'export_sha256': digest, 'inventory': exported,
        'restored_inventory_matches': True,
        'restored_content_matches': True,
        'export_exclusions': export_exclusions,
        'recorded_checkout_paths': paths,
        'recorded_repos': repos,
        'isolation': {
            'child_env_allowlist': list(INHERITED_ENV),
            'tracker_index': 'throwaway copy; the source index is never written',
            'foreign_checkout_reads': 'refused before the export runs',
            'credential_scan': 'no URL carrying userinfo in manifest, rows or receipt',
        },
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
    text = json.dumps(receipt, indent=2)
    assert_no_credentials('rehearsal receipt', text)
    receipt_path = output / 'receipt.json'
    with receipt_path.open('x') as stream:
        os.chmod(receipt_path, 0o600)
        stream.write(text + '\n')
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cadence', required=True, help='explicit binary, never installed/replaced')
    parser.add_argument('--source-state', required=True, help='offline owned /tmp state with cadence.lock')
    parser.add_argument('--tracker', required=True, help='clean isolated tracker clone')
    parser.add_argument('--out', required=True, help='new owned /tmp output directory')
    parser.add_argument('--dry-run', action='store_true',
                        help='report the plan and mutate nothing: no output directory, '
                             'no index write, no retained lock')
    args = parser.parse_args()
    try:
        receipt = rehearse(args.cadence, args.source_state, args.tracker, args.out,
                           dry_run=args.dry_run)
    except (ValueError, OSError, sqlite3.Error, json.JSONDecodeError) as error:
        print(f'rehearsal refused: {error}', file=sys.stderr)
        return 1
    text = json.dumps(receipt, indent=2)
    assert_no_credentials('rehearsal output', text)
    print(text)
    return 0


if __name__ == '__main__':
    sys.exit(main())
