//! Owner-authorized protected managed-Pi launch. The fixed private supervisor
//! service provisions one fresh generation before verify-only topology access.
//! Its authenticated image profile binds the helper; the helper independently
//! arms the exact operation after close_fds, verifies Node/full Pi graph and
//! consumes current authority immediately before FD exec. No authority survives
//! in argv/env or arbitrary inherited descriptors. Absent service/custody/image
//! authorization remains UNKNOWN, not a guest-selected enablement flag.

#![allow(dead_code)]

use super::ProviderEnv;
use crate::error::{Error, Result};
use crate::store::Agent;
use crate::{helper_image_trust, HelperImageTrust};

#[path = "helper_authentication.rs"]
mod authentication;
mod envp;
pub(crate) mod execfd;
pub(crate) mod owner;
pub(crate) mod service;
pub(crate) mod topology;

use crate::protected_pi_profile::authority;
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
    selection: authority::Selection,
    /// SAME authenticated supervisor control channel; actual root creates and
    /// retains the helper. Daemon receives stdio only, never executable custody.
    channel: authority::Channel,
    launch: authority::Authorized,
    proof: authentication::HelperAuthorization,
}

/// Legacy unbound/unsupported-host probe. No operation/alias/model means no
/// authority can be returned. Linux production uses authenticated provisioning
/// in `establish`, never this context-free gate.
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
    /// selector parsing — is sound. The owner provisions a fresh generation
    /// only after authenticating supervisor custody and current launch policy.
    pub(crate) fn establish(
        agent: &Agent,
        _env: &ProviderEnv,
        agent_uid: u32,
        generation: &str,
    ) -> Result<Self> {
        let segs = Segments::new(&agent.alias, generation)?;
        let role = Role::of(agent);
        let model = agent
            .params
            .as_ref()
            .and_then(|p| p.get("model"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::rejected("protected Pi requires explicit model"))?;
        let selection = authority::Selection {
            alias_sha256: segs.alias_hex(),
            generation: segs.generation_hex(),
            role: if role == Role::Master {
                authority::Role::Master
            } else {
                authority::Role::Worker
            },
            model: model.to_owned(),
        };
        selection.validate()?;
        if agent_uid != authority::GUEST_UID
            || unsafe { libc::geteuid() } != authority::SUPERVISOR_UID
        {
            return Err(Error::rejected(
                "protected Pi must be provisioned by the fixed supervisor",
            ));
        }
        // Provision is authenticated by the service against retained supervisor
        // custody and current owner policy before it creates ANY generation leaf.
        let qualified = authentication::QualifiedImage::load()?;
        let mut channel = authority::Channel::connect()?;
        let (launch, signed) = channel.authorize_signed(
            &authority::Request::Provision {
                version: 1,
                selection: selection.clone(),
                alias: agent.alias.clone(),
            },
            &selection,
        )?;
        let proof = qualified.authenticate(&launch, &signed)?;
        channel.bound_until(proof.deadline())?;
        proof.recheck()?;
        let topo = ProtectedTopology::verify(agent_uid)?;
        // Verify the durable + generation slot dirs exist and are exactly the
        // pre-provisioned shape; a missing slot refuses (never creates).
        topo.verify_view(&segs, role)?;
        proof.recheck()?;
        // No local setuid exec: NNP/NOSUID cannot regain root. The constructor
        // selects the measured helper for this retained owner operation and
        // creates it realUID21000/effective0, retaining actual kernel custody.
        Ok(Self {
            topo,
            segs,
            role,
            alias: agent.alias.clone(),
            selection,
            channel,
            launch,
            proof,
        })
    }

    /// Request the root-created exact helper and receive ONLY its three stdio
    /// pipes plus the retained finite control channel. No PID/FD/executable
    /// adoption, local SUID fallback, helper authority env or inherited FD.
    pub(crate) fn launch(
        self,
        routing: &crate::protected_pi_profile::Routing,
    ) -> Result<authority::RemoteLaunch> {
        // Routing must exactly echo the provisioned selectors. Only the helper's
        // separately authenticated arm/consume may authorize actual Node exec.
        if routing.model() != Some(self.selection.model.as_str())
            || routing.no_session() != (self.selection.role == authority::Role::Master)
        {
            return Err(Error::rejected(
                "protected Pi routing differs from owner provisioning",
            ));
        }
        self.proof.recheck()?;
        self.topo.verify_view(&self.segs, self.role)?;
        let launched = self.channel.launch(&self.launch)?;
        self.proof.recheck()?;
        Ok(launched)
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
