#!/usr/bin/env python3
"""CAD-1423 independent refusal driver (author: acceptance lane 01a12, rev 3).

Black-box, stdlib-only, read-only until the target is verified. It drives an
isolated board over real HTTP with two ephemeral, normally signed-in human
accounts (A and B, distinct subjects) and optionally a third account C. It
never reads production state, never uses test/release seams, never sets
caller/operator flags, never clears or forges identity, never signals a
process and never sends a push (provider-shaped endpoints are only stored,
never contacted by this driver).

Real contract this driver is written against (src/ui/human_threads.rs,
src/daemon/human_threads_rpc.rs, src/web_push.rs):
  * refusal body: {"error": <message>, "code": <code>, "revision": null}
  * success: 200 with the RPC value, e.g. {"device_id": ...} or {"removed": bool}
  * DELETE of another human's or an unknown device: 200 {"removed": false}
    (a no-op), so a 200 is never proof by itself; see device-integrity.

A refusal case PASSes only when ALL hold:
  * status is 400/401/403/409/422, the body is a JSON object, it is not
    success-shaped (no ok:true, device_id, removed or data);
  * it carries the intended guard: an exact `code` where the guard sets one
    (identity_field_refused) or the guard's own `error` text (endpoint and
    ownership refusals). Unrelated refusals, such as an expired sign-in,
    never match and never PASS.
Generic 404, 503, non-JSON and success-shaped bodies are never a refusal PASS.

Exit codes:
  0 all core and optional cases PASS
  1 a FAIL (a guard let a bad case through, or device integrity broke)
  2 INVALID (identity or positive control failed; denials not meaningful)
  3 NOT RUN (preconditions missing or target not verified)
  4 UNRESOLVED (any AMBIGUOUS case; NOT a pass)
  5 CORE PASS, OPTIONAL NOT RUN (qualified; not an unqualified pass)

Usage:
  python3 cad1423_refusal_driver.py --base-url http://127.0.0.1:<port> \
      --cookie-a-file A.txt --cookie-b-file B.txt [--cookie-c-file C.txt] \
      --binary <built-binary> --state-dir <isolated-dir> --receipt <controller.json>
  python3 cad1423_refusal_driver.py --self-check    (offline classifier check, no board)
"""

import argparse
import base64
import http.client
import json
import os
import sys
import urllib.parse

COOKIE_NAME = "__Host-aos-board-session"
REFUSAL_STATUS = {400, 401, 403, 409, 422}
SUCCESS_KEYS = ("device_id", "removed", "data")

# Intended guard markers (exact text the real guard sets).
IDENTITY_CODE = "identity_field_refused"
ENDPOINT_TEXT = "push endpoint is not an allowed provider"
OWNED_ELSEWHERE_TEXT = "push endpoint is already enrolled for another account"

# Public P-256 generator point G (standard SEC 2 domain parameters). Public,
# not secret; used only as a syntactically valid uncompressed P-256 key.
P256_GX = 0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296
P256_GY = 0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5


def b64url(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


P256DH = b64url(bytes([4]) + P256_GX.to_bytes(32, "big") + P256_GY.to_bytes(32, "big"))  # 65 bytes
AUTH = b64url(bytes(range(16)))  # 16 bytes

ENDPOINT_A = "https://fcm.googleapis.com/fcm/send/cad1423-acceptance-01a12-a"
ENDPOINT_B = "https://updates.push.services.mozilla.com/wpush/v2/cad1423-acceptance-01a12-b"
ENDPOINT_FORGE = "https://fcm.googleapis.com/fcm/send/cad1423-acceptance-01a12-forge"
BAD_ENDPOINTS = [
    "http://fcm.googleapis.com/fcm/send/cad1423-probe",  # not https
    "https://127.0.0.1/cad1423-probe",  # loopback literal
    "https://localhost:8443/cad1423-probe",  # local name
    "https://10.0.0.5/cad1423-probe",  # private literal
    "https://169.254.169.254/latest/meta-data",  # link-local metadata literal
    "https://[::1]/cad1423-probe",  # loopback v6
    "https://push-probe.invalid/cad1423-probe",  # arbitrary host
    "https://fcm.googleapis.com.attacker.invalid/cad1423-probe",  # suffix trick
    "https://user@fcm.googleapis.com/cad1423-probe",  # userinfo (expected refused)
    "https://fcm.googleapis.com:8443/cad1423-probe",  # non-standard port
]
FORGED_FIELDS = {
    "token": "forged-token",
    "issuer": "http://evil.invalid",
    "org": "forged-org",
    "organization_id": "forged-org",
    "subject": "forged-subject",
    "sub": "forged-subject",
    "as": "operator:forged",
    "actor": "forged-actor",
    "role": "owner",
}
PROD_STATE = os.path.realpath(os.path.expanduser("~/.local/state/cadence"))
PROD_BINARY = os.path.realpath(os.path.expanduser("~/.local/bin/cadence"))
PROD_PM = os.path.realpath(os.path.expanduser("~/pm"))


class NotRun(Exception):
    pass


class Invalid(Exception):
    pass


def proc_listens_on(pid, port):
    """Read-only: does pid own a LISTEN socket on port (via /proc)."""
    inodes = set()
    fd_dir = f"/proc/{pid}/fd"
    for fd in os.listdir(fd_dir):
        try:
            link = os.readlink(os.path.join(fd_dir, fd))
        except OSError:
            continue
        if link.startswith("socket:[") and link.endswith("]"):
            inodes.add(link[8:-1])
    for table in ("/proc/net/tcp", "/proc/net/tcp6"):
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


def verify_target(args):
    """Bounded read-only proof that the board on base-url is the built binary
    running on the isolated state dir. Requires a controller receipt naming
    pid, binary, state_dir and port. Sends no signal and changes nothing."""
    if not args.receipt:
        raise NotRun("--receipt (controller receipt with pid) not supplied")
    try:
        with open(args.receipt, "r", encoding="utf-8") as fh:
            receipt = json.load(fh)
    except (OSError, ValueError) as exc:
        raise NotRun(f"receipt unreadable: {exc}")
    pid, port = receipt.get("pid"), receipt.get("port")
    if not isinstance(pid, int) or not isinstance(port, int):
        raise NotRun("receipt needs integer pid and port")
    want_binary = os.path.realpath(args.binary)
    want_state = os.path.realpath(args.state_dir)
    if want_binary == PROD_BINARY:
        raise NotRun("production binary refused")
    if want_state.startswith(PROD_STATE) or want_state.startswith(PROD_PM):
        raise NotRun("production state dir refused")
    if os.path.realpath(receipt.get("binary", "")) != want_binary:
        raise NotRun("receipt binary differs from --binary")
    if os.path.realpath(receipt.get("state_dir", "")) != want_state:
        raise NotRun("receipt state_dir differs from --state-dir")
    url = urllib.parse.urlsplit(args.base_url)
    if url.port != port or url.hostname not in ("127.0.0.1", "localhost"):
        raise NotRun("base-url is not the receipt's loopback port")
    if port == 3010 or not (3110 <= port <= 3199):
        raise NotRun(f"port {port} outside isolated 3110-3199")
    try:
        exe = os.path.realpath(os.readlink(f"/proc/{pid}/exe"))
        with open(f"/proc/{pid}/cmdline", "rb") as fh:
            argv = [a.decode("utf-8", "replace") for a in fh.read().split(b"\0") if a]
    except OSError as exc:
        raise NotRun(f"pid {pid} identity unreadable: {exc}")
    if exe != want_binary:
        raise NotRun(f"pid {pid} runs {exe}, not the built binary")
    if receipt.get("state_dir") not in argv:
        raise NotRun(f"pid {pid} argv does not name the isolated state dir")
    if not proc_listens_on(pid, port):
        raise NotRun(f"pid {pid} does not listen on port {port}")


def read_cookie(path):
    """Return the board session value. Never printed, logged or stored."""
    try:
        with open(path, "r", encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if line.startswith(COOKIE_NAME + "="):
                    return line.split("=", 1)[1]
    except OSError as exc:
        raise NotRun(f"{path} unreadable: {exc}")
    raise NotRun(f"{path} has no {COOKIE_NAME} value")


class Board:
    def __init__(self, base_url, cookie):
        url = urllib.parse.urlsplit(base_url)
        self.host = url.hostname
        self.port = url.port
        self.origin = f"{url.scheme}://{url.netloc}"
        self.cookie = cookie

    def call(self, method, path, body=None):
        conn = http.client.HTTPConnection(self.host, self.port, timeout=15)
        headers = {"Cookie": f"{COOKIE_NAME}={self.cookie}"}
        payload = None
        if method in ("POST", "DELETE"):
            headers.update(
                {
                    "Content-Type": "application/json",
                    "X-Cadence-Board": "1",
                    "Origin": self.origin,
                    "Sec-Fetch-Site": "same-origin",
                }
            )
            payload = json.dumps(body if body is not None else {}).encode()
            headers["Content-Length"] = str(len(payload))
        try:
            conn.request(method, path, body=payload, headers=headers)
            resp = conn.getresponse()
            raw = resp.read(64 * 1024)
        finally:
            conn.close()
        try:
            data = json.loads(raw.decode("utf-8")) if raw else None
        except (ValueError, UnicodeDecodeError):
            data = None
        return resp.status, data


def device_id_of(data):
    if isinstance(data, dict) and isinstance(data.get("device_id"), str):
        return data["device_id"]
    return None


def removed_of(data):
    """True/False from a DELETE body, or None when the body is not that shape."""
    if isinstance(data, dict) and isinstance(data.get("removed"), bool) and set(data) == {"removed"}:
        return data["removed"]
    return None


def is_success_shape(data):
    """A success body: ok:true, or any RPC success key. ok:false is not success."""
    if not isinstance(data, dict):
        return False
    return data.get("ok") is True or any(k in data for k in SUCCESS_KEYS)


def is_guard_refusal(status, data, code=None, text=None):
    """True only for a structured refusal from the intended guard.

    `code` (exact) is required where the guard sets one. `text` must be a
    substring of the guard's own error message. Anything else is not a PASS.
    """
    if status not in REFUSAL_STATUS or not isinstance(data, dict):
        return False
    if is_success_shape(data):
        return False
    body_code = data.get("code")
    if not isinstance(body_code, str) or not body_code:
        return False
    message = data.get("error")
    if not isinstance(message, str):
        return False
    if code is not None and body_code != code:
        return False
    if text is not None and text not in message:
        return False
    return True


def enroll_body(endpoint, extra=None):
    body = {"endpoint": endpoint, "keys": {"p256dh": P256DH, "auth": AUTH}}
    if extra:
        body.update(extra)
    return body


def run(args):
    a = Board(args.base_url, read_cookie(args.cookie_a_file))
    b = Board(args.base_url, read_cookie(args.cookie_b_file))
    c = Board(args.base_url, read_cookie(args.cookie_c_file)) if args.cookie_c_file else None
    results = []

    def record(case, verdict, detail):
        results.append({"case": case, "verdict": verdict, "detail": detail})
        print(json.dumps(results[-1]))

    def fail_invalid(case, detail):
        record(case, "INVALID", detail)
        raise Invalid(case)

    # Identity: two distinct live humans, via the existing /me route.
    sa, da = a.call("GET", "/api/human-threads/me")
    sb, db = b.call("GET", "/api/human-threads/me")
    id_a = da.get("human", {}).get("id") if isinstance(da, dict) else None
    id_b = db.get("human", {}).get("id") if isinstance(db, dict) else None
    if sa != 200 or sb != 200 or not id_a or not id_b or id_a == id_b:
        fail_invalid("identity", f"me A={sa} me B={sb}; need two distinct live humans")
    record("identity", "PASS", "A and B resolved to distinct human ids via /me")

    # Positive controls: each human enrolls its own valid device.
    s, d = a.call("POST", "/api/human-threads/push/subscriptions", enroll_body(ENDPOINT_A))
    dev_a = device_id_of(d)
    if s != 200 or dev_a is None:
        fail_invalid("positive-control-A", f"status={s}; denials not meaningful")
    record("positive-control-A", "PASS", "A enrolled a valid device (200, device_id)")
    s, d = b.call("POST", "/api/human-threads/push/subscriptions", enroll_body(ENDPOINT_B))
    dev_b = device_id_of(d)
    if s != 200 or dev_b is None:
        fail_invalid("positive-control-B", f"status={s}; B cannot enroll its own device")
    record("positive-control-B", "PASS", "B enrolled its own valid device (200, device_id)")

    # Forged identity fields on A's enrollment (valid keys, new endpoint).
    s, d = a.call("POST", "/api/human-threads/push/subscriptions", enroll_body(ENDPOINT_FORGE, FORGED_FIELDS))
    stray = device_id_of(d)
    if stray:
        a.call("DELETE", f"/api/human-threads/push/subscriptions/{stray}")
        record("forged-identity-enrollment", "FAIL", "forged fields accepted: a device was created")
    elif is_guard_refusal(s, d, code=IDENTITY_CODE):
        record("forged-identity-enrollment", "PASS", f"status={s} code={IDENTITY_CODE}")
    else:
        record("forged-identity-enrollment", "AMBIGUOUS", f"status={s}: not the identity guard refusal")

    # Cross-user mutation: B deletes A's device. Acceptable only as a no-op
    # (200 removed:false) or a concealed 404. Device integrity is proven at
    # the end, when A deletes the same device and must get removed:true.
    s, d = b.call("DELETE", f"/api/human-threads/push/subscriptions/{dev_a}")
    if s == 200 and removed_of(d) is True:
        record("cross-user-delete", "FAIL", "B removed A's device (200 removed:true)")
    elif s == 200 and removed_of(d) is False:
        record("cross-user-delete", "PASS", "no-op: 200 removed:false (integrity checked at end)")
    elif s == 404:
        record("cross-user-delete", "PASS", "concealed 404 (integrity checked at end)")
    else:
        record("cross-user-delete", "AMBIGUOUS", f"status={s}: neither removed:false nor concealed 404")

    # Cross-user reassignment: B may not take A's endpoint.
    s, d = b.call("POST", "/api/human-threads/push/subscriptions", enroll_body(ENDPOINT_A))
    stray = device_id_of(d)
    if stray:
        b.call("DELETE", f"/api/human-threads/push/subscriptions/{stray}")
        record("cross-user-endpoint-reassign", "FAIL", "B took A's endpoint (200)")
    elif is_guard_refusal(s, d, text=OWNED_ELSEWHERE_TEXT):
        record("cross-user-endpoint-reassign", "PASS", f"status={s} owned-elsewhere guard")
    else:
        record("cross-user-endpoint-reassign", "AMBIGUOUS", f"status={s}: not the owned-elsewhere guard refusal")

    # B's own mutation still works: the cross-user no-op above is meaningful.
    s, d = b.call("DELETE", f"/api/human-threads/push/subscriptions/{dev_b}")
    if s == 200 and removed_of(d) is True:
        record("owner-control-B-own-delete", "PASS", "B removed its own device (200 removed:true)")
    else:
        fail_invalid("owner-control-B-own-delete", f"status={s}; B cannot remove its own device")

    # Bad push destinations with valid keys, so only the endpoint can refuse.
    # Every destination is tested; none stops the run.
    for endpoint in BAD_ENDPOINTS:
        label = endpoint.split("/")[2]
        s, d = a.call("POST", "/api/human-threads/push/subscriptions", enroll_body(endpoint))
        stray = device_id_of(d)
        if stray:
            a.call("DELETE", f"/api/human-threads/push/subscriptions/{stray}")
            record(f"bad-endpoint:{label}", "FAIL", "destination accepted (a device was created)")
        elif is_guard_refusal(s, d, text=ENDPOINT_TEXT):
            record(f"bad-endpoint:{label}", "PASS", f"status={s} endpoint allow-list guard")
        else:
            record(f"bad-endpoint:{label}", "AMBIGUOUS", f"status={s}: not the endpoint allow-list guard")

    # Optional cross-org: third account C. The driver confirms C is a distinct
    # live human; the org boundary is the operator's fixture.
    if c is None:
        record("cross-org-mutation", "OPTIONAL-NOT-RUN", "no --cookie-c-file supplied")
    else:
        sc, dc = c.call("GET", "/api/human-threads/me")
        id_c = dc.get("human", {}).get("id") if isinstance(dc, dict) else None
        if sc != 200 or not id_c or id_c in (id_a, id_b):
            record("cross-org-mutation", "AMBIGUOUS", f"C /me={sc}; not a distinct live human")
        else:
            s, d = c.call("DELETE", f"/api/human-threads/push/subscriptions/{dev_a}")
            if s == 200 and removed_of(d) is True:
                record("cross-org-mutation", "FAIL", "cross-org C removed A's device")
            elif (s == 200 and removed_of(d) is False) or s == 404:
                record("cross-org-mutation", "PASS", f"status={s} no-op (integrity checked at end)")
            else:
                record("cross-org-mutation", "AMBIGUOUS", f"status={s}: neither removed:false nor 404")

    # Device integrity: A deletes its original device. removed:true proves the
    # row is still A's and live after every B and C attempt. removed:false means
    # a foreign mutation changed it (FAIL). This is the only integrity proof;
    # a bare 200 or an idempotent no-op is not used.
    s, d = a.call("DELETE", f"/api/human-threads/push/subscriptions/{dev_a}")
    if s == 200 and removed_of(d) is True:
        record("device-integrity-after-B-and-C-attempts", "PASS", "A's original device still live: removed:true")
    elif s == 200 and removed_of(d) is False:
        record("device-integrity-after-B-and-C-attempts", "FAIL", "A's device was changed or revoked by another attempt")
    else:
        record("device-integrity-after-B-and-C-attempts", "AMBIGUOUS", f"status={s}: no removed:true proof")
    return results


def summarize(results):
    verdicts = {r["verdict"] for r in results}
    if "FAIL" in verdicts:
        return 1
    if "INVALID" in verdicts:
        return 2
    if "AMBIGUOUS" in verdicts:
        return 4
    if "OPTIONAL-NOT-RUN" in verdicts:
        return 5
    return 0


def self_check():
    """Offline classifier check. No board, no network."""
    cases = [
        ("ok:true success is not a refusal", 400, {"ok": True, "code": "x", "error": ENDPOINT_TEXT}, ENDPOINT_TEXT, False),
        ("device_id success is not a refusal", 400, {"device_id": "d", "error": ENDPOINT_TEXT, "code": "rejected"}, ENDPOINT_TEXT, False),
        ("ok:false with intended text is a refusal", 400, {"ok": False, "code": "rejected", "error": ENDPOINT_TEXT}, ENDPOINT_TEXT, True),
        ("intended endpoint text", 400, {"error": ENDPOINT_TEXT, "code": "rejected", "revision": None}, ENDPOINT_TEXT, True),
        ("unrelated expired sign-in is not the endpoint guard", 401, {"error": "sign in again", "code": "signed_out"}, ENDPOINT_TEXT, False),
        ("404 is never a refusal PASS", 404, {"error": "not found", "code": "rejected"}, ENDPOINT_TEXT, False),
        ("503 is never a refusal PASS", 503, {"error": ENDPOINT_TEXT, "code": "rejected"}, ENDPOINT_TEXT, False),
        ("non-JSON body is never a refusal PASS", 400, None, ENDPOINT_TEXT, False),
        ("forged identity needs the exact code", 400, {"error": "x", "code": IDENTITY_CODE}, None, True),
        ("another identity code does not satisfy identity", 400, {"error": "x", "code": "rejected"}, None, False),
    ]
    failed = []
    for name, status, data, text, want in cases:
        code = IDENTITY_CODE if name.startswith(("forged", "another")) else None
        got = is_guard_refusal(status, data, code=code, text=text)
        if got != want:
            failed.append(name)
        print(json.dumps({"case": name, "ok": got == want}))
    print(json.dumps({"self_check": "PASS" if not failed else "FAIL", "failed": failed}))
    return 0 if not failed else 1


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv == ["--self-check"]:
        return self_check()
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--base-url", required=True)
    p.add_argument("--cookie-a-file", required=True)
    p.add_argument("--cookie-b-file", required=True)
    p.add_argument("--cookie-c-file")
    p.add_argument("--binary", required=True)
    p.add_argument("--state-dir", required=True)
    p.add_argument("--receipt", required=True)
    args = p.parse_args(argv)
    try:
        verify_target(args)
        results = run(args)
    except NotRun as exc:
        print(json.dumps({"verdict": "NOT RUN", "detail": str(exc)}))
        return 3
    except Invalid as exc:
        print(json.dumps({"verdict": "INVALID", "detail": str(exc)}))
        return 2
    code = summarize(results)
    labels = {0: "ALL PASS", 1: "FAIL", 2: "INVALID", 4: "UNRESOLVED (not a pass)", 5: "CORE PASS, OPTIONAL NOT RUN (qualified)"}
    print(json.dumps({"overall": labels.get(code, "?"), "exit": code}))
    return code


if __name__ == "__main__":
    sys.exit(main())
