#!/usr/bin/env python3
"""Execute the recorded scope with pinned tools and fixed feature shapes.

CAD-858 reuse path: `--bundle DIR --expected FILE` (tests phase only)
runs the shard off the verified producer archive instead of compiling.
The bundle is verified against independently supplied context before any
nextest call; `target/` must be absent so the single archive-extraction
list is the only writer of `ROOT/target`; every later list/run is
metadata-only (extracted cargo/binaries metadata + workspace/target
remaps) and never invokes Cargo.
"""
import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import subprocess

# Trusted-base capability marker: the producer detects consumer archive
# support by AST, so this must stay a literal assignment, never a flag
# comment.
ARCHIVE_PROTOCOL = 1


def _load_bundle_helper():
    """Sibling ci-nextest-bundle.py — verify_bundle/check_archive only.

    Loaded by path so `python3 /runner-temp/ci-rust-tests.py` in CI works
    regardless of sys.path; a missing helper fails loud at module load.
    """
    path = Path(__file__).resolve().parent / "ci-nextest-bundle.py"
    spec = importlib.util.spec_from_file_location("ci_nextest_bundle", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# Trusted inventory runners are copied alone into RUNNER_TEMP. Legacy
# compilation/inventory must work there without an archive-helper sibling.
bundle = _load_bundle_helper() if Path(__file__).with_name('ci-nextest-bundle.py').is_file() else None


def scope_args(plan):
    if not isinstance(plan, dict):
        raise ValueError('invalid test plan')
    mode = plan.get("mode")
    targets = plan.get("targets")
    if (
        not isinstance(plan, dict)
        or type(plan.get("schema")) is not int
        or plan["schema"] != 1
        or not isinstance(targets, list)
    ):
        raise ValueError("invalid test plan")
    if mode in {"docs", "full"}:
        if targets:
            raise ValueError("docs/full plan cannot carry selected targets")
        return [] if mode == "docs" else ["--all-targets"]
    if mode != "selected" or not targets:
        raise ValueError("unknown or empty selected scope")
    args = ["--lib", "--bins"]
    for target in targets:
        if not isinstance(target, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*", target):
            raise ValueError("invalid Cargo test target")
        args.extend(["--test", target])
    if len(set(targets)) != len(targets):
        raise ValueError('duplicate Cargo test target')
    return args


def partition_args(raw):
    """One shard of the suite (`M/N`, 1-based) as (m, n), or None.

    Only the `tests` phase applies a partition — inventory must always
    list every test. Deliberately an explicit argument, never ambient
    env: exact-command contract tests broke twice on env leaking across
    steps.
    """
    if not raw or not raw.strip():
        return None
    text = raw.strip()
    match = re.fullmatch(r"([1-9][0-9]*)/([1-9][0-9]*)", text)
    if not match:
        raise ValueError(f"invalid test partition {raw!r}, want M/N")
    if int(match.group(1)) > int(match.group(2)):
        raise ValueError(f"invalid test partition {raw!r}, shard exceeds total")
    return int(match.group(1)), int(match.group(2))


def load_weights(path):
    """tests/shard-weights.json → {id: seconds}; malformed fails loud."""
    doc = json.loads(Path(path).read_text())
    if not isinstance(doc, dict) or doc.get("schema") != 1:
        raise ValueError(f"{path}: weights schema must be 1")
    weights = doc.get("weights")
    if not isinstance(weights, dict):
        raise ValueError(f"{path}: weights must be an object")
    out = {}
    for key, seconds in weights.items():
        if not isinstance(key, str) or key.count(" ") != 1:
            raise ValueError(f"{path}: weight key must be '<binary-id> <name>': {key!r}")
        if not isinstance(seconds, (int, float)) or isinstance(seconds, bool):
            raise ValueError(f"{path}: non-numeric weight for {key!r}")
        if not math.isfinite(seconds) or seconds < 0:
            raise ValueError(f"{path}: weight must be finite and >= 0 for {key!r}")
        out[key] = float(seconds)
    return out


def assign(tests, weights, n):
    """Deterministic LPT: heaviest first onto the least-loaded shard.

    `tests` is a list of (binary_id, name) pairs. Missing weights get
    the 0.1s default — weights only affect balance, never coverage.
    A non-finite or negative weight poisons the least-loaded scan, so
    it is refused here too, not only in `load_weights`.
    Returns n lists of ids, each sorted.
    """
    for key, seconds in weights.items():
        if not math.isfinite(seconds) or seconds < 0:
            raise ValueError(f"weight must be finite and >= 0 for {key!r}")
    by_weight = sorted(
        tests,
        key=lambda t: (-weights.get(f"{t[0]} {t[1]}", 0.1), f"{t[0]} {t[1]}"),
    )
    shards = [[] for _ in range(n)]
    loads = [0.0] * n
    for binary_id, name in by_weight:
        lightest = min(range(n), key=lambda i: (loads[i], i))
        shards[lightest].append(f"{binary_id} {name}")
        loads[lightest] += weights.get(f"{binary_id} {name}", 0.1)
    return [sorted(shard) for shard in shards]


def filterset(ids):
    """A nextest -E expression selecting exactly `ids` (`<bid> <name>`).

    `binary_id()` rejects quoted strings and a quoted `test(="...")`
    inside a conjunction silently selects nothing on pinned nextest
    0.9.145, so both travel unquoted — allowed characters verified
    against the DSL's bare-argument charset; anything else fails here,
    loudly, and the -E self-check is the second fence.
    """
    clauses = []
    for ident in ids:
        binary_id, name = ident.split(" ", 1)
        for label, part in (("binary id", binary_id), ("test name", name)):
            if not re.fullmatch(r"[A-Za-z0-9_:\-./]+", part):
                raise ValueError(f"{label} needs quoting the DSL refuses: {part!r}")
        clauses.append(f"(binary_id(={binary_id}) & test(={name}))")
    return " | ".join(clauses)


def list_tests(root, args, extra=None):
    """(binary_id, name) of every non-ignored test the scope selects."""
    command = [
        str(root / "scripts/cadence-nextest"),
        "list",
        *args,
        "--locked",
        "--features",
        "test-seam",
        "--message-format",
        "json",
    ]
    if extra:
        command += extra
    out = subprocess.run(command, cwd=root, check=True, capture_output=True, text=True)
    doc = json.loads(out.stdout)
    tests = []
    for binary_id, suite in doc["rust-suites"].items():
        for name, case in suite["testcases"].items():
            # A -E mismatch still lists the testcase, with status
            # "mismatch" — the self-check would silently see extra ids.
            if (
                case["kind"] == "test"
                and not case["ignored"]
                and case["filter-match"]["status"] == "matches"
            ):
                tests.append((binary_id, name))
    if len(set(tests)) != len(tests):
        raise ValueError("duplicate test id in nextest inventory")
    return sorted(tests)


def inventory_facts(doc):
    """Identity AND run eligibility must agree; matching names alone can omit tests."""
    if not isinstance(doc, dict) or not isinstance(doc.get('rust-suites'), dict):
        raise ValueError('inventory is not a rust-suites document')
    facts = {}
    for binary_id, suite in doc["rust-suites"].items():
        if (
            not isinstance(binary_id, str)
            or not isinstance(suite, dict)
            or not isinstance(suite.get("testcases"), dict)
        ):
            raise ValueError("malformed rust-suites inventory")
        for name, case in suite["testcases"].items():
            if (
                not isinstance(name, str)
                or not isinstance(case, dict)
                or case.get("kind") not in ('test', 'benchmark')
                or type(case.get("ignored")) is not bool
                or not isinstance(case.get("filter-match"), dict)
                or case["filter-match"].get("status") not in {"matches", "mismatch"}
            ):
                raise ValueError("malformed rust-suites testcase")
            ident = f'{binary_id} {name}'
            if ident in facts:
                raise ValueError('duplicate test id in inventory')
            facts[ident] = (case['kind'], case['ignored'], case['filter-match']['status'])
    return facts


def reuse_args(root):
    """Metadata-only nextest flags over the extracted tree — no Cargo."""
    return [
        "--cargo-metadata", str(root / "target/nextest/cargo-metadata.json"),
        "--binaries-metadata", str(root / "target/nextest/binaries-metadata.json"),
        "--workspace-remap", str(root),
        "--target-dir-remap", str(root / "target"),
    ]


def list_reused(root, extra=None):
    """(binary_id, name) of every selected non-ignored extracted test.

    Deliberately never combines --archive-file with the metadata flags:
    the extraction list is the single caller that carries archive flags.
    """
    command = [
        str(root / "scripts/cadence-nextest"),
        "list",
        "--message-format",
        "json",
        *reuse_args(root),
    ]
    if extra:
        command += extra
    out = subprocess.run(command, cwd=root, check=True, capture_output=True, text=True)
    doc = json.loads(out.stdout)
    tests = []
    for binary_id, suite in doc["rust-suites"].items():
        for name, case in suite["testcases"].items():
            if (
                case["kind"] == "test"
                and not case["ignored"]
                and case["filter-match"]["status"] == "matches"
            ):
                tests.append((binary_id, name))
    if len(set(tests)) != len(tests):
        raise ValueError("duplicate test id in extracted inventory")
    return sorted(tests)


def verify_compiled_source(root, expected_sha):
    """The restored binary must prove the exact expected source SHA."""
    binary = root / "target/debug/cadence"
    out = subprocess.run(
        [str(binary), "--version"], cwd=root,
        check=True, capture_output=True, text=True,
    ).stdout
    if not out.rstrip("\n").endswith("+" + expected_sha):
        raise ValueError(
            "extracted binary is not the expected source build: "
            f"{out.strip()!r} does not end with +{expected_sha}"
        )


def write_assignment(path, shard, total, mode, inventory, tests, weights_sha256):
    ids = sorted(inventory)
    digest = hashlib.sha256(("\n".join(ids)).encode()).hexdigest()
    doc = {
        "schema": 1,
        "shard": shard,
        "total": total,
        "mode": mode,
        "inventory_sha256": digest,
        "weights_sha256": weights_sha256,
        "inventory": ids,
        "tests": sorted(tests),
    }
    Path(path).write_text(json.dumps(doc, indent=2) + "\n")


def verify_reuse_inputs(root, bundle_dir, expected_path, plan_bytes):
    """Verify DIR against the expected context before any nextest call or
    receipt. Returns (expected, verified archive path).

    The trusted context is read and shape-checked by the sibling
    verifier; the bundle directory must not live inside the workspace
    root (it is downloaded output, never checkout content), and the CLI
    plan bytes must be identical to the bundle's recorded
    ci-test-plan.json — the shard executes exactly the scope the
    producer pinned.
    """
    if bundle is None:
        raise ValueError('archive reuse requires the helper beside the head runner')
    bundle_dir = Path(bundle_dir)
    if Path(expected_path).resolve().is_relative_to(bundle_dir.resolve()):
        raise ValueError('expected context must be independent of the bundle')
    if bundle_dir.resolve().is_relative_to(root):
        raise ValueError("bundle directory must be independent of the workspace root")
    if plan_bytes is None:
        raise ValueError("archive reuse requires the recorded plan file")
    expected = bundle.read_context(expected_path)
    if not isinstance(expected, dict):
        raise ValueError("expected context must be a JSON object")
    if expected.get("workspace_root") != str(root):
        raise ValueError("expected workspace_root does not equal the actual root")
    archive = bundle.verify_bundle(bundle_dir, expected)
    if plan_bytes != (bundle_dir / "ci-test-plan.json").read_bytes():
        raise ValueError("bundle ci-test-plan.json does not equal the CLI plan")
    return expected, archive


def verify_docs_markers(bundle_dir, archive):
    """A docs bundle is a verified empty receipt: an empty archive file
    and the empty rust-suites inventory. Both are hash-pinned by
    verify_bundle already; the marker check proves the producer chose
    the docs shape, so no extraction ever happens for docs.
    """
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size != 0:
        raise ValueError("docs bundle must carry an empty nextest.tar.zst marker")
    doc = json.loads((bundle_dir / "inventory.json").read_text())
    if doc != {"rust-suites": {}, "test-count": 0}:
        raise ValueError("docs bundle must carry the empty rust-suites inventory marker")


def run(root, plan, phase, partition="", weights=None, assignment_out=None,
        dry_run=False, bundle_dir=None, expected_path=None):
    if (bundle_dir is None) != (expected_path is None):
        raise ValueError("--bundle and --expected are only valid together")
    if bundle_dir is not None and phase != "tests":
        raise ValueError("archive reuse applies to the tests phase only")
    plan_bytes = None
    if isinstance(plan, (str, Path)):
        plan_bytes = Path(plan).read_bytes()
        plan = json.loads(plan_bytes)
    args = scope_args(plan)
    shard = partition_args(partition)
    if plan["mode"] == "docs":
        if bundle_dir is not None:
            _, archive = verify_reuse_inputs(
                root, bundle_dir, expected_path, plan_bytes
            )
            verify_docs_markers(Path(bundle_dir), archive)
        if assignment_out is not None and shard is not None:
            write_assignment(
                assignment_out, shard[0], shard[1], plan["mode"], [], [], ""
            )
        print("Rust execution omitted: " + plan["reason"])
        return
    if phase == "inventory":
        if shard is not None:
            raise ValueError("inventory must never be partitioned")
        command = [str(root / "scripts/nextest-inventory")]
        command += (["all-targets"] if plan["mode"] == "full" else ["selected", *args])
        command += ["--features", "test-seam"]
        subprocess.run(command, cwd=root, check=True)
        return

    reuse = bundle_dir is not None
    if reuse:
        expected, archive = verify_reuse_inputs(
            root, bundle_dir, expected_path, plan_bytes
        )
        target = root / "target"
        if target.exists() or target.is_symlink():
            raise ValueError("target must be absent before archive extraction")
        if os.environ.get("CARGO_TARGET_DIR") not in (None, ""):
            raise ValueError("CARGO_TARGET_DIR must not remap the extracted tree")
        # The archive bytes were verified; now gate unsafe members before
        # the single extraction list that materializes target/.
        bundle.check_archive(archive)
        out = subprocess.run(
            [
                str(root / "scripts/cadence-nextest"),
                "list",
                "--archive-file", str(archive),
                "--extract-to", str(root),
                "--message-format", "json",
            ],
            cwd=root, check=True, capture_output=True, text=True,
        )
        doc = json.loads(out.stdout)
        all_ids = []
        inventory = []
        for binary_id, suite in doc["rust-suites"].items():
            for name, case in suite["testcases"].items():
                all_ids.append(f"{binary_id} {name}")
                if (
                    case["kind"] == "test"
                    and not case["ignored"]
                    and case["filter-match"]["status"] == "matches"
                ):
                    inventory.append((binary_id, name))
        if len(set(all_ids)) != len(all_ids):
            raise ValueError("duplicate test id in extracted inventory")
        producer_doc = json.loads((bundle_dir / 'inventory.json').read_text())
        if inventory_facts(doc) != inventory_facts(producer_doc):
            raise ValueError('extracted inventory eligibility differs from producer inventory.json')
        inventory = sorted(inventory)
        verify_compiled_source(root, expected["source_sha"])
        # The consumer never compiles the suite, but unit tests invoke
        # `cargo` directly (e.g. `cargo tree --locked --offline` in
        # src/store/tests/app_runs.rs) — that needs the registry index
        # and sources cached, not target/. `fetch` materializes them
        # from the same locked Cargo.lock; docs never reaches here.
        subprocess.run(['cargo', 'fetch', '--locked'], cwd=root, check=True)
    else:
        (root / "target/nextest/cadence/junit.xml").unlink(missing_ok=True)
    command = [str(root / "scripts/cadence-nextest")]
    if reuse:
        command += reuse_args(root)
    else:
        command += [*args, "--locked", "--features", "test-seam"]
    if shard is not None:
        weights_doc = {}
        weights_sha256 = ""
        if weights is not None:
            weights_sha256 = hashlib.sha256(Path(weights).read_bytes()).hexdigest()
            weights_doc = load_weights(weights)
        if not reuse:
            inventory = list_tests(root, args)
        inventory_ids = [f"{b} {n}" for b, n in inventory]
        shards = assign(inventory, weights_doc, shard[1])
        mine = shards[shard[0] - 1]
        if assignment_out is not None:
            write_assignment(
                assignment_out, shard[0], shard[1], plan["mode"],
                inventory_ids, mine, weights_sha256,
            )
        if not mine:
            print(f"shard {shard[0]}/{shard[1]}: no tests assigned; nextest skipped")
            return
        expr = filterset(mine)
        # Self-check: the filterset must select exactly this shard's
        # assignment — a quoting or escaping bug fails here, never
        # silently drops tests.
        listed = {f"{b} {n}" for b, n in (
            list_reused(root, ["-E", expr])
            if reuse else list_tests(root, args, ["-E", expr])
        )}
        if listed != set(mine):
            raise ValueError(
                "filterset self-check mismatch — "
                f"missing: {sorted(set(mine) - listed)}; "
                f"extra: {sorted(listed - set(mine))}"
            )
        command += ["-E", expr]
    if dry_run:
        print("dry run:", *command)
        return
    subprocess.run(command, cwd=root, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--phase", choices=["inventory", "tests"], required=True)
    parser.add_argument("--partition", default="", help="test shard as M/N (tests phase only)")
    parser.add_argument("--weights", default=None, help="shard weights JSON (tests phase only)")
    parser.add_argument("--assignment-out", default=None, help="write this shard's assignment JSON")
    parser.add_argument("--dry-run", action="store_true", help="assign and self-check, don't run")
    parser.add_argument("--bundle", type=Path, default=None,
                        help="verified producer bundle dir (tests phase reuse)")
    parser.add_argument("--expected", type=Path, default=None,
                        help="independently supplied expected context JSON")
    args = parser.parse_args()
    if (args.bundle is None) != (args.expected is None):
        parser.error("--bundle and --expected must be used together")
    run(
        args.root.resolve(),
        args.plan,
        args.phase,
        args.partition,
        args.weights,
        args.assignment_out,
        args.dry_run,
        bundle_dir=args.bundle.resolve() if args.bundle else None,
        expected_path=args.expected.resolve() if args.expected else None,
    )


if __name__ == "__main__":
    main()
