//! The actual private AgenticOS tracker-v1 protocol. No credential, endpoint
//! election, retries or bootstrap calls. Timers bound native transport, not R2.
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{sha256_hex, Binding, Deadline, Hosted, Receipt, Store, StoreOutcome};
use crate::error::{Error, Result};

const CHUNK: usize = 1048576;
const RESPONSE_CAP: u64 = 131072;
const SAFE_INTEGER: u64 = 9007199254740991;
const BASE: &str = "http://store.internal/company/tracker-v1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Legacy,
    Required,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityBinding {
    pub company: String,
    pub instance: String,
    pub generation: u64,
    pub authority_epoch: u64,
    pub boot_id: String,
}
impl AuthorityBinding {
    fn validate(&self) -> Result<()> {
        if self.company.is_empty()
            || self.instance.is_empty()
            || self.generation == 0
            || self.generation > SAFE_INTEGER
            || self.authority_epoch == 0
            || self.authority_epoch > SAFE_INTEGER
            || !safe_id(&self.boot_id, 80)
        {
            return Err(unavailable("invalid authority binding"));
        }
        Ok(())
    }
    fn origin(&self) -> Binding {
        Binding {
            company: self.company.clone(),
            instance: self.instance.clone(),
            generation: self.generation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostReceipt {
    pub publication_id: String,
    pub sequence: u64,
    pub store_effect_id: String,
    pub binding: AuthorityBinding,
    pub commit: String,
    #[serde(deserialize_with = "nullable")]
    pub origin_lease_epoch: Option<u64>,
    pub artifact_sha256: String,
    pub artifact_bytes: u64,
    pub manifest_sha256: String,
}
impl HostReceipt {
    fn validate(&self) -> Result<()> {
        self.binding.validate()?;
        if !safe_id(&self.publication_id, 200)
            || self.store_effect_id.is_empty()
            || self.store_effect_id.len() > 512
            || self.sequence == 0
            || self.sequence > SAFE_INTEGER
            || !hex(&self.commit, 40)
            || !hex(&self.artifact_sha256, 64)
            || !hex(&self.manifest_sha256, 64)
            || self.artifact_bytes == 0
            || self.artifact_bytes > super::FREEZE_CAP
            || self.origin_lease_epoch.is_some_and(|n| n > SAFE_INTEGER)
        {
            return Err(unavailable("invalid content receipt"));
        }
        Ok(())
    }
    fn matches_origin(
        &self,
        receipt: &Receipt,
        manifest: &str,
        authority: &AuthorityBinding,
    ) -> bool {
        self.binding == *authority
            && authority.origin() == receipt.binding
            && self.commit == receipt.commit
            && self.origin_lease_epoch == receipt.lease_epoch
            && self.artifact_sha256 == receipt.artifact_sha256
            && self.artifact_bytes == receipt.artifact_bytes
            && self.manifest_sha256 == manifest
    }
}

// Struct declaration order is the host's JSON.stringify canonical order.
#[derive(Debug, Serialize)]
pub struct Descriptor {
    version: u8,
    commit: String,
    origin_lease_epoch: Option<u64>,
    artifact_sha256: String,
    artifact_bytes: u64,
    chunks: Vec<Chunk>,
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Chunk {
    ordinal: usize,
    bytes: usize,
    sha256: String,
}
impl Descriptor {
    pub(super) fn from_artifact(receipt: &Receipt, artifact: &[u8]) -> Result<Self> {
        if artifact.is_empty()
            || artifact.len() as u64 > super::FREEZE_CAP
            || receipt.artifact_bytes != artifact.len() as u64
            || receipt.artifact_sha256 != sha256_hex(artifact)
            || !hex(&receipt.commit, 40)
            || receipt.lease_epoch.is_some_and(|n| n > SAFE_INTEGER)
        {
            return Err(unavailable("invalid frozen artifact"));
        }
        Ok(Self {
            version: 1,
            commit: receipt.commit.clone(),
            origin_lease_epoch: receipt.lease_epoch,
            artifact_sha256: receipt.artifact_sha256.clone(),
            artifact_bytes: receipt.artifact_bytes,
            chunks: artifact
                .chunks(CHUNK)
                .enumerate()
                .map(|(ordinal, bytes)| Chunk {
                    ordinal,
                    bytes: bytes.len(),
                    sha256: sha256_hex(bytes),
                })
                .collect(),
        })
    }
    pub(super) fn canonical(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).map_err(|_| unavailable("manifest encoding"))?;
        if bytes.len() > RESPONSE_CAP as usize {
            return Err(unavailable("manifest cap"));
        }
        Ok(bytes)
    }
}

#[derive(Clone, Debug)]
pub struct Reservation {
    pub receipt: HostReceipt,
    pub expires_at: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    #[serde(rename = "artifactBytes")]
    artifact_bytes: u64,
    #[serde(rename = "chunkBytes")]
    chunk_bytes: u64,
    chunks: u64,
    #[serde(rename = "manifestBytes")]
    manifest_bytes: u64,
    #[serde(rename = "ioMs")]
    io_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Boot {
    protocol: String,
    mode: Mode,
    binding: AuthorityBinding,
    #[serde(deserialize_with = "nullable")]
    head: Option<HostReceipt>,
    restore_ready: bool,
    limits: Limits,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    binding: AuthorityBinding,
    #[serde(deserialize_with = "nullable")]
    head: Option<HostReceipt>,
    mode: Mode,
    restore_ready: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReserveReply {
    receipt: HostReceipt,
    expires_at: u64,
}

pub(super) struct HttpStore {
    binding: AuthorityBinding,
    root: PathBuf,
    mode: Mode,
    base: String,
}

/// Independent QA may exercise the REAL adapter against its loopback host
/// fixture. This constructor is absent from ordinary builds and never reads
/// request JSON or environment endpoint overrides.
#[cfg(test)]
pub fn loopback_hosted(
    binding: AuthorityBinding,
    root: PathBuf,
    address: std::net::SocketAddr,
) -> Result<Hosted> {
    binding.validate()?;
    if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        || !(3110..=3199).contains(&address.port())
    {
        return Err(unavailable("fixture requires isolated 127.0.0.1 port"));
    }
    Ok(Hosted {
        binding: binding.origin(),
        store: Arc::new(HttpStore {
            binding,
            root,
            mode: Mode::Required,
            base: format!("http://{address}/company/tracker-v1"),
        }),
        persist_budget: Duration::from_secs(20),
    })
}

// QA wraps the ACTUAL verify_boot_command / signed route on its own thread.
// No environment endpoint override and no production feature flag exists.
#[cfg(test)]
thread_local! {
    static TEST_ENDPOINT: std::cell::RefCell<Option<std::net::SocketAddr>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub fn with_loopback_transport<T>(
    address: std::net::SocketAddr,
    operation: impl FnOnce() -> T,
) -> Result<T> {
    if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        || !(3110..=3199).contains(&address.port())
    {
        return Err(unavailable("fixture requires isolated 127.0.0.1 port"));
    }
    struct Reset(Option<std::net::SocketAddr>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_ENDPOINT.with(|slot| *slot.borrow_mut() = self.0);
        }
    }
    let previous = TEST_ENDPOINT.with(|slot| slot.borrow_mut().replace(address));
    let _reset = Reset(previous);
    Ok(operation())
}
fn private_endpoint() -> String {
    #[cfg(test)]
    if let Some(address) = TEST_ENDPOINT.with(|slot| *slot.borrow()) {
        return format!("http://{address}/company/tracker-v1");
    }
    BASE.to_string()
}

fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

fn unavailable(message: &str) -> Error {
    Error::rejected(format!("capability_unavailable: {message}"))
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn safe_id(s: &str, cap: usize) -> bool {
    !s.is_empty()
        && s.len() <= cap
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Kill/reap only our child on expiry or oversized output. curl also sets real
/// connect/read/write total timeout. No pipe can wedge the watchdog. Output and
/// request spools are owner-only NamedTempFiles, cleaned on all return paths.
pub(super) fn run_bounded(mut command: Command, deadline: &Deadline, cap: u64) -> Result<Vec<u8>> {
    let mut output = tempfile::NamedTempFile::new()?;
    command
        .stdout(Stdio::from(output.reopen()?))
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    if deadline.expired() {
        return Err(unavailable("transport deadline"));
    }
    let mut child = crate::reaper::spawn(&mut command)?;
    let status = loop {
        let size = output.as_file().metadata().map(|m| m.len());
        if deadline.expired() || size.is_err() || size.is_ok_and(|n| n > cap) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable("transport deadline or response cap"));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        }
        std::thread::sleep(Duration::from_millis(2).min(deadline.remaining()));
    };
    if !status.success() || deadline.expired() {
        return Err(unavailable("private transport refused"));
    }
    output.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    output.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap || deadline.expired() {
        return Err(unavailable("response cap or deadline"));
    }
    Ok(bytes)
}
impl HttpStore {
    fn request<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        deadline: &Deadline,
    ) -> Result<T> {
        let mut command = Command::new("/usr/bin/curl");
        command.args([
            "--disable",
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--proto",
            "=http",
            "--max-redirs",
            "0",
            "--request",
            method,
            "--connect-timeout",
            &format!("{:.3}", deadline.remaining().as_secs_f64()),
            "--max-time",
            &format!("{:.3}", deadline.remaining().as_secs_f64()),
        ]);
        for (name, value) in [
            ("x-tracker-boot-id", self.binding.boot_id.clone()),
            ("x-tracker-generation", self.binding.generation.to_string()),
            (
                "x-tracker-authority-epoch",
                self.binding.authority_epoch.to_string(),
            ),
        ] {
            command.args(["--header", &format!("{name}: {value}")]);
        }
        let mut spool = None;
        if let Some(body) = body {
            let mut file = tempfile::NamedTempFile::new()?;
            file.write_all(body)?;
            command.args([
                "--header",
                if method == "PUT" {
                    "Content-Type: application/octet-stream"
                } else {
                    "Content-Type: application/json"
                },
            ]);
            command
                .arg("--data-binary")
                .arg(format!("@{}", file.path().display()));
            spool = Some(file);
        }
        command.args(["--write-out", "\n%{http_code}"]);
        command.arg(format!("{}{path}", self.base));
        let response = run_bounded(command, deadline, RESPONSE_CAP)?;
        drop(spool);
        let split = response
            .iter()
            .rposition(|b| *b == b'\n')
            .ok_or_else(|| unavailable("missing HTTP status"))?;
        let status = std::str::from_utf8(&response[split + 1..])
            .ok()
            .and_then(|s| s.parse::<u16>().ok());
        let value: Value = serde_json::from_slice(&response[..split])
            .map_err(|_| unavailable("invalid private JSON"))?;
        if value.get("ok") == Some(&Value::Bool(false))
            && value.as_object().is_some_and(|o| o.len() == 2)
            && status.is_some_and(|s| (400..600).contains(&s))
        {
            let code = value["code"].as_str().filter(|c| {
                matches!(
                    *c,
                    "invalid_request"
                        | "lease_lost"
                        | "stale_generation"
                        | "conflict"
                        | "superseded"
                        | "not_ready"
                        | "capability_unavailable"
                        | "expired"
                        | "capacity"
                        | "custody_unavailable"
                        | "not_found"
                        | "unknown"
                )
            });
            return Err(Error::rejected(format!(
                "tracker-v1 refusal: {}",
                code.unwrap_or("invalid_response")
            )));
        }
        if !status.is_some_and(|s| (200..300).contains(&s)) {
            return Err(unavailable("private HTTP refusal/redirect"));
        }
        if value.get("ok") != Some(&Value::Bool(true))
            || value.as_object().is_none_or(|o| o.len() != 2)
        {
            return Err(unavailable("private authority refusal"));
        }
        let decoded = serde_json::from_value(value["data"].clone())
            .map_err(|_| unavailable("invalid private schema"))?;
        if deadline.expired() {
            return Err(unavailable("late private response"));
        }
        Ok(decoded)
    }
    fn live(&self, deadline: &Deadline) -> Result<Head> {
        let head: Head = self.request("GET", "/head", None, deadline)?;
        if head.binding != self.binding || head.mode != self.mode || !head.restore_ready {
            return Err(unavailable("live binding/readiness changed"));
        }
        if let Some(receipt) = &head.head {
            receipt.validate()?;
        }
        Ok(head)
    }
}
impl Store for HttpStore {
    fn ordered_protocol(&self) -> bool {
        true
    }
    fn validate_required(&self, binding: &Binding) -> Result<()> {
        self.binding.validate()?;
        if self.binding.origin() != *binding {
            return Err(unavailable("serving binding mismatch"));
        }
        // Per-write probe never bootstraps (bootstrap closes readiness).
        let deadline = Deadline::after(Duration::from_secs(5));
        let head = self.live(&deadline)?;
        restore_profile(&self.root, "HEAD", &deadline)?;
        if let Some(receipt) = head.head {
            preserve_head(&self.root, &receipt.commit, &deadline)?;
        }
        Ok(())
    }
    fn reserve(
        &self,
        receipt: &Receipt,
        descriptor: &Descriptor,
        manifest: &str,
        deadline: &Deadline,
    ) -> Result<Reservation> {
        // Preserve Descriptor property order; do not round-trip it through Value.
        let canonical = descriptor.canonical()?;
        let mut body = b"{\"descriptor\":".to_vec();
        body.extend(&canonical);
        body.extend(
            format!(",\"manifest_sha256\":\"{manifest}\",\"budget_ms\":20000}}").as_bytes(),
        );
        let reply: ReserveReply = self.request("POST", "/reserve", Some(&body), deadline)?;
        reply.receipt.validate()?;
        if !reply
            .receipt
            .matches_origin(receipt, manifest, &self.binding)
            || reply.expires_at > SAFE_INTEGER
            || reply.expires_at <= unix_ms()?
            || deadline.expired()
        {
            return Err(unavailable("reservation proof mismatch/expiry"));
        }
        Ok(Reservation {
            receipt: reply.receipt,
            expires_at: reply.expires_at,
        })
    }
    fn complete_reserved(
        &self,
        receipt: &Receipt,
        artifact: &[u8],
        reservation: &Reservation,
        deadline: &Deadline,
    ) -> Result<HostReceipt> {
        let descriptor = Descriptor::from_artifact(receipt, artifact)?;
        let manifest = sha256_hex(&descriptor.canonical()?);
        reservation.receipt.validate()?;
        if !reservation
            .receipt
            .matches_origin(receipt, &manifest, &self.binding)
            || unix_ms()? >= reservation.expires_at
        {
            return Err(unavailable("held reservation expired/mismatch"));
        }
        for (chunk, bytes) in descriptor.chunks.iter().zip(artifact.chunks(CHUNK)) {
            let response: Chunk = self.request(
                "PUT",
                &format!(
                    "/intents/{}/chunks/{}",
                    reservation.receipt.publication_id, chunk.ordinal
                ),
                Some(bytes),
                deadline,
            )?;
            if response != *chunk {
                return Err(unavailable("chunk receipt mismatch"));
            }
        }
        let completed: HostReceipt = self.request(
            "POST",
            &format!("/intents/{}/complete", reservation.receipt.publication_id),
            Some(b"{}"),
            deadline,
        )?;
        completed.validate()?;
        if completed != reservation.receipt || deadline.expired() {
            return Err(unavailable("completion proof mismatch/late"));
        }
        Ok(completed)
    }
    fn persist(&self, _: &Receipt, _: &[u8], _: &Deadline) -> Result<StoreOutcome> {
        // No token recovery, reserve, or retry after the originating lock drops.
        Err(unavailable("held-lock reservation required"))
    }
}
fn unix_ms() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .map_err(|_| unavailable("system clock"))
}
fn local_head(root: &Path, deadline: &Deadline) -> Result<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(["rev-parse", "HEAD"]);
    let bytes = run_bounded(command, deadline, 128)?;
    let head = String::from_utf8(bytes)
        .map_err(|_| unavailable("invalid local HEAD"))?
        .trim()
        .to_string();
    if !hex(&head, 40) {
        return Err(unavailable("invalid local HEAD"));
    }
    Ok(head)
}
/// Host restore rejects symlinks/submodules/special modes. Its current
/// ls-files index verification has a 128KiB command-output ceiling: this
/// slightly more conservative ls-tree ceiling avoids acknowledging an
/// artifact known not to be restorable under that source profile.
fn split_tree_record(record: &[u8]) -> Result<(&[u8], &[u8])> {
    let separator = record
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or_else(|| unavailable("invalid tracker tree entry"))?;
    Ok((&record[..separator], &record[separator + 1..]))
}
pub(super) fn restore_profile(root: &Path, commit: &str, deadline: &Deadline) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", "-z", "--full-tree", commit]);
    let tree = run_bounded(command, deadline, RESPONSE_CAP)?;
    for record in tree.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let (metadata, path) = split_tree_record(record)?;
        if !(metadata.starts_with(b"100644 blob ") || metadata.starts_with(b"100755 blob "))
            || path.is_empty()
        {
            return Err(unavailable(
                "tracker restore requires regular Git tree modes",
            ));
        }
    }
    Ok(())
}
/// Verify that every tracked blob/mode in the selected commit exists in the
/// installed checkout. File access is descriptor-relative and refuses symlink
/// path components; hashing streams bytes under the boot deadline.
fn verify_checkout(root: &Path, commit: &str, deadline: &Deadline) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", "-z", "--full-tree", commit]);
    let tree = run_bounded(command, deadline, RESPONSE_CAP)?;
    let mut root_open = std::fs::OpenOptions::new();
    root_open
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let root_fd = root_open.open(root)?;
    for record in tree.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if deadline.expired() {
            return Err(unavailable("checkout verification deadline"));
        }
        let (metadata, path) = split_tree_record(record)?;
        let mut columns = metadata.split(|b| *b == b' ');
        let mode = columns.next().unwrap_or_default();
        let kind = columns.next().unwrap_or_default();
        let object = columns.next().unwrap_or_default();
        if kind != b"blob" || !hex(std::str::from_utf8(object).unwrap_or_default(), 40) {
            return Err(unavailable("invalid tracked blob entry"));
        }
        let components: Vec<_> = path.split(|b| *b == b'/').collect();
        if components.is_empty()
            || components
                .iter()
                .any(|part| part.is_empty() || *part == b"." || *part == b".." || part.contains(&0))
        {
            return Err(unavailable("unsafe tracked path"));
        }
        let mut parent = root_fd.try_clone()?;
        for component in &components[..components.len() - 1] {
            let name = std::ffi::CString::new(*component)
                .map_err(|_| unavailable("unsafe tracked path"))?;
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(unavailable("tracked directory missing or unsafe"));
            }
            parent = unsafe { std::fs::File::from_raw_fd(fd) };
        }
        let name = std::ffi::CString::new(*components.last().unwrap())
            .map_err(|_| unavailable("unsafe tracked path"))?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(unavailable("tracked file missing or unsafe"));
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let file_meta = file.metadata()?;
        if !file_meta.is_file()
            || (mode == b"100755") != (file_meta.mode() & 0o111 != 0)
            || (mode != b"100644" && mode != b"100755")
        {
            return Err(unavailable("tracked file mode mismatch"));
        }
        let mut hasher = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
        hasher.update(format!("blob {}\0", file_meta.len()).as_bytes());
        let mut buffer = [0u8; 8192];
        loop {
            if deadline.expired() {
                return Err(unavailable("checkout verification deadline"));
            }
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let actual = hasher.finish();
        let actual = actual
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if actual.as_bytes() != object {
            return Err(unavailable("tracked file content mismatch"));
        }
    }
    Ok(())
}
fn preserve_head(root: &Path, commit: &str, deadline: &Deadline) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--is-ancestor", commit, "HEAD"]);
    run_bounded(command, deadline, 128)?;
    Ok(())
}
fn boot_file(file: &Path) -> Result<Boot> {
    let mut input = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(file)?;
    let metadata = input.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
        || metadata.len() > RESPONSE_CAP
    {
        return Err(unavailable(
            "boot file must be owned regular non-writable handoff",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut input)
        .take(RESPONSE_CAP + 1)
        .read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| unavailable("invalid boot file schema"))
}

/// Startup verification uses one monotonic five-second budget. File claims are
/// checked against live private head/readiness and the real canonical PM root.
/// The boot ID env is only a cross-check; it does not confer authority.
pub fn verify_boot(file: &Path, root: &Path) -> Result<(Mode, Option<Hosted>)> {
    bind_boot(file, root, true)
}
fn bind_boot(file: &Path, root: &Path, startup: bool) -> Result<(Mode, Option<Hosted>)> {
    let deadline = Deadline::after(Duration::from_secs(5));
    let boot = boot_file(file)?;
    boot.binding.validate()?;
    if boot.protocol != "tracker-v1"
        || !boot.restore_ready
        || boot.limits.artifact_bytes != super::FREEZE_CAP
        || boot.limits.chunk_bytes != CHUNK as u64
        || boot.limits.chunks != 256
        || boot.limits.manifest_bytes != RESPONSE_CAP
        || boot.limits.io_ms != 20000
        || std::env::var("CADENCE_TRACKER_BOOT_ID").ok().as_deref() != Some(&boot.binding.boot_id)
    {
        return Err(unavailable(
            "boot protocol/readiness/limits/identity mismatch",
        ));
    }
    let expected = crate::issue::default_dir()?;
    let legacy_first_boot = boot.mode == Mode::Legacy && boot.head.is_none();
    // The configured PM path may be the installer's logical symlink (e.g.
    // /root/pm); all Git/file operations use its validated canonical target.
    // For the one permitted missing Legacy leaf, prove the leaf itself is
    // absent with lstat rather than treating a dangling symlink as absence.
    let canonical_root = match expected.canonicalize() {
        Ok(canonical) => {
            if root.canonicalize()? != canonical {
                return Err(unavailable("tracker root mismatch"));
            }
            canonical
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && legacy_first_boot => {
            if root != expected
                || expected.symlink_metadata().is_ok()
                || expected
                    .symlink_metadata()
                    .is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound)
                || !expected
                    .parent()
                    .is_some_and(|parent| parent.canonicalize().is_ok())
            {
                return Err(unavailable("tracker root mismatch"));
            }
            expected.clone()
        }
        Err(error) => return Err(error.into()),
    };
    // Installer journal identity follows the trusted logical root, not the
    // canonical target basename (the two differ for hosted symlink installs).
    let journal = expected.parent().unwrap_or(Path::new("/")).join(format!(
        ".{}.tracker-install.json",
        expected.file_name().unwrap_or_default().to_string_lossy()
    ));
    if journal.symlink_metadata().is_ok() {
        return Err(unavailable("interrupted tracker install"));
    }
    let store = HttpStore {
        binding: boot.binding,
        root: canonical_root,
        mode: boot.mode,
        base: private_endpoint(),
    };
    let live = store.live(&deadline)?;
    if let Some(receipt) = &boot.head {
        receipt.validate()?;
    }
    if legacy_first_boot && live.head.is_none() {
        // Explicit Legacy plus a valid owned boot declaration and matching
        // live private binding/readiness is the only no-head first-boot path.
        // The PM may not exist or be initialized as Git until setup runs.
        if deadline.expired() {
            return Err(unavailable("boot verification deadline"));
        }
        return Ok((Mode::Legacy, None));
    }
    let head = local_head(&store.root, &deadline)?;
    restore_profile(&store.root, &head, &deadline)?;
    if startup {
        // Exact checkout equality is a startup admission check only. Runtime
        // writers may observe legitimate in-progress working-tree changes;
        // their fence is live binding plus acknowledged-head ancestry.
        verify_checkout(&store.root, &head, &deadline)?;
        let mut fsck = Command::new("git");
        fsck.arg("-C").arg(&store.root).args(["fsck", "--full"]);
        run_bounded(fsck, &deadline, RESPONSE_CAP)?;
        if live.head != boot.head {
            return Err(unavailable("selected boot head changed"));
        }
        if live.head.as_ref().is_some_and(|r| r.commit != head) {
            return Err(unavailable("installed commit is not selected head"));
        }
    } else {
        // The boot handoff pins the RESTORED selection, not forever the latest
        // head. Preserve both that history and the current acknowledged head.
        for receipt in [&boot.head, &live.head].into_iter().flatten() {
            preserve_head(&store.root, &receipt.commit, &deadline)?;
        }
    }
    // Pm::at parses real YAML and verifies the installed root is openable.
    let pm = crate::issue::Pm::at(&store.root)?;
    crate::issue::board::load_all(&pm.dir, None)?;
    if deadline.expired() {
        return Err(unavailable("boot verification deadline"));
    }
    let mode = store.mode;
    let hosted = if mode == Mode::Required {
        Some(Hosted {
            binding: store.binding.origin(),
            store: Arc::new(store),
            persist_budget: Duration::from_secs(20),
        })
    } else {
        None
    };
    Ok((mode, hosted))
}

/// No boot artifact means explicit ordinary local/legacy configuration. A
/// declared path that is missing/malformed never degrades to local mode.
pub fn configure_from_env(root: &Path) -> Result<Option<(Mode, Option<Hosted>)>> {
    from_env(root, true)
}
pub fn runtime_from_env(root: &Path) -> Result<Option<(Mode, Option<Hosted>)>> {
    from_env(root, false)
}
fn from_env(root: &Path, startup: bool) -> Result<Option<(Mode, Option<Hosted>)>> {
    match std::env::var_os("CADENCE_TRACKER_BOOT_FILE") {
        Some(file) => bind_boot(Path::new(&file), root, startup).map(Some),
        None if std::env::var_os("CADENCE_TRACKER_BOOT_ID").is_some() => {
            Err(unavailable("boot handoff missing"))
        }
        None => Ok(None),
    }
}

pub fn verify_boot_command(file: &Path) -> Result<Value> {
    let root = crate::issue::default_dir()?;
    let (mode, _) = verify_boot(file, &root)?;
    Ok(json!({"protocol":"tracker-v1", "mode":mode, "verified":true}))
}
