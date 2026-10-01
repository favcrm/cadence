#!/usr/bin/env python3
"""Capture pinned-container userspace identity, independently of archive bytes.

The workflow selects the source-pinned image. This probe is not a Docker
attestation or caller authentication endpoint. It checks that job selection,
container execution, distro, compiler and actual installed userspace agree;
the existing source/job-output custody still supplies the trust boundary.
Linux kernel and hosted-image labels are observations, not userspace identity.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import time

spec = importlib.util.spec_from_file_location('runtime_bundle', Path(__file__).with_name('ci-nextest-bundle.py'))
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)

PIN_KEYS = {'CI_TEST_IMAGE', 'CI_TEST_SNAPSHOT', 'CI_TEST_OS_ID', 'CI_TEST_OS_VERSION', 'CI_TEST_TOOLCHAIN'}
ABI_FILES = {'libc': '/usr/lib/x86_64-linux-gnu/libc.so.6',
             'libstdcxx': '/usr/lib/x86_64-linux-gnu/libstdc++.so.6',
             'loader': '/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2'}


def read_pin(root):
    with bundle.regular_file(Path(root) / '.config/ci-test-runtime.env') as stream:
        raw = stream.read(65537)
    if len(raw) > 65536:
        raise ValueError('runtime pin too large')
    pin = {}
    for line in raw.decode().splitlines():
        if not line or line.startswith('#'):
            continue
        match = re.fullmatch(r"([A-Z_]+)='([a-zA-Z0-9./:@_-]+)'", line)
        if not match or match[1] not in PIN_KEYS or match[1] in pin:
            raise ValueError('runtime pin has invalid/duplicate fields')
        pin[match[1]] = match[2]
    if set(pin) != PIN_KEYS:
        raise ValueError('runtime pin incomplete')
    bundle.checked_text(pin['CI_TEST_IMAGE'], r'docker\.io/library/rust@sha256:[0-9a-f]{64}', 'runtime image')
    bundle.checked_text(pin['CI_TEST_SNAPSHOT'], r'[0-9]{8}T[0-9]{6}Z', 'runtime snapshot')
    bundle.checked_text(pin['CI_TEST_TOOLCHAIN'], r'[0-9]+\.[0-9]+\.[0-9]+', 'runtime toolchain')
    if pin['CI_TEST_OS_ID'] != 'debian' or pin['CI_TEST_OS_VERSION'] not in ('12', '13'):
        raise ValueError('unsupported runtime distribution')
    return pin


def git_merge_base_capability(env):
    """The suite's audit/review tests need `git merge-tree --write-tree
    --merge-base=X` — Git 2.39 lacks it (the option arrived in 2.40), so a
    producer/consumer on an older Git fails those tests mid-suite. Probe
    the actual installed git, not the pinned image version string: create a
    scratch repo, prove the command line parses and the merge runs.
    """
    with tempfile.TemporaryDirectory(prefix='ci-git-probe-') as tmp:
        base = ['git', '-C', tmp]
        def run(*args):
            return subprocess.run(base + list(args), env=env, check=True,
                                  capture_output=True, text=True, timeout=30)
        run('init', '-q', '-b', 'main')
        run('-c', 'user.name=ci', '-c', 'user.email=ci@probe', 'commit', '-q', '--allow-empty', '-m', 'base')
        base_sha = run('rev-parse', 'HEAD').stdout.strip()
        run('-c', 'user.name=ci', '-c', 'user.email=ci@probe', 'commit', '-q', '--allow-empty', '-m', 'head')
        head_sha = run('rev-parse', 'HEAD').stdout.strip()
        try:
            run('merge-tree', '--write-tree', '--name-only',
                f'--merge-base={base_sha}', base_sha, head_sha)
        except subprocess.CalledProcessError as error:
            raise ValueError('installed git lacks the required merge-tree --merge-base capability: '
                             + error.stderr.strip()) from error


def orphan_reap_capability():
    """The suite's mocked tmux provider detaches pane processes, so a CI
    container without an init at PID 1 (no Docker `--init`) leaves exited
    orphans as zombies that `kill(pid, 0)` still counts as alive —
    death/liveness tests then fail. Prove this runtime reaps: double-fork
    so a grandchild is orphaned to PID 1, then require its /proc entry to
    disappear after it exits.
    """
    read_fd, write_fd = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(read_fd)
        grandchild = os.fork()
        if grandchild == 0:
            os._exit(0)  # orphaned grandchild exits; PID 1 must reap it
        os.write(write_fd, str(grandchild).encode())
        os._exit(0)      # child exits; grandchild reparents to PID 1
    os.close(write_fd)
    orphan_pid = int(os.read(read_fd, 64).decode().strip())
    os.close(read_fd)
    os.waitpid(child, 0)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            Path(f'/proc/{orphan_pid}/stat').read_text()
        except (FileNotFoundError, ProcessLookupError):
            # procfs can lose the task between open and read (ESRCH).
            return
        time.sleep(0.05)
    raise ValueError(
        'PID 1 does not reap orphaned descendants; the job container needs Docker --init')


def identity(root, env):
    pin = read_pin(root)
    if env.get('CADENCE_TEST_CONTAINER_IMAGE') != pin['CI_TEST_IMAGE']:
        raise ValueError('job container selection does not match source pin')
    with bundle.regular_file('/.dockerenv'):
        pass
    if platform.system() != 'Linux' or platform.machine() != 'x86_64' or os.getuid() != 1001:
        raise ValueError('runtime requires non-root uid 1001 on Linux x86_64')
    release = Path('/etc/os-release').resolve(strict=True)
    with bundle.regular_file(release) as stream:
        raw = stream.read(65537)
    if len(raw) > 65536:
        raise ValueError('os-release too large')
    entries = dict(line.split('=', 1) for line in raw.decode().splitlines() if '=' in line)
    if (entries.get('ID', '').strip('"') != pin['CI_TEST_OS_ID'] or
            entries.get('VERSION_ID', '').strip('"') != pin['CI_TEST_OS_VERSION']):
        raise ValueError('runtime distro does not match source pin')
    compiler = subprocess.run(['rustc', '--version'], check=True, capture_output=True, text=True, timeout=30).stdout.split()
    if compiler[:2] != ['rustc', pin['CI_TEST_TOOLCHAIN']]:
        raise ValueError('runtime compiler does not match source pin')
    git_merge_base_capability(env)
    orphan_reap_capability()
    packages = subprocess.run(['dpkg-query', '-W', '-f=${binary:Package}=${Version}\n'],
                              env=dict(env, LC_ALL='C'), check=True, capture_output=True, timeout=30).stdout
    if not packages or len(packages) > 1048576:
        raise ValueError('invalid installed-package inventory')
    lines = packages.splitlines()
    if len(lines) != len(set(lines)):
        raise ValueError('duplicate installed-package inventory')
    abi = {}
    for name, path in ABI_FILES.items():
        resolved = Path(path).resolve(strict=True)
        if not (resolved.is_relative_to('/usr/lib') or resolved.is_relative_to('/lib')):
            raise ValueError('runtime library resolves outside system library roots')
        with bundle.regular_file(resolved) as stream:
            header = stream.read(20)
        if header[:6] != b'\x7fELF\x02\x01' or header[18:20] != b'\x3e\x00':
            raise ValueError('runtime library is not x86_64 ELF')
        abi[name] = bundle.file_digest(resolved)
    return {'container_image': pin['CI_TEST_IMAGE'],
            'os_release_sha256': hashlib.sha256(raw).hexdigest(),
            'packages_sha256': hashlib.sha256(b'\n'.join(sorted(lines)) + b'\n').hexdigest(),
            'abi_sha256': abi}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(identity(args.root, os.environ), sort_keys=True))
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print('ci-test-runtime: ' + str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
