#!/usr/bin/env python3
"""Real protected-runtime image/profile recipe generator (no fake keys).

This is build *source*: it computes the actual pins for a staged, immutable
runtime image and writes a `native-oci-agent/1` profile. It never launches a
container, never deploys and never fabricates a key or digest: every value is
read from the staged tree or generated fresh into a new isolated directory.

Staged image contract (all under --image-root, root-owned):
  opt/cadence/node/bin/node                  pinned Node binary
  opt/cadence/node/observer.mjs              pinned observer extension
  opt/cadence/node/pi.mjs                    pinned Pi 1.4.0 CLI entry
  opt/cadence/node/cadence_seal.node         reviewed NAPI seal addon, staged by
                                             --stage-seal BEFORE image construction
  opt/cadence/material/**                    pinned material graph (Pi install, policy)
  opt/cadence/run/                           root-owned 0755 read-only socket-parent
  seccomp.json                               pinned seccomp document (copied into the profile dir)

The recipe launches the pinned Node directly with the approved `node_args`; the
observer loads the addon from its fixed path after exec. There is no separate
pre-exec seal executable.

Digest algorithms are the exact ones the owner/observer use:
  * file pin            : plain sha256 of the file bytes, "sha256:<hex>"
  * material tree pin   : sha256 over "cadence.runtime.material.v1\0" + for each
                          entry sorted by relative path: relpath + "\0" + line + "\n"
                          line = "dir:<mode octal>" | "file:<mode octal>:<size>:sha256:<hex>"
  * registry pin        : sha256 over "cadence.runtime.registry.v1\0" +
                          canonical JSON (sorted keys, no whitespace, integers only)
                          of the approved payload without its counter.

Requires: python3, openssl (Ed25519 PKCS#8), and for --stage-seal: a C compiler
plus the pinned Node headers.
"""

import argparse
import base64
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import uuid

MATERIAL_DOMAIN = b"cadence.runtime.material.v1\0"
SDK_DOMAIN = b"cadence.runtime.sdk.v1\0"
REGISTRY_DOMAIN = b"cadence.runtime.registry.v1\0"


def fail(message: str) -> "NoReturn":
    print(f"generate_profile: {message}", file=sys.stderr)
    raise SystemExit(1)


def sha256_bytes(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            h.update(chunk)
    return "sha256:" + h.hexdigest()


def canonical_seccomp(path: str) -> bytes:
    def unique_object(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                fail("seccomp document has duplicate keys")
            value[key] = item
        return value

    def nonfinite(_):
        fail("seccomp document has nonfinite numbers")

    try:
        with open(path, "r", encoding="utf-8") as handle:
            document = json.load(
                handle, object_pairs_hook=unique_object, parse_constant=nonfinite
            )
        if not isinstance(document, dict):
            fail("seccomp document must be a JSON object")
        return canonical(document).encode("utf-8")
    except (OSError, UnicodeError, ValueError):
        fail("seccomp document is not valid UTF-8 JSON")


def canonical(value) -> str:
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, int):
        if value < 0:
            fail("canonical JSON refuses negative integers")
        return str(value)
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, list):
        return "[" + ",".join(canonical(item) for item in value) + "]"
    if isinstance(value, dict):
        keys = sorted(value.keys())
        return "{" + ",".join(json.dumps(k, ensure_ascii=False) + ":" + canonical(value[k]) for k in keys) + "}"
    fail("unsupported canonical JSON value")


def tree_digest(root: str, domain: bytes) -> str:
    entries = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for name in dirnames + filenames:
            path = os.path.join(dirpath, name)
            meta = os.lstat(path)
            if stat.S_ISLNK(meta.st_mode):
                fail(f"tree contains a symlink: {path}")
            rel = os.path.relpath(path, root)
            mode = format(stat.S_IMODE(meta.st_mode), "o")
            if stat.S_ISDIR(meta.st_mode):
                entries.append((rel, f"dir:{mode}"))
            elif stat.S_ISREG(meta.st_mode):
                entries.append((rel, f"file:{mode}:{meta.st_size}:{sha256_file(path)}"))
            else:
                fail(f"tree contains a special file: {path}")
    entries.sort(key=lambda item: item[0])
    h = hashlib.sha256()
    h.update(domain)
    for rel, line in entries:
        h.update(rel.encode("utf-8"))
        h.update(b"\0")
        h.update(line.encode("utf-8"))
        h.update(b"\n")
    return "sha256:" + h.hexdigest()


def material_digest(root: str) -> str:
    entries = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for name in dirnames + filenames:
            path = os.path.join(dirpath, name)
            meta = os.lstat(path)
            if stat.S_ISLNK(meta.st_mode):
                fail(f"material tree contains a symlink: {path}")
            rel = os.path.relpath(path, root)
            mode = format(stat.S_IMODE(meta.st_mode), "o")
            if stat.S_ISDIR(meta.st_mode):
                entries.append((rel, f"dir:{mode}"))
            elif stat.S_ISREG(meta.st_mode):
                entries.append((rel, f"file:{mode}:{meta.st_size}:{sha256_file(path)}"))
            else:
                fail(f"material tree contains a special file: {path}")
    entries.sort(key=lambda item: item[0])
    h = hashlib.sha256()
    h.update(MATERIAL_DOMAIN)
    for rel, line in entries:
        h.update(rel.encode("utf-8"))
        h.update(b"\0")
        h.update(line.encode("utf-8"))
        h.update(b"\n")
    return "sha256:" + h.hexdigest()


def registry_digest(approved_payload: dict) -> str:
    return sha256_bytes(REGISTRY_DOMAIN + canonical(approved_payload).encode("utf-8"))


def require_file(path: str, label: str) -> str:
    if not os.path.isfile(path) or os.path.islink(path):
        fail(f"{label} must be a regular file: {path}")
    return path


def require_root_owned_tree(path: str) -> None:
    if os.geteuid() != 0:
        fail("profile generation must run as root so ownership/pins are real")
    current = os.path.abspath(path)
    while True:
        meta = os.stat(current)
        if meta.st_uid != 0 or (stat.S_IMODE(meta.st_mode) & 0o022):
            fail(f"path component is not root-owned/non-writable: {current}")
        parent = os.path.dirname(current)
        if parent == current:
            break
        current = parent


def require_root_executable(path: str, label: str) -> str:
    require_file(path, label)
    if not os.path.isabs(path):
        fail(f"{label} path must be absolute: {path}")
    meta = os.lstat(path)
    if meta.st_uid != 0:
        fail(f"{label} must be root-owned: {path}")
    if stat.S_IMODE(meta.st_mode) & 0o022:
        fail(f"{label} must not be group/other writable: {path}")
    require_root_owned_tree(os.path.dirname(path))
    return path


def generate_ed25519_key(key_path: str, label: str) -> tuple[str, str]:
    if os.path.exists(key_path):
        fail(f"{label} already exists — refuse to overwrite authority material")
    subprocess.run(
        [
            "openssl",
            "genpkey",
            "-algorithm",
            "ED25519",
            "-outform",
            "DER",
            "-out",
            key_path,
        ],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    os.chmod(key_path, 0o600)
    public = public_from_private(key_path)
    return key_path, public


def public_from_private(key_path: str) -> str:
    der = subprocess.run(
        ["openssl", "pkey", "-in", key_path, "-pubout", "-outform", "DER"],
        stdin=subprocess.DEVNULL,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    ).stdout
    prefix = bytes.fromhex("302a300506032b6570032100")
    if len(der) != len(prefix) + 32 or not der.startswith(prefix):
        fail("openssl did not return canonical Ed25519 SubjectPublicKeyInfo")
    raw = der[len(prefix):]
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def normalize_ed25519_key(source_path: str, key_path: str, label: str) -> tuple[str, str]:
    """Stage canonical PKCS#8 DER without changing the supplied authority."""
    source_path = require_root_private_key(source_path, label)
    public = public_from_private(source_path)
    try:
        fd = os.open(key_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except FileExistsError:
        fail(f"{label} already exists — refuse to overwrite authority material")
    complete = False
    try:
        with os.fdopen(fd, "wb") as output:
            subprocess.run(
                ["openssl", "pkey", "-in", source_path, "-outform", "DER"],
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.DEVNULL,
                check=True,
            )
            output.flush()
            os.fsync(output.fileno())
        require_root_private_key(key_path, label)
        if public_from_private(key_path) != public:
            fail(f"{label} public pin changed during normalization")
        complete = True
        return key_path, public
    finally:
        if not complete:
            os.unlink(key_path)


def generate_owner_key(key_path: str) -> tuple[str, str]:
    return generate_ed25519_key(key_path, "owner key")


def require_root_private_key(path: str, label: str) -> str:
    require_file(path, label)
    if not os.path.isabs(path):
        fail(f"{label} path must be absolute: {path}")
    meta = os.lstat(path)
    if meta.st_uid != 0:
        fail(f"{label} must be root-owned: {path}")
    if stat.S_IMODE(meta.st_mode) & 0o077:
        fail(f"{label} must be 0600/root-only: {path}")
    if meta.st_size == 0 or meta.st_size > 4096:
        fail(f"{label} size is outside the bound: {path}")
    require_root_owned_tree(os.path.dirname(path))
    return path


def build_seal_addon(image_root: str, headers: str) -> str:
    source = os.path.join(os.path.dirname(__file__), "..", "seal", "cadence_seal.c")
    source = require_file(os.path.abspath(source), "seal source")
    if not os.path.isdir(headers):
        fail(f"Node headers directory missing: {headers}")
    target = os.path.join(image_root, "opt", "cadence", "node", "cadence_seal.node")
    subprocess.run(
        [
            os.environ.get("CC", "cc"),
            "-O2",
            "-fPIC",
            "-shared",
            "-I",
            headers,
            source,
            "-o",
            target,
        ],
        check=True,
    )
    os.chmod(target, 0o755)
    return target


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image-root", required=True)
    parser.add_argument(
        "--stage-seal",
        action="store_true",
        help="build the reviewed seal addon into the staged image root (before docker build) and exit",
    )
    parser.add_argument("--node-headers", help="Node headers directory; required with --stage-seal")
    parser.add_argument("--out", help="fresh directory for profile/keys")
    parser.add_argument("--image-ref", help="repo/name@sha256:<64hex>")
    parser.add_argument("--workspace-id")
    parser.add_argument("--core-id")
    parser.add_argument("--lineage-reference")
    parser.add_argument("--database-epoch", type=int, default=1)
    parser.add_argument("--supervisor-uid", type=int)
    parser.add_argument("--supervisor-gid", type=int)
    parser.add_argument("--supervisor-exe")
    parser.add_argument("--installer-exe")
    parser.add_argument("--installer-generation")
    parser.add_argument("--owner-exe")
    parser.add_argument("--issuer-kid")
    parser.add_argument("--issuer-private-key", help="root-only Ed25519 PKCS#8 PEM/DER input; staged as fresh 0600 DER in --out")
    parser.add_argument("--seccomp")
    parser.add_argument("--approved-registry", help="qualification manifest JSON")
    parser.add_argument("--policy-digest")
    parser.add_argument("--manifest-id", default="protected-local-1")
    parser.add_argument("--manifest-revision", type=int, default=1)
    parser.add_argument("--uid-start", type=int)
    parser.add_argument("--uid-end", type=int)
    parser.add_argument("--operator-origin")
    parser.add_argument("--docker-socket", default="/run/docker.sock")
    parser.add_argument("--max-runtimes", type=int, default=2)
    args = parser.parse_args()

    require_root_owned_tree(args.image_root)
    image_root = os.path.abspath(args.image_root)
    if args.stage_seal:
        if not args.node_headers:
            fail("--stage-seal requires --node-headers")
        target = build_seal_addon(image_root, args.node_headers)
        print(json.dumps({
            "v": 1,
            "seal_addon": target,
            "seal_digest": sha256_file(target),
        }, indent=2))
        return

    required = {
        "out": args.out,
        "image-ref": args.image_ref,
        "workspace-id": args.workspace_id,
        "core-id": args.core_id,
        "lineage-reference": args.lineage_reference,
        "supervisor-uid": args.supervisor_uid,
        "supervisor-gid": args.supervisor_gid,
        "supervisor-exe": args.supervisor_exe,
        "installer-exe": args.installer_exe,
        "installer-generation": args.installer_generation,
        "owner-exe": args.owner_exe,
        "issuer-kid": args.issuer_kid,
        "seccomp": args.seccomp,
        "approved-registry": args.approved_registry,
        "policy-digest": args.policy_digest,
        "uid-start": args.uid_start,
        "uid-end": args.uid_end,
        "operator-origin": args.operator_origin,
    }
    missing = [flag for flag, value in required.items() if value is None or value == ""]
    if missing:
        parser.error("missing required arguments: " + ", ".join("--" + flag for flag in missing))

    if os.path.exists(args.out) and os.listdir(args.out):
        fail("--out must be a fresh/empty directory (never overwrite authority)")
    os.makedirs(args.out, mode=0o755, exist_ok=True)
    os.chmod(args.out, 0o755)

    node = require_file(os.path.join(image_root, "opt/cadence/node/bin/node"), "Node binary")
    observer = require_file(os.path.join(image_root, "opt/cadence/node/observer.mjs"), "observer")
    pi_entry = require_file(os.path.join(image_root, "opt/cadence/node/pi.mjs"), "Pi entry")
    # The recipe stages this addon into the rootfs BEFORE `docker build`; pins
    # below are computed from those exact bytes so the image and profile agree.
    addon = require_file(
        os.path.join(image_root, "opt/cadence/node/cadence_seal.node"),
        "reviewed seal addon",
    )
    material_root = os.path.join(image_root, "opt/cadence/material")
    if not os.path.isdir(material_root):
        fail("staged material root is missing")
    guest_session = require_file(
        os.path.join(material_root, "guest", "session.json"), "guest session config"
    )
    require_file(os.path.join(material_root, "model-bridge.mjs"), "guest keyless provider bridge")
    sdk_graph = os.path.join(image_root, "opt/cadence/node/node_modules")
    if not os.path.isdir(sdk_graph):
        fail("pinned Pi SDK graph is missing (opt/cadence/node/node_modules)")
    sdk_digest = tree_digest(sdk_graph, SDK_DOMAIN)
    if not os.path.isfile(guest_session):
        fail("guest session config is not a regular file")
    seccomp = require_file(args.seccomp, "seccomp document")
    seccomp_document = canonical_seccomp(seccomp)
    supervisor = require_root_executable(args.supervisor_exe, "supervisor executable")
    installer = require_root_executable(args.installer_exe, "installer executable")
    owner_exe = require_root_executable(args.owner_exe, "owner executable")

    with open(args.approved_registry, "r", encoding="utf-8") as handle:
        registry_input = json.load(handle)
    approved = registry_input.get("approved")
    if not isinstance(approved, list) or not approved:
        fail("approved registry manifest needs a non-empty 'approved' tool list")
    for tool in approved:
        for field in ("name", "schema_digest", "implementation_digest"):
            if field not in tool:
                fail(f"approved tool missing {field}")
    projection_tools = [
        {
            "name": tool["name"],
            "schema_digest": tool["schema_digest"],
            "implementation_digest": tool["implementation_digest"],
        }
        for tool in sorted(approved, key=lambda t: t["name"])
    ]
    registry_pin = registry_digest({
        "v": 1,
        "pi_version": "1.4.0",
        "tools": projection_tools,
    })

    owner_key, owner_public = generate_owner_key(os.path.join(args.out, "owner.key"))
    os.chmod(owner_key, 0o600)
    if args.issuer_private_key:
        issuer_key, issuer_public = normalize_ed25519_key(
            args.issuer_private_key,
            os.path.abspath(os.path.join(args.out, "issuer.key")),
            "issuer key",
        )
    else:
        issuer_key, issuer_public = generate_ed25519_key(
            os.path.abspath(os.path.join(args.out, "issuer.key")), "issuer key"
        )

    if sha256_file(installer) != sha256_file(owner_exe):
        fail(
            "--installer-exe must be the same pinned owner binary bytes "
            "(the fixed --owner-launched-install mode)"
        )

    profile_dir = args.out
    state_dir = os.path.join(profile_dir, "state")
    run_dir = os.path.join(profile_dir, "run")
    os.makedirs(state_dir, mode=0o700, exist_ok=True)
    os.makedirs(run_dir, mode=0o755, exist_ok=True)

    profile = {
        "v": 1,
        "deployment_class": "native-oci-agent/1",
        "workspace_id": args.workspace_id,
        "core_id": args.core_id,
        "lineage_reference": args.lineage_reference,
        "database_epoch": args.database_epoch,
        "owner": {
            "exe_digest": sha256_file(owner_exe),
            "public_key": owner_public,
            "private_key_file": owner_key,
        },
        "supervisor": {
            "uid": args.supervisor_uid,
            "gid": args.supervisor_gid,
            "exe_digest": sha256_file(supervisor),
        },
        "installer": {
            "exe_digest": sha256_file(installer),
            "generation": args.installer_generation,
            "path": os.path.abspath(installer),
            "grant_issuer": {
                "kid": args.issuer_kid,
                "private_key_file": issuer_key,
            },
        },
        "issuer_keys": [{"kid": args.issuer_kid, "x": issuer_public}],
        "image": {
            "ref": args.image_ref,
            "pins": {
                "source": registry_input.get("source", "0" * 40),
                "helper": registry_input.get("helper", sha256_file(addon)[7:]),
                "node": sha256_file(node)[7:],
                "piGraph": sdk_digest[7:],
                "policy": args.policy_digest[7:],
            },
        },
        "runtime": {
            "node_path": "/opt/cadence/node/bin/node",
            "observer_path": "/opt/cadence/node/observer.mjs",
            "seal_digest": sha256_file(addon),
            "node_digest": sha256_file(node),
            "observer_digest": sha256_file(observer),
            "pi_digest": sha256_file(pi_entry),
            "material_root": "/opt/cadence/material",
            "material_digest": material_digest(material_root),
            "env": [
                "PATH=/opt/cadence/node/bin:/usr/bin:/bin",
                "HOSTNAME=cadence-protected",
                "HOME=/home/agent",
                "PI_CODING_AGENT_DIR=/home/agent/pi",
            ],
            "node_args": [
                "--disable-sigusr1",
                "--no-warnings",
                "/opt/cadence/node/pi.mjs",
                "--no-extensions",
                "-e",
                "/opt/cadence/node/observer.mjs",
            ],
            "tmpfs_bytes": 33554432,
        },
        "seccomp": {"path": os.path.join(profile_dir, "seccomp.json"), "digest": sha256_bytes(seccomp_document)},
        "uids": {"start": args.uid_start, "end": args.uid_end},
        "operator_origin": args.operator_origin,
        "docker": {"socket": args.docker_socket},
        "max_runtimes": args.max_runtimes,
        "registry": {
            "manifest_id": args.manifest_id,
            "manifest_revision": args.manifest_revision,
            "policy_digest": args.policy_digest,
            "registry_digest": registry_pin,
            "approved": approved,
        },
        "paths": {
            "state_dir": state_dir,
            "run_dir": run_dir,
            "control_socket": os.path.join(run_dir, "control.sock"),
        },
    }

    with open(os.path.join(profile_dir, "seccomp.json"), "wb") as handle:
        handle.write(seccomp_document)
        handle.flush()
        os.fsync(handle.fileno())
    os.chmod(os.path.join(profile_dir, "seccomp.json"), 0o644)
    profile_path = os.path.join(profile_dir, "profile.json")
    temporary = profile_path + ".tmp"
    with open(temporary, "w", encoding="utf-8") as handle:
        json.dump(profile, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.chmod(temporary, 0o644)
    os.rename(temporary, profile_path)
    os.chmod(profile_path, 0o644)

    print(json.dumps({
        "v": 1,
        "profile": profile_path,
        "state_dir": state_dir,
        "run_dir": run_dir,
        "owner_key": owner_key,
        "owner_public_key": owner_public,
        "issuer_key": issuer_key,
        "issuer_public_key": issuer_public,
        "registry_digest": profile["registry"]["registry_digest"],
        "material_digest": profile["runtime"]["material_digest"],
        "note": "staged image was not built/launched; pins are computed from this tree",
    }, indent=2))


if __name__ == "__main__":
    main()
