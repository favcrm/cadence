//! Operator sign-in for the board suites (CAD-313): the real flow, no
//! seam. `cadence ui login` runs as the operator's own shell — detached
//! with `setsid -f`, off this test process's ancestry, env cleared, stdio
//! not a pane — the shape `peer::operator_proof` accepts, however the
//! suite itself is run (in an agent pane included). The printed link's
//! fragment nonce is then exchanged at `POST /api/session` for the
//! session cookie, exactly as the SPA's login view does.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Runs argv off `runner`'s ancestry and lands `{rc, stdout, stderr}`.
const OPERATOR_CLI_PY: &str = r#"
import json, os, subprocess, sys, time

out, runner = sys.argv[1:3]
argv = sys.argv[3:]

def on_lineage(pid):
    p = os.getpid()
    while p > 1:
        if p == pid:
            return True
        with open("/proc/%d/status" % p) as f:
            p = int([l for l in f if l.startswith("PPid:")][0].split()[1])
    return False

while on_lineage(int(runner)):
    time.sleep(0.02)
r = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True)
with open(out + ".tmp", "w") as f:
    json.dump({"rc": r.returncode, "stdout": r.stdout.decode(errors="replace"),
               "stderr": r.stderr.decode(errors="replace")}, f)
os.rename(out + ".tmp", out)
"#;

/// Sends one frame on the daemon socket off `runner`'s ancestry and
/// lands the raw response line.
const OPERATOR_RPC_PY: &str = r#"
import os, socket, sys, time

sock_path, frame_path, out, runner = sys.argv[1:5]
frame = open(frame_path).read()

def on_lineage(pid):
    p = os.getpid()
    while p > 1:
        if p == pid:
            return True
        with open("/proc/%d/status" % p) as f:
            p = int([l for l in f if l.startswith("PPid:")][0].split()[1])
    return False

while on_lineage(int(runner)):
    time.sleep(0.02)
s = socket.socket(socket.AF_UNIX)
s.connect(sock_path)
s.sendall((frame + "\n").encode())
line = s.makefile().readline()
s.close()
with open(out + ".tmp", "w") as f:
    f.write(line)
os.rename(out + ".tmp", out)
"#;

/// One raw daemon RPC from an operator-shaped process (as
/// [`operator_cli`]), for requests the CLI would never build — a wrong
/// secret, none. The frame goes through a private file, never argv or
/// the environment. Answers the response frame.
pub fn operator_rpc(socket: &Path, method: &str, params: Value) -> Value {
    let dir = tempfile::Builder::new()
        .prefix("oprpc")
        .tempdir_in("/tmp")
        .unwrap();
    let script = dir.path().join("rpc.py");
    std::fs::write(&script, OPERATOR_RPC_PY).unwrap();
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
    let dir = tempfile::Builder::new()
        .prefix("opcli")
        .tempdir_in("/tmp")
        .unwrap();
    let script = dir.path().join("op.py");
    std::fs::write(&script, OPERATOR_CLI_PY).unwrap();
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
}

impl Session {
    /// A write carrying this session and its own Origin.
    pub fn request(&self, method: &str, path: &str, body: &str) -> String {
        request(
            method,
            path,
            &self.host,
            Some(&self.origin),
            Some(&self.cookie),
            body,
        )
    }

    /// The raw `Name: value\r\n` headers a signed-in write adds.
    pub fn headers(&self) -> String {
        format!("Origin: {}\r\nCookie: {}\r\n", self.origin, self.cookie)
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
    let (status, head, body) = exchange(port, host, &nonce_of(&link));
    assert_eq!(status, 204, "{head}\n{body}");
    let set = set_cookie(&head).unwrap_or_else(|| panic!("no Set-Cookie: {head}"));
    let cookie = set.split(';').next().unwrap().trim().to_string();
    Session {
        host: host.to_string(),
        origin: format!("http://{host}"),
        cookie,
        set_cookie: set,
    }
}
