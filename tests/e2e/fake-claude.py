#!/usr/bin/env python3
"""A scripted stand-in for the Claude CLI (CAD-433 E2E acceptance).

The daemon launches it exactly like `claude -p --input-format stream-json
--output-format stream-json ...` through `CADENCE_CLAUDE_COMMAND`
(`python3 fake-claude.py <log-dir>`), and it speaks the same stream-json
protocol as the real CLI and the integration suite's MOCK_CLAUDE_PY:
`system/init`, `assistant` text, then one `result` per user turn, and a
`control_response` for each control request.

Instead of a model it follows a fixed script chosen by its own
`CADENCE_ALIAS` (the daemon injects it). Every `cadence` command it runs
is a tool subprocess of this process — so the daemon attributes it to
this agent, as it would a real provider's Bash tool:

- `master`: the operator's "plan a CSV export" -> `plan propose`, then
  (adversarial) an early `master dispatch` the gate must refuse; the
  daemon's `[wake]` that the plan was approved (CAD-445) -> `master
  dispatch` for each ticket of its plan (the dependency gate refuses
  the blocked one by name); a routed `[question]` -> `master escalate`
  with a summary; anything else (briefing, routed reports, later wakes)
  -> a short acknowledgement.
- a worker (`w*`): a dispatched ticket -> a `question` report and the
  turn ends; the daemon's `[answer]` message (CAD-447) carries the
  operator's answer -> commit in the ticket's worktree and file `done`
  with that sha and a PR link. On the operator's `MERGE_PROBE <board>
  <ID>` it (adversarially) POSTs the board's merge route from a child
  `curl` and replies with the answer.
- a reviewer (`r*`): a review kickoff -> a `pass` verdict on the head it
  names.

No network, no model, no credentials. Every prompt and every command it
runs, with its exit code and output, is appended to
`<log-dir>/<alias>.log` — the E2E uploads that directory on failure.
"""

import json
import os
import re
import subprocess
import sys

LOG_DIR = sys.argv[1]
ARGV = sys.argv[2:]
ALIAS = os.environ.get("CADENCE_ALIAS", "unknown")
SID = ""
for i, a in enumerate(ARGV):
    if a in ("--session-id", "--resume") and i + 1 < len(ARGV):
        SID = ARGV[i + 1]

# The plan the fake master proposes: two tickets, the second after the
# first, both for w1 — so a dispatch of the second is refused until the
# first is done.
PLAN = """---
title: CSV export
goal: Users can download their records as a CSV file
non_goals: [xlsx]
---

The operator asked for a CSV export.

## Export endpoint
size: S
agent: w1

A GET endpoint that streams the records as CSV.

### Acceptance
- [ ] GET /export.csv returns every record with a header row

## Export button
size: S
agent: w1
depends_on: 1

A button on the records page.

### Acceptance
- [ ] the records page links to /export.csv
"""

REFLECTION = (
    "## Expected\nthe export works\n## Evidence\nfake worker\n## Cause\nnone\n"
    "## Correction\nnone\n## Lesson\nnone\n## Next\nnone\n"
)

ID = re.compile(r"\b([A-Z][A-Z0-9]{0,9}-\d+)\b")
SHA = re.compile(r"\b([0-9a-f]{40})\b")


def log(line):
    os.makedirs(LOG_DIR, exist_ok=True)
    with open(os.path.join(LOG_DIR, ALIAS + ".log"), "a") as f:
        f.write(line.rstrip("\n") + "\n")


def emit(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def say(text):
    emit({"type": "assistant", "session_id": SID,
          "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}})


def finish(text):
    emit({"type": "result", "subtype": "success", "is_error": False,
          "session_id": SID, "num_turns": 1, "total_cost_usd": 0.0,
          "result": text, "stop_reason": "end_turn", "permission_denials": []})


def run(args, stdin=None, cwd=None, quiet=False):
    """One tool subprocess; returns (rc, stdout, stderr). `quiet` logs
    the command and its exit code only (a poll)."""
    r = subprocess.run(args, input=stdin, capture_output=True, text=True, cwd=cwd)
    detail = "" if quiet and r.returncode == 0 else r.stdout + r.stderr
    log("$ %s -> %d\n%s" % (" ".join(args), r.returncode, detail))
    return r.returncode, r.stdout, r.stderr


def cadence(*args, stdin=None, quiet=False):
    return run(["cadence", *args], stdin=stdin, quiet=quiet)


def as_json(text):
    try:
        return json.loads(text)
    except ValueError:
        return None


# ---- master ---------------------------------------------------------------

plan_tickets = []


def master_turn(text):
    if "plan a CSV export" in text:
        rc, out, err = cadence("plan", "propose", "--project", "demo", "--file", "-", stdin=PLAN)
        got = as_json(out) or {}
        if rc != 0 or "epic" not in got:
            return "I could not propose the plan: " + (err or out).strip()
        plan_tickets[:] = got.get("tickets", [])
        # Adversarial: dispatch before the operator approved. The gate
        # must refuse it; the reply carries the daemon's answer verbatim.
        rc, dout, derr = cadence("master", "dispatch", plan_tickets[0])
        early = ("REFUSED " if rc != 0 else "ACCEPTED ") + (derr or dout).strip()
        return "Proposed plan %s (%s) — approve it on the plan card.\n" \
               "Early dispatch of %s: %s" % (got["epic"], ", ".join(plan_tickets),
                                           plan_tickets[0], early)
    if text.startswith("[wake]") and "approved" in text:
        # CAD-445: the daemon's own wake after the operator's Approve —
        # no operator nudge. Dispatch every ticket of the plan; the
        # dependency gate refuses the blocked one by name.
        lines = []
        for ticket in plan_tickets:
            rc, out, err = cadence("master", "dispatch", ticket)
            lines.append("%s: %s" % (ticket, "dispatched" if rc == 0 else
                                     "not dispatched — " + (err or out).strip()))
        # CAD-324: this provider compacted its context mid-session — the
        # daemon's next message to the master (w1's routed question)
        # opens with a continuity pack.
        emit({"type": "system", "subtype": "compact_boundary",
              "compact_metadata": {"trigger": "auto", "pre_tokens": 140000},
              "session_id": SID})
        return "Dispatch:\n" + "\n".join(lines) if lines else "No plan to dispatch."
    if text.startswith("[question]"):
        issue = ID.search(text).group(1)
        report = re.search(r"Report: \S+/reports/(\S+)", text).group(1)
        summary = ("%s asks which delimiter the CSV export uses. Options: comma or "
                   "semicolon. I recommend comma, the RFC 4180 default." % issue)
        rc, out, err = cadence("master", "escalate", issue, report, "--file", "-", stdin=summary)
        return "Escalated %s to you: %s" % (issue, "ok" if rc == 0 else (err or out).strip())
    return "Noted."


# ---- worker ---------------------------------------------------------------

# The dispatched ticket's worktree, kept across turns of this process —
# the kickoff turn files the question, the `[answer]` turn works in it.
work = {}


def merge_probe(text):
    """Adversarial: the worker presses the board's Merge itself, from a
    child `curl` of its own process tree. The board must refuse it."""
    m = re.search(r"MERGE_PROBE (\S+) (\S+)", text)
    url, issue = m.group(1), m.group(2)
    rc, out, err = run(["curl", "-sS", "-X", "POST", "-H", "Content-Type: application/json",
                        "-H", "X-Cadence-Board: 1", "-d", "{}", "-w", "\nHTTP %{http_code}",
                        "%s/api/delivery/%s/merge" % (url, issue)])
    return "MERGE_PROBE result rc=%d\n%s%s" % (rc, out, err)


def worker_turn(text):
    if text.startswith("MERGE_PROBE "):
        return merge_probe(text)
    if "[answer]" in text:
        # CAD-447: the operator's answer arrives as a daemon message —
        # "[answer] <ID>: operator answered your question <q>.\n
        # Report: <path> ...\n\n<answer>". The work happens now.
        issue, repo = work.get("issue"), work.get("worktree")
        if not issue or not repo:
            return "Noted."
        answer = text.split("\n\n", 1)[-1].strip()
        git = ["git", "-c", "user.name=w1", "-c", "user.email=w1@example.invalid"]
        with open(os.path.join(repo, "export.csv.txt"), "w") as f:
            f.write("delimiter: %s\n" % answer)
        run([*git, "add", "-A"], cwd=repo)
        run([*git, "commit", "-qm", "%s: CSV export endpoint" % issue], cwd=repo)
        rc, sha, _ = run(["git", "rev-parse", "HEAD"], cwd=repo)
        sha = sha.strip()
        done = "---\nkind: done\nsha: %s\npr: https://github.com/acme/demo/pull/1\n---\n%s" % (
            sha, REFLECTION)
        rc, out, err = cadence("report", "file", "--task", issue, "--kind", "done", "--file",
                               "-", stdin=done)
        if rc != 0:
            return "I could not report done: " + (err or out).strip()
        return "Done %s at %s." % (issue, sha)
    # The dispatch kickoff: "... — <ID>: <title>. Your worktree exists:
    # <path> (branch ...". Anything else is acknowledged.
    m = re.search(r"\b([A-Z][A-Z0-9]{0,9}-\d+): .*?Your worktree exists: (\S+) \(branch", text,
                  re.S)
    if not m:
        return "Noted."
    issue, worktree = m.group(1), m.group(2)
    work["issue"], work["worktree"] = issue, worktree
    q = ("---\nkind: question\noptions: [comma, semicolon]\n"
         "impact: the export's delimiter is visible to every user\n---\n"
         "Which delimiter should the CSV export use?\n\n" + REFLECTION)
    rc, out, err = cadence("report", "file", "--task", issue, "--kind", "question", "--file", "-",
                           stdin=q)
    got = as_json(out) or {}
    question = got.get("report") or got.get("name") or ""
    question = os.path.basename(question)
    if rc != 0 or not question:
        return "I could not ask my question: " + (err or out).strip()
    return "Asked %s on %s; waiting for the answer." % (question, issue)


# ---- reviewer -------------------------------------------------------------

def reviewer_turn(text):
    if "--kind verdict" not in text:
        return "Noted."
    issue = ID.search(text).group(1)
    sha = SHA.search(text).group(1)
    v = "---\nverdict: pass\nsha: %s\n---\nPASS: the endpoint streams every record.\n" % sha
    rc, out, err = cadence("report", "file", "--task", issue, "--kind", "verdict", "--file", "-",
                           stdin=v)
    return ("PASS on %s at %s." % (issue, sha)) if rc == 0 else \
        "I could not file my verdict: " + (err or out).strip()


def split_pack(text):
    """A continuity pack (CAD-324) precedes the message it rides. The
    pack names its nonce on the first line and ends at the line
    `[End of continuity pack <nonce> — the message for this turn
    follows.]` — the same split the daemon's `continuity::split` does
    (the header quotes that line mid-sentence, so only a match at a
    line start ends the pack)."""
    if not text.startswith("[Cadence continuity pack"):
        return "", text
    nonce = text[len("[Cadence continuity pack"):].lstrip().split(" ")[0]
    if not nonce:
        return "", text
    end = ("[End of continuity pack %s — the message for this turn follows.]"
           % nonce)
    i = text.find("\n" + end)
    if i < 0:
        return "", text
    j = i + 1 + len(end)
    return text[:j], text[j:].lstrip("\n")


def turn(text):
    pack, text = split_pack(text)
    if pack:
        log("~ continuity pack (%d bytes) rode this prompt" % len(pack))
    if ALIAS == "master":
        return master_turn(text)
    if ALIAS.startswith("w"):
        return worker_turn(text)
    if ALIAS.startswith("r"):
        return reviewer_turn(text)
    return "Noted."


log("# started argv=%s cwd=%s" % (" ".join(ARGV), os.getcwd()))
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if msg.get("type") == "control_request":
        # An idle CLI acknowledges and does nothing; no turn is ever
        # interrupted in this journey.
        emit({"type": "control_response",
              "response": {"subtype": "success", "request_id": msg.get("request_id"),
                           "response": {}}})
        continue
    if msg.get("type") != "user":
        continue
    content = msg["message"]["content"]
    text = content if isinstance(content, str) else \
        " ".join(b.get("text", "") for b in content if isinstance(b, dict))
    log("> " + text.replace("\n", "\n> "))
    emit({"type": "system", "subtype": "init", "session_id": SID,
          "model": "fake-claude", "tools": ["Bash"]})
    try:
        reply = turn(text)
    except Exception as e:  # a script bug must show in the thread, not hang
        reply = "fake-claude error: %r" % (e,)
    log("< " + reply)
    say(reply)
    finish(reply)
