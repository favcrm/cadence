//! Root Pi service: owns BOTH the actual constructor-created helper and the
//! authentic one-use launch permit. Wire descriptions/selectors cannot create
//! this object. The constructor admits its supervisor before issuance/spawn;
//! accepted helper streams themselves are forwarded to root, never claimed PIDs.
use super::owner::{GenerationView, LaunchPermit};
use crate::error::{Error, Result};
use crate::installer_bundle::constructor::{HelperPhase, HelperStdio, OwnedHelper};
use crate::protected_pi_profile::authority::{
    self, Authorized, Request, Response, SignedOperation,
};
use std::cell::Cell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

fn refused() -> Error {
    Error::rejected("protected Pi owned helper/current service refused/UNKNOWN")
}
/// No Default/Deserialize/Clone or caller-shaped constructor inputs.
/// Holding a generic RuntimeProof/image JSON is NOT helper registration.
pub(crate) struct PendingLaunch {
    permit: LaunchPermit,
    helper: OwnedHelper,
}
impl PendingLaunch {
    pub(crate) fn spawn(permit: LaunchPermit, until: Instant) -> Result<(Self, HelperStdio)> {
        if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 {
            return Err(refused());
        }
        permit.require_provisioned()?;
        crate::installer_bundle::constructor::runtime_proof(until)?.recheck(until)?;
        // Constructor creates the ACTUAL helper for THIS opaque permit. There
        // is no API to pair an arbitrary helper/permit, adopt a PID or infer
        // registration from a self-selected Authorized descriptor.
        let (helper, stdio) = OwnedHelper::spawn(&permit, until)?;
        Ok((Self { permit, helper }, stdio))
    }
    pub(crate) fn describe(&self) -> &Authorized {
        self.permit.describe()
    }
    pub(crate) fn signed_operation(&self) -> Result<Box<SignedOperation>> {
        self.permit.signed_operation()
    }
    /// Production guard, BEFORE Arm changes state or authorizes graph/Node open.
    /// A same-UID peer, copied PID or self-selected Authorized cannot satisfy it.
    pub(crate) fn arm(
        &self,
        stream: &UnixStream,
        request: &Request,
        until: Instant,
    ) -> Result<Response> {
        let Request::Arm {
            version: 1,
            selection,
        } = request
        else {
            return Err(refused());
        };
        self.helper
            .require_peer(stream, HelperPhase::Privileged, until)?;
        self.permit.arm(selection)?;
        // Corroborate owned kernel custody/current again before response/effects.
        self.helper
            .require_peer(stream, HelperPhase::Privileged, until)?;
        self.permit.recheck()?;
        Ok(Response::Authorized {
            launch: self.permit.describe().clone(),
            signed: self.signed_operation()?,
        })
    }
    /// Production guard on the SAME retained connection after capability seal.
    /// The caller cannot substitute another stream/phase/reference/graph scope.
    fn consume(&self, stream: &UnixStream, request: &Request, until: Instant) -> Result<Response> {
        let Request::Consume {
            version: 1,
            selection,
            operation,
        } = request
        else {
            return Err(refused());
        };
        self.helper
            .require_peer(stream, HelperPhase::Sealed, until)?;
        self.permit.consume(selection, operation)?;
        self.helper
            .require_peer(stream, HelperPhase::Sealed, until)?;
        self.permit.recheck()?;
        Ok(Response::Consumed {
            version: 1,
            selection: selection.clone(),
            operation: operation.clone(),
        })
    }
    /// Retains the exact accepted stream, handles Arm then one Consume ONLY.
    /// Error/lost ACK drops the actual owned helper; no retry/reconnect/adoption.
    pub(crate) fn serve(self, stream: UnixStream, until: Instant) -> Result<RunningLaunch> {
        let until = until.min(Instant::now() + Duration::from_secs(authority::DEADLINE_SECS));
        let mut wire = Wire { stream, until };
        let request = wire.read()?;
        let response = self.arm(&wire.stream, &request, until)?;
        wire.write(&response)?;
        let request = wire.read()?;
        let response = self.consume(&wire.stream, &request, until)?;
        wire.write(&response)?;
        // ACK lets the helper attempt selected exec; constructor's tracer keeps
        // Node stopped before any user instruction until this actual gate passes.
        self.helper.release_node(&self.permit, until)?;
        let selection = self.permit.describe().selection.clone();
        let view = self.permit.into_generation_view()?;
        Ok(RunningLaunch {
            helper: self.helper,
            selection,
            view,
            retirement: Cell::new(Retirement::Live),
        })
    }
}
/// Runtime must retain this actual owned object for cancellation/retirement.
/// StdIO/PID observations are not an ownership substitute. Drop is physical
/// cleanup only, never a durable external owner retirement assertion.
pub(crate) struct RunningLaunch {
    pub(crate) helper: OwnedHelper,
    pub(crate) selection: authority::Selection,
    view: GenerationView,
    retirement: Cell<Retirement>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retirement {
    Live,
    Unknown,
    Completed,
}
impl RunningLaunch {
    /// N1 diagnostic: read-only, timestamped; callable before teardown drops
    /// custody. Changes no state.
    #[cfg(all(debug_assertions, feature = "test-seam"))]
    #[allow(dead_code)] // read by the native harness
    pub(crate) fn observe_pre_teardown(&self) -> super::diag_seam::HeldObservation {
        super::diag_seam::observe(
            &self.selection,
            self.retirement.get() == Retirement::Unknown,
            self.view.observe_unknown(),
        )
    }
    /// Kernel family retirement AND exact held view isolation, before a retire
    /// reply/drain. Worker/stream quiescence is still an independent later ACK.
    pub(crate) fn retire(&self, until: Instant) -> Result<()> {
        let until = until.min(Instant::now() + Duration::from_secs(10));
        match self.retirement.get() {
            Retirement::Completed => return self.require_retired(until),
            Retirement::Unknown => return Err(refused()),
            Retirement::Live => self.retirement.set(Retirement::Unknown),
        }
        let family = self.helper.retire_family(until)?;
        self.view.isolate(&family, until)?;
        self.retirement.set(Retirement::Completed);
        Ok(())
    }
    /// Expected control closure requires THIS completed view and actual original
    /// family witness. A helper-only retired flag cannot discard an UNKNOWN view.
    pub(crate) fn require_retired(&self, until: Instant) -> Result<()> {
        if self.retirement.get() != Retirement::Completed {
            return Err(refused());
        }
        let until = until.min(Instant::now() + Duration::from_secs(10));
        let result = (|| {
            let family = self.helper.retire_family(until)?;
            self.view.require_isolated(&family, until)
        })();
        if result.is_err() {
            self.retirement.set(Retirement::Unknown);
        }
        result
    }
}
struct Wire {
    stream: UnixStream,
    until: Instant,
}
impl Wire {
    fn budget(&self) -> Result<Duration> {
        self.until
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(refused)
    }
    fn exact(&mut self, mut bytes: &mut [u8]) -> Result<()> {
        while !bytes.is_empty() {
            self.stream.set_read_timeout(Some(self.budget()?))?;
            let n = self.stream.read(bytes)?;
            if n == 0 {
                return Err(refused());
            }
            bytes = &mut bytes[n..];
        }
        Ok(())
    }
    fn read(&mut self) -> Result<Request> {
        let mut length = [0; 4];
        self.exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        // Client requests never contain a graph; only finite selectors/ref.
        if length == 0 || length > authority::MAX_REQUEST {
            return Err(refused());
        }
        let mut bytes = vec![0; length];
        self.exact(&mut bytes)?;
        self.budget()?;
        serde_json::from_slice(&bytes).map_err(|_| refused())
    }
    fn write(&mut self, response: &Response) -> Result<()> {
        let body = serde_json::to_vec(response)?;
        if body.is_empty() || body.len() > authority::MAX_FRAME {
            return Err(refused());
        }
        let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
        bytes.extend(body);
        let mut bytes = bytes.as_slice();
        while !bytes.is_empty() {
            self.stream.set_write_timeout(Some(self.budget()?))?;
            let n = self.stream.write(bytes)?;
            if n == 0 {
                return Err(refused());
            }
            bytes = &bytes[n..];
        }
        self.budget()?;
        Ok(())
    }
}
