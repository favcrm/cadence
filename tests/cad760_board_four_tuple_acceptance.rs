//! CAD-760 independent acceptance check, written from the ticket by the
//! Spec/security reviewer, not the implementer (AGENTS.md "Gates and
//! security work"). The implementer may not edit or weaken this file.
//!
//! Black box over the REAL guard: a `cadence daemon run` and a
//! `cadence ui run` child per fixture, real sockets, no caller seam (no
//! `X-Cadence-Test-*` assertion is honoured: the board runs without
//! `--test-seam`). Operator sessions are minted through the native
//! `operator_auth` store before the daemon starts (the cad1193 pattern);
//! the one registered pane is a row written through the daemon's own
//! store API (`register_agent` + `set_identity`, which records the pid's
//! start time). Neither stands in for the `/proc` four-tuple lookup or
//! the operator ancestry walk under test.
//!
//! The collision is made deterministic with TWO real boards on the same
//! port: board A at 127.0.0.1:L and board B at 127.0.0.2:L. One client
//! source 127.0.0.1:S is shared by two live connections, one to each
//! board. Both boards scan the same `/proc/net/tcp`; a lookup keyed on
//! the port alone gives both boards the SAME row, so whichever row the
//! kernel lists first, one board judges the wrong process. Keyed on the
//! full four-tuple, each board judges its own connection's owner.
//!
//! Wildcard binds (`0.0.0.0`, `::`, `::ffff:0.0.0.0`) are NOT started
//! here: on a shared host that would expose a listener beyond loopback.
//! The host-name form (`localhost`, loopback-only) drives the same
//! `concrete_board_server` refusal through the real board; the wildcard
//! literals are pinned by the unit test in `src/ui/operator.rs`.
#![cfg(target_os = "linux")]

use cadence_agent::{adapter, client, issue, operator_auth, reaper, store};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const WRITE: &str = "/api/plans/CAD-760/approve";
const PANE: &str = "pane760";

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct Session {
    token: String,
    key: String,
}

/// One isolated daemon + board on `host:port`.
struct Fixture {
    root: PathBuf,
    state: PathBuf,
    port: u16,
    sessions: Vec<Session>,
    _board: OwnedChild,
    _daemon: OwnedChild,
}

fn fixture_command(root: &Path, state: &Path, pm: &Path) -> Command {
    let mut cmd = Command::new(BINARY);
    cmd.env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("xdg"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("TMPDIR", root.join("tmp"))
        .env("TMUX_TMPDIR", root.join("tmp"))
        .env("CADENCE_PM_DIR", pm)
        .env("CADENCE_SUITE_LOCK", root.join("suite.lock"))
        .args(["--state-dir", state.to_str().unwrap()])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in adapter::PROVIDER_COMMAND_VARS {
        cmd.env(name, adapter::REFUSED_COMMAND);
    }
    for name in ["CARGO_HOME", "RUSTUP_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd
}

impl Fixture {
    fn start(parent: &Path, name: &str, host: &str, connect: &[&str], port: u16) -> Self {
        let root = parent.join(name);
        for dir in [
            "", "state", "pm", "home", "xdg", "config", "data", "tmp", "bin", "dist",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        // No real gh or tmux is reachable from the fixture processes.
        for tool in ["gh", "tmux"] {
            std::os::unix::fs::symlink("/bin/false", root.join("bin").join(tool)).unwrap();
        }
        let state = root.join("state");
        let pm = root.join("pm");
        issue::Pm::init(&pm).unwrap();
        let now = issue::time::now_epoch();
        let mut auth = operator_auth::Auth::load(&state);
        let sessions = (0..3)
            .map(|i| {
                let nonce = auth.mint(operator_auth::Origin::Loopback, now).unwrap();
                let opened = auth
                    .open(
                        &nonce,
                        operator_auth::Origin::Loopback,
                        &format!("CAD-760 acceptance {name} {i}"),
                        now,
                    )
                    .unwrap()
                    .unwrap();
                Session {
                    token: opened.token,
                    key: opened.key,
                }
            })
            .collect();
        let mut daemon = OwnedChild(
            reaper::spawn(fixture_command(&root, &state, &pm).args(["daemon", "run"])).unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_private_timeout(
            &state,
            "daemon_info",
            json!({}),
            Duration::from_millis(200),
        )
        .is_err()
        {
            assert!(
                daemon.0.try_wait().unwrap().is_none(),
                "{name}: daemon exited"
            );
            assert!(Instant::now() < deadline, "{name}: daemon not ready");
            std::thread::sleep(Duration::from_millis(30));
        }
        let mut board = OwnedChild(
            reaper::spawn(
                fixture_command(&root, &state, &pm)
                    .args([
                        "ui",
                        "run",
                        "--host",
                        host,
                        "--port",
                        &port.to_string(),
                        "--dist",
                    ])
                    .arg(root.join("dist")),
            )
            .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while !connect
            .iter()
            .any(|ip| TcpStream::connect((*ip, port)).is_ok())
        {
            assert!(
                board.0.try_wait().unwrap().is_none(),
                "{name}: board exited"
            );
            assert!(Instant::now() < deadline, "{name}: board did not bind");
            std::thread::sleep(Duration::from_millis(30));
        }
        Fixture {
            root,
            state,
            port,
            sessions,
            _board: board,
            _daemon: daemon,
        }
    }

    /// Byte-for-byte what a board tab sends for an operator-only write,
    /// plus whatever `extra` headers the caller forges.
    fn request(
        &self,
        method: &str,
        path: &str,
        session: Option<&Session>,
        extra: &[&str],
    ) -> String {
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nX-Cadence-Board: 1\r\n"
        );
        if method == "POST" {
            req.push_str(&format!(
                "Origin: http://{host}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n"
            ));
        }
        if let Some(s) = session {
            req.push_str(&format!(
                "Cookie: cadence_operator_{}={}\r\nX-Cadence-Session: {}\r\n",
                self.port, s.token, s.key
            ));
        }
        for h in extra {
            req.push_str(h);
            req.push_str("\r\n");
        }
        req.push_str("\r\n");
        if method == "POST" {
            req.push_str("{}");
        }
        req
    }

    /// Record `pid` as the live registered pane `PANE` (pid + start time
    /// + generation), the same row a real pty open writes.
    fn seed_pane(&self, pid: u32) {
        let db = store::Store::open_side(&self.state.join("cadence.sqlite3")).unwrap();
        if db.agent_opt(PANE).unwrap().is_none() {
            let cwd = self.root.join("tmp");
            db.register_agent(&store::NewAgent {
                alias: PANE,
                provider: "tui-stub",
                endpoint_kind: "pty",
                role: "worker",
                cwd: cwd.to_str().unwrap(),
                sandbox: "workspace-write",
                instructions: None,
                params: Some(r#"{"session":"cad760"}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        }
        db.set_identity(
            PANE,
            &adapter::Identity {
                thread_id: "cad760-thread".into(),
                session_id: "cad760-session".into(),
                model: None,
                effort: None,
                pid,
                endpoint: Some("tmux://cad760".into()),
                generation: Some(format!("cad760-{pid}")),
                attach: None,
            },
        )
        .unwrap();
        let listed = client::rpc_private_timeout(
            &self.state,
            "agent_list",
            json!({}),
            Duration::from_secs(5),
        )
        .unwrap();
        let row = listed["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["alias"] == PANE)
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(row["pid"], json!(pid), "seeded pane row is live: {row}");
        assert_eq!(row["endpoint_kind"], "pty");
        assert!(
            row["pid_start"].is_u64() && !row["generation"].is_null(),
            "{row}"
        );
    }
}

/// A loopback client bound to `src` with `SO_REUSEADDR`, connected to
/// `dst`: two of these can share one source IP:port.
fn reuse_connect(src: SocketAddrV4, dst: SocketAddrV4) -> TcpStream {
    let sockaddr = |a: SocketAddrV4| libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: a.port().to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(a.ip().octets()),
        },
        sin_zero: [0; 8],
    };
    let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: plain socket syscalls on a fresh fd the stream owns at once.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        assert!(fd >= 0);
        let stream = TcpStream::from_raw_fd(fd);
        let one: libc::c_int = 1;
        let size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ok = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&one as *const libc::c_int).cast(),
            size,
        );
        assert_eq!(ok, 0);
        let ok = libc::bind(fd, (&sockaddr(src) as *const libc::sockaddr_in).cast(), len);
        assert_eq!(ok, 0, "bind {src}: {}", std::io::Error::last_os_error());
        let ok = libc::connect(fd, (&sockaddr(dst) as *const libc::sockaddr_in).cast(), len);
        assert_eq!(ok, 0, "connect {dst}: {}", std::io::Error::last_os_error());
        stream
    }
}

#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

fn parse(raw: &[u8]) -> Reply {
    let text = String::from_utf8_lossy(raw);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no HTTP status in reply: {text:?}"));
    let body = match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if b > a => serde_json::from_str(&text[a..=b]).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    Reply { status, body }
}

/// The calling (operator) process sends on its own socket.
fn operator_send(stream: &mut TcpStream, request: &str) -> Reply {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    parse(&raw)
}

/// An agent process that holds `conn` ALONE (the test drops its copy)
/// and sends `request` on it once released. `alias` is put in its
/// environment as an agent's would be.
struct AgentProc {
    child: OwnedChild,
    go: PathBuf,
    resp: PathBuf,
}

impl AgentProc {
    fn spawn(dir: &Path, tag: &str, conn: TcpStream, request: &str, alias: &str) -> Self {
        let (go, req, resp) = (
            dir.join(format!("{tag}.go")),
            dir.join(format!("{tag}.req")),
            dir.join(format!("{tag}.resp")),
        );
        std::fs::write(&req, request).unwrap();
        let held = conn.as_raw_fd();
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            // The pane process keeps the socket open after its reply, so the
            // row stays listed until both boards have judged.
            r#"while [ ! -e "$1" ]; do sleep 0.02; done; cat "$2" >&3; cat <&3 >"$3"; : >"$3.done"; while [ ! -e "$1.end" ]; do sleep 0.02; done"#,
            "sh",
        ])
        .arg(&go)
        .arg(&req)
        .arg(&resp)
        .env("CADENCE_ALIAS", alias)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        // SAFETY: dup2/fcntl are async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(held, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = OwnedChild(reaper::spawn(&mut cmd).unwrap());
        drop(conn);
        AgentProc { child, go, resp }
    }

    fn pid(&self) -> u32 {
        self.child.0.id()
    }

    fn release(&self) {
        std::fs::write(&self.go, b"").unwrap();
    }

    fn reply(mut self) -> Reply {
        let done = self.resp.with_extension("resp.done");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done.exists() {
            assert!(
                self.child.0.try_wait().unwrap().is_none(),
                "agent process exited early"
            );
            assert!(Instant::now() < deadline, "agent request did not complete");
            std::thread::sleep(Duration::from_millis(20));
        }
        let reply = parse(&std::fs::read(&self.resp).unwrap());
        std::fs::write(self.go.with_extension("go.end"), b"").unwrap();
        let _ = self.child.0.wait();
        reply
    }
}

fn admitted(reply: &Reply) -> bool {
    reply.status != 403 && reply.body.get("check").is_none()
}

fn assert_refused(reply: &Reply, check: &str, needle: &str, case: &str) {
    assert_eq!(reply.status, 403, "{case}: {reply:?}");
    assert_eq!(reply.body["check"], check, "{case}: {reply:?}");
    let error = reply.body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains(needle),
        "{case}: `{needle}` not in {error:?}"
    );
}

fn free_port(ips: &[Ipv4Addr]) -> u16 {
    (3110..=3199)
        .find(|p| {
            ips.iter()
                .all(|ip| std::net::TcpListener::bind((*ip, *p)).is_ok())
        })
        .expect("a port free on every fixture address in 3110..=3199")
}

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("c760-").tempdir().unwrap()
}

/// One collision: the agent holds 127.0.0.1:S → board A, the operator
/// (this process) holds 127.0.0.1:S → board B, both live while each
/// board judges its request.
fn collide(
    a: &Fixture,
    b: &Fixture,
    dir: &Path,
    tag: &str,
    agent_request: &str,
    alias: &str,
    seed: bool,
) -> (Reply, Reply) {
    let one = Ipv4Addr::LOCALHOST;
    let two = Ipv4Addr::new(127, 0, 0, 2);
    let mut operator = reuse_connect(SocketAddrV4::new(one, 0), SocketAddrV4::new(two, b.port));
    let SocketAddr::V4(src) = operator.local_addr().unwrap() else {
        unreachable!()
    };
    let agent_conn = reuse_connect(src, SocketAddrV4::new(one, a.port));
    assert_eq!(
        agent_conn.local_addr().unwrap(),
        operator.local_addr().unwrap()
    );
    let agent = AgentProc::spawn(dir, tag, agent_conn, agent_request, alias);
    if seed {
        a.seed_pane(agent.pid());
    }
    // Both requests in flight while both client rows are listed.
    agent.release();
    let to_b = operator_send(
        &mut operator,
        &b.request("POST", WRITE, Some(&b.sessions[0]), &[]),
    );
    let to_a = agent.reply();
    drop(operator);
    (to_a, to_b)
}

/// (a) + (c): socket collision through two real boards, and the
/// regression controls through the same boards.
#[test]
fn cad760_colliding_source_ports_are_judged_on_their_own_socket() {
    let dir = scratch();
    let port = free_port(&[Ipv4Addr::LOCALHOST, Ipv4Addr::new(127, 0, 0, 2)]);
    let a = Fixture::start(dir.path(), "a", "127.0.0.1", &["127.0.0.1"], port);
    let b = Fixture::start(dir.path(), "b", "127.0.0.2", &["127.0.0.2"], port);

    // (c) control: the genuine operator, plain loopback, passes on both
    // boards — operator-only read and write.
    for f in [&a, &b] {
        let ip = if f.state == a.state {
            "127.0.0.1"
        } else {
            "127.0.0.2"
        };
        for (method, path) in [("GET", "/api/master/permissions"), ("POST", WRITE)] {
            let mut s = TcpStream::connect((ip, f.port)).unwrap();
            let r = operator_send(&mut s, &f.request(method, path, Some(&f.sessions[2]), &[]));
            assert!(
                admitted(&r),
                "genuine operator {method} {path} on {ip}: {r:?}"
            );
        }
    }

    // (a) An unregistered agent environment presents the operator's
    // stolen session on board A while the operator's own connection to
    // board B shares its source IP:port. A judges the agent's socket:
    // the positive operator proof refuses it. B judges the operator's:
    // admitted.
    let stolen = a.request("POST", WRITE, Some(&a.sessions[0]), &[]);
    let (to_a, to_b) = collide(&a, &b, dir.path(), "rogue", &stolen, "rogue760", false);
    assert_refused(
        &to_a,
        "operator_proof",
        "CADENCE_ALIAS",
        "stolen session, unregistered agent",
    );
    assert!(
        admitted(&to_b),
        "operator to B beside the rogue agent: {to_b:?}"
    );

    // (a)+(c) A registered pane, forging identity headers, presents the
    // operator's session on board A. It is attributed to ITS pane (the
    // session is revoked as stolen), not to the operator's row; the
    // operator's connection to B still passes.
    let forged = [
        "Tailscale-User-Login: operator@example.com",
        "Tailscale-User-Name: Operator",
        "X-Forwarded-For: 127.0.0.2",
        "Forwarded: for=127.0.0.2",
    ];
    let stolen = a.request("POST", WRITE, Some(&a.sessions[1]), &forged);
    let (to_a, to_b) = collide(&a, &b, dir.path(), "pane-s", &stolen, PANE, true);
    assert_refused(
        &to_a,
        "session_from_agent",
        &format!("agent '{PANE}'"),
        "pane with stolen session",
    );
    assert!(admitted(&to_b), "operator to B beside the pane: {to_b:?}");

    // (c) The same pane with no session and the forged headers is the
    // pane's request: refused as the agent, by name.
    let bare = a.request("POST", WRITE, None, &forged);
    let (to_a, to_b) = collide(&a, &b, dir.path(), "pane-n", &bare, PANE, true);
    assert_refused(
        &to_a,
        "operator_only",
        &format!("agent '{PANE}'"),
        "pane without session",
    );
    assert!(
        admitted(&to_b),
        "operator to B beside the bare pane: {to_b:?}"
    );

    // (c) A forged caller-seam assertion from the pane, with the stolen
    // session, is refused outright on a board without the seam armed.
    let seam = a.request(
        "POST",
        WRITE,
        Some(&a.sessions[0]),
        &[
            "X-Cadence-Test-As: operator",
            "X-Cadence-Test-Token: forged",
        ],
    );
    let (to_a, to_b) = collide(&a, &b, dir.path(), "pane-t", &seam, PANE, true);
    assert_eq!(to_a.status, 403, "forged seam assertion: {to_a:?}");
    assert!(!admitted(&to_a), "forged seam assertion: {to_a:?}");
    assert!(
        admitted(&to_b),
        "operator to B beside the seam forger: {to_b:?}"
    );
}

/// (b): a board bound to a host name has no concrete destination; the
/// genuine operator with a valid session is refused, never let through
/// on a port-only fallback.
#[test]
fn cad760_board_without_a_concrete_bind_refuses_operator_proof() {
    let dir = scratch();
    let port = free_port(&[Ipv4Addr::LOCALHOST]);
    let c = Fixture::start(dir.path(), "c", "localhost", &["127.0.0.1", "::1"], port);
    let mut checked = 0;
    for ip in ["127.0.0.1", "::1"] {
        let Ok(mut s) = TcpStream::connect((ip, port)) else {
            continue;
        };
        for (method, path) in [("GET", "/api/master/permissions"), ("POST", WRITE)] {
            let r = operator_send(&mut s, &c.request(method, path, Some(&c.sessions[0]), &[]));
            assert_refused(&r, "caller_identity", "not a concrete IP", "localhost bind");
            checked += 1;
            s = match TcpStream::connect((ip, port)) {
                Ok(s) => s,
                Err(_) => break,
            };
        }
    }
    assert!(checked >= 2, "the localhost board answered no request");
}
