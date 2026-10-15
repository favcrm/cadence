#!/usr/bin/env python3
"""CAD-1423 / AOS-238 host-callback refusal driver (acceptance lane 01a12, rev 2).

AUTHORED, NOT RUN. Black-box against the native daemon's service guards for the
host-custodied push bridge: prepare and finish, over HTTP (/api/internal/
notifications/...) and, when method names are supplied, over the daemon's Unix
socket RPC. Written from /tmp/cad1423-start-01a12/aos238/bridge-contract.md and
/tmp/threads-notification-prototype-01a12/ticket.md, not from the implementation.
Independent of source assertions. Do not edit to match an implementation.

Stdlib plus `cryptography` (Ed25519). The only key is a synthetic isolated key
from --make-test-key. It must be trusted by the isolated board's JWKS for the
positive control to pass; if it is not, the positive control fails and the run
is INVALID, never a denial PASS. No production keyring, credential, state dir,
~/.local/bin/cadence or ~/pm is read or signalled.

Per case, a REFUSAL PASSes only when ALL hold:
  * the response is a refusal status (400/401/403/409/422) or an RPC error
    frame (ok:false), with a JSON object body;
  * error code is one of that case's listed intended codes (exact match).
    Lease, business and unprovisioned codes never satisfy a proof case;
  * the body is not success-shaped (no lease_id, device_id, removed, data, result).
404, 503, non-JSON, unlisted codes and unrelated gates are NOT PASS.
An ACCEPT case (positive control) PASSes only on 200 with its expected shape.

Exit codes (highest-priority first):
  1 FAIL          a guard let a bad case through (200 or ok:true)
  2 INVALID       a positive control was refused; denials are not meaningful
  3 NOT RUN       route absent (404/503) or preconditions missing
  4 UNRESOLVED    ambiguous code, unlisted gate, or socket RPC not supplied
  0 all cases PASS
The RPC transport needs a daemon method name from source (--rpc-prepare-method,
--rpc-finish-method) and the proof param name (--rpc-proof-field). Nothing is
guessed. Without them the RPC cases are UNRESOLVED (exit 4), never PASS.

Receipt protocol (written by QA, read-only here): {"pid": <board host pid>, "port":
<loopback port 3110-3199>, "binary": <path>, "state_dir": <path>, "daemon_pid": <daemon host
pid, optional; absent means single process}. Both pids must be live, run the pinned binary
(/proc/<pid>/exe) and name the exact absolute state_dir in argv. Listen checks read the board
pid's own /proc/<pid>/net/tcp and tcp6; the --socket check reads the daemon pid's own
/proc/<pid>/net/unix. Nothing is trusted from the receipt beyond these matches. Optional public_host (e.g. cad1423fixture.board.localhost:3147) is validated, must equal the board pid's --board-host argument, and is sent only as the HTTP Host header of the loopback HTTP sends.

Usage:
  python3 host_bridge_refusal.py --self-check
  python3 host_bridge_refusal.py --make-test-key OUT.json
  python3 host_bridge_refusal.py --base-url http://127.0.0.1:<port> \
      --binary <built-binary> --state-dir <isolated-dir> --receipt <controller.json> \
      --test-key OUT.json --company <id> --instance <id> \
      --intent-id <int> --device-id <id> \
      [--socket <state-dir>/cadence.sock --rpc-prepare-method M --rpc-finish-method M \
       --rpc-intent-id <int> --rpc-device-id <id>]
"""

import argparse
import base64
import json
import os
import re
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

PREPARE_PATH = "/api/internal/notifications/prepare"
FINISH_PATH = "/api/internal/notifications/finish"
ISSUER = "http://api.internal"
TYP = "agenticos-notification-delivery+jwt"
PURPOSE = "cadence-notification-delivery/1"
REFUSAL_STATUS = {400, 401, 403, 409, 422}
ROUTE_ABSENT_STATUS = {404, 503}
SUCCESS_KEYS = ("lease_id", "device_id", "removed", "data", "result")
PROOF_CODES = {"invalid_proof", "unauthorized"}
PROD_BINARY = os.path.realpath(os.path.expanduser("~/.local/bin/cadence"))
PROD_STATE = os.path.realpath(os.path.expanduser("~/.local/state/cadence"))
PROD_PM = os.path.realpath(os.path.expanduser("~/pm"))
RELAY_PATH = "/tmp/cad1423-socket-relay-01a12/relay"
RELAY_SHA256 = "430b9ebe17c6d00e066f98e0dd4a188139321af37800025b9ca027cdcfff8224"
CONTAINER_OWNER = "cad1423-haiku-runtime-qa-01a12"
# Projects only the fields the check reads (no Env, mounts or other config).
INSPECT_FORMAT = ('{"State":{"Pid":{{json .State.Pid}},"Running":{{json .State.Running}}},'
                  '"Config":{"User":{{json .Config.User}},'
                  '"Labels":{"owner":{{json (index .Config.Labels "owner")}}}}}')
CONTAINER_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,62}")

PASS_REFUSAL = "PASS_REFUSAL"
PASS_ACCEPT = "PASS_ACCEPT"
FAIL = "FAIL"
INVALID = "INVALID"
NOT_RUN = "NOT_RUN"
AMBIGUOUS = "AMBIGUOUS"


class NotRun(Exception):
    pass


class Invalid(Exception):
    pass


class Unresolved(Exception):
    """Transport could not deliver a usable frame; never a PASS."""
    pass


def b64u(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def b64u_json(value) -> str:
    return b64u(json.dumps(value, separators=(",", ":"), sort_keys=True).encode())


def make_test_key(out_path: str) -> None:
    """Synthetic Ed25519 key for isolated acceptance only. Not a production key."""
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    key = Ed25519PrivateKey.generate()
    raw = key.private_bytes(
        serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption()
    )
    pub = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    doc = {
        "kid": "cad1423-acceptance-synthetic",
        "d_raw_b64u": b64u(raw),
        "x_b64u": b64u(pub),
        "note": "synthetic isolated acceptance key; never production",
    }
    fd = os.open(out_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(doc, fh)


def load_test_key(path: str):
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    with open(path, "r", encoding="utf-8") as fh:
        doc = json.load(fh)
    raw = base64.urlsafe_b64decode(doc["d_raw_b64u"] + "=" * (-len(doc["d_raw_b64u"]) % 4))
    return Ed25519PrivateKey.from_private_bytes(raw), doc["kid"]


def mint(key, kid, *, path, aud, instance, intent_id, device_id, lease_id=None,
         outcome=None, purpose=PURPOSE, typ=TYP, iss=ISSUER, ttl=5, iat=None) -> str:
    """Compact EdDSA token in the contract's closed claim shape, signed with the
    synthetic key. Negative cases change exactly one field."""
    now = int(time.time()) if iat is None else iat
    claims = {
        "purpose": purpose, "iss": iss, "aud": aud, "instance": instance,
        "method": "POST", "path": path, "intent_id": intent_id, "device_id": device_id,
        "iat": now, "exp": now + ttl, "jti": str(uuid.uuid4()),
    }
    if lease_id is not None:
        claims["lease_id"] = lease_id
    if outcome is not None:
        claims["outcome"] = outcome
    header = {"alg": "EdDSA", "typ": typ, "kid": kid}
    signing_input = f"{b64u_json(header)}.{b64u_json(claims)}"
    return f"{signing_input}.{b64u(key.sign(signing_input.encode('ascii')))}"


def refusal_code(doc):
    """Native shape: top-level code. AOS/RPC shape: error.code."""
    if not isinstance(doc, dict):
        return None
    if isinstance(doc.get("code"), str):
        return doc["code"]
    err = doc.get("error")
    if isinstance(err, dict) and isinstance(err.get("code"), str):
        return err["code"]
    return None


def judge(case, res):
    """Classify one response. res = {"status": int|None, "doc": obj|None, "ok": bool|None}.
    status is set for HTTP; ok is the RPC frame flag. Never PASS on 404/503."""
    status, doc, frame_ok = res.get("status"), res.get("doc"), res.get("ok")
    if status in ROUTE_ABSENT_STATUS:
        return NOT_RUN
    if case["expect"] == "accept":
        if status is not None:
            if status != 200:
                return INVALID if status in REFUSAL_STATUS else AMBIGUOUS
        elif frame_ok is not True:
            return INVALID if frame_ok is False else AMBIGUOUS
        if not isinstance(doc, dict) or refusal_code(doc) is not None:
            return INVALID if isinstance(doc, dict) else AMBIGUOUS
        if case.get("need") == "lease_id":
            body = doc.get("result") if "result" in doc else doc
            if not isinstance(body, dict) or not isinstance(body.get("lease_id"), str) or not body["lease_id"]:
                return INVALID
        return PASS_ACCEPT
    # refuse
    if (status is not None and 200 <= status < 300) or frame_ok is True:
        return FAIL
    if status is not None and status not in REFUSAL_STATUS:
        return AMBIGUOUS
    if frame_ok is None and status is None:
        return AMBIGUOUS
    if not isinstance(doc, dict):
        return AMBIGUOUS
    if any(k in doc for k in SUCCESS_KEYS):
        return AMBIGUOUS
    code = refusal_code(doc)
    if code is None or code not in case["codes"]:
        return AMBIGUOUS
    return PASS_REFUSAL


def post_http(base, path, body, token, host=None):
    data = json.dumps(body).encode()
    headers = {"content-type": "application/json", "authorization": f"Bearer {token}"}
    if host is not None:
        headers["Host"] = host
    req = urllib.request.Request(
        base + path,
        data=data,
        headers=headers,
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            status, text = r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        status, text = exc.code, exc.read().decode("utf-8", "replace")
    try:
        doc = json.loads(text)
    except ValueError:
        doc = None
    return {"status": status, "doc": doc, "ok": None}


def call_rpc(sock_path, method, params, token, proof_field):
    frame_params = dict(params)
    frame_params[proof_field] = token
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(10)
    try:
        s.connect(sock_path)
        s.sendall((json.dumps({"method": method, "params": frame_params}) + "\n").encode())
        buf = b""
        while b"\n" not in buf and len(buf) < (1 << 20):
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
    finally:
        s.close()
    line = buf.split(b"\n", 1)[0].decode("utf-8", "replace")
    try:
        frame = json.loads(line)
    except ValueError:
        return {"status": None, "doc": None, "ok": None}
    ok = frame.get("ok") if isinstance(frame, dict) else None
    return {"status": None, "doc": frame if isinstance(frame, dict) else None,
            "ok": ok if isinstance(ok, bool) else None}


def call_rpc_relay(container, sock_path, method, params, token, proof_field):
    """Same frame as call_rpc, sent through the owned relay inside the native container.
    The frame goes on stdin as one JSON line; argv carries only the container and socket."""
    frame_params = dict(params)
    frame_params[proof_field] = token
    line = (json.dumps({"method": method, "params": frame_params}) + "\n").encode()
    argv = ["docker", "exec", "-i", container, RELAY_PATH, sock_path]
    try:
        proc = subprocess.run(argv, input=line, capture_output=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Unresolved(f"relay transport failed: {type(exc).__name__}")
    out = proc.stdout
    if proc.returncode != 0 or len(out) > (1 << 20) or out.count(b"\n") != 1 or not out.endswith(b"\n"):
        raise Unresolved("relay transport returned no single bounded response line")
    try:
        frame = json.loads(out.decode("utf-8"))
    except ValueError:
        raise Unresolved("relay response is not JSON")
    ok = frame.get("ok") if isinstance(frame, dict) else None
    return {"status": None, "doc": frame if isinstance(frame, dict) else None,
            "ok": ok if isinstance(ok, bool) else None}


def container_check(name, daemon_pid, relay):
    """Read-only: the container is ours, runs as the daemon uid, shares the daemon pid
    namespace, and holds the reviewed relay binary at the exact owned path."""
    if not CONTAINER_RE.fullmatch(name):
        raise NotRun("rpc container name is not a bounded literal")
    if relay != RELAY_PATH:
        raise NotRun("rpc relay must be the exact owned relay path")
    try:
        ins = subprocess.run(["docker", "inspect", "--format", INSPECT_FORMAT, name],
                             capture_output=True, text=True, timeout=10, check=False)
        if ins.returncode != 0:
            raise NotRun("rpc container not found")
        info = json.loads(ins.stdout)
        sha = subprocess.run(["docker", "exec", name, "sha256sum", relay], capture_output=True,
                             text=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired, ValueError, KeyError) as exc:
        raise NotRun(f"rpc container check unavailable: {type(exc).__name__}")
    labels = (info.get("Config") or {}).get("Labels") or {}
    if labels.get("owner") != CONTAINER_OWNER:
        raise NotRun("rpc container owner label does not match this acceptance lane")
    if not (info.get("State") or {}).get("Running"):
        raise NotRun("rpc container is not running")
    user = ((info.get("Config") or {}).get("User") or "").split(":")[0]
    try:
        with open(f"/proc/{daemon_pid}/status", "r", encoding="ascii") as fh:
            daemon_uid = next(l.split()[1] for l in fh if l.startswith("Uid:"))
        init_pid = info["State"]["Pid"]
        same_ns = (os.readlink(f"/proc/{init_pid}/ns/pid")
                   == os.readlink(f"/proc/{daemon_pid}/ns/pid"))
    except (OSError, StopIteration, KeyError, TypeError) as exc:
        raise NotRun(f"rpc container or daemon identity unreadable: {type(exc).__name__}")
    if user != daemon_uid:
        raise NotRun("rpc container user does not match the daemon uid")
    if not same_ns:
        raise NotRun("rpc container pid namespace is not the daemon pid namespace")
    if sha.returncode != 0 or sha.stdout.split()[:1] != [RELAY_SHA256]:
        raise NotRun("rpc relay binary in container is not the reviewed build")


def proc_listens_on(pid, port):
    """Read-only: pid's own netns LISTEN table (/proc/<pid>/net/tcp*) and its fd inodes."""
    inodes = set()
    fd_dir = f"/proc/{pid}/fd"
    for fd in os.listdir(fd_dir):
        try:
            link = os.readlink(os.path.join(fd_dir, fd))
        except OSError:
            continue
        if link.startswith("socket:[") and link.endswith("]"):
            inodes.add(link[8:-1])
    for table in (f"/proc/{pid}/net/tcp", f"/proc/{pid}/net/tcp6"):
        try:
            with open(table, "r", encoding="ascii") as fh:
                rows = fh.readlines()[1:]
        except OSError:
            continue
        for row in rows:
            parts = row.split()
            if len(parts) < 10:
                continue
            local_port = int(parts[1].rsplit(":", 1)[1], 16)
            if parts[3] == "0A" and local_port == port and parts[9] in inodes:
                return True
    return False


def pid_owns_unix_socket(pid, sock_path):
    """Read-only: pid owns the socket inode that /proc/net/unix binds at sock_path."""
    inodes = set()
    fd_dir = f"/proc/{pid}/fd"
    for fd in os.listdir(fd_dir):
        try:
            link = os.readlink(os.path.join(fd_dir, fd))
        except OSError:
            continue
        if link.startswith("socket:[") and link.endswith("]"):
            inodes.add(link[8:-1])
    with open(f"/proc/{pid}/net/unix", "r", encoding="ascii") as fh:
        rows = fh.readlines()[1:]
    for row in rows:
        parts = row.split()
        if len(parts) >= 8 and parts[7] == sock_path and parts[6] in inodes:
            return True
    return False


def _argv_names_state(argv, want_state):
    """True when an argv element is the exact absolute isolated state dir,
    bare or as --state-dir=<dir>."""
    for arg in argv:
        value = arg.split("=", 1)[1] if arg.startswith("--state-dir=") else arg
        if value.startswith(os.sep) and os.path.realpath(value) == want_state:
            return True
    return False


def _check_pid(pid, label, want_binary, want_state):
    """Read-only: live pid runs the pinned binary with the exact owned state dir."""
    try:
        exe = os.path.realpath(os.readlink(f"/proc/{pid}/exe"))
        with open(f"/proc/{pid}/cmdline", "rb") as fh:
            argv = [a.decode("utf-8", "replace") for a in fh.read().split(b"\0") if a]
    except OSError as exc:
        raise NotRun(f"{label} pid {pid} identity unreadable: {exc}")
    if exe != want_binary:
        raise NotRun(f"{label} pid {pid} runs {exe}, not the built binary")
    if not _argv_names_state(argv, want_state):
        raise NotRun(f"{label} pid {pid} argv does not name the isolated state dir")
    return argv


PUBLIC_HOST_MAX = 256
LABEL_CHARS = frozenset("abcdefghijklmnopqrstuvwxyz0123456789-")
PUBLIC_HOST_CHARS = LABEL_CHARS | {".", ":"}


def validate_public_host(value, port):
    """Receipt public_host: ASCII host:port where host is 127.0.0.1, localhost or
    *.localhost, and the port is explicit and equal to receipt.port. Refuses all else."""
    if not isinstance(value, str) or not 0 < len(value) <= PUBLIC_HOST_MAX:
        raise NotRun("receipt public_host must be a non-empty string of at most 256 chars")
    if not PUBLIC_HOST_CHARS.issuperset(value):
        raise NotRun("receipt public_host has characters outside a-z 0-9 - . :")
    host, sep, port_text = value.rpartition(":")
    if not sep or port_text != str(port):
        raise NotRun("receipt public_host needs an explicit port equal to receipt.port")
    if host != "127.0.0.1":
        labels = host.split(".")
        if labels[-1] != "localhost" or not all(_valid_label(lbl) for lbl in labels):
            raise NotRun("receipt public_host must be localhost, *.localhost or 127.0.0.1")
    return value


def _valid_label(label):
    return 0 < len(label) <= 63 and label[0] != "-" and label[-1] != "-" and LABEL_CHARS.issuperset(label)


def board_host_arg(argv):
    """Value of the single --board-host argument (bare or --board-host=value), else None."""
    values = []
    for i, arg in enumerate(argv):
        if arg.startswith("--board-host="):
            values.append(arg.split("=", 1)[1])
        elif arg == "--board-host":
            values.append(argv[i + 1] if i + 1 < len(argv) else None)
    return values[0] if len(values) == 1 else None


def verify_target(args):
    """Receipt, exe and exact owned state_dir argv for board and daemon pids, board
    TCP LISTEN on the receipt port in its own netns, daemon Unix socket owned by the
    daemon pid inside the isolated state dir. Port 3110-3199 and not 3010. Read-only."""
    if not args.receipt:
        raise NotRun("--receipt not supplied")
    try:
        with open(args.receipt, "r", encoding="utf-8") as fh:
            receipt = json.load(fh)
    except (OSError, ValueError) as exc:
        raise NotRun(f"receipt unreadable: {exc}")
    pid, port = receipt.get("pid"), receipt.get("port")
    daemon_pid = receipt.get("daemon_pid", pid)  # absent: single process
    if not all(isinstance(v, int) for v in (pid, port, daemon_pid)):
        raise NotRun("receipt needs integer pid and port (daemon_pid optional, integer)")
    if pid <= 0 or daemon_pid <= 0:
        raise NotRun("receipt pids must be positive live PIDs")
    want_binary = os.path.realpath(args.binary)
    want_state = os.path.realpath(args.state_dir)
    if want_binary == PROD_BINARY:
        raise NotRun("production binary refused")
    if want_state.startswith(PROD_STATE) or want_state.startswith(PROD_PM):
        raise NotRun("production state dir refused")
    if port == 3010 or not (3110 <= port <= 3199):
        raise NotRun(f"port {port} outside isolated 3110-3199")
    url = urllib.parse.urlsplit(args.base_url)
    if url.port != port or url.hostname not in ("127.0.0.1", "localhost"):
        raise NotRun("base-url is not the receipt loopback port")
    if os.path.realpath(receipt.get("binary", "")) != want_binary:
        raise NotRun("receipt binary differs from --binary")
    if os.path.realpath(receipt.get("state_dir", "")) != want_state:
        raise NotRun("receipt state_dir differs from --state-dir")
    board_argv = _check_pid(pid, "board", want_binary, want_state)
    _check_pid(daemon_pid, "daemon", want_binary, want_state)
    if not proc_listens_on(pid, port):
        raise NotRun(f"board pid {pid} does not listen on port {port} in its own netns")
    public_host = None
    if "public_host" in receipt:
        public_host = validate_public_host(receipt["public_host"], port)
        if board_host_arg(board_argv) != public_host:
            raise NotRun("receipt public_host does not equal the board pid --board-host argument")
    if args.socket:
        sock = os.path.realpath(args.socket)
        if not sock.startswith(want_state + os.sep):
            raise NotRun("--socket is not inside the isolated state dir")
        if not pid_owns_unix_socket(daemon_pid, sock):
            raise NotRun(f"daemon pid {daemon_pid} does not own socket {sock}")
    return public_host, daemon_pid


def build_cases(lease_holder, iid, did, co, inst, key, kid):
    """Ordered case table shared by both transports. lease_holder['lease'] is set
    by the positive prepare. Tokens are minted when each case runs."""
    def tok(path=PREPARE_PATH, **over):
        kw = dict(key=key, kid=kid, path=path, aud=co, instance=inst,
                  intent_id=iid, device_id=did)
        kw.update(over)
        return mint(**kw)

    def zero_sig(token):
        head, body, _ = token.split(".")
        return f"{head}.{body}.{b64u(bytes(64))}"

    lease = lambda: lease_holder["lease"]  # noqa: E731
    prep_body = {"intent_id": iid, "device_id": did}
    fin_body = lambda lid, outcome="retry": {  # noqa: E731
        "intent_id": iid, "device_id": did, "lease_id": lid, "outcome": outcome}
    cases = [
        {"name": "prepare positive control", "op": "prepare", "expect": "accept",
         "need": "lease_id", "body": prep_body, "token": lambda: tok(), "codes": set(),
         "capture": "lease"},
        {"name": "prepare refuses exact proof replay while lease live", "op": "prepare",
         "expect": "refuse", "body": prep_body, "token": lambda: lease_holder["token"],
         "codes": {"lease_active"}},
    ]
    for name, token in [
        ("forged signature", lambda: zero_sig(tok())),
        ("wrong company", lambda: tok(aud="foreign-company")),
        ("wrong instance", lambda: tok(instance="foreign1")),
        ("wrong purpose", lambda: tok(purpose="cadence-board-membership/1")),
        ("generic relay typ", lambda: tok(typ="agenticos-board-relay/1")),
        ("board assertion typ", lambda: tok(typ="JWT")),
        ("expired", lambda: tok(iat=int(time.time()) - 60, ttl=5)),
        ("ttl over 5s", lambda: tok(ttl=30)),
        ("wrong path", lambda: tok(path=FINISH_PATH)),
        ("wrong intent binding", lambda: tok(intent_id=iid + 1)),
        ("wrong device binding", lambda: tok(device_id=did + "-other")),
    ]:
        codes = set(PROOF_CODES)
        if name == "wrong company":
            codes.add("wrong_company")
        if name == "wrong instance":
            codes.add("wrong_instance")
        if name == "expired":
            codes.add("expired")
        cases.append({"name": f"prepare refuses {name}", "op": "prepare", "expect": "refuse",
                      "body": prep_body, "token": token, "codes": codes})
    for name, extra in [
        ("caller endpoint", {"endpoint": "https://fcm.googleapis.com/x"}),
        ("caller recipient", {"recipient": "forged-subject"}),
        ("caller organization", {"organization_id": "forged-org"}),
    ]:
        cases.append({"name": f"prepare refuses {name} override", "op": "prepare",
                      "expect": "refuse", "body": {**prep_body, **extra},
                      "token": lambda: tok(), "codes": {"invalid_request"}})
    for name, body in [
        ("malformed intent_id type", {"intent_id": str(iid), "device_id": did}),
        ("missing device_id", {"intent_id": iid}),
    ]:
        cases.append({"name": f"prepare refuses {name}", "op": "prepare", "expect": "refuse",
                      "body": body, "token": lambda: tok(), "codes": {"invalid_request"}})
    cases.append({"name": "prepare refuses replay while lease live", "op": "prepare",
                  "expect": "refuse", "body": prep_body, "token": lambda: tok(),
                  "codes": {"lease_active", "stale_lease"}})
    cases.append({"name": "finish refuses wrong lease", "op": "finish", "expect": "refuse",
                  "body": lambda: fin_body("0" * 36),
                  "token": lambda: tok(path=FINISH_PATH, lease_id="0" * 36, outcome="retry"),
                  "codes": {"stale_lease"}})
    cases.append({"name": "finish refuses wrong purpose", "op": "finish", "expect": "refuse",
                  "body": lambda: fin_body(lease()),
                  "token": lambda: tok(path=FINISH_PATH, lease_id=lease(), outcome="retry",
                                       purpose="cadence-board-membership/1"),
                  "codes": set(PROOF_CODES)})
    cases.append({"name": "finish positive control (legitimate, lease valid)", "op": "finish",
                  "expect": "accept", "body": lambda: fin_body(lease()),
                  "token": lambda: tok(path=FINISH_PATH, lease_id=lease(), outcome="retry"),
                  "codes": set()})
    cases.append({"name": "finish refuses replay of finished lease", "op": "finish",
                  "expect": "refuse", "body": lambda: fin_body(lease()),
                  "token": lambda: tok(path=FINISH_PATH, lease_id=lease(), outcome="retry"),
                  "codes": {"stale_lease"}})
    return cases


def run_transport(name, cases, send, lease_holder):
    """Runs cases in order. A positive control that is not PASS_ACCEPT stops the
    transport: denials after an invalid positive control are not meaningful."""
    out = []
    for case in cases:
        body = case["body"]() if callable(case["body"]) else case["body"]
        token = case["token"]()
        res = send(case["op"], body, token)
        verdict = judge(case, res)
        out.append((f"{name}: {case['name']}", verdict))
        if case["expect"] == "accept":
            if verdict != PASS_ACCEPT:
                return out, True
            if case.get("capture") == "lease":
                doc = res["doc"]
                body = doc["result"] if "result" in doc else doc
                lease_holder["lease"] = body["lease_id"]
                lease_holder["token"] = token
    return out, False


def run_live(args) -> int:
    public_host, daemon_pid = verify_target(args)
    key, kid = load_test_key(args.test_key)
    if args.intent_id is None or not args.device_id:
        raise NotRun("needs seeded --intent-id and --device-id for the HTTP sequence")
    base = args.base_url.rstrip("/")
    co, inst = args.company, args.instance
    results = []

    def send_http(op, body, token):
        path = PREPARE_PATH if op == "prepare" else FINISH_PATH
        return post_http(base, path, body, token, host=public_host)

    holder = {}
    http_cases = build_cases(holder, args.intent_id, args.device_id, co, inst, key, kid)
    out, stop = run_transport("HTTP", http_cases, send_http, holder)
    results.extend(out)
    if stop:
        last = out[-1][1]
        if last == NOT_RUN:
            raise NotRun("HTTP prepare/finish route absent or unavailable (404/503)")
        raise Invalid(f"HTTP positive control not accepted: {last}")

    if args.socket and args.rpc_prepare_method and args.rpc_finish_method:
        if args.rpc_intent_id is None or not args.rpc_device_id:
            raise NotRun("socket RPC needs --rpc-intent-id and --rpc-device-id (separate intent)")
        rpc_holder = {}
        methods = {"prepare": args.rpc_prepare_method, "finish": args.rpc_finish_method}

        if args.rpc_container:
            container_check(args.rpc_container, daemon_pid, args.rpc_relay)

        def send_rpc(op, body, token):
            if args.rpc_container:
                return call_rpc_relay(args.rpc_container, args.socket, methods[op], body, token,
                                      args.rpc_proof_field)
            return call_rpc(args.socket, methods[op], body, token, args.rpc_proof_field)

        rpc_cases = build_cases(rpc_holder, args.rpc_intent_id, args.rpc_device_id, co, inst,
                                key, kid)
        try:
            out, stop = run_transport("RPC", rpc_cases, send_rpc, rpc_holder)
        except Unresolved as exc:
            results.append((f"RPC transport: {exc}", AMBIGUOUS))
        else:
            results.extend(out)
            if stop:
                raise Invalid(f"RPC positive control not accepted: {out[-1][1]}")
    else:
        results.append(("socket RPC cases: no daemon method wire supplied", AMBIGUOUS))

    for label, verdict in results:
        print(f"{label}: {verdict}")
    verdicts = [v for _, v in results]
    if FAIL in verdicts:
        return 1
    if INVALID in verdicts:
        return 2
    if NOT_RUN in verdicts:
        return 3
    if all(v in (PASS_REFUSAL, PASS_ACCEPT) for v in verdicts):
        return 0
    return 4


def self_check() -> int:
    """Classifier logic only, on synthetic responses. Not acceptance evidence."""
    refuse_proof = {"expect": "refuse", "codes": set(PROOF_CODES) | {"expired"}}
    refuse_stale = {"expect": "refuse", "codes": {"stale_lease"}}
    refuse_replay = {"expect": "refuse", "codes": {"lease_active"}}
    accept_lease = {"expect": "accept", "need": "lease_id", "codes": set()}
    cases = [
        (refuse_proof, {"status": 400, "doc": {"error": "bad", "code": "invalid_proof"}}, PASS_REFUSAL),
        (refuse_proof, {"status": 400, "doc": {"ok": False, "error": {"code": "unauthorized"}}}, PASS_REFUSAL),
        (refuse_proof, {"status": 409, "doc": {"code": "lease_active"}}, AMBIGUOUS),
        (refuse_proof, {"status": 403, "doc": {"code": "not_provisioned"}}, AMBIGUOUS),
        (refuse_proof, {"status": 404, "doc": {"error": "not found"}}, NOT_RUN),
        (refuse_proof, {"status": 503, "doc": {"ok": False, "error": {"code": "capability_unavailable"}}}, NOT_RUN),
        (refuse_proof, {"status": 200, "doc": {"lease_id": "x"}}, FAIL),
        (refuse_proof, {"status": None, "doc": {"ok": True, "result": {}}, "ok": True}, FAIL),
        (refuse_stale, {"status": 400, "doc": {"code": "invalid_proof"}}, AMBIGUOUS),
        (refuse_stale, {"status": 409, "doc": {"code": "stale_lease"}}, PASS_REFUSAL),
        (refuse_stale, {"status": 400, "doc": {"code": "stale_lease", "lease_id": "x"}}, AMBIGUOUS),
        (refuse_replay, {"status": 409, "doc": {"code": "lease_active"}}, PASS_REFUSAL),
        (refuse_replay, {"status": 409, "doc": {"code": "stale_lease"}}, AMBIGUOUS),
        (refuse_proof, {"status": None, "doc": {"ok": False, "error": {"code": "invalid_proof"}}, "ok": False}, PASS_REFUSAL),
        (accept_lease, {"status": 200, "doc": {"lease_id": "abc"}}, PASS_ACCEPT),
        (accept_lease, {"status": 200, "doc": {}}, INVALID),
        (accept_lease, {"status": 400, "doc": {"code": "invalid_request"}}, INVALID),
        (accept_lease, {"status": 404, "doc": None}, NOT_RUN),
        (accept_lease, {"status": None, "doc": {"ok": True, "result": {"lease_id": "r"}}, "ok": True}, PASS_ACCEPT),
        (refuse_proof, {"status": None, "doc": None, "ok": None}, AMBIGUOUS),
    ]
    ok = True
    for case, res, want in cases:
        got = judge(case, res)
        if got != want:
            ok = False
            print(f"self-check MISMATCH want={want} got={got} res={res}")
    host_cases = [
        ("cad1423fixture.board.localhost:3147", 3147, True),
        ("localhost:3147", 3147, True),
        ("127.0.0.1:3147", 3147, True),
        ("cad1423fixture.board.localhost:3148", 3147, False),
        ("cad1423fixture.board.localhost", 3147, False),
        ("evil.example:3147", 3147, False),
        ("x.localhost:3147/path", 3147, False),
        ("user@x.localhost:3147", 3147, False),
        ("X.localhost:3147", 3147, False),
        ("-x.localhost:3147", 3147, False),
        ("a\r\nHost: evil.localhost:3147", 3147, False),
        ("a:b.localhost:3147", 3147, False),
        ("x.localhost:03147", 3147, False),
        ("a..localhost:3147", 3147, False),
        ("", 3147, False),
        ("a" * 250 + ".localhost:3147", 3147, False),
    ]
    for value, port_value, want in host_cases:
        try:
            validate_public_host(value, port_value)
            got = True
        except NotRun:
            got = False
        if got != want:
            ok = False
            print(f"self-check public_host MISMATCH value={value!r} want={want}")
    argv_cases = [
        (["x", "--board-host", "h.localhost:3147"], "h.localhost:3147"),
        (["x", "--board-host=h.localhost:3147"], "h.localhost:3147"),
        (["x", "--board-host"], None),
        (["x"], None),
        (["--board-host", "a", "--board-host=b"], None),
    ]
    for argv, want in argv_cases:
        if board_host_arg(argv) != want:
            ok = False
            print(f"self-check board_host_arg MISMATCH argv={argv!r}")
    print("self-check " + ("OK (classifier logic only; no live evidence)" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv=None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--self-check", action="store_true")
    p.add_argument("--make-test-key", metavar="OUT")
    p.add_argument("--base-url")
    p.add_argument("--binary")
    p.add_argument("--state-dir")
    p.add_argument("--receipt")
    p.add_argument("--test-key")
    p.add_argument("--company")
    p.add_argument("--instance")
    p.add_argument("--intent-id", type=int)
    p.add_argument("--device-id")
    p.add_argument("--socket", help="daemon Unix socket inside --state-dir (RPC cases)")
    p.add_argument("--rpc-prepare-method", help="daemon method name for prepare (from source)")
    p.add_argument("--rpc-finish-method", help="daemon method name for finish (from source)")
    p.add_argument("--rpc-proof-field", default="proof", help="param field carrying the proof")
    p.add_argument("--rpc-intent-id", type=int)
    p.add_argument("--rpc-device-id")
    p.add_argument("--rpc-container", help="native container name for the owned relay transport")
    p.add_argument("--rpc-relay", default=RELAY_PATH, help="relay path inside the container")
    args = p.parse_args(argv)
    if args.self_check:
        return self_check()
    if args.make_test_key:
        make_test_key(args.make_test_key)
        print("synthetic test key written (isolated acceptance only)")
        return 0
    required = [args.base_url, args.binary, args.state_dir, args.test_key, args.company, args.instance]
    if not all(required):
        print("NOT RUN: missing --base-url/--binary/--state-dir/--test-key/--company/--instance")
        return 3
    try:
        return run_live(args)
    except NotRun as exc:
        print(f"NOT RUN: {exc}")
        return 3
    except Invalid as exc:
        print(f"INVALID: {exc}")
        return 2
    except Unresolved as exc:
        print(f"UNRESOLVED: {exc}")
        return 4


if __name__ == "__main__":
    sys.exit(main())
