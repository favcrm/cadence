//! CAD-1012 — the protected managed-Pi launch seam (verify-only first batch).
//!
//! This module implements the *construction and refusal* half of a split
//! (`agent_uid.is_some()`) managed-Pi launch: the verified FD-exec spawn plan,
//! the openat2 verify-only walk of the pre-provisioned protected topology, and
//! the post-drop guest environment vector. **It enables nothing on its own** —
//! [`protected_prereqs_satisfied`] returns `Err(UNKNOWN)` until the external
//! restore-lineage, sealed-helper and namespace-policy pins exist, so a
//! production launch can never pass through here yet.
//!
//! Custody model (r5 contract, root direction): the helper ELF and the Node
//! interpreter are the two *exec'd* binaries and are bound **by fd** —
//! `execveat(fd, "", AT_EMPTY_PATH)` runs the verified inode, so a path swap
//! after verification cannot substitute bytes. The Pi module graph
//! (`cli.js` → `cli-runtime.js` → `chunks/*` → `node_modules/`) is a *tree*
//! `node` reads by canonical path; it is verified as an immutable root-owned
//! non-writable tree, never fd-pinned (a tree cannot ride one fd), and never
//! executed through `/proc/self/fd` (which breaks `import.meta.url` sibling
//! resolution). The *actual* denial of out-of-tree reads is the external
//! protected FS policy — not claimed here.
//!
//! No guest caller boolean, restored marker, refreshed local flag, or `params`
//! field is ever an eligibility input: the only inputs are `agent_uid`, the
//! verified topology, and the source-owned [`EXEC_PINS`].
//!
//! First-batch note: the launch path is gated by `protected_prereqs_satisfied`
//! which is always `Err` until external authority exists, so `establish` and
//! the spawn plan are exercised only by this module's tests. The items are
//! real, not dead — the lint is held pending the enablement that the external
//! prerequisites gate.

#![allow(dead_code)]

use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use super::ProviderEnv;
use crate::error::{Error, Result};
use crate::store::Agent;

mod envp;
pub(crate) mod execfd;
pub(crate) mod topology;

pub(crate) use envp::guest_envp;
use execfd::{PreparedExec, EXEC_PINS};
use topology::ProtectedTopology;

/// The four image-local accounts/groups the protected layout is built on —
/// resolved by NSS name, never hardcoded to a uid and never taken from a
/// caller. `cadence-supervisor` owns the private/protected tree; the guest
/// runs as `cadence-agent`.
pub(crate) mod acct {
    pub const SUPERVISOR: &str = "cadence-supervisor";
    pub const GUEST: &str = "cadence-agent";
    pub const LAUNCH_GROUP: &str = "cadence-launch";
    pub const SHARED_GROUP: &str = "cadence";
}

/// A typed launch-profile segment. Alias and generation reach the helper only
/// as fixed-charset fixed-length tokens — `alias` as its lowercase-hex sha256
/// (64 chars), `generation` as 32 lowercase-hex chars — never as path text or
/// free strings, so a `..`, `/`, or over-length value cannot form a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segments {
    pub alias_sha256: [u8; 32],
    pub generation: [u8; 16],
}

/// Reject anything that is not a fixed-charset fixed-length token. The helper
/// only ever sees the *hex* form; this validates that form.
fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl Segments {
    /// Mint the profile segments for one open. `alias` is hashed (raw alias
    /// text never becomes a path segment); `generation` is the caller-minted
    /// 32-hex open generation. Both are checked before they may name a dir.
    pub(crate) fn new(alias: &str, generation: &str) -> Result<Self> {
        use sha2::{Digest, Sha256};
        let alias_sha256: [u8; 32] = Sha256::digest(alias.as_bytes()).into();
        let generation = hex_decode_16(generation).ok_or_else(|| {
            Error::rejected(format!(
                "generation '{generation}' is not 32 lowercase-hex chars"
            ))
        })?;
        Ok(Self {
            alias_sha256,
            generation,
        })
    }

    pub(crate) fn alias_hex(&self) -> String {
        hex_encode(&self.alias_sha256)
    }
    pub(crate) fn generation_hex(&self) -> String {
        hex_encode(&self.generation)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode_16(s: &str) -> Option<[u8; 16]> {
    if !is_lower_hex(s, 32) {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Which slot a launch occupies — the durable per-alias layer (session,
/// history) or the per-generation scratch layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layer {
    /// `…/<alias-sha256>/durable` — persists across every generation.
    Durable,
    /// `…/<alias-sha256>/<generation-32hex>` — recreated per open.
    Generation,
}

/// Whether the agent is the master or a worker — only the leaf set the guest
/// needs differs; the protected skeleton is identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Master,
    Worker,
}

impl Role {
    fn of(agent: &Agent) -> Self {
        if crate::master::is_master(&agent.alias) {
            Role::Master
        } else {
            Role::Worker
        }
    }
}

/// A fully-verified protected launch context. Constructed only after every
/// prerequisite holds; it is *evidence*, not an authority a caller can mint.
pub(crate) struct GuestCtx {
    topo: ProtectedTopology,
    segs: Segments,
    role: Role,
    /// The agent's routing alias — carried verbatim to `CADENCE_ALIAS` (it is
    /// routing text, never a path segment or a principal proof).
    alias: String,
    /// Bound exec fds: the setuid helper and the node ELF.
    helper: execfd::BoundExec,
    node: execfd::BoundExec,
}

/// The mandatory fail-closed gate for production eligibility. The external
/// prerequisite factory — sealed-helper pin record, namespace policy and the
/// pre-start restore-lineage pin — is external authority this source does not
/// mint. Until it is supplied and verified, this is `Err`, permanently UNKNOWN:
/// no guest boolean, restored marker or refreshed flag can satisfy it.
///
/// This batch ships the constructor and refusal path only; the real factory
/// arrives with the external provisioning work.
pub(crate) fn protected_prereqs_satisfied() -> Result<()> {
    Err(Error::rejected(
        "protected managed-Pi prerequisites unavailable — external \
         sealed-helper pin, namespace policy and pre-start restore lineage \
         are not present; launch eligibility is UNKNOWN and stays refused",
    ))
}

impl GuestCtx {
    /// Verify the whole protected launch context for `agent`'s open. Fails
    /// closed on any missing pre-requisite, any mis-owned or symlinked
    /// topology component, or any exec digest that does not match the
    /// compiled pin. Returns the context only when construction — not
    /// enablement — is sound. `protected_prereqs_satisfied` is consulted
    /// by the caller before any spawn; it stays `Err` this batch.
    pub(crate) fn establish(
        agent: &Agent,
        _env: &ProviderEnv,
        agent_uid: u32,
        generation: &str,
    ) -> Result<Self> {
        // The split gate already proved agent_uid.is_some() upstream; assert
        // the resolved uid is the guest account, never 0 or the supervisor.
        let segs = Segments::new(&agent.alias, generation)?;
        let topo = ProtectedTopology::verify(agent_uid)?;
        let role = Role::of(agent);
        // Verify the durable + generation slot dirs exist and are exactly the
        // pre-provisioned shape; a missing slot refuses (never creates).
        topo.verify_view(&segs, role)?;
        // Bind the two exec'd binaries by fd against the compiled pins.
        let helper = execfd::open_bound(&EXEC_PINS[0])?;
        let node = execfd::open_bound(&EXEC_PINS[1])?;
        Ok(Self {
            topo,
            segs,
            role,
            alias: agent.alias.clone(),
            helper,
            node,
        })
    }

    /// The verified helper fd for the `pre_exec` `execveat` — the daemon
    /// executes this inode (kernel applies setuid to it), never a path.
    pub(crate) fn helper_fd(&self) -> std::os::unix::io::RawFd {
        self.helper.fd.as_raw_fd()
    }

    /// Build the spawn plan handed to `StdioAdapter::launch`: the materialized
    /// argv/envp for `pre_exec`+`execveat`. The provider argv is appended
    /// verbatim after the fixed helper profile tokens. The plan takes
    /// ownership of the bound helper fd's `OwnedFd` — custody moves with the
    /// spawn, so no closed/reused descriptor can be exec'd in the child.
    ///
    /// This consumes the helper binding: a `GuestCtx` produces at most one
    /// spawn plan, which is correct — one open, one launch, one fd.
    pub(crate) fn spawn_plan(mut self, provider_argv: &[String]) -> Result<PreparedExec> {
        // Take the bound helper fd out of the ctx so the plan owns it.
        let helper = std::mem::replace(
            &mut self.helper,
            execfd::BoundExec {
                fd: std::fs::File::open("/dev/null")
                    .map_err(|e| Error::internal(format!("sentinel fd: {e}")))?
                    .into(),
                digest: [0u8; 32],
                canon: PathBuf::new(),
            },
        );
        Ok(
            PreparedExec::assemble(&self.segs, provider_argv, guest_envp(&self))?
                .with_bound(helper),
        )
    }

    /// The verified per-launch view dirfd for `layer`.
    pub(crate) fn view(&self, layer: Layer) -> Result<std::os::unix::io::OwnedFd> {
        self.topo.view_dir(&self.segs, layer)
    }

    pub(crate) fn role(&self) -> Role {
        self.role
    }
    pub(crate) fn segments(&self) -> &Segments {
        &self.segs
    }
    /// The routing alias for `CADENCE_ALIAS`.
    pub(crate) fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_token_must_be_32_lower_hex() {
        assert!(Segments::new("w1", "0123456789abcdef0123456789abcdef").is_ok());
        for bad in [
            "",
            "abc",
            "0123456789abcdef0123456789abcde",   // 31
            "0123456789abcdef0123456789abcdef0", // 33
            "0123456789ABCDEF0123456789abcdef",  // uppercase
            "../3456789abcdef0123456789abcdef",  // traversal
            "0123456789abcdef0123456789abcdeg",  // not hex
        ] {
            assert!(Segments::new("w1", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn alias_is_hashed_never_a_raw_segment() {
        let s = Segments::new("../evil", "0123456789abcdef0123456789abcdef").unwrap();
        // The alias dir is the sha256 of the alias — traversal text is gone.
        assert_eq!(s.alias_hex().len(), 64);
        assert!(s.alias_hex().bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn protected_prereqs_is_fail_closed_unknown() {
        let e = protected_prereqs_satisfied().unwrap_err();
        assert!(e.to_string().contains("UNKNOWN"), "{e}");
    }
}
