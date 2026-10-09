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
Rows: state=stopped, enabled=0, no thread_id/session_id/endpoint/pid.

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
import socket
import sqlite3
import statistics
import subprocess
import sys
import threading
import time

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
            return f"HOME={root}/home".encode() in f.read()
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
                abort(self.root, self.binary, reason)
                return


def abort(root, binary, reason):
    print(f"WATCHDOG ABORT: {reason}", file=sys.stderr, flush=True)
    # Stop only the daemon/board this run started (graceful, by state dir).
    for args in (("ui", "stop"), ("daemon", "stop")):
        try:
            cadence(binary, root, *args, timeout=60)
        except Exception as e:  # noqa: BLE001
            print(f"abort: {args}: {e}", file=sys.stderr)
    os._exit(3)


# ----------------------------------------------------------------- seed


def seed(root, agents, msgs_per_agent, events_per_agent, jobs, tasks):
    db = f"{paths(root)['state']}/cadence.sqlite3"
    c = sqlite3.connect(db)
    c.execute("PRAGMA busy_timeout=5000")
    (version,) = c.execute("SELECT version FROM schema_version").fetchone()
    if c.execute("SELECT COUNT(*) FROM agents").fetchone()[0]:
        sys.exit("refusing: state already has agents (use a fresh --root)")
    now = time.time()
    cwd = paths(root)["tmp"]
    aliases = [f"bench-{i:04d}" for i in range(agents)]
    c.executemany(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,enabled,"
        "created,updated,params) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        [
            (a, "claude", "pty", "worker", cwd, "workspace-write", "stopped", 0,
             now - 86400 * 30, now - 86400, json.dumps({"session": a}))
            for a in aliases
        ],
    )
    c.executemany(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated) "
        "VALUES(?,?,?,?,?,?,?,?)",
        [(f"job-{j:05d}", f"synthetic job {j}", "/dev/null", aliases[0], f"CAD-{j}",
          "done" if j % 20 else "open", now - 9e5, now - 8e5) for j in range(jobs)],
    )
    # ~2% of tasks are non-terminal (what `tasks_for_assignee` returns); the
    # rest are terminal history it must scan past (tasks has no assignee index).
    c.executemany(
        "INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) VALUES(?,?,?,?,?,?,?)",
        [(f"task-{t:06d}", f"job-{t % jobs:05d}", f"synthetic task {t}", aliases[t % agents],
          "assigned" if t % 50 == 0 else "done", now - 9e5, now - 8e5 + t) for t in range(tasks)],
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
    c.close()
    return version, counts


def verify_non_launchable(c):
    bad = {
        "enabled": "SELECT COUNT(*) FROM agents WHERE enabled<>0",
        "not stopped": "SELECT COUNT(*) FROM agents WHERE state<>'stopped'",
        "thread/session/endpoint/pid": "SELECT COUNT(*) FROM agents WHERE thread_id IS NOT NULL "
        "OR session_id IS NOT NULL OR endpoint IS NOT NULL OR pid IS NOT NULL",
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
    cadence(binary, root, "daemon", "stop", timeout=90)


def port_free(port):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def pick_port(want):
    """`want` if free, else the next free port in 3110-3199 (other lanes
    run boards there; we only ever bind, never connect to, a port we did
    not start)."""
    for port in [want] + list(range(3110, 3200)):
        if port_free(port):
            return port
    sys.exit("no free port in 3110-3199")


def start_board(binary, root, port):
    p = paths(root)
    port = pick_port(port)
    if not port_free(port):
        sys.exit(f"port {port} is in use")
    log = open(f"{p['log']}/board.log", "ab")
    proc = subprocess.Popen(
        [binary, "ui", "run", "--port", str(port), "--dist", p["dist"]],
        env=child_env(root), stdout=log, stderr=log, stdin=subprocess.DEVNULL,
        start_new_session=True,
    )
    for _ in range(100):
        if proc.poll() is not None:
            sys.exit(f"board exited early; see {p['log']}/board.log")
        try:
            http_get(port, "/api/health", timeout=2)
            body = http_json(port, "/api/health")
            if body.get("daemon") != "reachable":
                sys.exit(f"board cannot reach the daemon: {body}")
            proc.port = port
            return proc
        except OSError:
            time.sleep(0.2)
    proc.terminate()
    sys.exit("board did not come up")


def stop_board(proc):
    proc.terminate()
    try:
        proc.wait(30)
    except subprocess.TimeoutExpired:
        proc.kill()


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


def cmd_prepare(a):
    require_root(a.root)
    setup_dirs(a.root)
    pid = start_daemon(a.cadence, a.root)  # creates the schema; empty registry
    stop_daemon(a.cadence, a.root)
    version, counts = seed(a.root, a.agents, a.msgs, a.events, a.jobs, a.tasks)
    print(json.dumps({"schema_version": version, "seeded": counts, "bootstrap_pid": pid}))


def cmd_preflight(a):
    """Start the daemon on the seeded registry, wait past every timer, and
    scan /proc. Exit 0 only when nothing spawned."""
    require_root(a.root)
    pid = start_daemon(a.cadence, a.root)
    roots = [pid]
    wd = Watchdog(a.root, a.cadence, lambda: roots)
    wd.start()
    try:
        # Timers: relaunch at start; stall tick (auto_resume) every tick;
        # auto-stop sweep every 60 s. Wait a full 60 s past the slowest.
        time.sleep(a.settle)
        agents = rpc(a.root, "agent_list", {})["result"]["agents"]
        states = sorted({x["state"] for x in agents})
        hits = scan_spawned(a.root, roots, {os.getpid(), os.getppid()})
        trip = f"{paths(a.root)['log']}/TRIPWIRE.log"
        tripped = os.path.exists(trip) and os.path.getsize(trip) > 0
    finally:
        wd.stop.set()
        stop_daemon(a.cadence, a.root)
    report = {
        "agents": len(agents), "states": states, "spawned": hits,
        "tripwire_fired": tripped, "watchdog": wd.tripped,
        "min_mem_available_mib": wd.min_avail // 1024, "max_load1": round(wd.max_load, 1),
        "settle_secs": a.settle,
    }
    print(json.dumps(report))
    sys.exit(0 if not hits and not tripped and not wd.tripped and states == ["stopped"] else 1)


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
    pid = start_daemon(a.cadence, a.root)
    roots = [pid]
    try:
        board = start_board(a.cadence, a.root, a.port)
    except SystemExit:
        stop_daemon(a.cadence, a.root)
        raise
    roots.append(board.pid)
    wd = Watchdog(a.root, a.cadence, lambda: list(roots))
    wd.start()
    res = {"label": a.label, "binary": a.cadence, "load1_start": os.getloadavg()[0],
           "cpus": os.cpu_count()}
    try:
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
        stop_board(board)
        board = start_board(a.cadence, a.root, board.port)
        roots.append(board.pid)
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
