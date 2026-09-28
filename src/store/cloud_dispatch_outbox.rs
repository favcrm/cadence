//! CAD-720's legacy v26 rows are local dispatch facts, never cloud work.
//!
//! A cloud turn may only follow an issuer-derived enrolled-child mapping
//! committed with a cloud-mode dispatch. That mapping does not exist yet, so
//! this internal claim remains closed. There is no publisher or reader.

use crate::error::{Error, Result};
use rusqlite::OptionalExtension;

use super::Store;

impl Store {
    /// No caller can supply an organization to convert a local v26 row into
    /// cloud work. A future claim must take only a verified, immutable cloud
    /// eligibility pin committed by the cloud dispatch producer.
    #[allow(dead_code)]
    pub(crate) fn claim_cloud_dispatch_turn(&self, message_id: &str) -> Result<String> {
        let eligible: Option<i64> = self
            .conn()
            .query_row(
                "SELECT cloud_eligible FROM cloud_dispatch_outbox WHERE message_id=?1",
                [message_id],
                |row| row.get(0),
            )
            .optional()?;
        if eligible != Some(1) {
            return Err(Error::rejected(
                "dispatch has no verified cloud enrollment pin",
            ));
        }
        // v27 only permits cloud_eligible=0. This branch is defensive: a
        // future schema must introduce a verified publisher and receipt flow
        // before it may implement a turn claim.
        Err(Error::rejected("cloud turn claim is not activated"))
    }
}
