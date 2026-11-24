//! Installation-scoped versioned Works documents (CAD-1649).
//!
//! One row per `(context_id, id)` in the installation's record file,
//! immutable revisions carrying the content digest, revision CAS,
//! idempotent mutation replay, and an audit row written in the same
//! transaction as every mutation/export. Authority is enforced by the
//! daemon (`daemon::app_works_rpc`): operator-only in v0, with the
//! verified human derived from the connection, never request text.
//!
//! Content grammar is deliberately small: `doc` blocks, `deck` slides,
//! and `sheet` columns/rows. It lives in `cadence_core::works` — the same
//! functions a hosted adapter evaluates — so this module stores only what
//! that grammar admits and never keeps a second copy.

use super::app_records::RecordStore;
use super::app_runs::{artifact_digest, material_digest};
use super::*;
use cadence_core::works::{self, Decision, Draft, State, Violation};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// A query never lists more than this many items.
pub const LIST_MAX: usize = 500;

/// Carry a shared Works refusal onto the native wire unchanged: the same
/// kind, code and message the store has always produced.
impl From<Violation> for Error {
    fn from(violation: Violation) -> Self {
        match violation.kind {
            "conflict" => Error::conflict_coded(violation.code, violation.message),
            "internal" => Error::internal(violation.message),
            _ => Error::invalid(violation.code, violation.message),
        }
    }
}

/// The stored review/export columns of one item, as the shared gates read
/// them.
type Item = (
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
    Option<i64>,
    bool,
    f64,
);

fn item_state(item: &Item) -> State<'_> {
    State {
        current: item.4,
        reviewed: item.5,
        review_status: &item.6,
        pending: item.7,
        trashed: item.8,
    }
}

/// `sha256:` plus the hex digest of the shared content material.
fn content_digest(kind: &str, title: &str, content: &Value) -> String {
    artifact_digest(works::content_material(kind, title, content).as_bytes())
}

/// `sha256:` plus the hex digest of the shared content-plus-tags material,
/// which pins an idempotent create to its exact payload.
fn payload_digest(draft: &Draft) -> String {
    artifact_digest(draft.payload_material().as_bytes())
}

fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 48 {
        out.truncate(48);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "content".to_string()
    } else {
        out
    }
}

#[allow(clippy::too_many_arguments)]
impl RecordStore {
    fn works_audit(
        &self,
        conn: &Connection,
        context: &str,
        actor: &str,
        on_behalf_of: Option<&str>,
        action: &str,
        id: &str,
        revision: i64,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO app_works_audit(context_id,at,actor,on_behalf_of,action,id,revision) VALUES(?,?,?,?,?,?,?)",
            params![context, now(), actor, on_behalf_of, action, id, revision],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        Ok(())
    }

    fn works_replay(
        &self,
        conn: &Connection,
        context: &str,
        actor: &str,
        op: &str,
        key: &str,
        payload_digest: &str,
    ) -> Result<Option<Value>> {
        let key = format!("{actor}:{key}");
        let existing: Option<(String, String, String, String)> = conn
            .query_row(
                "SELECT actor,op,payload_digest,result_json FROM app_works_idempotency WHERE context_id=? AND key=?",
                params![context, key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?;
        match existing {
            None => Ok(None),
            Some((stored_actor, stored_op, stored_digest, result)) => {
                if stored_actor != actor || stored_op != op || stored_digest != payload_digest {
                    return Err(Error::conflict_coded(
                        "conflict",
                        "works idempotency key was reused with a different payload",
                    ));
                }
                let value =
                    serde_json::from_str(&result).map_err(|e| Error::internal(e.to_string()))?;
                Ok(Some(value))
            }
        }
    }

    fn works_remember(
        &self,
        conn: &Connection,
        context: &str,
        actor: &str,
        op: &str,
        key: &str,
        payload_digest: &str,
        result: &Value,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO app_works_idempotency(context_id,key,actor,op,payload_digest,result_json,at) VALUES(?,?,?,?,?,?,?)",
            params![
                context,
                format!("{actor}:{key}"),
                actor,
                op,
                payload_digest,
                result.to_string(),
                now()
            ],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        Ok(())
    }

    fn works_item(&self, conn: &Connection, context: &str, id: &str) -> Result<Option<Item>> {
        conn.query_row(
            "SELECT kind,title,tags,owner,revision,reviewed_revision,review_status,pending_revision,trashed,updated FROM app_works_items WHERE context_id=? AND id=?",
            params![context, id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get::<_, i64>(8)? != 0,
                    row.get(9)?,
                ))
            },
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    fn works_detail(&self, conn: &Connection, context: &str, id: &str) -> Result<Value> {
        let (kind, title, tags, owner, revision, reviewed, status, pending, trashed, updated) =
            self.works_item(conn, context, id)?
                .ok_or_else(|| Error::invalid("not_found", "works content was not found"))?;
        if trashed {
            return Err(Error::invalid("not_found", "works content was not found"));
        }
        let (content_json, content_digest, author): (String, String, String) = conn
            .query_row(
                "SELECT content_json,content_digest,author FROM app_works_revisions WHERE context_id=? AND id=? AND revision=?",
                params![context, id, revision],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        let content: Value =
            serde_json::from_str(&content_json).map_err(|e| Error::internal(e.to_string()))?;
        let tags: Vec<String> =
            serde_json::from_str(&tags).map_err(|e| Error::internal(e.to_string()))?;
        let state = State {
            current: revision,
            reviewed,
            review_status: &status,
            pending,
            trashed,
        };
        let review_state = works::review_state(&state);
        let ready = works::export_state(&state);
        let updated_at = chrono::DateTime::<chrono::Utc>::from_timestamp(updated as i64, 0)
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
        let snippet = match kind.as_str() {
            "doc" => content
                .get("blocks")
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.first())
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            "deck" => content
                .get("slides")
                .and_then(Value::as_array)
                .and_then(|slides| slides.first())
                .and_then(|slide| slide.get("body"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            _ => format!(
                "{} rows",
                content
                    .get("rows")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0)
            ),
        };
        let mut detail = json!({
            "id": id,
            "kind": kind,
            "title": title,
            "revision": revision,
            "reviewedRevision": reviewed,
            "reviewState": review_state,
            "reviewStatus": status,
            "reviewRequestedRevision": pending,
            "exportState": if ready { "ready" } else { "blocked" },
            "exportReason": works::export_reason(&state),
            "updated": "just now",
            "updatedAt": updated_at,
            "owner": owner,
            "author": author,
            "tags": tags,
            "snippet": snippet,
            "contentDigest": content_digest,
        });
        if let Value::Object(content) = content {
            if let Value::Object(map) = &mut detail {
                for key in ["blocks", "slides", "columns", "rows"] {
                    if let Some(value) = content.get(key) {
                        map.insert(key.to_string(), value.clone());
                    }
                }
            }
        }
        Ok(detail)
    }

    pub fn app_works_get(&self, context: &str, id: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(id, "content ID")?;
        let conn = self.conn();
        self.works_detail(&conn, context, id)
    }

    pub fn app_works_create(
        &self,
        context: &str,
        draft: &Draft,
        actor: &str,
        key: &str,
        on_behalf_of: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let payload = payload_digest(draft);
        let mut result = None;
        self.write_tx(|tx| {
            if let Some(existing) =
                self.works_replay(tx, context, actor, "create", key, &payload)?
            {
                result = Some(existing);
                return Ok(());
            }
            let base = slug(&draft.title);
            let mut id = base.clone();
            let mut suffix = 2;
            loop {
                let taken: bool = tx
                    .query_row(
                        "SELECT 1 FROM app_works_items WHERE context_id=? AND id=?",
                        params![context, id],
                        |_| Ok(true),
                    )
                    .optional()
                    .map_err(|e| Error::internal(e.to_string()))?
                    .unwrap_or(false);
                if !taken {
                    break;
                }
                id = format!("{base}-{suffix}");
                suffix += 1;
                if suffix > 1000 {
                    return Err(Error::invalid("invalid", "works content ID space is exhausted"));
                }
            }
            let tags = serde_json::to_string(&draft.tags)
                .map_err(|e| Error::internal(e.to_string()))?;
            let content = serde_json::to_string(&draft.content)
                .map_err(|e| Error::internal(e.to_string()))?;
            let digest = content_digest(&draft.kind, &draft.title, &draft.content);
            tx.execute(
                "INSERT INTO app_works_items(context_id,id,kind,title,tags,owner,revision,reviewed_revision,review_status,pending_revision,trashed,updated) VALUES(?,?,?,?,?,?,1,0,'none',NULL,0,?)",
                params![context, id, draft.kind, draft.title, tags, actor, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            tx.execute(
                "INSERT INTO app_works_revisions(context_id,id,revision,kind,title,content_json,content_digest,author,at) VALUES(?,?,1,?,?,?,?,?,?)",
                params![context, id, draft.kind, draft.title, content, digest, actor, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            let detail = self.works_detail(tx, context, &id)?;
            self.works_audit(tx, context, actor, on_behalf_of, "create", &id, 1)?;
            self.works_remember(tx, context, actor, "create", key, &payload, &detail)?;
            result = Some(detail);
            Ok(())
        })?;
        result.ok_or_else(|| Error::internal("works create produced no result"))
    }

    pub fn app_works_query(&self, context: &str, params: &Value) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id FROM app_works_items WHERE context_id=? AND trashed=0 ORDER BY updated DESC LIMIT ?",
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        let ids: Vec<String> = stmt
            .query_map(params![context, LIST_MAX as i64 + 1], |row| row.get(0))
            .map_err(|e| Error::internal(e.to_string()))?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::internal(e.to_string()))?;
        if ids.len() > LIST_MAX {
            return Err(Error::internal("works listing exceeds its bound"));
        }
        drop(stmt);
        let text = params
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let kind = params.get("kind").and_then(Value::as_str).unwrap_or("all");
        let review = params.get("review").and_then(Value::as_str);
        let export = params.get("export").and_then(Value::as_str);
        let tag = params.get("tag").and_then(Value::as_str);
        let owner = params.get("owner").and_then(Value::as_str);
        let sort = params
            .get("sort")
            .and_then(Value::as_str)
            .unwrap_or("updated");
        let mut rows = Vec::new();
        for id in ids {
            let detail = match self.works_detail(&conn, context, &id) {
                Ok(detail) => detail,
                Err(_) => continue,
            };
            if kind != "all" && detail["kind"] != kind {
                continue;
            }
            if let Some(review) = review {
                let keep = match review {
                    "pending" | "changes_requested" => detail["reviewStatus"] == review,
                    "approved" => {
                        detail["reviewStatus"] == "approved" && detail["reviewState"] == "reviewed"
                    }
                    _ => detail["reviewState"] == review,
                };
                if !keep {
                    continue;
                }
            }
            if let Some(export) = export {
                if detail["exportState"] != export {
                    continue;
                }
            }
            if let Some(tag) = tag {
                if !detail["tags"]
                    .as_array()
                    .is_some_and(|tags| tags.iter().any(|value| value == tag))
                {
                    continue;
                }
            }
            if let Some(owner) = owner {
                if detail["owner"] != owner {
                    continue;
                }
            }
            if !text.is_empty() {
                let haystack = detail.to_string().to_lowercase();
                if !haystack.contains(&text) {
                    continue;
                }
            }
            rows.push(json!({
                "id": detail["id"],
                "kind": detail["kind"],
                "title": detail["title"],
                "revision": detail["revision"],
                "reviewedRevision": detail["reviewedRevision"],
                "reviewState": detail["reviewState"],
                "exportState": detail["exportState"],
                "updated": detail["updated"],
                "updatedAt": detail["updatedAt"],
                "owner": detail["owner"],
                "tags": detail["tags"],
                "snippet": detail["snippet"],
                "reviewStatus": detail["reviewStatus"],
            }));
        }
        if sort == "name" {
            rows.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
        }
        Ok(Value::Array(rows))
    }

    pub fn app_works_patch(
        &self,
        context: &str,
        id: &str,
        if_revision: i64,
        operations: &[Value],
        actor: &str,
        key: &str,
        on_behalf_of: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(id, "content ID")?;
        if operations.is_empty() {
            return Err(Error::invalid("invalid", "works operations are required"));
        }
        let payload = material_digest(
            &json!({"id": id, "ifRevision": if_revision, "operations": operations}),
        );
        let mut result = None;
        self.write_tx(|tx| {
            if let Some(existing) = self.works_replay(tx, context, actor, "patch", key, &payload)? {
                result = Some(existing);
                return Ok(());
            }
            let item = self
                .works_item(tx, context, id)?
                .ok_or_else(|| Error::invalid("not_found", "works content was not found"))?;
            if item.8 {
                return Err(Error::invalid("not_found", "works content was not found"));
            }
            if item.4 != if_revision {
                return Err(Error::conflict_coded(
                    "conflict",
                    "works content revision is stale",
                ));
            }
            let kind = item.0;
            let mut title = item.1;
            let mut content = self.works_content(tx, context, id, item.4)?;
            for operation in operations {
                let object = operation
                    .as_object()
                    .ok_or_else(|| Error::invalid("invalid", "works operation must be an object"))?;
                match object.get("type").and_then(Value::as_str) {
                    Some("set_title") => {
                        if object
                            .keys()
                            .any(|key| !matches!(key.as_str(), "type" | "title"))
                        {
                            return Err(Error::invalid(
                                "invalid",
                                "set_title has unsupported fields",
                            ));
                        }
                        let value = object
                            .get("title")
                            .and_then(Value::as_str)
                            .and_then(works::bounded_title)
                            .ok_or_else(|| Error::invalid("invalid", "set_title requires a bounded title"))?;
                        title = value.to_string();
                    }
                    Some("set_content") => {
                        if object
                            .keys()
                            .any(|key| !matches!(key.as_str(), "type" | "content"))
                        {
                            return Err(Error::invalid(
                                "invalid",
                                "set_content has unsupported fields",
                            ));
                        }
                        content = object
                            .get("content")
                            .cloned()
                            .ok_or_else(|| Error::invalid("invalid", "set_content requires content"))?;
                        works::validate_content(&kind, &content)?;
                    }
                    _ => return Err(Error::invalid("invalid", "works operation is unsupported")),
                }
            }
            works::validate_content(&kind, &content)?;
            let next = if_revision + 1;
            let digest = content_digest(&kind, &title, &content);
            let content_json =
                serde_json::to_string(&content).map_err(|e| Error::internal(e.to_string()))?;
            let changed = tx
                .execute(
                    "UPDATE app_works_items SET title=?,revision=?,reviewed_revision=reviewed_revision,review_status='none',pending_revision=NULL,updated=? WHERE context_id=? AND id=? AND revision=? AND trashed=0",
                    params![title, next, now(), context, id, if_revision],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::conflict_coded(
                    "conflict",
                    "works content revision is stale",
                ));
            }
            tx.execute(
                "INSERT INTO app_works_revisions(context_id,id,revision,kind,title,content_json,content_digest,author,at) VALUES(?,?,?,?,?,?,?,?,?)",
                params![context, id, next, kind, title, content_json, digest, actor, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            self.works_audit(tx, context, actor, on_behalf_of, "patch", id, next)?;
            let detail = self.works_detail(tx, context, id)?;
            self.works_remember(tx, context, actor, "patch", key, &payload, &detail)?;
            result = Some(detail);
            Ok(())
        })?;
        result.ok_or_else(|| Error::internal("works patch produced no result"))
    }

    fn works_content(
        &self,
        conn: &Connection,
        context: &str,
        id: &str,
        revision: i64,
    ) -> Result<Value> {
        let content: String = conn
            .query_row(
                "SELECT content_json FROM app_works_revisions WHERE context_id=? AND id=? AND revision=?",
                params![context, id, revision],
                |row| row.get(0),
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        serde_json::from_str(&content).map_err(|e| Error::internal(e.to_string()))
    }

    pub fn app_works_trash(
        &self,
        context: &str,
        id: &str,
        if_revision: i64,
        actor: &str,
        key: &str,
        on_behalf_of: Option<&str>,
    ) -> Result<Value> {
        self.works_flag(
            context,
            id,
            if_revision,
            true,
            actor,
            key,
            on_behalf_of,
            "trash",
        )
    }

    pub fn app_works_restore(
        &self,
        context: &str,
        id: &str,
        if_revision: i64,
        actor: &str,
        key: &str,
        on_behalf_of: Option<&str>,
    ) -> Result<Value> {
        self.works_flag(
            context,
            id,
            if_revision,
            false,
            actor,
            key,
            on_behalf_of,
            "restore",
        )
    }

    fn works_flag(
        &self,
        context: &str,
        id: &str,
        if_revision: i64,
        trashed: bool,
        actor: &str,
        key: &str,
        on_behalf_of: Option<&str>,
        action: &str,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(id, "content ID")?;
        let payload =
            material_digest(&json!({"id": id, "ifRevision": if_revision, "trashed": trashed}));
        let mut result = None;
        self.write_tx(|tx| {
            if let Some(existing) = self.works_replay(tx, context, actor, action, key, &payload)? {
                result = Some(existing);
                return Ok(());
            }
            let item = self
                .works_item(tx, context, id)?
                .ok_or_else(|| Error::invalid("not_found", "works content was not found"))?;
            if item.4 != if_revision {
                return Err(Error::conflict_coded(
                    "conflict",
                    "works content revision is stale",
                ));
            }
            let changed = tx
                .execute(
                    "UPDATE app_works_items SET trashed=?,updated=? WHERE context_id=? AND id=? AND revision=?",
                    params![if trashed { 1 } else { 0 }, now(), context, id, if_revision],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::conflict_coded(
                    "conflict",
                    "works content revision is stale",
                ));
            }
            self.works_audit(tx, context, actor, on_behalf_of, action, id, if_revision)?;
            let detail = if trashed {
                json!({"id": id, "revision": if_revision, "trashed": true})
            } else {
                self.works_detail(tx, context, id)?
            };
            self.works_remember(tx, context, actor, action, key, &payload, &detail)?;
            result = Some(detail);
            Ok(())
        })?;
        result.ok_or_else(|| Error::internal("works flag produced no result"))
    }

    pub fn app_works_request_review(
        &self,
        context: &str,
        id: &str,
        revision: i64,
        actor: &str,
        on_behalf_of: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(id, "content ID")?;
        self.write_tx(|tx| {
            let item = self
                .works_item(tx, context, id)?
                .ok_or_else(|| Error::invalid("not_found", "works content was not found"))?;
            if item.8 {
                return Err(Error::invalid("not_found", "works content was not found"));
            }
            if item.4 != revision {
                return Err(Error::conflict_coded(
                    "conflict",
                    "works content revision is stale",
                ));
            }
            tx.execute(
                "UPDATE app_works_items SET review_status='pending',pending_revision=?,updated=? WHERE context_id=? AND id=?",
                params![revision, now(), context, id],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            self.works_audit(tx, context, actor, on_behalf_of, "request_review", id, revision)?;
            self.works_detail(tx, context, id)
        })
    }

    pub fn app_works_review(
        &self,
        context: &str,
        id: &str,
        revision: i64,
        decision: &str,
        note: Option<&str>,
        actor: &str,
        on_behalf_of: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(id, "content ID")?;
        let decision = works::review_decision(decision, note)?;
        self.write_tx(|tx| {
            let item = self
                .works_item(tx, context, id)?
                .ok_or_else(|| Error::invalid("not_found", "works content was not found"))?;
            works::review(&item_state(&item), revision)?;
            let author: String = tx
                .query_row(
                    "SELECT author FROM app_works_revisions WHERE context_id=? AND id=? AND revision=?",
                    params![context, id, revision],
                    |row| row.get(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            works::reviewer(&author, actor)?;
            match decision {
                Decision::Approve => {
                    tx.execute(
                        "UPDATE app_works_items SET review_status='approved',reviewed_revision=?,pending_revision=NULL,updated=? WHERE context_id=? AND id=?",
                        params![revision, now(), context, id],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                }
                Decision::Changes => {
                    tx.execute(
                        "UPDATE app_works_items SET review_status='changes_requested',pending_revision=NULL,updated=? WHERE context_id=? AND id=?",
                        params![now(), context, id],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                }
            }
            self.works_audit(
                tx,
                context,
                actor,
                on_behalf_of,
                match decision {
                    Decision::Approve => "review_approve",
                    Decision::Changes => "review_changes",
                },
                id,
                revision,
            )?;
            self.works_detail(tx, context, id)
        })
    }

    pub fn app_works_export(
        &self,
        context: &str,
        id: &str,
        revision: i64,
        format: &str,
        actor: &str,
        on_behalf_of: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(id, "content ID")?;
        works::check_format(format)?;
        self.write_tx(|tx| {
            let item = self
                .works_item(tx, context, id)?
                .ok_or_else(|| Error::invalid("not_found", "works content was not found"))?;
            works::export(&item_state(&item), revision)?;
            let digest: String = tx
                .query_row(
                    "SELECT content_digest FROM app_works_revisions WHERE context_id=? AND id=? AND revision=?",
                    params![context, id, revision],
                    |row| row.get(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            self.works_audit(tx, context, actor, on_behalf_of, "export", id, revision)?;
            Ok(json!({"id": id, "revision": revision, "format": format, "digest": digest}))
        })
    }
}
