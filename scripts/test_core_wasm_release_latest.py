#!/usr/bin/env python3
"""CAD-1800: the core-wasm release can never become the GitHub "Latest" release.

The public installer downloads `releases/latest/download/install.sh`. A
`core-v*` release carries no install.sh, so if it became Latest every new
install would break. Every `gh release create`/`edit` in core-wasm.yml must
pass `--latest=false`, and one unconditional edit must re-assert it so a
re-run on an existing release cannot leave it Latest.
"""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/core-wasm.yml"


def release_commands(text):
    """Every `gh release create|edit ...` command, joined across `\\` continuations."""
    joined = re.sub(r"\\\n\s*", " ", text)
    return [m.group(0) for m in re.finditer(r"gh release (?:create|edit)\b[^\n|&;]*", joined)]


class CoreWasmReleaseNeverLatest(unittest.TestCase):
    def test_every_release_create_and_edit_opts_out_of_latest(self):
        commands = release_commands(WORKFLOW.read_text())
        self.assertTrue(
            any(c.startswith("gh release create") for c in commands),
            "core-wasm.yml no longer creates its release; update this check",
        )
        for command in commands:
            self.assertIn("--latest=false", command, f"would let core-v* become Latest: {command}")

    def test_an_unconditional_edit_reasserts_not_latest(self):
        lines = [l.strip() for l in WORKFLOW.read_text().splitlines()]
        unconditional = [
            l for l in lines
            if l.startswith("gh release edit") and "--latest=false" in l
        ]
        self.assertTrue(
            unconditional,
            "a re-run on an existing core-v* release must re-assert --latest=false with its own `gh release edit` line",
        )


if __name__ == "__main__":
    unittest.main()
