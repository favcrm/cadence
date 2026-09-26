//! Operator sign-in for the board suites (CAD-313): the real flow.
//! On a seam-armed fixture (CAD-482, `test-seam` builds) the operator
//! identity is asserted in-band — `CADENCE_TEST_AS` on the `ui login`
//! child, `X-Cadence-Test-As`/`X-Cadence-Test-Token` on board requests —
//! identical in a pane and in CI. Without it, `cadence ui login` runs
//! as the operator's own shell — detached with `setsid -f`, off this
//! test process's ancestry, env cleared, stdio not a pane — the shape
//! `peer::operator_proof` accepts, however the suite itself is run
//! (in an agent pane included). The printed link's fragment nonce is
//! then exchanged at `POST /api/session` for the session cookie,
//! exactly as the SPA's login view does.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The wait every operator-shaped script runs before it acts:
/// `on_lineage(pid)` is true while this process can still prove it
/// descends from `pid` by walking /proc PPid links up from itself. A
/// hop can vanish mid-walk — the `setsid -f` detach's intermediates
/// exit fast and the kernel reparents only once they do — and an
/// incomplete chain proves neither tied nor detached: a vanished hop
/// is "not yet proven", so the walk keeps polling instead of raising
/// (CAD-545's rule; one shared copy here, CAD-554).
pub const LINEAGE_WAIT_PY: &str = r#"def on_lineage(pid):
    p = os.getpid()
    while p > 1:
        if p == pid:
            return True
        # A hop can vanish mid-walk — the detach's intermediates exit
        # fast and the kernel reparents only once they do. An incomplete
        # chain proves neither tied nor detached: keep waiting, never die.
        try:
            with open("/proc/%d/status" % p) as f:
                p = int([l for l in f if l.startswith("PPid:")][0].split()[1])
        except (OSError, IndexError):
            return True
    return False

while on_lineage(int(runner)):
    time.sleep(0.02)"#;

/// `head` (imports and argv parsing), the shared lineage wait, `tail`
/// (the action): the shape every operator-shaped script is built from,
/// so the vanished-hop guard above lives in exactly one place. Splicing
/// rather than one `format!` over the whole script keeps each script's
/// own braces untouched.
pub fn lineage_script(head: &str, tail: &str) -> String {
    format!("{head}\n{LINEAGE_WAIT_PY}\n{tail}")
}

/// `TestDaemon::operator_rpc`'s caller (tests/common/mod.rs), also
/// used by tests/board.rs and tests/board_read_model.rs: one raw RPC
/// frame on argv, sent from a caller off `runner`'s ancestry, the
/// response line landed at `out` atomically. The frame rides argv —
/// callers that must keep a value out of `ps` use
/// [`rpc_script_from_file`].
pub fn rpc_script() -> String {
    lineage_script(
        r#"import json, os, socket, sys, time

sock_path, frame, out, runner = sys.argv[1:5]"#,
        r#"s = socket.socket(socket.AF_UNIX)
s.connect(sock_path)
s.sendall((frame + "\n").encode())
line = s.makefile().readline()
s.close()
with open(out + ".tmp", "w") as f:
    f.write(line)
os.rename(out + ".tmp", out)"#,
    )
}

/// [`operator_rpc`]'s caller: one raw RPC frame read from a private
/// file — never argv or the environment — sent from a caller off
/// `runner`'s ancestry; the response line lands at `out`.
pub fn rpc_script_from_file() -> String {
    lineage_script(
        r#"import os, socket, sys, time

sock_path, frame_path, out, runner = sys.argv[1:5]
frame = open(frame_path).read()"#,
        r#"s = socket.socket(socket.AF_UNIX)
s.connect(sock_path)
s.sendall((frame + "\n").encode())
line = s.makefile().readline()
s.close()
with open(out + ".tmp", "w") as f:
    f.write(line)
os.rename(out + ".tmp", out)"#,
    )
}

/// argv run as the operator's own shell, off `runner`'s ancestry, with
/// empty stdin; `{rc, stdout, stderr}` landed at `out`.
pub fn cli_script() -> String {
    lineage_script(
        r#"import json, os, subprocess, sys, time

out, runner = sys.argv[1:3]
argv = sys.argv[3:]"#,
        r#"r = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True)
with open(out + ".tmp", "w") as f:
    json.dump({"rc": r.returncode, "stdout": r.stdout.decode(errors="replace"),
               "stderr": r.stderr.decode(errors="replace")}, f)
os.rename(out + ".tmp", out)"#,
    )
}

/// [`cli_script`] extended to feed one env var to the child's stdin —
/// `--token-stdin` reads it there (tests/platform.rs), keeping the
/// value off argv and out of the output.
pub fn cli_script_stdin_env() -> String {
    lineage_script(
        r#"import json, os, subprocess, sys, time

out, runner = sys.argv[1:3]
argv = sys.argv[3:]"#,
        r#"r = subprocess.run(argv, input=os.environ.get("TOKEN_STDIN", "").encode(),
                   capture_output=True)
with open(out + ".tmp", "w") as f:
    json.dump({"rc": r.returncode, "stdout": r.stdout.decode(errors="replace"),
               "stderr": r.stderr.decode(errors="replace")}, f)
os.rename(out + ".tmp", out)"#,
    )
}

/// A spec'd command run as an operator shell outside every agent and
/// outside the (in-process) daemon's tree: `{argv, env, cwd}` from
/// `spec_path`, then `<out>.stdout`, `<out>.stderr` and `<out>`
/// (`{"rc"}`) landed.
pub fn exec_script() -> String {
    lineage_script(
        r#"import json, os, subprocess, sys, time

spec_path, out, runner = sys.argv[1:4]
spec = json.load(open(spec_path))"#,
        r#"r = subprocess.run(spec["argv"], env=spec["env"], cwd=spec["cwd"],
                   stdin=subprocess.DEVNULL, capture_output=True)
open(out + ".stdout", "wb").write(r.stdout)
open(out + ".stderr", "wb").write(r.stderr)
with open(out + ".tmp", "w") as f:
    json.dump({"rc": r.returncode}, f)
os.rename(out + ".tmp", out)"#,
    )
}

/// One HTTP request read from a file, sent from a caller off
/// `runner`'s ancestry; the raw reply lands at `out_path`
/// (tests/local_outbox.rs).
pub fn http_script() -> String {
    lineage_script(
        r#"import os, socket, sys, time

req_path, out_path, port, runner = sys.argv[1:5]"#,
        r#"s = socket.create_connection(("127.0.0.1", int(port)))
s.sendall(open(req_path, "rb").read())
data = b""
while True:
    chunk = s.recv(65536)
    if not chunk:
        break
    data += chunk
with open(out_path + ".tmp", "wb") as f:
    f.write(data)
os.rename(out_path + ".tmp", out_path)"#,
    )
}

/// The assertion headers an armed fixture board honors for caller
/// `who` (`operator`, `agent:<alias>`, `unproven`), or "" when the seam
/// is not armed on `state` — spliced in before a request's blank line.
pub fn seam_headers(state: &Path, who: &str) -> String {
    match cadence_agent::test_seam::Seam::token_at(state) {
        Some(token) if cfg!(feature = "test-seam") => format!(
            "{}: {who}\r\n{}: {token}\r\n",
            cadence_agent::test_seam::AS_HEADER,
            cadence_agent::test_seam::TOKEN_HEADER,
        ),
        _ => String::new(),
    }
}

/// `req` with the fixture's seam headers for `who` spliced in — a no-op
/// when the daemon on `state` is not armed.
pub fn assert_as(req: String, state: &Path, who: &str) -> String {
    let headers = seam_headers(state, who);
    if headers.is_empty() {
        return req;
    }
    req.replacen("\r\n\r\n", &format!("\r\n{headers}\r\n"), 1)
}

/// One raw daemon RPC from an operator-shaped process (as
/// [`operator_cli`]), for requests the CLI would never build — a wrong
/// secret, none. The frame goes through a private file, never argv or
/// the environment. On a seam-armed fixture the same frame goes
/// in-process with its asserted caller instead (CAD-482). Answers the
/// response frame.
pub fn operator_rpc(socket: &Path, method: &str, params: Value) -> Value {
    if cfg!(feature = "test-seam") {
        if let Some(token) = socket
            .parent()
            .and_then(cadence_agent::test_seam::Seam::token_at)
        {
            use std::os::unix::net::UnixStream;
            let frame = serde_json::json!({
                "method": method,
                "params": params,
                "test_caller": {"token": token, "as": "operator"},
            });
            let mut s = UnixStream::connect(socket).unwrap();
            s.write_all(format!("{frame}\n").as_bytes()).unwrap();
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(s), &mut line).unwrap();
            return serde_json::from_str(&line).unwrap();
        }
    }
    let dir = tempfile::Builder::new()
        .prefix("oprpc")
        .tempdir_in("/tmp")
        .unwrap();
    let script = dir.path().join("rpc.py");
    std::fs::write(&script, rpc_script_from_file()).unwrap();
    let out = dir.path().join("out.json");
    let frame = dir.path().join("frame.json");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&frame)
            .unwrap();
        f.write_all(
            serde_json::json!({"method": method, "params": params})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    }
    let status = Command::new("setsid")
        .arg("-f")
        .arg("python3")
        .arg(&script)
        .arg(socket)
        .arg(&frame)
        .arg(&out)
        .arg(std::process::id().to_string())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "setsid -f failed: {status}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !out.exists() {
        assert!(
            Instant::now() < deadline,
            "operator rpc {method} never answered"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap()
}

/// `cadence --state-dir <state> <args>` as the operator's own shell.
/// Answers `(success, stdout, stderr)`.
pub fn operator_cli(bin: &str, state: &Path, args: &[&str]) -> (bool, String, String) {
    operator_cli_env(bin, state, args, &[])
}

/// [`operator_cli`] with `env` set on the command (after clearing).
pub fn operator_cli_env(
    bin: &str,
    state: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (bool, String, String) {
    cli_as(bin, state, args, env, "operator")
}

/// [`operator_cli_env`] asserting `who` (`operator`, `agent:<alias>`,
/// `unproven`) when `state` is armed; on an unarmed fixture `env` alone
/// shapes the ambient caller — the `env` an agent-shaped caller needs
/// (`CADENCE_ALIAS`, say) travels in `env` either way.
pub fn cli_as(
    bin: &str,
    state: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    who: &str,
) -> (bool, String, String) {
    let dir = tempfile::Builder::new()
        .prefix("opcli")
        .tempdir_in("/tmp")
        .unwrap();
    if cadence_agent::test_seam::armed(state) {
        // CAD-482: the child asserts `who` through CADENCE_TEST_AS —
        // identical in a pane and in CI.
        let out = Command::new(bin)
            .arg("--state-dir")
            .arg(state)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", dir.path())
            .env(cadence_agent::test_seam::AS_ENV, who)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        return (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        );
    }
    let script = dir.path().join("op.py");
    std::fs::write(&script, cli_script()).unwrap();
    let out = dir.path().join("out.json");
    let status = Command::new("setsid")
        .arg("-f")
        .arg("python3")
        .arg(&script)
        .arg(&out)
        .arg(std::process::id().to_string())
        .arg(bin)
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.path())
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "setsid -f failed: {status}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !out.exists() {
        assert!(
            Instant::now() < deadline,
            "operator cadence {args:?} never finished"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    (
        v["rc"] == 0,
        v["stdout"].as_str().unwrap_or_default().to_string(),
        v["stderr"].as_str().unwrap_or_default().to_string(),
    )
}

/// The nonce in a login link's fragment.
pub fn nonce_of(link: &str) -> String {
    link.split_once("#n=")
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| panic!("no fragment nonce in {link}"))
}

/// `cadence ui login --json --port <port> [extra]` as the operator:
/// the minted link, or the failure's stderr.
pub fn login_link(bin: &str, state: &Path, port: u16, extra: &[&str]) -> Result<String, String> {
    let port = port.to_string();
    let mut args = vec!["ui", "login", "--json", "--port", &port];
    args.extend_from_slice(extra);
    let (ok, stdout, stderr) = operator_cli(bin, state, &args);
    if !ok {
        return Err(format!("{stdout}{stderr}"));
    }
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    Ok(v["link"].as_str().unwrap().to_string())
}

/// One raw HTTP/1.0 exchange: `(status, head, body)`.
pub fn raw(port: u16, request: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).ok();
    s.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (status, head.to_string(), body.to_string())
}

/// A board request with the write guards, `Origin: <origin>` (none when
/// `None`) and `cookie` (none when `None`).
pub fn request(
    method: &str,
    path: &str,
    host: &str,
    origin: Option<&str>,
    cookie: Option<&str>,
    body: &str,
) -> String {
    let mut r = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\n"
    );
    if let Some(o) = origin {
        r.push_str(&format!("Origin: {o}\r\n"));
    }
    if let Some(c) = cookie {
        r.push_str(&format!("Cookie: {c}\r\n"));
    }
    r.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    r
}

/// The `name=value` a response sets, from its `Set-Cookie` header.
pub fn set_cookie(head: &str) -> Option<String> {
    head.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .map(|l| l.split_once(':').unwrap().1.trim().to_string())
}

/// A signed-in board client: its Host, Origin and cookie pair.
#[derive(Clone, Debug)]
pub struct Session {
    pub host: String,
    pub origin: String,
    pub cookie: String,
    /// The full `Set-Cookie` value, for attribute checks.
    pub set_cookie: String,
    /// The session's second credential, sent as `X-Cadence-Session`.
    pub key: String,
    /// The caller-assertion headers (CAD-482) when the fixture is
    /// seam-armed — sent on every session write; "" otherwise.
    pub seam: String,
}

impl Session {
    /// A write carrying this session and its own Origin, asserted as
    /// the session's operator caller (or ambient, when unarmed).
    pub fn request(&self, method: &str, path: &str, body: &str) -> String {
        self.request_as(method, path, body, &self.seam)
    }

    /// `request` with a different caller-assertion block — a session
    /// replayed by another asserted caller (`agent:<alias>`) or none at
    /// all (`""`). Use [`seam_headers`] to build the block.
    pub fn request_as(&self, method: &str, path: &str, body: &str, seam: &str) -> String {
        let req = request(
            method,
            path,
            &self.host,
            Some(&self.origin),
            Some(&self.cookie),
            body,
        );
        req.replacen(
            "X-Cadence-Board: 1\r\n",
            &format!("X-Cadence-Board: 1\r\n{}\r\n{}", self.key_header(), seam),
            1,
        )
    }

    /// The raw `Name: value\r\n` headers a signed-in write adds,
    /// asserted as the session's caller.
    pub fn headers(&self) -> String {
        self.headers_as(&self.seam)
    }

    /// `headers` with a different caller-assertion block — like
    /// [`Session::request_as`] for callers that build requests by hand.
    pub fn headers_as(&self, seam: &str) -> String {
        format!(
            "Origin: {}\r\nCookie: {}\r\n{}\r\n{}",
            self.origin,
            self.cookie,
            self.key_header(),
            seam
        )
    }

    /// `X-Cadence-Session: <key>` (no line end).
    pub fn key_header(&self) -> String {
        format!("X-Cadence-Session: {}", self.key)
    }
}

/// Exchange `nonce` at `host` (Origin `http://<host>`).
pub fn exchange(port: u16, host: &str, nonce: &str) -> (u16, String, String) {
    raw(
        port,
        &request(
            "POST",
            "/api/session",
            host,
            Some(&format!("http://{host}")),
            None,
            &format!(r#"{{"nonce":"{nonce}"}}"#),
        ),
    )
}

/// This board's own name — the only loopback Host a session lives on.
pub fn board_host(port: u16) -> String {
    format!("cadence-{port}.localhost:{port}")
}

/// Sign in to the board on `port` (state dir `state`) as the operator,
/// on the board's own Host ([`board_host`]).
pub fn sign_in(bin: &str, state: &Path, port: u16) -> Session {
    sign_in_at(bin, state, port, &board_host(port))
}

/// [`sign_in`] on an explicit loopback Host.
pub fn sign_in_at(bin: &str, state: &Path, port: u16, host: &str) -> Session {
    let link = login_link(bin, state, port, &[]).unwrap_or_else(|e| panic!("ui login: {e}"));
    let req = assert_as(
        request(
            "POST",
            "/api/session",
            host,
            Some(&format!("http://{host}")),
            None,
            &format!(r#"{{"nonce":"{}"}}"#, nonce_of(&link)),
        ),
        state,
        "operator",
    );
    let (status, head, body) = raw(port, &req);
    assert_eq!(status, 200, "{head}\n{body}");
    let set = set_cookie(&head).unwrap_or_else(|| panic!("no Set-Cookie: {head}"));
    let cookie = set.split(';').next().unwrap().trim().to_string();
    let key = serde_json::from_str::<Value>(&body).unwrap()["session_key"]
        .as_str()
        .unwrap()
        .to_string();
    Session {
        host: host.to_string(),
        origin: format!("http://{host}"),
        cookie,
        set_cookie: set,
        key,
        seam: seam_headers(state, "operator"),
    }
}
