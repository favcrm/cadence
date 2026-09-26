#!/usr/bin/env python3
"""Check local Markdown link paths in the tracked contributor guide set.

Supports inline links/images and reference definitions; skips code examples,
external URLs and fragments. This is a path check, not an anchor/HTML validator.
Read working contents, but resolve against Git's index so ignored local docs
cannot make a broken fresh-clone link pass. Run after staging new guide files.
"""

import argparse
import posixpath
import re
import subprocess
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent
GUIDES = (
    "README.md",
    "CONTRIBUTING.md",
    "docs/START-HERE.md",
    "docs/ARCHITECTURE.md",
    "docs/CHARTER.md",
    "docs/BOARD.md",
    "docs/AUDIT.md",
    "docs/design/DEVELOPMENT-TEAM.md",
    "docs/roles/risk-classes.md",
)
DESTINATION = r"(<[^>\n]+>|(?:[^\s()]|\([^()]*\))+)"
INLINE = re.compile(r"!?\[[^\]\n]*\]\(\s*" + DESTINATION + r"(?:\s+[^\n)]*)?\s*\)")
REFERENCE = re.compile(r"^\s{0,3}\[[^\]\n]+\]:\s*" + DESTINATION, re.MULTILINE)


def prose(markdown):
    """Exclude fenced blocks and inline code while preserving line numbers."""
    lines = []
    fence = None
    for line in markdown.splitlines(keepends=True):
        marker = re.match(r"^\s{0,3}(`{3,}|~{3,})", line)
        if fence:
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence):
                fence = None
            lines.append("\n" if line.endswith("\n") else "")
        elif marker:
            fence = marker[1]
            lines.append("\n" if line.endswith("\n") else "")
        else:
            lines.append(re.sub(r"(`+).*?\1", "", line))
    return "".join(lines)


def broken_links(document, markdown, tracked):
    text = prose(markdown)
    errors = []
    for pattern in (INLINE, REFERENCE):
        for match in pattern.finditer(text):
            destination = match[1].strip("<>")
            url = urlsplit(destination)
            if url.scheme or url.netloc or not url.path:
                continue
            path = unquote(url.path)
            target = posixpath.normpath(posixpath.join(posixpath.dirname(document), path))
            # A directory link must contain at least one tracked file.
            exists = target in tracked or any(name.startswith(target + "/") for name in tracked)
            if path.startswith("/") or target == ".." or target.startswith("../") or not exists:
                line = text.count("\n", 0, match.start()) + 1
                errors.append(f"{document}:{line}: {destination} resolves outside the tracked tree or is absent ({target})")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("documents", nargs="*", help="repo-relative Markdown paths (default: contributor guide set)")
    args = parser.parse_args()
    tracked = set(subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0"))
    tracked.discard("")
    errors = []
    documents = args.documents or GUIDES
    for document in documents:
        if document not in tracked:
            errors.append(f"{document}: guide is not tracked")
            continue
        errors.extend(broken_links(document, (ROOT / document).read_text(), tracked))
    if errors:
        print("\n".join(errors))
        return 1
    print(f"Documentation paths resolve in the tracked tree ({len(documents)} guides).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
