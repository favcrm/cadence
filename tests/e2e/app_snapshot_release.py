#!/usr/bin/env python3
"""Exercise frozen receipts using a normal release CLI, without dev features.

All state and synthetic fake endpoints belong to this process. No provider
credentials, production state, board ports or outward effects are used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile


def check(binary):
    root = Path(tempfile.mkdtemp(prefix="c701-"))
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"}
    for variable, directory in {
        "HOME": "home", "XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "data",
        "XDG_STATE_HOME": "xdg-state", "XDG_CACHE_HOME": "cache",
        "TMPDIR": "tmp", "CADENCE_STATE_DIR": "state", "CADENCE_PM_DIR": "pm",
    }.items():
        path = root / directory
        path.mkdir(mode=0o700)
        env[variable] = str(path)
    env["CADENCE_PROFILE"] = "sandbox:cad701-release"
    state = root / "state"
    started = False

    def cli(*args, failure=None):
        result = subprocess.run(
            [str(binary), "--state-dir", str(state), *args],
            cwd=root, env=env, text=True, capture_output=True, timeout=30,
        )
        if failure:
            assert result.returncode != 0, "forged material was accepted"
            assert failure in result.stderr, result.stderr
            return
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout) if result.stdout.strip().startswith("{") else None

    try:
        cli("issue", "init")
        cli("daemon", "start")
        started = True
        cli("agent", "register", "lead", "--provider", "inbox", "--endpoint", "inbox", "--role", "pm")
        for alias in ["writer", "reviewer"]:
            cli("agent", "register", alias, "--provider", "fake", "--endpoint", "fake",
                "--role", "worker", "--param", "upstream=lead", "--cwd", str(root))
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.executemany("UPDATE agents SET created=? WHERE alias=?", [
                (1790532111.9785945, "writer"), (1790532113.5222745, "reviewer"),
            ])
        source = Path(__file__).resolve().parents[2] / "workspace-apps/social-content"
        installation = cli("app", "catalog", "install", str(source))
        install_id = installation["install_id"]
        cli("app", "catalog", "approve", install_id, "--digest", installation["digest"])
        inputs = json.dumps({"subject": "Release parser proof", "source": "Synthetic source facts.",
                             "writer": "writer", "reviewer": "reviewer"})
        inputs_file = root / "inputs.json"
        inputs_file.write_text(inputs)
        run = cli("app", "run", "create", install_id, "--workflow", "instagram",
                  "--inputs", str(inputs_file), "--request-id", "release-parser-proof", "--owner-pm", "lead")
        run_id, digest = run["id"], run["snapshot_digest"]
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            raw = connection.execute("SELECT snapshot FROM app_runs WHERE id=?", [run_id]).fetchone()[0]
        assert "sha256:" + hashlib.sha256(("cadence-app-material-v1\n" + raw).encode()).hexdigest() == digest
        created = run["snapshot"]["assignments"]["s1"]["identity"]["created"]
        assert created.hex() == (1790532111.9785945).hex(), "release parser changed the frozen timestamp"
        cli("app", "run", "approve", run_id, "--digest", "sha256:forged", failure="snapshot digest")
        forged = json.loads(raw)
        forged["workflow"]["steps"][0]["instruction"] = "Forged instruction"
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.execute("UPDATE app_runs SET snapshot=? WHERE id=?", [json.dumps(forged), run_id])
        cli("app", "run", "approve", run_id, "--digest", digest, failure="snapshot receipt is corrupt")
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.execute("UPDATE app_runs SET snapshot=? WHERE id=?", [raw, run_id])
        cli("app", "run", "approve", run_id, "--digest", digest)
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.execute("UPDATE agents SET created=created+1 WHERE alias='writer'")
        cli("app", "run", "dispatch", run_id, failure="assignment changed")
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.execute("UPDATE agents SET created=? WHERE alias='writer'", [1790532111.9785945])
        dispatched = cli("app", "run", "dispatch", run_id)
        assert dispatched["steps"][0]["message_id"], "valid frozen plan was not dispatched"
        assert dispatched["snapshot_digest"] == digest, "dispatch rewrote the historical receipt"
        print("PASS: release CLI preserves frozen fractional timestamps; tamper and identity changes refused; valid plan dispatched")
    finally:
        if started:
            cli("daemon", "stop")
        shutil.rmtree(root)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    check(parser.parse_args().binary.resolve())
