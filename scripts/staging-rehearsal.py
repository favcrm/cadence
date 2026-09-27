#!/usr/bin/env python3
"""Rehearse a previous release -> candidate -> backup recovery in /tmp.

Both binaries must already have been verified by delivery-candidate.py.
No live database is read. The fixture contains only an inbox agent, so
no provider or external side effect can be resumed.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="cad-stage-", dir="/tmp"))
    env = {"PATH": "/usr/bin:/bin", "HOME": str(root / "home"),
           "XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
           "XDG_CACHE_HOME": str(root / "cache"), "XDG_STATE_HOME": str(root / "xdg-state"),
           "CADENCE_PM_DIR": str(root / "pm"), "TMPDIR": str(root / "tmp")}
    for path in env.values():
        if path.startswith(str(root)):
            Path(path).mkdir(parents=True, exist_ok=True)
    state, recovery = root / "state", root / "recovery"
    owned = []

    def cli(binary, directory, *argv):
        result = subprocess.run([str(binary), "--state-dir", str(directory), *argv],
                                env=env, cwd=root, capture_output=True, text=True, timeout=60)
        with (args.out / "commands.log").open("a") as log:
            log.write(f"{binary.name} {' '.join(argv)}: exit {result.returncode}\n{result.stdout}{result.stderr}\n")
        result.check_returncode()
        return json.loads(result.stdout)

    def start(binary, directory):
        result = cli(binary, directory, "daemon", "start", "--as", "operator:staging")
        if result.get("state") != "started":
            raise RuntimeError("rehearsal did not start its own daemon")
        owned.append((binary, directory))

    def stop(binary, directory):
        cli(binary, directory, "daemon", "stop")
        owned.remove((binary, directory))

    def snapshot(directory, dest):
        with sqlite3.connect(directory / "cadence.sqlite3") as source:
            assert source.execute("PRAGMA quick_check").fetchall() == [("ok",)]
            schema = source.execute("SELECT version FROM schema_version").fetchone()[0]
            rows = source.execute("SELECT id, alias, provider FROM agents ORDER BY id").fetchall()
            if dest:
                with sqlite3.connect(dest) as backup:
                    source.backup(backup)
            return schema, rows

    try:
        start(args.baseline, state)
        cli(args.baseline, state, "agent", "register", "staging-fixture", "--provider", "inbox")
        stop(args.baseline, state)
        cli(args.baseline, state, "rollout", "claim", "--as", "operator:staging",
            "--reason", "isolated migration rehearsal", "--target", args.sha)
        backup = root / "before.sqlite3"
        before_schema, before_rows = snapshot(state, backup)
        cli(args.baseline, state, "rollout", "backup", "--as", "operator:staging", "--path", str(backup))
        start(args.candidate, state)
        after_schema, after_rows = snapshot(state, None)
        if after_rows != before_rows:
            raise RuntimeError("migration changed the fixture agent identities")
        stop(args.candidate, state)
        # Recovery restores the consistent pre-migration backup before
        # starting the old binary, including when schemas cannot go back.
        recovery.mkdir()
        shutil.copyfile(backup, recovery / "cadence.sqlite3")
        start(args.baseline, recovery)
        recovery_schema, recovery_rows = snapshot(recovery, None)
        if (recovery_schema, recovery_rows) != (before_schema, before_rows):
            raise RuntimeError("backup recovery did not preserve the original fixture")
        stop(args.baseline, recovery)
        (args.out / "migration.json").write_text(json.dumps({
            "candidate_sha": args.sha, "schema_from": before_schema,
            "schema_to": after_schema, "preserved_agents": len(before_rows),
            "backup_recovery": "passed", "fixture": "isolated inbox agent",
            "limitations": "No production snapshot, live turns or real-provider continuity tested",
        }, indent=2) + "\n")
    finally:
        for binary, directory in list(owned):
            try:
                stop(binary, directory)
            except (OSError, subprocess.SubprocessError):
                # Only our detached daemon, identified by its isolated
                # state and startup receipt, was ever targeted.
                pass
        for directory in (state, recovery):
            if (directory / "daemon.log").exists():
                shutil.copyfile(directory / "daemon.log", args.out / f"{directory.name}-daemon.log")
        # Keep failure state for runner teardown; never remove a directory
        # if an owned daemon failed its graceful stop.
        if not owned:
            shutil.rmtree(root)


if __name__ == "__main__":
    main()
