#!/usr/bin/env python3
"""CAD-778: frozen run snapshot digest must survive the store round trip.

Runs a no-dev-deps CLI binary (production JSON parser) through freeze →
approve with the exact pilot floats from the frozen row
(`1790647563.6348941` / `1790648372.680178`). Pre-fix the approve fails
with `snapshot receipt is corrupt`; post-fix the full flow passes while
forged digests, tampered snapshots and changed assignments stay refused.

All state is synthetic and isolated. No provider credentials, production
state, board ports or outward effects are used.
"""
import argparse
import json
import sqlite3
import subprocess
import tempfile
from pathlib import Path
import shutil

CREATED_WRITER = 1790647563.6348941
CREATED_REVIEWER = 1790648372.680178


def check(binary):
    root = Path(tempfile.mkdtemp(prefix="c778-"))
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"}
    for variable, directory in {
        "HOME": "home", "XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "data",
        "XDG_STATE_HOME": "xdg-state", "XDG_CACHE_HOME": "cache",
        "TMPDIR": "tmp", "CADENCE_STATE_DIR": "state", "CADENCE_PM_DIR": "pm",
    }.items():
        path = root / directory
        path.mkdir(mode=0o700)
        env[variable] = str(path)
    env["CADENCE_PROFILE"] = "sandbox:cad778-proof"
    state = root / "state"
    started = False

    def cli(*args, failure=None):
        result = subprocess.run(
            [str(binary), "--state-dir", str(state), *args],
            cwd=root, env=env, text=True, capture_output=True, timeout=60,
        )
        if failure:
            assert result.returncode != 0, f"expected refusal, got: {result.stdout}"
            assert failure in result.stderr, result.stderr
            return None
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout) if result.stdout.strip().startswith("{") else None

    try:
        cli("issue", "init")
        cli("daemon", "start")
        started = True
        cli("agent", "register", "lead", "--provider", "inbox",
            "--endpoint", "inbox", "--role", "pm")
        for alias in ["writer", "reviewer"]:
            cli("agent", "register", alias, "--provider", "fake",
                "--endpoint", "fake", "--role", "worker",
                "--param", "upstream=lead", "--cwd", str(root))
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.executemany(
                "UPDATE agents SET created=?, updated=? WHERE alias=?",
                [(CREATED_WRITER, CREATED_WRITER, "writer"),
                 (CREATED_REVIEWER, CREATED_REVIEWER, "reviewer")],
            )
        source = Path(__file__).resolve().parents[2] / "workspace-apps/social-content"
        installation = cli("app", "catalog", "install", str(source))
        install_id = installation["install_id"]
        cli("app", "catalog", "approve", install_id, "--digest", installation["digest"])
        inputs = json.dumps({"subject": "Snapshot digest proof",
                             "source": "Synthetic source facts.",
                             "writer": "writer", "reviewer": "reviewer"})
        inputs_file = root / "inputs.json"
        inputs_file.write_text(inputs)
        run = cli("app", "run", "create", install_id, "--workflow", "instagram",
                  "--inputs", str(inputs_file), "--request-id", "cad778-proof",
                  "--owner-pm", "lead")
        run_id, digest = run["id"], run["snapshot_digest"]
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            raw = connection.execute(
                "SELECT snapshot FROM app_runs WHERE id=?", [run_id]).fetchone()[0]
        created = run["snapshot"]["assignments"]["s1"]["identity"]["created"]
        assert created.hex() == CREATED_WRITER.hex(), (
            f"parser changed the frozen timestamp: {created.hex()}")
        # Guard stays strict: forged digest refused.
        cli("app", "run", "approve", run_id, "--digest", "sha256:forged",
            failure="snapshot digest")
        # Guard stays strict: tampered stored text refused as corrupt.
        forged = json.loads(raw)
        forged["workflow"]["steps"][0]["instruction"] = "Forged instruction"
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.execute("UPDATE app_runs SET snapshot=? WHERE id=?",
                               [json.dumps(forged), run_id])
        cli("app", "run", "approve", run_id, "--digest", digest,
            failure="snapshot receipt is corrupt")
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.execute("UPDATE app_runs SET snapshot=? WHERE id=?",
                               [raw, run_id])
        # The reported bug: exact frozen digest must approve.
        cli("app", "run", "approve", run_id, "--digest", digest)
        # Changed assignment still refused at dispatch.
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.execute("UPDATE agents SET created=created+1 WHERE alias='writer'")
        cli("app", "run", "dispatch", run_id, failure="assignment changed")
        with sqlite3.connect(state / "cadence.sqlite3") as connection:
            connection.execute("UPDATE agents SET created=? WHERE alias='writer'",
                               [CREATED_WRITER])
        dispatched = cli("app", "run", "dispatch", run_id)
        assert dispatched["steps"][0]["message_id"], "valid frozen plan was not dispatched"
        assert dispatched["snapshot_digest"] == digest, "dispatch rewrote the receipt"
        print("PASS: frozen snapshot digest survives the store round trip; "
              "tamper and identity changes refused; valid plan dispatched")
    finally:
        if started:
            cli("daemon", "stop")
        shutil.rmtree(root)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    check(parser.parse_args().binary.resolve())
