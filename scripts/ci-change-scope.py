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
UI_CONFIG = re.compile(r"(?:^|/)(?:package[^/]*\.json|tsconfig[^/]*\.json|"
                       r"pnpm-[^/]*\.yaml|[^/]*\.config\.[^/]+)$")
UI_EXT = {".ts", ".tsx", ".js", ".jsx", ".mjs", ".css", ".html", ".svg",
          ".png", ".jpg", ".jpeg", ".webp", ".woff", ".woff2", ".json"}


def result(scope, reason):
    return dict(scope=scope, rust="true" if scope == "full" else "false",
                ui="false" if scope == "docs" else "true", reason=reason)


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True,
                          capture_output=True, timeout=30).stdout


def category(path):
    if path in {"README.md", "CONTRIBUTING.md"}:
        return "docs"
    if path.startswith("docs/") and path.endswith(".md"):
        if path not in DOC_POLICY and not path.startswith(("docs/roles/", "docs/design/", "docs/security")):
            return "docs"
    if path.startswith("ui/") and not UI_CONFIG.search(path):
        if path != "ui/pnpm-lock.yaml" and PurePosixPath(path).suffix.lower() in UI_EXT:
            return "ui"
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
        for header, path in zip(fields[::2], fields[1::2]):
            parts = header.split()
            if len(parts) != 5 or not parts[0].startswith(":"):
                return result("full", "malformed change record")
            old_mode, new_mode, _, _, status = parts
            if status not in {"A", "M"} or new_mode != "100644":
                return result("full", "deletion, mode change or unsupported change")
            if status == "M" and old_mode != ":100644":
                return result("full", "type or mode change")
            categories.add(category(path))
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
