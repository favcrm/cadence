
import json, os, socket, subprocess, sys, threading, time

def rpc(sock_path, frame):
    params = frame.setdefault("params", {})
    if params.get("pid") == "$PID":
        params["pid"] = os.getpid()
    s = socket.socket(socket.AF_UNIX)
    s.connect(sock_path)
    s.sendall((json.dumps(frame) + "\n").encode())
    line = s.makefile().readline()
    s.close()
    return json.loads(line)

def land(path, value):
    with open(path + ".tmp", "w") as f:
        json.dump(value, f)
    os.rename(path + ".tmp", path)

def off_lineage(pid):
    p = os.getpid()
    while p > 1:
        if p == pid:
            return False
        # A hop can vanish mid-walk — the detach's intermediates exit
        # fast and the kernel reparents only once they do. An incomplete
        # chain proves neither tied nor detached: keep waiting, never die.
        try:
            with open("/proc/%d/status" % p) as f:
                p = int([l for l in f if l.startswith("PPid:")][0].split()[1])
        except (OSError, IndexError):
            return False
    return True

if sys.argv[1] == "--child":
    sock_path, frame, out = sys.argv[2:5]
    land(out, rpc(sock_path, json.loads(frame)))
    sys.exit(0)

if sys.argv[1] == "--detached":
    sock_path, frame, out, root = sys.argv[2:6]
    if os.fork() > 0:
        os.wait()
        sys.exit(0)
    os.setsid()
    if os.fork() > 0:
        os._exit(0)
    while not off_lineage(int(root)):
        time.sleep(0.02)
    land(out, rpc(sock_path, json.loads(frame)))
    os._exit(0)

pidfile, sock_path, cmd_dir = sys.argv[1:4]
with open(pidfile + ".tmp", "w") as f:
    f.write(str(os.getpid()))
os.rename(pidfile + ".tmp", pidfile)

def serve():
    n = 0
    while True:
        req = os.path.join(cmd_dir, "req-%d.json" % n)
        if not os.path.exists(req):
            time.sleep(0.02)
            continue
        cmd = json.load(open(req))
        out = os.path.join(cmd_dir, "resp-%d.json" % n)
        if cmd["how"] == "exec":
            # A tool subprocess running a real command (CAD-276).
            r = subprocess.run(cmd["argv"], capture_output=True, text=True)
            land(out, {"rc": r.returncode, "out": r.stdout, "err": r.stderr})
            n += 1
            continue
        frame = json.dumps(cmd["frame"])
        me = sys.executable, os.path.abspath(__file__)
        if cmd["how"] == "self":
            land(out, rpc(sock_path, cmd["frame"]))
        elif cmd["how"] == "child":
            # Answer only once the child is reaped: its hold's holder
            # is then provably dead for the next reap pass.
            subprocess.run([*me, "--child", sock_path, frame, out + ".c"], check=True)
            os.rename(out + ".c", out)
        else:
            # `detached-bare`: the detach also scrubs CADENCE_ALIAS.
            env = dict(os.environ)
            if cmd["how"] == "detached-bare":
                env.pop("CADENCE_ALIAS", None)
            subprocess.run([*me, "--detached", sock_path, frame, out,
                            str(os.getpid())], check=True, env=env)
        n += 1

threading.Thread(target=serve, daemon=True).start()
for _ in sys.stdin:
    pass
