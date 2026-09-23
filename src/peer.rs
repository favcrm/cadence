//! Connection-bound caller identity for TCP peers (CAD-254).
//!
//! The daemon's Unix socket names its peer through `SO_PEERCRED`; a TCP
//! connection carries no credentials. The board recovers the peer the
//! way `ss -p` does: the connection's client-side socket in
//! `/proc/net/tcp{,6}` gives its inode, and whichever process holds
//! `socket:[inode]` in `/proc/<pid>/fd` is the peer. From there the
//! daemon's own derivation applies — `/proc` ancestry to the nearest
//! registered pane ([`crate::adapter::pty::nearest_pane`]).
//!
//! Only processes this user can inspect are visible: a socket held by
//! another user's process (a root proxy) cannot be attributed, and the
//! caller decides what that means — the board refuses the write.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::adapter::pty::{caller_chain, nearest_pane};

/// Which registered pane the TCP peer `peer` of a connection to our
/// `server_port` descends from. `Ok(None)` — the peer is a local
/// process on no pane's lineage, or (non-loopback address, no local
/// socket holds the connection's other end) a different host, which
/// no pane here can be. `Err` — the peer could not be attributed at
/// all: callers must fail closed, never read it as "no pane".
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
        let chain = caller_chain(pid)
            .ok_or_else(|| format!("peer pid {pid}: /proc ancestry unreadable"))?;
        if let Some(alias) = nearest_pane(&chain, panes) {
            agents.insert(alias.clone());
        }
    }
    if agents.len() > 1 {
        return Err(format!(
            "peer {peer} is held by processes of several panes ({})",
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
