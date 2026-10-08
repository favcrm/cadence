//! CAD-1177: durable, standalone (non-run) tool invocation receipts.
//!
//! A screen-invoked tool is not an app workflow run: there is no
//! `app_runs` row, no job/task, no owner PM and no assigned turn. These
//! tables mirror the run-bound capability claim/record contract
//! (`app_capabilities`) with one deliberate difference — identity and
//! dedup are scoped by the caller's stable `request_id`, not by a run.
//! The same guard shape holds: a claim is reserved BEFORE any provider
//! I/O, and exactly one result is retained per request id.
//!
//! `request_id` names ONE intent. Repeating a request id with the same
//! (install, alias, binding, input) returns the retained receipt — an
//! uncertain transport retries the same provider operation, never a
//! second charge. The same request id with a different input or binding
//! refuses (SEC-003 "unchanged inputs"); a different request id is a
//! new intent and gets its own row, so re-reading a new handle or
//! re-doing a draft works.

use super::StoreConn;
use super::*;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_tool_results(
 id TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE,
 install_id TEXT NOT NULL, alias TEXT NOT NULL, slot TEXT NOT NULL,
 binding_digest TEXT NOT NULL, input_digest TEXT NOT NULL,
 input TEXT NOT NULL, result TEXT NOT NULL, result_digest TEXT NOT NULL,
 asset_type TEXT, asset_digest TEXT, asset BLOB, created REAL NOT NULL);
CREATE INDEX IF NOT EXISTS app_tool_results_install
 ON app_tool_results(install_id,created,id);
CREATE TABLE IF NOT EXISTS app_tool_claims(
 request_id TEXT PRIMARY KEY, install_id TEXT NOT NULL, alias TEXT NOT NULL,
 slot TEXT NOT NULL, binding_digest TEXT NOT NULL, input_digest TEXT NOT NULL,
 call_id TEXT NOT NULL UNIQUE, created REAL NOT NULL);
";

pub const RESULT_BYTES: usize = 256 * 1024;
pub const ASSET_BYTES: usize = 2 * 1024 * 1024;

pub(crate) struct AppToolRecord<'a> {
    pub id: &'a str,
    pub request: &'a str,
    pub install: &'a str,
    pub alias: &'a str,
    pub slot: &'a str,
    pub binding_digest: &'a str,
    pub input_digest: &'a str,
    pub input: &'a Value,
    pub result: &'a Value,
    pub asset: Option<(&'a str, &'a [u8])>,
}

pub(crate) struct AppToolClaim<'a> {
    pub request: &'a str,
    pub install: &'a str,
    pub alias: &'a str,
    pub slot: &'a str,
    pub binding_digest: &'a str,
    pub input_digest: &'a str,
    pub call_id: &'a str,
}

struct ReceiptFields<'a> {
    request: &'a str,
    install: &'a str,
    alias: &'a str,
    slot: &'a str,
    binding_digest: &'a str,
    input_digest: &'a str,
    result: &'a Value,
    asset: Option<(&'a str, usize, &'a str)>,
}

fn receipt_digest(f: &ReceiptFields<'_>) -> String {
    app_runs::material_digest(&json!({
        "request_id": f.request, "install_id": f.install, "alias": f.alias,
        "slot": f.slot, "binding_digest": f.binding_digest,
        "input_digest": f.input_digest, "result": f.result,
        "asset": f.asset.map(|(media, size, digest)| json!({
            "media_type": media, "size": size, "digest": digest,
        })),
    }))
}

impl Store {
    /// Reserve the one paid operation for this request id BEFORE provider
    /// I/O. A repeat of the same request id is admitted only when it is
    /// byte-for-byte the same intent (same install, alias, slot, binding,
    /// input and call id) — an uncertain transport retry. Any difference
    /// refuses so a recycled id can never run a second/changed operation.
    pub(crate) fn app_tool_claim(&self, claim: AppToolClaim<'_>) -> Result<()> {
        let AppToolClaim {
            request,
            install,
            alias,
            slot,
            binding_digest,
            input_digest,
            call_id,
        } = claim;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            tx.execute(
                "INSERT OR IGNORE INTO app_tool_claims VALUES(?,?,?,?,?,?,?,?)",
                params![
                    request,
                    install,
                    alias,
                    slot,
                    binding_digest,
                    input_digest,
                    call_id,
                    now()
                ],
            )?;
            let existing: Option<(String, String, String, String, String, String)> = tx.query_opt(
                "SELECT install_id,alias,slot,binding_digest,input_digest,call_id
                     FROM app_tool_claims WHERE request_id=?",
                [request],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )?;
            if let Some(existing) = existing {
                if existing
                    != (
                        install.into(),
                        alias.into(),
                        slot.into(),
                        binding_digest.into(),
                        input_digest.into(),
                        call_id.into(),
                    )
                {
                    return Err(Error::rejected(
                        "tool request id already claimed a different operation",
                    ));
                }
            }
            Ok(())
        })
    }

    /// Whether this exact claim identity is already reserved — used by the
    /// record path to prove the durable claim precedes the result.
    fn app_tool_claimed(conn: &impl StoreConn, claim: &AppToolClaim<'_>) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_tool_claims WHERE request_id=? AND call_id=?
             AND install_id=? AND alias=? AND slot=? AND binding_digest=? AND input_digest=?)",
            params![
                claim.request,
                claim.call_id,
                claim.install,
                claim.alias,
                claim.slot,
                claim.binding_digest,
                claim.input_digest
            ],
            |r| r.get(0),
        )?)
    }

    /// The retained receipt for one request id, or `None`.
    pub(crate) fn app_tool_result_for_request(&self, request: &str) -> Result<Option<Value>> {
        let id = self
            .conn()
            .query_row(
                "SELECT id FROM app_tool_results WHERE request_id=?",
                [request],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        id.map(|id| self.app_tool_result(&id)).transpose()
    }

    /// Persist the one result for a request id after the provider call.
    /// Requires the matching pre-call claim; a second result for the same
    /// request id returns the existing receipt (the unique request id is
    /// the dedup), and a mismatched repeat refuses.
    pub(crate) fn app_tool_record(&self, record: AppToolRecord<'_>) -> Result<Value> {
        let AppToolRecord {
            id,
            request,
            install,
            alias,
            slot,
            binding_digest,
            input_digest,
            input,
            result,
            asset,
        } = record;
        let serialized = serde_json::to_vec(result)?;
        if serialized.len() > RESULT_BYTES {
            return Err(Error::rejected("app tool result exceeds 256 KiB"));
        }
        if asset.is_some_and(|(_, bytes)| bytes.is_empty() || bytes.len() > ASSET_BYTES) {
            return Err(Error::rejected("app tool asset exceeds 2 MiB"));
        }
        let asset_digest = asset.map(|(_, bytes)| app_runs::artifact_digest(bytes));
        let digest = receipt_digest(&ReceiptFields {
            request,
            install,
            alias,
            slot,
            binding_digest,
            input_digest,
            result,
            asset: asset.map(|(media, bytes)| {
                (
                    media,
                    bytes.len(),
                    asset_digest.as_deref().unwrap_or_default(),
                )
            }),
        });
        let input_text = serde_json::to_string(input)?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            if !Self::app_tool_claimed(
                &tx,
                &AppToolClaim {
                    request,
                    install,
                    alias,
                    slot,
                    binding_digest,
                    input_digest,
                    call_id: id,
                },
            )? {
                return Err(Error::rejected(
                    "app tool result has no matching pre-call claim",
                ));
            }
            let existing = tx.query_opt(
                "SELECT id,binding_digest,input_digest FROM app_tool_results WHERE request_id=?",
                [request],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )?;
            if let Some((existing_id, existing_binding, existing_input)) = existing {
                if existing_binding != binding_digest || existing_input != input_digest {
                    return Err(Error::rejected(
                        "tool request id already has a different result",
                    ));
                }
                return Self::app_tool_result_in(&tx, &existing_id);
            }
            tx.execute(
                "INSERT INTO app_tool_results(id,request_id,install_id,alias,slot,
                    binding_digest,input_digest,input,result,result_digest,asset_type,
                    asset_digest,asset,created) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    id,
                    request,
                    install,
                    alias,
                    slot,
                    binding_digest,
                    input_digest,
                    input_text,
                    String::from_utf8(serialized)
                        .map_err(|_| Error::internal("result encoding"))?,
                    digest,
                    asset.map(|(kind, _)| kind),
                    asset_digest,
                    asset.map(|(_, bytes)| bytes),
                    now()
                ],
            )?;
            Self::event(
                &tx,
                Self::DAEMON_STREAM,
                "app_tool_result_recorded",
                json!({"install_id":install,"alias":alias,"receipt_id":id,"digest":digest}),
            )?;
            Self::app_tool_result_in(&tx, id)
        })
    }

    pub(crate) fn app_tool_result(&self, id: &str) -> Result<Value> {
        Self::app_tool_result_in(&self.conn(), id)
    }

    /// Read retained bytes only for an install-owned receipt. The caller
    /// rechecks draft attachment/context and image integrity before serving.
    pub(crate) fn app_tool_asset_bytes(
        &self,
        install: &str,
        id: &str,
    ) -> Result<(String, String, Vec<u8>)> {
        let receipt = self.app_tool_result(id)?;
        if receipt["install_id"] != install {
            return Err(Error::rejected(
                "tool asset belongs to another installation",
            ));
        }
        let row=self.conn().query_row("SELECT asset_type,asset_digest,asset FROM app_tool_results WHERE id=? AND install_id=?",params![id,install],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<Vec<u8>>>(2)?))).optional()?;
        let (mime, digest, bytes) =
            row.ok_or_else(|| Error::rejected("tool asset is unavailable"))?;
        let mime = mime.ok_or_else(|| Error::rejected("tool receipt has no retained asset"))?;
        let digest = digest.ok_or_else(|| Error::rejected("tool asset digest is missing"))?;
        let bytes = bytes.ok_or_else(|| Error::rejected("tool asset bytes are unavailable"))?;
        if bytes.is_empty()
            || bytes.len() > ASSET_BYTES
            || app_runs::artifact_digest(&bytes) != digest
        {
            return Err(Error::rejected(
                "tool asset custody digest or size is invalid",
            ));
        }
        Ok((mime, digest, bytes))
    }

    fn app_tool_result_in(conn: &impl StoreConn, id: &str) -> Result<Value> {
        let row = conn
            .query_row(
                "SELECT request_id,install_id,alias,slot,binding_digest,input_digest,input,
                    result,result_digest,asset_type,asset_digest,length(asset),created FROM app_tool_results
                 WHERE id=?",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, String>(8)?,
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<i64>>(11)?,
                        r.get::<_, f64>(12)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::rejected("app tool result unavailable"))?;
        let result: Value = serde_json::from_str(&row.7)?;
        let input: Value = serde_json::from_str(&row.6)?;
        let asset_meta = row
            .9
            .as_ref()
            .map(|kind| json!({"media_type": kind, "digest": row.10, "size": row.11}));
        let expected = receipt_digest(&ReceiptFields {
            request: &row.0,
            install: &row.1,
            alias: &row.2,
            slot: &row.3,
            binding_digest: &row.4,
            input_digest: &row.5,
            result: &result,
            asset: asset_meta.as_ref().map(|a| {
                (
                    a["media_type"].as_str().unwrap_or_default(),
                    a["size"].as_i64().unwrap_or_default() as usize,
                    a["digest"].as_str().unwrap_or_default(),
                )
            }),
        });
        if expected != row.8 {
            return Err(Error::rejected("app tool result receipt is corrupt"));
        }
        Ok(json!({
            "id": id, "request_id": row.0, "install_id": row.1, "alias": row.2,
            "slot": row.3, "binding_digest": row.4, "input_digest": row.5,
            "input": input, "result": result, "digest": row.8,
            "asset": asset_meta, "created_at": row.12,
        }))
    }

    /// The receipts for one installation (operator read), newest last.
    /// Verify a selected post against a retained read receipt owned by this install.
    /// The receipt remains install-scoped; this does not assert it was private to a context.
    pub(crate) fn app_tool_source_post_exists(
        &self,
        install: &str,
        receipt_id: &str,
        post_id: &str,
    ) -> Result<bool> {
        let receipt = match self.app_tool_result(receipt_id) {
            Ok(receipt) if receipt["install_id"] == install => receipt,
            Ok(_) | Err(_) => return Ok(false),
        };
        Ok(receipt["result"]["kind"] == "social.source.posts"
            && receipt["result"]["posts"]
                .as_array()
                .is_some_and(|posts| posts.iter().any(|p| p["id"] == post_id)))
    }

    pub(crate) fn app_tool_results(&self, install: &str) -> Result<Value> {
        let conn = self.conn();
        let ids = conn
            .prepare(
                "SELECT id FROM app_tool_results WHERE install_id=? ORDER BY created,id LIMIT 100",
            )?
            .query_map([install], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(conn);
        Ok(json!({
            "results": ids
                .iter()
                .map(|id| self.app_tool_result(id))
                .collect::<Result<Vec<_>>>()?,
        }))
    }
}
