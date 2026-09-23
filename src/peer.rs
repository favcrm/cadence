//! Connection-bound caller identity — the one rule the daemon's Unix
//! socket and the board's TCP writes share (CAD-102, CAD-254, CAD-263).
//!
//! A peer *process* is tied to a registered pane by three signals
//! ([`PeerTies`]). Two are *process* signals a client cannot choose:
//! the pane's pid on its `/proc` ancestry, and the pane's pty among its
//! stdio fds (a `setsid` detach keeps stdio). The third — the pane's
//! `CADENCE_ALIAS` in its environment (a detach keeps that too) — IS
//! caller-choosable: any process can export `CADENCE_ALIAS=B`.
//!
//! - `agent answer` uses all three ([`PeerTies::tied_to`],
//!   [`PeerTies::agents`]): a caller tied to the target pane by any of
//!   them is refused, and the audit `by` names the first other pane it
//!   is tied to. The alias can only narrow there — forging it refuses
//!   its forger or mislabels an audit stamp; it authorizes nothing.
//! - A board write is attributed to an agent on a process signal only
//!   ([`PeerTies::attributed_agents`]): an env alias alone attributes
//!   nothing, so `CADENCE_ALIAS=B curl …` from a pane-less process
//!   writes as `operator (ui)`, not as agent B.
//!
//! The daemon names its Unix peer through `SO_PEERCRED`; a TCP
//! connection carries no credentials, so the board recovers the peer
//! the way `ss -p` does: the connection's client-side socket in
//! `/proc/net/tcp{,6}` gives its inode, and whichever process holds
//! `socket:[inode]` in `/proc/<pid>/fd` is the peer.
//!
//! Only processes this user can inspect are visible: a socket held by
//! another user's process (a root proxy) cannot be attributed, and the
//! caller decides what that means — the board refuses the write.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::adapter::pty::caller_chain;
use crate::error::Error;

/// What ties one peer process to registered panes — the three signals
/// of the caller rule, read once per peer. `chain` is `None` when any
/// `/proc` read failed mid-walk: an incomplete chain proves neither
/// membership nor its absence, so callers must treat it as
/// unverifiable rather than outside.
pub(crate) struct PeerTies {
    pid: u32,
    chain: Option<Vec<u32>>,
    env_alias: Option<String>,
}

impl PeerTies {
    /// Read the peer's ancestry and `CADENCE_ALIAS` now.
    pub(crate) fn probe(pid: u32) -> Self {
        Self {
            pid,
            chain: caller_chain(pid),
            env_alias: caller_env_alias(pid),
        }
    }

    /// Whether the whole `/proc` ancestry could be walked.
    pub(crate) fn walked(&self) -> bool {
        self.chain.is_some()
    }

    /// The process signals: the pane pid is on the peer's ancestry, or
    /// the peer holds the pane's pty. Neither is caller-choosable.
    fn process_tied(&self, pane_pid: u32) -> bool {
        self.chain.as_deref().is_some_and(|c| c.contains(&pane_pid))
            || holds_pane_tty(self.pid, pane_pid)
    }

    /// The widest tie — a process signal OR the pane's `CADENCE_ALIAS`
    /// in the peer's env. For narrowing authority only (a pane may not
    /// answer its own menu): a forged alias can only refuse its forger.
    pub(crate) fn tied_to(&self, alias: &str, pane_pid: u32) -> bool {
        self.process_tied(pane_pid) || self.env_alias.as_deref() == Some(alias)
    }

    /// The registered panes (`(alias, pane_pid)`) whose agent the peer
    /// may be ATTRIBUTED as — process signals only, sorted by alias
    /// and deduplicated. `CADENCE_ALIAS` is caller-chosen, so an env
    /// tie never attributes by itself; when a process signal
    /// corroborates it, the process signal already decides.
    pub(crate) fn attributed_agents<'a>(
        &self,
        panes: impl IntoIterator<Item = (&'a str, u32)>,
    ) -> Vec<String> {
        let tied: BTreeSet<&str> = panes
            .into_iter()
            .filter(|(_, pane_pid)| self.process_tied(*pane_pid))
            .map(|(alias, _)| alias)
            .collect();
        tied.into_iter().map(str::to_string).collect()
    }

    /// Every registered pane (`(alias, pane_pid)`) the peer is tied to,
    /// sorted by alias and deduplicated — deterministic, never map
    /// order.
    pub(crate) fn agents<'a>(
        &self,
        panes: impl IntoIterator<Item = (&'a str, u32)>,
    ) -> Vec<String> {
        let tied: BTreeSet<&str> = panes
            .into_iter()
            .filter(|(alias, pane_pid)| self.tied_to(alias, *pane_pid))
            .map(|(alias, _)| alias)
            .collect();
        tied.into_iter().map(str::to_string).collect()
    }

    /// Does the peer hold a pty at all? Meaningful only after every
    /// pane membership check failed, so any pts fd is foreign by
    /// definition — positive evidence of an interactive terminal. A
    /// detached caller (`setsid … </dev/null >&2`) holds none.
    pub(crate) fn on_tty(&self) -> bool {
        (0..=2).any(|fd| {
            std::fs::read_link(format!("/proc/{}/fd/{fd}", self.pid))
                .is_ok_and(|p| p.to_string_lossy().starts_with("/dev/pts/"))
        })
    }
}

/// The `CADENCE_ALIAS` the peer carries — pane env survives `setsid`,
/// so a detached pane process still names its agent. An alias this
/// daemon never registered means nothing (a stale or foreign daemon's
/// env) — only registered panes match.
fn caller_env_alias(peer_pid: u32) -> Option<String> {
    let env = std::fs::read(format!("/proc/{peer_pid}/environ")).ok()?;
    env.split(|b| *b == 0)
        .filter_map(|kv| std::str::from_utf8(kv).ok())
        .find_map(|kv| kv.strip_prefix("CADENCE_ALIAS="))
        .filter(|a| !a.is_empty())
        .map(str::to_string)
}

/// Does `peer_pid` hold the pane's pty? A detached pane process loses
/// its ancestry and controlling terminal but keeps stdio — the fd
/// targets still name the pane's pts device. Redirected stdio is the
/// documented residual (a maximal-effort detach), accepted because the
/// same actor could `tmux send-keys` its own pane directly.
fn holds_pane_tty(peer_pid: u32, pane_pid: u32) -> bool {
    let pane_tty = (0..=2)
        .filter_map(|fd| std::fs::read_link(format!("/proc/{pane_pid}/fd/{fd}")).ok())
        .find(|p| p.to_string_lossy().starts_with("/dev/pts/"));
    let Some(tty) = pane_tty else {
        return false;
    };
    (0..=2)
        .any(|fd| std::fs::read_link(format!("/proc/{peer_pid}/fd/{fd}")).is_ok_and(|p| p == tty))
}

/// The caller matched no pane — place it honestly for a pane-attention
/// verb. A broken `/proc` walk (`walked == false`) proves neither
/// membership nor its absence: a refusal while the target pane is
/// alive, `unknown` once it is gone. A clean walk with no match is
/// `operator` only with positive terminal evidence (`foreign_tty`); a
/// detached caller is `unknown`, never `operator`.
pub(crate) fn unmatched_caller(
    walked: bool,
    target_alive: bool,
    foreign_tty: bool,
    verb: &str,
) -> crate::Result<(String, &'static str)> {
    if !walked {
        if target_alive {
            return Err(Error::rejected(format!(
                "cannot derive the caller for `{verb}` — /proc could \
                 not be walked while the target pane is alive; run it \
                 from a shell attached to a pane or outside all panes",
            )));
        }
        return Ok(("unknown".to_string(), "unknown"));
    }
    if foreign_tty {
        Ok(("operator".to_string(), "operator"))
    } else {
        Ok(("unknown".to_string(), "unknown"))
    }
}

/// Which registered agent the TCP peer `peer` of a connection to our
/// `server_port` is attributed as — [`PeerTies::attributed_agents`]:
/// the pane on its ancestry or whose pty it holds; `panes` maps pane
/// pid to alias. `Ok(None)` — the peer is a local process tied to no pane,
/// or (non-loopback address, no local socket holds the connection's
/// other end) a different host, which no pane here can be. `Err` — the
/// peer could not be attributed at all (unreadable ancestry, several
/// agents): callers must fail closed, never read it as "no pane".
pub(crate) fn tcp_peer_pane(
    server_port: u16,
    peer: SocketAddr,
    panes: &HashMap<u32, String>,
) -> Result<Option<String>, String> {
    let peer = canonical(peer);
    let Some(inode) = client_socket_inode(server_port, peer)? else {
        if !peer.ip().is_loopback() {
            return Ok(None);
        }
        return Err(format!(
            "no local socket is the client end of {peer} → port {server_port}"
        ));
    };
    let pids = socket_owners(inode);
    if pids.is_empty() {
        return Err(format!(
            "socket {inode} of peer {peer} has no visible owner — another \
             user's process holds it, or it already closed"
        ));
    }
    let mut agents = BTreeSet::new();
    for pid in pids {
        let ties = PeerTies::probe(pid);
        if !ties.walked() {
            return Err(format!("peer pid {pid}: /proc ancestry unreadable"));
        }
        agents.extend(
            ties.attributed_agents(panes.iter().map(|(pid, alias)| (alias.as_str(), *pid))),
        );
    }
    if agents.len() > 1 {
        return Err(format!(
            "peer {peer} is tied to several panes ({})",
            agents.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(agents.into_iter().next())
}

/// A v4-mapped v6 address (`::ffff:127.0.0.1`, what a dual-stack
/// listener reports) compares as the v4 address the kernel lists.
fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(IpAddr::V4(v4), addr.port()),
            None => addr,
        },
        IpAddr::V4(_) => addr,
    }
}

/// The inode of the socket whose local end is `peer` and whose remote
/// end is our `server_port` — the client's half of the connection, not
/// the accepted half this server holds. `Ok(None)` when no table lists
/// it; `Err` only when neither table could be read.
fn client_socket_inode(server_port: u16, peer: SocketAddr) -> Result<Option<u64>, String> {
    let mut read_any = false;
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        read_any = true;
        if let Some(inode) = find_client_inode(&text, server_port, peer) {
            return Ok(Some(inode));
        }
    }
    if !read_any {
        return Err("/proc/net/tcp and /proc/net/tcp6 are unreadable".to_string());
    }
    Ok(None)
}

/// Scan one `/proc/net/tcp{,6}` table: `sl local rem st … uid timeout
/// inode`. Inode 0 (a TIME_WAIT remnant) is owned by nobody — skipped.
fn find_client_inode(table: &str, server_port: u16, peer: SocketAddr) -> Option<u64> {
    table.lines().skip(1).find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let local = canonical(parse_addr(cols.get(1)?)?);
        let remote = canonical(parse_addr(cols.get(2)?)?);
        let inode: u64 = cols.get(9)?.parse().ok()?;
        (local == peer && remote.port() == server_port && inode != 0).then_some(inode)
    })
}

/// `0100007F:1F90` / 32-hex-digit v6 + `:PORT`. The kernel prints each
/// 32-bit address word in host byte order (`%08X` of the raw word), so
/// `to_ne_bytes` recovers the network-order bytes on any endianness;
/// the port is printed as a plain host-order number.
fn parse_addr(field: &str) -> Option<SocketAddr> {
    let (ip, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let word = |i: usize| -> Option<[u8; 4]> {
        let hex = ip.get(i * 8..i * 8 + 8)?;
        Some(u32::from_str_radix(hex, 16).ok()?.to_ne_bytes())
    };
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(word(0)?)),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Every visible process holding `socket:[inode]` — more than one when
/// the socket was inherited across `fork`. Processes whose `fd` table
/// this user cannot read are skipped, never guessed.
fn socket_owners(inode: u64) -> Vec<u32> {
    let want = format!("socket:[{inode}]");
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut owners = Vec::new();
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        let holds = fds.flatten().any(|fd| {
            std::fs::read_link(fd.path()).is_ok_and(|target| target.as_os_str() == want.as_str())
        });
        if holds {
            owners.push(pid);
        }
    }
    owners
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn parses_proc_net_addresses() {
        let v4 = u32::from_ne_bytes([127, 0, 0, 1]);
        let field = format!("{v4:08X}:1F90");
        assert_eq!(parse_addr(&field), Some("127.0.0.1:8080".parse().unwrap()));
        let mut v6 = String::new();
        let bytes = Ipv6Addr::LOCALHOST.octets();
        for chunk in bytes.chunks(4) {
            let word = u32::from_ne_bytes(chunk.try_into().unwrap());
            v6.push_str(&format!("{word:08X}"));
        }
        assert_eq!(
            parse_addr(&format!("{v6}:0050")),
            Some("[::1]:80".parse().unwrap())
        );
        assert_eq!(parse_addr("zz:0050"), None);
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:5".parse().unwrap();
        assert_eq!(canonical(mapped), "127.0.0.1:5".parse().unwrap());
    }

    /// A live loopback connection resolves to this very process, and
    /// the pane map decides: our pid planted → that alias; no pane on
    /// our lineage → `None`.
    #[test]
    fn a_loopback_peer_resolves_to_its_process_and_pane() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        assert_eq!(peer, client.local_addr().unwrap());

        let none = HashMap::new();
        assert_eq!(tcp_peer_pane(port, peer, &none), Ok(None));
        let panes = HashMap::from([(std::process::id(), "w1".to_string())]);
        assert_eq!(tcp_peer_pane(port, peer, &panes), Ok(Some("w1".into())));

        // A loopback address no local socket holds is not "no pane" —
        // it is unattributable, and the caller must refuse.
        let ghost: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(tcp_peer_pane(port, ghost, &panes).is_err());
        // A non-loopback address with no local client end is a
        // different host: no pane here can be it.
        let remote: SocketAddr = "192.0.2.7:40000".parse().unwrap();
        assert_eq!(tcp_peer_pane(port, remote, &panes), Ok(None));
    }
}
