#!/usr/bin/env python3
"""Exercise the workflow's actual gate predicates without a runner or build."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest

WORKFLOW = Path(os.environ.get("CAD420_WORKFLOW", str(Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml")))
GATES = ["fmt", "clippy", "test", "build", "ui"]


class GateEvidence(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text()
        self.queue = self.workflow.split("  queue-evidence:\n", 1)[1].split("\n  fmt:", 1)[0]
        self.release = self.workflow.split("  release-gate:\n", 1)[1].split("\n  release-build:", 1)[0]

    def queue_passes(self, pages):
        expression = re.search(r"jobs\?filter=latest&per_page=100.*?--jq '([^']+)'", self.queue, re.S)[1]
        # gh --slurp returns all page objects together. Without pagination,
        # the original command only evaluates page one.
        data = pages if "--paginate --slurp" in self.queue else pages[0]
        result = subprocess.run(["jq", "-r", expression], input=json.dumps(data), text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip() == "true"

    def jobs(self):
        return [{"name": name, "status": "completed", "conclusion": "success"} for name in GATES]

    def test_valid_evidence(self):
        self.assertTrue(self.queue_passes([{"jobs": self.jobs()}]))

    def test_missing_gate(self):
        self.assertFalse(self.queue_passes([{"jobs": self.jobs()[:-1]}]))

    def test_every_same_name_job_must_pass(self):
        for conclusion in ["failure", "cancelled", "skipped", None]:
            for first in [True, False]:
                with self.subTest(conclusion=conclusion, first=first):
                    collision = {"name": "test", "status": "completed", "conclusion": conclusion}
                    jobs = [collision] + self.jobs() if first else self.jobs() + [collision]
                    self.assertFalse(self.queue_passes([{"jobs": jobs}]))

    def test_success_conclusion_requires_completed_status(self):
        jobs = self.jobs()
        jobs[0]["status"] = "in_progress"
        self.assertFalse(self.queue_passes([{"jobs": jobs}]))

    def test_failed_collision_on_later_page(self):
        self.assertFalse(self.queue_passes([
            {"jobs": self.jobs()},
            {"jobs": [{"name": "build", "status": "completed", "conclusion": "failure"}]},
        ]))

    def test_gates_split_across_pages_and_successful_duplicates(self):
        self.assertTrue(self.queue_passes([{"jobs": self.jobs()[:2]}, {"jobs": self.jobs()[2:] + self.jobs()}]))

    def run_queue(self, pages, *, fail=False, ref="refs/heads/main"):
        # Execute the checked-in shell; the fake API uses real jq and reports
        # a pagination error even after emitting plausible success output.
        script = textwrap.dedent(self.queue.split("        run: |\n", 1)[1])
        with tempfile.TemporaryDirectory(prefix="cad420-") as directory:
            root = Path(directory)
            gh = root / "gh"
            gh.write_text('''#!/usr/bin/env python3
import json, os, subprocess, sys
if "/jobs?" not in sys.argv[2]:
    print("123")
    sys.exit(0)
pages = json.loads(os.environ["FIXTURE_PAGES"])
data = pages if "--paginate" in sys.argv and "--slurp" in sys.argv else pages[0]
expression = sys.argv[sys.argv.index("--jq") + 1]
result = subprocess.run(["jq", "-r", expression], input=json.dumps(data), text=True)
sys.exit(1 if os.environ["FIXTURE_FAIL"] == "true" else result.returncode)
''')
            gh.chmod(0o755)
            output = root / "output"
            env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                   "FIXTURE_PAGES": json.dumps(pages), "FIXTURE_FAIL": str(fail).lower(),
                   "REF": ref, "REPO": "synthetic/repo", "SHA": "synthetic-sha",
                   "GITHUB_STEP_SUMMARY": str(root / "summary"), "GITHUB_OUTPUT": str(output)}
            result = subprocess.run(["bash", "-c", script], env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            return output.read_text()

    def test_api_error_never_reuses_partial_success(self):
        self.assertIn("tested=false", self.run_queue([{"jobs": self.jobs()}], fail=True))

    def test_queue_shell_success_and_tag_fallback(self):
        self.assertIn("tested=true", self.run_queue([{"jobs": self.jobs()}]))
        self.assertIn("tested=false", self.run_queue([{"jobs": self.jobs()}], ref="refs/tags/v0.1.0-beta.1"))

    def test_tag_gate_runs_even_when_needs_fail_and_stays_tag_only(self):
        self.assertIn("always() && github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')", self.release)
        self.assertIn("needs: [fmt, clippy, test, build, ui]", self.release)

    def test_tag_gate_explicitly_rejects_non_success_and_missing_needs(self):
        self.assertIn("NEEDS_JSON: ${{ toJSON(needs) }}", self.release)
        expression = re.search(r"jq -e '([^']+)'", self.release, re.S)[1]
        good = {name: {"result": "success"} for name in GATES}
        def accepted(data):
            return subprocess.run(["jq", "-e", expression], input=json.dumps(data), text=True, capture_output=True).returncode == 0
        self.assertTrue(accepted(good))
        for name in GATES:
            for result in ["failure", "cancelled", "skipped", None]:
                with self.subTest(name=name, result=result):
                    self.assertFalse(accepted({**good, name: {"result": result}}))
            self.assertFalse(accepted({k: v for k, v in good.items() if k != name}))
        self.assertLess(self.release.index("NEEDS_JSON:"), self.release.index("actions/checkout@"))


if __name__ == "__main__":
    unittest.main()
