#!/usr/bin/env python3
"""Run the checked-in publication shell with synthetic assets and a strict gh double.

No network, release, tag, build or installed executable is used.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

WORKFLOW = Path(os.environ.get("CAD661_WORKFLOW", str(Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml")))


class PublicationPolicy(unittest.TestCase):
    def run_publish(self, tag, state, *, published_pre=False, different=False):
        workflow = WORKFLOW.read_text()
        step = workflow.split("      - name: Publish the GitHub Release\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("\n  # CAD-311:", 1)[0])
        with tempfile.TemporaryDirectory(prefix="cad661-") as directory:
            root = Path(directory)
            dist = root / "dist"
            dist.mkdir()
            for name in [f"cadence-{tag}-x86_64-linux.tar.gz", f"cadence-{tag}-x86_64-linux.tar.gz.sha256", "install.sh"]:
                (dist / name).write_text("synthetic bytes\n")
            gh = root / "gh"
            gh.write_text('''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as calls:
    calls.write(json.dumps(args) + "\\n")
if args[:2] == ["release", "view"]:
    field = args[args.index("--json") + 1]
    if os.environ["STATE"] == "missing":
        sys.exit(1)
    if field == "isDraft":
        print("true" if os.environ["STATE"] == "draft" else "false")
    elif field == "isPrerelease":
        print(os.environ["PUBLISHED_PRE"])
    else:
        raise SystemExit("unsupported view field")
elif args[:2] == ["release", "download"]:
    dest = pathlib.Path(args[args.index("--dir") + 1])
    for source in pathlib.Path.cwd().iterdir():
        if source.is_file():
            shutil.copyfile(source, dest / source.name)
    if os.environ["DIFFERENT"] == "true":
        (dest / "install.sh").write_text("different bytes")
elif args[:2] in [["release", "create"], ["release", "edit"], ["release", "upload"]]:
    # All exercised flags are real gh release create/edit/upload options;
    # create retains draft+verify-tag, and publish is an explicit edit.
    pass
else:
    raise SystemExit("unexpected command: " + repr(args))
''')
            gh.chmod(0o755)
            calls = root / "calls"
            env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}", "TAG": tag,
                   "STATE": state, "PUBLISHED_PRE": str(published_pre).lower(),
                   "DIFFERENT": str(different).lower(), "CALLS": str(calls)}
            result = subprocess.run(["bash", "-c", script], cwd=dist, env=env, text=True, capture_output=True)
            return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def assert_classification(self, calls, command, pre):
        selected = [call for call in calls if call[:2] == ["release", command]]
        self.assertTrue(selected, calls)
        for call in selected:
            self.assertIn(f"--prerelease={str(pre).lower()}", call)
            self.assertIn(f"--latest={str(not pre).lower()}", call)

    def test_fresh_draft_has_explicit_classification_before_publication(self):
        for tag, pre in [("v0.1.0-beta.1", True), ("v0.1.0", False), ("v0.1.0+build-stamp", False)]:
            with self.subTest(tag=tag):
                result, calls = self.run_publish(tag, "missing")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assert_classification(calls, "create", pre)
                self.assert_classification(calls, "edit", pre)
                create = next(call for call in calls if call[:2] == ["release", "create"])
                self.assertIn("--draft", create)
                self.assertIn("--verify-tag", create)

    def test_reused_draft_classification_is_set_explicitly(self):
        for tag, pre in [("v0.1.0-beta.1", True), ("v0.1.0", False), ("v0.1.0+build-stamp", False)]:
            with self.subTest(tag=tag):
                result, calls = self.run_publish(tag, "draft", published_pre=not pre)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assert_classification(calls, "edit", pre)
                self.assertEqual(len([call for call in calls if call[:2] == ["release", "upload"]]), 1)
                self.assertFalse(any(call[:2] == ["release", "create"] for call in calls))

    def test_published_classification_mismatch_refuses_without_mutation(self):
        for tag, actual in [("v0.1.0-beta.1", False), ("v0.1.0", True)]:
            with self.subTest(tag=tag):
                result, calls = self.run_publish(tag, "published", published_pre=actual)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(call[1] in ["create", "edit", "upload"] for call in calls))

    def test_published_matching_bytes_and_classification_are_read_only(self):
        for tag, pre in [("v0.1.0-beta.1", True), ("v0.1.0", False)]:
            with self.subTest(tag=tag):
                result, calls = self.run_publish(tag, "published", published_pre=pre)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(any(call[:2] == ["release", "view"] and "isPrerelease" in call for call in calls))
                self.assertFalse(any(call[1] in ["create", "edit", "upload"] for call in calls))

    def test_published_different_bytes_still_refuse_without_mutation(self):
        result, calls = self.run_publish("v0.1.0-beta.1", "published", published_pre=True, different=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call[1] in ["create", "edit", "upload"] for call in calls))


if __name__ == "__main__":
    unittest.main()
