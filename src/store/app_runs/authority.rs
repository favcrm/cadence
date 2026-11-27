//! Read-only authority checks for active runs and historical material.

use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use super::material_digest;
use crate::error::{Error, Result};
use crate::store::{self, Store, StoreConn};

impl Store {
    pub(in crate::store) fn app_current_in(
        conn: &impl StoreConn,
        run: &Value,
        bundle: &str,
    ) -> Result<()> {
        Self::app_authority_in(conn, run, bundle, false)
    }
    /// Completed material retains its original approval epoch across package
    /// upgrades. It is never used for a new/active run or worker dispatch.
    pub(in crate::store) fn app_completed_current_in(
        conn: &impl StoreConn,
        run: &Value,
    ) -> Result<()> {
        if !matches!(run["state"].as_str(), Some("succeeded" | "failed"))
            || run["approved_digest"] != run["snapshot_digest"]
        {
            return Err(Error::rejected(
                "historical run is not completed and approved",
            ));
        }
        let bundle = run["snapshot"]["bundle_digest"]
            .as_str()
            .ok_or_else(|| Error::rejected("historical bundle digest is missing"))?;
        Self::app_authority_in(conn, run, bundle, true)
    }
    fn app_authority_in(
        conn: &impl StoreConn,
        run: &Value,
        bundle: &str,
        historical: bool,
    ) -> Result<()> {
        Self::app_context_current_in(conn, run)?;
        if material_digest(&run["snapshot"]) != run["snapshot_digest"] {
            return Err(Error::rejected(
                "immutable app run snapshot receipt is corrupt",
            ));
        }
        if run["snapshot"]["schema"] == 3
            || (run["snapshot"]["schema"] == 4 && run["snapshot"]["publication"].is_object())
        {
            let slot = run["snapshot"]["publication"]["slot"]
                .as_str()
                .ok_or_else(|| Error::rejected("publication snapshot slot is missing"))?;
            if run["snapshot"]["workflow"]["publication_slot"].as_str() != Some(slot) {
                return Err(Error::rejected(
                    "publication slot differs from its frozen workflow",
                ));
            }
            match run["snapshot"]["publication"].get("binding") {
                Some(Value::Null) => {}
                Some(value) => {
                    let proof: store::app_bindings::BindingProof =
                        serde_json::from_value(value.clone()).map_err(|_| {
                            Error::rejected("publication binding snapshot is invalid")
                        })?;
                    if proof.config["bundle_digest"] != bundle
                        || !store::app_bindings::binding_current_in(
                            conn,
                            run["install_id"].as_str().unwrap(),
                            run["context_id"].as_str(),
                            slot,
                            &proof,
                        )?
                    {
                        return Err(Error::rejected("publication binding snapshot is stale"));
                    }
                }
                None => return Err(Error::rejected("publication binding snapshot is missing")),
            }
        } else if run["snapshot"].get("publication").is_some()
            || run["snapshot"]["workflow"]
                .get("publication_slot")
                .is_some()
        {
            return Err(Error::rejected(
                "legacy snapshot cannot carry publication authority",
            ));
        }
        if run["snapshot"]["schema"] != 4
            && run["snapshot"]["workflow"]
                .get("required_asset_slot")
                .is_some()
        {
            return Err(Error::rejected(
                "required asset slot needs a capability snapshot",
            ));
        }
        if run["snapshot"]["schema"] == 4 {
            let declared = run["snapshot"]["workflow"]["capability_slots"]
                .as_array()
                .ok_or_else(|| {
                    Error::rejected("capability slots are missing from frozen workflow")
                })?;
            if let Some(required) = run["snapshot"]["workflow"].get("required_asset_slot") {
                required
                    .as_str()
                    .filter(|slot| declared.iter().any(|candidate| candidate == *slot))
                    .ok_or_else(|| {
                        Error::rejected("required asset slot differs from frozen capabilities")
                    })?;
            }
            let frozen = run["snapshot"]["capabilities"]
                .as_object()
                .ok_or_else(|| Error::rejected("capability binding map is missing"))?;
            let quotes = run["snapshot"]["quotes"]
                .as_object()
                .ok_or_else(|| Error::rejected("capability price quote map is missing"))?;
            if (declared.is_empty() && run["snapshot"]["source"].is_null())
                || declared.len() != frozen.len()
                || declared.len() != quotes.len()
                || declared.len() > 8
            {
                return Err(Error::rejected(
                    "frozen capability slot set differs from workflow",
                ));
            }
            for name in declared {
                let slot = name
                    .as_str()
                    .ok_or_else(|| Error::rejected("frozen capability slot is invalid"))?;
                let quote: crate::platform::AppCapabilityQuote = serde_json::from_value(
                    quotes
                        .get(slot)
                        .ok_or_else(|| Error::rejected("frozen capability quote is missing"))?
                        .clone(),
                )
                .map_err(|_| Error::rejected("frozen capability quote is invalid"))?;
                if !quote.valid() {
                    return Err(Error::rejected("frozen capability quote is invalid"));
                }
                let proof: store::app_bindings::BindingProof = serde_json::from_value(
                    frozen
                        .get(slot)
                        .ok_or_else(|| Error::rejected("frozen capability binding is missing"))?
                        .clone(),
                )
                .map_err(|_| Error::rejected("frozen capability binding is invalid"))?;
                if proof.config["bundle_digest"] != bundle
                    || !matches!(
                        proof.config["mapping"]["effect"].as_str(),
                        Some("read" | "draft")
                    )
                    || !store::app_bindings::binding_current_in(
                        conn,
                        run["install_id"].as_str().unwrap(),
                        run["context_id"].as_str(),
                        slot,
                        &proof,
                    )?
                {
                    return Err(Error::rejected("frozen capability binding is stale"));
                }
            }
        } else if run["snapshot"].get("capabilities").is_some()
            || run["snapshot"]["workflow"]
                .get("capability_slots")
                .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
        {
            return Err(Error::rejected(
                "legacy snapshot cannot carry capability authority",
            ));
        }
        let table = if historical {
            "app_capability_epochs"
        } else {
            "app_install_capabilities"
        };
        let found=conn.query_row(&format!("SELECT 1 FROM {table} WHERE install_id=? AND epoch=? AND digest=? AND state='approved'"),params![run["install_id"].as_str(),run["epoch"].as_i64(),bundle], |_|Ok(())).optional()?;
        if found.is_none() || run["snapshot"]["bundle_digest"].as_str() != Some(bundle) {
            return Err(Error::rejected(
                "app capability epoch or bundle digest is stale",
            ));
        }
        Ok(())
    }
    fn app_context_current_in(conn: &impl StoreConn, run: &Value) -> Result<()> {
        let context = run["context_id"].as_str();
        if let Some(id) = context {
            let snapshot = &run["snapshot"];
            if !matches!(snapshot["schema"].as_u64(), Some(2..=4))
                || snapshot["context"]["id"].as_str() != Some(id)
            {
                return Err(Error::rejected("context snapshot association is invalid"));
            }
            let proof = store::app_contexts::ContextProof {
                id: id.to_string(),
                install_id: run["install_id"].as_str().unwrap().to_string(),
                revision: snapshot["context"]["revision"]
                    .as_i64()
                    .ok_or_else(|| Error::rejected("context snapshot revision is invalid"))?,
                digest: snapshot["context"]["digest"]
                    .as_str()
                    .ok_or_else(|| Error::rejected("context snapshot digest is invalid"))?
                    .to_string(),
            };
            Self::app_context_proof_current_in(conn, run["install_id"].as_str().unwrap(), &proof)?;
        } else if !((run["snapshot"]["schema"] == 1 && run["snapshot"].get("context").is_none())
            || (matches!(run["snapshot"]["schema"].as_u64(), Some(3 | 4))
                && run["snapshot"].get("context").is_some_and(Value::is_null)))
        {
            return Err(Error::rejected(
                "context-free snapshot association is invalid",
            ));
        }
        Ok(())
    }
}
