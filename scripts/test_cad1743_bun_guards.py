#!/usr/bin/env python3
"""Independent Spec/security reviewer acceptance for CAD-1743.

Authored by the independent openai-codex/gpt-6.1-sol task reviewer, not the
migration implementer; no registered/native reviewer identity is claimed.
The implementer must not edit or weaken this check.

Exercise the real enqueue evaluator with its existing offline API fixture,
the real clean-install PATH guard, and the exact publisher Python validator.
Only temporary fixture files are written; no Bun installation, Rust build,
backend, daemon, network access, publication or operator approval is used.
The enqueue fixture is not native authorization or registered-review evidence.
"""
from pathlib import Path
import hashlib
import importlib.util
import io
import json
import subprocess
import sys
import tarfile
import tempfile
import textwrap


REPO = Path(__file__).resolve().parents[1]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def check_operator_refusal():
    sys.path.insert(0, str(REPO / "scripts"))
    from test_enqueue_scope_approval import Case, SCRIPT, enq

    require(Path(SCRIPT).resolve() == REPO / "scripts/enqueue-reviewed",
            "acceptance must exercise the actual repository enqueue guard")
    risk_text = (REPO / enq.RISK_PATHS_FILE).read_text()
    for path in ("bun.lock", "app-toolchain/bun.lock", "apps/cad1743/bun.lock"):
        require(enq.is_human_path(path, enq.risk_globs(risk_text)),
                f"{path} is not human-class")
        triggers, unmapped = enq.path_triggers([path], risk_text)
        require(triggers is not None and 4 in triggers and not unmapped,
                f"{path} must map to supply-chain trigger 4: {triggers}, {unmapped}")

    with tempfile.TemporaryDirectory(prefix="cad1743-enqueue-") as directory:
        case = Case(directory)
        case.paths = ["apps/cad1743/readme.md"]
        case.store = []
        case.note_text = case.note_text.replace(
            "Risk: human (4, 7) — scripts", "Risk: auto")
        original_note = case.note

        def auto_note():
            note = original_note()
            note["risk"] = "auto"
            return note

        case.note = auto_note
        reasons, report = case.evaluate()
        require(not reasons, f"ordinary-path positive control failed: {reasons}, {report}")
        # Only the changed path changes; no verdict Risk line or unrelated
        # human-class path may accidentally supply the approval requirement.
        case.paths = ["apps/cad1743/bun.lock"]
        reasons, report = case.evaluate()
        require(any("no operator approval recorded" in reason for reason in reasons),
                f"Bun-lock diff was not refused without approval: {reasons}, {report}")


def check_bun_path_refusal():
    spec = importlib.util.spec_from_file_location(
        "cad1743_clean", REPO / "scripts/clean-install.py")
    clean = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(clean)
    with tempfile.TemporaryDirectory(prefix="cad1743-path-") as directory:
        private = Path(directory)
        restricted, linked = clean.restricted_path(private)
        require("bun" not in linked, "positive control exposed Bun")
        require(restricted == str(private / "bin"), "unexpected restricted PATH")
        injected = private / "bin/bun"
        injected.write_text("#!/bin/sh\nexit 0\n")
        injected.chmod(0o755)
        try:
            clean.restricted_path(private)
        except ValueError as error:
            require("forbidden tool bun leaked into restricted PATH" in str(error),
                    f"injected Bun failed for an unrelated reason: {error}")
        else:
            raise RuntimeError("injected Bun was accepted")


def check_publisher_refusals():
    lines = (REPO / ".github/workflows/ci.yml").read_text().splitlines()
    marker = "python3 - \"$asset\" \"$TAG\" \"$SOURCE_SHA\" ../src/ui/package.json <<" + chr(39) + "PY" + chr(39)
    starts = [i for i, line in enumerate(lines) if line.strip() == marker]
    require(len(starts) == 1, f"expected one actual publisher validator: {starts}")
    start = starts[0] + 1
    ends = [i for i in range(start, len(lines)) if lines[i].strip() == "PY"]
    require(bool(ends), "publisher validator heredoc is unterminated")
    guard = textwrap.dedent("\n".join(lines[start:ends[0]]))
    ui = json.loads((REPO / "ui/package.json").read_text())
    require(ui["packageManager"] == "bun@1.4.4", "UI exact Bun pin changed")
    tag, sha = "v0.0.0", "a" * 40
    pin_names = ("react", "react-dom", "vite", "@vitejs/plugin-react")
    source_pins = {**ui["dependencies"], **ui["devDependencies"]}
    pins = {name: source_pins[name] for name in pin_names}
    receiver_names = ("README.md", "package.json", "src/index.mjs",
                      "src/receiver.mjs", "src/refusal.mjs", "types/index.d.ts")
    receiver = {name: (REPO / "ui/app-kit" / name).read_bytes()
                for name in receiver_names}
    package = {
        "name": "cadence-app-toolchain", "private": True,
        "version": tag[1:], "type": "module", "packageManager": "bun@1.4.4",
        "scripts": {"dev": "vite", "build": "vite build"},
        "dependencies": {name: pins[name] for name in pin_names[:2]},
        "devDependencies": {name: pins[name] for name in pin_names[2:]},
    }
    metadata = {
        "schema": "cadence-app-toolchain/1", "tag": tag, "source_sha": sha,
        "package_manager": "bun@1.4.4", "pins": pins,
        "app_kit": {
            "version": json.loads(receiver["package.json"])["version"],
            "files": {name: hashlib.sha256(data).hexdigest()
                      for name, data in receiver.items()},
        },
    }

    def encode(value):
        return json.dumps(value).encode()

    members = {
        "LICENSE": b"fixture", "README.md": b"fixture",
        "frame-kit/frame.html": b"fixture", "frame-kit/kit.css": b"fixture",
        "frame-kit/metadata.json": b"{}", "package.json": encode(package),
        "toolchain.json": encode(metadata),
        "bun.lock": (REPO / "app-toolchain/bun.lock").read_bytes(),
        **{"app-kit/" + name: data for name, data in receiver.items()},
    }
    with tempfile.TemporaryDirectory(prefix="cad1743-publisher-") as directory:
        archive = Path(directory) / "toolchain.tar.gz"

        def validate(contents):
            with tarfile.open(archive, "w:gz") as tar:
                for name, data in contents.items():
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    tar.addfile(entry, io.BytesIO(data))
            return subprocess.run(
                [sys.executable, "-c", guard, str(archive), tag, sha,
                 str(REPO / "ui/package.json")],
                capture_output=True, text=True, timeout=20)

        result = validate(members)
        require(result.returncode == 0,
                f"valid publisher positive control failed: {result.stdout}, {result.stderr}")
        old_lock = dict(members)
        old_lock["pnpm-lock.yaml"] = old_lock.pop("bun.lock")
        result = validate(old_lock)
        require(result.returncode != 0 and "unexpected member list or file type" in result.stderr,
                f"old lock member was not specifically refused: {result.stderr}")
        old_manager = dict(members)
        old_manager["package.json"] = encode({**package, "packageManager": "pnpm@12.2.1"})
        result = validate(old_manager)
        require(result.returncode != 0 and "package manifest differs" in result.stderr,
                f"old package manager was not specifically refused: {result.stderr}")
        old_metadata = dict(members)
        old_metadata["toolchain.json"] = encode({**metadata, "package_manager": "pnpm@12.2.1"})
        result = validate(old_metadata)
        require(result.returncode != 0 and "package manager, pins or app-kit do not match" in result.stderr,
                f"old metadata manager was not specifically refused: {result.stderr}")


def main():
    check_operator_refusal()
    check_bun_path_refusal()
    check_publisher_refusals()
    print("PASS: independent CAD-1743 Bun guard acceptance; operator approval, "
          "injected Bun, old lock member and package-manager mismatches refused")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.TimeoutExpired) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
