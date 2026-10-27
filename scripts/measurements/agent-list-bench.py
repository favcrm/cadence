#!/usr/bin/env python3
"""CAD-1266: safe agent-list latency benchmark.

Seeds an isolated daemon state with a ~400-agent registry whose rows can
NEVER be launched, proves that no provider or tmux process appears, then
times the agent list, the other RPCs and the board routes against it.

Why the rows cannot launch (code paths, checked against origin/main):
  * src/daemon/serve.rs `relaunch_agents` (daemon start) skips every row
    with `enabled = 0` before it reaches `launch_actor`.
  * src/daemon/timers.rs `auto_resume_tick` only resumes a stopped agent
    that has a QUEUED message (`store.queued_for_stopped`); the seed
    queues none and refuses to run if it finds one.
  * `start_actor_locked` (src/daemon.rs) is reached only from the two
    paths above, `agent register`/`agent resume` RPCs and the master
    wake; the harness sends none of them.
  * Defence in depth: the daemon runs with a PATH whose only entries are
    stubs for claude/codex/pi/devin/cursor/tmux that log to TRIPWIRE.log
    and exit 127, so even a launch would spawn nothing real.
Rows (CAD-1280 widened the fleet; every row is still non-launchable):
  * stopped: state=stopped, enabled=0, no thread_id/session_id/endpoint/pid.
  * fenced: state=attention, enabled=0, same blanks, with a recovery error.
    `relaunch_agents` skips `enabled = 0` before anything else, and the
    fence text exercises the board's `fenced` totals and recovery column.
  * inbox: provider/kind `inbox`, state=idle, endpoint `inbox://<alias>`.
    A mailbox owns no actor (`registry::has_actor("inbox","inbox")` is
    false), and `relaunch_agents` skips every row without one. These are
    the live (non-stopped) rows the Agents screen shows beside the fleet.
  * no actor-kind row is ever seeded `idle`/`running`: that would need an
    endpoint or pid, and the liveness probe on such a row runs tmux.
  Open tasks are spread over many agents, not eight.

Confinement: with the agent UID configured (`agent-uid.json` or the
`agent-uid-mode.json` marker in the state dir), providers start through
the setuid helper, which ignores the daemon's PATH, so the stubs would not
fire. Every command refuses a state dir that carries either file.

A/B runs: seed once PER BUILD. A build newer than the one that recorded
the store's schema version refuses it, and a main build refuses a store a
newer build migrated.

Isolation: everything lives under --root (short /tmp path); HOME, XDG_*,
TMPDIR, CADENCE_STATE_DIR and CADENCE_PM_DIR are set for child processes
only. Board and daemon use a port in 3110-3199. Nothing here touches the
production state dir, port 3010 or the installed binary.

Subcommands: prepare | preflight | measure   (see --help)
"""
import argparse
import http.client
import json
import os
import re
import shutil
import signal
import socket
import sqlite3
import statistics
import subprocess
import sys
import threading
import time

# Board pids this run started (foreground `ui run`); the abort path kills them.
BOARD_PIDS = []
PROVIDERS = ("claude", "codex", "pi", "devin", "cursor", "tmux")
MIN_AVAILABLE_KIB = 6 * 1024 * 1024  # abort below 6 GiB MemAvailable
ALIAS_RE = re.compile(r"bench-\d{4}")


def paths(root):
    return {
        "state": f"{root}/state",
        "pm": f"{root}/pm",
        "home": f"{root}/home",
        "tmp": f"{root}/tmp",
        "stubs": f"{root}/stubs",
        "log": f"{root}/log",
        "dist": f"{root}/dist",
    }


def child_env(root):
    p = paths(root)
    return {
        "HOME": p["home"],
        "XDG_CONFIG_HOME": f"{p['home']}/.config",
        "XDG_STATE_HOME": f"{p['home']}/.state",
        "XDG_DATA_HOME": f"{p['home']}/.data",
        "XDG_CACHE_HOME": f"{p['home']}/.cache",
        "TMPDIR": p["tmp"],
        "CADENCE_STATE_DIR": p["state"],
        "CADENCE_PM_DIR": p["pm"],
        "PATH": f"{p['stubs']}:/usr/bin:/bin",
    }


def require_root(root):
    if not root.startswith("/tmp/") or len(root) > 40:
        sys.exit("refusing: --root must be a short /tmp path")
    if "/.local/state/cadence" in root or root.rstrip("/").endswith("/pm"):
        sys.exit("refusing: --root looks like a production path")


def require_port(port):
    if not 3110 <= port <= 3199:
        sys.exit("refusing: port must be in 3110-3199")


def setup_dirs(root):
    p = paths(root)
    for d in p.values():
        os.makedirs(d, exist_ok=True)
    for sub in (".config", ".state", ".data", ".cache"):
        os.makedirs(f"{p['home']}/{sub}", exist_ok=True)
    for name in PROVIDERS:
        stub = f"{p['stubs']}/{name}"
        with open(stub, "w") as f:
            f.write(f'#!/bin/sh\necho "$0 $*" >> {p["log"]}/TRIPWIRE.log\nexit 127\n')
        os.chmod(stub, 0o755)


def require_no_agent_uid(root):
    """Providers launched through the agent-UID helper never see the stub
    PATH, so refuse any state dir that opts into (or was ever in) that mode."""
    for name in ("agent-uid.json", "agent-uid-mode.json"):
        if os.path.lexists(f"{paths(root)['state']}/{name}"):
            sys.exit(f"refusing: {name} is in the state dir; agent-uid confinement "
                     "bypasses the PATH stubs")


def verify_stubs(root):
    """Each provider name must resolve to OUR stub on the children's PATH."""
    p = paths(root)
    for name in PROVIDERS:
        found = shutil.which(name, path=child_env(root)["PATH"])
        if found != f"{p['stubs']}/{name}":
            sys.exit(f"refusing: {name} resolves to {found}, not the tripwire stub")


def cadence(binary, root, *args, timeout=120):
    return subprocess.run(
        [binary, *args],
        env=child_env(root),
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def rpc(root, method, params, timeout=700):
    """One line-delimited JSON frame over the daemon socket. Returns the
    decoded frame; callers read `["result"]`."""
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(timeout)
    s.connect(f"{paths(root)['state']}/cadence.sock")
    s.sendall((json.dumps({"method": method, "params": params}) + "\n").encode())
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = s.recv(1 << 20)
        if not chunk:
            break
        buf += chunk
    s.close()
    return json.loads(buf)


def timed(fn):
    t0 = time.monotonic()
    out = fn()
    return time.monotonic() - t0, out


# ---------------------------------------------------------------- /proc


def mem_available_kib():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1])
    return 0


def proc_table():
    table = {}
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            with open(f"/proc/{pid}/stat") as f:
                stat = f.read()
            ppid = int(stat.rsplit(")", 1)[1].split()[1])
            with open(f"/proc/{pid}/cmdline", "rb") as f:
                argv = [a.decode("utf-8", "replace") for a in f.read().split(b"\0") if a]
            with open(f"/proc/{pid}/comm") as f:
                comm = f.read().strip()
        except (OSError, ValueError, IndexError):
            continue
        table[int(pid)] = (ppid, comm, argv)
    return table


def descendants(table, roots):
    found, frontier = set(), set(roots)
    while frontier:
        nxt = {p for p, (pp, _, _) in table.items() if pp in frontier and p not in found}
        found |= nxt
        frontier = nxt
    return found


def environ_marks(pid, root):
    try:
        with open(f"/proc/{pid}/environ", "rb") as f:
            return f"HOME={root}/home\0".encode() in f.read()
    except OSError:
        return False


def scan_spawned(root, daemon_roots, own):
    """Provider or tmux processes that belong to THIS run: descendants of
    the daemon/board we started, or any process carrying this run's HOME.
    Other lanes' real agents are ignored by design: they carry neither."""
    table = proc_table()
    mine = descendants(table, daemon_roots) | set(daemon_roots)
    hits = []
    for pid, (ppid, comm, argv) in table.items():
        if pid in own or pid in daemon_roots:
            continue
        names = {os.path.basename(a).split(":")[0] for a in argv[:1]} | {comm.split(":")[0]}
        by_name = any(n in PROVIDERS or n.rstrip("0123456789.-") in PROVIDERS for n in names)
        by_alias = any(ALIAS_RE.search(a) for a in argv)
        ours = pid in mine or environ_marks(pid, root)
        if ours and (by_name or by_alias):
            hits.append((pid, comm, " ".join(argv)[:160]))
    return hits


class Watchdog(threading.Thread):
    """Aborts the run if a provider/tmux process descended from our daemon
    appears, the tripwire log grows, or MemAvailable drops below 6 GiB."""

    def __init__(self, root, binary, roots_fn):
        super().__init__(daemon=True)
        self.root, self.binary, self.roots_fn = root, binary, roots_fn
        self.stop = threading.Event()
        self.tripped = None
        self.min_avail = mem_available_kib()
        self.max_load = 0.0

    def run(self):
        own = {os.getpid(), os.getppid()}
        trip = f"{paths(self.root)['log']}/TRIPWIRE.log"
        while not self.stop.wait(0.5):
            avail = mem_available_kib()
            self.min_avail = min(self.min_avail, avail)
            self.max_load = max(self.max_load, os.getloadavg()[0])
            reason = None
            if avail < MIN_AVAILABLE_KIB:
                reason = f"MemAvailable {avail // 1024} MiB below 6 GiB"
            elif os.path.exists(trip) and os.path.getsize(trip) > 0:
                reason = "stub tripwire fired (a provider launch was attempted)"
            else:
                hits = scan_spawned(self.root, self.roots_fn(), own)
                if hits:
                    reason = f"provider/tmux process from this run: {hits}"
            if reason:
                self.tripped = reason
                abort(self.root, self.binary, reason, list(BOARD_PIDS))
                return


def kill_ours(pid, root):
    """SIGTERM then SIGKILL one process this run started. Only a pid whose
    environment carries this run's HOME is touched: a recycled pid is not
    ours to signal."""
    if not environ_marks(pid, root):
        return
    for sig, wait in ((signal.SIGTERM, 5.0), (signal.SIGKILL, 5.0)):
        try:
            os.kill(pid, sig)
        except OSError:
            return
        end = time.monotonic() + wait
        while time.monotonic() < end:
            if not os.path.exists(f"/proc/{pid}") or _is_zombie(pid):
                return
            time.sleep(0.1)


def _is_zombie(pid):
    try:
        with open(f"/proc/{pid}/stat") as f:
            return f.read().rsplit(")", 1)[1].split()[0] == "Z"
    except (OSError, IndexError):
        return True


def marked_pids(root):
    me = {os.getpid(), os.getppid()}
    return [pid for pid in proc_table() if pid not in me and environ_marks(pid, root)]


def sweep(root, grace=0.0):
    """Stop whatever still carries this run's HOME (the daemon and board
    this run started, and anything they spawned), after waiting up to
    `grace` seconds for them to exit on their own. The HOME is unique to the
    run, so nothing else matches; the harness itself keeps the caller's."""
    end = time.monotonic() + grace
    while marked_pids(root) and time.monotonic() < end:
        time.sleep(0.5)
    for pid in marked_pids(root):
        kill_ours(pid, root)


def abort(root, binary, reason, boards=()):
    """Stop the daemon and every board this run started, then exit 3. The
    foreground `ui run` board is not reached by `ui stop` (that stops a
    detached board), so its pid is signalled directly."""
    print(f"WATCHDOG ABORT: {reason}", file=sys.stderr, flush=True)
    for pid in boards:
        kill_ours(pid, root)  # boards first; the daemon stops gracefully below
    for args in (("ui", "stop"), ("daemon", "stop")):
        try:
            cadence(binary, root, *args, timeout=60)
        except Exception as e:  # noqa: BLE001
            print(f"abort: {args}: {e}", file=sys.stderr)
    sweep(root)  # a daemon that ignored `daemon stop`, or was still starting
    os._exit(3)


# ----------------------------------------------------------------- seed


def seed(root, agents, msgs_per_agent, events_per_agent, jobs, tasks, inbox=0, fenced=0):
    db = f"{paths(root)['state']}/cadence.sqlite3"
    c = sqlite3.connect(db)
    c.execute("PRAGMA busy_timeout=5000")
    (version,) = c.execute("SELECT version FROM schema_version").fetchone()
    if c.execute("SELECT COUNT(*) FROM agents").fetchone()[0]:
        sys.exit("refusing: state already has agents (use a fresh --root)")
    now = time.time()
    cwd = paths(root)["tmp"]
    if inbox + fenced >= agents:
        sys.exit("refusing: --inbox + --fenced must leave stopped agents")
    aliases = [f"bench-{i:04d}" for i in range(agents)]
    stopped, rest = aliases[: agents - inbox - fenced], aliases[agents - inbox - fenced:]
    fenced_aliases, inbox_aliases = rest[:fenced], rest[fenced:]
    actors = stopped + fenced_aliases
    c.executemany(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,enabled,"
        "created,updated,params) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        [
            (a, "claude", "pty", "worker", cwd, "workspace-write", "stopped", 0,
             now - 86400 * 30, now - 86400, json.dumps({"session": a}))
            for a in stopped
        ],
    )
    # Fenced: attention + enabled=0 (never relaunched), with a recovery text.
    c.executemany(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,enabled,"
        "error,created,updated,params) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
        [
            (a, "claude", "pty", "worker", cwd, "workspace-write", "attention", 0,
             "synthetic fence: reconcile with `cadence agent unfence`",
             now - 86400 * 30, now - 86400, json.dumps({"session": a}))
            for a in fenced_aliases
        ],
    )
    # Mailboxes: no actor exists for them (see the header).
    c.executemany(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,enabled,"
        "endpoint,created,updated,params) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
        [
            (a, "inbox", "inbox", "worker", cwd, "workspace-write", "idle", 1,
             f"inbox://{a}", now - 86400 * 30, now - 86400, "{}")
            for a in inbox_aliases
        ],
    )
    c.executemany(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated) "
        "VALUES(?,?,?,?,?,?,?,?)",
        [(f"job-{j:05d}", f"synthetic job {j}", "/dev/null", aliases[0], f"CAD-{j}",
          "done" if j % 20 else "open", now - 9e5, now - 8e5) for j in range(jobs)],
    )
    # ~2% of tasks are non-terminal (what `tasks_for_assignee` returns); the
    # rest are terminal history it must scan past. The k-th open task goes to
    # actors[(7k) % len]: 7 is coprime with the usual fleet sizes, so open
    # tasks reach every actor agent (a few get two), not eight of them.
    open_ids = {t: k for k, t in enumerate(range(0, tasks, 50))}
    c.executemany(
        "INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) VALUES(?,?,?,?,?,?,?)",
        [(f"task-{t:06d}", f"job-{t % jobs:05d}", f"synthetic task {t}",
          actors[(7 * open_ids[t]) % len(actors)] if t in open_ids else aliases[t % agents],
          "assigned" if t in open_ids else "done", now - 9e5, now - 8e5 + t) for t in range(tasks)],
    )
    # History only: every message is terminal. Nothing queued, running or held.
    body = "synthetic message body " * 8
    c.executemany(
        "INSERT INTO messages(id,alias,body,source,state,created,started,completed,result) "
        "VALUES(?,?,?,?,?,?,?,?,?)",
        [(f"m-{a}-{k:05d}", a, body, "operator", "completed" if k % 9 else "failed",
          now - 9e5 + k, now - 9e5 + k, now - 9e5 + k + 5, '{"ok":true}')
         for a in aliases for k in range(msgs_per_agent)],
    )
    kinds = ("state", "turn_started", "turn_done", "ack", "tool_use", "note")
    c.executemany(
        "INSERT INTO events(alias,kind,payload,at) VALUES(?,?,?,?)",
        [(a, kinds[k % len(kinds)], '{"synthetic":true}', now - 9e5 + k)
         for a in aliases for k in range(events_per_agent)],
    )
    c.commit()
    verify_non_launchable(c)
    counts = {t: c.execute(f"SELECT COUNT(*) FROM {t}").fetchone()[0]
              for t in ("agents", "messages", "events", "jobs", "tasks")}
    counts["agents_by_state"] = dict(c.execute("SELECT state, COUNT(*) FROM agents GROUP BY state"))
    counts["agents_with_open_tasks"] = c.execute(
        "SELECT COUNT(DISTINCT assignee) FROM tasks WHERE state='assigned'").fetchone()[0]
    c.close()
    return version, counts


def verify_non_launchable(c):
    """Every actor-kind row is disabled and blank; the only enabled rows are
    mailboxes (no actor). Nothing is queued, running or held."""
    mailbox = "provider='inbox' AND endpoint_kind='inbox'"
    bad = {
        "enabled actor": f"SELECT COUNT(*) FROM agents WHERE enabled<>0 AND NOT ({mailbox})",
        "actor not stopped/attention": "SELECT COUNT(*) FROM agents WHERE "
        f"NOT ({mailbox}) AND state NOT IN ('stopped','attention')",
        "actor thread/session/endpoint/pid": f"SELECT COUNT(*) FROM agents WHERE NOT ({mailbox}) AND "
        "(thread_id IS NOT NULL OR session_id IS NOT NULL OR endpoint IS NOT NULL OR pid IS NOT NULL)",
        "mailbox not idle": f"SELECT COUNT(*) FROM agents WHERE ({mailbox}) AND state<>'idle'",
        "queued/running messages": "SELECT COUNT(*) FROM messages WHERE state NOT IN ('completed','failed')",
    }
    for name, sql in bad.items():
        n = c.execute(sql).fetchone()[0]
        if n:
            sys.exit(f"refusing: {n} seeded rows are launchable ({name})")


# ------------------------------------------------------------ lifecycle


def start_daemon(binary, root):
    r = cadence(binary, root, "daemon", "start")
    if r.returncode != 0:
        sys.exit(f"daemon start failed: {r.stdout} {r.stderr}")
    pid = json.loads(r.stdout.splitlines()[-1]).get("pid")
    return pid


def stop_daemon(binary, root):
    try:
        cadence(binary, root, "daemon", "stop", timeout=90)
    finally:
        sweep(root, grace=30)  # `daemon stop` can return before (or without) the exit


def port_free(port):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def pick_port(want, skip=()):
    """`want` if free, else the next free port in 3110-3199 (other lanes
    run boards there; we only ever bind, never connect to, a port we did
    not start). Free now is not free later: `start_board` proves the
    listener it ends up with is its own."""
    for port in [want] + list(range(3110, 3200)):
        if port not in skip and 3110 <= port <= 3199 and port_free(port):
            return port
    sys.exit("no free port in 3110-3199")


def listen_inodes(port):
    """Socket inodes in LISTEN state on `port` (any local address, v4 or v6)."""
    found = set()
    for name in ("/proc/net/tcp", "/proc/net/tcp6"):
        try:
            with open(name) as f:
                rows = f.read().splitlines()[1:]
        except OSError:
            continue
        for row in rows:
            cols = row.split()
            if cols[3] == "0A" and int(cols[1].rsplit(":", 1)[1], 16) == port:
                found.add(cols[9])
    return found


def socket_inodes(pid):
    found = set()
    try:
        for fd in os.listdir(f"/proc/{pid}/fd"):
            link = os.readlink(f"/proc/{pid}/fd/{fd}")
            if link.startswith("socket:["):
                found.add(link[8:-1])
    except OSError:
        pass
    return found


def port_is_ours(port, pid):
    """True only when something listens on `port` and EVERY listener there
    is a socket held by the board process we spawned. A foreign board that
    holds the port (or shares it) answers HTTP just as well, so the answer
    alone proves nothing."""
    listeners = listen_inodes(port)
    return bool(listeners) and listeners <= socket_inodes(pid)


def start_board(binary, root, want, on_spawn=None, attempts=5):
    """Start a foreground `ui run` board on a port of 3110-3199 and return
    its Popen, with `.port` set. The port is picked free, then bound by the
    board a moment later, so another lane can win the race: a board that
    exits (bind lost) or that is not the listener on its port is stopped and
    the next free port is tried. The caller learns the pid at once through
    `on_spawn`, before the board is up, so the watchdog covers it."""
    p = paths(root)
    verify_stubs(root)
    tried = set()
    for _ in range(attempts):
        port = pick_port(want, tried)
        tried.add(port)
        log = open(f"{p['log']}/board.log", "ab")
        proc = subprocess.Popen(
            [binary, "ui", "run", "--port", str(port), "--dist", p["dist"]],
            env=child_env(root), stdout=log, stderr=log, stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
        BOARD_PIDS.append(proc.pid)
        if on_spawn:
            on_spawn(proc)
        ours = False
        for _ in range(100):
            if proc.poll() is not None:
                break  # exited: usually the bind lost the race
            if listen_inodes(port) and not port_is_ours(port, proc.pid):
                break  # someone else listens on this port too
            if port_is_ours(port, proc.pid):
                ours = True
                break
            time.sleep(0.2)
        if ours:
            body = None
            for _ in range(50):
                try:
                    body = http_json(port, "/api/health", timeout=10)
                    break
                except OSError:
                    time.sleep(0.2)
            if body is not None:
                if body.get("daemon") != "reachable":
                    stop_board(proc)
                    sys.exit(f"board cannot reach the daemon: {body}")
                if not port_is_ours(port, proc.pid):  # re-prove after the first answer
                    stop_board(proc)
                    continue
                proc.port = port
                return proc
        stop_board(proc)
        print(f"board on port {port} was not ours; trying another", file=sys.stderr)
    sys.exit("could not start a board of our own in 3110-3199")


def stop_board(proc):
    if proc.poll() is None:
        proc.terminate()
    try:
        proc.wait(30)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(10)


def http_json(port, path, timeout=700):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    conn.request("GET", path, headers={"Host": f"127.0.0.1:{port}"})
    r = conn.getresponse()
    body = r.read()
    conn.close()
    if r.status != 200:
        raise OSError(f"{path}: HTTP {r.status}")
    return json.loads(body)


def http_get(port, path, timeout=700):
    """Latency probe that fails loudly on a non-200 (a fast error is not a
    fast answer)."""
    return len(json.dumps(http_json(port, path, timeout)))


def open_stream(port):
    """Attach one `/api/stream` subscriber and hold it open (drained by a
    thread so the server never blocks on us)."""
    s = socket.create_connection(("127.0.0.1", port), timeout=700)
    s.sendall(f"GET /api/stream HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n".encode())

    def drain():
        try:
            while s.recv(1 << 16):
                pass
        except OSError:
            pass

    threading.Thread(target=drain, daemon=True).start()
    return s


# ------------------------------------------------------------ commands


def db_state_counts(root):
    """(agents by state, verify_non_launchable) from the store, read-only."""
    c = sqlite3.connect(f"file:{paths(root)['state']}/cadence.sqlite3?mode=ro", uri=True)
    try:
        verify_non_launchable(c)
        return dict(c.execute("SELECT state, COUNT(*) FROM agents GROUP BY state"))
    finally:
        c.close()


def cmd_prepare(a):
    require_root(a.root)
    setup_dirs(a.root)
    require_no_agent_uid(a.root)
    verify_stubs(a.root)
    pid = start_daemon(a.cadence, a.root)  # creates the schema; empty registry
    stop_daemon(a.cadence, a.root)
    require_no_agent_uid(a.root)
    version, counts = seed(a.root, a.agents, a.msgs, a.events, a.jobs, a.tasks,
                           a.inbox, a.fenced)
    print(json.dumps({"schema_version": version, "seeded": counts, "bootstrap_pid": pid}))


def cmd_preflight(a):
    """Start the daemon on the seeded registry, wait past every timer, and
    scan /proc. Exit 0 only when nothing spawned and the daemon changed no
    agent row (every actor row is still blank and disabled, the per-state
    counts are the seeded ones)."""
    require_root(a.root)
    require_no_agent_uid(a.root)
    verify_stubs(a.root)
    seeded_states = db_state_counts(a.root)
    roots = []
    wd = Watchdog(a.root, a.cadence, lambda: list(roots))  # before the daemon
    wd.start()
    try:
        roots.append(start_daemon(a.cadence, a.root))
        # Timers: relaunch at start; stall tick (auto_resume) every tick;
        # auto-stop sweep every 60 s. Wait a full 60 s past the slowest.
        time.sleep(a.settle)
        agents = rpc(a.root, "agent_list", {})["result"]["agents"]
        states = {}
        for x in agents:
            states[x["state"]] = states.get(x["state"], 0) + 1
        hits = scan_spawned(a.root, roots, {os.getpid(), os.getppid()})
        trip = f"{paths(a.root)['log']}/TRIPWIRE.log"
        tripped = os.path.exists(trip) and os.path.getsize(trip) > 0
    finally:
        wd.stop.set()
        stop_daemon(a.cadence, a.root)
    rows_unchanged = db_state_counts(a.root) == seeded_states and states == seeded_states
    report = {
        "agents": len(agents), "states": states, "seeded_states": seeded_states,
        "rows_unchanged": rows_unchanged, "spawned": hits,
        "tripwire_fired": tripped, "watchdog": wd.tripped,
        "min_mem_available_mib": wd.min_avail // 1024, "max_load1": round(wd.max_load, 1),
        "settle_secs": a.settle,
    }
    print(json.dumps(report))
    sys.exit(0 if not hits and not tripped and not wd.tripped and rows_unchanged else 1)


def pcts(xs):
    xs = sorted(xs)
    return {"n": len(xs), "p50": round(statistics.median(xs), 3),
            "max": round(xs[-1], 3), "min": round(xs[0], 3)}


def sample_during(work, probes, interval=0.05):
    """Run `work` in a thread while each probe is sampled back to back;
    returns work duration and per-probe latency stats."""
    out = {}
    done = threading.Event()

    def runner():
        out["work"], out["val"] = timed(work)
        done.set()

    t = threading.Thread(target=runner)
    t.start()
    samples = {name: [] for name in probes}
    while not done.is_set():
        for name, fn in probes.items():
            if done.is_set():
                break
            samples[name].append(timed(fn)[0])
        time.sleep(interval)
    t.join()
    return round(out["work"], 3), {k: pcts(v) if v else None for k, v in samples.items()}


def cmd_measure(a):
    require_root(a.root)
    require_port(a.port)
    require_no_agent_uid(a.root)
    verify_stubs(a.root)
    db_state_counts(a.root)  # refuses a launchable store before anything starts
    roots = []
    wd = Watchdog(a.root, a.cadence, lambda: list(roots))  # before daemon and board
    wd.start()
    board = None
    res = {"label": a.label, "binary": a.cadence, "load1_start": os.getloadavg()[0],
           "cpus": os.cpu_count()}
    try:
        roots.append(start_daemon(a.cadence, a.root))
        board = start_board(a.cadence, a.root, a.port, on_spawn=lambda pr: roots.append(pr.pid))
        res["board_port"] = board.port
        time.sleep(a.settle)
        if wd.tripped:
            sys.exit(1)
        alias = "bench-0007"
        lst = lambda board_mode=True: rpc(a.root, "agent_list", {"board": True} if board_mode else {})
        res["rpc_agent_list_board_cold_s"] = round(timed(lst)[0], 3)
        res["rpc_agent_list_board_warm_s"] = pcts([timed(lst)[0] for _ in range(a.reps)])
        res["rpc_agent_list_plain_s"] = pcts([timed(lambda: lst(False))[0] for _ in range(a.reps)])
        res["rpc_health_idle_s"] = pcts([timed(lambda: rpc(a.root, "health", {}))[0] for _ in range(10)])
        res["rpc_daemon_info_idle_s"] = pcts([timed(lambda: rpc(a.root, "daemon_info", {}))[0] for _ in range(10)])
        res["cli_daemon_status_idle_s"] = pcts(
            [timed(lambda: cadence(a.cadence, a.root, "daemon", "status"))[0] for _ in range(5)])
        res["http_health_idle_s"] = pcts([timed(lambda: http_get(board.port, "/api/health"))[0] for _ in range(10)])
        res["http_meta_idle_s"] = pcts([timed(lambda: http_get(board.port, "/api/meta"))[0] for _ in range(10)])
        res["http_agent_detail_s"] = pcts(
            [timed(lambda: http_get(board.port, f"/api/agents/{alias}"))[0] for _ in range(5)])

        # A3: other calls while a board-mode agent_list runs.
        work, probes = sample_during(lst, {
            "rpc_health": lambda: rpc(a.root, "health", {}),
            "cli_daemon_status": lambda: cadence(a.cadence, a.root, "daemon", "status"),
            "http_health": lambda: http_get(board.port, "/api/health"),
            "http_meta": lambda: http_get(board.port, "/api/meta"),
        })
        res["during_agent_list"] = {"agent_list_s": work, "probes": probes}

        # Board route, cold-after-restart is measured by the caller by
        # re-running with a fresh board; here: warm, with a stream, under load.
        res["http_agents_first_s"] = round(timed(lambda: http_get(board.port, "/api/agents"))[0], 3)
        res["http_agents_warm_s"] = pcts([timed(lambda: http_get(board.port, "/api/agents"))[0]
                                           for _ in range(a.reps)])
        stream = open_stream(board.port)
        time.sleep(2)
        res["http_agents_with_stream_s"] = pcts(
            [timed(lambda: http_get(board.port, "/api/agents"))[0] for _ in range(a.reps)])
        loaders = [threading.Thread(target=lambda: [rpc(a.root, "agent_list", {"board": True})
                                                    for _ in range(3)]) for _ in range(4)]
        for t in loaders:
            t.start()
        res["http_agents_under_load_s"] = pcts(
            [timed(lambda: http_get(board.port, "/api/agents"))[0] for _ in range(3)])
        res["http_health_under_load_s"] = pcts(
            [timed(lambda: http_get(board.port, "/api/health"))[0] for _ in range(5)])
        res["cli_daemon_status_under_load_s"] = pcts(
            [timed(lambda: cadence(a.cadence, a.root, "daemon", "status"))[0] for _ in range(3)])
        for t in loaders:
            t.join()
        stream.close()
        # Cold again, this time with a stream subscriber attached first.
        # Re-pick and re-prove the port: the old one may be taken by now.
        stop_board(board)
        board = start_board(a.cadence, a.root, board.port,
                            on_spawn=lambda pr: roots.append(pr.pid))
        res["board_port_after_restart"] = board.port
        # Cold board, no stream: the first read after a restart pays it all.
        res["http_agents_first_nostream_s"] = round(
            timed(lambda: http_get(board.port, "/api/agents"))[0], 3)
        stream = open_stream(board.port)
        res["http_agents_first_with_stream_s"] = round(
            timed(lambda: http_get(board.port, "/api/agents"))[0], 3)
        stream.close()
    finally:
        wd.stop.set()
        res["watchdog"] = wd.tripped
        res["min_mem_available_mib"] = wd.min_avail // 1024
        res["max_load1"] = round(wd.max_load, 1)
        res["spawned_at_end"] = scan_spawned(a.root, roots, {os.getpid(), os.getppid()})
        if board is not None:
            stop_board(board)
        stop_daemon(a.cadence, a.root)
    print(json.dumps(res, indent=1))
    sys.exit(0 if not res["spawned_at_end"] and not wd.tripped else 1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name, fn in (("prepare", cmd_prepare), ("preflight", cmd_preflight), ("measure", cmd_measure)):
        p = sub.add_parser(name)
        p.add_argument("--cadence", required=True, help="cadence binary under test")
        p.add_argument("--root", default="/tmp/c1266")
        p.set_defaults(fn=fn)
        if name == "prepare":
            p.add_argument("--agents", type=int, default=400)
            p.add_argument("--msgs", type=int, default=150)
            p.add_argument("--events", type=int, default=1000)
            p.add_argument("--jobs", type=int, default=2000)
            p.add_argument("--tasks", type=int, default=20000)
            p.add_argument("--inbox", type=int, default=40,
                           help="of --agents: mailbox rows (idle, no actor)")
            p.add_argument("--fenced", type=int, default=20,
                           help="of --agents: attention rows (disabled, never relaunched)")
        if name == "preflight":
            p.add_argument("--settle", type=int, default=130,
                           help="seconds to wait (default 130: 60 s past the 60 s auto-stop sweep)")
        if name == "measure":
            p.add_argument("--label", required=True)
            p.add_argument("--port", type=int, default=3161)
            p.add_argument("--settle", type=int, default=5)
            p.add_argument("--reps", type=int, default=5)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
