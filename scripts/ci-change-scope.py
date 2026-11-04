#!/usr/bin/env python3
"""Trusted-base PR feedback policy; all non-PR/unknown cases run full gates.

Narrow PR scopes are feedback only, never permission to skip merge-queue
integration validation. The workflow loads THIS file from the base commit
with isolated Python; PR code cannot change its own selection policy.
"""
import argparse
import json
from pathlib import PurePosixPath
import re
import subprocess

SHA = re.compile(r"[0-9a-f]{40}")
DOC_POLICY = {"docs/TEAM.md", "docs/CHARTER.md"}
# CAD-1370: a narrowed PR may touch only pre-existing top-level
# tests/<name>.rs integration targets. [A-Za-z0-9_]+ only.
TEST_PATH = re.compile(r"^tests/([A-Za-z0-9_]+)\.rs\Z")
UI_CONFIG = re.compile(r"(?:^|/)(?:package[^/]*\.json|tsconfig[^/]*\.json|"
                       r"pnpm-[^/]*\.yaml|[^/]*\.config\.[^/]+)$")
UI_EXT = {".ts", ".tsx", ".js", ".jsx", ".mjs", ".css", ".html", ".svg",
          ".png", ".jpg", ".jpeg", ".webp", ".woff", ".woff2", ".json"}


# ci.yml's cross-build legs compile this one test target on the aarch64
# runners. A change that reaches it must keep those legs, which is why it is
# named here rather than inferred: an edit to this file is the one test-only
# change a build gate actually observes.
CROSS_TEST_TARGET = "tests/cad1425_client_update_refusal_acceptance.rs"


def result(scope, reason, targets=(), narrow_cross=True):
    # rust covers the fmt/clippy/build gates; `tests` narrows only which
    # integration targets the `test` job runs, not whether rust gates run.
    out = dict(scope=scope,
               rust="false" if scope in {"docs", "ui"} else "true",
               ui="true" if scope in {"ui", "full"} else "false",
               reason=reason)
    out["tests"] = "true" if scope in {"tests", "tests-only", "full"} else "false"
    out["targets"] = sorted(targets)
    # CAD-1970: a change confined to tests/**.rs cannot alter the shipping
    # binary, the ui bundle or another target's compilation, so those two
    # gates are PR feedback that costs ~1100 runner-seconds to compile code
    # the diff does not touch. They still run in the merge queue, on main and
    # on any non-PR event, which `result()` never narrows.
    out["build"] = "false" if scope == "tests-only" else "true"
    out["cross"] = "false" if scope == "tests-only" and narrow_cross else "true"
    return out


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True,
                          capture_output=True, timeout=30).stdout


def git_base(repo, base, *args):
    return subprocess.run(["git", "-C", repo, *args, base + "^{commit}"],
                          check=True, capture_output=True, timeout=30).stdout


def rust_sources(repo, base):
    """Every tracked path at base, NUL-safe (trusted-base git data only)."""
    raw = git_base(repo, base, "ls-tree", "-r", "-z", "--name-only")
    names = raw.decode("utf-8").split("\0")
    if not names or names.pop() != "":
        raise ValueError("malformed base tree listing")
    return names


def test_targets(repo, base, paths):
    """Selected integration targets for a tests-only PR, else None.

    Every changed path must be a top-level tests/<name>.rs that already
    exists at base. Any explicit [[test]] table or autotests = false in
    the BASE manifest, a nested tests/**/main.rs cargo target at base, or
    a reference to the file's basename, repo path or `mod <name>` from ANY
    OTHER tracked base .rs source means ownership is ambiguous: the caller
    falls back to the full suite rather than trusting head cargo metadata
    or import heuristics.
    """
    targets = sorted({TEST_PATH.match(p).group(1) for p in paths})
    try:
        import tomllib
        manifest = tomllib.loads(
            git(repo, "show", base + ":Cargo.toml").decode("utf-8"))
        package = manifest.get("package") or {}
        if package.get("autotests") is False or manifest.get("test"):
            return None
    except (OSError, subprocess.SubprocessError, ValueError, KeyError):
        return None
    names = rust_sources(repo, base)
    selected_files = {f"tests/{t}.rs" for t in targets}
    if not selected_files <= set(names):
        return None
    for name in names:
        # Only tests/<dir>/main.rs is a cargo target; deeper .rs files
        # (tests/fixtures/*.rs) are module helpers, not ambiguity.
        if re.match(r"^tests/[^/]+/main\.rs$", name):
            return None
    # Ownership guard, one git grep over the whole base tree: a token hit
    # in any OTHER tracked .rs source means the selected file is shared
    # (a `mod` sibling, a #[path]/include! unit, or a fixture reference)
    # and the PR is not independent.
    tokens = sorted(set(targets) | selected_files |
                    {f"tests/{t}" for t in targets})
    args = []
    for token in tokens:
        args += ["-e", token]
    grep = subprocess.run(
        ["git", "-c", "core.quotePath=false", "-C", repo,
         "grep", "-l", "-F", *args, base + "^{commit}"],
        capture_output=True, timeout=60)
    if grep.returncode not in (0, 1):
        return None  # grep failed: conservative fallback
    for line in grep.stdout.decode("utf-8", errors="replace").splitlines():
        _, _, path = line.partition(":")
        if not path:
            return None
        if path.endswith(".rs") and path not in selected_files:
            return None
    return targets


def category(path):
    if path in {"README.md", "CONTRIBUTING.md"}:
        return "docs"
    if path.startswith("docs/") and path.endswith(".md"):
        if path not in DOC_POLICY and not path.startswith(("docs/roles/", "docs/design/", "docs/security")):
            return "docs"
    if path.startswith("ui/") and not UI_CONFIG.search(path):
        if path != "ui/pnpm-lock.yaml" and PurePosixPath(path).suffix.lower() in UI_EXT:
            return "ui"
    if TEST_PATH.match(path):
        return "tests"
    return "full"


def classify(repo, base, head, event):
    if event != "pull_request":
        return result("full", "non-PR: authoritative integration validation")
    if not SHA.fullmatch(base) or not SHA.fullmatch(head):
        return result("full", "invalid commit SHA")
    try:
        for sha in (base, head):
            if git(repo, "cat-file", "-t", sha).strip() != b"commit":
                return result("full", "revision is not a commit")
        # One query includes modes, status and NUL-delimited paths. Disable
        # rename detection: rename becomes deletion+addition and falls back.
        raw = git(repo, "diff", "--raw", "--no-abbrev", "-z", "--no-renames", base + "..." + head)
        fields = raw.decode("utf-8").split("\0")
        if fields.pop() != "" or not fields or len(fields) % 2:
            return result("full", "empty or malformed diff")
        categories = set()
        paths = []
        for header, path in zip(fields[::2], fields[1::2]):
            parts = header.split()
            if len(parts) != 5 or not parts[0].startswith(":"):
                return result("full", "malformed change record")
            old_mode, new_mode, _, _, status = parts
            if status not in {"A", "M"} or new_mode != "100644":
                return result("full", "deletion, mode change or unsupported change")
            if status == "M" and old_mode != ":100644":
                return result("full", "type or mode change")
            kind = category(path)
            if kind == "tests" and status != "M":
                return result("full", "new test targets require full validation")
            categories.add(kind)
            paths.append(path)
        if categories == {"tests"}:
            # Only M-of-100644 edits to pre-existing top-level integration
            # targets, with the trusted-base ownership guard above.
            #
            # CAD-1970: when that guard refuses — this repository declares
            # explicit [[test]] tables and funnels most acceptance files into
            # the `suite` target, so it refuses every test edit — the diff is
            # still provably confined to tests/**.rs. Nothing outside tests
            # can change, so the full suite, clippy --all-targets and fmt are
            # the entire meaningful signal; `build` and `cross-build` compile
            # the shipping binary and another target, which this diff cannot
            # reach. That is `tests-only`, and the merge queue is unchanged.
            targets = test_targets(repo, base, paths)
            if targets:
                return result("tests", "independent test-only PR feedback scope; "
                                       "queue remains full", targets)
            if all(re.fullmatch(r"tests/.+\.rs", p) for p in paths):
                return result("tests-only", "tests-only PR feedback scope; "
                                            "queue remains full",
                              narrow_cross=CROSS_TEST_TARGET not in paths)
            return result("full", "test selection is new, deleted, shared or ambiguous")
        if len(categories) == 1 and "full" not in categories:
            return result(categories.pop(), "isolated PR feedback scope; queue remains full")
        return result("full", "shared, mixed, policy or unknown inputs")
    except (OSError, subprocess.SubprocessError, ValueError):
        return result("full", "classification unavailable: conservative fallback")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("repo", "base", "head", "event"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    print(json.dumps(classify(args.repo, args.base, args.head, args.event)))


if __name__ == "__main__":
    main()
