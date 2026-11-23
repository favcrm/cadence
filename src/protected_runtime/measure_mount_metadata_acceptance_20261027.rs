//! Independent real-engine CAD-1085 metadata-mount acceptance.
//!
//! Register as a Linux cfg(test) child of protected_runtime::measure_mounts.
//! Explicit invocation after manager provisioning, never against a host daemon:
//!
//! sudo env CADENCE_MOUNT_ACCEPTANCE_ISOLATED=1 \
//!   CADENCE_MOUNT_ACCEPTANCE_IMAGE='<actual-repository>@sha256:<manifest>' \
//!   CADENCE_MOUNT_ACCEPTANCE_NODE='<absolute Node path in approved image>' \
//!   CADENCE_MOUNT_ACCEPTANCE_SECCOMP=/srv/cad1085-native14-57439/profile/seccomp.json \
//!   <test-binary> --ignored --exact \
//!   protected_runtime::measure_mounts::measure_mount_metadata_acceptance_20261027::cad1085_metadata_mounts_actual_kernel \
//!   --test-threads=1
//!
//! The approved manager-owned isolated Docker29 engine is fixed below. Its
//! session must be root0700 with root-owned non-writable ancestors. The leaf
//! never starts/configures a daemon, builds/pulls images, contacts a registry,
//! uses a default Docker socket, or operates on any pre-existing container.
//! The approved image must already have a real resolvable RepoDigest and the
//! exact immutable config ID below, Node supporting --disable-sigusr1, and
//! ordinary /etc/passwd and /mnt targets. Only fixed idle Node code is run.
//! The actual root-owned q14 policy is mandatory, has an exact raw-byte digest,
//! and is copied unchanged; no alternate or allow-all policy is substituted.
//!
//! Two real Docker layouts are exercised. The exact-production lane sends
//! spec::create_body bytes unchanged, retaining NetworkDisabled=true and
//! NetworkMode=none, and proves empty pinned-image /etc files on the read-only
//! root mount with no metadata binds. The separately approved metadata-only
//! fixture lane changes only NetworkDisabled=false while retaining actual
//! NetworkMode=none and every other producer setting, so Docker itself creates
//! all three metadata resources. Its real inspect and kernel objects are used
//! unchanged for positive production proof and the existing security faults.
//!
//! Requires root with CAP_SYS_ADMIN, Python3 at /usr/bin/python3, Linux x86_64,
//! readable host /proc, pidfd_open, open_tree, mount_setattr and move_mount.
//! Missing prerequisites fail explicit invocation, never silently skip.
//! Every fault uses fresh random exact-owned container names and listeners.
//! Cleanup is exact-ID/name/nonce/label scoped; the manager's registry and all
//! other containers are untouched. Only fresh /run/c1085-<nonce> artifacts are
//! created by this leaf outside the isolated daemon's own data directory.

use std::ffi::CString;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::super::docker::DockerClient;
use super::super::measure_mounts::{verify_owned_mounts, EngineMountRoot};
use super::super::peer;
use super::super::profile::{sha256_digest, LocalProfile};
use super::super::spec::{self, RuntimeSpec};

const SOCKET: &str = "/srv/cad1085-mount-acceptance-engine-57439/overlay-docker.sock";
const ENGINE_ROOT: &str = "/srv/cad1085-mount-acceptance-engine-57439/overlay-data";
const IMAGE_ID: &str = "sha256:d19a99adbb855e946699436aab41337ca49725c86202a8b3f03cac1883228f2a";
const SECCOMP_SOURCE: &str = "/srv/cad1085-native14-57439/profile/seccomp.json";
const SECCOMP_DIGEST: &str =
    "sha256:6c4922890583fba4ab9746d2cfd5b9ce5979b9e13a59836a6cf8792e7ebc2a18";
const UID: u32 = 59001;
const GID: u32 = 59001;

fn approved_seccomp() -> Vec<u8> {
    const MAX_BYTES: u64 = 256 * 1024;
    let configured = std::env::var("CADENCE_MOUNT_ACCEPTANCE_SECCOMP")
        .expect("explicit actual approved q14 seccomp path required");
    assert!(
        configured == SECCOMP_SOURCE,
        "only the explicitly approved q14 document may be selected"
    );
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .expect("open held trusted policy root");
    let root_meta = root.metadata().expect("trusted policy root metadata");
    assert!(
        root_meta.is_dir() && root_meta.uid() == 0 && root_meta.mode() & 0o022 == 0,
        "trusted policy root must be root-owned and non-writable"
    );
    let mut held = vec![root];
    let parts: Vec<_> = SECCOMP_SOURCE.split('/').skip(1).collect();
    for (index, part) in parts.iter().enumerate() {
        assert!(
            !part.is_empty() && *part != "." && *part != "..",
            "canonical approved policy path required"
        );
        let name = CString::new(*part).expect("approved policy component");
        let last = index + 1 == parts.len();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if last { 0 } else { libc::O_DIRECTORY };
        let fd = unsafe {
            libc::openat(
                held.last().expect("held policy ancestor").as_raw_fd(),
                name.as_ptr(),
                flags,
            )
        };
        assert!(
            fd >= 0,
            "approved policy source or no-symlink proof unavailable"
        );
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file.metadata().expect("held approved policy metadata");
        assert!(
            meta.uid() == 0 && meta.mode() & 0o022 == 0,
            "approved policy source and ancestors must be root-owned and non-writable"
        );
        assert!(
            if last { meta.is_file() } else { meta.is_dir() },
            "approved policy source type refused"
        );
        held.push(file);
    }
    let source = held.last_mut().expect("held actual policy document");
    let before = source.metadata().expect("policy before-read metadata");
    assert!(
        before.len() > 0 && before.len() <= MAX_BYTES,
        "approved policy size bound"
    );
    let mut bytes = Vec::new();
    Read::by_ref(source)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .expect("bounded actual approved policy read");
    let after = source.metadata().expect("policy after-read metadata");
    let key = |m: &fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mode(),
            m.uid(),
            m.gid(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    assert!(
        key(&before) == key(&after)
            && bytes.len() as u64 == before.len()
            && bytes.len() as u64 <= MAX_BYTES,
        "approved policy changed during bounded read"
    );
    assert!(
        sha256_digest(&bytes) == SECCOMP_DIGEST,
        "actual approved q14 raw-byte digest required"
    );
    bytes
}

fn random_nonce() -> String {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .expect("fixture nonce source")
        .read_exact(&mut bytes)
        .expect("complete fixture nonce");
    let mut value = String::with_capacity(32);
    for byte in bytes {
        write!(&mut value, "{byte:02x}").expect("format fixture nonce");
    }
    value
}

fn runtime_uuid(nonce: &str) -> String {
    format!(
        "{}-{}-4{}-8{}-{}",
        &nonce[..8],
        &nonce[8..12],
        &nonce[13..16],
        &nonce[17..20],
        &nonce[20..]
    )
}

fn create_owned_dir(path: &Path) {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .expect("create exact fresh root-owned directory");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("seal root-owned directory");
}

struct OwnedDir {
    path: PathBuf,
    retain: bool,
}

impl OwnedDir {
    fn create(nonce: &str) -> Self {
        // Short enough for the unmodified production runtimes/<uuid>/socket
        // layout under Linux sun_path. Never reuse an existing directory.
        let path = Path::new("/run").join(format!("c1085-{nonce}"));
        create_owned_dir(&path);
        Self {
            path,
            retain: false,
        }
    }
}

impl Drop for OwnedDir {
    fn drop(&mut self) {
        if self.retain {
            return;
        }
        if fs::remove_dir_all(&self.path).is_err() {
            if thread::panicking() {
                eprintln!("CAD-1085: exact owned run directory cleanup failed");
            } else {
                panic!("exact owned run directory cleanup failed");
            }
        }
    }
}

fn write_owned(path: &Path, bytes: &[u8], mode: u32) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .expect("create exact new fixture file");
    file.write_all(bytes).expect("write fixture file");
    file.set_permissions(fs::Permissions::from_mode(mode))
        .expect("seal fixture file mode");
    file.sync_all().expect("persist fixture file");
}

fn listening_socket(path: &Path) -> UnixListener {
    assert!(
        path.as_os_str().as_bytes().len() < 108,
        "production fixture socket path must fit sun_path"
    );
    let listener = UnixListener::bind(path).expect("bind real fixture listener");
    let cpath = CString::new(path.as_os_str().as_bytes()).expect("fixture socket path");
    assert_eq!(unsafe { libc::chown(cpath.as_ptr(), 0, GID) }, 0);
    fs::set_permissions(path, fs::Permissions::from_mode(0o660)).expect("seal fixture socket mode");
    assert!(fs::symlink_metadata(path)
        .expect("fixture socket metadata")
        .file_type()
        .is_socket());
    listener
}

fn fixture_profile(dir: &Path, image: &str, node: &str, seccomp_bytes: &[u8]) -> LocalProfile {
    // Public fixed bytes, not usable credentials and never consumed by any
    // signer/installer. The actual root-profile loader stats these pins.
    // Unrelated runtime pins do not claim full Node admission: this leaf
    // exercises the actual complete mount proof with genuine kernel objects.
    let nonsecret_pin = dir.join("nonsecret-stat-only-pin");
    write_owned(
        &nonsecret_pin,
        b"CAD-1085 public nonsecret test pin\n",
        0o600,
    );
    let installer = dir.join("never-executed-installer-pin");
    write_owned(
        &installer,
        b"CAD-1085 never-executed public test data\n",
        0o600,
    );
    assert!(
        sha256_digest(seccomp_bytes) == SECCOMP_DIGEST,
        "only verified actual q14 policy bytes may be copied"
    );
    let seccomp = dir.join("seccomp.json");
    write_owned(&seccomp, seccomp_bytes, 0o600);
    let digest = format!("sha256:{}", "1".repeat(64));
    let document = json!({
        "v": 1,
        "deployment_class": "native-oci-agent/1",
        "workspace_id": "cad1085-metadata-acceptance",
        "core_id": "cad1085-metadata-acceptance",
        "lineage_reference": "cad1085-metadata-acceptance",
        "database_epoch": 1,
        "owner": {
            "exe_digest": digest,
            "public_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "private_key_file": nonsecret_pin
        },
        "supervisor": {"uid": 20001, "gid": 20001, "exe_digest": digest},
        "installer": {
            "exe_digest": digest,
            "generation": "2".repeat(32),
            "path": installer,
            "grant_issuer": {"kid": "fixture-public-pin", "private_key_file": nonsecret_pin}
        },
        "issuer_keys": [{"kid": "fixture-public-pin", "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}],
        "image": {
            "ref": image,
            "pins": {
                "source": "1".repeat(40), "helper": "1".repeat(64),
                "node": "1".repeat(64), "piGraph": "1".repeat(64), "policy": "1".repeat(64)
            }
        },
        "runtime": {
            "node_path": node,
            "observer_path": "/opt/cadence/acceptance/unused-observer.mjs",
            "seal_digest": digest, "node_digest": digest,
            "observer_digest": digest, "pi_digest": digest,
            "material_root": "/opt/cadence/acceptance", "material_digest": digest,
            "env": ["PATH=/usr/local/bin:/usr/bin:/bin"],
            "node_args": [
                "--disable-sigusr1", "--no-warnings", "--input-type=module",
                "-e", "setInterval(() => {}, 1000)"
            ],
            "tmpfs_bytes": 33554432
        },
        "seccomp": {"path": seccomp, "digest": sha256_digest(seccomp_bytes)},
        "uids": {"start": UID, "end": UID},
        "operator_origin": "https://metadata-acceptance.invalid",
        "docker": {"socket": SOCKET},
        "registry": {
            "manifest_id": "metadata-acceptance", "manifest_revision": 1,
            "policy_digest": digest, "registry_digest": digest,
            "approved": [{"name": "fixture", "schema_digest": digest, "implementation_digest": digest}]
        },
        "paths": {
            "state_dir": dir.join("unused-state"), "run_dir": dir,
            "control_socket": dir.join("unused-control.sock")
        },
        "max_runtimes": 1
    });
    let path = dir.join("profile.json");
    write_owned(
        &path,
        &serde_json::to_vec(&document).expect("encode root fixture profile"),
        0o600,
    );
    LocalProfile::load_from(&path)
        .expect("actual root-owned profile loader and actual q14 policy pin")
}

fn helper(input: Value) -> Value {
    let mut child = Command::new("/usr/bin/python3")
        .args(["-I", "-c", KERNEL_HELPER])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start finite isolated fixture helper");
    let payload = serde_json::to_vec(&input).expect("encode exact owned helper input");
    child
        .stdin
        .take()
        .expect("helper input")
        .write_all(&payload)
        .expect("send exact owned helper input");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child
            .try_wait()
            .expect("observe helper completion")
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("finite fixture helper exceeded deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child
        .wait_with_output()
        .expect("reap finite fixture helper");
    // Never print payloads, metadata bytes, or unexpected exception strings.
    assert!(
        output.status.success(),
        "isolated fixture helper refused; prerequisite or fault setup unavailable"
    );
    assert!(output.stdout.len() <= 4096, "fixture helper output bound");
    serde_json::from_slice(&output.stdout).expect("bounded fixture helper result")
}

struct Harness {
    image: String,
    node: String,
    seccomp: Vec<u8>,
}

impl Harness {
    fn new() -> Self {
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "explicit root invocation required"
        );
        assert_eq!(unsafe { libc::getegid() }, 0, "root fixture group required");
        assert!(
            std::env::var("CADENCE_MOUNT_ACCEPTANCE_ISOLATED").as_deref() == Ok("1"),
            "explicit isolated Docker approval required"
        );
        let seccomp = approved_seccomp();
        let info = helper(json!({"op": "info"}));
        assert!(
            info["root"].as_str() == Some(ENGINE_ROOT),
            "actual engine root must be the approved isolated root"
        );
        assert!(
            info["version"]
                .as_str()
                .is_some_and(|v| v.starts_with("29.")),
            "Docker 29 required"
        );
        let image = std::env::var("CADENCE_MOUNT_ACCEPTANCE_IMAGE")
            .expect("actual preloaded immutable repository digest required");
        let (_, digest) = image
            .rsplit_once("@sha256:")
            .expect("repository manifest digest required");
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "lowercase immutable manifest digest required"
        );
        let docker = DockerClient::new(Path::new(SOCKET));
        let actual = docker
            .image_inspect(&image)
            .expect("preloaded digest must resolve; never pull");
        assert!(
            actual["Id"].as_str() == Some(IMAGE_ID),
            "only the exact manager-approved config image may run"
        );
        assert!(
            actual["RepoDigests"]
                .as_array()
                .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(image.as_str()))),
            "actual exact repository digest association required; never fabricate one"
        );
        let node = std::env::var("CADENCE_MOUNT_ACCEPTANCE_NODE")
            .expect("actual approved-image Node path required");
        assert!(
            Path::new(&node).is_absolute(),
            "absolute fixture Node path required"
        );
        Self {
            image,
            node,
            seccomp,
        }
    }
}

fn actual_mount_id(file: &File) -> u64 {
    let mut raw = std::mem::MaybeUninit::<libc::statx>::zeroed();
    let rc = unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            raw.as_mut_ptr(),
        )
    };
    assert!(rc == 0, "actual descriptor mount-ID proof unavailable");
    let value = unsafe { raw.assume_init() };
    assert!(
        value.stx_mask & libc::STATX_MNT_ID != 0,
        "actual descriptor mount-ID mask unavailable"
    );
    value.stx_mnt_id
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataLayout {
    ProducerWithoutMetadata,
    EngineGeneratedMetadata,
}

struct Fixture {
    dir: OwnedDir,
    _ops: UnixListener,
    _attest: UnixListener,
    docker: DockerClient,
    engine: EngineMountRoot,
    profile: LocalProfile,
    runtime: RuntimeSpec,
    layout: MetadataLayout,
    id: Option<String>,
    pid: u32,
    starttime: u64,
}

impl Fixture {
    fn new(h: &Harness) -> Self {
        Self::create(h, MetadataLayout::EngineGeneratedMetadata)
    }

    fn exact_producer(h: &Harness) -> Self {
        Self::create(h, MetadataLayout::ProducerWithoutMetadata)
    }

    fn create(h: &Harness, layout: MetadataLayout) -> Self {
        let nonce = random_nonce();
        let dir = OwnedDir::create(&nonce);
        let runtime_id = runtime_uuid(&nonce);
        let runtimes = dir.path.join("runtimes");
        create_owned_dir(&runtimes);
        let socket_dir = runtimes.join(&runtime_id);
        create_owned_dir(&socket_dir);
        let ops_socket = socket_dir.join("ops.sock");
        let attest_socket = socket_dir.join("attest.sock");
        let ops = listening_socket(&ops_socket);
        let attest = listening_socket(&attest_socket);
        let profile = fixture_profile(&dir.path, &h.image, &h.node, &h.seccomp);
        let runtime = RuntimeSpec {
            runtime_id,
            generation: 1,
            owner_epoch: 1,
            nonce: nonce.clone(),
            os_uid: UID,
            os_gid: GID,
            container_name: format!("cad1085-metadata-acceptance-{nonce}"),
            ops_socket,
            attest_socket,
        };
        let docker = DockerClient::new(Path::new(SOCKET));
        let engine = EngineMountRoot::from_docker(&docker)
            .expect("actual fixed-API production engine-root proof");
        let producer_body =
            spec::create_body(&profile, &runtime).expect("actual production create body");
        let producer_value: Value =
            serde_json::from_slice(&producer_body).expect("production create JSON");
        assert!(
            producer_value["NetworkDisabled"] == json!(true),
            "production producer must retain NetworkDisabled=true"
        );
        assert!(
            producer_value["Image"].as_str() == Some(h.image.as_str()),
            "unmodified immutable producer image"
        );
        assert!(
            producer_value["Hostname"] == json!("cadence-protected"),
            "producer hostname contract"
        );
        assert!(
            producer_value["HostConfig"]["Dns"] == json!(["127.0.0.1"]),
            "producer DNS contract"
        );
        assert!(
            producer_value["HostConfig"]["DnsSearch"] == json!(["."]),
            "producer DNS search contract"
        );
        assert!(
            producer_value["HostConfig"]["DnsOptions"] == json!(["ndots:0"]),
            "producer DNS option contract"
        );
        assert!(
            producer_value["User"] == json!(format!("{UID}:{GID}")),
            "nonroot producer identity"
        );
        assert!(
            producer_value["HostConfig"]["NetworkMode"] == json!("none"),
            "no-network fixture"
        );
        assert!(
            producer_value["HostConfig"]["ReadonlyRootfs"] == json!(true),
            "read-only fixture root"
        );
        assert!(
            producer_value["HostConfig"]["Privileged"] == json!(false),
            "never privileged fixture"
        );
        assert!(
            producer_value["HostConfig"]["CapDrop"] == json!(["ALL"]),
            "drop all guest capabilities"
        );
        let body = match layout {
            MetadataLayout::ProducerWithoutMetadata => producer_body,
            MetadataLayout::EngineGeneratedMetadata => {
                // Explicitly approved fixture-only difference. Docker itself
                // generates all three resources; positive inspect is never
                // rewritten, and production create_body remains unchanged.
                let mut fixture_value = producer_value.clone();
                fixture_value["NetworkDisabled"] = json!(false);
                let mut restored = fixture_value.clone();
                restored["NetworkDisabled"] = json!(true);
                assert!(
                    restored == producer_value,
                    "metadata fixture may change only NetworkDisabled"
                );
                assert!(
                    fixture_value["HostConfig"]["NetworkMode"] == json!("none"),
                    "metadata fixture must retain actual network none"
                );
                serde_json::to_vec(&fixture_value)
                    .expect("encode explicitly approved metadata fixture")
            }
        };
        let mut f = Self {
            dir,
            _ops: ops,
            _attest: attest,
            docker,
            engine,
            profile,
            runtime,
            layout,
            id: None,
            pid: 0,
            starttime: 0,
        };
        match f.docker.create(&f.runtime.container_name, &body) {
            Ok(created) => f.id = Some(created.id),
            Err(_) => {
                // Reconcile only this exact random name, never retry create.
                if let Ok(inspect) = f.docker.inspect(&f.runtime.container_name) {
                    if f.owned(&inspect) {
                        f.id = inspect["Id"].as_str().map(str::to_string);
                    }
                }
                if f.id.is_none() {
                    f.dir.retain = true;
                }
                panic!("fresh create outcome unavailable; no create retry performed");
            }
        }
        f.docker
            .start(f.id())
            .expect("start exact fresh owned container once");
        let inspect = f.docker.inspect(f.id()).expect("fresh live Docker inspect");
        assert!(f.owned(&inspect), "fresh owner identity");
        assert!(
            inspect["State"]["Running"] == json!(true),
            "fixture process must be live"
        );
        f.pid = u32::try_from(inspect["State"]["Pid"].as_u64().expect("actual live PID"))
            .expect("bounded actual live PID");
        assert!(f.pid > 1, "never mutate PID 1");
        f.starttime = peer::starttime(f.pid).expect("actual guest init starttime");
        assert!(f.starttime > 0);
        f.assert_layout();
        f.prove();
        if layout == MetadataLayout::EngineGeneratedMetadata {
            let bytes = f.resolver_bytes();
            write_owned(&f.dir.path.join("same-content"), &bytes, 0o644);
        }
        f
    }

    fn assert_layout(&self) {
        let inspect = self.current();
        let disabled = match inspect["Config"].get("NetworkDisabled") {
            // Docker omits false-valued optional Config fields in inspect.
            None => false,
            Some(Value::Bool(value)) => *value,
            _ => panic!("actual NetworkDisabled inspect type unavailable"),
        };
        assert!(
            disabled == (self.layout == MetadataLayout::ProducerWithoutMetadata),
            "actual inspected network-disabled flag must match the selected real fixture"
        );
        assert!(
            inspect["HostConfig"]["NetworkMode"] == json!("none"),
            "both actual fixture lanes must remain network none"
        );
        assert!(
            inspect["HostConfig"]["ReadonlyRootfs"] == json!(true)
                && inspect["HostConfig"]["Privileged"] == json!(false)
                && inspect["HostConfig"]["CapDrop"] == json!(["ALL"]),
            "both actual fixture lanes must remain read-only and unprivileged"
        );
        for (field, name, target) in [
            ("ResolvConfPath", "resolv.conf", "/etc/resolv.conf"),
            ("HostnamePath", "hostname", "/etc/hostname"),
            ("HostsPath", "hosts", "/etc/hosts"),
        ] {
            match self.layout {
                MetadataLayout::ProducerWithoutMetadata => {
                    assert!(
                        inspect[field].as_str() == Some(""),
                        "exact producer must actually have empty metadata source fields"
                    );
                    assert!(
                        self.records(target).is_empty(),
                        "exact producer must actually have no metadata target mounts"
                    );
                }
                MetadataLayout::EngineGeneratedMetadata => {
                    let expected = Path::new(ENGINE_ROOT)
                        .join("containers")
                        .join(self.id())
                        .join(name);
                    assert!(
                        inspect[field].as_str() == expected.to_str(),
                        "metadata fixture must actually use exact engine-generated sources"
                    );
                    assert!(
                        self.records(target).len() == 1,
                        "metadata fixture must actually have one engine mount per target"
                    );
                }
            }
        }
        if self.layout == MetadataLayout::ProducerWithoutMetadata {
            self.assert_empty_rootfs_metadata();
        }
        let _ = self.current();
    }

    fn assert_empty_rootfs_metadata(&self) {
        let _ = self.current();
        // /proc/<owned pid>/root is an intentional kernel process-root link;
        // below that held root, every image-path component is opened nofollow.
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(format!("/proc/{}/root", self.pid))
            .expect("hold exact owned guest root");
        let etc_fd = unsafe {
            libc::openat(
                root.as_raw_fd(),
                c"etc".as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        assert!(etc_fd >= 0, "held nofollow image etc directory required");
        let etc = unsafe { File::from_raw_fd(etc_fd) };
        for directory in [&root, &etc] {
            let meta = directory.metadata().expect("held image directory metadata");
            assert!(
                meta.is_dir() && meta.uid() == 0 && meta.mode() & 0o022 == 0,
                "image metadata ancestors must be root-owned and non-writable"
            );
        }
        let root_id = actual_mount_id(&root);
        let mountinfo = fs::read_to_string(format!("/proc/{}/mountinfo", self.pid))
            .expect("actual zero-lane guest mountinfo");
        let mut root_records = 0;
        for line in mountinfo.lines() {
            let (left, _) = line.split_once(" - ").expect("actual mountinfo separator");
            let fields: Vec<_> = left.split_whitespace().collect();
            assert!(fields.len() >= 6, "complete actual root mount record");
            if fields[4] == "/" {
                root_records += 1;
                assert!(
                    fields[0].parse::<u64>().expect("actual root mount ID") == root_id
                        && fields[5].split(',').any(|flag| flag == "ro"),
                    "held guest root must correlate to the exact read-only root mount"
                );
            }
        }
        assert!(
            root_records == 1,
            "one unstacked actual guest root mount required"
        );
        let stable = |m: &fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mode(),
                m.uid(),
                m.gid(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        for name in ["resolv.conf", "hostname", "hosts"] {
            let component = CString::new(name).expect("fixed image metadata filename");
            let fd = unsafe {
                libc::openat(
                    etc.as_raw_fd(),
                    component.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            assert!(
                fd >= 0,
                "actual nofollow pinned-image metadata descriptor required"
            );
            let mut file = unsafe { File::from_raw_fd(fd) };
            let before = file
                .metadata()
                .expect("actual image file before-read metadata");
            assert!(
                before.is_file()
                    && before.uid() == 0
                    && before.mode() & 0o022 == 0
                    && before.len() == 0,
                "zero lane requires actual root-owned empty regular image files"
            );
            assert!(
                actual_mount_id(&file) == root_id,
                "zero-lane image files must belong to the held read-only root mount"
            );
            let mut byte = [0u8; 1];
            assert!(
                file.read(&mut byte)
                    .expect("bounded actual empty image-file read")
                    == 0,
                "zero-lane image files must actually contain no bytes"
            );
            let after = file
                .metadata()
                .expect("actual image file after-read metadata");
            assert!(
                stable(&before) == stable(&after),
                "held image file changed during proof"
            );
        }
        let _ = self.current();
    }

    fn id(&self) -> &str {
        self.id
            .as_deref()
            .expect("successfully created exact owned ID")
    }

    fn owned(&self, inspect: &Value) -> bool {
        let Some(id) = inspect["Id"].as_str() else {
            return false;
        };
        let name = format!("/{}", self.runtime.container_name);
        id.len() == 64
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && self.id.as_deref().is_none_or(|expected| expected == id)
            && inspect["Name"].as_str() == Some(name.as_str())
            && spec::labels(&self.profile, &self.runtime)
                .iter()
                .all(|(key, value)| {
                    inspect["Config"]["Labels"][key].as_str() == Some(value.as_str())
                })
    }

    fn current(&self) -> Value {
        let inspect = self
            .docker
            .inspect(self.id())
            .expect("fresh actual owned inspect");
        assert!(
            self.owned(&inspect),
            "owned inspect identity must remain exact"
        );
        assert!(
            inspect["State"]["Running"] == json!(true),
            "owned init must remain live"
        );
        assert!(
            inspect["State"]["Pid"].as_u64() == Some(u64::from(self.pid)),
            "owned PID must remain exact"
        );
        assert!(
            peer::starttime(self.pid) == Some(self.starttime),
            "owned starttime must remain exact"
        );
        inspect
    }

    fn prove(&self) {
        let inspect = self.current();
        if let Err(error) = verify_owned_mounts(
            &self.engine,
            &self.profile,
            &self.runtime,
            &inspect,
            self.pid,
            self.starttime,
        ) {
            let message = error.to_string();
            let category = match message.as_str() {
                "mount provenance refused" => "mount provenance refused",
                "mount identity refused" => "mount identity refused",
                "mount writable refused" => "mount writable refused",
                "mount unexpected refused" => "mount unexpected refused",
                "mount stacked refused" => "mount stacked refused",
                "mount content refused" => "mount content refused",
                "mount stale process refused" => "mount stale process refused",
                "mount proof unavailable" => "mount proof unavailable",
                _ => "unrecognized non-public proof error",
            };
            panic!("complete actual production mount proof must succeed: {category}");
        }
        let _ = self.current();
    }

    fn refuse(&self, inspect: &Value, starttime: u64, category: &str) {
        let _ = self.current();
        match verify_owned_mounts(
            &self.engine,
            &self.profile,
            &self.runtime,
            inspect,
            self.pid,
            starttime,
        ) {
            Ok(()) => panic!("actual production proof accepted the injected security fault"),
            Err(error) => assert!(
                error.to_string() == category,
                "expected exact public refusal category: {category}"
            ),
        }
        let _ = self.current();
    }

    fn source(&self) -> PathBuf {
        Path::new(ENGINE_ROOT)
            .join("containers")
            .join(self.id())
            .join("resolv.conf")
    }

    fn guest_resolver(&self) -> PathBuf {
        PathBuf::from(format!("/proc/{}/root/etc/resolv.conf", self.pid))
    }

    fn resolver_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        File::open(self.source())
            .expect("exact owned engine resolver")
            .take(8193)
            .read_to_end(&mut bytes)
            .expect("bounded owned resolver read");
        assert!(bytes.len() <= 8192, "fixture resolver read bound");
        bytes
    }

    fn identity(&self, path: &Path) -> (u64, u64, u32) {
        let m = fs::metadata(path).expect("actual fixture file identity");
        (m.dev(), m.ino(), m.mode() & libc::S_IFMT)
    }

    fn records(&self, target: &str) -> Vec<(String, String)> {
        let text = fs::read_to_string(format!("/proc/{}/mountinfo", self.pid))
            .expect("actual guest mountinfo for fault establishment");
        let mut records = Vec::new();
        for line in text.lines() {
            let (left, right) = line.split_once(" - ").expect("kernel mountinfo separator");
            let fields: Vec<_> = left.split_whitespace().collect();
            let fs_fields: Vec<_> = right.split_whitespace().collect();
            assert!(
                fields.len() >= 6 && fs_fields.len() >= 3,
                "complete kernel mountinfo record"
            );
            if fields[4] == target {
                records.push((fields[5].to_string(), fs_fields[1].to_string()));
            }
        }
        records
    }

    fn mutate(&self, operation: &str) {
        assert!(
            self.layout == MetadataLayout::EngineGeneratedMetadata,
            "metadata kernel faults require the genuine engine-generated fixture lane"
        );
        let _ = self.current();
        let result = helper(json!({
            "op": operation, "case": self.dir.path, "nonce": self.runtime.nonce,
            "id": self.id(), "name": self.runtime.container_name,
            "labels": spec::labels(&self.profile, &self.runtime),
            "pid": self.pid, "starttime": self.starttime
        }));
        assert!(
            result["ok"] == json!(true),
            "finite exact-owned namespace mutation must complete"
        );
        let _ = self.current();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let Some(id) = self.id.as_deref() else {
            return;
        };
        let cleaned = (|| {
            let inspect = self.docker.inspect(id).ok()?;
            if !self.owned(&inspect) {
                return None;
            }
            if inspect["State"]["Running"] == json!(true) {
                // Reconcile a lost stop acknowledgement with fresh state,
                // never blindly retry the operation.
                let _ = self.docker.stop(id);
            }
            let stopped = self.docker.inspect(id).ok()?;
            if !self.owned(&stopped) || stopped["State"]["Running"] != json!(false) {
                return None;
            }
            self.docker.remove(id).ok()?;
            Some(())
        })()
        .is_some();
        if !cleaned {
            self.dir.retain = true;
            if thread::panicking() {
                eprintln!("CAD-1085: exact owned fixture cleanup failed; owned artifacts retained");
            } else {
                panic!("exact owned fixture cleanup failed; owned artifacts retained");
            }
        }
    }
}

#[test]
#[ignore = "explicit root/CAP_SYS_ADMIN plus approved isolated Docker29, immutable image, and actual pinned q14 policy required"]
fn cad1085_metadata_mounts_actual_kernel() {
    let harness = Harness::new();
    {
        // Genuine production launch shape: exact original create_body bytes,
        // no generated /etc sources or mounts, and actual empty rootfs files.
        let producer = Fixture::exact_producer(&harness);
        producer.assert_layout();
        producer.prove();
    }
    for fault in [
        "positive",
        "forged-source",
        "forged-id",
        "wrong-inode",
        "writable",
        "extra-host",
        "extra-tmpfs",
        "stacked",
        "directive",
        "comment",
        "stale",
    ] {
        let f = Fixture::new(&harness);
        let before_source = f.identity(&f.source());
        let before_guest = f.identity(&f.guest_resolver());
        assert!(
            before_source == before_guest,
            "positive source/guest inode proof"
        );
        assert!(
            f.records("/etc/resolv.conf").len() == 1,
            "unstacked positive target"
        );
        match fault {
            "positive" => f.prove(),
            "forged-source" => {
                let mut inspect = f.current();
                inspect["ResolvConfPath"] = json!(f.dir.path.join("same-content"));
                f.refuse(&inspect, f.starttime, "mount provenance refused");
            }
            "forged-id" => {
                // A real different freshly owned ID has real metadata files:
                // this must reject provenance, not merely fail opening a
                // nonexistent resource directory for an invented ID.
                let other = Fixture::new(&harness);
                assert!(f.id() != other.id(), "distinct actual fresh owned IDs");
                let mut inspect = f.current();
                inspect["Id"] = json!(other.id());
                f.refuse(&inspect, f.starttime, "mount provenance refused");
            }
            "stale" => f.refuse(
                &f.current(),
                f.starttime.checked_add(1).expect("distinct starttime"),
                "mount stale process refused",
            ),
            operation => {
                // New fixture for each kernel fault, after complete success.
                f.mutate(operation);
                let after_source = f.identity(&f.source());
                let after_guest = f.identity(&f.guest_resolver());
                let category = match operation {
                    "wrong-inode" => {
                        assert!(
                            after_source == before_source,
                            "engine source inode unchanged"
                        );
                        assert!(
                            after_guest != before_guest,
                            "actual wrong guest inode required"
                        );
                        let mut guest_bytes = Vec::new();
                        File::open(f.guest_resolver())
                            .expect("actual replaced resolver")
                            .take(8193)
                            .read_to_end(&mut guest_bytes)
                            .expect("bounded replaced resolver read");
                        assert!(
                            guest_bytes == f.resolver_bytes(),
                            "wrong inode must have identical bytes"
                        );
                        assert!(
                            f.records("/etc/resolv.conf").len() == 1,
                            "replacement must not merely stack"
                        );
                        // The exhaustive mount table rejects this substituted
                        // bind before the later per-target inode comparison.
                        "mount unexpected refused"
                    }
                    "writable" => {
                        assert!(
                            after_source == before_source && after_guest == before_guest,
                            "writable fault must retain identical correct inode"
                        );
                        let records = f.records("/etc/resolv.conf");
                        assert!(
                            records.len() == 1 && records[0].0.split(',').any(|v| v == "rw"),
                            "exact approved mount itself must be writable, not a replacement stack"
                        );
                        "mount writable refused"
                    }
                    "extra-host" => {
                        assert!(
                            f.records("/etc/passwd").len() == 1,
                            "actual extra host bind required"
                        );
                        "mount unexpected refused"
                    }
                    "extra-tmpfs" => {
                        let records = f.records("/mnt");
                        assert!(
                            records.len() == 1 && records[0].1 == "cad1085-unapproved-nonabsolute",
                            "actual nonabsolute source required"
                        );
                        "mount unexpected refused"
                    }
                    "stacked" => {
                        assert!(
                            after_source == before_source && after_guest == before_guest,
                            "stacked target must retain correct visible inode"
                        );
                        assert!(
                            f.records("/etc/resolv.conf").len() >= 2,
                            "actual duplicate/stacked target required"
                        );
                        "mount stacked refused"
                    }
                    "directive" | "comment" => {
                        assert!(
                            after_source == before_source && after_guest == before_guest,
                            "in-place content fault must retain identical source/guest inode"
                        );
                        let bytes = f.resolver_bytes();
                        let suffix: &[u8] = if operation == "directive" {
                            b"\nsearch cad1085-unapproved.invalid\n"
                        } else {
                            b"\n# cad1085-unrecognized-acceptance-comment\n"
                        };
                        assert!(
                            bytes.ends_with(suffix),
                            "actual in-place unapproved bytes required"
                        );
                        "mount content refused"
                    }
                    _ => unreachable!("closed acceptance fault list"),
                };
                f.refuse(&f.current(), f.starttime, category);
            }
        }
        // Drop touches only this exact created ID/name/nonce/label set.
    }
}

const KERNEL_HELPER: &str = r#"
import ctypes
import http.client
import json
import os
import platform
import re
import select
import signal
import socket
import stat
import sys

signal.alarm(10)
SESSION = '/srv/cad1085-mount-acceptance-engine-57439'
SOCKET = SESSION + '/overlay-docker.sock'
ROOT = SESSION + '/overlay-data'

class Refused(Exception):
    pass

def need(condition, reason):
    if not condition:
        raise Refused(reason)

def ident(s):
    return (s.st_dev, s.st_ino)

def secure_open(path, flags, directory=False):
    need(isinstance(path, str) and path.startswith('/') and not path.startswith('//')
         and os.path.normpath(path) == path, 'fixture-path-refused')
    parts = path.split('/')[1:]
    need(all(p not in ('', '.', '..') for p in parts), 'fixture-path-refused')
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        root = os.fstat(fd)
        need(root.st_uid == 0 and not root.st_mode & 0o022, 'fixture-parent-refused')
        for index, part in enumerate(parts):
            final = index == len(parts) - 1
            fl = flags if final else os.O_RDONLY | os.O_DIRECTORY
            if final and directory:
                fl |= os.O_DIRECTORY
            nxt = os.open(part, fl | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
            os.close(fd)
            fd = nxt
            meta = os.fstat(fd)
            need(meta.st_uid == 0, 'fixture-owner-refused')
            if not final or directory:
                need(stat.S_ISDIR(meta.st_mode) and not meta.st_mode & 0o022,
                     'fixture-parent-refused')
        return fd
    except BaseException:
        os.close(fd)
        raise

class UnixHTTP(http.client.HTTPConnection):
    def __init__(self):
        super().__init__('docker', timeout=3)
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(3)
        self.sock.connect(SOCKET)

def engine_get(path):
    need(path == '/info' or re.fullmatch('/containers/[0-9a-f]{64}/json', path),
         'fixture-api-path-refused')
    conn = UnixHTTP()
    try:
        conn.request('GET', '/v1.44' + path, headers={'Connection': 'close'})
        response = conn.getresponse()
        payload = response.read(16 * 1024 * 1024 + 1)
        need(response.status == 200 and len(payload) <= 16 * 1024 * 1024,
             'fixture-api-unavailable')
        result = json.loads(payload)
        need(isinstance(result, dict), 'fixture-api-unavailable')
        return result
    finally:
        conn.close()

def start_from(fd):
    os.lseek(fd, 0, os.SEEK_SET)
    raw = os.read(fd, 8193)
    need(len(raw) <= 8192, 'fixture-process-refused')
    tail = raw.rsplit(b')', 1)[1].split()
    need(len(tail) > 19, 'fixture-process-refused')
    return int(tail[19])

def private_mounts(fd):
    os.lseek(fd, 0, os.SEEK_SET)
    chunks = []
    size = 0
    while True:
        chunk = os.read(fd, 65536)
        if not chunk:
            break
        size += len(chunk)
        need(size <= 4 * 1024 * 1024, 'fixture-mountinfo-bound')
        chunks.append(chunk)
    for line in b''.join(chunks).splitlines():
        left, separator, right = line.partition(b' - ')
        fields = left.split()
        need(separator and len(fields) >= 6 and len(right.split()) >= 3,
             'fixture-mountinfo-refused')
        # A different mount namespace alone does not prevent propagation.
        # Refuse shared groups before any attached mount mutation.
        need(not any(p.startswith(b'shared:') for p in fields[6:]),
             'fixture-shared-propagation-refused')

libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long

class MountAttr(ctypes.Structure):
    _fields_ = [('attr_set', ctypes.c_uint64), ('attr_clr', ctypes.c_uint64),
                ('propagation', ctypes.c_uint64), ('userns_fd', ctypes.c_uint64)]

def detached_readonly(fd):
    # Explicit Linux x86_64 prerequisite below guards these syscall numbers.
    # OPEN_TREE_CLONE makes a detached clone; no host mount is attached,
    # remounted or unmounted. Attribute changes affect only that clone.
    tree = libc.syscall(ctypes.c_long(428), ctypes.c_int(fd), ctypes.c_char_p(b''),
                        ctypes.c_uint(1 | os.O_CLOEXEC | 0x1000))
    need(tree >= 0, 'fixture-open-tree-unavailable')
    # Read-only plus MS_PRIVATE, so the imported tree has no shared peers.
    attr = MountAttr(1, 0, 1 << 18, 0)
    rc = libc.syscall(ctypes.c_long(442), ctypes.c_int(tree), ctypes.c_char_p(b''),
                     ctypes.c_uint(0x1000), ctypes.byref(attr), ctypes.c_size_t(ctypes.sizeof(attr)))
    need(rc == 0, 'fixture-mount-setattr-unavailable')
    return int(tree)

def attach(tree, target):
    rc = libc.syscall(ctypes.c_long(429), ctypes.c_int(tree), ctypes.c_char_p(b''),
                     ctypes.c_int(-100), ctypes.c_char_p(target), ctypes.c_uint(4))
    need(rc == 0, 'fixture-move-mount-unavailable')

def main():
    need(os.geteuid() == 0 and os.getegid() == 0 and sys.platform == 'linux'
         and platform.machine() == 'x86_64', 'fixture-root-linux-x86-64-required')
    with open('/proc/self/status', 'r', encoding='ascii') as status:
        caps = next(line.split()[1] for line in status if line.startswith('CapEff:'))
    need(int(caps, 16) & (1 << 21), 'fixture-cap-sys-admin-required')
    raw = sys.stdin.buffer.read(65537)
    need(len(raw) <= 65536, 'fixture-input-bound')
    request = json.loads(raw)
    op = request.get('op')
    need(op in ('info', 'wrong-inode', 'writable', 'extra-host', 'extra-tmpfs',
                'stacked', 'directive', 'comment'), 'fixture-operation-refused')
    session_fd = secure_open(SESSION, os.O_RDONLY, True)
    need(stat.S_IMODE(os.fstat(session_fd).st_mode) == 0o700, 'fixture-session-mode-refused')
    socket_fd = secure_open(SOCKET, os.O_PATH)
    need(stat.S_ISSOCK(os.fstat(socket_fd).st_mode), 'fixture-socket-refused')
    info = engine_get('/info')
    need(info.get('DockerRootDir') == ROOT
         and str(info.get('ServerVersion', '')).startswith('29.'), 'fixture-isolated-engine-refused')
    root_fd = secure_open(ROOT, os.O_RDONLY, True)
    if op == 'info':
        print(json.dumps({'root': ROOT, 'version': info['ServerVersion']}))
        return

    nonce = request.get('nonce')
    need(isinstance(nonce, str) and re.fullmatch('[0-9a-f]{32}', nonce), 'fixture-nonce-refused')
    case = '/run/c1085-' + nonce
    need(request.get('case') == case, 'fixture-case-refused')
    case_fd = secure_open(case, os.O_RDONLY, True)
    cid = request.get('id')
    need(isinstance(cid, str) and re.fullmatch('[0-9a-f]{64}', cid), 'fixture-id-refused')
    name = 'cad1085-metadata-acceptance-' + nonce
    need(request.get('name') == name, 'fixture-name-refused')
    inspected = engine_get('/containers/' + cid + '/json')
    labels = request.get('labels')
    need(isinstance(labels, dict) and labels.get('cadence.protected.owner') == '1'
         and labels.get('cadence.protected.nonce') == nonce, 'fixture-labels-refused')
    actual_labels = inspected.get('Config', {}).get('Labels', {})
    need(inspected.get('Id') == cid and inspected.get('Name') == '/' + name
         and actual_labels == labels, 'fixture-owner-refused')
    state = inspected.get('State', {})
    pid = request.get('pid')
    starttime = request.get('starttime')
    need(type(pid) is int and pid > 1 and type(starttime) is int and starttime > 0
         and state.get('Running') is True and state.get('Paused') is False
         and state.get('Pid') == pid, 'fixture-process-refused')
    cfg = inspected.get('Config', {})
    host = inspected.get('HostConfig', {})
    need(cfg.get('User') == '59001:59001' and host.get('NetworkMode') == 'none'
         and cfg.get('NetworkDisabled', False) is False
         and host.get('ReadonlyRootfs') is True and host.get('Privileged') is False
         and host.get('CapDrop') == ['ALL'] and not host.get('CapAdd'), 'fixture-confinement-refused')
    for field, filename in (('ResolvConfPath', 'resolv.conf'),
                            ('HostnamePath', 'hostname'), ('HostsPath', 'hosts')):
        need(inspected.get(field) == ROOT + '/containers/' + cid + '/' + filename,
             'fixture-source-refused')

    pidfd = os.pidfd_open(pid, 0)
    poller = select.poll()
    poller.register(pidfd, select.POLLIN)
    statfd = os.open('/proc/' + str(pid) + '/stat', os.O_RDONLY | os.O_CLOEXEC)
    def alive():
        need(not poller.poll(0) and start_from(statfd) == starttime,
             'fixture-stale-process-refused')
    alive()
    with open('/proc/' + str(pid) + '/cgroup', 'rb') as cgroup:
        cg = cgroup.read(65537)
    need(len(cg) <= 65536 and cid.encode('ascii') in cg, 'fixture-cgroup-refused')
    nsfd = os.open('/proc/' + str(pid) + '/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
    self_ns_dir = os.open('/proc/self/ns', os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    own_ns = os.open('/proc/self/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
    init_ns = os.open('/proc/1/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
    nsid = ident(os.fstat(nsfd))
    need(nsid != ident(os.fstat(own_ns)) and nsid != ident(os.fstat(init_ns)),
         'fixture-host-namespace-refused')
    mountinfo = os.open('/proc/' + str(pid) + '/mountinfo', os.O_RDONLY | os.O_CLOEXEC)
    private_mounts(mountinfo)
    guest_root = os.open('/proc/' + str(pid) + '/root', os.O_PATH | os.O_DIRECTORY | os.O_CLOEXEC)
    etc = os.open('etc', os.O_PATH | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=guest_root)
    guest = os.open('resolv.conf', os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=etc)
    source_path = ROOT + '/containers/' + cid + '/resolv.conf'
    source = secure_open(source_path, os.O_RDWR if op in ('directive', 'comment') else os.O_RDONLY)
    sm, gm = os.fstat(source), os.fstat(guest)
    need(stat.S_ISREG(sm.st_mode) and sm.st_uid == 0 and sm.st_gid == 0
         and stat.S_IMODE(sm.st_mode) == 0o644 and ident(sm) == ident(gm),
         'fixture-original-inode-refused')
    original = os.read(source, 8193)
    need(len(original) <= 8192, 'fixture-content-bound')
    tree = None
    if op in ('wrong-inode', 'extra-host'):
        replacement = os.open('same-content', os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=case_fd)
        rm = os.fstat(replacement)
        need(stat.S_ISREG(rm.st_mode) and rm.st_uid == 0 and rm.st_gid == 0
             and stat.S_IMODE(rm.st_mode) == 0o644 and ident(rm) != ident(sm), 'fixture-replacement-refused')
        need(os.read(replacement, 8193) == original, 'fixture-identical-content-required')
        tree = detached_readonly(replacement)
    elif op == 'stacked':
        tree = detached_readonly(source)

    # Pin process and exact namespace before entry. Detached clones above do
    # not modify the host namespace. No fallback or mutation retry is allowed.
    alive()
    check_ns = os.open('/proc/' + str(pid) + '/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
    need(ident(os.fstat(check_ns)) == nsid, 'fixture-stale-namespace-refused')
    private_mounts(mountinfo)
    need(libc.unshare(ctypes.c_int(0x200)) == 0, 'fixture-private-fs-unavailable')
    need(libc.setns(ctypes.c_int(nsfd), ctypes.c_int(0x20000)) == 0, 'fixture-setns-unavailable')
    entered = os.open('mnt', os.O_RDONLY | os.O_CLOEXEC, dir_fd=self_ns_dir)
    need(ident(os.fstat(entered)) == nsid, 'fixture-entered-namespace-refused')
    alive()
    private_mounts(mountinfo)
    os.fchdir(guest_root)
    os.chroot('.')
    os.chdir('/')
    target = b'/etc/resolv.conf'
    need(ident(os.stat(target, follow_symlinks=False)) == ident(sm), 'fixture-target-refused')
    if op == 'wrong-inode':
        need(libc.umount2(ctypes.c_char_p(target), ctypes.c_int(2)) == 0,
             'fixture-exact-unmount-unavailable')
        attach(tree, target)
        need(ident(os.stat(target, follow_symlinks=False)) != ident(sm), 'fixture-wrong-inode-not-established')
    elif op == 'writable':
        # MS_BIND|MS_REMOUNT changes this existing mount, not a new overlay.
        need(libc.mount(None, ctypes.c_char_p(target), None,
                        ctypes.c_ulong(4096 | 32 | 2 | 4), None) == 0, 'fixture-exact-remount-unavailable')
        need(ident(os.stat(target, follow_symlinks=False)) == ident(sm), 'fixture-inode-changed')
    elif op == 'extra-host':
        need(stat.S_ISREG(os.stat('/etc/passwd', follow_symlinks=False).st_mode),
             'fixture-extra-file-target-unavailable')
        attach(tree, b'/etc/passwd')
    elif op == 'extra-tmpfs':
        need(stat.S_ISDIR(os.stat('/mnt', follow_symlinks=False).st_mode),
             'fixture-extra-directory-target-unavailable')
        need(libc.mount(ctypes.c_char_p(b'cad1085-unapproved-nonabsolute'),
                        ctypes.c_char_p(b'/mnt'), ctypes.c_char_p(b'tmpfs'),
                        ctypes.c_ulong(1 | 2 | 4 | 8), ctypes.c_char_p(b'size=4096,mode=0555')) == 0,
             'fixture-extra-tmpfs-unavailable')
    elif op == 'stacked':
        attach(tree, target)
        need(ident(os.stat(target, follow_symlinks=False)) == ident(sm), 'fixture-stacked-inode-changed')
    elif op in ('directive', 'comment'):
        suffix = (b'\nsearch cad1085-unapproved.invalid\n' if op == 'directive'
                  else b'\n# cad1085-unrecognized-acceptance-comment\n')
        need(ident(os.fstat(source)) == ident(sm), 'fixture-source-inode-changed')
        os.lseek(source, 0, os.SEEK_END)
        remaining = suffix
        while remaining:
            written = os.write(source, remaining)
            need(written > 0, 'fixture-content-write-unavailable')
            remaining = remaining[written:]
        os.fsync(source)
        need(ident(os.fstat(source)) == ident(sm)
             and ident(os.stat(target, follow_symlinks=False)) == ident(sm), 'fixture-content-inode-changed')
    else:
        raise Refused('fixture-operation-refused')
    alive()
    print('{"ok":true}')

try:
    main()
except Refused as error:
    # Each reason is a fixed public test category, never file contents.
    sys.stderr.write(str(error) + '\n')
    sys.exit(1)
except BaseException:
    sys.stderr.write('fixture-helper-unavailable\n')
    sys.exit(1)
"#;
