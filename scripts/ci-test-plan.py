#!/usr/bin/env python3
"""Select only provably isolated PR test edits; uncertainty runs all targets.

CI reads this policy from the PR base. Merge groups, tags and main pushes
always keep full coverage. This is deliberately not a Rust dependency parser.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess


def full(reason):
    return {"schema": 1, "mode": "full", "targets": [], "reason": reason}


def parse_changes(raw):
    fields = raw.decode("utf-8").split("\0")
    if fields[-1] != "" or len(fields[:-1]) % 2:
        raise ValueError("malformed NUL-delimited diff")
    return list(zip(fields[:-1:2], fields[1:-1:2]))


def doc(path):
    return (path in {"README.md", "CONTRIBUTING.md", "LICENSE"}
            or path.startswith("docs/") and path.endswith(".md") and path != "docs/AUDIT.md")


def select(event, changes, targets):
    if event != "pull_request":
        return full("non-PR events retain all-targets")
    if not changes:
        return full("empty or unknown diff")
    chosen = set()
    for status, path in changes:
        if status not in {"A", "M"}:
            return full("deletion, rename or unrecognised diff status")
        if doc(path):
            continue
        if path not in targets:
            return full("shared, production or unrecognised path: " + path)
        chosen.add(targets[path])
    if not chosen:
        return {"schema": 1, "mode": "docs", "targets": [],
                "reason": "only documentation changed; Rust execution not needed"}
    # The split manifests have cross-file inventory contracts even when
    # only one generated integration binary changed.
    if "tests/split_map_inventory.rs" in targets:
        chosen.add(targets["tests/split_map_inventory.rs"])
    return {"schema": 1, "mode": "selected", "targets": sorted(chosen),
            "reason": "isolated integration edits; include all lib/bin tests and split inventory"}


def metadata_targets(root):
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"],
        cwd=root, capture_output=True, text=True, check=True,
    )
    metadata = json.loads(result.stdout)
    targets = {}
    members = set(metadata["workspace_members"])
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        for target in package["targets"]:
            if "test" not in target["kind"] or target.get("required-features"):
                continue
            source = Path(target["src_path"]).resolve().relative_to(root)
            # Nested fixtures and custom source paths are shared/unknown.
            if (len(source.parts) == 2 and source.parts[0] == "tests"
                    and source.suffix == ".rs"
                    and re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*", target["name"])):
                targets[source.as_posix()] = target["name"]
    return targets


def referenced_changes(root, changes):
    # A test entry can also be included/read by a different target. Treat
    # any tracked source reference (even a comment) as shared rather than
    # guessing Rust module or runtime filesystem dependencies.
    files = subprocess.run(["git", "ls-files", "-z"], cwd=root,
                           capture_output=True, check=True).stdout.decode("utf-8").split("\0")
    names = {path: Path(path).name for _, path in changes}
    for source in files:
        if not source or source.endswith(".md") or source in names:
            continue
        file = root / source
        if not file.is_file():
            continue
        text = file.read_bytes()
        for path, name in names.items():
            stem = Path(path).stem.encode()
            module = (path.endswith(".rs") and source.endswith(".rs")
                      and re.search(rb"\bmod\s+" + re.escape(stem) + rb"\b", text))
            if path.encode() in text or name.encode() in text or module:
                return source + " references " + path
    return None


def root_readme_content_reference(root):
    """Find Rust source that actually embeds or directly reads the root README.

    Bare README names in fixtures and tracker code are not references to the
    checkout's README. Keep a full PR scope for direct source-content reads;
    an expression too complex to resolve is also a full-scope fallback.
    """
    files = subprocess.run(["git", "ls-files", "-z"], cwd=root,
                           capture_output=True, check=True).stdout.decode("utf-8").split("\0")
    target = (root / "README.md").resolve()
    direct = re.compile(
        rb'(?P<call>include_(?:str|bytes)!|fs::read(?:_to_string)?|File::open|Path::new)'
        rb'\s*\(\s*"(?P<path>[^"\n]*README\.md)"'
    )
    calls = re.compile(rb'include_(?:str|bytes)!|fs::read(?:_to_string)?|File::open|Path::new')
    for source in files:
        if not source.endswith(".rs"):
            continue
        file = root / source
        if not file.is_file():
            continue
        text = file.read_bytes()
        if b"README.md" not in text:
            continue
        for call in calls.finditer(text):
            match = direct.match(text, call.start())
            if match:
                literal = match.group("path").decode("utf-8", errors="replace")
                # Compile-time includes resolve from the source file; runtime
                # relative paths use the repository cwd or remain uncertain.
                base = file.parent if match.group("call").startswith(b"include_") else root
                if literal.startswith("../") and base == root:
                    return source + " may read README.md"
                if (base / literal).resolve() == target:
                    return source + " reads README.md"
            elif b"README.md" in text[call.end():].split(b";", 1)[0]:
                # concat!, a variable argument, or a more complex expression
                # that mentions README: do not guess where it resolves.
                return source + " may read README.md"
    return None


def make_plan(root, event, payload):
    if event != "pull_request":
        return full("non-PR events retain all-targets")
    base = payload["pull_request"]["base"]["sha"]
    head = payload["pull_request"]["head"]["sha"]
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (base, head)):
        raise ValueError("diff requires full base/head SHAs")
    result = subprocess.run(
        ["git", "diff", "--name-status", "-z", "--no-renames", base + "..." + head],
        cwd=root, capture_output=True, check=True,
    )
    changes = parse_changes(result.stdout)
    # The root README is explicitly a documentation path, but the broad
    # basename scan catches unrelated tracker/fixture README strings. Use a
    # content-read check only for this exact one-file documentation edit.
    readme_only = len(changes) == 1 and changes[0][0] in {"A", "M"} and changes[0][1] == "README.md"
    reference = (root_readme_content_reference(root) if readme_only
                 else referenced_changes(root, changes))
    if reference:
        return full("shared file: " + reference)
    # Documentation can be classified without Cargo or a toolchain install.
    targets = {} if changes and all(doc(path) for _, path in changes) else metadata_targets(root)
    plan = select(event, changes, targets)
    plan.update({"base_sha": base, "head_sha": head,
                 "changes": [{"status": s, "path": p} for s, p in changes]})
    return plan


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--event-name", default=os.environ.get("GITHUB_EVENT_NAME", ""))
    parser.add_argument("--event-file", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        plan = make_plan(args.root.resolve(), args.event_name, json.loads(args.event_file.read_text()))
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        plan = full("selection unavailable; full fallback: " + type(error).__name__)
    args.out.write_text(json.dumps(plan) + "\n")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write("mode=" + plan["mode"] + "\n")
    print(json.dumps(plan))


if __name__ == "__main__":
    main()
