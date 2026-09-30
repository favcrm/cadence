#!/usr/bin/env python3
"""Producer-to-shard provenance contracts; synthetic payloads, no Rust builds."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('ci_nextest_bundle', ROOT / 'scripts/ci-nextest-bundle.py')
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class BundleContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='nextest-bundle.')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'nextest.tar.zst').write_bytes(b'synthetic archive, not executable nextest data')
        (self.root / 'ci-test-plan.json').write_text(json.dumps({'schema': 1, 'mode': 'full', 'targets': []}))
        (self.root / 'inventory.json').write_bytes(b'{"synthetic": true}\n')
        self.expected = {
            'schema': 1, 'source_sha': 'a' * 40, 'run_id': '12345', 'producer_attempt': '1',
            'nextest_version': '0.9.145', 'features': ['test-seam'],
            'workspace_root': '/workspace/cadence',
            'build': {
                'target_root': '/workspace/cadence/target', 'profile': 'test',
                'rustc_version': 'rustc 1.98.1\nhost: x86_64-unknown-linux-gnu',
                'cargo_version': 'cargo 1.98.1', 'target_triple': 'x86_64-unknown-linux-gnu',
                'rustflags': '-D warnings', 'runner_os': 'Linux', 'runner_arch': 'X64',
                'image_os': 'ubuntu24', 'image_version': '20260920.314.1',
                'nextest_sha256': 'b' * 64,
                'config_sha256': {name: 'c' * 64 for name in bundle.CONFIG_FILES},
            },
            'plan_sha256': digest(self.root / 'ci-test-plan.json'),
            'inventory_sha256': digest(self.root / 'inventory.json'),
            'archive_sha256': digest(self.root / 'nextest.tar.zst'),
        }
        self.manifest = json.loads(json.dumps(self.expected))
        self.store_manifest()

    def store_manifest(self):
        path = self.root / 'bundle.json'
        path.write_text(json.dumps(self.manifest) + '\n')
        # Simulates separately supplied trusted producer output, NOT authority
        # discovered from downloaded bytes. No real build attestation is claimed.
        self.expected['manifest_sha256'] = digest(path)

    def test_accepts_exact_independently_bound_payload(self):
        self.assertEqual(bundle.verify_bundle(self.root, self.expected), self.root / 'nextest.tar.zst')

    def test_rejects_source_run_attempt_pin_feature_or_workspace_mismatch(self):
        for field, value in (
            ('source_sha', 'd' * 40), ('run_id', '98765'), ('producer_attempt', '2'),
            ('nextest_version', '0.9.144'), ('features', ['default']), ('workspace_root', '/other/workspace'),
        ):
            with self.subTest(field=field):
                original = self.manifest[field]
                self.manifest[field] = value
                self.store_manifest()
                try:
                    with self.assertRaises(ValueError):
                        bundle.verify_bundle(self.root, self.expected)
                finally:
                    self.manifest[field] = original
                    self.store_manifest()

    def test_build_context_drift_refuses(self):
        changes = {'target_root': '/other/target', 'profile': 'release',
                   'rustc_version': 'rustc 1.99.0', 'cargo_version': 'cargo 1.99.0',
                   'target_triple': 'aarch64-unknown-linux-gnu', 'rustflags': '',
                   'runner_os': 'macOS', 'runner_arch': 'ARM64', 'image_os': 'ubuntu22',
                   'image_version': 'different', 'nextest_sha256': 'd' * 64,
                   'config_sha256': {name: 'e' * 64 for name in bundle.CONFIG_FILES}}
        for field, value in changes.items():
            with self.subTest(field=field):
                original = self.manifest['build'][field]
                self.manifest['build'][field] = value
                self.store_manifest()
                try:
                    with self.assertRaises(ValueError):
                        bundle.verify_bundle(self.root, self.expected)
                finally:
                    self.manifest['build'][field] = original
                    self.store_manifest()

    def test_corrupted_payloads_refuse(self):
        for name in ('nextest.tar.zst', 'ci-test-plan.json', 'inventory.json', 'bundle.json'):
            with self.subTest(file=name):
                path = self.root / name
                original = path.read_bytes()
                path.write_bytes(original + b'tampered')
                try:
                    with self.assertRaises(ValueError):
                        bundle.verify_bundle(self.root, self.expected)
                finally:
                    path.write_bytes(original)

    def test_missing_independent_authority_refuses(self):
        for field in ('source_sha', 'run_id', 'producer_attempt', 'manifest_sha256', 'build', 'inventory_sha256'):
            with self.subTest(field=field), self.assertRaises(ValueError):
                bundle.verify_bundle(self.root, {key: value for key, value in self.expected.items() if key != field})

    def test_unknown_duplicate_or_boolean_schema_refuses(self):
        mutations = ({'schema': True}, {'allow_missing_archive': True}, {'schema': 2})
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                original = dict(self.manifest)
                self.manifest.update(mutation)
                self.store_manifest()
                try:
                    with self.assertRaises(ValueError):
                        bundle.verify_bundle(self.root, self.expected)
                finally:
                    self.manifest = original
                    self.store_manifest()
        raw = ('{"source_sha":"' + 'd' * 40 + '",' + json.dumps(self.manifest)[1:]).encode()
        (self.root / 'bundle.json').write_bytes(raw)
        self.expected['manifest_sha256'] = digest(self.root / 'bundle.json')
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            bundle.verify_bundle(self.root, self.expected)

    def test_symlink_and_nonregular_payloads_refuse(self):
        for name in ('bundle.json', 'nextest.tar.zst', 'ci-test-plan.json', 'inventory.json'):
            with self.subTest(file=name):
                path = self.root / name
                moved = self.root / ('original-' + name)
                path.rename(moved)
                path.symlink_to(moved)
                try:
                    with self.assertRaisesRegex(ValueError, 'regular'):
                        bundle.verify_bundle(self.root, self.expected)
                finally:
                    path.unlink()
                    moved.rename(path)
        archive = self.root / 'nextest.tar.zst'
        archive.unlink()
        archive.mkdir()
        with self.assertRaisesRegex(ValueError, 'regular'):
            bundle.verify_bundle(self.root, self.expected)

    def test_oversized_descriptor_refuses(self):
        path = self.root / 'bundle.json'
        path.write_bytes(path.read_bytes() + b' ' * 65537)
        self.expected['manifest_sha256'] = digest(path)
        with self.assertRaisesRegex(ValueError, 'large'):
            bundle.verify_bundle(self.root, self.expected)

    def test_even_mutually_equal_invalid_authority_cannot_pass(self):
        for field, value in (('source_sha', 'main'), ('run_id', '0'), ('producer_attempt', True),
                             ('schema', True), ('workspace_root', '/workspace/../cadence'),
                             ('features', ['test-seam', 'extra'])):
            with self.subTest(field=field):
                original = self.manifest[field]
                self.manifest[field] = value
                self.expected[field] = value
                self.store_manifest()
                try:
                    with self.assertRaises(ValueError):
                        bundle.verify_bundle(self.root, self.expected)
                finally:
                    self.manifest[field] = original
                    self.expected[field] = original
                    self.store_manifest()

    def artifact_inputs(self, consumer_attempt='2'):
        context = {'source_sha': self.expected['source_sha'], 'run_id': self.expected['run_id'],
                   'consumer_attempt': consumer_attempt}
        outputs = {key: self.expected[key] for key in (
            'source_sha', 'run_id', 'producer_attempt', 'manifest_sha256',
            'archive_sha256', 'plan_sha256', 'inventory_sha256')}
        outputs['artifact_id'] = '67890'
        outputs['mode'] = 'full'
        return context, {'result': 'success', 'outputs': outputs}

    def test_failed_only_rerun_retains_exact_original_producer_id_and_attempt(self):
        context, producer = self.artifact_inputs()
        selected = bundle.select_artifact(context, producer)
        self.assertEqual(selected['artifact_id'], '67890')
        self.assertEqual(selected['producer_attempt'], '1')

    def test_complete_producer_rerun_can_supply_a_new_exact_id(self):
        context, producer = self.artifact_inputs()
        producer['outputs'].update(producer_attempt='2', artifact_id='67891')
        self.assertEqual(bundle.select_artifact(context, producer)['artifact_id'], '67891')

    def test_failed_skipped_missing_or_future_producer_refuses(self):
        for result in ('failure', 'skipped', 'cancelled', None, True):
            context, producer = self.artifact_inputs()
            producer['result'] = result
            with self.subTest(result=result), self.assertRaises(ValueError):
                bundle.select_artifact(context, producer)
        context, producer = self.artifact_inputs()
        producer['outputs']['producer_attempt'] = '3'
        with self.assertRaises(ValueError):
            bundle.select_artifact(context, producer)
        with self.assertRaises(ValueError):
            bundle.select_artifact(context, {})
        with self.assertRaises(ValueError):
            bundle.select_artifact(context, None)

    def test_cross_source_run_missing_and_malformed_outputs_refuse(self):
        for field, value in (
            ('source_sha', 'd' * 40), ('run_id', '54321'), ('artifact_id', 'latest'),
            ('artifact_id', '0'), ('producer_attempt', True), ('archive_sha256', 'broken'),
            ('inventory_sha256', None), ('manifest_sha256', 'A' * 64),
        ):
            context, producer = self.artifact_inputs()
            producer['outputs'][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                bundle.select_artifact(context, producer)
        context, producer = self.artifact_inputs()
        producer['outputs'].pop('artifact_id')
        with self.assertRaises(ValueError):
            bundle.select_artifact(context, producer)

    def test_unknown_fields_and_malformed_consumer_context_refuse(self):
        context, producer = self.artifact_inputs()
        producer['outputs']['artifact_name'] = 'latest'
        with self.assertRaises(ValueError):
            bundle.select_artifact(context, producer)
        for key, value in (('consumer_attempt', True), ('source_sha', 'main'),
                           ('run_id', '0'), ('allow_cross_run', True)):
            context, producer = self.artifact_inputs()
            context[key] = value
            with self.subTest(field=key), self.assertRaises(ValueError):
                bundle.select_artifact(context, producer)

    def test_cli_without_required_operation_does_not_succeed(self):
        result = subprocess.run([sys.executable, str(ROOT / 'scripts/ci-nextest-bundle.py')], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)

    def test_verify_cli_uses_external_authority_and_preserves_tamper_failure(self):
        with tempfile.TemporaryDirectory(prefix='nextest-authority.') as temporary:
            authority = Path(temporary) / 'expected.json'
            authority.write_text(json.dumps(self.expected))
            argv = [sys.executable, str(ROOT / 'scripts/ci-nextest-bundle.py'), 'verify',
                    '--directory', str(self.root), '--expected', str(authority)]
            passed = subprocess.run(argv, capture_output=True, text=True)
            self.assertEqual(passed.returncode, 0, passed.stderr)
            self.assertTrue(json.loads(passed.stdout)['verified'])
            (self.root / 'nextest.tar.zst').write_bytes(b'tampered')
            failed = subprocess.run(argv, capture_output=True, text=True)
            self.assertEqual(failed.returncode, 1)
            self.assertIn('archive_sha256 mismatch', failed.stderr)

    def test_verify_cli_rejects_authority_file_inside_downloaded_bundle(self):
        authority = self.root / 'expected.json'
        authority.write_text(json.dumps(self.expected))
        result = subprocess.run([sys.executable, str(ROOT / 'scripts/ci-nextest-bundle.py'), 'verify',
                                 '--directory', str(self.root), '--expected', str(authority)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn('independent', result.stderr)

    def test_build_record_requires_known_complete_config_set(self):
        for mutation in ('missing', 'unknown', 'bad-digest'):
            with self.subTest(mutation=mutation):
                configs = dict(self.manifest['build']['config_sha256'])
                key = next(iter(configs))
                if mutation == 'missing':
                    configs.pop(key)
                elif mutation == 'unknown':
                    configs['allow-retries.toml'] = 'a' * 64
                else:
                    configs[key] = True
                record = dict(self.expected['build'], config_sha256=configs)
                original = self.manifest['build']
                self.manifest['build'] = record
                self.expected['build'] = record
                self.store_manifest()
                try:
                    with self.assertRaises(ValueError):
                        bundle.verify_bundle(self.root, self.expected)
                finally:
                    self.manifest['build'] = original
                    self.expected['build'] = json.loads(json.dumps(original))
                    self.store_manifest()


if __name__ == '__main__':
    unittest.main()
