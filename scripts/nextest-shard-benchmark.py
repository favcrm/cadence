#!/usr/bin/env python3
"""Opt-in, same-main-SHA archive measurement; never changes required CI policy."""
import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import xml.etree.ElementTree as ET


def require(condition, message):
    if not condition:
        raise ValueError(message)


def loads(text):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f'duplicate JSON key: {key}')
            result[key] = value
        return result
    return json.loads(text, object_pairs_hook=unique)


def read(path):
    require(path.is_file() and not path.is_symlink(), f'missing or linked file: {path}')
    return path.read_text()


def digest(path):
    require(path.is_file() and not path.is_symlink(), f'missing or linked file: {path}')
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def inventory(data, partition=False):
    result = {}
    for binary, suite in data['rust-suites'].items():
        require(binary and suite['binary-id'] == binary and suite.get('status') == 'listed', 'unlisted or inconsistent binary')
        for name, case in suite['testcases'].items():
            require(name and type(case['ignored']) is bool, 'invalid test identity/ignored flag')
            match = case['filter-match']
            selected = match == {'status': 'matches'}
            reason = match.get('reason')
            require(selected or (match.get('status') == 'mismatch' and
                    reason in (('ignored', 'partition') if partition else ('ignored',))), 'unexplained test exclusion')
            require(not selected or not case['ignored'], 'ignored test selected')
            require(reason != 'ignored' or case['ignored'], 'false ignored exclusion')
            result[(binary, name)] = {'ignored': case['ignored'], 'selected': selected, 'reason': reason}
    require(result and type(data['test-count']) is int and data['test-count'] == len(result), 'empty/inconsistent test count')
    return result


def selected(manifest):
    return Counter(key for key, case in manifest.items() if case['selected'])


def partitions(full, parts):
    require(len(parts) == 2, 'exactly two partitions required')
    for part in parts:
        require(part.keys() == full.keys() and all(part[k]['ignored'] == full[k]['ignored'] for k in full), 'partition inventory drift')
        require(selected(part), 'empty partition')
        require(all(not case['selected'] or full[key]['selected'] for key, case in part.items()), 'partition selected excluded test')
    require(selected(parts[0]) + selected(parts[1]) == selected(full), 'duplicate, missing or wrong partition coverage')


def junit(text, manifest):
    require('<!DOCTYPE' not in text and '<!ENTITY' not in text, 'DTD/entity JUnit')
    try:
        root = ET.fromstring(text)
    except ET.ParseError as error:
        raise ValueError('malformed JUnit') from error
    executed, skipped = Counter(), Counter()
    for node in root.iter():
        status = node.tag.lower()
        require(status not in ('failure', 'error') and not status.startswith(('rerun', 'flaky')), 'failure/error/retry in JUnit subtree')

    def auxiliary(node):
        if node.tag == 'properties':
            require(all(child.tag == 'property' and not list(child) for child in node), 'invalid properties structure')
        else:
            require(not list(node), 'nested status/output/skip structure')

    def counts(node, actual):
        for attr, value in zip(('tests', 'failures', 'errors', 'skipped', 'disabled'), actual):
            if attr in node.attrib:
                supplied = node.attrib[attr]
                require(re.fullmatch(r'[0-9]+', supplied) and int(supplied) == value, f'contradictory {attr} count')
        require(not any('retry' in attr_name.lower() or 'rerun' in attr_name.lower() for attr_name in node.attrib), 'retry metadata')

    def suite(node):
        require(node.tag in ('testsuite', 'testsuites'), 'invalid suite node')
        if node.tag == 'testsuite':
            require(node.get('name') in {identity[0] for identity in manifest}, 'foreign binary suite')
        total, skip_count = 0, 0
        for child in node:
            if child.tag in ('testsuite', 'testsuites'):
                require(node.tag == 'testsuites', 'nested binary suite')
                child_total, child_skip = suite(child)
                total += child_total
                skip_count += child_skip
            elif child.tag == 'testcase':
                require(node.tag == 'testsuite', 'case outside binary suite')
                binary, name = node.get('name'), child.get('name')
                identity = (binary, name)
                require(child.get('classname') == binary and identity in manifest and identity not in executed and identity not in skipped, 'unexpected or repeated testcase')
                require(all(c.tag in ('skipped', 'system-out', 'system-err', 'properties') for c in child), 'failure/error/retry or unknown testcase status')
                for auxiliary_node in child:
                    auxiliary(auxiliary_node)
                skips = child.findall('skipped')
                require(len(skips) <= 1 and bool(skips) != manifest[identity]['selected'], 'selected skip or peer execution')
                duration = float(child.get('time', '0'))
                require(math.isfinite(duration) and duration >= 0, 'invalid duration')
                (skipped if skips else executed)[identity] += 1
                total += 1
                skip_count += bool(skips)
            else:
                require(child.tag in ('properties', 'system-out', 'system-err'), 'unknown suite element')
                auxiliary(child)
        counts(node, (total, 0, 0, skip_count, 0))
        return total, skip_count

    suite(root)
    require(executed == selected(manifest), 'missing executed tests')
    require(executed + skipped == Counter(manifest.keys()), 'missing skipped/discovered tests')
    return {'executed': sorted(executed), 'skipped': sorted(skipped)}


def fresh_report(path, started_ns):
    text = read(path)
    require(path.stat().st_mtime_ns >= started_ns and text.strip(), 'stale or empty JUnit')
    return text


def reuse_args(root):
    return ['--cargo-metadata', str(root / 'target/nextest/cargo-metadata.json'),
            '--binaries-metadata', str(root / 'target/nextest/binaries-metadata.json'),
            '--workspace-remap', str(root), '--target-dir-remap', str(root / 'target')]


class Measurement:
    def __init__(self, root, out, role):
        self.root, self.out = root, out
        require(not out.exists(), 'measurement output already exists')
        out.mkdir(parents=True)
        self.receipt = {'role': role, 'status': 'failed', 'phases': [], 'files': {}}

    def command(self, name, argv, capture=None):
        started = time.time_ns()
        log = self.out / (name + '.log')
        output = self.out / capture if capture else log
        with log.open('wb') as errors, output.open('wb') if capture else log.open('ab') as stdout:
            result = subprocess.run(argv, cwd=self.root, stdout=stdout, stderr=errors if capture else subprocess.STDOUT, timeout=2400)
        self.receipt['phases'].append({'name': name, 'argv': argv, 'start_ns': started,
                                      'end_ns': time.time_ns(), 'exit_code': result.returncode})
        require(result.returncode == 0, f'{name} failed: {result.returncode}; see {log}')
        return read(output).strip()

    def test(self, name, argv, manifest=None):
        report = self.root / 'target/nextest/cadence/junit.xml'
        require(not report.is_symlink(), 'linked JUnit destination')
        if report.exists():
            report.unlink()
        started = time.time_ns()
        try:
            self.command(name, argv)
        finally:
            if report.is_file() and not report.is_symlink():
                shutil.copyfile(report, self.out / (name + '.xml'))
        text = fresh_report(report, started)
        if manifest is None:
            # Production feature shape has precisely these two real refusal proofs.
            xml = ET.fromstring(text)
            cases = xml.findall('.//testcase')
            names = {'without_the_feature_arm_is_refused', 'without_the_feature_the_field_is_refused'}
            require(len(cases) == 2 and {c.get('name') for c in cases} == names, 'default-feature refusal inventory changed')
            manifest = {(c.get('classname'), c.get('name')): {'selected': True} for c in cases}
        self.receipt[name] = junit(text, manifest)

    def finish(self):
        self.receipt['files'] = {p.name: digest(p) for p in self.out.iterdir() if p.is_file() and p.name != 'receipt.json'}
        (self.out / 'receipt.json').write_text(json.dumps(self.receipt, indent=2) + '\n')


def context(root):
    def output(argv):
        return subprocess.check_output(argv, cwd=root, text=True).strip()
    sha = os.environ['GITHUB_SHA']
    require(os.environ['GITHUB_REF'] == 'refs/heads/main' and re.fullmatch('[0-9a-f]{40}', sha), 'main dispatch only')
    require(os.environ['GITHUB_EVENT_NAME'] == 'workflow_dispatch' and os.environ['GITHUB_RUN_ATTEMPT'] == '1', 'fresh manual run only; no reruns')
    require(output(['git', 'rev-parse', 'HEAD']) == sha, 'checkout SHA mismatch')
    require(os.environ.get('CARGO_TARGET_DIR', str(root / 'target')) == str(root / 'target'), 'target layout override')
    require(os.environ.get('RUSTFLAGS') == '-D warnings' and os.environ.get('CARGO_BUILD_JOBS') == '4', 'build policy mismatch')
    return {'source': sha, 'run': os.environ['GITHUB_RUN_ID'], 'attempt': os.environ['GITHUB_RUN_ATTEMPT'],
            'workspace': str(root), 'target': str(root / 'target'),
            'rustc': output(['rustc', '--version', '--verbose']), 'cargo': output(['cargo', '--version']),
            'nextest': output([os.environ['CADENCE_NEXTTEST_BIN'], '--version']),
            'node': output(['node', '--version']), 'pnpm': output(['pnpm', '--version']),
            'runner': {k: os.environ.get(k, 'unset') for k in ('RUNNER_OS', 'RUNNER_ARCH', 'ImageOS', 'ImageVersion')},
            'environment': {k: os.environ.get(k, 'unset') for k in ('RUSTFLAGS', 'CARGO_BUILD_JOBS', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_INCREMENTAL')},
            'tool_sha': digest(Path(os.environ['CADENCE_NEXTTEST_BIN'])),
            'config': {p: digest(root / p) for p in ('Cargo.toml', 'Cargo.lock', '.config/nextest.toml', '.config/cargo-nextest.sha256', 'scripts/cadence-nextest', 'scripts/nextest-shard-benchmark.py')},
            'cache': 'fresh hosted target; no Rust cache action; dependency/tool downloads not claimed cold'}


def run(root, out, role, prepared=None):
    measurement = Measurement(root, out, role)
    wrapper = str(root / 'scripts/cadence-nextest')
    feature_args = ['--all-targets', '--locked', '--features', 'test-seam']
    try:
        measurement.receipt['context'] = context(root)
        require(not (root / 'target').exists(), 'target must be absent on a fresh runner')
        if role in ('baseline', 'prepare'):
            measurement.command('inventory', [str(root / 'scripts/nextest-inventory'), 'all-targets', '--features', 'test-seam'])
            if role == 'prepare':
                archive = out / 'suite.tar.zst'
                measurement.command('archive', [wrapper, 'archive', '--archive-file', str(archive)] + feature_args)
                measurement.receipt['archive'] = {'sha256': digest(archive), 'bytes': archive.stat().st_size}
                listing_args = ['--archive-file', str(archive), '--extract-to', str(root), '--extract-overwrite']
            else:
                listing_args = feature_args
            full = inventory(loads(measurement.command('full-list', [wrapper, 'list', '--message-format', 'json'] + listing_args, 'full.json')))
            if role == 'prepare':
                parts = [inventory(loads(measurement.command(f'partition-{i}-list', [wrapper, 'list', '--message-format', 'json'] + reuse_args(root) + ['--partition', f'hash:{i}/2'], f'partition-{i}.json')), True) for i in (1, 2)]
                partitions(full, parts)
                version = measurement.command('compiled-identity', [str(root / 'target/debug/cadence'), '--version'])
                require(version.endswith('+' + measurement.receipt['context']['source']), 'archive binary source mismatch')
                with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
                    output.write('archive_sha=' + measurement.receipt['archive']['sha256'] + '\n')
            measurement.test('default-refusals', [wrapper, '--test', 'test_seam', '--locked'])
            if role == 'baseline':
                measurement.test('full-suite', [wrapper] + feature_args, full)
            measurement.command('doctests', ['cargo', 'test', '--doc', '--locked'])
        else:
            prep = verify_receipt(prepared)
            require(prep['role'] == 'prepare' and prep['context'] == measurement.receipt['context'], 'preparation context mismatch')
            archive = prepared / 'suite.tar.zst'
            require(digest(archive) == prep['archive']['sha256'] == os.environ['EXPECTED_ARCHIVE_SHA'], 'archive digest mismatch')
            measurement.receipt['archive'] = prep['archive']
            full = inventory(loads(measurement.command('extract-full-list', [wrapper, 'list', '--message-format', 'json', '--archive-file', str(archive), '--extract-to', str(root)], 'full.json')))
            require(full == inventory(loads(read(prepared / 'full.json'))), 'archive inventory mismatch')
            for suite in loads(read(out / 'full.json'))['rust-suites'].values():
                require(Path(suite['binary-path']).is_file() and suite['cwd'] == str(root), 'embedded fixture layout unavailable')
            version = measurement.command('compiled-identity', [str(root / 'target/debug/cadence'), '--version'])
            require(version.endswith('+' + prep['context']['source']), 'extracted binary source mismatch')
            number = int(role[-1])
            part = inventory(loads(measurement.command('partition-list', [wrapper, 'list', '--message-format', 'json'] + reuse_args(root) + ['--partition', f'hash:{number}/2'], 'partition.json')), True)
            require(part == inventory(loads(read(prepared / f'partition-{number}.json')), True), 'partition changed')
            measurement.test('shard', [wrapper] + reuse_args(root) + ['--partition', f'hash:{number}/2'], part)
        measurement.receipt['status'] = 'success'
    except (ValueError, OSError, KeyError, subprocess.SubprocessError, ET.ParseError) as error:
        measurement.receipt['error'] = str(error)
        raise
    finally:
        measurement.finish()


def verify_receipt(directory, require_archive=True):
    receipt = loads(read(directory / 'receipt.json'))
    required = {
        'baseline': {'inventory', 'full-list', 'default-refusals', 'full-suite', 'doctests'},
        'prepare': {'inventory', 'archive', 'full-list', 'partition-1-list', 'partition-2-list', 'compiled-identity', 'default-refusals', 'doctests'},
        'shard-1': {'extract-full-list', 'compiled-identity', 'partition-list', 'shard'},
        'shard-2': {'extract-full-list', 'compiled-identity', 'partition-list', 'shard'},
    }
    names = [p['name'] for p in receipt['phases']]
    require(receipt['role'] in required and len(names) == len(set(names)) and set(names) == required[receipt['role']], 'missing/duplicate/unexpected phase')
    require(receipt['status'] == 'success' and receipt['phases'] and all(p['exit_code'] == 0 for p in receipt['phases']), 'failed/incomplete measurement')
    artifacts = {name + '.log' for name in names}
    artifacts.add('full.json')
    if receipt['role'] in ('baseline', 'prepare'):
        artifacts.add('default-refusals.xml')
    if receipt['role'] == 'baseline':
        artifacts.add('full-suite.xml')
    elif receipt['role'] == 'prepare':
        artifacts.update(('partition-1.json', 'partition-2.json', 'suite.tar.zst'))
    else:
        artifacts.update(('partition.json', 'shard.xml'))
    require(isinstance(receipt['files'], dict) and artifacts <= receipt['files'].keys(), 'missing required artifact ledger entries')
    require(all(isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value) for value in receipt['files'].values()), 'invalid artifact digest')
    if receipt['role'] == 'prepare':
        require(receipt['files']['suite.tar.zst'] == receipt['archive']['sha256'], 'archive ledger mismatch')
    for name, expected in receipt['files'].items():
        require(Path(name).name == name, 'artifact path escape')
        if name == 'suite.tar.zst' and not require_archive:
            continue
        require(digest(directory / name) == expected, 'artifact digest mismatch')
    return receipt


def aggregate(directory, needs):
    require(set(needs) == {'baseline', 'prepare', 'shards'} and all(v['result'] == 'success' for v in needs.values()), 'failed/skipped upstream job')
    receipts = {role: verify_receipt(directory / ('measurement-' + role), require_archive=False) for role in ('baseline', 'prepare', 'shard-1', 'shard-2')}
    prep = receipts['prepare']
    require(re.fullmatch('[0-9a-f]{64}', prep['archive']['sha256']) and prep['archive']['bytes'] > 0 and
            needs['prepare']['outputs']['archive_sha'] == prep['archive']['sha256'], 'missing or inconsistent archive output')
    require(all(r['role'] == role and r['context'] == prep['context'] for role, r in receipts.items()), 'mixed source/run/layout/tool/config context')
    require(prep['context']['source'] == os.environ['GITHUB_SHA'] and prep['context']['run'] == os.environ['GITHUB_RUN_ID'], 'stale source/run')
    full = inventory(loads(read(directory / 'measurement-prepare/full.json')))
    baseline = directory / 'measurement-baseline'
    require(inventory(loads(read(baseline / 'full.json'))) == full, 'baseline inventory mismatch')
    junit(read(baseline / 'full-suite.xml'), full)
    parts = [inventory(loads(read(directory / f'measurement-prepare/partition-{i}.json')), True) for i in (1, 2)]
    partitions(full, parts)
    for i, part in enumerate(parts, 1):
        shard = directory / f'measurement-shard-{i}'
        require(receipts[f'shard-{i}']['archive'] == prep['archive'], 'mixed archive')
        require(inventory(loads(read(shard / 'full.json'))) == full and inventory(loads(read(shard / 'partition.json')), True) == part, 'wrong shard manifest')
        junit(read(shard / 'shard.xml'), part)
    return {'source': prep['context']['source'], 'archive': prep['archive'], 'discovered': len(full), 'executed': sum(selected(full).values()),
            'phase_seconds': {role: {p['name']: (p['end_ns'] - p['start_ns']) / 1e9 for p in r['phases']} for role, r in receipts.items()},
            'limits': 'Phase wall times exclude queue/setup/upload; use GitHub job timestamps for critical path and runner minutes. Two shards do not prove savings; archive portability and exactly-once executed coverage are checked. Fixture Cargo subprocesses remain real.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('role', choices=('baseline', 'prepare', 'shard-1', 'shard-2', 'aggregate'))
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--prepared', type=Path)
    args = parser.parse_args()
    try:
        if args.role == 'aggregate':
            result = aggregate(args.output, loads(os.environ['NEEDS_JSON']))
            (args.output / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
            print(json.dumps(result, indent=2))
        else:
            run(Path.cwd().resolve(), args.output.resolve(), args.role, args.prepared)
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError, ET.ParseError) as error:
        print(f'Invalid archive measurement: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
