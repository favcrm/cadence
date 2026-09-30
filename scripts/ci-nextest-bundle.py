#!/usr/bin/env python3
"""Verify producer bytes against independent workflow context and job outputs.

This boundary does not compile, extract or authenticate a caller. Expected
identity/build values must come from checkout/tool/runner context and producer
outputs, never from the downloaded manifest being checked.
"""
import argparse
import ast
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
import tarfile

MAX_MANIFEST_BYTES = 65536
CONFIG_FILES = frozenset({
    'Cargo.toml', 'Cargo.lock', 'build.rs', '.config/nextest.toml',
    '.config/cargo-nextest.sha256', 'scripts/cadence-nextest',
    'scripts/nextest-inventory', 'scripts/ci-rust-tests.py',
    'scripts/ci-nextest-bundle.py', 'tests/shard-weights.json',
    '.config/ci-test-runtime.env', 'scripts/ci-test-runtime.py',
    'scripts/ci-test-runtime-bootstrap', '.github/workflows/ci.yml',
})
FIELDS = frozenset({
    'schema', 'source_sha', 'run_id', 'producer_attempt', 'nextest_version',
    'features', 'workspace_root', 'build', 'plan_sha256', 'inventory_sha256', 'archive_sha256',
})


def regular_file(path):
    """Open only a regular final component, never a symlink/device/FIFO."""
    path = Path(path)
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError as error:
        raise ValueError(f'{path.name}: regular file required') from error
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise ValueError(f'{path.name}: regular file required')
        return os.fdopen(fd, 'rb')
    except BaseException:
        os.close(fd)
        raise


def file_digest(path):
    """Stream large archives rather than duplicate them in every shard's RAM."""
    digest = hashlib.sha256()
    with regular_file(path) as source:
        for chunk in iter(lambda: source.read(65536), b''):
            digest.update(chunk)
    return digest.hexdigest()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f'duplicate JSON key: {key}')
        result[key] = value
    return result


def checked_text(value, pattern, field):
    if type(value) is not str or not re.fullmatch(pattern, value):
        raise ValueError(f'invalid {field}')


def select_artifact(context, producer):
    """Select only immutable ID/attempt supplied by trusted successful job needs.

    A later failed-only consumer rerun retains the successful producer's original
    ID and attempt. This validates supplied authority; it does not query Actions,
    authenticate callers, guess artifact names or prove retained bytes exist.
    """
    if type(context) is not dict or set(context) != {'source_sha', 'run_id', 'consumer_attempt'}:
        raise ValueError('workflow context missing or unknown')
    checked_text(context['source_sha'], r'[0-9a-f]{40}', 'source_sha')
    for field in ('run_id', 'consumer_attempt'):
        checked_text(context[field], r'[1-9][0-9]{0,19}', field)
    if type(producer) is not dict or set(producer) != {'result', 'outputs'} or producer['result'] != 'success':
        raise ValueError('producer must have complete successful job evidence')
    outputs = producer['outputs']
    digests = {'archive_sha256', 'plan_sha256', 'inventory_sha256', 'manifest_sha256'}
    fields = {'source_sha', 'run_id', 'producer_attempt', 'artifact_id', 'mode'} | digests
    if type(outputs) is not dict or set(outputs) != fields:
        raise ValueError('producer outputs missing or unknown')
    if outputs['mode'] not in ('full', 'selected', 'docs'):
        raise ValueError('unknown producer scope mode')
    checked_text(outputs['source_sha'], r'[0-9a-f]{40}', 'producer source_sha')
    for field in ('run_id', 'producer_attempt', 'artifact_id'):
        checked_text(outputs[field], r'[1-9][0-9]{0,19}', field)
    for field in digests:
        checked_text(outputs[field], r'[0-9a-f]{64}', field)
    if outputs['source_sha'] != context['source_sha'] or outputs['run_id'] != context['run_id']:
        raise ValueError('producer source/run does not match workflow context')
    if int(outputs['producer_attempt']) > int(context['consumer_attempt']):
        raise ValueError('producer attempt cannot be in the future')
    return dict(outputs)


def validate_record(record):
    """Accept only the feature/profile/runner shape this bundle protocol serves."""
    if type(record) is not dict or set(record) != FIELDS:
        raise ValueError('record fields missing or unknown')
    if type(record['schema']) is not int or record['schema'] != 1:
        raise ValueError('schema must be integer 1')
    checked_text(record['source_sha'], r'[0-9a-f]{40}', 'source_sha')
    for field in ('run_id', 'producer_attempt'):
        checked_text(record[field], r'[1-9][0-9]{0,19}', field)
    if record['nextest_version'] != '0.9.145' or record['features'] != ['test-seam']:
        raise ValueError('nextest/feature shape must remain pinned')
    root = record['workspace_root']
    if type(root) is not str or not root.startswith('/') or '..' in PurePosixPath(root).parts or str(PurePosixPath(root)) != root:
        raise ValueError('workspace_root must be a canonical absolute POSIX path')
    build = record['build']
    fields = {'target_root', 'profile', 'rustc_version', 'cargo_version', 'target_triple',
              'rustflags', 'runner_os', 'runner_arch', 'image_os', 'image_version',
              'nextest_sha256', 'config_sha256', 'runtime'}
    if type(build) is not dict or set(build) != fields:
        raise ValueError('build fields missing or unknown')
    if build['target_root'] != root + '/target' or build['profile'] != 'test' or build['rustflags'] != '-D warnings':
        raise ValueError('build target/profile/flags are incompatible')
    if build['runner_os'] != 'Linux' or build['runner_arch'] != 'X64' or build['target_triple'] != 'x86_64-unknown-linux-gnu':
        raise ValueError('archive runner/target shape is incompatible')
    for field in ('rustc_version', 'cargo_version', 'image_os', 'image_version'):
        if type(build[field]) is not str or not build[field].strip() or len(build[field]) > 4096:
            raise ValueError(f'invalid build {field}')
    if not build['rustc_version'].startswith('rustc ') or 'host: ' + build['target_triple'] not in build['rustc_version'].splitlines():
        raise ValueError('rustc identity does not match target')
    if not build['cargo_version'].startswith('cargo '):
        raise ValueError('cargo identity missing')
    runtime = build['runtime']
    if type(runtime) is not dict or set(runtime) != {'container_image', 'os_release_sha256', 'packages_sha256', 'abi_sha256'}:
        raise ValueError('build runtime fields missing or unknown')
    checked_text(runtime['container_image'], r'docker\.io/library/rust@sha256:[0-9a-f]{64}', 'build container image')
    for field in ('os_release_sha256', 'packages_sha256'):
        checked_text(runtime[field], r'[0-9a-f]{64}', 'build runtime ' + field)
    if type(runtime['abi_sha256']) is not dict or set(runtime['abi_sha256']) != {'libc', 'libstdcxx', 'loader'}:
        raise ValueError('build runtime ABI set missing or unknown')
    for name, digest in runtime['abi_sha256'].items():
        checked_text(digest, r'[0-9a-f]{64}', 'build runtime ABI ' + name)
    checked_text(build['nextest_sha256'], r'[0-9a-f]{64}', 'nextest_sha256')
    configs = build['config_sha256']
    if type(configs) is not dict or set(configs) != CONFIG_FILES:
        raise ValueError('build config set missing or unknown')
    for path, digest in configs.items():
        checked_text(digest, r'[0-9a-f]{64}', 'config digest: ' + path)
    for field in ('plan_sha256', 'inventory_sha256', 'archive_sha256'):
        checked_text(record[field], r'[0-9a-f]{64}', field)


def verify_bundle(directory, expected):
    """Return the verified archive path; extraction/runtime remain separate."""
    if type(expected) is not dict or set(expected) != FIELDS | {'manifest_sha256'}:
        raise ValueError('trusted context missing or unknown')
    validate_record({field: expected[field] for field in FIELDS})
    checked_text(expected['manifest_sha256'], r'[0-9a-f]{64}', 'manifest_sha256')
    directory = Path(directory)
    with regular_file(directory / 'bundle.json') as stream:
        raw = stream.read(MAX_MANIFEST_BYTES + 1)
    if len(raw) > MAX_MANIFEST_BYTES:
        raise ValueError('manifest is too large')
    if hashlib.sha256(raw).hexdigest() != expected['manifest_sha256']:
        raise ValueError('manifest_sha256 mismatch')
    manifest = json.loads(raw, object_pairs_hook=unique_object)
    validate_record(manifest)
    for key in FIELDS:
        actual, wanted = manifest[key], expected[key]
        if key == 'build':
            # Host image labels remain immutable observations in the sealed
            # manifest, not the userspace compatibility authority. The exact
            # pinned container, installed packages, ELF ABI bytes, compiler,
            # tools, flags, config and paths must still all match.
            observations = {'image_os', 'image_version'}
            actual = {name: value for name, value in actual.items() if name not in observations}
            wanted = {name: value for name, value in wanted.items() if name not in observations}
        if type(actual) is not type(wanted) or actual != wanted:
            raise ValueError(f'{key} mismatch')
    for filename, field in (
        ('nextest.tar.zst', 'archive_sha256'), ('ci-test-plan.json', 'plan_sha256'),
        ('inventory.json', 'inventory_sha256'),
    ):
        if file_digest(directory / filename) != expected[field]:
            raise ValueError(f'{field} mismatch')
    return directory / 'nextest.tar.zst'


def read_context(path):
    with regular_file(path) as stream:
        raw = stream.read(MAX_MANIFEST_BYTES + 1)
    if len(raw) > MAX_MANIFEST_BYTES:
        raise ValueError('trusted context is too large')
    return json.loads(raw, object_pairs_hook=unique_object)


class LimitedArchiveReader:
    def __init__(self, source, limit):
        self.source = source
        self.remaining = limit

    def read(self, size=-1):
        if size < 0:
            raise ValueError('unbounded archive read refused')
        data = self.source.read(min(size, self.remaining + 1))
        self.remaining -= len(data)
        if self.remaining < 0:
            raise ValueError('archive byte budget exceeded')
        return data

    def __getattr__(self, name):
        return getattr(self.source, name)


class CheckedTarInfo(tarfile.TarInfo):
    """Bound pseudo-header allocation before tarfile resolves effective names.

    Pinned nextest emits GNU headers, never pax or link headers. Accept its
    GNU long names and sparse files, but do not let their metadata evade the
    member/expanded-byte checks by allocating first inside the stdlib parser.
    """
    def _proc_member(self, archive):
        archive.bundle_headers = getattr(archive, 'bundle_headers', 0) + 1
        if archive.bundle_headers > archive.bundle_max_headers:
            raise ValueError('archive header budget exceeded')
        allowed = (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE,
                   tarfile.GNUTYPE_SPARSE, tarfile.GNUTYPE_LONGNAME)
        if self.type not in allowed or self.size < 0:
            raise ValueError('archive entry type refused')
        if self.type == tarfile.GNUTYPE_LONGNAME:
            depth = getattr(archive, 'bundle_metadata_depth', 0)
            if self.size > 65536 or depth >= 16:
                raise ValueError('archive metadata budget exceeded')
            archive.bundle_metadata_depth = depth + 1
            try:
                return super()._proc_member(archive)
            finally:
                archive.bundle_metadata_depth = depth
        if self.type == tarfile.GNUTYPE_SPARSE:
            stored = self.size
            logical = self._sparse_structs[2]
            if logical < stored or logical > archive.bundle_max_expanded:
                raise ValueError('sparse expanded-size budget exceeded')
            original = archive.fileobj
            archive.fileobj = LimitedArchiveReader(original, 65536)
            try:
                result = super()._proc_member(archive)
            finally:
                archive.fileobj = original
            end = total = 0
            for offset, length in result.sparse:
                if offset < 0 or length < 0 or offset + length > logical or (length and offset < end):
                    raise ValueError('invalid sparse map')
                if length:
                    end = offset + length
                    total += length
            if total != stored:
                raise ValueError('sparse map does not match stored bytes')
            return result
        if self.size > archive.bundle_max_expanded:
            raise ValueError('archive expanded-size budget exceeded')
        return super()._proc_member(archive)


class CheckedTarFile(tarfile.TarFile):
    # Class defaults are needed during TarFile.__init__'s first header parse.
    bundle_max_headers = 40064
    bundle_max_expanded = 32 * 1024**3


def check_archive(path, *, max_members=20000, max_expanded_bytes=32 * 1024**3):
    """Preflight pinned GNU tar.zst shape; never writes an extracted file.

    Limits are explicit refusal thresholds, not sampling. They need current CI
    validation against the real archive before any compatibility/speed claim.
    """
    if type(max_members) is not int or max_members <= 0 or type(max_expanded_bytes) is not int or max_expanded_bytes <= 0:
        raise ValueError('invalid archive budget')
    required = {'target/nextest/binaries-metadata.json', 'target/nextest/cargo-metadata.json'}
    seen = set()
    expanded = 0
    with regular_file(path) as source:
        process = subprocess.Popen(['zstd', '--decompress', '--stdout', '--no-progress'],
                                   stdin=source, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        try:
            reader = LimitedArchiveReader(process.stdout, max_expanded_bytes + 64 * 1024**2)
            with CheckedTarFile.open(fileobj=reader, mode='r|', tarinfo=CheckedTarInfo,
                                     encoding='utf-8', errors='strict') as archive:
                archive.bundle_max_headers = max_members * 2 + 64
                archive.bundle_max_expanded = max_expanded_bytes
                for member in archive:
                    name = member.name
                    parts = name.split('/')
                    if len(parts) < 2 or parts[0] != 'target' or any(part in ('', '.', '..') for part in parts) or '\\' in name:
                        raise ValueError('archive path must have normal target components')
                    name.encode('utf-8', errors='strict')
                    if name in seen:
                        raise ValueError('duplicate archive member')
                    seen.add(name)
                    expanded += member.size
                    if len(seen) > max_members or expanded > max_expanded_bytes:
                        raise ValueError('archive member/expanded-size budget exceeded')
                    if name in required and member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE):
                        raise ValueError('archive metadata must be regular files')
                if not required.issubset(seen):
                    raise ValueError('required archive metadata missing')
                # Drain buffered padding and the frame trailer so checksum and
                # decoder status are checked, with no hidden second tar stream.
                for chunk in iter(lambda: archive.fileobj.read(65536), b''):
                    if any(chunk):
                        raise ValueError('non-padding bytes after archive end')
            if process.wait(timeout=30) != 0:
                raise ValueError('zstd archive decoding failed')
        except (tarfile.TarError, UnicodeError, OSError, subprocess.SubprocessError) as error:
            raise ValueError('invalid compressed archive') from error
        finally:
            process.stdout.close()
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    return {'members': len(seen), 'expanded_bytes': expanded}


def command_text(root, command, env):
    return subprocess.run(command, cwd=root, env=env, check=True, capture_output=True,
                          text=True, timeout=30).stdout.strip()


def workflow_context(root, env):
    root = Path(root).resolve(strict=True)
    sha = command_text(root, ['git', 'rev-parse', 'HEAD'], env)
    if sha != env.get('GITHUB_SHA') or str(root) != env.get('GITHUB_WORKSPACE'):
        raise ValueError('checkout source/workspace does not match workflow')
    subprocess.run(['git', 'diff', '--quiet', 'HEAD', '--'], cwd=root, env=env, check=True)
    context = {'source_sha': sha, 'run_id': env.get('GITHUB_RUN_ID'),
               'consumer_attempt': env.get('GITHUB_RUN_ATTEMPT')}
    checked_text(sha, r'[0-9a-f]{40}', 'workflow SHA')
    for field in ('run_id', 'consumer_attempt'):
        checked_text(context[field], r'[1-9][0-9]{0,19}', field)
    return context


def collect_identity(root, env):
    root = Path(root).resolve(strict=True)
    context = workflow_context(root, env)
    for name in ('CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET', 'RUSTC', 'CARGO',
                 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'):
        if env.get(name):
            raise ValueError('unsupported ambient build override: ' + name)
    if any(name.startswith('CARGO_PROFILE_') for name in env):
        raise ValueError('unsupported ambient Cargo profile override')
    if env.get('CARGO_TARGET_DIR') and env['CARGO_TARGET_DIR'] != str(root / 'target'):
        raise ValueError('CARGO_TARGET_DIR does not match baked fixture paths')
    cargo_home = Path(env.get('CARGO_HOME', str(Path(env['HOME']) / '.cargo')))
    for path in (root / '.cargo/config', root / '.cargo/config.toml',
                 cargo_home / 'config', cargo_home / 'config.toml'):
        if path.exists() or path.is_symlink():
            raise ValueError('unrecorded Cargo configuration: ' + str(path))
    tool = Path(env['CADENCE_NEXTTEST_BIN'])
    tool_sha = file_digest(tool)
    with regular_file(root / '.config/cargo-nextest.sha256') as stream:
        pins = stream.read(MAX_MANIFEST_BYTES + 1).decode()
    matches = re.findall(r'(?m)^([0-9a-f]{64})\s+cargo-nextest$', pins)
    if matches != [tool_sha]:
        raise ValueError('nextest executable differs from source pin')
    version = command_text(root, [str(tool), '--version'], env)
    if version.split()[:2] != ['cargo-nextest', '0.9.145']:
        raise ValueError('nextest version differs from protocol pin')
    rustc = command_text(root, ['rustc', '--version', '--verbose'], env)
    hosts = re.findall(r'(?m)^host: (.+)$', rustc)
    if len(hosts) != 1:
        raise ValueError('rustc host identity missing or ambiguous')
    runtime_text = command_text(root, [sys.executable, str(root / 'scripts/ci-test-runtime.py'),
                                      '--root', str(root)], env)
    if len(runtime_text.encode()) > MAX_MANIFEST_BYTES:
        raise ValueError('runtime identity too large')
    runtime = json.loads(runtime_text, object_pairs_hook=unique_object)
    with regular_file(root / '.config/ci-test-runtime.env') as stream:
        pin = stream.read(MAX_MANIFEST_BYTES + 1).decode()
    images = re.findall(r"(?m)^CI_TEST_IMAGE='([^']+)'$", pin)
    if len(images) != 1 or type(runtime) is not dict or runtime.get('container_image') != images[0]:
        raise ValueError('local runtime differs from source-pinned container')
    identity = {'schema': 1, 'source_sha': context['source_sha'], 'run_id': context['run_id'],
                'producer_attempt': context['consumer_attempt'], 'nextest_version': '0.9.145',
                'features': ['test-seam'], 'workspace_root': str(root),
                'build': {'target_root': str(root / 'target'), 'profile': 'test',
                          'rustc_version': rustc, 'cargo_version': command_text(root, ['cargo', '--version'], env),
                          'target_triple': hosts[0], 'rustflags': env.get('RUSTFLAGS'),
                          'runner_os': env.get('RUNNER_OS'), 'runner_arch': env.get('RUNNER_ARCH'),
                          'image_os': env.get('ImageOS') or 'unreported',
                          'image_version': env.get('ImageVersion') or 'unreported',
                          'runtime': runtime,
                          'nextest_sha256': tool_sha,
                          'config_sha256': {name: file_digest(root / name) for name in CONFIG_FILES}}}
    validate_record(dict(identity, archive_sha256='0' * 64, plan_sha256='0' * 64, inventory_sha256='0' * 64))
    return identity


def write_json(path, doc):
    raw = (json.dumps(doc, sort_keys=True, indent=2) + '\n').encode()
    if len(raw) > MAX_MANIFEST_BYTES:
        raise ValueError('output descriptor is too large')
    with Path(path).open('xb') as stream:
        stream.write(raw)
    return hashlib.sha256(raw).hexdigest()


def publish_outputs(outputs, env):
    path = env.get('GITHUB_OUTPUT')
    if path:
        for key, value in outputs.items():
            if type(value) is not str or '\n' in value or '\r' in value:
                raise ValueError('unsafe job output: ' + key)
        with open(path, 'a') as stream:
            stream.write(''.join(f'{key}={value}\n' for key, value in sorted(outputs.items())))


def expected_context(root, reference, env):
    context = workflow_context(root, env)
    selected = select_artifact(context, {'result': 'success', 'outputs': reference})
    identity = collect_identity(root, env)
    identity['producer_attempt'] = selected['producer_attempt']
    return dict(identity, **{field: selected[field] for field in
                            ('archive_sha256', 'plan_sha256', 'inventory_sha256', 'manifest_sha256')})


def archive_capable(path):
    with regular_file(path) as stream:
        tree = ast.parse(stream.read().decode())
    return any(isinstance(node, ast.Assign) and isinstance(node.value, ast.Constant)
               and type(node.value.value) is int and node.value.value == 1
               and any(isinstance(target, ast.Name) and target.id == 'ARCHIVE_PROTOCOL' for target in node.targets)
               for node in tree.body)


def prepare_bundle(root, plan_path, inventory_runner, directory, env):
    root = Path(root).resolve(strict=True)
    identity = collect_identity(root, env)
    plan = read_context(plan_path)
    spec = importlib.util.spec_from_file_location('bundle_scope_runner', Path(__file__).with_name('ci-rust-tests.py'))
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    if type(plan.get('schema')) is not int:
        raise ValueError('plan schema must be integer')
    runner.scope_args(plan)
    if not archive_capable(inventory_runner):
        plan = dict(plan, mode='full', targets=[], original_plan_sha256=file_digest(plan_path),
                    reason='base runner lacks archive protocol; conservative full archive fallback')
    selectors = runner.scope_args(plan)
    directory = Path(directory).absolute()
    if directory.is_relative_to(root / 'target'):
        raise ValueError('bundle destination must be outside target')
    directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    write_json(directory / 'ci-test-plan.json', plan)
    archive = directory / 'nextest.tar.zst'
    if plan['mode'] == 'docs':
        archive.write_bytes(b'')
        write_json(directory / 'inventory.json', {'rust-suites': {}, 'test-count': 0})
    else:
        subprocess.run([sys.executable, str(inventory_runner), '--root', str(root), '--plan',
                        str(directory / 'ci-test-plan.json'), '--phase', 'inventory'], cwd=root, env=env, check=True)
        prefix = [str(root / 'scripts/cadence-nextest')]
        args = [*selectors, '--locked', '--features', 'test-seam']
        listed = subprocess.run([*prefix, 'list', *args, '--message-format', 'json'], cwd=root,
                                env=env, check=True, capture_output=True, text=True, timeout=300)
        inventory = json.loads(listed.stdout, object_pairs_hook=unique_object)
        if type(inventory) is not dict or not inventory.get('rust-suites'):
            raise ValueError('producer inventory must be nonempty')
        # Preserve the exact pinned-tool JSON rather than reconstructing suites.
        with (directory / 'inventory.json').open('x') as output:
            output.write(listed.stdout)
        with regular_file(root / 'target/debug/cadence'):
            pass
        version = command_text(root, [str(root / 'target/debug/cadence'), '--version'], env)
        if not version.endswith('+' + identity['source_sha']):
            raise ValueError('compiled CLI version does not match producer source')
        subprocess.run([*prefix, 'archive', *args, '--archive-file', str(archive)],
                       cwd=root, env=env, check=True)
    if collect_identity(root, env) != identity:
        raise ValueError('producer context changed while building')
    digests = {field: file_digest(directory / filename) for filename, field in (
        ('nextest.tar.zst', 'archive_sha256'), ('ci-test-plan.json', 'plan_sha256'),
        ('inventory.json', 'inventory_sha256'))}
    manifest = dict(identity, **digests)
    validate_record(manifest)
    manifest_sha = write_json(directory / 'bundle.json', manifest)
    verify_bundle(directory, dict(manifest, manifest_sha256=manifest_sha))
    outputs = dict(digests, manifest_sha256=manifest_sha, source_sha=identity['source_sha'],
                   run_id=identity['run_id'], producer_attempt=identity['producer_attempt'], mode=plan['mode'])
    publish_outputs(outputs, env)
    return outputs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest='operation', required=True)
    verify = modes.add_parser('verify', help='verify against independently supplied workflow authority')
    verify.add_argument('--directory', type=Path, required=True)
    verify.add_argument('--expected', type=Path, required=True)
    select = modes.add_parser('select', help='validate immutable trusted producer job outputs')
    select.add_argument('--root', type=Path, required=True)
    select.add_argument('--producer', type=Path, required=True)
    select.add_argument('--out', type=Path, required=True)
    expect = modes.add_parser('expect', help='capture independent local consumer context')
    expect.add_argument('--root', type=Path, required=True)
    expect.add_argument('--reference', type=Path, required=True)
    expect.add_argument('--out', type=Path, required=True)
    prepare = modes.add_parser('prepare', help='compile parity once, archive and seal producer bytes')
    prepare.add_argument('--root', type=Path, required=True)
    prepare.add_argument('--plan', type=Path, required=True)
    prepare.add_argument('--inventory-runner', type=Path, required=True)
    prepare.add_argument('--directory', type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.operation == 'prepare':
            print(json.dumps(prepare_bundle(args.root, args.plan, args.inventory_runner, args.directory, os.environ), sort_keys=True))
            return 0
        if args.operation == 'select':
            selected = select_artifact(workflow_context(args.root, os.environ), read_context(args.producer))
            write_json(args.out, selected)
            publish_outputs({'artifact_id': selected['artifact_id']}, os.environ)
            return 0
        if args.operation == 'expect':
            write_json(args.out, expected_context(args.root, read_context(args.reference), os.environ))
            return 0
        directory = args.directory.resolve(strict=True)
        authority = args.expected.resolve(strict=True)
        if authority.is_relative_to(directory):
            raise ValueError('expected context must be independent of the downloaded bundle directory')
        expected = read_context(args.expected)
        archive = verify_bundle(args.directory, expected)
        print(json.dumps({'verified': True, 'source_sha': expected['source_sha'],
                          'run_id': expected['run_id'], 'producer_attempt': expected['producer_attempt'],
                          'archive': str(archive), 'archive_sha256': expected['archive_sha256']}, sort_keys=True))
        return 0
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f'nextest-bundle: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
