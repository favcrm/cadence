//! Actual fixed root construction and paired operation. No adoption, generic
//! exec, caller-selected command/key, or observed-JSON positive path exists.
use super::super::{files, refused, Deadline, Result};
use super::{channel, child, context, custody, pin, wire, QualifiedBootstrap};
use std::os::fd::AsRawFd;
use std::time::Instant;
struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        context::cleanup();
    }
}
fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| refused())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn nonce() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| refused())?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    Ok(format!(
        "{}-{}-{}-{}-{}",
        hex(&bytes[..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..])
    ))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn run(
    bootstrap: QualifiedBootstrap,
    configure: &[u8],
    mut provider: channel::Duplex,
    deadline: Deadline,
) -> Result<()> {
    // Close every unelected inherited descriptor before opening any custody.
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } != 0 {
        return Err(refused());
    }
    let now = context::runtime_ms()?;
    if now < bootstrap.authenticated_at_ms || now >= bootstrap.expires_at_ms {
        return Err(refused());
    }
    let root = custody::RootCustody::acquire(&bootstrap, deadline)?;
    let a = &bootstrap.manifest.artifacts;
    let installer = child::construct(
        files::HeldArtifact::open(files::Artifact::Client, pin(&a.client)?, deadline)?,
        child::Principal::Installer,
        deadline,
    )?;
    let recipient = child::construct(
        files::HeldArtifact::open(files::Artifact::Recipient, pin(&a.supervisor)?, deadline)?,
        child::Principal::Recipient,
        deadline,
    )?;
    let mut pair = context::Pair {
        installer,
        recipient,
    };
    let installer = wire::Installer {
        pid: pair.installer.pid(),
        starttime: pair.installer.starttime().to_string(),
        uid: 21000,
        gid: 21000,
        client_digest: a.client.clone(),
        carrier_digest: a.carrier.clone(),
        observer_digest: a.observer.clone(),
    };
    let recipient = wire::Recipient {
        pid: pair.recipient.pid(),
        starttime: pair.recipient.starttime().to_string(),
        generation: random_hex()?,
        nonce: nonce()?,
    };
    let binding = wire::binding(&bootstrap, &installer, &recipient)?;
    pair.installer.recheck(deadline)?;
    pair.recipient.recheck(deadline)?;
    root.recheck(deadline)?;
    provider.send(&wire::json(
        &serde_json::json!({"version":1,"type":"constructed","operation":bootstrap.operation,
        "barrierNonce":bootstrap.barrier_nonce,"installer":installer,"recipient":recipient}),
    )?)?;
    let release = provider.receive()?;
    let release: wire::Release = serde_json::from_slice(&release.0).map_err(|_| refused())?;
    let frame = release.frame(&bootstrap)?;
    let now = context::runtime_ms()?;
    if now < bootstrap.authenticated_at_ms || now >= bootstrap.expires_at_ms {
        return Err(refused());
    }
    let keys = crate::daemon::installer_enrollment_wire::keys_from_qualified(&bootstrap)?;
    let grant_keys = bootstrap.grant_public_keys()?;
    let refs: Vec<&[u8]> = grant_keys.iter().map(|k| k.as_slice()).collect();
    crate::daemon::installer_enrolled::verify_constructor_format(
        &frame,
        &keys,
        &refs,
        binding.as_bytes(),
        deadline.0,
    )?;
    pair.installer.recheck(deadline)?;
    pair.recipient.recheck(deadline)?;
    root.recheck(deadline)?;
    let mut installer_channel = channel::Duplex::new(
        pair.installer.owner().as_raw_fd(),
        pair.installer.owner().as_raw_fd(),
        deadline,
    )?;
    let mut recipient_channel = channel::Duplex::new(
        pair.recipient.owner().as_raw_fd(),
        pair.recipient.owner().as_raw_fd(),
        deadline,
    )?;
    let configure = String::from_utf8(configure.to_vec()).map_err(|_| refused())?;
    for (role, ch) in [
        ("installer", &mut installer_channel),
        ("recipient", &mut recipient_channel),
    ] {
        let remaining_ms = deadline
            .0
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining_ms == 0 {
            return Err(refused());
        }
        ch.send(&wire::json(&wire::ChildContext {
            version: 1,
            kind: "child-context".into(),
            configure: configure.clone(),
            role: role.into(),
            installer: installer.clone(),
            recipient: recipient.clone(),
            binding_json: binding.clone(),
            remaining_ms,
        })?)?;
    }
    // Children may initialize their finite verifier/transport, but root keeps
    // the release frame withheld until authenticated PREPARED in the real core.
    pair.installer.resume(deadline)?;
    pair.recipient.resume(deadline)?;
    let _cleanup = Cleanup;
    context::publish(context::Publication {
        bootstrap,
        root,
        pair,
        installer,
        recipient,
        binding,
        provider,
        installer_channel,
        recipient_channel,
        deadline,
    })?;
    crate::daemon::installer_enrolled::consume_constructor_frame(&frame, deadline.0)?;
    context::finish()
}
