//! Root-only bridge to the SAME finite Pi-purpose verifier as the helper.
//! Only real OwnerProfile obtains this ring/expiry from qualified constructor
//! runtime; the signature result alone cannot reconstruct a LaunchPermit.
use super::{refuse, OperationScope};
use crate::error::Result;
use crate::installer_bundle::constructor::PiKeyRecord;
use crate::protected_pi_profile::purpose;

pub(super) use purpose::AuthenticatedOperation;
pub(super) fn now_ms() -> Result<u64> {
    purpose::now_ms().map_err(|_| refuse())
}
pub(super) fn authenticate_operation(
    scope: &OperationScope,
    reference: &str,
    authorization: &str,
    keys: &[PiKeyRecord],
    now: u64,
    runtime_image_expiry: u64,
) -> Result<AuthenticatedOperation> {
    purpose::authenticate_operation(
        scope,
        reference,
        authorization,
        keys,
        now,
        runtime_image_expiry,
    )
    .map_err(|_| refuse())
}
