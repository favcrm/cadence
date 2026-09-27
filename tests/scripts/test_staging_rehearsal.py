"""Exercise rehearsal ordering, recovery, clean child env and failure handling."""
from pathlib import Path
import json
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/staging-rehearsal.py"
FAKE = '''#!/usr/bin/python3
import json, os, sqlite3, sys
from pathlib import Path
assert not any(k in os.environ for k in ("GH_TOKEN", "CADENCE_ALIAS", "CADENCE_STATE_DIR"))
state = Path(sys.argv[2]); state.mkdir(parents=True, exist_ok=True)
args = sys.argv[3:]
with sqlite3.connect(state / "cadence.sqlite3") as db:
    db.execute("CREATE TABLE IF NOT EXISTS schema_version(version INTEGER)")
    if not db.execute("SELECT * FROM schema_version").fetchall(): db.execute("INSERT INTO schema_version VALUES(18)")
    db.execute("CREATE TABLE IF NOT EXISTS agents(alias TEXT, provider TEXT, endpoint_kind TEXT)")
    if args[:2] == ["daemon", "start"]:
        if "candidate" in Path(sys.argv[0]).name: db.execute("UPDATE schema_version SET version=19")
        if "broken" in Path(sys.argv[0]).name: db.execute("DELETE FROM agents")
        print(json.dumps({"state": "started", "pid": 12345}))
    elif args[:2] == ["agent", "register"]:
        db.execute("INSERT INTO agents VALUES(?, 'inbox', 'inbox')", [args[2]])
        print('{}')
    else: print('{}')
'''


class RehearsalTests(unittest.TestCase):
    def run_fixture(self, broken=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            baseline = root / "baseline"
            candidate = root / ("broken-candidate" if broken else "candidate")
            for binary in (baseline, candidate):
                binary.write_text(FAKE)
                binary.chmod(0o755)
            out = root / "evidence"
            result = subprocess.run([
                sys.executable, str(SCRIPT), "--baseline", str(baseline),
                "--candidate", str(candidate), "--sha", "a" * 40, "--out", str(out),
            ], capture_output=True, text=True,
                env={"PATH": "/usr/bin:/bin", "GH_TOKEN": "must-not-reach-binary", "CADENCE_ALIAS": "forged"})
            receipt = out / "migration.json"
            return result, json.loads(receipt.read_text()) if receipt.exists() else None, (out / "commands.log").read_text()

    def test_previous_schema_migrates_and_backup_recovery_preserves_the_fixture(self):
        result, receipt, log = self.run_fixture()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((receipt["schema_from"], receipt["schema_to"]), (18, 19))
        self.assertEqual(receipt["preserved_agents"], 1)
        self.assertEqual(receipt["backup_recovery"], "passed")
        self.assertLess(log.index("rollout claim"), log.index("rollout backup"))
        self.assertIn("candidate daemon start", log)

    def test_lost_fixture_blocks_readiness_and_stops_the_owned_daemon(self):
        result, receipt, log = self.run_fixture(broken=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(receipt)
        self.assertIn("migration changed the fixture", result.stderr)
        self.assertIn("broken-candidate daemon stop", log)


if __name__ == "__main__":
    unittest.main()
