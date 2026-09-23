//! Proof that a board request came through `tailscale serve` (CAD-336)
//! — the only caller whose `Tailscale-User-*` headers mean anything.
//!
//! Host and a loopback peer prove nothing: any local process can send
//! both. What the board can check, in this order (the first that fails
//! is the [`Refusal`]'s [`Check`]):
//!
//! 1. `loopback` — the peer is a loopback address (serve dials locally);
//! 2. `tailscaled_socket` — tailscaled's LocalAPI socket is found and is
//!    a socket, not a symlink; its owner is tailscaled's uid (only that
//!    user can create it under `/run/tailscale`);
//! 3. `localapi` — the LocalAPI answers `status` and `serve-config`;
//! 4. `kernel_networking` — tailscaled uses a TUN device. Under
//!    userspace networking it dials `127.0.0.1:<port>` itself for any
//!    tailnet peer the ACL lets reach the port, carrying that peer's
//!    bytes — tagged nodes included;
//! 5. `no_tcp_forwarder` — no serve `TCPForward` handler (`serve
//!    --tcp`, `tcp://`) anywhere in the serve config targets the
//!    board's port: a raw forwarder passes the client's headers through
//!    untouched, where the HTTPS proxy replaces them;
//! 6. `client_socket` — the connection's client socket is listed in
//!    `/proc/net/tcp{,6}`;
//! 7. `socket_owner` — that socket was created by tailscaled's uid (the
//!    table's uid column, readable for another user's socket);
//! 8. `foreign_uid` — tailscaled's uid is not the board's: otherwise any
//!    same-uid process could pose as it.
//!
//! What stays unproven, and is documented in `docs/BOARD.md`: whoever
//! can make tailscaled open a connection to the board port can still
//! mint an identity. That is root, and tailscaled's operator user
//! (`tailscale set --operator`), which can add a TCP forwarder between
//! two reads — the LocalAPI facts are cached for [`CACHE_TTL`]. A local
//! process may also browse the tailnet URL itself: the proxy then names
//! this node's owner, as it would for any tailnet client on this node.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::peer::{canonical, client_socket, proc_uids};

/// Where tailscaled keeps its LocalAPI socket on Linux (`/var/run` is
/// normally a symlink to `/run`).
pub const DEFAULT_SOCKETS: [&str; 2] = [
    "/run/tailscale/tailscaled.sock",
    "/var/run/tailscale/tailscaled.sock",
];

/// How long one LocalAPI read of tailscaled's facts is reused.
pub const CACHE_TTL: Duration = Duration::from_secs(2);

const LOCALAPI_TIMEOUT: Duration = Duration::from_secs(2);
const LOCALAPI_MAX_BYTES: u64 = 4 << 20;

/// The check a request failed — see the module doc for the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Loopback,
    TailscaledSocket,
    Localapi,
    KernelNetworking,
    NoTcpForwarder,
    ClientSocket,
    SocketOwner,
    ForeignUid,
}

impl Check {
    pub fn as_str(self) -> &'static str {
        match self {
            Check::Loopback => "loopback",
            Check::TailscaledSocket => "tailscaled_socket",
            Check::Localapi => "localapi",
            Check::KernelNetworking => "kernel_networking",
            Check::NoTcpForwarder => "no_tcp_forwarder",
            Check::ClientSocket => "client_socket",
            Check::SocketOwner => "socket_owner",
            Check::ForeignUid => "foreign_uid",
        }
    }
}

/// Why a request is not the proxy: the first failed check and what it
/// saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub check: Check,
    pub why: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {}", self.check.as_str(), self.why)
    }
}

fn refuse(check: Check, why: impl Into<String>) -> Refusal {
    Refusal {
        check,
        why: why.into(),
    }
}

/// What tailscaled reports about itself over its LocalAPI.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Facts {
    /// `status.TUN`: kernel networking. False is userspace networking.
    tun: bool,
    /// Every `TCPForward` target anywhere in the serve config —
    /// background, foreground sessions and services alike.
    tcp_forwards: Vec<String>,
}

/// Is the TCP peer `peer` of a connection to the board's `board_port`
/// the `tailscale serve` HTTPS proxy? `socket` is tailscaled's LocalAPI
/// socket — `None` looks in [`DEFAULT_SOCKETS`]; tests inject a fixture.
pub fn prove(socket: Option<&Path>, board_port: u16, peer: SocketAddr) -> Result<(), Refusal> {
    let peer = canonical(peer);
    if !peer.ip().is_loopback() {
        return Err(refuse(
            Check::Loopback,
            format!("peer {peer} is not loopback — tailscale serve connects locally"),
        ));
    }
    let (path, tailscaled_uid) = anchor(socket)?;
    let facts = facts(&path).map_err(|e| refuse(Check::Localapi, e))?;
    let socket_uid = match client_socket(board_port, peer) {
        Ok(Some((_, uid))) => uid,
        Ok(None) => {
            return Err(refuse(
                Check::ClientSocket,
                format!("no local socket is the client end of {peer} → port {board_port}"),
            ));
        }
        Err(e) => return Err(refuse(Check::ClientSocket, e)),
    };
    let (_, own_uid) = proc_uids(std::process::id()).map_err(|e| refuse(Check::ForeignUid, e))?;
    decide(&facts, board_port, tailscaled_uid, socket_uid, own_uid)
}

/// Checks 4, 5, 7 and 8 once everything is read.
fn decide(
    facts: &Facts,
    board_port: u16,
    tailscaled_uid: u32,
    socket_uid: u32,
    own_uid: u32,
) -> Result<(), Refusal> {
    if !facts.tun {
        return Err(refuse(
            Check::KernelNetworking,
            "tailscaled runs with userspace networking — it dials loopback for any \
             tailnet peer, so its sockets carry that peer's bytes",
        ));
    }
    if let Some(target) = facts
        .tcp_forwards
        .iter()
        .find(|t| forwards_to(t, board_port))
    {
        return Err(refuse(
            Check::NoTcpForwarder,
            format!(
                "the serve config has a TCP forwarder to {target} — raw TCP to the board's \
                 port {board_port} passes client headers through"
            ),
        ));
    }
    if socket_uid != tailscaled_uid {
        return Err(refuse(
            Check::SocketOwner,
            format!("the client socket belongs to uid {socket_uid}, not tailscaled's uid {tailscaled_uid}"),
        ));
    }
    if tailscaled_uid == own_uid {
        return Err(refuse(
            Check::ForeignUid,
            format!(
                "tailscaled runs as this board's uid {own_uid} — any same-uid process \
                 could pose as its proxy"
            ),
        ));
    }
    Ok(())
}

/// Does a `TCPForward` target (`host:port`) reach `port`? A target
/// whose port cannot be read is assumed to — fail closed.
fn forwards_to(target: &str, port: u16) -> bool {
    target
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .is_none_or(|p| p == port)
}

/// tailscaled's LocalAPI socket and its owner uid — the first
/// candidate that is a socket itself (a symlink is never followed).
fn anchor(socket: Option<&Path>) -> Result<(PathBuf, u32), Refusal> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let candidates: Vec<PathBuf> = match socket {
        Some(p) => vec![p.to_path_buf()],
        None => DEFAULT_SOCKETS.iter().map(PathBuf::from).collect(),
    };
    candidates
        .iter()
        .find_map(|path| {
            let meta = std::fs::symlink_metadata(path).ok()?;
            meta.file_type()
                .is_socket()
                .then(|| (path.clone(), meta.uid()))
        })
        .ok_or_else(|| {
            let names: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
            refuse(
                Check::TailscaledSocket,
                format!(
                    "tailscaled's LocalAPI socket is not at {} — its uid is unknown",
                    names.join(" or ")
                ),
            )
        })
}

type Cached = (Instant, Result<Facts, String>);

/// [`read_facts`] through a per-socket cache of [`CACHE_TTL`].
fn facts(socket: &Path) -> Result<Facts, String> {
    static CACHE: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);
    let mut guard = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some((at, facts)) = cache.get(socket) {
        if at.elapsed() < CACHE_TTL {
            return facts.clone();
        }
    }
    let facts = read_facts(socket);
    cache.insert(socket.to_path_buf(), (Instant::now(), facts.clone()));
    facts
}

fn read_facts(socket: &Path) -> Result<Facts, String> {
    let status = localapi_get(socket, "/localapi/v0/status?peers=false")?;
    let tun = status["TUN"]
        .as_bool()
        .ok_or_else(|| "LocalAPI status carries no TUN field".to_string())?;
    let serve = localapi_get(socket, "/localapi/v0/serve-config")?;
    let mut tcp_forwards = Vec::new();
    collect_tcp_forwards(&serve, &mut tcp_forwards);
    Ok(Facts { tun, tcp_forwards })
}

/// Every non-empty `TCPForward` string at any depth — the serve config
/// nests handlers under `TCP`, `Foreground.<session>` and
/// `Services.<name>`, and a walk needs no knowledge of that layout.
fn collect_tcp_forwards(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                match (k.as_str(), v) {
                    ("TCPForward", Value::String(t)) if !t.is_empty() => out.push(t.clone()),
                    _ => collect_tcp_forwards(v, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_tcp_forwards(v, out)),
        _ => {}
    }
}

/// One LocalAPI GET over the unix socket — HTTP/1.0, so the answer is
/// never chunked and ends at EOF. Anything but `200` + JSON is an error.
fn localapi_get(socket: &Path, path: &str) -> Result<Value, String> {
    let at = || format!("LocalAPI {path} at {}", socket.display());
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("{}: {e}", at()))?;
    stream
        .set_read_timeout(Some(LOCALAPI_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(LOCALAPI_TIMEOUT)))
        .map_err(|e| format!("{}: {e}", at()))?;
    // One write: a request split across writes may meet a server that
    // reads it in pieces.
    let request = format!("GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("{}: {e}", at()))?;
    let mut raw = Vec::new();
    stream
        .take(LOCALAPI_MAX_BYTES)
        .read_to_end(&mut raw)
        .map_err(|e| format!("{}: {e}", at()))?;
    parse_response(&raw).map_err(|e| format!("{}: {e}", at()))
}

fn parse_response(raw: &[u8]) -> Result<Value, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no complete HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let code = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_default();
    if code != "200" {
        return Err(format!("answered http {code}"));
    }
    serde_json::from_slice(&raw[split + 4..]).map_err(|e| format!("malformed JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::{TcpListener, TcpStream};

    fn good() -> Facts {
        Facts {
            tun: true,
            tcp_forwards: vec!["127.0.0.1:22".into()],
        }
    }

    fn check(r: Result<(), Refusal>) -> Option<Check> {
        r.err().map(|r| r.check)
    }

    /// tailscaled as root (0), board as 1000, board port 3010: each
    /// check refuses on its own condition and names itself.
    #[test]
    fn decide_names_the_failed_check() {
        assert_eq!(decide(&good(), 3010, 0, 0, 1000), Ok(()));
        let userspace = Facts {
            tun: false,
            ..good()
        };
        assert_eq!(
            check(decide(&userspace, 3010, 0, 0, 1000)),
            Some(Check::KernelNetworking)
        );
        for target in ["127.0.0.1:3010", "localhost:3010", "[::1]:3010", "garbage"] {
            let fwd = Facts {
                tcp_forwards: vec![target.into()],
                ..good()
            };
            assert_eq!(
                check(decide(&fwd, 3010, 0, 0, 1000)),
                Some(Check::NoTcpForwarder),
                "{target}"
            );
        }
        assert_eq!(
            check(decide(&good(), 3010, 0, 1000, 1000)),
            Some(Check::SocketOwner)
        );
        assert_eq!(
            check(decide(&good(), 3010, 0, 1001, 1000)),
            Some(Check::SocketOwner)
        );
        assert_eq!(
            check(decide(&good(), 3010, 1000, 1000, 1000)),
            Some(Check::ForeignUid)
        );
    }

    #[test]
    fn tcp_forwards_are_found_at_any_depth() {
        let serve = json!({
            "TCP": {"443": {"HTTPS": true}, "2222": {"TCPForward": "127.0.0.1:22"}},
            "Foreground": {"sess": {"TCP": {"9999": {"TCPForward": "127.0.0.1:3010"}}}},
            "Services": {"svc:x": {"TCP": {"80": {"TCPForward": "localhost:8080"}}}},
            "Web": {"h:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3010"}}}}
        });
        let mut out = Vec::new();
        collect_tcp_forwards(&serve, &mut out);
        out.sort();
        assert_eq!(out, ["127.0.0.1:22", "127.0.0.1:3010", "localhost:8080"]);
        let mut none = Vec::new();
        collect_tcp_forwards(&Value::Null, &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn localapi_responses_parse_or_refuse() {
        let ok = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"TUN\":true}";
        assert_eq!(parse_response(ok).unwrap()["TUN"], true);
        assert!(parse_response(b"HTTP/1.0 403 Forbidden\r\n\r\n{}").is_err());
        assert!(parse_response(b"HTTP/1.0 200 OK\r\n\r\nnot json").is_err());
        assert!(parse_response(b"HTTP/1.0 200 OK").is_err());
    }

    /// Wiring, not just the pure decision: each early check refuses by
    /// name from `prove` itself.
    #[test]
    fn prove_refuses_by_name_before_the_uid_checks() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();

        let remote: SocketAddr = "192.0.2.7:40000".parse().unwrap();
        assert_eq!(check(prove(None, port, remote)), Some(Check::Loopback));

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.sock");
        assert_eq!(
            check(prove(Some(&missing), port, peer)),
            Some(Check::TailscaledSocket)
        );
        let file = dir.path().join("file.sock");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(
            check(prove(Some(&file), port, peer)),
            Some(Check::TailscaledSocket)
        );
        // A socket that answers nothing useful: the LocalAPI refusal.
        let sock = dir.path().join("mute.sock");
        let mute = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            for s in mute.incoming().flatten() {
                drop(s);
            }
        });
        assert_eq!(check(prove(Some(&sock), port, peer)), Some(Check::Localapi));
    }
}
