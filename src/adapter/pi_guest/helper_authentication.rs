//! Private independent public image + Pi-purpose qualification for helper and
//! supervisor client. Each parent imports the SAME fixed public getter/type,
//! never a callback. No returned ring, UID, parent/PID or caller file is trust.
use super::{helper_image_trust, HelperImageTrust};
use crate::protected_pi_profile::{authority, purpose};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;

pub(super) struct QualifiedImage {
    trust: HelperImageTrust,
}
impl QualifiedImage {
    pub(super) fn load() -> io::Result<Self> {
        // Fixed immutable media and SAME build-qualified image authority keys
        // as bootstrap. Empty/missing pins refuse; never response-elected keys.
        Ok(Self {
            trust: helper_image_trust(purpose::now_ms()?).map_err(|_| authority::refused())?,
        })
    }
    pub(super) fn authenticate(
        self,
        expected: &authority::Selection,
        launch: &authority::Authorized,
        signed: &authority::SignedOperation,
    ) -> io::Result<HelperAuthorization> {
        let scope = &signed.scope;
        // Bind to the privately selected profile/client expectation, never the
        // response's own self-selected description. Channel correlation is an
        // additional prerequisite, not a substitute for this shared trust gate.
        launch.validate(expected)?;
        if scope.version != 1 || scope.selection != launch.selection || scope.alias != launch.alias
        {
            return Err(authority::refused());
        }
        if scope.helper_sha256
            != self
                .trust
                .helper_sha256()
                .map_err(|_| authority::refused())?
            || scope.node_sha256 != self.trust.node_sha256().map_err(|_| authority::refused())?
            || scope.profile_sha256
                != self
                    .trust
                    .profile_sha256()
                    .map_err(|_| authority::refused())?
            || scope.policy_sha256
                != self
                    .trust
                    .policy_sha256()
                    .map_err(|_| authority::refused())?
            || launch.image.helper_sha256 != scope.helper_sha256
            || launch.image.node_sha256 != scope.node_sha256
        {
            return Err(authority::refused());
        }
        // Entire canonical profile, NOT files-only hashing or a response pin.
        let profile = serde_json::to_vec(
            &serde_json::to_value(&launch.image).map_err(|_| authority::refused())?,
        )
        .map_err(|_| authority::refused())?;
        if profile.len() > authority::MAX_FRAME
            || <[u8; 32]>::from(Sha256::digest(&profile)) != scope.profile_sha256
        {
            return Err(authority::refused());
        }
        // Bind independently qualified image to the exact signed native tuple;
        // full live owner/current and actual helper custody remain ROOT guards.
        let binding: serde_json::Value =
            serde_json::from_str(&scope.binding_json).map_err(|_| authority::refused())?;
        if serde_json::to_string(&binding).map_err(|_| authority::refused())? != scope.binding_json
        {
            return Err(authority::refused());
        }
        let pins = binding
            .get("challenge")
            .and_then(|v| v.get("pins"))
            .ok_or_else(authority::refused)?;
        if pins.get("image").and_then(serde_json::Value::as_str) != Some(self.trust.image()) {
            return Err(authority::refused());
        }
        for (name, digest) in [
            ("helper", scope.helper_sha256),
            ("node", scope.node_sha256),
            ("piGraph", scope.profile_sha256),
            ("policy", scope.policy_sha256),
        ] {
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            if pins.get(name).and_then(serde_json::Value::as_str) != Some(hex.as_str()) {
                return Err(authority::refused());
            }
        }
        let keys = self
            .trust
            .pi_public_keys()
            .map_err(|_| authority::refused())?;
        let operation = purpose::authenticate_operation(
            scope,
            &launch.operation,
            &signed.authorization,
            &keys,
            purpose::now_ms()?,
            self.trust.expires_at_ms(),
        )?;
        // Image is not model permission: require the actual independent elected
        // route policy bytes, no wrapper reinterpretation or unsigned fallback.
        require_policy(launch, scope.policy_sha256)?;
        operation.recheck(purpose::now_ms()?)?;
        Ok(HelperAuthorization {
            image: self,
            operation,
        })
    }
}
pub(super) struct HelperAuthorization {
    image: QualifiedImage,
    operation: purpose::AuthenticatedOperation,
}
impl HelperAuthorization {
    pub(super) fn recheck(&self) -> io::Result<()> {
        let now = purpose::now_ms()?;
        if now >= self.image.trust.expires_at_ms() {
            return Err(authority::refused());
        }
        self.operation.recheck(now)
    }
    pub(super) fn deadline(&self) -> std::time::Instant {
        self.operation.deadline()
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    routes: Vec<Route>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Route {
    alias: String,
    role: authority::Role,
    models: Vec<String>,
}
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn require_policy(launch: &authority::Authorized, digest: [u8; 32]) -> io::Result<()> {
    for path in ["/", "/opt", "/opt/cadence"] {
        let m = std::fs::symlink_metadata(path)?;
        if !m.is_dir() || m.uid() != 0 || m.gid() != 0 || m.mode() & 0o7777 != 0o755 {
            return Err(authority::refused());
        }
    }
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK) as u64,
        mode: 0,
        resolve: libc::RESOLVE_NO_SYMLINKS,
    };
    let raw = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            c"/opt/cadence/pi-policy.json".as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = File::from(unsafe { OwnedFd::from_raw_fd(raw as i32) });
    let before = file.metadata()?;
    let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
    if !before.is_file()
        || before.uid() != 0
        || before.gid() != 0
        || before.mode() & 0o7777 != 0o644
        || before.nlink() != 1
        || before.size() == 0
        || before.size() > 65536
        || unsafe { libc::fstatvfs(file.as_raw_fd(), &mut fs) } != 0
        || fs.f_flag & libc::ST_RDONLY == 0
    {
        return Err(authority::refused());
    }
    let mut bytes = Vec::new();
    (&file).take(65537).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let stamp = |m: &std::fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.size(),
            m.ctime(),
            m.ctime_nsec(),
            m.mtime(),
            m.mtime_nsec(),
        )
    };
    if bytes.len() as u64 != before.size()
        || stamp(&before) != stamp(&after)
        || <[u8; 32]>::from(Sha256::digest(&bytes)) != digest
    {
        return Err(authority::refused());
    }
    let policy: Policy = serde_json::from_slice(&bytes).map_err(|_| authority::refused())?;
    let canonical =
        serde_json::to_vec(&serde_json::to_value(&policy).map_err(|_| authority::refused())?)
            .map_err(|_| authority::refused())?;
    if canonical != bytes
        || policy.version != 1
        || policy.routes.is_empty()
        || policy.routes.len() > 128
    {
        return Err(authority::refused());
    }
    let mut aliases = std::collections::BTreeSet::new();
    for route in &policy.routes {
        if route.alias.is_empty()
            || route.alias.len() > 192
            || route.alias.contains(['\0', '\n', '\r'])
            || !aliases.insert(&route.alias)
            || route.models.is_empty()
            || route.models.len() > 64
        {
            return Err(authority::refused());
        }
        for model in &route.models {
            if !model.contains('/')
                || crate::protected_pi_profile::Routing::for_agent(
                    route.role == authority::Role::Master,
                    model,
                )
                .is_err()
            {
                return Err(authority::refused());
            }
        }
    }
    if !policy.routes.iter().any(|route| {
        route.alias == launch.alias
            && route.role == launch.selection.role
            && route.models.contains(&launch.selection.model)
    }) {
        return Err(authority::refused());
    }
    Ok(())
}
