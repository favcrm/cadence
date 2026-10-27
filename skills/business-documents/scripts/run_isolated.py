#!/usr/bin/env python3
"""Run business document intake/rendering in the pinned offline container."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

IMAGE = "cadence-business-files:cad-1393-dev1"
PINNED_IMAGE_ID = "sha256:06ea75fdd2b41762d37dcf68a101703be3962976e6be1aebd2247503431cee64"
IMAGE_LABELS = "{{.Id}}|{{index .Config.Labels \"org.cadence.ticket\"}}|{{index .Config.Labels \"org.cadence.workflow\"}}"
SCRIPT_DIR = Path(__file__).resolve().parent


class Blocked(Exception):
    pass


def checked_input(path: Path) -> Path:
    raw = path.expanduser().absolute()
    if raw.is_symlink() or any(parent.is_symlink() for parent in raw.parents):
        raise Blocked(f"input path must not contain symlinks: {path}")
    try:
        resolved = raw.resolve(strict=True)
    except (OSError, RuntimeError) as exc:
        raise Blocked(f"input does not resolve: {path} ({exc})") from exc
    if not resolved.is_file():
        raise Blocked(f"input is not a regular file: {path}")
    if any(char in str(resolved) for char in ",:\n\r"):
        raise Blocked("input path contains a Docker mount delimiter")
    if not re.fullmatch(r"[A-Za-z0-9._-]+", resolved.name):
        raise Blocked("input filename must use only letters, digits, dot, underscore or hyphen")
    return resolved


def checked_output(path: Path, inputs: list[Path]) -> Path:
    raw = path.expanduser().absolute()
    if raw.is_symlink() or any(parent.is_symlink() for parent in raw.parents):
        raise Blocked("output directory path must not contain symlinks")
    # Require an existing, task-owned directory; do not create through attacker-controlled links.
    try:
        resolved = raw.resolve(strict=True)
    except (OSError, RuntimeError) as exc:
        raise Blocked(f"output directory must already exist ({exc})") from exc
    if not resolved.is_dir() or any(char in str(resolved) for char in ",:\n\r"):
        raise Blocked("output directory is not a safe Docker bind path")
    if os.getuid() == 0 or os.getgid() == 0:
        raise Blocked("isolated workflow must be started by a non-root user/group")
    info = resolved.stat()
    if info.st_uid != os.getuid() or not os.access(resolved, os.W_OK | os.X_OK):
        raise Blocked("output directory must be writable and owned by the current unprivileged user")
    for item in inputs:
        if item == resolved or resolved in item.parents or resolved == item.parent:
            raise Blocked("output directory must be separate from every input")
    return resolved


def image_id() -> str:
    try:
        result = subprocess.run(
            ["docker", "image", "inspect", "--format", IMAGE_LABELS, IMAGE],
            check=True, capture_output=True, text=True, timeout=15,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise Blocked(f"cannot inspect the required pinned local image {IMAGE}: {exc}") from exc
    pieces = result.stdout.strip().split("|")
    if len(pieces) != 3 or pieces[0] != PINNED_IMAGE_ID or pieces[1:] != ["CAD-1345", "business-documents-v1"]:
        raise Blocked("container image is missing the expected CAD-1345 workflow labels")
    return pieces[0]


def execute(command: str, inputs: list[Path], output: Path, sample: bool = False) -> int:
    image = image_id()
    docker = [
        "docker", "run", "--rm", "--network", "none", "--read-only", "--user", f"{os.getuid()}:{os.getgid()}",
        "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--pids-limit", "64",
        "--memory", "1g", "--cpus", "1", "--ulimit", "cpu=90:90",
        "--ulimit", "fsize=104857600:104857600", "--ulimit", "nofile=128:128",
        "--tmpfs", "/tmp:rw,nosuid,nodev,size=128m,mode=1777", "--env", "HOME=/tmp",
        "--workdir", "/tmp", "--mount", f"type=bind,src={SCRIPT_DIR},dst=/app,readonly",
    ]
    mapped: list[str] = []
    used_names: set[str] = set()
    for index, path in enumerate(inputs):
        container_name = path.name if path.name not in used_names else f"{index}-{path.name}"
        used_names.add(container_name)
        name = f"/inputs/{container_name}"
        docker.extend(["--mount", f"type=bind,src={path},dst={name},readonly"])
        mapped.append(name)
    docker.extend(["--mount", f"type=bind,src={output},dst=/outputs", image, "/opt/business-venv/bin/python", f"/app/{command}.py"])
    if command == "intake":
        docker.extend([mapped[0], "--output-dir", "/outputs"])
    elif command == "render_template":
        docker.extend(["--template", mapped[0], "--context", mapped[1], "--output-dir", "/outputs"])
        if sample:
            docker.append("--sample")
    else:
        docker.extend(["--template", mapped[0], "--brief", mapped[1]])
        if len(mapped) == 3:
            docker.extend(["--rates", mapped[2]])
        docker.extend(["--output-dir", "/outputs"])
    try:
        result = subprocess.run(docker, check=False, timeout=105)
    except subprocess.TimeoutExpired:
        print("Isolated command refused: exceeded 105-second wall-clock bound", file=sys.stderr)
        return 124
    except OSError as exc:
        print(f"Isolated command refused: {exc}", file=sys.stderr)
        return 125
    return result.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    intake = sub.add_parser("intake")
    intake.add_argument("source", type=Path)
    intake.add_argument("--output-dir", required=True, type=Path)
    render = sub.add_parser("render")
    render.add_argument("--template", required=True, type=Path)
    render.add_argument("--brief", required=True, type=Path)
    render.add_argument("--rates", type=Path)
    render.add_argument("--output-dir", required=True, type=Path)
    template = sub.add_parser("render-template")
    template.add_argument("--template", required=True, type=Path)
    template.add_argument("--context", required=True, type=Path)
    template.add_argument("--output-dir", required=True, type=Path)
    template.add_argument("--sample", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "intake":
            inputs = [checked_input(args.source)]
            output = checked_output(args.output_dir, inputs)
            return execute("intake", inputs, output)
        if args.command == "render-template":
            inputs = [checked_input(args.template), checked_input(args.context)]
            output = checked_output(args.output_dir, inputs)
            return execute("render_template", inputs, output, sample=args.sample)
        inputs = [checked_input(args.template), checked_input(args.brief)]
        if args.rates:
            inputs.append(checked_input(args.rates))
        output = checked_output(args.output_dir, inputs)
        return execute("render", inputs, output)
    except Blocked as exc:
        print(f"Isolated command refused: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
