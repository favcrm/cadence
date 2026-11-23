//! Explicitly refusing protected-runtime surface for hosts other than Linux.
//! The real module provisions root-owned OCI runtimes and needs Linux kernel
//! facilities; on other targets the shared types exist so cross-platform code
//! compiles, and every entry point refuses.

use crate::error::{Error, Result};

/// The largest integer the wire format carries exactly.
pub(crate) const MAX_SAFE_INT: u64 = 9_007_199_254_740_991;

/// A verified operations caller. Private fields; on this host the listener
/// that alone produces one never binds, so no caller can be admitted.
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
    #[allow(dead_code)]
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

pub mod console {
    use std::os::unix::net::UnixStream;

    /// The owned OCI console type. On this host no console can exist.
    pub(crate) struct OwnerConsole;

    pub(crate) struct ConsoleParts {
        pub(crate) stdin: UnixStream,
        pub(crate) stdout: UnixStream,
        pub(crate) pid: u32,
    }

    impl OwnerConsole {
        pub(crate) fn into_parts(self) -> crate::error::Result<ConsoleParts> {
            Err(crate::error::Error::rejected(
                "the protected console is unavailable on this host",
            ))
        }
    }
}

pub mod qualified {
    /// The live qualified reader. On this host no reader can be issued, so
    /// every lookup refuses.
    pub(crate) struct CurrentReader<'a>(std::marker::PhantomData<&'a ()>);

    impl CurrentReader<'_> {
        pub(crate) fn qualified(&self) -> Option<&QualifiedRuntime> {
            None
        }

        pub(crate) fn require(&self) -> crate::error::Result<&QualifiedRuntime> {
            Err(crate::error::Error::rejected(
                "no current qualified runtime — admission refused",
            ))
        }

        pub(crate) fn runtime_id(&self) -> Option<&str> {
            None
        }
    }

    /// A runtime the qualification registry would publish; on this host the
    /// registry never holds one.
    pub(crate) struct QualifiedRuntime;
}

/// The strict JSON parser is platform-neutral and shared verbatim with the
/// real module, so cross-platform callers keep the same wire validation.
#[path = "protected_runtime/strict.rs"]
pub mod strict;

pub mod ops {
    pub(crate) use super::RuntimeCaller;

    /// The fixed operations method set. On this host the router never
    /// dispatches them; the type exists so shared code compiles.
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
    }
}

/// The owner CLI entry point. On this host no owner can run, so the binary
/// exists, prints the refusal and exits non-zero.
pub fn run_owner_cli() -> i32 {
    eprintln!("the protected runtime owner is unavailable on this host");
    1
}

/// No reviewed protected profile can be active on this host.
pub fn protected_profile_active() -> bool {
    false
}

/// The refusal every protected entry point returns here.
#[allow(dead_code)]
pub(crate) fn unavailable() -> Error {
    Error::rejected("the protected runtime is unavailable on this host")
}

#[allow(dead_code)]
fn _assert_result_used(_: Result<()>) {}
