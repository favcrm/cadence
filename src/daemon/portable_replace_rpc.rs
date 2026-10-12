//! CAD-1163: `portable_replace` and `portable_replace_preflight` RPC handlers.
//!
//! `portable_replace` runs [`Shared::portable_operator_connection`] on the
//! real peer PID as its first statement. Only then does it build the request,
//! using the controller's own `state_dir` (never a caller field), and mint the
//! native proof bound to that canonical request. `portable_replace_preflight`
//! is also operator-gated and reports the digests an operator copies; it
//! carries no authority.
//!
//! Design note (CAD-1163 security fix). The shared `operator_connection`
//! admits any same-uid peer whose ancestry is not a registered pane, an
//! enrolled endpoint or a daemon descendant, so an unregistered agent
//! runtime (node, Python, Pi) passes it. The two portable verbs add one
//! strict origin check on top of it, [`direct_terminal_origin`]:
//!
//! - the peer must be the same executable file as this daemon (the native
//!   `cadence` CLI, by dev/inode, never by argv or environment);
//! - every ancestor is walked, with no early success. Each same-uid hop must
//!   be a system-path shell or `sshd-session`, or (intermediary only, never
//!   an anchor) an allowlisted terminal emulator. Each such executable must
//!   be root-owned and not group/world-writable when its metadata is readable;
//! - the anchor is a root `sshd` or `login` above at least one shell. A
//!   generic root PID 1, `systemd`, a user manager or a GUI-only session is
//!   not an anchor and refuses.
//!
//! The exact-command grant-exec channel does not admit the portable verbs:
//! a grant child is a daemon descendant and fails the origin walk. A test-seam
//! assertion decides only in test-seam builds. Currently supported: the
//! operator runs the verb from a direct SSH/login shell. Terminal emulators
//! and daemon-spawned commands are refused.

use super::*;
use crate::backup::portable_replace::{self, NativeOperatorProof, ReplaceRequest};
use std::os::unix::fs::MetadataExt;

/// Shells accepted as same-uid intermediates (system directories only).
const SHELLS: &[&str] = &["bash", "sh", "dash", "zsh", "fish"];
/// Terminal emulators accepted as an intermediary, never as an anchor.
const TERMINALS: &[&str] = &[
    "gnome-terminal-server",
    "gnome-terminal",
    "xterm",
    "kitty",
    "alacritty",
    "konsole",
    "wezterm-gui",
    "wezterm",
    "ghostty",
    "tilix",
    "xfce4-terminal",
    "foot",
];
/// Directories a trusted executable may live in. Agent-writable paths are
/// absent, so a copied binary named `bash` never matches.
const SYSTEM_DIRS: &[&str] = &[
    "/usr/bin/",
    "/bin/",
    "/usr/sbin/",
    "/sbin/",
    "/usr/libexec/",
    "/usr/lib/",
];

/// `Ok` when the executable of `pid` is root-owned and not group/world-
/// writable. Only called on a readable exe: an unreadable exe is handled by
/// the caller (refused for a same-uid hop, allowed only for a root anchor).
fn root_owned_system_exe(pid: u32, meta: &std::fs::Metadata) -> std::result::Result<(), String> {
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "pid {pid}: executable is not root-owned or is group/world-writable"
        ));
    }
    Ok(())
}

/// The kernel `comm` of `pid`, or `None` when unreadable.
fn proc_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|c| c.trim().to_string())
}

/// A root anchor: real and effective uid 0, kernel `comm` exactly `sshd` or
/// `login`. When its exe is readable it must be a root-owned file in a
/// system directory whose file name is that same `comm`. An unreadable exe
/// is accepted here only because the root uid and `comm` are verified.
fn root_anchor(pid: u32) -> std::result::Result<(), String> {
    let (real, eff) = crate::peer::proc_uids(pid)?;
    if real != 0 || eff != 0 {
        return Err(format!(
            "pid {pid}: anchor is not root (real and effective)"
        ));
    }
    let comm = proc_comm(pid).unwrap_or_default();
    if comm != "sshd" && comm != "login" {
        return Err(format!(
            "pid {pid}: anchor comm '{comm}' is not sshd or login"
        ));
    }
    if let Ok(meta) = std::fs::metadata(format!("/proc/{pid}/exe")) {
        let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = std::path::Path::new(&exe)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if !SYSTEM_DIRS.iter().any(|d| exe.starts_with(d)) || file != comm {
            return Err(format!(
                "pid {pid}: anchor executable is not the exact system {comm}"
            ));
        }
        root_owned_system_exe(pid, &meta)?;
    }
    Ok(())
}

/// The peer must be this daemon's own native `cadence` executable (same
/// dev/inode as `/proc/self/exe`). Its ancestry is walked from the peer up
/// with no early success: at least one same-uid shell, any number of
/// same-uid `sshd-session` or shells, same-uid terminal emulators as
/// intermediaries, then a root `sshd` or `login` anchor. Every other hop,
/// a missing anchor, an unreadable link or a `/proc` read error refuses
/// (fail closed).
///
/// Narrow exception (CAD-1163): a same-uid `sshd-session` whose exe is
/// unreadable (non-dumpable, so `/proc/<pid>/exe` is denied) is admitted only
/// when its readable `comm` is exactly `sshd-session` or `sshd`, a shell sits
/// below it, and its IMMEDIATE parent is a verified root anchor. The walk
/// still continues to that anchor. Any other unreadable same-uid hop refuses.
fn direct_terminal_origin(peer_pid: u32) -> std::result::Result<(), String> {
    let own = std::fs::metadata("/proc/self/exe").map_err(|e| format!("own exe: {e}"))?;
    let peer_exe = std::fs::metadata(format!("/proc/{peer_pid}/exe"))
        .map_err(|e| format!("peer exe unreadable: {e}"))?;
    if (peer_exe.dev(), peer_exe.ino()) != (own.dev(), own.ino()) {
        return Err(format!(
            "pid {peer_pid} is not this daemon's cadence executable"
        ));
    }
    let uid = unsafe { libc::geteuid() };
    let chain = crate::adapter::pty::caller_chain(peer_pid)
        .ok_or_else(|| format!("pid {peer_pid}: /proc ancestry unreadable"))?;
    let mut shell_seen = false;
    for (idx, &hop) in chain.iter().enumerate().skip(1) {
        let (real, _) = crate::peer::proc_uids(hop)?;
        let meta = std::fs::metadata(format!("/proc/{hop}/exe")).ok();
        let exe = std::fs::read_link(format!("/proc/{hop}/exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = std::path::Path::new(&exe)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if real == uid {
            let Some(meta) = meta else {
                // Narrow exception: unreadable exe is admitted only for a
                // sshd session directly under a verified root anchor.
                let comm = proc_comm(hop).unwrap_or_default();
                if (comm == "sshd-session" || comm == "sshd") && shell_seen {
                    if let Some(&parent) = chain.get(idx + 1) {
                        let start = crate::peer::proc_starttime(hop);
                        root_anchor(parent)?;
                        if start.is_some() && crate::peer::proc_starttime(hop) == start {
                            continue;
                        }
                        return Err(format!("pid {hop}: session changed during the walk"));
                    }
                }
                return Err(format!(
                    "ancestor pid {hop} ('{name}') has an unreadable executable and is not a sshd session under a root anchor"
                ));
            };
            let system = SYSTEM_DIRS.iter().any(|d| exe.starts_with(d));
            if system {
                root_owned_system_exe(hop, &meta)?;
                if SHELLS.contains(&name.as_str()) {
                    shell_seen = true;
                    continue;
                }
                if name == "sshd-session" && shell_seen {
                    continue;
                }
                if TERMINALS.contains(&name.as_str()) && shell_seen {
                    continue;
                }
            }
        }
        // The anchor: a root sshd or login above a shell.
        if real == 0 && shell_seen {
            root_anchor(hop)?;
            return Ok(());
        }
        return Err(format!(
            "ancestor pid {hop} ('{name}') is not a trusted shell, ssh transport or root login"
        ));
    }
    Err("no trusted root sshd or login on the ancestry".to_string())
}

impl Shared {
    /// The connection gate of the two new verbs (CAD-1163): the shared
    /// operator connection, then the strict origin walk.
    fn portable_operator_connection(
        &self,
        verb: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<()> {
        self.operator_connection(verb, params, peer_pid)?;
        // Test-seam assertions decide in seam builds only; they never reach
        // a default build (`asserted` is always `None` there). The CAD-615
        // grant channel is not an origin here: its child fails the walk.
        if crate::test_seam::asserted().is_some() {
            return Ok(());
        }
        direct_terminal_origin(peer_pid).map_err(|why| {
            Error::rejected(format!(
                "{verb} is an operator action — this connection is not from a direct \
                 operator terminal: {why}; run it directly from the operator terminal \
                 or an approved operator channel"
            ))
        })
    }

    /// `portable_replace` (CAD-1163) — operator only.
    pub(super) fn rpc_portable_replace(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.portable_operator_connection("portable replace", params, peer_pid)?;
        let req = self.replace_request(params)?;
        let proof = NativeOperatorProof::mint(&req);
        portable_replace::replace_authorized(&proof, &req)
    }

    /// `portable_replace_preflight` (CAD-1163) — operator only, read-only.
    pub(super) fn rpc_portable_replace_preflight(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.portable_operator_connection("portable replace preflight", params, peer_pid)?;
        self.controller_state_param(params)?;
        let source = required_str(params, "source")?;
        let destination = required_str(params, "destination")?;
        portable_replace::preflight(
            std::path::Path::new(source),
            std::path::Path::new(destination),
            &self.state_dir,
        )
    }

    /// `portable_activate_preflight` (CAD-1163) — operator only, read-only.
    /// Reports the target's full digest and generation and the exact required
    /// reconciliation inventory the operator must supply. Changes nothing.
    pub(super) fn rpc_portable_activate_preflight(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.portable_operator_connection("portable activate preflight", params, peer_pid)?;
        self.controller_state_param(params)?;
        let destination = std::path::PathBuf::from(required_str(params, "destination")?);
        let snap = crate::backup::portable_replace::snapshot_target(&destination)?;
        let required = crate::backup::portable_activate::required_inventory(&destination)?;
        Ok(json!({
            "execution_mode": "disabled",
            "supported_mode": "read-only (execution disabled); PM writer activation and execution bindings are not supported",
            "destination_digest": snap.digest,
            "destination_generation": snap.generation,
            "bindings": [],
            "required_reconciliation": required.iter().map(|r| json!({
                "kind": r.kind, "id": r.id, "status": r.status, "action": "retain_inert"
            })).collect::<Vec<_>>(),
        }))
    }

    /// `portable_activate` (CAD-1163) — operator only. Activates an inactive
    /// root as execution-disabled. Authorized by the operator connection and
    /// the controller check, never by the replace proof.
    pub(super) fn rpc_portable_activate(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.portable_operator_connection("portable activate", params, peer_pid)?;
        let controller = self.controller_state_param(params)?;
        let digest = |field: &str| -> Result<String> {
            let v = required_str(params, field)?.to_string();
            if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::rejected(format!(
                    "'{field}' must be a 64-character sha256 hex digest"
                )));
            }
            Ok(v)
        };
        let bindings = params
            .get("bindings")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| Error::rejected("'bindings' must be an array"))?;
        let reconciliation = crate::backup::portable_activate::parse_reconciliation(
            params
                .get("reconciliation")
                .ok_or_else(|| Error::rejected("'reconciliation' is required"))?,
        )?;
        let req = crate::backup::portable_activate::ActivateRequest {
            destination: std::path::PathBuf::from(required_str(params, "destination")?),
            controller_state: controller,
            expected_package_digest: digest("expected_package_digest")?,
            expected_destination_digest: digest("expected_destination_digest")?,
            expected_generation: digest("expected_generation")?,
            execution_mode: required_str(params, "execution_mode")?.to_string(),
            bindings,
            reconciliation,
        };
        let source = std::path::PathBuf::from(required_str(params, "source")?);
        crate::backup::portable_activate::activate(&source, &req)
    }

    /// Build the canonical request. The controller path is `self.state_dir`;
    /// a caller-supplied `controller_state` must equal it or the request is
    /// refused, so no alternate controller root can bypass the overlap guard.
    /// The `controller_state` field must name this live controller's state
    /// dir (canonical), or the request is refused. Shared by replace and
    /// preflight so both are equally strict.
    fn controller_state_param(&self, params: &Value) -> Result<PathBuf> {
        let claim = required_str(params, "controller_state")?;
        let controller = self
            .state_dir
            .canonicalize()
            .map_err(|e| Error::rejected(format!("controller state dir is not canonical: {e}")))?;
        if std::path::Path::new(claim).canonicalize().ok().as_deref() != Some(controller.as_path())
        {
            return Err(Error::rejected(
                "controller_state does not name this live controller's state dir; nothing was written",
            ));
        }
        Ok(controller)
    }

    fn replace_request(&self, params: &Value) -> Result<ReplaceRequest> {
        let text =
            |field: &str| -> Result<String> { required_str(params, field).map(str::to_string) };
        let controller = self.controller_state_param(params)?;
        let digest = |field: &str| -> Result<String> {
            let v = text(field)?;
            if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::rejected(format!(
                    "'{field}' must be a 64-character sha256 hex digest"
                )));
            }
            Ok(v)
        };
        Ok(ReplaceRequest {
            source: std::path::PathBuf::from(text("source")?),
            destination: std::path::PathBuf::from(text("destination")?),
            controller_state: controller,
            expected_package_digest: digest("expected_package_digest")?,
            expected_destination_digest: digest("expected_destination_digest")?,
            expected_generation: digest("expected_generation")?,
        })
    }
}
