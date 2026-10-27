//! Post-start measurement of the actual guest.
//!
//! Nothing here trusts a label, a receipt, a claimed fact or the guest's own
//! `/proc` text as authority. The root owner inspects the Docker object it
//! created, the live init process, its namespaces/cgroup, capability and seccomp
//! facts, argv/environment, executable digest and the two bind-mounted sockets.
//! A missing/ambiguous fact refuses readiness rather than assuming it.

use std::collections::BTreeSet;

use crate::error::{Error, Result};

use super::docker::DockerClient;
use super::peer;
use super::profile::LocalProfile;
use super::registry;
use super::spec::{self, RuntimeSpec};
use super::store::now_ms;

#[derive(Debug)]
pub(crate) struct LiveMeasurement {
    pub(crate) container_id: String,
    pub(crate) node_pid: u32,
    pub(crate) node_starttime: u64,
    pub(crate) image_id: String,
    pub(crate) argv_digest: String,
    pub(crate) env_digest: String,
    pub(crate) measured_at_ms: u64,
}

/// Measure the owned container after `docker start`.
pub(crate) fn measure_started(
    profile: &LocalProfile,
    runtime: &RuntimeSpec,
    docker: &DockerClient,
    engine: &super::measure_mounts::EngineMountRoot,
) -> Result<LiveMeasurement> {
    let inspect = docker
        .inspect(&runtime.container_name)
        .map_err(|_| Error::unknown("owned container is not inspectable"))?;
    let state = inspect
        .get("State")
        .ok_or_else(|| Error::unknown("container state missing"))?;
    if state.get("Running").and_then(|v| v.as_bool()) != Some(true) {
        return Err(Error::unknown("owned container is not running"));
    }
    let pid = state
        .get("Pid")
        .and_then(|v| v.as_u64())
        .filter(|p| *p > 0 && *p <= 4_194_304)
        .ok_or_else(|| Error::unknown("container pid is not live"))? as u32;
    let container_id = inspect
        .get("Id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::unknown("container id missing"))?;
    if container_id.len() != 64 || !container_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::unknown("container id is malformed"));
    }
    verify_config(profile, runtime, &inspect)?;
    verify_image(profile, &inspect, docker)?;
    let image_id = inspect
        .get("Image")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::unknown("container image id missing"))?
        .to_string();
    let starttime =
        peer::starttime(pid).ok_or_else(|| Error::unknown("guest init starttime unreadable"))?;
    let proc = ProcFacts::read(pid)?;
    verify_proc(profile, runtime, pid, container_id, &proc)?;
    super::measure_mounts::verify_owned_mounts(engine, profile, runtime, &inspect, pid, starttime)?;
    Ok(LiveMeasurement {
        container_id: container_id.to_string(),
        node_pid: pid,
        node_starttime: starttime,
        image_id,
        argv_digest: proc.argv_digest,
        env_digest: proc.env_digest,
        measured_at_ms: now_ms(),
    })
}

fn verify_config(
    profile: &LocalProfile,
    runtime: &RuntimeSpec,
    inspect: &serde_json::Value,
) -> Result<()> {
    let cfg = inspect
        .get("Config")
        .ok_or_else(|| Error::unknown("container config missing"))?;
    let host = inspect
        .get("HostConfig")
        .ok_or_else(|| Error::unknown("container host config missing"))?;
    let get_str = |v: &serde_json::Value, key: &str| -> Result<String> {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::unknown(format!("container field {key} missing")))
    };
    if get_str(cfg, "Image")? != profile.image().reference {
        return Err(Error::unknown("container image reference changed"));
    }
    if get_str(cfg, "User")? != format!("{}:{}", runtime.os_uid, runtime.os_gid) {
        return Err(Error::unknown(
            "container user is not the enrolled runtime uid",
        ));
    }
    if get_str(cfg, "WorkingDir")? != "/workspace" {
        return Err(Error::unknown("container working dir changed"));
    }
    let entrypoint = cfg
        .get("Entrypoint")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::unknown("container entrypoint missing"))?;
    if entrypoint.len() != 1 || entrypoint[0].as_str() != Some(profile.runtime().node_path.as_str())
    {
        return Err(Error::unknown(
            "container entrypoint is not the pinned Node",
        ));
    }
    let expected_cmd = &profile.runtime().node_args;
    let cmd = cfg
        .get("Cmd")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::unknown("container command missing"))?;
    if cmd.len() != expected_cmd.len()
        || cmd
            .iter()
            .zip(expected_cmd.iter())
            .any(|(got, want)| got.as_str() != Some(want.as_str()))
    {
        return Err(Error::unknown(
            "container command is not the pinned Node argv",
        ));
    }
    let env = cfg
        .get("Env")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::unknown("container env missing"))?;
    let mut got_env: BTreeSet<String> = BTreeSet::new();
    for entry in env {
        got_env.insert(
            entry
                .as_str()
                .ok_or_else(|| Error::unknown("container env entry is not a string"))?
                .to_string(),
        );
    }
    let want_env: BTreeSet<String> = profile.runtime().env.iter().cloned().collect();
    if got_env != want_env || got_env.len() != env.len() {
        return Err(Error::unknown(
            "container environment differs from the pinned environment",
        ));
    }
    if cfg.get("Tty").and_then(|v| v.as_bool()) != Some(false)
        || cfg.get("OpenStdin").and_then(|v| v.as_bool()) != Some(true)
        || cfg.get("AttachStdin").and_then(|v| v.as_bool()) != Some(true)
        || cfg.get("AttachStdout").and_then(|v| v.as_bool()) != Some(true)
        || cfg.get("AttachStderr").and_then(|v| v.as_bool()) != Some(true)
        || cfg.get("StdinOnce").and_then(|v| v.as_bool()) != Some(false)
    {
        return Err(Error::unknown("container stdio/pty flags changed"));
    }
    if get_str(host, "NetworkMode")? != "none" {
        return Err(Error::unknown("container has network access"));
    }
    if host.get("Privileged").and_then(|v| v.as_bool()) != Some(false) {
        return Err(Error::unknown("container is privileged"));
    }
    if host.get("ReadonlyRootfs").and_then(|v| v.as_bool()) != Some(true) {
        return Err(Error::unknown("container rootfs is writable"));
    }
    let cap_add = host
        .get("CapAdd")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::unknown("container CapAdd missing"))?;
    if !cap_add.is_empty() {
        return Err(Error::unknown("container adds capabilities"));
    }
    let cap_drop = host
        .get("CapDrop")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::unknown("container CapDrop missing"))?;
    if cap_drop.len() != 1 || cap_drop[0].as_str() != Some("ALL") {
        return Err(Error::unknown("container does not drop ALL capabilities"));
    }
    if !get_str(host, "PidMode")?.is_empty() {
        return Err(Error::unknown("container shares the host pid namespace"));
    }
    if get_str(host, "IpcMode")? != "private" {
        return Err(Error::unknown("container ipc namespace is not private"));
    }
    if get_str(host, "CgroupnsMode")? != "private" {
        return Err(Error::unknown("container cgroup namespace is not private"));
    }
    let security = host
        .get("SecurityOpt")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::unknown("container SecurityOpt missing"))?;
    let mut nnp = false;
    let mut seccomp = false;
    for entry in security {
        let s = entry.as_str().unwrap_or("");
        if s == "no-new-privileges:true" {
            nnp = true;
        }
        if s.starts_with("seccomp=") {
            seccomp = true;
        }
        if s == "seccomp=unconfined" || s == "apparmor=unconfined" {
            return Err(Error::unknown("container runs unconfined"));
        }
    }
    if !nnp || !seccomp {
        return Err(Error::unknown(
            "container lacks no-new-privileges or the pinned seccomp profile",
        ));
    }
    if expired_log_config(host)? {
        return Err(Error::unknown("container logs are written to host storage"));
    }
    // Mount provenance (the three generated metadata files and the two owned
    // socket binds) is proven by `measure_mounts::verify_owned_mounts` against
    // held descriptors and the parsed guest mount table. Classifying a mount
    // by a source string here would accept tmpfs host binds or the generated
    // `/etc` files by name, so this function deliberately does not do it.
    // Ulimits: core must be 0.
    let ulimits = host
        .get("Ulimits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::unknown("container ulimits missing"))?;
    let mut core_off = false;
    for limit in ulimits {
        if limit.get("Name").and_then(|v| v.as_str()) == Some("core") {
            core_off = limit.get("Soft").and_then(|v| v.as_i64()) == Some(0)
                && limit.get("Hard").and_then(|v| v.as_i64()) == Some(0);
        }
    }
    if !core_off {
        return Err(Error::unknown("container core limit is not zero"));
    }
    Ok(())
}

fn expired_log_config(host: &serde_json::Value) -> Result<bool> {
    let log = host
        .get("LogConfig")
        .ok_or_else(|| Error::unknown("container log config missing"))?;
    Ok(log.get("Type").and_then(|v| v.as_str()) != Some("none"))
}

fn verify_image(
    profile: &LocalProfile,
    inspect: &serde_json::Value,
    docker: &DockerClient,
) -> Result<()> {
    let image_id = inspect
        .get("Image")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::unknown("container image id missing"))?;
    let image = docker.image_inspect(&profile.image().reference)?;
    if image.get("Id").and_then(|v| v.as_str()) != Some(image_id) {
        return Err(Error::unknown(
            "container image id is not the pinned image reference",
        ));
    }
    let digests = image
        .get("RepoDigests")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::unknown("image repo digests missing"))?;
    let wanted = &profile.image().reference;
    if !digests.iter().any(|d| d.as_str() == Some(wanted.as_str())) {
        return Err(Error::unknown(
            "pinned image digest is not the pulled repo digest",
        ));
    }
    Ok(())
}

struct ProcFacts {
    uid: [u32; 2],
    gid: [u32; 2],
    cap_eff: String,
    cap_prm: String,
    cap_inh: String,
    cap_bnd: String,
    no_new_privs: u64,
    seccomp: u64,
    seccomp_filters: u64,
    nspid: Vec<u32>,
    core_dumping: u64,
    argv_digest: String,
    env_digest: String,
}

impl ProcFacts {
    fn read(pid: u32) -> Result<Self> {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .map_err(|_| Error::unknown("guest process status unreadable"))?;
        let mut uid = [0u32; 2];
        let mut gid = [0u32; 2];
        let mut cap_eff = String::new();
        let mut cap_prm = String::new();
        let mut cap_inh = String::new();
        let mut cap_bnd = String::new();
        let mut no_new_privs = u64::MAX;
        let mut seccomp = u64::MAX;
        let mut seccomp_filters = 0u64;
        let mut nspid = Vec::new();
        let mut core_dumping = u64::MAX;
        for line in status.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match name {
                "Uid" => {
                    let parts: Vec<u32> = value
                        .split_whitespace()
                        .filter_map(|v| v.parse().ok())
                        .collect();
                    if parts.len() != 4 {
                        return Err(Error::unknown("guest Uid line malformed"));
                    }
                    uid = [parts[0], parts[1]];
                }
                "Gid" => {
                    let parts: Vec<u32> = value
                        .split_whitespace()
                        .filter_map(|v| v.parse().ok())
                        .collect();
                    if parts.len() != 4 {
                        return Err(Error::unknown("guest Gid line malformed"));
                    }
                    gid = [parts[0], parts[1]];
                }
                "CapEff" => cap_eff = value.to_string(),
                "CapPrm" => cap_prm = value.to_string(),
                "CapInh" => cap_inh = value.to_string(),
                "CapBnd" => cap_bnd = value.to_string(),
                "NoNewPrivs" => {
                    no_new_privs = value
                        .parse()
                        .map_err(|_| Error::unknown("NoNewPrivs malformed"))?
                }
                "Seccomp" => {
                    seccomp = value
                        .parse()
                        .map_err(|_| Error::unknown("Seccomp malformed"))?
                }
                "Seccomp_filters" => {
                    seccomp_filters = value
                        .parse()
                        .map_err(|_| Error::unknown("Seccomp_filters malformed"))?
                }
                "NSpid" => {
                    nspid = value
                        .split_whitespace()
                        .filter_map(|v| v.parse().ok())
                        .collect()
                }
                "CoreDumping" => {
                    core_dumping = value
                        .parse()
                        .map_err(|_| Error::unknown("CoreDumping malformed"))?
                }
                _ => {}
            }
        }
        if cap_eff.is_empty() || cap_prm.is_empty() || cap_inh.is_empty() || cap_bnd.is_empty() {
            return Err(Error::unknown("guest capability sets unreadable"));
        }
        let argv = read_nul_list(&format!("/proc/{pid}/cmdline"))?;
        let env = read_nul_list(&format!("/proc/{pid}/environ"))?;
        Ok(Self {
            uid,
            gid,
            cap_eff,
            cap_prm,
            cap_inh,
            cap_bnd,
            no_new_privs,
            seccomp,
            seccomp_filters,
            nspid,
            core_dumping,
            argv_digest: digest_list(&argv),
            env_digest: digest_list(&env),
        })
    }
}

fn read_nul_list(path: &str) -> Result<Vec<String>> {
    let bytes = std::fs::read(path).map_err(|_| Error::unknown("guest process list unreadable"))?;
    if bytes.len() > 256 * 1024 {
        return Err(Error::unknown("guest process list exceeds the bound"));
    }
    let mut out = Vec::new();
    for part in bytes.split(|b| *b == 0) {
        if part.is_empty() {
            continue;
        }
        let s = std::str::from_utf8(part)
            .map_err(|_| Error::unknown("guest process list is not UTF-8"))?;
        out.push(s.to_string());
    }
    Ok(out)
}

fn digest_list(items: &[String]) -> String {
    let mut material = Vec::new();
    for item in items {
        material.extend_from_slice(item.as_bytes());
        material.push(0);
    }
    super::profile::sha256_digest(&material)
}

fn verify_proc(
    profile: &LocalProfile,
    runtime: &RuntimeSpec,
    pid: u32,
    container_id: &str,
    facts: &ProcFacts,
) -> Result<()> {
    if facts.uid[0] != runtime.os_uid || facts.uid[1] != runtime.os_uid {
        return Err(Error::unknown(
            "guest real/effective uid is not the enrolled uid",
        ));
    }
    if facts.gid[0] != runtime.os_gid || facts.gid[1] != runtime.os_gid {
        return Err(Error::unknown("guest gid is not the enrolled gid"));
    }
    let zero = "0000000000000000";
    if facts.cap_eff != zero
        || facts.cap_prm != zero
        || facts.cap_inh != zero
        || facts.cap_bnd != zero
    {
        return Err(Error::unknown("guest retains capabilities"));
    }
    if facts.no_new_privs != 1 {
        return Err(Error::unknown("guest NoNewPrivs is not 1"));
    }
    if facts.seccomp != 2 || facts.seccomp_filters == 0 {
        return Err(Error::unknown("guest seccomp filter is not active"));
    }
    if facts.core_dumping != 0 {
        return Err(Error::unknown("guest is dumping core"));
    }
    if facts.nspid.last().copied() != Some(1) {
        return Err(Error::unknown(
            "measured process is not PID 1 of a private pid namespace",
        ));
    }
    // Post-exec non-dumpability is not demanded here: the observer applies the
    // seal after Node boots, so this start-time pass proves the unqualified
    // process identity only. `verify_kernel_seal` is the observing-path proof.
    // Core limit 0 (RLIMIT_CORE) — no memory can be written to a core file.
    let limits = std::fs::read_to_string(format!("/proc/{pid}/limits"))
        .map_err(|_| Error::unknown("guest limits unreadable"))?;
    let core_soft = limits
        .lines()
        .find(|l| l.starts_with("Max core file size"))
        .and_then(|l| l.split_whitespace().nth(4))
        .ok_or_else(|| Error::unknown("guest core limit unreadable"))?;
    if core_soft != "0" {
        return Err(Error::unknown("guest core limit is not zero"));
    }
    // Private namespaces: every one of pid/mnt/net/ipc/uts/cgroup must differ
    // from the host's.
    for ns in ["pid", "mnt", "net", "ipc", "uts", "cgroup"] {
        let guest = std::fs::read_link(format!("/proc/{pid}/ns/{ns}"))
            .map_err(|_| Error::unknown("guest namespace link unreadable"))?;
        let host = std::fs::read_link(format!("/proc/self/ns/{ns}"))
            .map_err(|_| Error::unknown("host namespace link unreadable"))?;
        if guest == host {
            return Err(Error::unknown(format!(
                "guest shares the host {ns} namespace"
            )));
        }
    }
    // The process must be in the owned container's cgroup.
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map_err(|_| Error::unknown("guest cgroup unreadable"))?;
    if !cgroup.contains(container_id) {
        return Err(Error::unknown("guest is not in the owned container cgroup"));
    }
    // Executable must hash to the pinned Node binary.
    let exe = peer::exe_digest(pid)?;
    if format!("sha256:{}", peer::hex(&exe)) != profile.runtime().node_digest {
        return Err(Error::unknown("guest executable is not the pinned Node"));
    }
    // Argv must be exactly the pinned Node graph: Docker execs the pinned Node
    // entrypoint with the approved argv, so cmdline[0] is the Node path.
    let mut want_argv = vec![profile.runtime().node_path.clone()];
    want_argv.extend(profile.runtime().node_args.iter().cloned());
    let argv = read_nul_list(&format!("/proc/{pid}/cmdline"))?;
    if argv != want_argv {
        return Err(Error::unknown("guest argv is not the pinned Node graph"));
    }
    // Environment must be exactly the pinned set (no LD_/NODE_OPTIONS).
    let env = read_nul_list(&format!("/proc/{pid}/environ"))?;
    let got: BTreeSet<String> = env.iter().cloned().collect();
    let want: BTreeSet<String> = profile.runtime().env.iter().cloned().collect();
    if got != want || got.len() != env.len() {
        return Err(Error::unknown(
            "guest environment is not the pinned environment",
        ));
    }
    // The full mount proof (the fixed Linux Docker 29 table, the two owned
    // sockets and the three engine metadata files) is performed by
    // `measure_mounts::verify_owned_mounts` with held descriptors and the
    // parsed guest mount table.
    Ok(())
}

/// The actual post-exec kernel seal proof for the measured guest init. A
/// non-dumpable non-root process has `/proc/<pid>/environ` owned by root with no
/// group/other bits, even though the process runs as the enrolled uid. Linux
/// resets `mm->dumpable` on exec, so start-time measurement cannot demand this
/// (the observer applies the seal after Node boots); the observing path calls
/// this before issuing the private observation key.
pub(crate) fn verify_kernel_seal(pid: u32) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(format!("/proc/{pid}/environ"))
        .map_err(|_| Error::unknown("guest environ metadata unreadable"))?;
    if meta.uid() != 0 || meta.mode() & 0o077 != 0 {
        return Err(Error::rejected(
            "guest is dumpable — /proc/<pid>/environ is owned by its uid",
        ));
    }
    Ok(())
}

/// The live installed material must hash to the profile pin. Reads through
/// `/proc/<pid>/root`, so it measures the guest's actual read-only image.
pub(crate) fn verify_material(profile: &LocalProfile, node_pid: u32) -> Result<String> {
    let root = registry::guest_root(node_pid);
    let digest = registry::material_digest(&root, &profile.runtime().material_root)?;
    if digest != profile.runtime().material_digest {
        return Err(Error::rejected(
            "installed runtime material does not match the pinned material digest",
        ));
    }
    // The observer implementation is part of the measured graph.
    let observer = root.join(
        profile
            .runtime()
            .observer_path
            .strip_prefix('/')
            .ok_or_else(|| Error::rejected("observer path must be absolute"))?,
    );
    let bytes = std::fs::read(&observer)
        .map_err(|_| Error::unknown("observer implementation is unreadable"))?;
    if super::profile::sha256_digest(&bytes) != profile.runtime().observer_digest {
        return Err(Error::rejected(
            "observer implementation is not the pinned observer",
        ));
    }
    // The reviewed NAPI seal addon is part of the measured graph: the pinned
    // observer loads exactly this fixed path, so measure the guest's actual
    // read-only image bytes rather than trusting the observer's claim.
    let addon = root.join(
        spec::SEAL_ADDON_PATH
            .strip_prefix('/')
            .ok_or_else(|| Error::internal("seal addon path must be absolute"))?,
    );
    let bytes = std::fs::read(&addon)
        .map_err(|_| Error::unknown("seal addon implementation is unreadable"))?;
    if super::profile::sha256_digest(&bytes) != profile.runtime().seal_digest {
        return Err(Error::rejected(
            "seal addon implementation is not the pinned addon",
        ));
    }
    Ok(digest)
}

/// A cheap liveness re-check used by `recheck`: the exact container is still
/// running with the same measured init and image. It never re-mints authority;
/// the caller uses it to confirm stored readiness is still live.
pub(crate) fn verify_live(
    profile: &LocalProfile,
    runtime: &RuntimeSpec,
    docker: &DockerClient,
    engine: &super::measure_mounts::EngineMountRoot,
    stored_pid: u32,
    stored_starttime: u64,
) -> Result<()> {
    let inspect = docker.inspect(&runtime.container_name)?;
    let running = inspect
        .get("State")
        .and_then(|s| s.get("Running"))
        .and_then(|v| v.as_bool())
        == Some(true);
    if !running {
        return Err(Error::rejected("runtime container is not running"));
    }
    let pid = inspect
        .get("State")
        .and_then(|s| s.get("Pid"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    if pid != stored_pid {
        return Err(Error::rejected("runtime init pid changed"));
    }
    if peer::starttime(pid) != Some(stored_starttime) {
        return Err(Error::rejected("runtime init starttime changed"));
    }
    let image_id = inspect
        .get("Image")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let image = docker.image_inspect(&profile.image().reference)?;
    if image.get("Id").and_then(|v| v.as_str()) != Some(image_id) {
        return Err(Error::rejected("runtime image changed"));
    }
    // Live admission re-runs the same config and mount proof from a fresh
    // inspect, the held engine root and freshly read metadata bytes.
    verify_config(profile, runtime, &inspect)?;
    super::measure_mounts::verify_owned_mounts(
        engine,
        profile,
        runtime,
        &inspect,
        stored_pid,
        stored_starttime,
    )?;
    Ok(())
}

/// Path helper used by tests/fixtures.
pub(crate) fn proc_path(pid: u32) -> String {
    format!("/proc/{pid}")
}
