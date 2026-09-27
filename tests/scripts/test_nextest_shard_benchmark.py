#!/usr/bin/env python3
"""Fail-closed archive experiment contracts; never invokes Rust."""
import copy
import importlib.util
import json
import os
import shutil
import subprocess
from unittest.mock import patch
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('benchmark', ROOT / 'scripts/nextest-shard-benchmark.py')
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)


def listing(part=None):
    suites = {}
    for binary in ('pkg::one', 'pkg::two'):
        cases = {}
        for name in ('same_name', 'other', 'ignored'):
            ignored = name == 'ignored'
            reason = 'ignored' if ignored else 'partition'
            selected = not ignored and (part is None or (name == 'same_name') == (part == 1))
            cases[name] = {'ignored': ignored, 'filter-match': {'status': 'matches'} if selected else
                           {'status': 'mismatch', 'reason': reason}}
        suites[binary] = {'binary-id': binary, 'status': 'listed', 'testcases': cases}
    return {'test-count': 6, 'rust-suites': suites}


def report(part=None):
    rows = []
    for binary, suite in listing(part)['rust-suites'].items():
        cases = []
        for name, case in suite['testcases'].items():
            skip = '<skipped/>' if case['filter-match']['status'] == 'mismatch' else ''
            cases.append(f'<testcase classname="{binary}" name="{name}" time="0.1">{skip}</testcase>')
        skipped = 1 if part is None else 2
        rows.append(f'<testsuite name="{binary}" tests="3" failures="0" errors="0" skipped="{skipped}">' + ''.join(cases) + '</testsuite>')
    return '<testsuites tests="6" failures="0" errors="0" skipped="' + str(2 if part is None else 4) + '">' + ''.join(rows) + '</testsuites>'


class Coverage(unittest.TestCase):
    def test_binary_qualified_partition_union_with_peer_skips(self):
        full = M.inventory(listing())
        parts = [M.inventory(listing(i), partition=True) for i in (1, 2)]
        M.partitions(full, parts)
        self.assertEqual(len(full), 6)
        self.assertEqual(len(M.junit(report(1), parts[0])['executed']), 2)
        self.assertEqual(len(M.junit(report(), full)['executed']), 4)

    def test_inventory_unknown_status_reason_ignored_and_count(self):
        mutations = [('status', 'skipped'), ('reason', 'string'), ('ignored', False), ('count', 5)]
        for key, value in mutations:
            data = listing()
            if key == 'status':
                data['rust-suites']['pkg::one']['status'] = value
            elif key == 'count':
                data['test-count'] = value
            else:
                case = data['rust-suites']['pkg::one']['testcases']['ignored']
                if key == 'reason':
                    case['filter-match']['reason'] = value
                else:
                    case[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                M.inventory(data)

    def test_partition_duplicate_missing_wrong_ignored_and_empty(self):
        full = M.inventory(listing())
        good = [M.inventory(listing(i), partition=True) for i in (1, 2)]
        for change in ('duplicate', 'missing', 'ignored', 'empty'):
            parts = copy.deepcopy(good)
            if change == 'duplicate':
                parts[1] = parts[0]
            elif change == 'missing':
                parts[0].pop(('pkg::one', 'same_name'))
            elif change == 'ignored':
                parts[0][('pkg::one', 'ignored')]['ignored'] = False
            else:
                for case in parts[0].values():
                    case['selected'] = False
            with self.subTest(change=change), self.assertRaises(ValueError):
                M.partitions(full, parts)

    def test_junit_rejects_missing_duplicate_foreign_class_and_selected_skip(self):
        original = report(1)
        changes = [original.replace('name="same_name"', 'name="foreign"', 1),
                   original.replace('name="other"', 'name="same_name"', 1),
                   original.replace('classname="pkg::one"', 'classname="pkg::foreign"', 1),
                   original.replace('name="same_name" time="0.1"></testcase>', 'name="same_name" time="0.1"><skipped/></testcase>', 1),
                   original.replace('name="other" time="0.1"><skipped/>', 'name="other" time="0.1">', 1)]
        for xml in changes:
            with self.subTest(xml=xml), self.assertRaises(ValueError):
                M.junit(xml, M.inventory(listing(1), partition=True))

    def test_junit_counts_failures_errors_retries_malformed(self):
        original = report()
        for xml in [original.replace('tests="6"', 'tests="0"'),
                    original.replace('failures="0"', 'failures="-1"', 1),
                    original.replace('skipped="2"', 'skipped="x"', 1),
                    original.replace('</testcase>', '<failure/></testcase>', 1),
                    original.replace('</testcase>', '<error/></testcase>', 1),
                    original.replace('</testcase>', '<rerunFailure/></testcase>', 1),
                    original.replace('</testcase>', '<flakyFailure/></testcase>', 1),
                    original.replace('<skipped/>', '<skipped/><skipped/>', 1), '<invalid>']:
            with self.subTest(xml=xml), self.assertRaises(ValueError):
                M.junit(xml, M.inventory(listing()))

    def test_nested_status_nodes_never_hide_in_allowed_xml_subtrees(self):
        original = report()
        for status in ('failure', 'error', 'rerunFailure', 'flakyFailure'):
            for parent in ('properties', 'system-out', 'skipped'):
                nested = f'<{parent}><{status}/></{parent}>'
                xml = original.replace('</testcase>', nested + '</testcase>', 1)
                with self.subTest(status=status, parent=parent), self.assertRaises(ValueError):
                    M.junit(xml, M.inventory(listing()))
        valid = original.replace('</testcase>', '<properties><property name="result" value="okay"/></properties></testcase>', 1)
        self.assertEqual(len(M.junit(valid, M.inventory(listing()))['executed']), 4)

    def test_optional_counts_and_literal_peer_skip_message(self):
        xml = report(1).replace(' tests="6"', '').replace('<skipped/>', '<skipped message="Skipped: test is in a different partition"/>')
        self.assertEqual(len(M.junit(xml, M.inventory(listing(1), partition=True))['skipped']), 4)

    def test_duplicate_json_keys_fail(self):
        with self.assertRaises(ValueError):
            M.loads('{"rust-suites":{},"rust-suites":{}}')

    def test_missing_stale_symlink_reports_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'report.xml'
            with self.assertRaises(ValueError):
                M.fresh_report(path, 0)
            path.write_text(report())
            with self.assertRaises(ValueError):
                M.fresh_report(path, path.stat().st_mtime_ns + 1)
            link = Path(temp) / 'link.xml'
            link.symlink_to(path)
            with self.assertRaises(ValueError):
                M.fresh_report(link, 0)

    def test_execution_commands_reuse_metadata_without_cargo_build_flags(self):
        args = M.reuse_args(Path('/same/workspace'))
        self.assertIn('--binaries-metadata', args)
        self.assertIn('--cargo-metadata', args)
        self.assertNotIn('--features', args)
        self.assertNotIn('--all-targets', args)
        self.assertNotIn('--locked', args)


class FakeExecution(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='cad689-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'checkout'
        self.root.mkdir()
        (self.root / 'scripts').mkdir()
        self.sha = 'a' * 40
        self.context = {'source': self.sha, 'run': '17', 'attempt': '1', 'workspace': str(self.root)}
        for part in (None, 1, 2):
            data = listing(part)
            for suite in data['rust-suites'].values():
                suite.update({'cwd': str(self.root), 'binary-path': str(self.root / 'target/debug/cadence')})
            (self.root / f'list-{part}.json').write_text(json.dumps(data))
            (self.root / f'report-{part}.xml').write_text(report(part))
        default = '<testsuite name="pkg::test_seam" tests="2" failures="0" skipped="0">' + ''.join(
            f'<testcase classname="pkg::test_seam" name="{name}"/>' for name in
            ('without_the_feature_arm_is_refused', 'without_the_feature_the_field_is_refused')) + '</testsuite>'
        (self.root / 'default.xml').write_text(default)
        self.fake('scripts/nextest-inventory', """import sys
assert sys.argv[1:] == ['all-targets', '--features', 'test-seam'], sys.argv
print('cargo=6 nextest=6 equal=yes')
""")
        self.fake('cargo', """import sys
assert sys.argv[1:] == ['test', '--doc', '--locked'], sys.argv
print('doctests passed')
""")
        self.fake('scripts/cadence-nextest', r"""import json, pathlib, sys
root = pathlib.Path.cwd()
args = sys.argv[1:]
with (root / 'commands.jsonl').open('a') as output:
    output.write(json.dumps(args) + '\n')
target = root / 'target/debug'
target.mkdir(parents=True, exist_ok=True)
binary = target / 'cadence'
binary.write_text('#!/bin/sh\necho cadence 0.1+' + 'a' * 40 + '\n')
binary.chmod(0o755)
if args[0] == 'archive':
    pathlib.Path(args[args.index('--archive-file') + 1]).write_bytes(b'fake archive bytes')
elif args[0] == 'list':
    part = int(args[args.index('--partition') + 1].split(':')[1].split('/')[0]) if '--partition' in args else None
    print((root / f'list-{part}.json').read_text())
else:
    part = int(args[args.index('--partition') + 1].split(':')[1].split('/')[0]) if '--partition' in args else None
    report_path = root / 'target/nextest/cadence/junit.xml'
    report_path.parent.mkdir(parents=True, exist_ok=True)
    source = root / ('default.xml' if '--test' in args else f'report-{part}.xml')
    report_path.write_text(source.read_text())
""")
        self.env = patch.dict(os.environ, {'PATH': str(self.root) + os.pathsep + os.environ['PATH'],
            'GITHUB_OUTPUT': str(self.root / 'outputs'), 'GITHUB_SHA': self.sha, 'GITHUB_RUN_ID': '17'})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.mock_context = patch.object(M, 'context', return_value=self.context)
        self.mock_context.start()
        self.addCleanup(self.mock_context.stop)

    def fake(self, name, body):
        path = self.root / name
        path.write_text('#!/usr/bin/env python3\n' + body)
        path.chmod(0o755)

    def pipelines(self):
        directory = Path(self.temp.name) / 'artifacts'
        for role in ('baseline', 'prepare', 'shard-1', 'shard-2'):
            if (self.root / 'target').exists():
                shutil.rmtree(self.root / 'target')
            prepared = directory / 'measurement-prepare'
            if role.startswith('shard'):
                os.environ['EXPECTED_ARCHIVE_SHA'] = M.loads((prepared / 'receipt.json').read_text())['archive']['sha256']
            M.run(self.root, directory / ('measurement-' + role), role, prepared)
        return directory

    def test_fake_pipelines_execute_correct_commands_and_aggregate(self):
        directory = self.pipelines()
        needs = {role: {'result': 'success'} for role in ('baseline', 'prepare', 'shards')}
        needs['prepare']['outputs'] = {'archive_sha': os.environ['EXPECTED_ARCHIVE_SHA']}
        self.assertEqual(M.aggregate(directory, needs)['executed'], 4)
        commands = [json.loads(line) for line in (self.root / 'commands.jsonl').read_text().splitlines()]
        archive_index = next(i for i, argv in enumerate(commands) if argv[0] == 'archive')
        refusal_indexes = [i for i, argv in enumerate(commands) if '--test' in argv]
        self.assertLess(archive_index, refusal_indexes[-1])
        shard_runs = [argv for argv in commands if '--partition' in argv and argv[0] != 'list']
        self.assertEqual(len(shard_runs), 2)
        self.assertTrue(all('--binaries-metadata' in argv and '--features' not in argv for argv in shard_runs))
        self.assertEqual({argv[argv.index('--partition') + 1] for argv in shard_runs}, {'hash:1/2', 'hash:2/2'})

    def test_aggregate_rejects_failed_skipped_missing_jobs_and_artifact_mutation(self):
        directory = self.pipelines()
        good = {role: {'result': 'success'} for role in ('baseline', 'prepare', 'shards')}
        good['prepare']['outputs'] = {'archive_sha': os.environ['EXPECTED_ARCHIVE_SHA']}
        for status in ('failure', 'skipped', 'cancelled'):
            needs = copy.deepcopy(good)
            needs['shards']['result'] = status
            with self.subTest(status=status), self.assertRaises(ValueError):
                M.aggregate(directory, needs)
        needs = copy.deepcopy(good)
        del needs['prepare']
        with self.assertRaises(ValueError):
            M.aggregate(directory, needs)
        path = directory / 'measurement-shard-2/shard.xml'
        path.write_text(report(1))
        with self.assertRaises(ValueError):
            M.aggregate(directory, good)

    def test_aggregate_rejects_resealed_context_archive_role_and_coverage_drift(self):
        directory = self.pipelines()
        needs = {role: {'result': 'success'} for role in ('baseline', 'prepare', 'shards')}
        needs['prepare']['outputs'] = {'archive_sha': os.environ['EXPECTED_ARCHIVE_SHA']}
        receipt_path = directory / 'measurement-shard-2/receipt.json'
        original = receipt_path.read_text()
        for change in ('source', 'profile', 'archive', 'role', 'phase'):
            receipt = json.loads(original)
            if change == 'source':
                receipt['context']['source'] = 'b' * 40
            elif change == 'profile':
                receipt['context']['config'] = {'profile': 'foreign'}
            elif change == 'archive':
                receipt['archive']['sha256'] = 'b' * 64
            elif change == 'role':
                receipt['role'] = 'shard-1'
            else:
                receipt['phases'].pop()
            receipt_path.write_text(json.dumps(receipt))
            with self.subTest(change=change), self.assertRaises(ValueError):
                M.aggregate(directory, needs)
        receipt_path.write_text(original)
        # Even self-consistent file hashes cannot make wrong-shard execution valid.
        xml_path = directory / 'measurement-shard-2/shard.xml'
        xml_path.write_text(report(1))
        receipt = json.loads(original)
        receipt['files']['shard.xml'] = M.digest(xml_path)
        receipt_path.write_text(json.dumps(receipt))
        with self.assertRaises(ValueError):
            M.aggregate(directory, needs)

    def test_required_artifact_ledgers_cannot_be_empty_or_omit_consumed_paths(self):
        directory = self.pipelines()
        needs = {role: {'result': 'success'} for role in ('baseline', 'prepare', 'shards')}
        needs['prepare']['outputs'] = {'archive_sha': os.environ['EXPECTED_ARCHIVE_SHA']}
        roles = ('baseline', 'prepare', 'shard-1', 'shard-2')
        for role in roles:
            path = directory / f'measurement-{role}/receipt.json'
            original = path.read_text()
            for change in ('empty', 'missing', 'malformed'):
                receipt = json.loads(original)
                if change == 'empty':
                    receipt['files'] = {}
                elif change == 'missing':
                    del receipt['files']['full.json']
                else:
                    receipt['files']['full.json'] = 'invalid digest'
                path.write_text(json.dumps(receipt))
                with self.subTest(role=role, change=change), self.assertRaises(ValueError):
                    M.aggregate(directory, needs)
                path.write_text(original)

    def test_subprocess_failure_is_preserved_and_output_cannot_be_reused(self):
        self.fake('scripts/nextest-inventory', 'import sys\nprint("original failure")\nsys.exit(23)\n')
        out = Path(self.temp.name) / 'failure'
        with self.assertRaises(ValueError):
            M.run(self.root, out, 'baseline')
        receipt = M.loads((out / 'receipt.json').read_text())
        self.assertEqual(receipt['status'], 'failed')
        self.assertEqual(receipt['phases'][0]['exit_code'], 23)
        self.assertIn('original failure', (out / 'inventory.log').read_text())
        with self.assertRaises(ValueError):
            M.run(self.root, out, 'baseline')

    def test_wrapper_archive_retains_subcommand_through_real_flock(self):
        (self.root / '.config').mkdir()
        wrapper = self.root / 'scripts/cadence-nextest'
        shutil.copyfile(ROOT / 'scripts/cadence-nextest', wrapper)
        wrapper.chmod(0o755)
        (self.root / '.config/nextest.toml').write_text('[profile.cadence]\nretries=0\n')
        self.fake('cargo-nextest', """import json, os, pathlib, sys
if sys.argv[1:] == ['--version']:
    print('cargo-nextest 0.9.145')
else:
    pathlib.Path('wrapped.json').write_text(json.dumps({'argv': sys.argv[1:], 'lock': os.environ.get('CADENCE_SUITE_LOCK'), 'held': os.environ.get('CADENCE_REVIEW_SUITE_LOCK_HELD'), 'profile': os.environ.get('NEXTEST_PROFILE'), 'retries': os.environ.get('NEXTEST_RETRIES')}))
""")
        binary = self.root / 'cargo-nextest'
        (self.root / '.config/cargo-nextest.sha256').write_text(M.digest(binary) + '  cargo-nextest\n')
        env = os.environ.copy()
        env.update({'CADENCE_NEXTTEST_BIN': str(binary), 'CADENCE_SUITE_LOCK': str(self.root / 'suite.lock'), 'NEXTEST_PROFILE': 'foreign', 'NEXTEST_RETRIES': '7'})
        for name in ('CADENCE_REVIEW_SUITE_LOCK_HELD', 'CADENCE_REVIEW_PR', 'CADENCE_REVIEW_HEAD'):
            env.pop(name, None)
        result = subprocess.run([str(wrapper), 'archive', '--archive-file', 'suite.tar.zst', '--locked'], cwd=self.root, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads((self.root / 'wrapped.json').read_text())
        self.assertEqual(record['argv'][:2], ['nextest', 'archive'])
        self.assertIn('--profile', record['argv'])
        self.assertNotIn('--retries', record['argv'])
        self.assertEqual(record['lock'], '')
        self.assertEqual(record['held'], '1')
        self.assertIsNone(record['profile'])
        self.assertIsNone(record['retries'])
        bad = subprocess.run([str(wrapper), 'archive', '--profile', 'other'], cwd=self.root, env=env, capture_output=True)
        self.assertNotEqual(bad.returncode, 0)


if __name__ == '__main__':
    unittest.main()
