#!/usr/bin/env python3
"""Public producer/context CLI tests with private Git and fake tool executables."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CLI = ROOT / 'scripts/ci-nextest-bundle.py'
spec = importlib.util.spec_from_file_location('producer_bundle', CLI)
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


class ProducerCLI(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='cbp.')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'workspace'
        self.root.mkdir()
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        for name in bundle.CONFIG_FILES:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('fixture ' + name + '\n')
        self.program(self.bin / 'rustc', "assert sys.argv[1:] == ['--version', '--verbose']\nprint('rustc 1.98.1\\nhost: x86_64-unknown-linux-gnu')")
        self.program(self.bin / 'cargo', "assert sys.argv[1:] == ['--version']\nprint('cargo 1.98.1')")
        tool = self.bin / 'cargo-nextest'
        self.program(tool, "assert sys.argv[1:] == ['--version']\nprint('cargo-nextest 0.9.145')")
        digest = hashlib.sha256(tool.read_bytes()).hexdigest()
        (self.root / '.config/cargo-nextest.sha256').write_text(digest + '  cargo-nextest\n')
        self.env = dict(os.environ, HOME=str(self.root / 'home'), CARGO_HOME=str(self.root / 'cargo-home'),
                        PATH=str(self.bin) + ':' + os.environ['PATH'], GITHUB_WORKSPACE=str(self.root),
                        GITHUB_RUN_ID='12345', GITHUB_RUN_ATTEMPT='1', RUSTFLAGS='-D warnings',
                        RUNNER_OS='Linux', RUNNER_ARCH='X64', ImageOS='ubuntu24', ImageVersion='20260920.314.1',
                        CADENCE_NEXTTEST_BIN=str(tool))
        for name in ('CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET', 'CARGO_TARGET_DIR', 'RUSTC', 'CARGO',
                     'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'):
            self.env.pop(name, None)
        for name in list(self.env):
            if name.startswith('CARGO_PROFILE_'):
                self.env.pop(name)
        self.runtime = {'container_image': 'docker.io/library/rust@sha256:' + 'f' * 64,
                        'os_release_sha256': 'a' * 64, 'packages_sha256': 'b' * 64,
                        'abi_sha256': {'libc': 'c' * 64, 'libstdcxx': 'd' * 64, 'loader': 'e' * 64}}
        self.runtime_file = Path(self.temp.name) / 'runtime.json'
        self.runtime_file.write_text(json.dumps(self.runtime))
        self.env['RUNTIME_FIXTURE'] = str(self.runtime_file)
        (self.root / '.config/ci-test-runtime.env').write_text("CI_TEST_IMAGE='" + self.runtime['container_image'] + "'\n")
        self.program(self.root / 'scripts/ci-test-runtime.py',
                     "if os.environ.get('FAIL_RUNTIME'): sys.exit(74)\nprint(Path(os.environ['RUNTIME_FIXTURE']).read_text())")
        self.git('init', '-q')
        self.git('config', 'user.name', 'Private Test')
        self.git('config', 'user.email', 'test@example.invalid')
        self.git('add', '.')
        self.git('-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null', 'commit', '-qm', 'fixture')
        self.sha = self.git('rev-parse', 'HEAD').strip()
        self.env['GITHUB_SHA'] = self.sha
        self.plan = Path(self.temp.name) / 'plan.json'
        self.plan.write_text(json.dumps({'schema': 1, 'mode': 'full', 'targets': [], 'reason': 'fixture full'}))
        self.directory = Path(self.temp.name) / 'bundle'
        self.output = Path(self.temp.name) / 'outputs'
        self.env['GITHUB_OUTPUT'] = str(self.output)

    def program(self, path, body):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('#!' + sys.executable + '\nimport sys, os, json\nfrom pathlib import Path\n' + body + '\n')
        path.chmod(0o755)

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, env=self.env, text=True)

    def invoke(self, *args):
        return subprocess.run([sys.executable, str(CLI), *args], env=self.env, capture_output=True, text=True)

    def builder_fixture(self):
        self.env['COMMAND_TRACE'] = str(Path(self.temp.name) / 'commands.jsonl')
        self.runner = Path(self.temp.name) / 'trusted-runner.py'
        self.program(self.runner, "ARCHIVE_PROTOCOL = 1\nwith open(os.environ['COMMAND_TRACE'], 'a') as out: out.write(json.dumps(['inventory', *sys.argv[1:]]) + '\\n')\nif os.environ.get('FAIL_INVENTORY'): sys.exit(73)\nroot=Path(sys.argv[sys.argv.index('--root')+1]); cli=root/'target/debug/cadence'; cli.parent.mkdir(parents=True,exist_ok=True)\ncli.write_text('#!' + sys.executable + '\\nimport os\\nprint(\\\"cadence 0.1.0+\\\" + os.environ[\\\"GITHUB_SHA\\\"])\\n'); cli.chmod(0o755)")
        inventory = {'test-count': 1, 'rust-suites': {'cadence::lib': {'testcases': {
            'sample': {'kind': 'test', 'ignored': False, 'filter-match': {'status': 'matches'}}}}}}
        self.program(self.root / 'scripts/cadence-nextest', "with open(os.environ['COMMAND_TRACE'], 'a') as out: out.write(json.dumps(sys.argv[1:]) + '\\n')\nif sys.argv[1]=='list': print(" + repr(json.dumps(inventory)) + ")\nelif sys.argv[1]=='archive': Path(sys.argv[sys.argv.index('--archive-file')+1]).write_bytes(b'synthetic archive')\nelse: sys.exit(97)")
        self.git('add', '.')
        self.git('-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null', 'commit', '-qm', 'fake builder')
        self.sha = self.git('rev-parse', 'HEAD').strip()
        self.env['GITHUB_SHA'] = self.sha

    def prepare(self):
        return self.invoke('prepare', '--root', str(self.root), '--plan', str(self.plan),
                           '--inventory-runner', str(self.runner), '--directory', str(self.directory))

    def test_context_requires_independent_local_runtime_and_propagates_probe_failure(self):
        identity = bundle.collect_identity(self.root, self.env)
        self.assertEqual(identity['build']['runtime'], self.runtime)
        self.env['FAIL_RUNTIME'] = '1'
        with self.assertRaises(subprocess.CalledProcessError):
            bundle.collect_identity(self.root, self.env)

    def test_prepare_seals_real_files_and_publishes_outputs_after_parity(self):
        self.builder_fixture()
        result = self.prepare()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads((self.directory / 'bundle.json').read_text())
        expected = dict(manifest, manifest_sha256=hashlib.sha256((self.directory / 'bundle.json').read_bytes()).hexdigest())
        self.assertEqual(bundle.verify_bundle(self.directory, expected), self.directory / 'nextest.tar.zst')
        commands = [json.loads(line) for line in Path(self.env['COMMAND_TRACE']).read_text().splitlines()]
        self.assertEqual([command[0] for command in commands], ['inventory', 'list', 'archive'])
        self.assertEqual(commands[2][1:], ['--all-targets', '--locked', '--features', 'test-seam',
                                          '--archive-file', str(self.directory / 'nextest.tar.zst')])
        outputs = dict(line.split('=', 1) for line in self.output.read_text().splitlines())
        self.assertEqual(outputs['source_sha'], self.sha)
        self.assertEqual(outputs['manifest_sha256'], expected['manifest_sha256'])

    def test_selected_scope_and_old_base_conservative_full_fallback(self):
        self.builder_fixture()
        self.plan.write_text(json.dumps({'schema': 1, 'mode': 'selected', 'targets': ['board'], 'reason': 'selected fixture'}))
        result = self.prepare()
        self.assertEqual(result.returncode, 0, result.stderr)
        commands = [json.loads(line) for line in Path(self.env['COMMAND_TRACE']).read_text().splitlines()]
        self.assertEqual(commands[-1][1:-2], ['--lib', '--bins', '--test', 'board', '--locked', '--features', 'test-seam'])
        self.runner.write_text(self.runner.read_text().replace('ARCHIVE_PROTOCOL = 1', '# older runner'))
        self.directory = Path(self.temp.name) / 'fallback-bundle'
        result = self.prepare()
        self.assertEqual(result.returncode, 0, result.stderr)
        recorded = json.loads((self.directory / 'ci-test-plan.json').read_text())
        self.assertEqual(recorded['mode'], 'full')
        self.assertIn('base runner lacks archive protocol', recorded['reason'])
        self.assertEqual(recorded['original_plan_sha256'], hashlib.sha256(self.plan.read_bytes()).hexdigest())

    def test_large_changed_path_plans_survive_the_plan_budget(self):
        # ci-test-plan.make_plan records every {status, path} entry in the
        # diff, including on full-mode plans. ~1500 realistic paths exceed
        # the 64 KiB descriptor budget; the plan payload keeps its own
        # bound and must be retained verbatim through prepare and verify.
        self.builder_fixture()
        changes = [{'status': 'M', 'path': f'tests/generated/case_{i:05d}.rs'}
                   for i in range(1500)]
        for mode, targets in (('full', []), ('docs', []),
                              ('selected', ['board'])):
            with self.subTest(mode=mode):
                plan_doc = {'schema': 1, 'mode': mode, 'targets': targets,
                            'reason': mode + ' fixture', 'base_sha': self.sha,
                            'head_sha': self.sha, 'changes': changes}
                self.plan.write_text(json.dumps(plan_doc))
                self.assertGreater(len(self.plan.read_bytes()), bundle.MAX_MANIFEST_BYTES)
                directory = Path(self.temp.name) / ('bundle-' + mode)
                result = self.invoke('prepare', '--root', str(self.root), '--plan', str(self.plan),
                                     '--inventory-runner', str(self.runner), '--directory', str(directory))
                self.assertEqual(result.returncode, 0, result.stderr)
                recorded = json.loads((directory / 'ci-test-plan.json').read_text())
                self.assertEqual(recorded['changes'], changes)
                self.assertEqual(recorded['mode'], mode)
                self.assertEqual(recorded['targets'], targets)
                manifest = json.loads((directory / 'bundle.json').read_text())
                expected = dict(manifest, manifest_sha256=hashlib.sha256(
                    (directory / 'bundle.json').read_bytes()).hexdigest())
                self.assertEqual(bundle.verify_bundle(directory, expected),
                                 directory / 'nextest.tar.zst')
                outputs = dict(line.split('=', 1) for line in self.output.read_text().splitlines())
                self.assertEqual(outputs['mode'], mode)
                self.assertEqual(outputs['plan_sha256'], hashlib.sha256(
                    (directory / 'ci-test-plan.json').read_bytes()).hexdigest())
                self.output.unlink()

    def test_overbudget_plan_refuses_before_nextest_or_publication(self):
        self.builder_fixture()
        pad = 'x' * (17 * 1024 * 1024)
        self.plan.write_text('{"schema": 1, "mode": "full", "targets": [], "pad": "' + pad + '"}')
        self.assertGreater(len(self.plan.read_bytes()), bundle.MAX_PLAN_BYTES)
        result = self.prepare()
        self.assertEqual(result.returncode, 1)
        self.assertIn('large', result.stderr)
        self.assertFalse(self.directory.exists())
        self.assertFalse(self.output.exists())
        trace = Path(self.env['COMMAND_TRACE'])
        self.assertFalse(trace.exists() and trace.read_text().strip())

    def test_explicit_docs_mode_seals_zero_build_markers(self):
        self.builder_fixture()
        self.plan.write_text(json.dumps({'schema': 1, 'mode': 'docs', 'targets': [], 'reason': 'explicit permitted docs'}))
        result = self.prepare()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.directory / 'nextest.tar.zst').read_bytes(), b'')
        self.assertEqual(json.loads((self.directory / 'inventory.json').read_text()), {'rust-suites': {}, 'test-count': 0})
        self.assertFalse(Path(self.env['COMMAND_TRACE']).exists())
        self.assertIn('mode=docs', self.output.read_text())

    def test_failed_inventory_stops_archive_and_publishes_no_success(self):
        self.builder_fixture()
        self.env['FAIL_INVENTORY'] = '1'
        result = self.prepare()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.directory / 'bundle.json').exists())
        self.assertFalse(self.output.exists())
        commands = [json.loads(line) for line in Path(self.env['COMMAND_TRACE']).read_text().splitlines()]
        self.assertEqual([command[0] for command in commands], ['inventory'])

    def test_expect_uses_local_git_tools_and_retains_old_producer_attempt(self):
        reference = Path(self.temp.name) / 'reference.json'
        reference.write_text(json.dumps({'source_sha': self.sha, 'run_id': '12345', 'producer_attempt': '1',
                                        'artifact_id': '67890', 'mode': 'full', 'archive_sha256': 'a' * 64,
                                        'plan_sha256': 'b' * 64, 'inventory_sha256': 'c' * 64,
                                        'manifest_sha256': 'd' * 64}))
        self.env['GITHUB_RUN_ATTEMPT'] = '2'
        expected = Path(self.temp.name) / 'expected.json'
        result = self.invoke('expect', '--root', str(self.root), '--reference', str(reference), '--out', str(expected))
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads(expected.read_text())
        self.assertEqual(doc['source_sha'], self.sha)
        self.assertEqual(doc['producer_attempt'], '1')
        self.assertEqual(doc['build']['rustc_version'], 'rustc 1.98.1\nhost: x86_64-unknown-linux-gnu')
        self.assertEqual(doc['build']['target_root'], str(self.root / 'target'))
        self.assertEqual(doc['build']['config_sha256']['Cargo.lock'], hashlib.sha256((self.root / 'Cargo.lock').read_bytes()).hexdigest())

    def test_expect_refuses_forged_checkout_or_ambient_build_overrides(self):
        reference = Path(self.temp.name) / 'reference.json'
        reference.write_text(json.dumps({'source_sha': self.sha, 'run_id': '12345', 'producer_attempt': '1',
                                        'artifact_id': '67890', 'mode': 'full', 'archive_sha256': 'a' * 64,
                                        'plan_sha256': 'b' * 64, 'inventory_sha256': 'c' * 64,
                                        'manifest_sha256': 'd' * 64}))
        for key, value in (('GITHUB_SHA', 'e' * 40), ('CARGO_ENCODED_RUSTFLAGS', '-Copt-level=3'),
                           ('CARGO_BUILD_TARGET', 'aarch64-unknown-linux-gnu'), ('RUSTC_WRAPPER', '/unproven/wrapper')):
            with self.subTest(key=key):
                original = self.env.get(key)
                self.env[key] = value
                output = Path(self.temp.name) / ('expected-' + key + '.json')
                result = self.invoke('expect', '--root', str(self.root), '--reference', str(reference), '--out', str(output))
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(output.exists())
                if original is None:
                    self.env.pop(key)
                else:
                    self.env[key] = original


if __name__ == '__main__':
    unittest.main()
