#!/usr/bin/env python3
"""CAD-1423 prior-cookie account-transition acceptance; authored, NOT RUN.

Black-box against the real board HTTP peer. The flow uses normal live board
sessions, ordinary authorize-flow assertions, receipt-verified read-only Native
metadata, and a private endpoint-locator fixture. It never mints assertions,
changes Native state directly, or logs cookies/assertions/private values.

Fixture contract: D is A's live device consent bound to the one initial live A
cookie; E is A's historical consent under another digest (that old cookie need
not remain live); B and C are distinct live principals with their own live
devices. Assertion FIFOs are supplied by the ordinary authorize flow. A
successful same-subject A1->A2 response must set a native session cookie; that
captured A2 cookie is then used for the read-only rebind of D from A1's digest
to A2's digest. `--endpoint-file` is a private (0600) fixture file containing
only D's HTTPS endpoint locator. No source-level or synthetic response is used.

All refusal cases run while D and E are still live. The final genuine A2->B
transition must revoke D alone. This check does not claim expired public
sessions disable 30-day delivery. Missing setup/transport is NOT RUN, and
`--self-check` is offline syntax/classifier validation only.
"""

import argparse
import base64
import hashlib
import http.client
import importlib.util
import json
import os
import select
import stat
import sys
import time
import urllib.parse

HERE = os.path.dirname(os.path.realpath(__file__))
ROTATION_DRIVER = os.path.join(HERE, "consent_rotation_refusal.py")
SESSION_PATH = "/__platform/session"
ME_PATH = "/api/human-threads/me"
REBIND_PATH = "/api/human-threads/push/subscriptions/rebind"
COOKIE_NAME = "__Host-aos-board-session"
REFUSAL_STATUSES = {400, 401, 403, 409, 422}
VERIFIED_PUBLIC_HOST = None


class NotRun(Exception):
    pass


class Invalid(Exception):
    pass


def rotation_driver():
    spec = importlib.util.spec_from_file_location("cad1423_existing_rotation_driver", ROTATION_DRIVER)
    if spec is None or spec.loader is None:
        raise NotRun("read-only receipt/metadata helper is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def cookie(path):
    return rotation_driver().cookie_value(path)


def endpoint_file(path):
    try:
        info = os.stat(path, follow_symlinks=False)
        if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) != 0o600:
            raise NotRun("endpoint locator fixture must be a regular mode-0600 file")
        value = rotation_driver().endpoint_value(path)
    except OSError as exc:
        raise NotRun(f"private endpoint locator unavailable ({type(exc).__name__})")
    return value


def assertion_from_fifo(path):
    """Read one just-in-time ordinary-authorize assertion; never sign/mint."""
    try:
        if not stat.S_ISFIFO(os.stat(path, follow_symlinks=False).st_mode):
            raise NotRun("assertion input is not the named fixture FIFO")
        fd = os.open(path, os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC)
    except OSError as exc:
        raise NotRun(f"ordinary authorize FIFO unavailable ({type(exc).__name__})")
    try:
        ready, _, _ = select.select([fd], [], [], 2.0)
        if not ready:
            raise NotRun("normal board authorize flow did not provide a fresh assertion")
        value = os.read(fd, 8193).decode("ascii").strip()
    except (OSError, UnicodeDecodeError) as exc:
        raise NotRun(f"authorize assertion handoff failed ({type(exc).__name__})")
    finally:
        os.close(fd)
    if not value or len(value) > 8192 or value.count(".") != 2:
        raise NotRun("normal authorize flow yielded malformed assertion material")
    return value


def forged_signature(assertion):
    """Change only signature bytes of a captured legitimate compact assertion."""
    parts = assertion.split(".")
    if len(parts) != 3 or not parts[2]:
        raise NotRun("captured assertion is not compact signed-token syntax")
    raw = bytearray(base64.urlsafe_b64decode(parts[2] + "=" * (-len(parts[2]) % 4)))
    if not raw:
        raise NotRun("captured assertion signature is empty")
    raw[0] ^= 1
    parts[2] = base64.urlsafe_b64encode(bytes(raw)).rstrip(b"=").decode("ascii")
    return ".".join(parts)


def http_request(base_url, method, path, cookie_value=None, body=None):
    u = urllib.parse.urlsplit(base_url)
    if not VERIFIED_PUBLIC_HOST:
        raise NotRun("receipt-verified public_host is required for HTTP transport")
    conn = http.client.HTTPConnection(u.hostname, u.port, timeout=10)
    headers = {"Host": VERIFIED_PUBLIC_HOST}
    if cookie_value is not None:
        headers["Cookie"] = f"{COOKIE_NAME}={cookie_value}"
    if method == "POST":
        headers.update({"Content-Type": "application/json", "X-Cadence-Board": "1",
                        "Origin": f"{u.scheme}://{VERIFIED_PUBLIC_HOST}", "Sec-Fetch-Site": "same-origin"})
    payload = None if body is None else json.dumps(body, separators=(",", ":")).encode("utf-8")
    try:
        conn.request(method, path, body=payload, headers=headers)
        response = conn.getresponse()
        raw, status = response.read(65536), response.status
        set_cookies = response.headers.get_all("Set-Cookie", []) or []
    except (OSError, TimeoutError) as exc:
        raise NotRun(f"actual HTTP peer unavailable ({type(exc).__name__})")
    finally:
        conn.close()
    try:
        doc = json.loads(raw.decode("utf-8")) if raw else None
    except (ValueError, UnicodeDecodeError):
        doc = None
    native_cookie = None
    for header in set_cookies:
        for part in header.split(";"):
            part = part.strip()
            prefix = COOKIE_NAME + "="
            if part.startswith(prefix):
                candidate = part[len(prefix):]
                allowed = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._~-"
                if not candidate or len(candidate) > 256 or any(ch not in allowed for ch in candidate):
                    raise Invalid("HTTP peer set a malformed native session cookie")
                if native_cookie is not None:
                    raise Invalid("HTTP peer set multiple native session cookies")
                native_cookie = candidate
    return status, doc, native_cookie


def get_me(base_url, cookie_value):
    status, doc, _ = http_request(base_url, "GET", ME_PATH, cookie_value)
    human = doc.get("human") if isinstance(doc, dict) else None
    ident = human.get("id") if isinstance(human, dict) else None
    if status != 200 or not isinstance(ident, str) or not ident:
        raise NotRun("ordinary session did not resolve through the real identity peer")
    return ident


def require_invalid_cookie(base_url, cookie_value, label):
    status, _, _ = http_request(base_url, "GET", ME_PATH, cookie_value)
    if status not in (401, 403):
        if status in (404, 503):
            raise NotRun(f"{label}: identity HTTP peer absent/unavailable ({status})")
        raise Invalid(f"{label}: cookie did not produce the expected real authentication refusal")


def digest(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def snapshot(db, device_id):
    return rotation_driver().metadata(db, device_id)


def live(snapshot_value, label):
    if snapshot_value["revoked"] is not None or snapshot_value["expires"] <= time.time():
        raise NotRun(f"{label} consent is not live and unexpired")


def unchanged(before, after, label):
    if before != after:
        raise Invalid(f"{label}: consent/device metadata changed")


def revoked_only(target_before, target_after, untouched_before, untouched_after):
    if any(target_before[k] != target_after[k] for k in set(target_before) - {"revoked"}):
        raise Invalid("account transition changed target identity, session, keys, or consent metadata")
    if target_before["revoked"] is not None or target_after["revoked"] is None:
        raise Invalid("verified account transition did not revoke its prior-cookie device")
    unchanged(untouched_before, untouched_after, "different-digest same-owner device")


def call_transition(args, prior_cookie, assertion, extra=None):
    body = {"assertion": assertion}
    if extra:
        body.update(extra)
    return http_request(args.base_url, "POST", SESSION_PATH, prior_cookie, body)


def successful_transition(args, prior_cookie, assertion, expected_human, label):
    status, doc, new_cookie = call_transition(args, prior_cookie, assertion)
    if status in (404, 503):
        raise NotRun(f"{label}: actual session route absent/unavailable ({status})")
    if status != 200 or not isinstance(doc, dict) or doc.get("ok") is not True or new_cookie is None:
        raise Invalid(f"{label}: legitimate transition lacked HTTP 200, ok:true, or native Set-Cookie")
    if get_me(args.base_url, new_cookie) != expected_human:
        raise Invalid(f"{label}: returned native session did not authenticate as expected principal")
    return new_cookie


def require_refused(status, label):
    if status in (404, 503):
        raise NotRun(f"{label}: actual session route absent/unavailable ({status})")
    if status not in REFUSAL_STATUSES:
        raise Invalid(f"{label}: invalid authority was not refused with an expected 4xx status")


def run(args):
    helper = rotation_driver()
    target = argparse.Namespace(base_url=args.base_url, binary=args.binary,
                                state_dir=args.state_dir, receipt=args.receipt, socket=None)
    public_host, _ = helper.protected_driver().verify_target(target)
    if not public_host:
        raise NotRun("receipt has no verified public_host for session HTTP transport")
    global VERIFIED_PUBLIC_HOST
    VERIFIED_PUBLIC_HOST = public_host
    db = helper.open_native_db(args.state_dir)
    try:
        prior_a = cookie(args.prior_a_cookie)
        prior_b = cookie(args.prior_b_cookie)
        prior_c = cookie(args.prior_c_cookie)
        stale_a = cookie(args.stale_a_cookie)
        forged_a = cookie(args.forged_a_cookie)
        endpoint = endpoint_file(args.endpoint_file)
        if len({prior_a, prior_b, prior_c, stale_a, forged_a}) != 5:
            raise NotRun("initial A, B, C, stale and forged cookie fixtures must be distinct")
        require_invalid_cookie(args.base_url, stale_a, "stale prior cookie")
        require_invalid_cookie(args.base_url, forged_a, "forged prior cookie")
        a_id, b_id, c_id = (get_me(args.base_url, prior_a), get_me(args.base_url, prior_b),
                            get_me(args.base_url, prior_c))
        if len({a_id, b_id, c_id}) != 3:
            raise NotRun("A/B/C fixtures do not establish distinct verified principals")

        d0, e0, b0, c0 = (snapshot(db, args.device_a), snapshot(db, args.device_a_other),
                           snapshot(db, args.device_b), snapshot(db, args.device_c))
        if (d0["human_sha256"] != digest(a_id) or e0["human_sha256"] != digest(a_id)
                or d0["actor_sha256"] != e0["actor_sha256"]):
            raise NotRun("A D/E devices do not match the same verified issuer/org/subject owner")
        if (b0["human_sha256"] != digest(b_id) or c0["human_sha256"] != digest(c_id)
                or b0["actor_sha256"] in (d0["actor_sha256"], c0["actor_sha256"])
                or c0["actor_sha256"] == d0["actor_sha256"]):
            raise NotRun("B/C device owner tuples are not distinct verified principals")
        if d0["device_id"] == e0["device_id"] or d0["session_sha256"] != digest(prior_a):
            raise NotRun("D is not bound to the one initial live A cookie")
        if hashlib.sha256(endpoint.encode("utf-8")).hexdigest() != d0["endpoint_sha256"]:
            raise NotRun("private endpoint locator does not identify D's exact existing consent")
        if d0["session_sha256"] == e0["session_sha256"]:
            raise NotRun("E must be same-owner historical consent under a different session digest")
        for label, item in (("A target", d0), ("A historical device", e0),
                            ("B control", b0), ("C replay control", c0)):
            live(item, label)

        # Same-subject A1 -> A2 is a genuine successful rotation, not a
        # simultaneous-two-live-cookie assumption. Retain the native cookie.
        same_assertion = assertion_from_fifo(args.same_subject_fifo)
        d_before, e_before = snapshot(db, args.device_a), snapshot(db, args.device_a_other)
        a2 = successful_transition(args, prior_a, same_assertion, a_id, "same-subject rotation")
        unchanged(d_before, snapshot(db, args.device_a), "same-subject rotation target")
        unchanged(e_before, snapshot(db, args.device_a_other), "same-subject rotation historical device")

        # Read-only existing rebind route: only D's session digest may change.
        d_before, e_before = snapshot(db, args.device_a), snapshot(db, args.device_a_other)
        status, doc, _ = http_request(args.base_url, "POST", REBIND_PATH, a2, {"endpoint": endpoint})
        if status in (404, 503):
            raise NotRun(f"real HTTP rebind route absent/unavailable ({status})")
        if status != 200 or doc != {"device_id": args.device_a}:
            raise Invalid("A2's real HTTP rebind did not return the exact expected device id")
        d_after = snapshot(db, args.device_a)
        if (d_after["session_sha256"] != digest(a2)
                or any(d_before[k] != d_after[k] for k in set(d_before) - {"session_sha256"})
                or d_before["revoked"] is not None or d_after["revoked"] is not None):
            raise Invalid("rebind changed D beyond its session digest or failed to retain live consent")
        unchanged(e_before, snapshot(db, args.device_a_other), "rebind historical same-owner device")

        # Every following bad-case is tested before D is revoked. Valid B
        # assertions are genuine positive controls: HTTP 200, ok:true, native
        # cookie returned, and /me confirms B. Neither A consent row may change.
        for label, prior, fifo in (("missing prior cookie", None, args.missing_prior_fifo),
                                   ("forged prior cookie", forged_a, args.forged_prior_fifo),
                                   ("stale prior cookie", stale_a, args.stale_prior_fifo)):
            assertion = assertion_from_fifo(fifo)
            before_d, before_e, before_c = (snapshot(db, args.device_a),
                                             snapshot(db, args.device_a_other),
                                             snapshot(db, args.device_c))
            successful_transition(args, prior, assertion, b_id, label)
            live(snapshot(db, args.device_a), label + " target")
            live(snapshot(db, args.device_a_other), label + " historical device")
            unchanged(before_d, snapshot(db, args.device_a), label + " target")
            unchanged(before_e, snapshot(db, args.device_a_other), label + " historical device")
            unchanged(before_c, snapshot(db, args.device_c), label + " C-owned device")

        # Invalid new authority must be refused, not merely leave an already
        # revoked row unchanged. Corrupt only a newly captured legitimate proof.
        captured = assertion_from_fifo(args.forged_assertion_fifo)
        forged = forged_signature(captured)
        before_d, before_e = snapshot(db, args.device_a), snapshot(db, args.device_a_other)
        status, _, _ = call_transition(args, a2, forged)
        require_refused(status, "forged fresh assertion")
        unchanged(before_d, snapshot(db, args.device_a), "forged assertion target")
        unchanged(before_e, snapshot(db, args.device_a_other), "forged assertion historical device")

        # Consume a legitimate B assertion in C->B, prove C's exact consent
        # was revoked, then replay the exact proof beside the still-live A2.
        replay = assertion_from_fifo(args.replay_assertion_fifo)
        c_before, d_before, e_before = (snapshot(db, args.device_c),
                                        snapshot(db, args.device_a), snapshot(db, args.device_a_other))
        successful_transition(args, prior_c, replay, b_id, "first legitimate C-to-B assertion use")
        c_after = snapshot(db, args.device_c)
        if c_before["revoked"] is not None or c_after["revoked"] is None:
            raise Invalid("first legitimate C-to-B use did not revoke C's prior-cookie device")
        unchanged(d_before, snapshot(db, args.device_a), "replayed assertion A target")
        unchanged(e_before, snapshot(db, args.device_a_other), "replayed assertion A historical device")
        status, _, _ = call_transition(args, a2, replay)
        require_refused(status, "replayed assertion with live A2 cookie")
        unchanged(d_before, snapshot(db, args.device_a), "replayed assertion A target")
        unchanged(e_before, snapshot(db, args.device_a_other), "replayed assertion A historical device")

        # Body-supplied authority/identity is not accepted, with live A2 and
        # both A rows still intact as an explicit refusal oracle.
        body_assertion = assertion_from_fifo(args.body_forgery_fifo)
        before_d, before_e = snapshot(db, args.device_a), snapshot(db, args.device_a_other)
        status, _, _ = call_transition(
            args, a2, body_assertion,
            {"prior_session": "forged", "subject": "forged", "issuer": "forged",
             "organization_id": "forged", "endpoint": "forged"})
        require_refused(status, "body-claimed prior/identity fields")
        unchanged(before_d, snapshot(db, args.device_a), "body-claimed prior target")
        unchanged(before_e, snapshot(db, args.device_a_other), "body-claimed prior historical device")

        # Final legitimate A2 -> B switch is the only check that may revoke D.
        final_assertion = assertion_from_fifo(args.account_switch_fifo)
        d_before, e_before, b_before = (snapshot(db, args.device_a),
                                        snapshot(db, args.device_a_other), snapshot(db, args.device_b))
        successful_transition(args, a2, final_assertion, b_id, "verified A-to-B transition")
        d_after, e_after, b_after = (snapshot(db, args.device_a),
                                     snapshot(db, args.device_a_other), snapshot(db, args.device_b))
        if d_before["session_sha256"] != digest(a2):
            raise Invalid("final transition prior digest does not match current A2 cookie")
        revoked_only(d_before, d_after, e_before, e_after)
        unchanged(b_before, b_after, "B-owned device")
        print("PASS: real HTTP transition/refusal checks; exact Native device metadata only")
        print("Assertions, cookies, identity tuples, endpoint, and private payloads were not logged.")
        return 0
    finally:
        db.close()


def self_check():
    original = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJhIn0.AA"
    try:
        mutated = forged_signature(original)
        ok = mutated.split(".")[:2] == original.split(".")[:2] and mutated != original
    except Exception:
        ok = False
    print("self-check " + ("OK (offline syntax/classifier only; no guard evidence)" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--base-url")
    parser.add_argument("--binary")
    parser.add_argument("--state-dir")
    parser.add_argument("--receipt")
    parser.add_argument("--prior-a-cookie")
    parser.add_argument("--prior-b-cookie")
    parser.add_argument("--prior-c-cookie")
    parser.add_argument("--stale-a-cookie")
    parser.add_argument("--forged-a-cookie")
    parser.add_argument("--endpoint-file")
    parser.add_argument("--device-a")
    parser.add_argument("--device-a-other")
    parser.add_argument("--device-b")
    parser.add_argument("--device-c")
    for name in ("same-subject", "account-switch", "missing-prior", "forged-prior",
                 "stale-prior", "forged-assertion", "replay-assertion", "body-forgery"):
        parser.add_argument(f"--{name}-fifo")
    args = parser.parse_args(argv)
    if args.self_check:
        return self_check()
    required = (args.base_url, args.binary, args.state_dir, args.receipt,
                args.prior_a_cookie, args.prior_b_cookie, args.prior_c_cookie,
                args.stale_a_cookie, args.forged_a_cookie, args.endpoint_file,
                args.device_a, args.device_a_other, args.device_b, args.device_c,
                args.same_subject_fifo, args.account_switch_fifo, args.missing_prior_fifo,
                args.forged_prior_fifo, args.stale_prior_fifo, args.forged_assertion_fifo,
                args.replay_assertion_fifo, args.body_forgery_fifo)
    if not all(required):
        print("NOT RUN: missing receipt, ordinary session/device controls, endpoint locator, or authorize FIFOs")
        return 3
    try:
        return run(args)
    except NotRun as exc:
        print(f"NOT RUN: {exc}")
        return 3
    except Invalid as exc:
        print(f"INVALID: {exc}")
        return 2


if __name__ == "__main__":
    sys.exit(main())
