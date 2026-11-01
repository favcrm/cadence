#!/usr/bin/env python3
"""CAD-1370 independent acceptance for the trusted-base tests feedback scope.

Ticket (operator-revised scope): a pull_request diff touching only
*pre-existing, regular, top-level* ``tests/<identifier>.rs`` files that
Cargo auto-discovery owns at the trusted base may report the narrow
``tests`` feedback scope with exactly those target names. Everything else —
new/deleted/renamed/mode-changed test files, nested ``tests/**/main.rs``,
shared files referenced from other base sources, src/manifest/config edits,
mixed diffs, the policy file itself, bad SHAs, and every non-PR event
including ``merge_group`` — must be ``full`` or a nonzero refusal. It may
never emit a forged or narrowed scope.

Drives the real ``scripts/ci-change-scope.py --repo/--base/--head/--event``
executable on a synthetic temporary git repo (real commits, real diff);
nothing re-implements the policy. ~150 lines of focused behavioral checks —
no interface discovery, no full Cargo build, no runner harness. The
``run-result-tests`` runner probe is deferred until the implementer
publishes its interface.

Run:  python3 scripts/acceptance/test-cad-1370-feedback-contract.py
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
POLICY = ROOT / "scripts" / "ci-change-scope.py"

MANIFEST = '[package]\nname = "cad-1370-fixture"\nversion = "0.1.0"\nedition = "2021"\n'


def fail(msg: str) -> "NoReturn":
    print(f"cad-1370 acceptance FAIL: {msg}", file=sys.stderr)
    raise SystemExit(1)


def ok(msg: str) -> None:
    print(f"  {msg}")


def git(repo: Path, *args: str) -> str:
    r = subprocess.run(["git", "-C", str(repo), *args],
                       capture_output=True, text=True)
    if r.returncode != 0:
        fail(f"fixture git {' '.join(args)} failed: {r.stderr.strip()}")
    return r.stdout.strip()


def commit(repo: Path) -> str:
    git(repo, "add", "-A")
    git(repo, "-c", "user.name=cad1370", "-c", "user.email=c1370@x.invalid",
        "commit", "-qm", "c")
    return git(repo, "rev-parse", "HEAD")


def classify(repo: Path, base: str, head: str, event: str) -> dict:
    if not POLICY.is_file():
        fail(f"policy not found: {POLICY}")
    r = subprocess.run(
        [sys.executable, "-I", str(POLICY),
         "--repo", str(repo), "--base", base, "--head", head, "--event", event],
        capture_output=True, text=True)
    if r.returncode != 0:
        return {"scope": "__refused__", "reason": r.stderr.strip()}
    try:
        return json.loads(r.stdout)
    except ValueError:
        fail(f"policy emitted non-JSON: {r.stdout!r}")
    raise AssertionError


def scope_of(doc: dict) -> str:
    return doc.get("scope", "")


def must_full(repo: Path, base: str, head: str, event: str, tag: str) -> None:
    doc = classify(repo, base, head, event)
    scope = scope_of(doc)
    if scope == "tests":
        fail(f"{tag}: event={event} narrowed to tests {doc.get('targets')} — "
             "a bad/shared/forged diff must stay full")
    if scope not in {"full", "__refused__"}:
        fail(f"{tag}: unexpected scope {scope!r} for {event}; want full/refusal")
    if doc.get("targets"):
        fail(f"{tag}: scope {scope} must not carry targets {doc['targets']}")
    ok(f"{tag}: {scope} (required)")


def build_repo(repo: Path) -> str:
    """Base commit: manifest (plain auto-discovery) + lib + 3 test targets."""
    (repo / "src").mkdir(parents=True)
    (repo / "tests").mkdir()
    (repo / "docs").mkdir()
    (repo / "Cargo.toml").write_text(MANIFEST)
    (repo / "src" / "lib.rs").write_text("pub fn f() {}\n")
    for n in ("alpha", "beta", "gamma"):
        (repo / "tests" / f"{n}.rs").write_text("#[test] fn t() {}\n")
    (repo / "docs" / "guide.md").write_text("# g\n")
    git(repo, "init", "-q")
    return commit(repo)


def edit(repo: Path, base: str, ops: list[tuple[str, str]]) -> str:
    git(repo, "reset", "--hard", "-q", base)
    git(repo, "clean", "-fdq")
    for op, p in ops:
        f = repo / p
        if op == "w":
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text("// change\n" + (f.read_text() if f.exists() else "#[test] fn t() {}\n"))
        elif op == "d":
            f.unlink()
        elif op == "mv":
            f.rename(f.with_name(f.stem + "_r" + f.suffix))
        elif op == "x":
            f.chmod(0o755)
        elif op == "ln":
            f.unlink()
            f.symlink_to("../src/lib.rs")
    return commit(repo)


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="cad-1370-") as t:
        repo = Path(t) / "repo"
        base = build_repo(repo)

        # Positive: only pre-existing tests/*.rs edits narrow to owned names.
        head = edit(repo, base, [("w", "tests/alpha.rs"), ("w", "tests/gamma.rs")])
        doc = classify(repo, base, head, "pull_request")
        if scope_of(doc) != "tests":
            fail(f"positive M tests/*.rs did not select: {doc!r}")
        if sorted(doc.get("targets", [])) != ["alpha", "gamma"]:
            fail(f"selected targets {doc.get('targets')} != Cargo-owned "
                 "changed files [alpha, gamma]")
        if doc.get("rust") != "true" or doc.get("tests") != "true":
            fail(f"tests scope must keep rust+tests gates on: {doc!r}")
        ok(f"positive: tests {doc['targets']} rust={doc['rust']}")

        # Refusals: stay full / refuse, never narrow.
        cases = [
            ("add",      [("w", "tests/delta.rs")]),            # new target
            # An added file named tests/alpha.rs<LF>: an unanchored TEST_PATH
            # $ once matched it as the existing target 'alpha' and narrowed.
            ("add-newline-name", [("w", "tests/alpha.rs\n")]),
            ("add+edit", [("w", "tests/alpha.rs"), ("w", "tests/delta.rs")]),
            ("delete",   [("d", "tests/alpha.rs")]),
            ("rename",   [("mv", "tests/alpha.rs")]),
            ("symlink",  [("ln", "tests/alpha.rs")]),
            ("execmode", [("x", "tests/alpha.rs")]),
            ("subdir",   [("w", "tests/nested/main.rs")]),      # nested target
            ("src",      [("w", "src/lib.rs")]),
            ("manifest", [("w", "Cargo.toml")]),
            ("mixed",    [("w", "tests/alpha.rs"), ("w", "src/lib.rs")]),
            ("selfsel",  [("w", "scripts/ci-change-scope.py"),
                          ("w", "tests/alpha.rs")]),            # policy self-edit
        ]
        for tag, ops in cases:
            must_full(repo, base, edit(repo, base, ops), "pull_request", tag)

        # merge_group + a test-only diff can never narrow — same event rule
        # covers a forged selected plan: the classifier emits full for it.
        for ev in ("merge_group", "push", "workflow_dispatch",
                   "pull_request_target", ""):
            must_full(repo, base, edit(repo, base, [("w", "tests/alpha.rs")]),
                      ev, f"event-{ev or 'empty'}")

        # Shared: a base .rs source referencing the test file's name.
        #
        # CAD-1970: sharing still forbids per-target NARROWING, but it no
        # longer forces the build/cross/ui gates, because `tests-only` runs
        # every target in the full suite. The refusal this case guards is the
        # one that matters — no `targets` may be emitted for a shared file.
        (repo / "src" / "consumer.rs").write_text("// uses tests/beta.rs\n")
        base_shared = commit(repo)
        shared = classify(repo, base_shared,
                          edit(repo, base_shared, [("w", "tests/beta.rs")]),
                          "pull_request")
        if scope_of(shared) == "tests" or shared.get("targets"):
            fail(f"shared-ref must not narrow targets: {shared!r}")
        shared_scope = shared.get("scope")
        if shared_scope == "tests-only":
            if shared.get("build") != "false":
                fail(f"shared-ref must drop the build gate: {shared!r}")
        elif shared_scope != "full":
            fail(f"shared-ref: unexpected scope {shared_scope!r}")
        # ...while its unreferenced sibling still narrows.
        doc = classify(repo, base_shared,
                       edit(repo, base_shared, [("w", "tests/alpha.rs")]),
                       "pull_request")
        if scope_of(doc) != "tests" or doc.get("targets") != ["alpha"]:
            fail(f"independent sibling should still select alpha: {doc!r}")
        ok("shared-ref: shared file refused, sibling still selected")

        # Bad SHAs.
        for tag, b, h in (("badsha", "notasha", head), ("zero", "0"*40, head)):
            must_full(repo, b, h, "pull_request", tag)

        runner_refusals(Path(t) / "sel")
        workflow_handoff()

    print("CAD-1370 feedback acceptance passed")


# --- runner + workflow guards on the real implementation ------------------

RUNNER = ROOT / "scripts" / "run-result-tests"


def runner_refusals(seldir: Path) -> None:
    """Real run-result-tests --test-selection refusals, zero tool calls.

    Event/missing/empty eligibility refuses before metadata, tool install
    or any external command, so each probe runs with a dead PATH: reaching
    the guard is itself proof no cargo/nextest work happened.
    """
    if not RUNNER.is_file():
        fail(f"runner not found: {RUNNER}")
    seldir.mkdir()
    (seldir / "valid.txt").write_text("some_integration_target\n")
    (seldir / "empty.txt").write_text("")
    cases = [
        # (event, selection arg, required ::error substring)
        ("merge_group", "valid.txt", "pull_request feedback only"),
        ("pull_request", "empty.txt", "selected no targets"),
        ("pull_request", "missing.txt", "selection file does not exist"),
    ]
    for event, sel, want in cases:
        env = {"PATH": "/nonexistent-cad1370", "GITHUB_EVENT_NAME": event}
        r = subprocess.run(["/bin/bash", str(RUNNER),
                            "--test-selection", str(seldir / sel)],
                           cwd=ROOT, env=env, capture_output=True,
                           text=True, timeout=60)
        if r.returncode == 0:
            fail(f"runner accepted bad selection {sel} on {event}")
        if want not in (r.stderr + r.stdout):
            fail(f"runner refusal for {sel} lacked guard message {want!r}:\n"
                 f"{r.stderr}{r.stdout}")
        ok(f"runner {sel} event={event}: refused ({want!r})")


def workflow_handoff() -> None:
    """The test job must feed --test-selection only from trusted outputs."""
    text = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
    for needle in ("targets: ${{ steps.scope.outputs.targets }}",
                   "TARGETS: ${{ needs.change-scope.outputs.targets }}",
                   "run-result-tests --test-selection"):
        if needle not in text:
            fail(f"ci.yml missing trusted handoff {needle!r}")
    ok("workflow: scope->targets output reaches run-result-tests")


if __name__ == "__main__":
    main()
