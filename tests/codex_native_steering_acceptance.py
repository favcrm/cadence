#!/usr/bin/env python3
"""CAD-1583 INDEPENDENT acceptance — Codex app-server stale-turn steer refusal.

Authored independently of the implementer; the implementer may not edit or
weaken this file. This is the ticket-mandated bad-case check: it exercises
the REAL installed `codex app-server` (stdio transport) and proves the
provider-side `expectedTurnId` precondition fails closed — stale current-turn
guidance is refused by the runtime and is never queued into the successor
turn. It does NOT test Cadence adapter code; it verifies the runtime
behavior the adapter relies on.

INVOCATION (opt-in):

    python3 tests/codex_native_steering_acceptance.py            # handshake probe only (default)
    python3 tests/codex_native_steering_acceptance.py --run      # full behavioral check
    CODEX_STEER_ACCEPTANCE_RUN=1 python3 tests/...               # equivalent to --run

  Default (no --run): spawns `codex app-server --listen stdio://`, performs
  ONLY the initialize handshake, prints the negotiated userAgent, and exits.
  No thread is started and NO provider inference is launched, so the file is
  safe to execute inside the full test suite. If the `codex` CLI is absent,
  the default mode prints SKIP and exits 0.

  --run: performs the full check below against a single NEW ephemeral thread
  (thread/start "ephemeral": true — nothing is persisted to codex history)
  rooted in a scratch cwd. It launches real provider turns (model inference).
  The thread runs approvalPolicy "never" — the managed-Cadence posture —
  inside a read-only sandbox; any approval/elicitation request would be
  declined and flagged as a protocol failure. The controllable boundary is
  a long-running `sleep` shell command keeping a turn open deterministically.
  Requires the host's normal `codex login` authentication; no credentials are
  read, copied or printed. --run with a missing/broken `codex` binary exits
  2 (BLOCKED), never a false-green skip.

WHAT --run PROVES:

  1. initialize negotiates userAgent "cadence-agent/<server-version> ...";
     the server version token must equal the expected version (0.161.0,
     overridable via --expect-version) — the same admission signal the
     adapter gates steering on.
  2. Turn A runs to completion. Turn B starts and stays active long
     enough to steer (a `sleep 30` shell command). The thread is verified
     ephemeral BEFORE any inference runs.
  3. STALE GUARD: `turn/steer` with expectedTurnId = <completed turn A>
     while turn B is active MUST produce a genuine JSON-RPC error. A
     "successful" reply — even with a wrong/missing turnId — is an
     uncertain outcome, not a refusal, and FAILS this check. A second
     stale steer with a never-existent turn id is refused the same way.
  4. POSITIVE CONTROL: `turn/steer` with expectedTurnId = <live turn B>
     MUST be accepted with result.turnId == turnB — so a runtime that
     refuses ALL steering cannot satisfy this check.
  5. NO LEAK: after turn B completes, the stale guidance marker text is
     ABSENT from turn B's completed payload and every inbound server
     notification/request, while the accepted live-steer marker IS
     present in turn B — proving refused input was not silently queued
     into the successor turn. (Ephemeral threads persist nothing and
     refuse turn-listing; the leak surface is exactly inbound traffic.)

EXIT CODES: 0 pass (or skip/probe-ok), 1 behavioral assertion FAILED,
2 BLOCKED/INCONCLUSIVE (auth/quota, refused live steer, timeouts, protocol
surprises — reported, never hidden). Every run writes an evidence JSON
(--evidence; default <scratch>/evidence.json) recording userAgent, resolved
model, turn ids, raw steer replies, the server-request log and per-assertion
results.

Environment: CODEX (binary path); CODEX_HOME is intentionally NOT overridden
(normal installed CLI auth); session-marker env vars are scrubbed from the
child. Scratch defaults to /tmp/cadence-codex-0161-omp-20261019/refusal.
"""
import argparse
import hashlib
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

CLIENT_NAME = "cadence-agent"          # what the Cadence adapter sends
CLIENT_VERSION = "0.1.0"
STALE_MARKER = "CAD1583_STALE_GUIDANCE_7f3a"
STALE_MARKER_2 = "CAD1583_STALE_GUIDANCE_9b1c"
LIVE_MARKER = "CAD1583_LIVE_GUIDANCE_4e2d"
FABRICATED_TURN = "turn-cad1583-never-existed"
ENV_SCRUB = ("CODEX_THREAD_ID", "CODEX_SESSION_ID", "CLAUDE_CODE_SESSION_ID")
BLOCKER_PAT = ("unauthorized", "401", "rate limit", "rate_limit",
               "quota", "insufficient", "forbidden", "403", "login")

PROMPT_A = (
    "This is an automated protocol conformance check. Using the shell/exec "
    "tool, run exactly this one command: printf 'cad1583-a-ran\\n' — then "
    "reply with the single word DONE. Do not run anything else."
)
PROMPT_B = (
    "This is an automated protocol conformance check. Using the shell/exec "
    "tool, run exactly this one command: sleep 30 — wait for it to finish, "
    "then reply with the single word DONE. Do not run anything else."
)


class Blocked(Exception):
    """Environment/protocol blocker — not a guard verdict."""


class AppServer:
    """Newline-delimited JSON-RPC client over `codex app-server` stdio."""

    def __init__(self, binary, cwd, stderr_path):
        env = {k: v for k, v in os.environ.items() if k not in ENV_SCRUB}
        self._stderr = open(stderr_path, "w")
        self.proc = subprocess.Popen(
            [binary, "app-server", "--listen", "stdio://"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self._stderr, text=True, cwd=str(cwd), env=env)
        self.inbox = queue.Queue()
        self.events = []            # notifications seen, in order
        self.server_requests = []   # server->client requests handled
        self.unexpected_requests = False  # set on any approval/elicitation
        self._next = 0
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                self.inbox.put(json.loads(line))
            except ValueError:
                self.inbox.put({"_unparsed": line[:500]})

    def send(self, obj):
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def _handle_server_request(self, msg):
        method = msg.get("method", "")
        rid = msg.get("id")
        self.server_requests.append({"method": method, "id": rid,
                                     "params": msg.get("params")})
        # approvalPolicy is "never" (managed-Cadence posture): any approval
        # or elicitation request is a protocol surprise — decline it and
        # flag it; the run asserts none arrived. Anything else gets a
        # JSON-RPC error so that call fails rather than hanging.
        self.unexpected_requests = True
        if method in ("item/commandExecution/requestApproval",
                      "item/fileChange/requestApproval"):
            reply = {"id": rid, "result": {"decision": "decline"}}
        elif method == "mcpServer/elicitation/request":
            reply = {"id": rid, "result": {"action": "decline"}}
        else:
            reply = {"id": rid, "error": {
                "code": -32601,
                "message": f"acceptance check does not service {method}"}}
        self.send(reply)

    def wait_for(self, pred, timeout, what, after=0):
        """Return the first message matching pred.

        Events collected during earlier waits are rescanned first (a
        notification may arrive while a different wait is in flight);
        ``after`` skips events consumed before a marked point so a stale
        same-method notification from a previous turn is never re-matched.
        """
        deadline = time.monotonic() + timeout
        seen = after
        while True:
            while seen < len(self.events):
                msg = self.events[seen]
                seen += 1
                if pred(msg):
                    return msg
            left = deadline - time.monotonic()
            if left <= 0:
                raise Blocked(f"timed out ({timeout}s) waiting for {what}")
            try:
                msg = self.inbox.get(timeout=left)
            except queue.Empty:
                raise Blocked(f"timed out ({timeout}s) waiting for {what}")
            if msg.get("method") and "id" in msg:
                self._handle_server_request(msg)
                continue
            if msg.get("method"):
                self.events.append(msg)
            if pred(msg):
                return msg

    def request(self, method, params, timeout=15):
        self._next += 1
        rid = self._next
        self.send({"id": rid, "method": method, "params": params})
        msg = self.wait_for(lambda m: m.get("id") == rid, timeout,
                            f"reply to {method}")
        if "error" in msg:
            return {"error": msg["error"]}
        return {"result": msg.get("result")}

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        self.proc.terminate()
        try:
            self.proc.wait(5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(5)
        self._stderr.close()

    def stderr_tail(self):
        try:
            self._stderr.flush()
            return Path(self._stderr.name).read_text()[-2000:]
        except Exception:
            return ""


def find_turn_id(msg):
    """Locate a turn id in a notification or reply."""
    turn = msg.get("params", {}).get("turn") or msg.get("turn")
    if isinstance(turn, dict) and isinstance(turn.get("id"), str):
        return turn["id"]

    def walk(v):
        if isinstance(v, dict):
            if isinstance(v.get("id"), str):
                return v["id"]
            for x in v.values():
                r = walk(x)
                if r:
                    return r
        elif isinstance(v, list):
            for x in v:
                r = walk(x)
                if r:
                    return r
        return None
    return walk(msg)


def looks_blocked(text):
    low = (text or "").lower()
    return any(p in low for p in BLOCKER_PAT)


def main():
    ap = argparse.ArgumentParser(
        description="CAD-1583 independent stale-turn steer acceptance check "
                    "against the real installed `codex app-server`. Default "
                    "mode runs only the initialize handshake (no inference).",
        epilog="--run launches real provider turns on an owned ephemeral "
               "thread; see module docstring for what is proven.")
    ap.add_argument("--run", action="store_true",
                    help="run the full behavioral check (provider inference; "
                         "also enabled by CODEX_STEER_ACCEPTANCE_RUN=1)")
    ap.add_argument("--scratch",
                    default="/tmp/cadence-codex-0161-omp-20261019/refusal",
                    help="owned scratch dir (thread cwd, evidence, stderr log)")
    ap.add_argument("--evidence", default=None,
                    help="evidence JSON path (default <scratch>/evidence.json)")
    ap.add_argument("--expect-version", default="0.161.0",
                    help="required server version token in userAgent")
    ap.add_argument("--turn-timeout", type=float, default=180.0,
                    help="max seconds to wait for a turn to complete")
    args = ap.parse_args()
    run = args.run or os.environ.get("CODEX_STEER_ACCEPTANCE_RUN") == "1"

    binary = os.environ.get("CODEX") or shutil.which("codex")
    if not binary:
        if run:
            print("BLOCKED: --run requested but `codex` CLI is not on PATH",
                  flush=True)
            return 2
        print("SKIP: `codex` CLI not on PATH", flush=True)
        return 0
    try:
        ver = subprocess.run([binary, "--version"], capture_output=True,
                             text=True, timeout=15)
        cli_version = ver.stdout.strip() or ver.stderr.strip()
        if ver.returncode != 0 or not cli_version:
            raise RuntimeError(f"exit {ver.returncode}: {ver.stderr.strip()}")
    except Exception as exc:
        if run:
            print(f"BLOCKED: --run requested but `{binary} --version` "
                  f"failed: {exc}", flush=True)
            return 2
        print(f"SKIP: `{binary} --version` failed: {exc}", flush=True)
        return 0
    print(f"codex cli: {cli_version}")

    scratch = Path(args.scratch)
    work = scratch / "work"
    work.mkdir(parents=True, exist_ok=True)
    evidence_path = Path(args.evidence or scratch / "evidence.json")
    evidence = {
        "check": "cad-1583 stale expectedTurnId steer refusal (independent)",
        "mode": "run" if run else "probe",
        "codex_cli": cli_version,
        "source_sha256": hashlib.sha256(
            Path(__file__).read_bytes()).hexdigest(),
        "stderr_log": str(scratch / "appserver.stderr.log"),
        "assertions": [],
        "blocked": None,
    }

    def record(name, ok, detail):
        evidence["assertions"].append(
            {"name": name, "ok": bool(ok), "detail": str(detail)})
        print(("PASS" if ok else "FAIL") + f" {name}: {detail}", flush=True)
        return ok

    def finalize(code):
        evidence["exit_code"] = code
        try:
            evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
            print(f"evidence: {evidence_path}", flush=True)
        except Exception as exc:
            print(f"evidence write failed: {exc}", flush=True)
        return code

    def fail(name, detail):
        record(name, False, detail)
        raise SystemExit(finalize(1))

    server = AppServer(binary, work, scratch / "appserver.stderr.log")
    try:
        # ---- initialize handshake (default mode stops here) -------------
        hello = server.request("initialize", {
            "clientInfo": {"name": CLIENT_NAME, "version": CLIENT_VERSION},
            "capabilities": {"experimentalApi": True}}, timeout=20)
        if "error" in hello:
            txt = json.dumps(hello["error"])
            if looks_blocked(txt):
                evidence["blocked"] = f"initialize refused: {txt}"
                return finalize(2)
            fail("initialize", f"server error: {txt}")
        result = hello["result"] or {}
        ua = result.get("userAgent", "")
        evidence["userAgent"] = ua
        print(f"userAgent: {ua}")
        server.send({"method": "initialized", "params": {}})
        if not run:
            print("probe mode: initialize handshake only; "
                  "pass --run for the behavioral check", flush=True)
            return finalize(0)

        first = ua.split()[0] if ua.split() else ""
        name, _, version = first.partition("/")
        evidence["server_version"] = version
        if name != CLIENT_NAME or not version:
            fail("initialize userAgent family",
                 f"userAgent {ua!r} lacks cadence-agent/<version>")
        record("initialize userAgent family", True, f"userAgent={ua!r}")
        if version != args.expect_version:
            fail("server version admission",
                 f"server {version!r} != expected {args.expect_version!r}")
        record("server version admission", True,
               f"userAgent token {first!r} matches expected runtime")

        # ---- owned ephemeral thread ------------------------------------
        ts = server.request("thread/start", {
            "cwd": str(work), "ephemeral": True,
            "sandbox": "read-only", "approvalPolicy": "never"}, timeout=20)
        if "error" in ts:
            fail("thread/start", f"error: {json.dumps(ts['error'])}")
        thread = (ts["result"] or {}).get("thread") or {}
        thread_id = thread.get("id")
        evidence["thread_id"] = thread_id
        evidence["thread_ephemeral"] = thread.get("ephemeral")
        evidence["model"] = thread.get("model")
        evidence["effort"] = thread.get("reasoningEffort")
        print(f"thread {thread_id} ephemeral={thread.get('ephemeral')} "
              f"model={thread.get('model')} "
              f"effort={thread.get('reasoningEffort')}")
        if not thread_id:
            fail("thread/start", "reply carried no thread.id")
        # Verify BEFORE any inference: the check must never persist turns
        # into the operator's codex history.
        if thread.get("ephemeral") is not True:
            fail("thread is ephemeral",
                 f"ephemeral={thread.get('ephemeral')!r} — refusing to run "
                 "provider turns on a persistent thread")
        record("thread is ephemeral", True,
               "thread/start honored ephemeral=true before inference")

        def start_turn(prompt):
            mark = len(server.events)
            r = server.request("turn/start", {
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt}],
            }, timeout=20)
            if "error" in r:
                fail("turn/start", f"error: {json.dumps(r['error'])}")
            tid = find_turn_id(r["result"] or {})
            started = server.wait_for(
                lambda m: (m.get("method") == "turn/started"
                           and (tid is None
                                or find_turn_id(m) == tid)),
                20, "turn/started notification", after=mark)
            return tid or find_turn_id(started)

        def wait_completed(turn_id, timeout):
            if not turn_id:
                fail("turn correlation", "no turn id to wait on")
            msg = server.wait_for(
                lambda m: (m.get("method") == "turn/completed"
                           and find_turn_id(m) == turn_id),
                timeout, f"turn/completed for {turn_id}")
            return (msg.get("params") or {}).get("turn") or {}

        # ---- turn A: the turn that will go stale -------------------------
        turn_a = start_turn(PROMPT_A)
        evidence["turn_a"] = turn_a
        done_a = wait_completed(turn_a, args.turn_timeout)
        evidence["turn_a_status"] = done_a.get("status")
        print(f"turn A {turn_a} status={done_a.get('status')}")
        if done_a.get("status") != "completed":
            txt = json.dumps(done_a)[:400]
            if looks_blocked(txt):
                evidence["blocked"] = f"turn A ended badly: {txt}"
                return finalize(2)
            fail("turn A completed", f"status={done_a.get('status')!r}")
        record("turn A completed", True, f"turn {turn_a} completed")

        # ---- turn B: active while a stale steer targets A -----------------
        turn_b = start_turn(PROMPT_B)
        evidence["turn_b"] = turn_b

        def steer(expected, text):
            return server.request("turn/steer", {
                "threadId": thread_id, "expectedTurnId": expected,
                "input": [{"type": "text", "text": text}],
                "clientUserMessageId": f"cad1583-{expected[:12]}",
            }, timeout=10)

        stale = steer(turn_a, STALE_MARKER)
        stale2 = steer(FABRICATED_TURN, STALE_MARKER_2)
        live = steer(turn_b, LIVE_MARKER)
        evidence["stale_steer_reply"] = stale
        evidence["stale_steer_fabricated_reply"] = stale2
        evidence["live_steer_reply"] = live
        print(f"stale steer (turn A)    -> {json.dumps(stale)[:300]}")
        print(f"stale steer (fabricated)-> {json.dumps(stale2)[:300]}")
        print(f"live steer  (turn B)    -> {json.dumps(live)[:300]}")

        def refused(r, expected):
            # A stale steer must produce a genuine JSON-RPC error. A
            # "successful" reply — even one echoing a wrong/missing turnId —
            # is an uncertain provider outcome, NOT a refusal, and must
            # fail this check rather than pass as fail-closed.
            if "error" in r:
                return True, (f"json-rpc error {r['error'].get('code')}: "
                              f"{str(r['error'].get('message'))[:160]}")
            got = (r.get("result") or {}).get("turnId")
            return False, (f"unexpected success reply turnId={got!r} "
                           f"for stale target {expected!r} — outcome is "
                           "unknown, not a refusal")

        ok, why = refused(stale, turn_a)
        record("stale expectedTurnId refused while successor active", ok,
               f"expectedTurnId={turn_a}: {why}")
        ok2, why2 = refused(stale2, FABRICATED_TURN)
        record("fabricated expectedTurnId refused", ok2,
               f"expectedTurnId={FABRICATED_TURN}: {why2}")

        # Positive control — live current-turn steer must be accepted.
        live_id = (live.get("result") or {}).get("turnId")
        if "error" in live or live_id != turn_b:
            evidence["blocked"] = (
                "live steer control refused/mismatched: "
                f"{json.dumps(live)[:400]} — cannot isolate stale refusal "
                "(turn may have ended early or runtime denies steering)")
            print("INCONCLUSIVE: live steer control failed; see evidence")
            try:
                wait_completed(turn_b, args.turn_timeout)
            except Blocked:
                pass
            return finalize(2)
        record("live expectedTurnId accepted (positive control)", True,
               f"turn/steer accepted for active turn {turn_b}")

        done_b = wait_completed(turn_b, args.turn_timeout)
        evidence["turn_b_status"] = done_b.get("status")
        print(f"turn B {turn_b} status={done_b.get('status')}")
        if done_b.get("status") != "completed":
            txt = json.dumps(done_b)[:400]
            if looks_blocked(txt):
                evidence["blocked"] = f"turn B ended badly: {txt}"
                return finalize(2)
            fail("turn B completed", f"status={done_b.get('status')!r}")
        record("turn B completed", True, f"turn {turn_b} completed")

        # ---- no leak: stale guidance never reached the successor ---------
        # Observed on 0.161.0: ephemeral threads refuse turn-listing
        # methods ("ephemeral threads do not support includeTurns" /
        # "thread/turns/list"), so nothing is persisted to inspect. The
        # leak surface is therefore exactly what the server pushed to this
        # client: every notification, every turn/completed payload and
        # every server->client request. A bare thread/read still returns
        # thread metadata (incl. preview) and is checked too.
        tr = server.request("thread/read", {"threadId": thread_id},
                            timeout=20)
        if "error" in tr:
            fail("thread/read", f"error: {json.dumps(tr['error'])}")
        thread_doc = (tr.get("result") or {}).get("thread") or {}
        evidence["thread_status"] = thread_doc.get("status")

        def event_turn(e):
            p = e.get("params")
            if not isinstance(p, dict):
                return None
            if isinstance(p.get("turnId"), str):
                return p["turnId"]
            t = p.get("turn")
            if isinstance(t, dict) and isinstance(t.get("id"), str):
                return t["id"]
            return None

        turn_b_events = [e for e in server.events
                         if event_turn(e) == turn_b]
        blob_b = (json.dumps(done_b) + json.dumps(turn_b_events)
                  + json.dumps([r for r in server.server_requests
                                if event_turn(r) == turn_b]))
        inbound = (json.dumps(server.events)
                   + json.dumps(server.server_requests)
                   + json.dumps(thread_doc))
        evidence["turn_b_event_methods"] = [
            e["method"] for e in turn_b_events]
        evidence["turn_b_agent_text"] = "\n".join(
            str((e.get("params") or {}).get("item", {}).get("text"))
            for e in turn_b_events
            if e.get("method") == "item/completed"
            and isinstance(e.get("params", {}).get("item"), dict)
            and e["params"]["item"].get("type") == "agentMessage")[:600]

        started_ids = {event_turn(e) for e in server.events
                       if e.get("method") == "turn/started"}
        evidence["turn_started_ids"] = sorted(x for x in started_ids if x)

        record("stale steer #1 absent from successor turn",
               STALE_MARKER not in blob_b,
               "marker not present in turn B payload/events"
               if STALE_MARKER not in blob_b else
               "STALE GUIDANCE FOUND — refused input reached successor turn")
        record("stale steer #2 absent from successor turn",
               STALE_MARKER_2 not in blob_b,
               "marker not present in turn B payload/events"
               if STALE_MARKER_2 not in blob_b else
               "STALE GUIDANCE FOUND — refused input reached successor turn")
        record("stale guidance absent from all inbound traffic",
               STALE_MARKER not in inbound
               and STALE_MARKER_2 not in inbound,
               "no stale marker in any server->client message"
               if STALE_MARKER not in inbound
               and STALE_MARKER_2 not in inbound
               else "stale marker surfaced in server traffic")
        record("live steer joined current turn",
               LIVE_MARKER in blob_b,
               "accepted steer marker present in turn B record/traffic"
               if LIVE_MARKER in blob_b else
               "accepted steer marker not visible in turn B record")
        record("no extra turns spawned by refused steers",
               started_ids == {turn_a, turn_b},
               f"turn/started ids={evidence['turn_started_ids']}")
        record("no approval/elicitation requests under approvalPolicy=never",
               not server.unexpected_requests,
               "server honored approvalPolicy=never; nothing escalated"
               if not server.unexpected_requests else
               f"unexpected requests: "
               f"{[r['method'] for r in server.server_requests]}")

        evidence["server_request_methods"] = [
            r["method"] for r in server.server_requests]
        ok_all = all(a["ok"] for a in evidence["assertions"])
        return finalize(0 if ok_all else 1)
    except Blocked as exc:
        evidence["blocked"] = str(exc)
        evidence["stderr_tail"] = server.stderr_tail()
        if looks_blocked(evidence["stderr_tail"] + str(exc)):
            print(f"BLOCKED: {exc} (auth/quota signature in stderr)")
        else:
            print(f"INCONCLUSIVE/BLOCKED: {exc}")
        return finalize(2)
    except SystemExit:
        raise
    except Exception as exc:  # protocol surprise — report, never hide
        evidence["blocked"] = f"unexpected: {exc!r}"
        evidence["stderr_tail"] = server.stderr_tail()
        print(f"INCONCLUSIVE/BLOCKED: {exc!r}")
        return finalize(2)
    finally:
        server.close()


if __name__ == "__main__":
    sys.exit(main())
