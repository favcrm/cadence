//! The single per-runtime operations socket.
//!
//! One socket file — never its directory — is bind-mounted read-only into the
//! runtime guest. The listener is constructed for the enrolled runtime
//! identity; every connection is admitted only because the kernel's
//! `SO_PEERCRED` names the enrolled runtime uid and a live process. The
//! resulting [`RuntimeCaller`] is server-derived and cannot be built from a
//! request field; it never routes through the daemon's `operator_connection`
//! or a supervisor PID.
//!
//! The method set is fixed: the five protected Connection operations, the
//! metadata-only `connection_request_v1` human-setup intent and the three
//! bounded native model-turn methods of the r7 refinement. There is no
//! management method here, and a runtime's shell or detached child gains no
//! authority beyond that runtime's own operations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Error, Result};

use super::frame;
use super::peer;
use super::strict;
use super::{MAX_OPERATION_BYTES, MAX_RECEIPT_BYTES, REQUEST_BUDGET, RESPONSE_BUDGET};

/// The enrolled runtime identity the listener is constructed for. Produced by
/// the owner `prepare_runtime` reply; the daemon binds exactly once per
/// runtime generation.
#[derive(Clone, Debug)]
pub(crate) struct AgentEnrollment {
    pub(crate) runtime_id: String,
    pub(crate) agent_uid: String,
    pub(crate) session_binding: String,
    pub(crate) runtime_generation: u64,
    pub(crate) os_uid: u32,
}

/// A verified operations caller. Private fields; produced only by the listener
/// after the kernel peer check. There is no request/env qualified flag.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeCaller {
    runtime_id: String,
    agent_uid: String,
    session_binding: String,
    runtime_generation: u64,
    os_uid: u32,
    peer_pid: u32,
    peer_starttime: u64,
}

impl RuntimeCaller {
    /// Coordinator-internal construction from a closed registered handle.
    /// Never used for request admission: the listener is the only producer of
    /// a caller for an operations connection, and this exists so the adapter
    /// can bind the durable model task to the same exact subject.
    pub(crate) fn registered(
        runtime_id: String,
        agent_uid: String,
        session_binding: String,
        runtime_generation: u64,
        os_uid: u32,
        peer_pid: u32,
        peer_starttime: u64,
    ) -> Self {
        Self {
            runtime_id,
            agent_uid,
            session_binding,
            runtime_generation,
            os_uid,
            peer_pid,
            peer_starttime,
        }
    }

    pub(crate) fn runtime_id(&self) -> &str {
        &self.runtime_id
    }
    pub(crate) fn agent_uid(&self) -> &str {
        &self.agent_uid
    }
    pub(crate) fn session_binding(&self) -> &str {
        &self.session_binding
    }
    pub(crate) fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }
    pub(crate) fn os_uid(&self) -> u32 {
        self.os_uid
    }
    pub(crate) fn peer_pid(&self) -> u32 {
        self.peer_pid
    }
    pub(crate) fn peer_starttime(&self) -> u64 {
        self.peer_starttime
    }
}

/// Which durable authority a guest method belongs to. The operations socket
/// carries the five Connection operations, the metadata-only human-setup
/// intent and the three bounded native-model-turn methods (r7). The split is
/// fixed here so the router never guesses from the tag string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpsFamily {
    /// Custody/Connection backend (`Shared::handle_runtime`).
    Connection,
    /// Bounded keyless native model broker (`Shared::native_model`).
    NativeModel,
}

/// The exact guest method set: five protected Connection operations, the
/// metadata-only credential request intent, and the three native model-turn
/// methods. There is no management or generic provider method here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpsMethod {
    ConnectionSnapshotV1,
    ConnectionModelQuoteV1,
    ConnectionStageSendV1,
    ConnectionInvokeV1,
    ConnectionResultV1,
    ConnectionRequestV1,
    NativeModelTurnPrepareV1,
    NativeModelTurnV1,
    NativeModelTurnResultV1,
}

impl OpsMethod {
    fn parse(tag: &str) -> Result<Self> {
        match tag {
            "connection_snapshot_v1" => Ok(Self::ConnectionSnapshotV1),
            "connection_model_quote_v1" => Ok(Self::ConnectionModelQuoteV1),
            "connection_stage_send_v1" => Ok(Self::ConnectionStageSendV1),
            "connection_invoke_v1" => Ok(Self::ConnectionInvokeV1),
            "connection_result_v1" => Ok(Self::ConnectionResultV1),
            "connection_request_v1" => Ok(Self::ConnectionRequestV1),
            "native_model_turn_prepare_v1" => Ok(Self::NativeModelTurnPrepareV1),
            "native_model_turn_v1" => Ok(Self::NativeModelTurnV1),
            "native_model_turn_result_v1" => Ok(Self::NativeModelTurnResultV1),
            _ => Err(Error::rejected("unknown runtime operation")),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ConnectionSnapshotV1 => "connection_snapshot_v1",
            Self::ConnectionModelQuoteV1 => "connection_model_quote_v1",
            Self::ConnectionStageSendV1 => "connection_stage_send_v1",
            Self::ConnectionInvokeV1 => "connection_invoke_v1",
            Self::ConnectionResultV1 => "connection_result_v1",
            Self::ConnectionRequestV1 => "connection_request_v1",
            Self::NativeModelTurnPrepareV1 => "native_model_turn_prepare_v1",
            Self::NativeModelTurnV1 => "native_model_turn_v1",
            Self::NativeModelTurnResultV1 => "native_model_turn_result_v1",
        }
    }

    pub(crate) fn family(self) -> OpsFamily {
        match self {
            Self::NativeModelTurnPrepareV1
            | Self::NativeModelTurnV1
            | Self::NativeModelTurnResultV1 => OpsFamily::NativeModel,
            _ => OpsFamily::Connection,
        }
    }

    /// The measured Node process alone may use the native model-turn methods;
    /// the Connection methods keep their separately authorized tool/child
    /// behavior. The router applies this before dispatch.
    pub(crate) fn model_turn_only(self) -> bool {
        matches!(self.family(), OpsFamily::NativeModel)
    }
}

/// The daemon's dispatch implementation for the five operations. Arguments are
/// passed through for the strict DTO parser owned by the caller; the caller
/// identity is never taken from them.
pub(crate) trait RuntimeOpsHandler: Send + Sync {
    fn handle(
        &self,
        caller: &RuntimeCaller,
        method: OpsMethod,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value>;
}

pub(crate) struct RuntimeOpsListener {
    listener: std::os::unix::net::UnixListener,
    path: PathBuf,
    enrollment: AgentEnrollment,
}

impl RuntimeOpsListener {
    /// Replace the owner-prepared regular placeholder with the listening
    /// socket. Refuses any existing non-placeholder object.
    pub(crate) fn bind(path: &Path, enrollment: AgentEnrollment) -> Result<Self> {
        if enrollment.runtime_id.is_empty() || enrollment.agent_uid.is_empty() {
            return Err(Error::rejected("runtime enrollment is incomplete"));
        }
        super::spec::take_placeholder(path)?;
        let listener = std::os::unix::net::UnixListener::bind(path)
            .map_err(|_| Error::internal("runtime operations socket cannot be bound"))?;
        let mut perms = std::fs::metadata(path)
            .map_err(|_| Error::internal("runtime operations socket metadata unreadable"))?
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)
            .map_err(|_| Error::internal("runtime operations socket cannot be sealed"))?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            enrollment,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Serve one connection synchronously (used by tests/fixtures) or forever.
    pub(crate) fn serve(self, handler: Arc<dyn RuntimeOpsHandler>) -> Result<()> {
        for incoming in self.listener.incoming() {
            let stream = match incoming {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let handler = Arc::clone(&handler);
            let enrollment = self.enrollment.clone();
            std::thread::spawn(move || {
                let _ = serve_connection(stream, enrollment, handler);
            });
        }
        Ok(())
    }

    /// Serve connections until `stop` is set. The daemon uses this so a
    /// stopped runtime tears its listener down instead of leaking a blocked
    /// accept thread for the life of the process.
    pub(crate) fn serve_managed(
        self,
        handler: Arc<dyn RuntimeOpsHandler>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        use std::sync::atomic::Ordering;
        self.listener
            .set_nonblocking(true)
            .map_err(|_| Error::internal("operations listener cannot be made nonblocking"))?;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(false).is_err() {
                        continue;
                    }
                    let handler = Arc::clone(&handler);
                    let enrollment = self.enrollment.clone();
                    std::thread::spawn(move || {
                        let _ = serve_connection(stream, enrollment, handler);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }

    /// Remove this listener's socket file after the runtime is fenced. Never
    /// removes a foreign object.
    pub(crate) fn remove_socket(&self) {
        use std::os::unix::fs::FileTypeExt;
        if let Ok(meta) = std::fs::symlink_metadata(&self.path) {
            if meta.file_type().is_socket() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    pub(crate) fn serve_once(&self, handler: Arc<dyn RuntimeOpsHandler>) -> Result<()> {
        let (stream, _) = self
            .listener
            .accept()
            .map_err(|_| Error::unknown("operations accept failed"))?;
        serve_connection(stream, self.enrollment.clone(), handler)
    }
}

fn serve_connection(
    mut stream: std::os::unix::net::UnixStream,
    enrollment: AgentEnrollment,
    handler: Arc<dyn RuntimeOpsHandler>,
) -> Result<()> {
    let (os_uid, pid, starttime) = peer::admit_uid(&stream, enrollment.os_uid)?;
    let response = (|| -> Result<serde_json::Value> {
        let body = frame::read_frame(&stream, REQUEST_BUDGET, MAX_OPERATION_BYTES)?;
        let value = strict::parse(&body)?;
        let m = strict::object(&value, &["v", "method", "args"])?;
        if strict::safe_u64(m, "v")? != 1 {
            return Err(Error::rejected("operations v must be exactly 1"));
        }
        let method = OpsMethod::parse(strict::string(m, "method")?)?;
        let args = m
            .get("args")
            .ok_or_else(|| Error::rejected("operations args missing"))?;
        let caller = RuntimeCaller {
            runtime_id: enrollment.runtime_id.clone(),
            agent_uid: enrollment.agent_uid.clone(),
            session_binding: enrollment.session_binding.clone(),
            runtime_generation: enrollment.runtime_generation,
            os_uid,
            peer_pid: pid,
            peer_starttime: starttime,
        };
        handler.handle(&caller, method, args)
    })();
    let payload = match response {
        Ok(data) => frame::ok(data),
        Err(e) => {
            // Keep the broker's own closed code and fixed message. Collapsing
            // every `rejected` error to `invalid_request`/`operation refused`
            // hides `auth_unavailable`, `route_unavailable`,
            // `stale_configuration` and the rest from the measured guest, so
            // the guest cannot distinguish a refusal from a malformed request.
            let code = e.code().unwrap_or(match e.kind() {
                "rejected" => "invalid_request",
                "conflict" => "stale_revision",
                "busy" => "rate_limit",
                _ => "unavailable",
            });
            let message: String = e.to_string().chars().take(200).collect();
            frame::err(code, &message)
        }
    };
    if payload.len() > MAX_RECEIPT_BYTES {
        return Err(Error::internal(
            "operations response exceeds the receipt bound",
        ));
    }
    frame::write_frame(&stream, &payload, RESPONSE_BUDGET)?;
    let _ = stream.flush();
    Ok(())
}

use std::io::Write as _;
