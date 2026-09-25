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
//! 3. `localapi` — the LocalAPI answers `status`, `prefs` and
//!    `serve-config`;
//! 4. `kernel_networking` — tailscaled uses a TUN device. Under
//!    userspace networking it dials `127.0.0.1:<port>` itself for any
//!    tailnet peer the ACL lets reach the port, carrying that peer's
//!    bytes — tagged nodes included;
//! 5. `not_operator_user` — the board's uid is not tailscaled's
//!    `OperatorUser` (prefs; the name resolved to a uid). The operator
//!    user may reconfigure serve without root, so it can make tailscaled
//!    dial the board on its behalf at any time: add a TCP forwarder,
//!    open a connection through it, remove the forwarder, and send the
//!    request later — no config read can see that. While the board runs
//!    as that user, every process of it (agents included) could mint any
//!    identity, so none is trusted;
//! 6. `operator_latched` — the board's uid was never tailscaled's
//!    operator user during this board process's life ([`OperatorLatch`]):
//!    read at startup and at every later read. A connection set up while
//!    it was — through a forwarder since removed — outlives the operator
//!    clearing itself, so one sighting (or a failed startup read)
//!    refuses tailnet identity until the board restarts;
//! 7. `no_tcp_forwarder` — no serve `TCPForward` handler (`serve
//!    --tcp`, `tcp://`) anywhere in the serve config targets the
//!    board's port: a raw forwarder passes the client's headers through
//!    untouched, where the HTTPS proxy replaces them. This catches a
//!    standing or accidental forwarder; a deliberate one is checks 5-6's;
//! 8. `client_socket` — the connection's client socket is listed in
//!    `/proc/net/tcp{,6}`;
//! 9. `socket_owner` — that socket was created by tailscaled's uid (the
//!    table's uid column, readable for another user's socket);
//! 10. `foreign_uid` — tailscaled's uid is not the board's: otherwise any
//!     same-uid process could pose as it.
//!
//! What stays unproven, and is documented in `docs/BOARD.md`: root — and
//! so a board user that can gain root, e.g. by passwordless sudo — and
//! anyone who is tailscaled's operator user while the board is not (they
//! can still make tailscaled dial the board). A request the proxy sends
//! without a login — Funnel from the internet, a tagged node — is proven
//! to come through serve but names nobody; the board refuses its writes.
//! A local process may also browse the tailnet URL itself: the proxy then
//! names this node's owner, as it would for any tailnet client here.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
/// Declaration order is the proof order: `Ord` sorts a set of
/// refusals to the first rung a request would fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
    Loopback,
    TailscaledSocket,
    Localapi,
    KernelNetworking,
    NotOperatorUser,
    OperatorLatched,
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
            Check::NotOperatorUser => "not_operator_user",
            Check::OperatorLatched => "operator_latched",
            Check::NoTcpForwarder => "no_tcp_forwarder",
            Check::ClientSocket => "client_socket",
            Check::SocketOwner => "socket_owner",
            Check::ForeignUid => "foreign_uid",
        }
    }

    /// The check `name` — `/api/meta` serialises `check` as
    /// [`as_str`](Check::as_str); a name it could not have written is
    /// `None`.
    pub fn named(name: &str) -> Option<Check> {
        [
            Check::Loopback,
            Check::TailscaledSocket,
            Check::Localapi,
            Check::KernelNetworking,
            Check::NotOperatorUser,
            Check::OperatorLatched,
            Check::NoTcpForwarder,
            Check::ClientSocket,
            Check::SocketOwner,
            Check::ForeignUid,
        ]
        .into_iter()
        .find(|c| c.as_str() == name)
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
    /// `prefs.OperatorUser` resolved to a uid: `None` when no operator
    /// is set, `Err` when the name resolves to no user.
    operator_uid: Result<Option<u32>, String>,
    /// Every `TCPForward` target anywhere in the serve config —
    /// background, foreground sessions and services alike.
    tcp_forwards: Vec<String>,
}

/// A board process's memory of tailscaled's operator user (check 6):
/// once the board's uid has been the operator — or the startup read
/// failed — tailnet identity stays refused until the process restarts.
/// A non-operator, non-root user cannot become the operator, so a board
/// that starts unlatched never meets a connection its user set up
/// through tailscaled; one that saw its user as operator may, even after
/// the operator is cleared.
///
/// `Default` is latched: only [`OperatorLatch::at_startup`], a real read,
/// yields an unlatched latch. Clones share the state.
#[derive(Clone, Debug)]
pub struct OperatorLatch(Arc<Mutex<Option<String>>>);

impl Default for OperatorLatch {
    fn default() -> Self {
        Self::latched("the board never read tailscaled's operator user at startup")
    }
}

impl OperatorLatch {
    fn latched(why: impl Into<String>) -> Self {
        Self(Arc::new(Mutex::new(Some(why.into()))))
    }

    /// Read tailscaled's operator user now, at board startup, through
    /// `socket` (`None`: [`DEFAULT_SOCKETS`]). Latched when the read
    /// fails, the name resolves to no user, or it is the board's uid.
    pub fn at_startup(socket: Option<&Path>) -> Self {
        let read = anchor(socket)
            .map_err(|r| r.why)
            .and_then(|(path, _)| operator_uid(&path))
            .and_then(|operator| {
                let (_, own) = proc_uids(std::process::id())?;
                Ok((operator, own))
            });
        let latch = Self(Arc::new(Mutex::new(None)));
        match read {
            Ok((operator, own)) => latch.observe(&operator, own),
            Err(why) => latch.observe(&Err(why), 0),
        }
        latch
    }

    /// Latch on a sighting of the board's uid as operator, or on an
    /// operator that could not be resolved. Never unlatches.
    fn observe(&self, operator: &Result<Option<u32>, String>, own_uid: u32) {
        let why = match operator {
            Ok(Some(uid)) if *uid == own_uid => format!(
                "the board's uid {own_uid} was tailscaled's operator user during this \
                 board's life — a connection set up then may outlive the operator \
                 clearing itself; restart the board"
            ),
            Err(e) => format!(
                "tailscaled's operator user could not be read during this board's life \
                 ({e}); restart the board"
            ),
            Ok(_) => return,
        };
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state.get_or_insert(why);
    }

    fn reason(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// Is the TCP peer `peer` of a connection to the board's `board_port`
/// the `tailscale serve` HTTPS proxy? `socket` is tailscaled's LocalAPI
/// socket — `None` looks in [`DEFAULT_SOCKETS`]; tests inject a fixture.
/// `latch` is this board process's [`OperatorLatch`].
pub fn prove(
    socket: Option<&Path>,
    latch: &OperatorLatch,
    board_port: u16,
    peer: SocketAddr,
) -> Result<(), Refusal> {
    let peer = canonical(peer);
    if !peer.ip().is_loopback() {
        return Err(refuse(
            Check::Loopback,
            format!("peer {peer} is not loopback — tailscale serve connects locally"),
        ));
    }
    let (path, tailscaled_uid) = anchor(socket)?;
    let (_, own_uid) = proc_uids(std::process::id()).map_err(|e| refuse(Check::ForeignUid, e))?;
    let facts = facts(&path).map_err(|e| refuse(Check::Localapi, e))?;
    latch.observe(&facts.operator_uid, own_uid);
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
    decide(
        &facts,
        latch.reason(),
        board_port,
        tailscaled_uid,
        socket_uid,
        own_uid,
    )
}

/// The refusals the host-side facts alone decide — `kernel_networking`,
/// `not_operator_user`, `no_tcp_forwarder` and `foreign_uid`, in proof
/// order. `decide` merges the board-memory `operator_latched` and the
/// per-connection `socket_owner` between them; [`host_refusals`] reports
/// this list whole so every remedy can print at once.
fn fact_refusals(
    facts: &Facts,
    own_uid: u32,
    board_port: u16,
    tailscaled_uid: u32,
) -> Vec<Refusal> {
    let mut out = Vec::new();
    if !facts.tun {
        out.push(refuse(
            Check::KernelNetworking,
            "tailscaled runs with userspace networking — it dials loopback for any \
             tailnet peer, so its sockets carry that peer's bytes",
        ));
    }
    match &facts.operator_uid {
        Err(why) => out.push(refuse(Check::NotOperatorUser, why.clone())),
        Ok(Some(uid)) if *uid == own_uid => {
            out.push(refuse(
                Check::NotOperatorUser,
                format!(
                    "the board's uid {own_uid} is tailscaled's operator user — it can make \
                     tailscaled dial the board with any headers (sudo tailscale set \
                     --operator= clears it)"
                ),
            ));
        }
        Ok(_) => {}
    }
    if let Some(target) = facts
        .tcp_forwards
        .iter()
        .find(|t| forwards_to(t, board_port))
    {
        out.push(refuse(
            Check::NoTcpForwarder,
            format!(
                "the serve config has a TCP forwarder to {target} — raw TCP to the board's \
                 port {board_port} passes client headers through"
            ),
        ));
    }
    if tailscaled_uid == own_uid {
        out.push(refuse(
            Check::ForeignUid,
            format!(
                "tailscaled runs as this board's uid {own_uid} — any same-uid process \
                 could pose as its proxy"
            ),
        ));
    }
    out
}

/// Checks 4-7, 9 and 10 once everything is read; `latched` is the
/// [`OperatorLatch`]'s reason, if any.
fn decide(
    facts: &Facts,
    latched: Option<String>,
    board_port: u16,
    tailscaled_uid: u32,
    socket_uid: u32,
    own_uid: u32,
) -> Result<(), Refusal> {
    let latched = latched.map(|why| refuse(Check::OperatorLatched, why));
    let socket_owner = (socket_uid != tailscaled_uid).then(|| {
        refuse(
            Check::SocketOwner,
            format!(
                "the client socket belongs to uid {socket_uid}, not tailscaled's uid {tailscaled_uid}"
            ),
        )
    });
    // Check order is the proof order — the smallest failing rung wins.
    fact_refusals(facts, own_uid, board_port, tailscaled_uid)
        .into_iter()
        .chain([latched, socket_owner].into_iter().flatten())
        .min_by_key(|r| r.check)
        .map_or(Ok(()), Err)
}

/// The host-side rungs of the proof, read up front so `doctor --host`
/// can print the whole remedy chain before a sign-in link is spent
/// (CAD-509). `own_uid` stands in for the board's uid — a board `ui
/// start` launches runs as its starter — and `board_port` for its port.
///
/// Not decidable here: `loopback`, `client_socket` and `socket_owner`
/// are per-connection, and `operator_latched` is the running board's
/// memory — a live board's `/api/meta` reports it. An unreadable rung
/// ends the pass: below the socket, then the LocalAPI, nothing can be
/// read.
pub fn host_refusals(socket: Option<&Path>, own_uid: u32, board_port: u16) -> Vec<Refusal> {
    let (path, tailscaled_uid) = match anchor(socket) {
        Ok(found) => found,
        Err(r) => return vec![r],
    };
    match read_facts(&path) {
        Err(e) => vec![refuse(Check::Localapi, e)],
        Ok(facts) => fact_refusals(&facts, own_uid, board_port, tailscaled_uid),
    }
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

/// [`read_facts`] through a per-socket cache of [`CACHE_TTL`]. The
/// lock is not held across the LocalAPI reads: a slow tailscaled delays
/// only the requests that need a fresh read.
fn facts(socket: &Path) -> Result<Facts, String> {
    static CACHE: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);
    let cached = CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(HashMap::new)
        .get(socket)
        .filter(|(at, _)| at.elapsed() < CACHE_TTL)
        .map(|(_, facts)| facts.clone());
    if let Some(facts) = cached {
        return facts;
    }
    let facts = read_facts(socket);
    CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(socket.to_path_buf(), (Instant::now(), facts.clone()));
    facts
}

fn read_facts(socket: &Path) -> Result<Facts, String> {
    let status = localapi_get(socket, "/localapi/v0/status?peers=false")?;
    let tun = status["TUN"]
        .as_bool()
        .ok_or_else(|| "LocalAPI status carries no TUN field".to_string())?;
    let operator_uid = operator_uid(socket)?;
    let serve = localapi_get(socket, "/localapi/v0/serve-config")?;
    let mut tcp_forwards = Vec::new();
    collect_tcp_forwards(&serve, &mut tcp_forwards);
    Ok(Facts {
        tun,
        operator_uid,
        tcp_forwards,
    })
}

/// `prefs.OperatorUser` resolved to a uid. The outer `Err` is an
/// unreadable LocalAPI — or a prefs body that is not a JSON object;
/// the inner one a name that resolves to no user. tailscaled omits an
/// empty `OperatorUser` (`omitempty`), so the field absent — or null —
/// is "no operator user", not a read failure; any other non-string
/// still fails closed.
fn operator_uid(socket: &Path) -> Result<Result<Option<u32>, String>, String> {
    let prefs = localapi_get(socket, "/localapi/v0/prefs")?;
    let map = prefs
        .as_object()
        .ok_or_else(|| "LocalAPI prefs is not a JSON object".to_string())?;
    Ok(match map.get("OperatorUser") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) if name.is_empty() => Ok(None),
        Some(Value::String(name)) => uid_of(name).map(Some),
        Some(_) => Err("LocalAPI prefs' OperatorUser is not a user name".to_string()),
    })
}

/// The uid of user `name` (`getpwnam_r`, so NSS users resolve too).
fn uid_of(name: &str) -> Result<u32, String> {
    let c_name = std::ffi::CString::new(name)
        .map_err(|_| format!("tailscaled's operator user {name:?} is not a valid name"))?;
    // SAFETY: `passwd` is plain data; getpwnam_r fills it, pointing its
    // strings into `buf`, which outlives every read of `pwd` below.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwnam_r(
            c_name.as_ptr(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        )
    };
    if rc != 0 {
        return Err(format!(
            "cannot resolve tailscaled's operator user {name:?}: {}",
            std::io::Error::from_raw_os_error(rc)
        ));
    }
    if found.is_null() {
        return Err(format!(
            "tailscaled's operator user {name:?} is no user on this host"
        ));
    }
    Ok(pwd.pw_uid)
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
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::net::{TcpListener, TcpStream};

    /// A fake tailscaled LocalAPI on a unix socket under `dir`:
    /// every request answers `<dir>/status.json`, `prefs.json` or
    /// `serve.json` read fresh per connection — a 500 when the file
    /// is absent — so a test rewrites answers between reads.
    /// `pub(crate)` so `doctor::host`'s tests share the fixture.
    pub(crate) fn localapi(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let sock = dir.join("ts.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let root = dir.to_path_buf();
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                // Read through the end of the request head.
                let mut raw = Vec::new();
                let mut buf = [0u8; 512];
                while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    match conn.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => raw.extend_from_slice(&buf[..n]),
                    }
                }
                let req = String::from_utf8_lossy(&raw).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or_default();
                let file = if path.starts_with("/localapi/v0/status") {
                    Some("status.json")
                } else if path == "/localapi/v0/prefs" {
                    Some("prefs.json")
                } else if path == "/localapi/v0/serve-config" {
                    Some("serve.json")
                } else {
                    None
                };
                let resp = match file.and_then(|f| std::fs::read(root.join(f)).ok()) {
                    Some(body) => [b"HTTP/1.0 200 OK\r\n\r\n".as_slice(), &body].concat(),
                    None => b"HTTP/1.0 500 Internal Server Error\r\n\r\n".to_vec(),
                };
                let _ = conn.write_all(&resp);
            }
        });
        sock
    }

    /// Write one of the fixture's LocalAPI answers.
    pub(crate) fn localapi_says(dir: &Path, file: &str, body: Value) {
        std::fs::write(dir.join(file), body.to_string()).unwrap();
    }

    fn good() -> Facts {
        Facts {
            tun: true,
            operator_uid: Ok(Some(1001)),
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
        assert_eq!(decide(&good(), None, 3010, 0, 0, 1000), Ok(()));
        let userspace = Facts {
            tun: false,
            ..good()
        };
        assert_eq!(
            check(decide(&userspace, None, 3010, 0, 0, 1000)),
            Some(Check::KernelNetworking)
        );
        let no_operator = Facts {
            operator_uid: Ok(None),
            ..good()
        };
        assert_eq!(decide(&no_operator, None, 3010, 0, 0, 1000), Ok(()));
        let board_is_operator = Facts {
            operator_uid: Ok(Some(1000)),
            ..good()
        };
        assert_eq!(
            check(decide(&board_is_operator, None, 3010, 0, 0, 1000)),
            Some(Check::NotOperatorUser)
        );
        let unknown_operator = Facts {
            operator_uid: Err("no such user".into()),
            ..good()
        };
        assert_eq!(
            check(decide(&unknown_operator, None, 3010, 0, 0, 1000)),
            Some(Check::NotOperatorUser)
        );
        for target in ["127.0.0.1:3010", "localhost:3010", "[::1]:3010", "garbage"] {
            let fwd = Facts {
                tcp_forwards: vec![target.into()],
                ..good()
            };
            assert_eq!(
                check(decide(&fwd, None, 3010, 0, 0, 1000)),
                Some(Check::NoTcpForwarder),
                "{target}"
            );
        }
        assert_eq!(
            check(decide(&good(), None, 3010, 0, 1000, 1000)),
            Some(Check::SocketOwner)
        );
        assert_eq!(
            check(decide(&good(), None, 3010, 0, 1001, 1000)),
            Some(Check::SocketOwner)
        );
        assert_eq!(
            check(decide(&good(), None, 3010, 1000, 1000, 1000)),
            Some(Check::ForeignUid)
        );
    }

    /// A latch never read at startup is latched; a sighting of the
    /// board's uid as operator, or an unresolvable operator, latches for
    /// good — a later "no operator" read never clears it.
    #[test]
    fn the_operator_latch_only_ever_closes() {
        assert!(OperatorLatch::default().reason().is_some());
        let open = OperatorLatch(Arc::new(Mutex::new(None)));
        open.observe(&Ok(None), 1000);
        open.observe(&Ok(Some(1001)), 1000);
        assert_eq!(open.reason(), None);
        open.observe(&Ok(Some(1000)), 1000);
        open.observe(&Ok(None), 1000);
        assert!(open
            .reason()
            .is_some_and(|r| r.contains("was tailscaled's operator")));
        let unresolved = OperatorLatch(Arc::new(Mutex::new(None)));
        unresolved.observe(&Err("no such user".into()), 1000);
        assert!(unresolved.reason().is_some());
        let dir = tempfile::tempdir().unwrap();
        assert!(
            OperatorLatch::at_startup(Some(&dir.path().join("none.sock")))
                .reason()
                .is_some()
        );
        assert_eq!(
            check(decide(&good(), Some("latched".into()), 3010, 0, 0, 1000)),
            Some(Check::OperatorLatched)
        );
    }

    #[test]
    fn operator_user_names_resolve_to_uids() {
        assert_eq!(uid_of("root"), Ok(0));
        assert!(uid_of("no-such-user-cad336").is_err());
        assert!(uid_of("bad\0name").is_err());
    }

    /// CAD-509: tailscaled omits an empty `OperatorUser` (`omitempty`)
    /// — absent or null reads as "no operator user", the same as an
    /// empty string. A non-object prefs body is a read error, and a
    /// present non-string still fails closed: neither can widen into
    /// "no operator".
    #[test]
    fn a_missing_operator_user_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let sock = localapi(dir.path());
        let prefs = |v: Value| localapi_says(dir.path(), "prefs.json", v);
        prefs(json!({"WantRunning": true}));
        assert_eq!(operator_uid(&sock), Ok(Ok(None)));
        prefs(json!({"OperatorUser": null, "WantRunning": true}));
        assert_eq!(operator_uid(&sock), Ok(Ok(None)));
        prefs(json!({"OperatorUser": "", "WantRunning": true}));
        assert_eq!(operator_uid(&sock), Ok(Ok(None)));
        prefs(json!({"OperatorUser": "root"}));
        assert_eq!(operator_uid(&sock), Ok(Ok(Some(0))));
        prefs(json!({"OperatorUser": "no-such-user-cad336"}));
        assert!(matches!(operator_uid(&sock), Ok(Err(_))));
        prefs(json!({"OperatorUser": 0}));
        assert!(matches!(operator_uid(&sock), Ok(Err(_))));
        prefs(json!(["OperatorUser"]));
        assert!(operator_uid(&sock).is_err());
    }

    /// `host_refusals` (CAD-509): the up-front pass lists every
    /// host-side rung in proof order — an unreadable rung ends it —
    /// and a missing `OperatorUser` no longer appears in it. A fixture
    /// socket is owned by this test's uid, so `foreign_uid` is
    /// expected whenever `own_uid` is the test's own.
    #[test]
    fn host_refusals_list_the_whole_chain_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let own = unsafe { libc::geteuid() };
        let not_me = own ^ 1;
        let names = |rs: Vec<Refusal>| rs.iter().map(|r| r.check).collect::<Vec<_>>();

        // Nothing below an unreadable rung can be read.
        let missing = dir.path().join("none.sock");
        assert_eq!(
            names(host_refusals(Some(&missing), not_me, 3010)),
            vec![Check::TailscaledSocket]
        );
        let sock = localapi(dir.path());
        std::fs::remove_file(dir.path().join("prefs.json")).ok();
        localapi_says(dir.path(), "status.json", json!({"TUN": true}));
        localapi_says(dir.path(), "serve.json", json!({}));
        assert_eq!(
            names(host_refusals(Some(&sock), not_me, 3010)),
            vec![Check::Localapi]
        );

        // Clean: TUN, prefs with no OperatorUser at all, no forwarder,
        // a board uid that is neither the operator's nor tailscaled's.
        localapi_says(dir.path(), "prefs.json", json!({"WantRunning": true}));
        assert_eq!(names(host_refusals(Some(&sock), not_me, 3010)), vec![]);
        // ... while the same socket seen as the fixture's owner uid
        // fails foreign_uid alone.
        assert_eq!(
            names(host_refusals(Some(&sock), own, 3010)),
            vec![Check::ForeignUid]
        );

        // Every failing rung at once, in proof order: userspace
        // networking, the board's uid as operator, a forwarder — and
        // foreign_uid when the board uid is the fixture's owner.
        localapi_says(dir.path(), "status.json", json!({"TUN": false}));
        localapi_says(dir.path(), "prefs.json", json!({"OperatorUser": "root"}));
        localapi_says(
            dir.path(),
            "serve.json",
            json!({"TCP": {"443": {"TCPForward": "127.0.0.1:3010"}}}),
        );
        assert_eq!(
            names(host_refusals(Some(&sock), 0, 3010)),
            if own == 0 {
                vec![
                    Check::KernelNetworking,
                    Check::NotOperatorUser,
                    Check::NoTcpForwarder,
                    Check::ForeignUid,
                ]
            } else {
                vec![
                    Check::KernelNetworking,
                    Check::NotOperatorUser,
                    Check::NoTcpForwarder,
                ]
            }
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
        assert_eq!(
            check(prove(None, &OperatorLatch::default(), port, remote)),
            Some(Check::Loopback)
        );

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.sock");
        assert_eq!(
            check(prove(Some(&missing), &OperatorLatch::default(), port, peer)),
            Some(Check::TailscaledSocket)
        );
        let file = dir.path().join("file.sock");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(
            check(prove(Some(&file), &OperatorLatch::default(), port, peer)),
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
        assert_eq!(
            check(prove(Some(&sock), &OperatorLatch::default(), port, peer)),
            Some(Check::Localapi)
        );
    }
}
