//! The root-owned `native-oci-agent/1` trust profile.
//!
//! This file describes *expected* measurements only. It has no `qualified`
//! flag, no runtime admission and no authority of its own: a runtime becomes
//! admissible only after the owner commits a consumed obligation and measures
//! the live guest. The profile is trusted because of *where it lives*
//! (root-owned, non-writable path), never because a caller, environment
//! variable or request supplied it.
//!
//! Rejected outright: unknown/duplicate JSON keys, non-root or
//! group/other-writable file or parent, symlinked profile, unset digest,
//! caller-supplied environment overrides, LD_/NODE_OPTIONS loader injection and
//! any deployment class other than `native-oci-agent/1`.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::strict;
use super::PROFILE_CLASS;

const MAX_PROFILE_BYTES: u64 = 256 * 1024;
const MAX_ENV_ENTRIES: usize = 24;
const MAX_NODE_ARGS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorPin {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) exe_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GrantIssuerPin {
    pub(crate) kid: String,
    /// Root-owned `0600` Ed25519 PKCS#8 file. The installer (UID 0) is the only
    /// reader; the supervisor/daemon only stats it.
    pub(crate) private_key_file: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallerPin {
    pub(crate) exe_digest: String,
    pub(crate) generation: String,
    /// The fixed root-owned installer binary the owner launches itself. It is
    /// never a caller command and never resolved from PATH.
    pub(crate) path: String,
    /// The local grant issuer whose public half is pinned in `issuer_keys`.
    pub(crate) grant_issuer: GrantIssuerPin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IssuerKey {
    pub(crate) kid: String,
    pub(crate) x: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImagePin {
    /// `ref@sha256:<64 hex>` — the exact digest-pinned image reference.
    pub(crate) reference: String,
    /// `sha256:<64 hex>` of the image's repo digest hex, for label binding.
    pub(crate) digest: String,
    pub(crate) source: String,
    pub(crate) helper: String,
    pub(crate) node: String,
    pub(crate) pi_graph: String,
    pub(crate) policy: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimePin {
    pub(crate) node_path: String,
    pub(crate) observer_path: String,
    /// sha256 of the NAPI seal addon the observer loads from
    /// `spec::SEAL_ADDON_PATH`.
    pub(crate) seal_digest: String,
    pub(crate) node_digest: String,
    pub(crate) observer_digest: String,
    pub(crate) pi_digest: String,
    pub(crate) material_root: String,
    pub(crate) material_digest: String,
    pub(crate) env: Vec<String>,
    pub(crate) node_args: Vec<String>,
    pub(crate) tmpfs_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SeccompPin {
    pub(crate) path: PathBuf,
    pub(crate) digest: String,
    /// The exact profile bytes; the digest and bytes must agree.
    pub(crate) document: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovedTool {
    pub(crate) name: String,
    pub(crate) schema_digest: String,
    pub(crate) implementation_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistryPin {
    pub(crate) manifest_id: String,
    pub(crate) manifest_revision: u64,
    pub(crate) policy_digest: String,
    pub(crate) registry_digest: String,
    pub(crate) approved: Vec<ApprovedTool>,
}

/// The validated root profile. Every field is private; construction is only
/// through [`LocalProfile::load_production`] / [`LocalProfile::load_from`]
/// after the file and parent ownership checks.
#[derive(Clone, Debug)]
pub(crate) struct LocalProfile {
    profile_path: PathBuf,
    raw: Vec<u8>,
    digest: String,
    workspace_id: String,
    core_id: String,
    lineage_reference: String,
    database_epoch: u64,
    owner_exe_digest: String,
    owner_public_key: [u8; 32],
    owner_private_key_file: PathBuf,
    supervisor: SupervisorPin,
    installer: InstallerPin,
    issuer_keys: Vec<IssuerKey>,
    image: ImagePin,
    runtime: RuntimePin,
    seccomp: SeccompPin,
    uid_start: u32,
    uid_end: u32,
    operator_origin: String,
    docker_socket: PathBuf,
    max_runtimes: u32,
    registry: RegistryPin,
    state_dir: PathBuf,
    run_dir: PathBuf,
    control_socket: PathBuf,
}

impl LocalProfile {
    /// Load the compiled-in production profile path. No environment variable
    /// or request field can redirect this path.
    pub(crate) fn load_production() -> Result<Self> {
        Self::load_from(Path::new(super::PROFILE_PATH))
    }

    /// Load a root-owned profile from an operator-supplied absolute path (the
    /// root service's own argv, never agent input). The same ownership,
    /// no-symlink, strict-schema and digest rules apply.
    pub(crate) fn load_from(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::rejected("profile path must be absolute"));
        }
        let canonical = std::fs::canonicalize(path)
            .map_err(|_| Error::rejected("profile is missing or unresolvable"))?;
        if canonical != path {
            return Err(Error::rejected(
                "profile path may not traverse a symlink or relative component",
            ));
        }
        let meta = std::fs::symlink_metadata(&canonical)
            .map_err(|_| Error::rejected("profile metadata unreadable"))?;
        if !meta.is_file() || meta.is_symlink() {
            return Err(Error::rejected("profile must be a regular file"));
        }
        if meta.uid() != 0 {
            return Err(Error::rejected("profile must be owned by root"));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(Error::rejected("profile must not be group/other writable"));
        }
        if meta.len() == 0 || meta.len() > MAX_PROFILE_BYTES {
            return Err(Error::rejected("profile size is outside the bound"));
        }
        verify_root_owned_parents(&canonical)?;
        let raw =
            std::fs::read(&canonical).map_err(|_| Error::rejected("profile is unreadable"))?;
        let value = strict::parse(&raw)?;
        let mut profile = parse_profile(&canonical, raw, &value)?;
        profile.load_seccomp()?;
        Ok(profile)
    }

    fn load_seccomp(&mut self) -> Result<()> {
        let path = self.seccomp.path.clone();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|_| Error::rejected("seccomp profile is missing"))?;
        if !meta.is_file() || meta.is_symlink() {
            return Err(Error::rejected("seccomp profile must be a regular file"));
        }
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return Err(Error::rejected(
                "seccomp profile must be root-owned and not group/other writable",
            ));
        }
        verify_root_owned_parents(&path)?;
        if meta.len() == 0 || meta.len() > MAX_PROFILE_BYTES {
            return Err(Error::rejected("seccomp profile size is outside the bound"));
        }
        let document =
            std::fs::read(&path).map_err(|_| Error::rejected("seccomp profile unreadable"))?;
        let digest = sha256_digest(&document);
        if digest != self.seccomp.digest {
            return Err(Error::rejected(
                "seccomp profile bytes do not match the pinned digest",
            ));
        }
        // The document must itself be a JSON object; the Engine API receives
        // these exact bytes.
        let _ = strict::parse(&document)?;
        self.seccomp.document = document;
        Ok(())
    }

    pub(crate) fn profile_path(&self) -> &Path {
        &self.profile_path
    }
    pub(crate) fn raw(&self) -> &[u8] {
        &self.raw
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    pub(crate) fn core_id(&self) -> &str {
        &self.core_id
    }
    pub(crate) fn lineage_reference(&self) -> &str {
        &self.lineage_reference
    }
    pub(crate) fn database_epoch(&self) -> u64 {
        self.database_epoch
    }
    pub(crate) fn supervisor(&self) -> &SupervisorPin {
        &self.supervisor
    }
    /// The pinned digest of the root owner binary the broker fence listener
    /// authenticates; never a caller-supplied pin.
    pub(crate) fn owner_exe_digest(&self) -> &str {
        &self.owner_exe_digest
    }
    pub(crate) fn owner_public_key(&self) -> &[u8; 32] {
        &self.owner_public_key
    }
    pub(crate) fn owner_private_key_file(&self) -> &Path {
        &self.owner_private_key_file
    }
    /// The private broker-fence socket (owner client → daemon writer fence).
    pub(crate) fn broker_fence_socket(&self) -> PathBuf {
        self.run_dir.join("broker").join("broker-fence.sock")
    }
    /// `true` only when this root-owned profile exists and validates. This is
    /// a DENY gate for unsafe legacy launches; it is never readiness. Actual
    /// protected readiness is only a `QualifiedRuntime` minted by a fresh
    /// owner recheck after a consumed obligation and live measurements.
    pub(crate) fn configured() -> bool {
        Self::load_production().is_ok()
    }
    pub(crate) fn installer(&self) -> &InstallerPin {
        &self.installer
    }
    pub(crate) fn issuer_keys(&self) -> &[IssuerKey] {
        &self.issuer_keys
    }
    pub(crate) fn image(&self) -> &ImagePin {
        &self.image
    }
    pub(crate) fn runtime(&self) -> &RuntimePin {
        &self.runtime
    }
    pub(crate) fn seccomp(&self) -> &SeccompPin {
        &self.seccomp
    }
    pub(crate) fn uid_range(&self) -> (u32, u32) {
        (self.uid_start, self.uid_end)
    }
    pub(crate) fn operator_origin(&self) -> &str {
        &self.operator_origin
    }
    pub(crate) fn docker_socket(&self) -> &Path {
        &self.docker_socket
    }
    pub(crate) fn max_runtimes(&self) -> u32 {
        self.max_runtimes
    }
    pub(crate) fn registry(&self) -> &RegistryPin {
        &self.registry
    }
    pub(crate) fn state_dir(&self) -> &Path {
        &self.state_dir
    }
    pub(crate) fn run_dir(&self) -> &Path {
        &self.run_dir
    }
    pub(crate) fn control_socket(&self) -> &Path {
        &self.control_socket
    }

    /// The supervisor grant-listener path: the fixed private supervisor-owned
    /// socket the installer is told to use. It is never derived from a caller
    /// field or an environment variable.
    pub(crate) fn supervisor_grant_socket(&self) -> PathBuf {
        PathBuf::from(super::SUPERVISOR_GRANT_SOCKET)
    }

    /// The canonical `company` the local issuer scope fixes for this workspace.
    pub(crate) fn company(&self) -> &str {
        &self.workspace_id
    }
}

#[cfg(test)]
impl LocalProfile {
    /// A fully populated in-memory profile for unit tests of proof
    /// verification. It is NOT loadable authority: every path is nonexistent
    /// and no production code can construct it.
    pub(crate) fn test_fixture(
        owner_public_key: [u8; 32],
        supervisor_uid: u32,
        supervisor_gid: u32,
        installer_generation: String,
    ) -> Self {
        let hex64 = |c: char| c.to_string().repeat(64);
        let sha = |c: char| format!("sha256:{}", hex64(c));
        let image = format!("registry.example/cadence@sha256:{}", hex64('d'));
        Self {
            profile_path: PathBuf::from("/nonexistent/profile.json"),
            raw: b"fixture".to_vec(),
            digest: sha256_digest(b"fixture"),
            workspace_id: "workspace-test".to_string(),
            core_id: "core-test".to_string(),
            lineage_reference: "lineage-test".to_string(),
            database_epoch: 1,
            owner_exe_digest: sha('a'),
            owner_public_key,
            owner_private_key_file: PathBuf::from("/nonexistent/owner.key"),
            supervisor: SupervisorPin {
                uid: supervisor_uid,
                gid: supervisor_gid,
                exe_digest: sha('b'),
            },
            installer: InstallerPin {
                exe_digest: sha('c'),
                generation: installer_generation,
                path: "/nonexistent/installer".to_string(),
                grant_issuer: GrantIssuerPin {
                    kid: "issuer-test".to_string(),
                    private_key_file: PathBuf::from("/nonexistent/issuer.key"),
                },
            },
            issuer_keys: vec![IssuerKey {
                kid: "issuer-test".to_string(),
                x: [0x11; 32],
            }],
            image: ImagePin {
                reference: image.clone(),
                digest: sha('d'),
                source: "e".repeat(40),
                helper: hex64('1'),
                node: hex64('2'),
                pi_graph: hex64('3'),
                policy: hex64('4'),
            },
            runtime: RuntimePin {
                node_path: "/opt/cadence/node/bin/node".to_string(),
                observer_path: "/opt/cadence/node/observer.mjs".to_string(),
                seal_digest: sha('5'),
                node_digest: sha('6'),
                observer_digest: sha('7'),
                pi_digest: sha('8'),
                material_root: "/opt/cadence/material".to_string(),
                material_digest: sha('9'),
                env: vec!["PATH=/usr/bin:/bin".to_string()],
                node_args: vec!["--no-warnings".to_string()],
                tmpfs_bytes: 33_554_432,
            },
            seccomp: SeccompPin {
                path: PathBuf::from("/nonexistent/seccomp.json"),
                digest: sha('0'),
                document: b"{}".to_vec(),
            },
            uid_start: 21001,
            uid_end: 21999,
            operator_origin: "https://operator.example.invalid".to_string(),
            docker_socket: PathBuf::from("/nonexistent/docker.sock"),
            max_runtimes: 1,
            registry: RegistryPin {
                manifest_id: "manifest-test".to_string(),
                manifest_revision: 1,
                policy_digest: sha('a'),
                registry_digest: sha('b'),
                approved: vec![ApprovedTool {
                    name: "read".to_string(),
                    schema_digest: sha('c'),
                    implementation_digest: sha('d'),
                }],
            },
            state_dir: PathBuf::from("/nonexistent/state"),
            run_dir: PathBuf::from("/nonexistent/run"),
            control_socket: PathBuf::from("/nonexistent/run/control.sock"),
        }
    }
}

fn parse_profile(path: &Path, raw: Vec<u8>, value: &serde_json::Value) -> Result<LocalProfile> {
    let m = strict::object(
        value,
        &[
            "v",
            "deployment_class",
            "workspace_id",
            "core_id",
            "lineage_reference",
            "database_epoch",
            "owner",
            "supervisor",
            "installer",
            "issuer_keys",
            "image",
            "runtime",
            "seccomp",
            "uids",
            "operator_origin",
            "docker",
            "registry",
            "paths",
            "max_runtimes",
        ],
    )?;
    if strict::safe_u64(m, "v")? != 1 {
        return Err(Error::rejected("profile v must be exactly 1"));
    }
    if strict::string(m, "deployment_class")? != PROFILE_CLASS {
        return Err(Error::rejected(format!(
            "profile deployment_class must be {PROFILE_CLASS}"
        )));
    }
    let workspace_id = bounded_name(strict::string(m, "workspace_id")?, 200)?;
    let core_id = bounded_name(strict::string(m, "core_id")?, 200)?;
    let lineage_reference = {
        let s = strict::string(m, "lineage_reference")?;
        if s.is_empty()
            || s.len() > 128
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(Error::rejected("lineage_reference is not a bounded ref"));
        }
        s.to_string()
    };
    let database_epoch = strict::positive_u64(m, "database_epoch")?;

    let owner_pin = strict::object(
        m.get("owner")
            .ok_or_else(|| Error::rejected("owner pin missing"))?,
        &["exe_digest", "public_key", "private_key_file"],
    )?;
    let owner_exe_digest = pinned_digest(owner_pin, "exe_digest")?;
    let owner_public_key: [u8; 32] = decode_b64url(strict::string(owner_pin, "public_key")?)?
        .try_into()
        .map_err(|_| Error::rejected("owner public_key must be 32 bytes"))?;
    let owner_private_key_file = PathBuf::from(strict::string(owner_pin, "private_key_file")?);
    if !owner_private_key_file.is_absolute() {
        return Err(Error::rejected("owner private_key_file must be absolute"));
    }
    verify_root_key_file(&owner_private_key_file)?;

    let sup = strict::object(
        m.get("supervisor")
            .ok_or_else(|| Error::rejected("supervisor pin missing"))?,
        &["uid", "gid", "exe_digest"],
    )?;
    let supervisor = SupervisorPin {
        uid: uid_value(sup, "uid")?,
        gid: gid_value(sup, "gid")?,
        exe_digest: pinned_digest(sup, "exe_digest")?,
    };

    let inst = strict::object(
        m.get("installer")
            .ok_or_else(|| Error::rejected("installer pin missing"))?,
        &["exe_digest", "generation", "path", "grant_issuer"],
    )?;
    let installer_generation = strict::string(inst, "generation")?;
    if !strict::is_lower_hex(installer_generation, 32) {
        return Err(Error::rejected(
            "installer generation must be 32 lowercase hex",
        ));
    }
    let installer_path = PathBuf::from(strict::string(inst, "path")?);
    if !installer_path.is_absolute() {
        return Err(Error::rejected("installer path must be absolute"));
    }
    verify_root_exec_file(&installer_path)?;
    let grant_issuer_obj = strict::object(
        inst.get("grant_issuer")
            .ok_or_else(|| Error::rejected("installer grant_issuer missing"))?,
        &["kid", "private_key_file"],
    )?;
    let grant_kid = strict::string(grant_issuer_obj, "kid")?;
    if grant_kid.is_empty()
        || grant_kid.len() > 64
        || !grant_kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
    {
        return Err(Error::rejected("grant issuer kid is malformed"));
    }
    let grant_key_file = PathBuf::from(strict::string(grant_issuer_obj, "private_key_file")?);
    if !grant_key_file.is_absolute() {
        return Err(Error::rejected(
            "grant issuer private_key_file must be absolute",
        ));
    }
    verify_root_private_file(&grant_key_file)?;

    let mut issuer_keys = Vec::new();
    for entry in strict::array(m, "issuer_keys")? {
        if issuer_keys.len() >= 8 {
            return Err(Error::rejected("issuer_keys exceeds 8"));
        }
        let k = strict::object(entry, &["kid", "x"])?;
        let kid = strict::string(k, "kid")?;
        if kid.is_empty()
            || kid.len() > 64
            || !kid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
        {
            return Err(Error::rejected("issuer key kid is malformed"));
        }
        let x = decode_b64url(strict::string(k, "x")?)?;
        let x: [u8; 32] = x
            .try_into()
            .map_err(|_| Error::rejected("issuer key x must be 32 bytes"))?;
        if issuer_keys
            .iter()
            .any(|existing: &IssuerKey| existing.kid == kid)
        {
            return Err(Error::rejected("duplicate issuer key kid"));
        }
        issuer_keys.push(IssuerKey {
            kid: kid.to_string(),
            x,
        });
    }
    if issuer_keys.is_empty() {
        return Err(Error::rejected(
            "at least one pinned issuer key is required for the local profile",
        ));
    }
    if !issuer_keys.iter().any(|key| key.kid.as_str() == grant_kid) {
        return Err(Error::rejected(
            "grant issuer kid is not a pinned issuer_keys entry",
        ));
    }
    let installer = InstallerPin {
        exe_digest: pinned_digest(inst, "exe_digest")?,
        generation: installer_generation.to_string(),
        path: installer_path
            .to_str()
            .ok_or_else(|| Error::rejected("installer path is not UTF-8"))?
            .to_string(),
        grant_issuer: GrantIssuerPin {
            kid: grant_kid.to_string(),
            private_key_file: grant_key_file,
        },
    };

    let image = parse_image(
        m.get("image")
            .ok_or_else(|| Error::rejected("image pin missing"))?,
    )?;
    let runtime = parse_runtime(
        m.get("runtime")
            .ok_or_else(|| Error::rejected("runtime pin missing"))?,
    )?;
    let seccomp = {
        let s = strict::object(
            m.get("seccomp")
                .ok_or_else(|| Error::rejected("seccomp pin missing"))?,
            &["path", "digest"],
        )?;
        let path = PathBuf::from(strict::string(s, "path")?);
        if !path.is_absolute() {
            return Err(Error::rejected("seccomp path must be absolute"));
        }
        SeccompPin {
            path,
            digest: pinned_digest(s, "digest")?,
            document: Vec::new(),
        }
    };

    let uids = strict::object(
        m.get("uids")
            .ok_or_else(|| Error::rejected("uid range missing"))?,
        &["start", "end"],
    )?;
    let uid_start = uid_value(uids, "start")?;
    let uid_end = uid_value(uids, "end")?;
    if uid_end < uid_start {
        return Err(Error::rejected("uid range is inverted"));
    }
    if uid_end - uid_start + 1 > 1024 {
        return Err(Error::rejected("uid range exceeds 1024"));
    }
    if uid_start <= supervisor.uid && supervisor.uid <= uid_end {
        return Err(Error::rejected("uid range contains the supervisor uid"));
    }
    if uid_start < 21001 {
        return Err(Error::rejected(
            "uid range must start at or above 21001 (nonoperator/nonsupervisor)",
        ));
    }

    let operator_origin = strict::string(m, "operator_origin")?;
    if !operator_origin.starts_with("https://")
        || operator_origin.len() > 200
        || operator_origin.bytes().any(|b| b.is_ascii_whitespace())
    {
        return Err(Error::rejected(
            "operator_origin must be a bounded https origin",
        ));
    }

    let docker = strict::object(
        m.get("docker")
            .ok_or_else(|| Error::rejected("docker settings missing"))?,
        &["socket"],
    )?;
    let docker_socket = PathBuf::from(strict::string(docker, "socket")?);
    if !docker_socket.is_absolute() {
        return Err(Error::rejected("docker socket must be absolute"));
    }
    let max_runtimes = strict::positive_u64(m, "max_runtimes")?;
    if max_runtimes > 64 {
        return Err(Error::rejected("max_runtimes exceeds 64"));
    }

    let registry = parse_registry(
        m.get("registry")
            .ok_or_else(|| Error::rejected("registry pin missing"))?,
    )?;

    let paths = strict::object(
        m.get("paths")
            .ok_or_else(|| Error::rejected("paths missing"))?,
        &["state_dir", "run_dir", "control_socket"],
    )?;
    let state_dir = PathBuf::from(strict::string(paths, "state_dir")?);
    let run_dir = PathBuf::from(strict::string(paths, "run_dir")?);
    let control_socket = PathBuf::from(strict::string(paths, "control_socket")?);
    for p in [&state_dir, &run_dir, &control_socket] {
        if !p.is_absolute() {
            return Err(Error::rejected("runtime paths must be absolute"));
        }
    }
    if !control_socket.starts_with(&run_dir) {
        return Err(Error::rejected(
            "control socket must live inside the run dir",
        ));
    }

    let digest = profile_digest(&raw);
    Ok(LocalProfile {
        profile_path: path.to_path_buf(),
        raw,
        digest,
        workspace_id,
        core_id,
        lineage_reference,
        database_epoch,
        owner_exe_digest,
        owner_public_key,
        owner_private_key_file,
        supervisor,
        installer,
        issuer_keys,
        image,
        runtime,
        seccomp,
        uid_start,
        uid_end,
        operator_origin: operator_origin.to_string(),
        docker_socket,
        max_runtimes: max_runtimes as u32,
        registry,
        state_dir,
        run_dir,
        control_socket,
    })
}

fn parse_image(value: &serde_json::Value) -> Result<ImagePin> {
    let m = strict::object(value, &["ref", "pins"])?;
    let reference = strict::string(m, "ref")?;
    let (_, hex) = reference
        .rsplit_once("@sha256:")
        .ok_or_else(|| Error::rejected("image ref must be pinned with @sha256:"))?;
    if !strict::is_lower_hex(hex, 64) || reference.len() > 512 {
        return Err(Error::rejected("image ref digest is malformed"));
    }
    let pins = strict::object(
        m.get("pins")
            .ok_or_else(|| Error::rejected("image pins missing"))?,
        &["source", "helper", "node", "piGraph", "policy"],
    )?;
    let source = strict::string(pins, "source")?;
    if !strict::is_lower_hex(source, 40) {
        return Err(Error::rejected("pins.source must be 40 lowercase hex"));
    }
    let sha = |key: &str| -> Result<String> {
        let s = strict::string(pins, key)?;
        if !strict::is_lower_hex(s, 64) {
            return Err(Error::rejected(format!(
                "pins.{key} must be 64 lowercase hex"
            )));
        }
        Ok(s.to_string())
    };
    Ok(ImagePin {
        reference: reference.to_string(),
        digest: format!("sha256:{hex}"),
        source: source.to_string(),
        helper: sha("helper")?,
        node: sha("node")?,
        pi_graph: sha("piGraph")?,
        policy: sha("policy")?,
    })
}

fn parse_runtime(value: &serde_json::Value) -> Result<RuntimePin> {
    let m = strict::object(
        value,
        &[
            "node_path",
            "observer_path",
            "seal_digest",
            "node_digest",
            "observer_digest",
            "pi_digest",
            "material_root",
            "material_digest",
            "env",
            "node_args",
            "tmpfs_bytes",
        ],
    )?;
    let abs = |key: &str| -> Result<String> {
        let s = strict::string(m, key)?;
        if !s.starts_with('/') || s.len() > 512 || s.bytes().any(|b| b == 0) {
            return Err(Error::rejected(format!("{key} must be an absolute path")));
        }
        Ok(s.to_string())
    };
    let node_path = abs("node_path")?;
    let observer_path = abs("observer_path")?;
    let material_root = abs("material_root")?;
    let mut env = Vec::new();
    for entry in strict::array(m, "env")? {
        if env.len() >= MAX_ENV_ENTRIES {
            return Err(Error::rejected("runtime env exceeds the bound"));
        }
        let s = entry
            .as_str()
            .ok_or_else(|| Error::rejected("runtime env entries must be strings"))?;
        validate_env_entry(s)?;
        if env.iter().any(|existing: &String| {
            existing.split_once('=').map(|(k, _)| k) == s.split_once('=').map(|(k, _)| k)
        }) {
            return Err(Error::rejected("duplicate runtime env key"));
        }
        env.push(s.to_string());
    }
    let mut node_args = Vec::new();
    for entry in strict::array(m, "node_args")? {
        if node_args.len() >= MAX_NODE_ARGS {
            return Err(Error::rejected("node_args exceeds the bound"));
        }
        let s = entry
            .as_str()
            .ok_or_else(|| Error::rejected("node_args entries must be strings"))?;
        if s.is_empty() || s.len() > 512 || s.bytes().any(|b| b == 0 || b == b'\n') {
            return Err(Error::rejected("node_args entry is malformed"));
        }
        if s.starts_with("--inspect") || s == "--eval" || s.starts_with("--eval=") {
            return Err(Error::rejected(
                "node_args may not enable the inspector or eval",
            ));
        }
        node_args.push(s.to_string());
    }
    if node_args.first().map(String::as_str) != Some("--disable-sigusr1") {
        return Err(Error::rejected(
            "node_args must start with --disable-sigusr1",
        ));
    }
    if node_args.len() < 3 {
        return Err(Error::rejected(
            "node_args must name the pinned Pi entry after the fixed node flags",
        ));
    }
    let tmpfs_bytes = strict::positive_u64(m, "tmpfs_bytes")?;
    if tmpfs_bytes > 1024 * 1024 * 1024 {
        return Err(Error::rejected("tmpfs_bytes exceeds 1GiB"));
    }
    Ok(RuntimePin {
        node_path,
        observer_path,
        seal_digest: pinned_digest(m, "seal_digest")?,
        node_digest: pinned_digest(m, "node_digest")?,
        observer_digest: pinned_digest(m, "observer_digest")?,
        pi_digest: pinned_digest(m, "pi_digest")?,
        material_root,
        material_digest: pinned_digest(m, "material_digest")?,
        env,
        node_args,
        tmpfs_bytes,
    })
}

fn parse_registry(value: &serde_json::Value) -> Result<RegistryPin> {
    let m = strict::object(
        value,
        &[
            "manifest_id",
            "manifest_revision",
            "policy_digest",
            "registry_digest",
            "approved",
        ],
    )?;
    let manifest_id = bounded_name(strict::string(m, "manifest_id")?, 128)?;
    let manifest_revision = strict::positive_u64(m, "manifest_revision")?;
    let mut approved = Vec::new();
    for entry in strict::array(m, "approved")? {
        if approved.len() >= 256 {
            return Err(Error::rejected("approved tools exceed 256"));
        }
        let t = strict::object(entry, &["name", "schema_digest", "implementation_digest"])?;
        let name = strict::string(t, "name")?;
        if name.is_empty()
            || name.len() > 128
            || name
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(Error::rejected("approved tool name is malformed"));
        }
        if approved
            .iter()
            .any(|existing: &ApprovedTool| existing.name == name)
        {
            return Err(Error::rejected("duplicate approved tool name"));
        }
        approved.push(ApprovedTool {
            name: name.to_string(),
            schema_digest: pinned_digest(t, "schema_digest")?,
            implementation_digest: pinned_digest(t, "implementation_digest")?,
        });
    }
    if approved.is_empty() {
        return Err(Error::rejected("approved registry may not be empty"));
    }
    Ok(RegistryPin {
        manifest_id,
        manifest_revision,
        policy_digest: pinned_digest(m, "policy_digest")?,
        registry_digest: pinned_digest(m, "registry_digest")?,
        approved,
    })
}

fn validate_env_entry(s: &str) -> Result<()> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| Error::rejected("runtime env entry must be KEY=VALUE"))?;
    if key.is_empty()
        || key.len() > 128
        || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || !key.as_bytes()[0].is_ascii_alphabetic()
    {
        return Err(Error::rejected("runtime env key is malformed"));
    }
    if value.bytes().any(|b| b == 0 || b == b'\n') {
        return Err(Error::rejected("runtime env value is malformed"));
    }
    if key.starts_with("LD_")
        || matches!(
            key,
            "NODE_OPTIONS"
                | "NODE_DEBUG"
                | "NODE_INSPECT"
                | "NODE_PATH"
                | "NODE_EXTRA_CA_CERTS"
                | "BASH_ENV"
                | "ENV"
                | "CADENCE_ALIAS"
                | "CADENCE_RUNNER_ID"
                | "CADENCE_MASTER"
                | "SSH_AUTH_SOCK"
                | "DOCKER_HOST"
        )
    {
        return Err(Error::rejected(format!(
            "runtime env may not carry loader/operator material ({key})"
        )));
    }
    Ok(())
}

fn uid_value(m: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<u32> {
    let n = strict::positive_u64(m, key)?;
    if n > u32::MAX as u64 {
        return Err(Error::rejected(format!("{key} exceeds the uid range")));
    }
    Ok(n as u32)
}

fn gid_value(m: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<u32> {
    let n = strict::positive_u64(m, key)?;
    if n > u32::MAX as u64 {
        return Err(Error::rejected(format!("{key} exceeds the gid range")));
    }
    Ok(n as u32)
}

fn pinned_digest(m: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<String> {
    let s = strict::string(m, key)?;
    strict::digest(s)?;
    Ok(s.to_string())
}

fn bounded_name(s: &str, max: usize) -> Result<String> {
    if s.is_empty() || s.len() > max || s.bytes().any(|b| b.is_ascii_control()) || s.contains(':') {
        return Err(Error::rejected("name is not a bounded identifier"));
    }
    Ok(s.to_string())
}

fn decode_b64url(s: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    if s.len() > 64 {
        return Err(Error::rejected("base64url value is too long"));
    }
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::rejected("base64url value is malformed"))
}

pub(crate) fn sha256_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let d = h.finalize();
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for b in d {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Domain-separated digest of the exact profile bytes.
pub(crate) fn profile_digest(bytes: &[u8]) -> String {
    let mut material = Vec::with_capacity(bytes.len() + 32);
    material.extend_from_slice(b"cadence.runtime-owner.profile.v1\0");
    material.extend_from_slice(bytes);
    sha256_digest(&material)
}

/// A root-owned executable the owner launches itself: regular, not a symlink,
/// root-owned, not group/other-writable, inside the root-owned non-writable
/// tree. Executable bits are not required beyond what the owner can exec as
/// root, but the file must be a real regular file.
fn verify_root_exec_file(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| Error::rejected("installer executable is missing"))?;
    if !meta.is_file() || meta.is_symlink() {
        return Err(Error::rejected(
            "installer executable must be a regular file",
        ));
    }
    if meta.uid() != 0 {
        return Err(Error::rejected(
            "installer executable must be owned by root",
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(Error::rejected(
            "installer executable must not be group/other writable",
        ));
    }
    if meta.len() == 0 {
        return Err(Error::rejected("installer executable is empty"));
    }
    verify_root_owned_parents(path)
}

/// The owner signing key: root-owned, `0600`, regular, bounded, inside the
/// root-owned non-writable tree. The daemon only *stats* it (never reads it);
/// the root owner loads it and requires its public half to equal the profile
/// pin.
fn verify_root_key_file(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| Error::rejected("owner signing key is missing"))?;
    if !meta.is_file() || meta.is_symlink() {
        return Err(Error::rejected("owner signing key must be a regular file"));
    }
    if meta.uid() != 0 {
        return Err(Error::rejected("owner signing key must be owned by root"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(Error::rejected(
            "owner signing key must not be group/other accessible",
        ));
    }
    if meta.len() == 0 || meta.len() > 4096 {
        return Err(Error::rejected(
            "owner signing key size is outside the bound",
        ));
    }
    verify_root_owned_parents(path)
}

/// A root-only private signing file (grant issuer): same custody rules as the
/// owner key. The daemon only *stats* it; the installer (UID 0) reads it.
fn verify_root_private_file(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| Error::rejected("private signing key is missing"))?;
    if !meta.is_file() || meta.is_symlink() {
        return Err(Error::rejected(
            "private signing key must be a regular file",
        ));
    }
    if meta.uid() != 0 {
        return Err(Error::rejected("private signing key must be owned by root"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(Error::rejected(
            "private signing key must not be group/other accessible",
        ));
    }
    if meta.len() == 0 || meta.len() > 4096 {
        return Err(Error::rejected(
            "private signing key size is outside the bound",
        ));
    }
    verify_root_owned_parents(path)
}

/// Every ancestor up to `/` must be root-owned and not group/other writable.
/// A symlinked profile was already refused before this runs.
fn verify_root_owned_parents(path: &Path) -> Result<()> {
    let mut current = path.parent();
    while let Some(dir) = current {
        let meta =
            std::fs::metadata(dir).map_err(|_| Error::rejected("profile parent is unreadable"))?;
        if !meta.is_dir() {
            return Err(Error::rejected("profile parent is not a directory"));
        }
        if meta.uid() != 0 {
            return Err(Error::rejected("profile parent must be owned by root"));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(Error::rejected(
                "profile parent must not be group/other writable",
            ));
        }
        current = dir.parent();
    }
    Ok(())
}
