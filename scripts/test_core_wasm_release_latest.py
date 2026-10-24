#!/usr/bin/env python3
"""CAD-1800/CAD-1801: the core-wasm release is never "Latest", and is drafted before upload.

The public installer downloads `releases/latest/download/install.sh`. A
`core-v*` release carries no install.sh, so if it became Latest every new
install would break. Every `gh release create`/`edit` in core-wasm.yml must
pass `--latest=false`, and one unconditional edit must re-assert it so a
re-run on an existing release cannot leave it Latest.

CAD-1801: the repo has immutable releases, so a published release refuses
asset uploads (core-v0.2.5 was left empty that way). The release must be
created as a draft, its assets uploaded, and only then published; a re-run
must check `isDraft` so it never tries to change an already published one.
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


class CoreWasmReleaseDraftThenPublish(unittest.TestCase):
    def lines(self):
        return [l.strip() for l in WORKFLOW.read_text().splitlines()]

    def test_release_is_created_as_a_draft(self):
        creates = [c for c in release_commands(WORKFLOW.read_text()) if c.startswith("gh release create")]
        self.assertTrue(creates, "core-wasm.yml no longer creates its release; update this check")
        for create in creates:
            self.assertIn("--draft", create, f"a published release refuses uploads (immutable releases): {create}")

    def test_publish_comes_after_the_upload(self):
        lines = self.lines()
        uploads = [i for i, l in enumerate(lines) if l.startswith("gh release upload")]
        publishes = [
            i for i, l in enumerate(lines)
            if l.startswith("gh release edit") and "--draft=false" in l and "--latest=false" in l
        ]
        self.assertTrue(uploads, "core-wasm.yml no longer uploads its assets")
        self.assertTrue(publishes, "the release must be published with `gh release edit ... --draft=false --latest=false`")
        self.assertGreater(min(publishes), max(uploads), "publish (--draft=false) must come after every upload")

    def test_rerun_checks_draft_state(self):
        self.assertIn(
            "isDraft",
            WORKFLOW.read_text(),
            "a re-run must check isDraft and refuse to touch an already published (immutable) release",
        )


if __name__ == "__main__":
    unittest.main()
