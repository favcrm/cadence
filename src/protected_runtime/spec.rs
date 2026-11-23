//! The deterministic owned container specification.
//!
//! Every field comes from the root profile plus the owner-allocated identity.
//! There is no caller path to an image, mount, command, capability, device,
//! network, host namespace or Docker flag. The object is owned by one exact
//! label set (owner/profile/workspace/runtime/generation/epoch/image/uid) and
//! one nonce (the obligation's operation id), which is also the reconciliation
//! key for a lost create/start acknowledgement.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::profile::LocalProfile;

/// Where the guest sees its single operations socket. The parent directory
/// lives on the read-only rootfs (root-owned, immutable), so a guest-owned
/// socket inside cannot be unlinked, chmodded or replaced; the mount itself is
/// read-only but connect(2) still works.
pub(crate) const OPS_TARGET: &str = "/opt/cadence/run/ops.sock";
/// Where the guest sees its attestation socket (same read-only parent).
pub(crate) const ATTEST_TARGET: &str = "/opt/cadence/run/attest.sock";
/// The fixed guest path of the reviewed NAPI seal addon. The pinned observer
/// requires exactly this path after exec; [`super::profile::RuntimePin`] pins
/// its bytes and the root owner hashes them through `/proc/<pid>/root` before
/// readiness.
pub(crate) const SEAL_ADDON_PATH: &str = "/opt/cadence/node/cadence_seal.node";

/// The deterministic producer hostname written to `/etc/hostname`. Without it
/// Docker synthesizes the short container id, so `/etc/hostname`, `/etc/hosts`
/// and the injected `HOSTNAME` would be owner-name dependent.
pub(crate) const HOSTNAME: &str = "cadence-protected";
/// The only resolver the network-none guest is allowed to see. Docker still
/// generates the container's `resolv.conf` (with a deterministic header),
/// but the approved bytes contain exactly this nameserver and option.
pub(crate) const DNS_NAMESERVER: &str = "127.0.0.1";
/// `.` is the approved search domain. Docker's `OverrideSearch` filters it
/// out of the generated file, while still recording the override header.
pub(crate) const DNS_SEARCH: &str = ".";
pub(crate) const DNS_OPTION: &str = "ndots:0";

/// The exact masked/read-only paths the producer sends. Measurement compares
/// the inspected config against these same lists, so a forged create body
/// cannot drop a configured restriction while the kernel table still looks
/// plausible.
pub(crate) const MASKED_PATHS: [&str; 10] = [
    "/proc/acpi",
    "/proc/asound",
    "/proc/kcore",
    "/proc/keys",
    "/proc/latency_stats",
    "/proc/timer_list",
    "/proc/timer_stats",
    "/proc/sched_debug",
    "/proc/scsi",
    "/sys/firmware",
];
pub(crate) const READONLY_PATHS: [&str; 5] = [
    "/proc/bus",
    "/proc/fs",
    "/proc/irq",
    "/proc/sys",
    "/proc/sysrq-trigger",
];

/// The four fixed tmpfs mounts and their exact option strings. Kept here so
/// the create body and the post-start measurement compare against one source
/// of truth instead of re-deriving the Docker syntax twice.
pub(crate) fn socket_binds(spec: &RuntimeSpec) -> Result<Vec<String>> {
    let ops = spec
        .ops_socket
        .to_str()
        .ok_or_else(|| Error::internal("ops socket path not UTF-8"))?;
    let attest = spec
        .attest_socket
        .to_str()
        .ok_or_else(|| Error::internal("attest socket path not UTF-8"))?;
    Ok(vec![
        format!("{ops}:{OPS_TARGET}:ro"),
        format!("{attest}:{ATTEST_TARGET}:ro"),
    ])
}

pub(crate) fn tmpfs_mounts(profile: &LocalProfile, spec: &RuntimeSpec) -> BTreeMap<String, String> {
    let tmpfs = profile.runtime().tmpfs_bytes;
    let mut mounts = BTreeMap::new();
    mounts.insert("/tmp".to_string(), format!("rw,nosuid,nodev,size={tmpfs}"));
    mounts.insert("/run".to_string(), format!("rw,nosuid,nodev,size={tmpfs}"));
    mounts.insert(
        "/home/agent".to_string(),
        format!(
            "rw,nosuid,nodev,uid={},gid={},mode=0700,size={tmpfs}",
            spec.os_uid, spec.os_gid
        ),
    );
    mounts.insert(
        "/workspace".to_string(),
        format!(
            "rw,nosuid,nodev,uid={},gid={},mode=0700,size={tmpfs}",
            spec.os_uid, spec.os_gid
        ),
    );
    mounts
}

pub(crate) const LABEL_OWNER: &str = "cadence.protected.owner";
pub(crate) const LABEL_PROFILE: &str = "cadence.protected.profile";
pub(crate) const LABEL_WORKSPACE: &str = "cadence.protected.workspace";
pub(crate) const LABEL_RUNTIME: &str = "cadence.protected.runtime";
pub(crate) const LABEL_GENERATION: &str = "cadence.protected.generation";
pub(crate) const LABEL_EPOCH: &str = "cadence.protected.owner_epoch";
pub(crate) const LABEL_IMAGE: &str = "cadence.protected.image";
pub(crate) const LABEL_UID: &str = "cadence.protected.uid";
pub(crate) const LABEL_NONCE: &str = "cadence.protected.nonce";
pub(crate) const OWNER_LABEL: &str = "cadence.protected.owner=1";

/// Everything the create body and the reconciliation/measurement need.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeSpec {
    pub(crate) runtime_id: String,
    pub(crate) generation: u64,
    pub(crate) owner_epoch: u64,
    pub(crate) nonce: String,
    pub(crate) os_uid: u32,
    pub(crate) os_gid: u32,
    pub(crate) container_name: String,
    pub(crate) ops_socket: PathBuf,
    pub(crate) attest_socket: PathBuf,
}

pub(crate) fn labels(profile: &LocalProfile, spec: &RuntimeSpec) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert(LABEL_OWNER.to_string(), "1".to_string());
    labels.insert(LABEL_PROFILE.to_string(), profile.digest().to_string());
    labels.insert(
        LABEL_WORKSPACE.to_string(),
        profile.workspace_id().to_string(),
    );
    labels.insert(LABEL_RUNTIME.to_string(), spec.runtime_id.clone());
    labels.insert(LABEL_GENERATION.to_string(), spec.generation.to_string());
    labels.insert(LABEL_EPOCH.to_string(), spec.owner_epoch.to_string());
    labels.insert(LABEL_IMAGE.to_string(), profile.image().digest.clone());
    labels.insert(LABEL_UID.to_string(), spec.os_uid.to_string());
    labels.insert(LABEL_NONCE.to_string(), spec.nonce.clone());
    labels
}

/// Build the exact `POST /containers/create` body.
pub(crate) fn create_body(profile: &LocalProfile, spec: &RuntimeSpec) -> Result<Vec<u8>> {
    let runtime = profile.runtime();
    let mut labels_json = serde_json::Map::new();
    for (k, v) in labels(profile, spec) {
        labels_json.insert(k, serde_json::Value::String(v));
    }
    let seccomp = std::str::from_utf8(&profile.seccomp().document)
        .map_err(|_| Error::internal("seccomp document is not UTF-8"))?;
    if seccomp.contains('\n') || seccomp.contains('\r') {
        return Err(Error::internal(
            "seccomp document must be one line for the Docker option",
        ));
    }
    // Node is the guest entrypoint; `node_args` carries the fixed Node flags
    // plus the pinned Pi entry and its approved arguments. The reviewed NAPI
    // seal addon is loaded by the observer inside this Node process; there is
    // no separate pre-exec executable.
    let cmd: Vec<serde_json::Value> = runtime
        .node_args
        .iter()
        .map(|arg| serde_json::Value::String(arg.clone()))
        .collect();
    let mut env = Vec::new();
    for entry in &runtime.env {
        env.push(serde_json::Value::String(entry.clone()));
    }
    let binds: Vec<serde_json::Value> = socket_binds(spec)?
        .into_iter()
        .map(serde_json::Value::String)
        .collect();
    let tmpfs_mounts = tmpfs_mounts(profile, spec);
    let host_config = serde_json::json!({
        "Binds": binds,
        "Dns": [DNS_NAMESERVER],
        "DnsSearch": [DNS_SEARCH],
        "DnsOptions": [DNS_OPTION],
        "NetworkMode": "none",
        "PortBindings": {},
        "RestartPolicy": {"Name": "no", "MaximumRetryCount": 0},
        "AutoRemove": false,
        "Privileged": false,
        "PublishAllPorts": false,
        "ReadonlyRootfs": true,
        "CapDrop": ["ALL"],
        "CapAdd": [],
        "Devices": [],
        "DeviceCgroupRules": [],
        "SecurityOpt": [
            "no-new-privileges:true",
            format!("seccomp={seccomp}")
        ],
        "PidMode": "",
        "IpcMode": "private",
        "CgroupnsMode": "private",
        "UTSMode": "",
        "UsernsMode": "",
        "Tmpfs": tmpfs_mounts,
        "Ulimits": [
            {"Name": "core", "Soft": 0, "Hard": 0},
            {"Name": "nofile", "Soft": 1024, "Hard": 1024},
            {"Name": "nproc", "Soft": 256, "Hard": 256}
        ],
        "LogConfig": {"Type": "none", "Config": {}},
        "MaskedPaths": MASKED_PATHS,
        "ReadonlyPaths": READONLY_PATHS
    });
    let mut body = serde_json::json!({
        "Hostname": HOSTNAME,
        "Domainname": "",
        "User": format!("{}:{}", spec.os_uid, spec.os_gid),
        // The owner attaches exactly this runtime's console after start; the
        // attach flags must be set at create time or Docker yields no stream.
        // StdinOnce stays false so closing an attachment never kills the guest.
        "AttachStdin": true,
        "AttachStdout": true,
        "AttachStderr": true,
        "Tty": false,
        "OpenStdin": true,
        "StdinOnce": false,
        "Env": env,
        "Cmd": cmd,
        "Entrypoint": [runtime.node_path.clone()],
        "Image": profile.image().reference.clone(),
        "Labels": serde_json::Value::Object(labels_json),
        "WorkingDir": "/workspace",
        "NetworkDisabled": true,
        "StopSignal": "SIGTERM",
    });
    body.as_object_mut()
        .expect("container spec is an object")
        .insert("HostConfig".to_string(), host_config);
    serde_json::to_vec(&body).map_err(|_| Error::internal("container spec cannot be encoded"))
}

/// Create the per-runtime socket directory (`0770 root:<supervisor gid>`) and
/// the placeholder regular files that the bind mounts name. The daemon later
/// replaces `ops.sock` with its listening socket; the owner replaces
/// `attest.sock` with its own listener before start.
pub(crate) fn prepare_runtime_dir(
    run_dir: &Path,
    runtime_id: &str,
    supervisor_gid: u32,
    ops: &Path,
    attest: &Path,
) -> Result<()> {
    let base = run_dir.join("runtimes");
    let dir = base.join(runtime_id);
    if dir.parent() != Some(base.as_path()) {
        return Err(Error::internal("runtime directory escaped the run dir"));
    }
    std::fs::create_dir_all(&base)
        .map_err(|_| Error::internal("runtime run directory cannot be created"))?;
    set_owner_mode(&base, 0, supervisor_gid, 0o750)?;
    std::fs::create_dir_all(&dir)
        .map_err(|_| Error::internal("runtime directory cannot be created"))?;
    set_owner_mode(&dir, 0, supervisor_gid, 0o770)?;
    for path in [ops, attest] {
        if path.exists() {
            return Err(Error::rejected(
                "runtime socket placeholder already exists — object is not fresh",
            ));
        }
        std::fs::write(path, b"")
            .map_err(|_| Error::internal("runtime socket placeholder cannot be created"))?;
        set_owner_mode(path, 0, 0, 0o600)?;
    }
    Ok(())
}

/// Remove a regular placeholder so the daemon can bind the real ops socket.
/// Refuses anything that is not a regular root-owned file (never unlink a
/// socket, directory or foreign object).
pub(crate) fn take_placeholder(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| Error::rejected("ops socket placeholder is missing"))?;
    if !meta.is_file() || meta.is_symlink() || meta.uid() != 0 || meta.mode() & 0o077 != 0 {
        return Err(Error::rejected(
            "ops socket placeholder is not the owned root placeholder",
        ));
    }
    std::fs::remove_file(path)
        .map_err(|_| Error::internal("ops socket placeholder cannot be replaced"))
}

/// Seal an allocated runtime's listening stream socket for the guest.
/// The root owner calls this only for paths from its own launch obligation.
pub(crate) fn seal_socket_path(path: &Path, os_uid: u32, os_gid: u32) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| Error::rejected("runtime socket is missing — bind it before start"))?;
    if !meta.file_type().is_socket() || !is_listening(path)? {
        return Err(Error::rejected("runtime socket is not a listening stream"));
    }
    chown(path, os_uid, os_gid)?;
    chmod(path, 0o600)?;
    Ok(())
}

/// `/proc/net/unix` reports sockfs inodes, not the VFS inode from stat(path).
/// Match the exact allocated pathname and require stream/SO_ACCEPTCONN.
pub(crate) fn is_listening(path: &Path) -> Result<bool> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::rejected("runtime socket path is not UTF-8"))?;
    let table = std::fs::read_to_string("/proc/net/unix")
        .map_err(|_| Error::unknown("unix socket table unreadable"))?;
    for line in table.lines().skip(1) {
        let Some(prefix) = line.strip_suffix(path) else {
            continue;
        };
        if !prefix.ends_with(' ') {
            continue;
        }
        let mut fields = prefix.split_whitespace();
        let flags = fields
            .nth(3)
            .and_then(|value| u32::from_str_radix(value, 16).ok());
        let stream = fields.next() == Some("0001");
        let _state = fields.next();
        let inode = fields.next().and_then(|value| value.parse::<u64>().ok());
        if fields.next().is_none()
            && stream
            && inode.is_some()
            && flags.is_some_and(|value| value & 0x0001_0000 != 0)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Re-own a root-allocated path (used by the owner console socket seal).
pub(crate) fn chown_path(path: &Path, uid: u32, gid: u32) -> Result<()> {
    chown(path, uid, gid)
}

fn chown(path: &Path, uid: u32, gid: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::internal("socket path is not a C string"))?;
    if unsafe { libc::chown(c.as_ptr(), uid, gid) } != 0 {
        return Err(Error::internal(format!(
            "cannot seal socket ownership: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn chmod(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::internal("socket path is not a C string"))?;
    if unsafe { libc::chmod(c.as_ptr(), mode as libc::mode_t) } != 0 {
        return Err(Error::internal(format!(
            "cannot seal socket mode: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn set_owner_mode(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    chown(path, uid, gid)?;
    chmod(path, mode)
}
