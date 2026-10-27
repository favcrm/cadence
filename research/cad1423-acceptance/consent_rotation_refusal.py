#!/usr/bin/env python3
"""CAD-1423 device-scoped consent rotation acceptance; authored, NOT RUN.

One isolated real-guard flow, derived from the ticket. It reuses the protected
host_bridge_refusal.py process/receipt/container verifier and RPC relay without
editing that driver. Product DTOs checked against current source:
  POST /api/human-threads/push/subscriptions/rebind {endpoint}
  human_push_rebind {token, endpoint}
  POST /api/internal/notifications/prepare {intent_id, device_id}
  human_notification_delivery_prepare {proof, intent_id, device_id}

The rebind response is exactly {device_id: <existing id>} for the owner and
{device_id: null} for concealed non-owner/no-match. HTTP and RPC success frames
are parsed only by their documented native shapes. Endpoint/cookies/proofs and
all database identity/key/payload fields stay in memory and are never logged.

Fixture prerequisites (not bootstrapped here): a normal same-sub sign-in has
rotated session A to current session B; device D is A's existing consent and
same-human device E is separately enrolled under a different older session;
verified human C has its own device F; there are four distinct pending, eligible
intent IDs; current cookies are ordinary live sessions. Four named FIFOs are
owned by the isolated host fixture signer, one per prepare; each yields a fresh
host-signed 5-second proof bound to the supplied operation, immediately on read.
No signing/minting occurs here. The fixture must keep the signer outside Native.

Metadata evidence is read directly from the exact native DB named by the
receipt-verified state-dir, via SQLite URI mode=ro and query_only. The read-only
queries use the current human_push_devices and human_notification_prefs schema.
They hash endpoint, keys, preferences and exact verified identity tuple; raw
values are neither persisted nor printed. There are no writes, authority
changes, session minting, synthetic keys, or natural-expiry waits.

Flow: establish real eligible HTTP+RPC prepare controls before logout using
separate intent references; prove A's D rebind and C's own F controls through
both peers; after each rebind attempt read D and E afresh; exercise wrong-user
C→D rebind through both peers and prove D and E unchanged after each attempt;
ordinary logout of A's current session; prove D alone is revoked while E's
old-session consent remains unchanged; then use two new eligible intent refs
and fresh signer proofs to show both real prepare peers return only
{state:"suppressed"}, never private payload. A positive prepare may create its
normal bounded delivery lease; intent refs must be dedicated to this acceptance.
Expired/unusable proofs, false positive controls, absent routes and transport
or fixture failures are INVALID/NOT RUN, never refusal PASS.

Run only with an admitted owned isolated runtime; no runtime is started here.
`--self-check` checks response classification only and is not acceptance.
"""

import argparse
import hashlib
import http.client
import importlib.util
import json
import os
import select
import socket
import sqlite3
import stat
import sys
import time
import urllib.parse

HERE = os.path.dirname(os.path.realpath(__file__))
PROTECTED_DRIVER = os.path.join(HERE, "host_bridge_refusal.py")
REBIND_PATH = "/api/human-threads/push/subscriptions/rebind"
ME_PATH = "/api/human-threads/me"
LOGOUT_PATH = "/api/session/logout"
PREPARE_PATH = "/api/internal/notifications/prepare"
REBIND_RPC = "human_push_rebind"
PREPARE_RPC = "human_notification_delivery_prepare"
NOT_RUN_HTTP = {404, 503}
PRIVATE_KEYS = {"issuer", "organization_id", "subject_id", "endpoint", "keys", "payload"}


class NotRun(Exception):
    pass


class Invalid(Exception):
    pass


def protected_driver():
    spec = importlib.util.spec_from_file_location("cad1423_protected_host_driver", PROTECTED_DRIVER)
    if spec is None or spec.loader is None:
        raise NotRun("protected target verifier is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def parse_cookie_text(text):
    name = "__Host-aos-board-session="
    for line in text.splitlines():
        line = line.strip()
        if line.startswith(name):
            value = line[len(name):]
            allowed = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._~-"
            if value and len(value) <= 256 and all(ch in allowed for ch in value):
                return value
            raise NotRun("captured board cookie is malformed")
    raise NotRun("captured file has no native board session cookie")


def cookie_value(path):
    try:
        with open(path, "r", encoding="utf-8") as fh:
            text = fh.read(16385)
    except OSError as exc:
        raise NotRun(f"ordinary session cookie unavailable ({type(exc).__name__})")
    if len(text) > 16384:
        raise NotRun("captured session-cookie file exceeds its size bound")
    return parse_cookie_text(text)


def endpoint_value(path):
    try:
        with open(path, "r", encoding="utf-8") as fh:
            endpoint = fh.read(4097).strip()
    except OSError as exc:
        raise NotRun(f"endpoint input unavailable ({type(exc).__name__})")
    if not endpoint or len(endpoint) > 4096 or not endpoint.startswith("https://"):
        raise NotRun("endpoint input is not a bounded HTTPS locator")
    return endpoint


def unwrap_rpc_relay(wrapper):
    """Unwrap only host_bridge_refusal.call_rpc_relay's documented transport wrapper."""
    if not isinstance(wrapper, dict) or set(wrapper) != {"status", "doc", "ok"}:
        raise NotRun("RPC relay returned an unexpected transport wrapper")
    frame = wrapper["doc"]
    if wrapper["status"] is not None or not isinstance(frame, dict):
        raise NotRun("RPC relay did not return a daemon frame")
    if frame.get("ok") is True:
        if set(frame) != {"ok", "result"} or wrapper["ok"] is not True:
            raise NotRun("RPC relay success frame does not match native protocol")
    elif frame.get("ok") is False:
        if set(frame) != {"ok", "error"} or not isinstance(frame["error"], dict) or wrapper["ok"] is not False:
            raise NotRun("RPC relay refusal frame does not match native protocol")
    else:
        raise NotRun("RPC relay document is not a native daemon result frame")
    return frame


def rpc_frame(container, socket_path, daemon_pid, method, params):
    driver = protected_driver()
    driver.container_check(container, daemon_pid, driver.RELAY_PATH)
    field = "token" if "token" in params else "proof"
    wrapper = driver.call_rpc_relay(container, socket_path, method, params, params[field], field)
    return unwrap_rpc_relay(wrapper)


def rpc_result(frame):
    if not isinstance(frame, dict) or set(frame) != {"ok", "result"} or frame.get("ok") is not True:
        return None
    return frame["result"]


def http_request(base_url, cookie, method, path, body=None, bearer=None):
    u = urllib.parse.urlsplit(base_url)
    conn = http.client.HTTPConnection(u.hostname, u.port, timeout=10)
    headers = {"Host": u.netloc}
    if cookie:
        headers["Cookie"] = f"__Host-aos-board-session={cookie}"
    if method in ("POST", "DELETE"):
        headers.update({"Content-Type": "application/json", "X-Cadence-Board": "1",
                       "Origin": f"{u.scheme}://{u.netloc}", "Sec-Fetch-Site": "same-origin"})
    if bearer is not None:
        headers["Authorization"] = f"Bearer {bearer}"
    payload = None if body is None else json.dumps(body, separators=(",", ":")).encode()
    try:
        conn.request(method, path, body=payload, headers=headers)
        resp = conn.getresponse()
        raw, status = resp.read(65536), resp.status
    except (OSError, TimeoutError) as exc:
        raise NotRun(f"HTTP transport unavailable ({type(exc).__name__})")
    finally:
        conn.close()
    try:
        doc = json.loads(raw.decode("utf-8")) if raw else None
    except (ValueError, UnicodeDecodeError):
        doc = None
    return status, doc


def fresh_proof(fifo):
    try:
        if not stat.S_ISFIFO(os.stat(fifo, follow_symlinks=False).st_mode):
            raise NotRun("proof input is not the fixture's named FIFO")
        # RDWR prevents the no-writer FIFO EOF condition from looking readable;
        # this process only reads, while the owned signer writes the proof.
        fd = os.open(fifo, os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC)
    except OSError as exc:
        raise NotRun(f"just-in-time signer FIFO unavailable ({type(exc).__name__})")
    try:
        ready, _, _ = select.select([fd], [], [], 2.0)
        if not ready:
            raise NotRun("host signer did not provide a fresh proof in time")
        token = os.read(fd, 8193).decode("ascii").strip()
    except (OSError, UnicodeDecodeError) as exc:
        raise NotRun(f"host signer proof handoff failed ({type(exc).__name__})")
    finally:
        os.close(fd)
    if not token or len(token) > 8192 or token.count(".") != 2:
        raise NotRun("host signer FIFO returned malformed proof material")
    return token


def sha(value):
    return hashlib.sha256(value).hexdigest()


def endpoint_matches(locator, metadata_row):
    return sha(locator.encode()) == metadata_row["endpoint_sha256"]


def canonical_hash(value):
    return sha(json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode())


def open_native_db(state_dir):
    db_path = os.path.realpath(os.path.join(state_dir, "cadence.sqlite3"))
    if not db_path.startswith(os.path.realpath(state_dir) + os.sep):
        raise NotRun("native database path escaped the receipt-verified state dir")
    uri = "file:" + urllib.parse.quote(db_path, safe="/") + "?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True, timeout=2)
        conn.execute("PRAGMA query_only=ON")
        conn.execute("SELECT 1").fetchone()
    except sqlite3.Error as exc:
        raise NotRun(f"receipt-owned native database is not readable read-only ({type(exc).__name__})")
    return conn


def metadata(conn, device_id):
    """Read one actual device row and its preferences; never returns raw private values."""
    try:
        row = conn.execute(
            "SELECT d.id,d.human,d.org,d.issuer,d.subject,d.endpoint,d.p256dh,d.auth,"
            "d.session,d.enrolled,d.expires,d.revoked,p.dm,p.mention,p.sound,p.preview,p.quiet,p.zone "
            "FROM human_push_devices d LEFT JOIN human_notification_prefs p ON p.human=d.human "
            "WHERE d.id=?", (device_id,)
        ).fetchone()
    except sqlite3.Error as exc:
        raise NotRun(f"native metadata query failed ({type(exc).__name__})")
    conn.commit()
    if row is None:
        raise NotRun("receipt-named device row is absent")
    (did, human, org, issuer, subject, endpoint, p256dh, auth, session,
     enrolled, expires, revoked, dm, mention, sound, preview, quiet, zone) = row
    actor = canonical_hash([issuer, org, subject, human])
    prefs = [dm, mention, sound, preview, quiet, zone]
    return {
        "device_id": did,
        "actor_sha256": actor,
        "endpoint_sha256": sha(endpoint.encode()),
        "keys_sha256": canonical_hash([p256dh, auth]),
        "preferences_sha256": canonical_hash(prefs),
        "created": enrolled,
        "expires": expires,
        "revoked": revoked,
        "session_sha256": session,
        "human_sha256": sha(human.encode()),
    }


def device_material(conn, device_id):
    """Return private row fields in memory only for positive prepare comparison."""
    try:
        row = conn.execute(
            "SELECT issuer,org,subject,endpoint,p256dh,auth FROM human_push_devices WHERE id=?",
            (device_id,),
        ).fetchone()
    except sqlite3.Error as exc:
        raise NotRun(f"native private metadata read failed ({type(exc).__name__})")
    conn.commit()
    if row is None:
        raise NotRun("receipt-named device row is absent")
    issuer, org, subject, endpoint, p256dh, auth = row
    return {"issuer": issuer, "org": org, "subject": subject, "endpoint": endpoint,
            "p256dh": p256dh, "auth": auth}


def intent_fingerprint(conn, intent_id, recipient):
    """Prove a pending, unused server-created intent belongs to the target recipient."""
    try:
        row = conn.execute(
            "SELECT i.room,i.recipient,i.reason,i.post,i.seq,i.created,"
            "(SELECT COUNT(*) FROM human_push_deliveries d WHERE d.intent=i.id) "
            "FROM human_notification_intents i WHERE i.id=?", (intent_id,),
        ).fetchone()
    except sqlite3.Error as exc:
        raise NotRun(f"native intent read failed ({type(exc).__name__})")
    conn.commit()
    if row is None or row[1] != recipient or row[6] != 0:
        raise NotRun("prepare input is not a fresh unused intent for the consent owner")
    membership = conn.execute(
        "SELECT cutoff FROM human_room_members WHERE room=? AND kind='human' AND member=? AND removed IS NULL",
        (row[0], recipient),
    ).fetchone()
    conn.commit()
    if membership is None or row[4] <= membership[0]:
        raise NotRun("prepare intent is not beyond the live human membership cutoff")
    return (row[0], row[1], row[2])


def same_consent_except_session(before, after, label, expected_session=None):
    fields = ("device_id", "actor_sha256", "endpoint_sha256", "keys_sha256",
              "preferences_sha256", "created", "expires", "revoked", "human_sha256")
    if any(before[k] != after[k] for k in fields):
        raise Invalid(f"{label}: rebind changed identity, endpoint, keys, preferences, creation or expiry")
    if before["revoked"] is not None or after["revoked"] is not None:
        raise Invalid(f"{label}: consent was not live before/after rebind")
    if before["expires"] <= time.time():
        raise NotRun(f"{label}: consent expired before the acceptance action")
    if expected_session is not None and after["session_sha256"] != expected_session:
        raise Invalid(f"{label}: stored session digest does not match current cookie digest")
    if expected_session is None and before["session_sha256"] != after["session_sha256"]:
        raise Invalid(f"{label}: non-owner attempt changed the target session digest")


def assert_rebind_http(status, body, device_id, label):
    if status in NOT_RUN_HTTP:
        raise NotRun(f"{label}: route absent/unavailable ({status})")
    if status != 200 or body != {"device_id": device_id}:
        raise Invalid(f"{label}: current owner's positive rebind control failed")


def assert_rebind_rpc(frame, device_id, label):
    if rpc_result(frame) != {"device_id": device_id}:
        raise Invalid(f"{label}: current owner's positive RPC rebind control failed")


def assert_hidden_http(status, body, label):
    if status in NOT_RUN_HTTP:
        raise NotRun(f"{label}: route absent/unavailable ({status})")
    if status == 200 and body == {"device_id": None}:
        return
    raise Invalid(f"{label}: wrong-user operation did not produce the exact documented concealed no-match")


def assert_hidden_rpc(frame, label):
    if rpc_result(frame) == {"device_id": None}:
        return
    raise Invalid(f"{label}: wrong-user operation did not produce the exact documented concealed no-match")


def assert_eligible_prepare_http(status, body, device, label):
    if status in NOT_RUN_HTTP:
        raise NotRun(f"{label}: route absent/unavailable ({status})")
    if status != 200 or not isinstance(body, dict):
        raise Invalid(f"{label}: legitimate eligible prepare was refused")
    required = {"lease_id", "issuer", "organization_id", "subject_id", "endpoint", "keys", "payload"}
    if set(body) != required or body["endpoint"] != device["endpoint"]:
        raise Invalid(f"{label}: eligible prepare lacked the source-defined private payload control")
    if body["issuer"] != device["issuer"] or body["organization_id"] != device["org"] or body["subject_id"] != device["subject"]:
        raise Invalid(f"{label}: eligible prepare returned a mismatched consent owner")
    if body["keys"] != {"p256dh": device["p256dh"], "auth": device["auth"]}:
        raise Invalid(f"{label}: eligible prepare returned mismatched device keys")


def assert_eligible_prepare_rpc(frame, device, label):
    result = rpc_result(frame)
    if not isinstance(result, dict):
        raise Invalid(f"{label}: legitimate eligible prepare was refused")
    required = {"lease_id", "issuer", "organization_id", "subject_id", "endpoint", "keys", "payload"}
    if set(result) != required or result["endpoint"] != device["endpoint"]:
        raise Invalid(f"{label}: eligible prepare lacked the source-defined private payload control")
    if result["issuer"] != device["issuer"] or result["organization_id"] != device["org"] or result["subject_id"] != device["subject"]:
        raise Invalid(f"{label}: eligible prepare returned a mismatched consent owner")
    if result["keys"] != {"p256dh": device["p256dh"], "auth": device["auth"]}:
        raise Invalid(f"{label}: eligible prepare returned mismatched device keys")


def assert_suppressed_http(status, body, label):
    if status in NOT_RUN_HTTP:
        raise NotRun(f"{label}: route absent/unavailable ({status})")
    if status != 200 or body != {"state": "suppressed"}:
        raise Invalid(f"{label}: valid post-logout prepare did not return exact payload-free suppression")


def assert_suppressed_rpc(frame, label):
    if rpc_result(frame) != {"state": "suppressed"}:
        raise Invalid(f"{label}: valid post-logout RPC did not return exact payload-free suppression")


def call_rebind(args, cookie, endpoint, transport):
    if transport == "http":
        return http_request(args.base_url, cookie, "POST", REBIND_PATH, {"endpoint": endpoint})
    return rpc_frame(args.rpc_container, args.socket, args.daemon_pid, REBIND_RPC,
                     {"token": cookie, "endpoint": endpoint})


def call_prepare(args, intent_id, device_id, transport):
    token = fresh_proof(getattr(args, f"proof_{transport}_{'live' if intent_id in (args.live_http_intent, args.live_rpc_intent) else 'after'}_fifo"))
    body = {"intent_id": intent_id, "device_id": device_id}
    if transport == "http":
        return http_request(args.base_url, None, "POST", PREPARE_PATH, body, bearer=token)
    return rpc_frame(args.rpc_container, args.socket, args.daemon_pid, PREPARE_RPC,
                     {"proof": token, **body})


def run(args):
    driver = protected_driver()
    verify_args = argparse.Namespace(base_url=args.base_url, binary=args.binary,
                                     state_dir=args.state_dir, receipt=args.receipt,
                                     socket=args.socket)
    _, daemon_pid = driver.verify_target(verify_args)
    args.daemon_pid = daemon_pid
    driver.container_check(args.rpc_container, daemon_pid, driver.RELAY_PATH)
    conn = open_native_db(args.state_dir)
    try:
        owner_cookie = cookie_value(args.current_cookie)
        previous_cookie = cookie_value(args.previous_cookie)
        other_cookie = cookie_value(args.other_cookie)
        endpoint_d = endpoint_value(args.endpoint_file)
        endpoint_f = endpoint_value(args.other_endpoint_file)
        owner_status, owner_me = http_request(args.base_url, owner_cookie, "GET", ME_PATH)
        other_status, other_me = http_request(args.base_url, other_cookie, "GET", ME_PATH)
        owner_id = owner_me.get("human", {}).get("id") if isinstance(owner_me, dict) else None
        other_id = other_me.get("human", {}).get("id") if isinstance(other_me, dict) else None
        if owner_status != 200 or other_status != 200 or not owner_id or not other_id or owner_id == other_id:
            raise NotRun("ordinary fixture sessions did not resolve to two different verified humans")

        d0, e0, f0 = (metadata(conn, args.device_d), metadata(conn, args.device_e), metadata(conn, args.device_f))
        material_d, material_e, material_f = (device_material(conn, args.device_d),
                                                device_material(conn, args.device_e),
                                                device_material(conn, args.device_f))
        if d0["human_sha256"] != sha(owner_id.encode()) or e0["human_sha256"] != sha(owner_id.encode()):
            raise NotRun("D and E are not two devices of the verified owner")
        if (f0["human_sha256"] != sha(other_id.encode()) or d0["device_id"] == e0["device_id"]
                or d0["actor_sha256"] != e0["actor_sha256"] or d0["actor_sha256"] == f0["actor_sha256"]):
            raise NotRun("F is not distinct from the two same-owner devices")
        current_digest = sha(owner_cookie.encode())
        if d0["session_sha256"] != sha(previous_cookie.encode()) or d0["session_sha256"] == current_digest:
            raise NotRun("fixture did not demonstrate same-sub session rotation from the enrolled session")
        if not endpoint_matches(endpoint_d, d0) or endpoint_f != material_f["endpoint"]:
            raise NotRun("endpoint locator files do not match the owned database rows")
        if d0["revoked"] is not None or d0["expires"] <= time.time():
            raise NotRun("target consent is not live and unexpired")
        if e0["revoked"] is not None or e0["expires"] <= time.time():
            raise NotRun("same-owner untouched-device consent is not live")
        if d0["session_sha256"] == e0["session_sha256"]:
            raise NotRun("same-owner device E must carry a different old session digest")
        if f0["revoked"] is not None or f0["expires"] <= time.time():
            raise NotRun("other verified actor's positive-control device is not live")
        intent_ids = (args.live_http_intent, args.live_rpc_intent,
                      args.after_http_intent, args.after_rpc_intent)
        if len(set(intent_ids)) != 4:
            raise NotRun("fixture must provide four distinct intent references")
        intents = {iid: intent_fingerprint(conn, iid, owner_id) for iid in intent_ids}
        baseline = intents[args.live_http_intent]
        if any(value[0] != baseline[0] or value[2] != baseline[2] for value in intents.values()):
            raise NotRun("fresh post-logout intents do not match the positively eligible room/reason")

        # Real eligible prepare controls, each on a distinct pending intent, before logout.
        for transport, iid in (("http", args.live_http_intent), ("rpc", args.live_rpc_intent)):
            res = call_prepare(args, iid, args.device_d, transport)
            if transport == "http":
                assert_eligible_prepare_http(*res, material_d, "pre-logout HTTP positive prepare")
            else:
                assert_eligible_prepare_rpc(res, material_d, "pre-logout RPC positive prepare")

        # Owner's live post-rotation cookie rebinds D on both real peers.
        for transport in ("http", "rpc"):
            before = metadata(conn, args.device_d)
            e_before = metadata(conn, args.device_e)
            result = call_rebind(args, owner_cookie, endpoint_d, transport)
            if transport == "http":
                assert_rebind_http(*result, args.device_d, "owner HTTP control")
            else:
                assert_rebind_rpc(result, args.device_d, "owner RPC control")
            after = metadata(conn, args.device_d)
            same_consent_except_session(before, after, f"owner {transport} rebind", sha(owner_cookie.encode()))
            if metadata(conn, args.device_e) != e_before:
                raise Invalid(f"owner {transport} rebind changed same-human device E")

        # Distinct actor C proves each peer is live using its own device F.
        for transport in ("http", "rpc"):
            before = metadata(conn, args.device_f)
            result = call_rebind(args, other_cookie, endpoint_f, transport)
            if transport == "http":
                assert_rebind_http(*result, args.device_f, "other-human HTTP positive control")
            else:
                assert_rebind_rpc(result, args.device_f, "other-human RPC positive control")
            after = metadata(conn, args.device_f)
            same_consent_except_session(before, after, f"other-human {transport} control", sha(other_cookie.encode()))

        # Each cross-human attempt is tied to fresh before/after reads of both A rows.
        for transport in ("http", "rpc"):
            d_before, e_before = metadata(conn, args.device_d), metadata(conn, args.device_e)
            result = call_rebind(args, other_cookie, endpoint_d, transport)
            if transport == "http":
                assert_hidden_http(*result, f"cross-human HTTP {transport}")
            else:
                assert_hidden_rpc(result, f"cross-human RPC {transport}")
            if metadata(conn, args.device_d) != d_before or metadata(conn, args.device_e) != e_before:
                raise Invalid(f"cross-human {transport} rebind changed an owner's device")

        # Normal logout revokes only D (current digest); E retains its old consent.
        d_before, e_before = metadata(conn, args.device_d), metadata(conn, args.device_e)
        status, _ = http_request(args.base_url, owner_cookie, "POST", LOGOUT_PATH, {})
        if status != 204:
            raise Invalid("normal current-session logout did not complete")
        d_after, e_after = metadata(conn, args.device_d), metadata(conn, args.device_e)
        if d_before["revoked"] is not None or d_after["revoked"] is None:
            raise Invalid("logout did not revoke the rebound device")
        if e_before != e_after or e_after["revoked"] is not None:
            raise Invalid("logout changed/revoked the same-human device with the old session digest")

        # Distinct fresh eligible intents, not the accepted positive controls.
        for transport, iid in (("http", args.after_http_intent), ("rpc", args.after_rpc_intent)):
            intent_fingerprint(conn, iid, owner_id)
            res = call_prepare(args, iid, args.device_d, transport)
            if transport == "http":
                assert_suppressed_http(*res, "post-logout HTTP prepare")
            else:
                assert_suppressed_rpc(res, "post-logout RPC prepare")
        print("PASS: real eligible controls, device-scoped HTTP/RPC rebind, cross-user refusal, session-scoped logout, payload-free prepare")
        print("No endpoint, cookie, proof, identity tuple, key, or payload was logged.")
        return 0
    finally:
        conn.close()


def self_check():
    cases = [
        ("valid positive shape", lambda: assert_rebind_http(200, {"device_id": "d"}, "d", "self"), True),
        ("null body is not hidden result", lambda: assert_hidden_http(200, None, "self"), False),
        ("extra fields are not hidden result", lambda: assert_hidden_http(200, {"device_id": None, "endpoint": "x"}, "self"), False),
        ("missing route is not pass", lambda: assert_hidden_http(404, {"device_id": None}, "self"), None),
        ("unavailable route is not pass", lambda: assert_hidden_http(503, {"device_id": None}, "self"), None),
        ("private prepare body fails", lambda: assert_suppressed_http(200, {"state": "suppressed", "payload": "x"}, "self"), False),
        ("suppressed exact shape", lambda: assert_suppressed_http(200, {"state": "suppressed"}, "self"), True),
    ]
    ok = True
    for label, check, expected in cases:
        try:
            check()
            got = True
        except NotRun:
            got = None
        except Invalid:
            got = False
        if got != expected:
            ok = False
            print(f"self-check mismatch: {label}")
    cookie_cases = [
        ("captured normal cookie line", "__Host-aos-board-session=abcDEF0123\n", "abcDEF0123"),
        ("missing cookie name", "other=value\n", None),
        ("malformed cookie value", "__Host-aos-board-session=bad value\n", None),
    ]
    for label, text, expected in cookie_cases:
        try:
            actual = parse_cookie_text(text)
        except NotRun:
            actual = None
        if actual != expected:
            ok = False
            print(f"self-check mismatch: {label}")
    native_ok = {"ok": True, "result": {"device_id": "d"}}
    wrapper = {"status": None, "doc": native_ok, "ok": True}
    try:
        if rpc_result(unwrap_rpc_relay(wrapper)) != {"device_id": "d"}:
            ok = False
            print("self-check mismatch: wrapped native RPC success")
    except NotRun:
        ok = False
        print("self-check mismatch: wrapped native RPC success rejected")
    for malformed in (
        {"status": None, "doc": native_ok, "ok": False},
        {"status": None, "doc": {"ok": True, "data": {"device_id": "d"}}, "ok": True},
        {"status": 404, "doc": {"ok": True, "result": {}}, "ok": True},
    ):
        try:
            unwrap_rpc_relay(malformed)
            ok = False
            print("self-check mismatch: malformed RPC wrapper accepted")
        except NotRun:
            pass
    if not endpoint_matches("https://push.example.invalid/e", {
            "endpoint_sha256": sha(b"https://push.example.invalid/e")}):
        ok = False
        print("self-check mismatch: endpoint locator digest comparison")
    print("self-check " + ("OK (classifier/interface only; no runtime evidence)" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--base-url")
    parser.add_argument("--binary")
    parser.add_argument("--state-dir")
    parser.add_argument("--receipt")
    parser.add_argument("--socket")
    parser.add_argument("--rpc-container")
    parser.add_argument("--previous-cookie", help="ordinary pre-rotation cookie; digest checked only, never presented")
    parser.add_argument("--current-cookie")
    parser.add_argument("--other-cookie")
    parser.add_argument("--endpoint-file")
    parser.add_argument("--other-endpoint-file")
    parser.add_argument("--device-d")
    parser.add_argument("--device-e")
    parser.add_argument("--device-f")
    parser.add_argument("--live-http-intent", type=int)
    parser.add_argument("--live-rpc-intent", type=int)
    parser.add_argument("--after-http-intent", type=int)
    parser.add_argument("--after-rpc-intent", type=int)
    for transport in ("http", "rpc"):
        for phase in ("live", "after"):
            parser.add_argument(f"--proof-{transport}-{phase}-fifo")
    args = parser.parse_args(argv)
    if args.self_check:
        return self_check()
    required = (args.base_url, args.binary, args.state_dir, args.receipt, args.socket,
                args.rpc_container, args.previous_cookie, args.current_cookie, args.other_cookie,
                args.endpoint_file, args.other_endpoint_file, args.device_d,
                args.device_e, args.device_f, args.live_http_intent, args.live_rpc_intent,
                args.after_http_intent, args.after_rpc_intent, args.proof_http_live_fifo,
                args.proof_rpc_live_fifo, args.proof_http_after_fifo, args.proof_rpc_after_fifo)
    if not all(required):
        print("NOT RUN: missing receipt-verified isolated target, normal fixture inputs, or host signer FIFOs")
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
