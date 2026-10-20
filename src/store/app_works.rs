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
//! and `sheet` columns/rows. Unknown kinds, fields, block types and
//! malformed/oversize content refuse before any write.

use super::app_records::RecordStore;
use super::app_runs::material_digest;
use super::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

pub const TITLE_BYTES: usize = 240;
pub const CONTENT_BYTES: usize = 64 * 1024;
pub const TAGS_MAX: usize = 20;
pub const LIST_MAX: usize = 500;
const KINDS: &[&str] = &["doc", "deck", "sheet"];

#[derive(Clone)]
pub struct WorksDraft {
    pub kind: String,
    pub title: String,
    pub content: Value,
    pub tags: Vec<String>,
}

impl WorksDraft {
    pub fn parse(params: &Value) -> Result<Self> {
        let kind = required_str(params, "kind")?.to_string();
        let title = required_str(params, "title")?.trim().to_string();
        let content = params
            .get("content")
            .cloned()
            .ok_or_else(|| Error::rejected("works content is required"))?;
        let tags = match params.get("tags") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => {
                if items.len() > TAGS_MAX {
                    return Err(Error::rejected("works tags exceed their bound"));
                }
                let mut tags = Vec::with_capacity(items.len());
                for item in items {
                    let tag = item
                        .as_str()
                        .ok_or_else(|| Error::rejected("works tags must be strings"))?
                        .trim()
                        .to_string();
                    if tag.is_empty() || tag.len() > 60 {
                        return Err(Error::rejected("works tags must be non-empty and bounded"));
                    }
                    if !tags.contains(&tag) {
                        tags.push(tag);
                    }
                }
                tags
            }
            Some(_) => return Err(Error::rejected("works tags must be an array")),
        };
        if title.is_empty() || title.len() > TITLE_BYTES {
            return Err(Error::rejected("works title is required and bounded"));
        }
        validate_content(&kind, &content)?;
        Ok(Self {
            kind,
            title,
            content,
            tags,
        })
    }

    fn payload_digest(&self) -> String {
        material_digest(&json!({
            "kind": self.kind,
            "title": self.title,
            "content": self.content,
            "tags": self.tags,
        }))
    }

    fn content_digest(&self) -> String {
        material_digest(&json!({
            "kind": self.kind,
            "title": self.title,
            "content": self.content,
        }))
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::rejected(format!("{key} is required")))
}

fn validate_content(kind: &str, content: &Value) -> Result<()> {
    if !KINDS.contains(&kind) {
        return Err(Error::rejected("works kind must be doc, deck or sheet"));
    }
    let object = content
        .as_object()
        .ok_or_else(|| Error::rejected("works content must be an object"))?;
    let encoded = serde_json::to_vec(content).map_err(|e| Error::internal(e.to_string()))?;
    if encoded.len() > CONTENT_BYTES {
        return Err(Error::rejected("works content exceeds its bound"));
    }
    match kind {
        "doc" => {
            let blocks = content_array(object, "blocks")?;
            for block in blocks {
                let block = block
                    .as_object()
                    .ok_or_else(|| Error::rejected("works doc blocks must be objects"))?;
                let kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("works doc block type is required"))?;
                if !matches!(kind, "h2" | "p" | "callout") {
                    return Err(Error::rejected("works doc block type is unsupported"));
                }
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("works doc block text is required"))?;
                if text.trim().is_empty() {
                    return Err(Error::rejected("works doc block text is required"));
                }
                if kind == "callout" {
                    if let Some(label) = block.get("label") {
                        if label.as_str().map(str::trim).is_none_or(str::is_empty) {
                            return Err(Error::rejected("works callout label must be a string"));
                        }
                    }
                }
            }
        }
        "deck" => {
            let slides = content_array(object, "slides")?;
            for slide in slides {
                let slide = slide
                    .as_object()
                    .ok_or_else(|| Error::rejected("works deck slides must be objects"))?;
                for field in ["title", "body"] {
                    if slide
                        .get(field)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .is_none_or(str::is_empty)
                    {
                        return Err(Error::rejected(format!(
                            "works deck slide {field} is required"
                        )));
                    }
                }
                if let Some(notes) = slide.get("notes") {
                    if notes.as_str().map(str::trim).is_none_or(str::is_empty) {
                        return Err(Error::rejected("works deck notes must be a string"));
                    }
                }
            }
        }
        "sheet" => {
            let columns = content_array(object, "columns")?;
            if columns.is_empty() {
                return Err(Error::rejected("works sheet columns are required"));
            }
            for column in columns {
                if column.as_str().map(str::trim).is_none_or(str::is_empty) {
                    return Err(Error::rejected("works sheet columns must be strings"));
                }
            }
            let rows = object
                .get("rows")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::rejected("works sheet rows are required"))?;
            for row in rows {
                let cells = row
                    .as_array()
                    .ok_or_else(|| Error::rejected("works sheet rows must be arrays"))?;
                if cells.len() != columns.len() {
                    return Err(Error::rejected("works sheet row must match its columns"));
                }
                for cell in cells {
                    if !cell.is_string() && !cell.is_number() {
                        return Err(Error::rejected(
                            "works sheet cells must be strings or numbers",
                        ));
                    }
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn content_array<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>> {
    let values = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::rejected(format!("works {key} are required")))?;
    if values.is_empty() {
        return Err(Error::rejected(format!("works {key} are required")));
    }
    Ok(values)
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
    if out.is_empty() {
        "content".to_string()
    } else {
        out
    }
}

fn now_iso() -> String {
    let seconds = now().floor().max(0.0) as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
        .map(|value| value.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

fn review_state(status: &str, reviewed: i64, revision: i64) -> &'static str {
    if status == "changes_requested" {
        "edited"
    } else if reviewed == 0 {
        "draft"
    } else if reviewed == revision {
        "reviewed"
    } else {
        "edited"
    }
}

fn export_state(status: &str, reviewed: i64, revision: i64, trashed: bool) -> bool {
    !trashed && status == "approved" && reviewed == revision
}

fn export_reason(status: &str, reviewed: i64, revision: i64) -> &'static str {
    if status == "changes_requested" {
        "Export is blocked until the requested changes are reviewed."
    } else if reviewed == revision && status == "approved" {
        "This revision is reviewed and ready for host export."
    } else {
        "Export is blocked until the current revision is reviewed."
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
                    return Err(Error::rejected(
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

    #[allow(clippy::type_complexity)]
    fn works_item(
        &self,
        conn: &Connection,
        context: &str,
        id: &str,
    ) -> Result<
        Option<(
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
        )>,
    > {
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
            self.works_item(conn, context, id)?.ok_or_else(|| {
                Error::invalid("app_works_missing", "works content was not found")
            })?;
        if trashed {
            return Err(Error::invalid(
                "app_works_missing",
                "works content was not found",
            ));
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
        let state = review_state(&status, reviewed, revision);
        let ready = export_state(&status, reviewed, revision, trashed);
        let updated_at = now_iso();
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
            "reviewState": state,
            "reviewStatus": status,
            "reviewRequestedRevision": pending,
            "exportState": if ready { "ready" } else { "blocked" },
            "exportReason": export_reason(&status, reviewed, revision),
            "updated": "just now",
            "updatedAt": updated_at,
            "owner": owner,
            "author": author,
            "tags": tags,
            "snippet": snippet,
            "contentDigest": content_digest,
            "_updated": updated,
        });
        if let Value::Object(content) = content {
            if let Value::Object(map) = &mut detail {
                for (key, value) in content {
                    map.insert(key, value);
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
        draft: &WorksDraft,
        actor: &str,
        key: &str,
        on_behalf_of: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let payload = draft.payload_digest();
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
                    return Err(Error::rejected("works content ID space is exhausted"));
                }
            }
            let tags = serde_json::to_string(&draft.tags)
                .map_err(|e| Error::internal(e.to_string()))?;
            let content = serde_json::to_string(&draft.content)
                .map_err(|e| Error::internal(e.to_string()))?;
            let digest = draft.content_digest();
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
                let value = detail["reviewState"].as_str().unwrap_or("");
                if review == "pending" {
                    if detail["reviewStatus"] != "pending" {
                        continue;
                    }
                } else if value != review {
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
            return Err(Error::rejected("works operations are required"));
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
                .ok_or_else(|| Error::invalid("app_works_missing", "works content was not found"))?;
            if item.8 {
                return Err(Error::invalid("app_works_missing", "works content was not found"));
            }
            if item.4 != if_revision {
                return Err(Error::rejected("works content revision is stale"));
            }
            let kind = item.0;
            let mut title = item.1;
            let mut content = self.works_content(tx, context, id, item.4)?;
            for operation in operations {
                let object = operation
                    .as_object()
                    .ok_or_else(|| Error::rejected("works operation must be an object"))?;
                match object.get("type").and_then(Value::as_str) {
                    Some("set_title") => {
                        let value = object
                            .get("title")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty() && value.len() <= TITLE_BYTES)
                            .ok_or_else(|| Error::rejected("set_title requires a bounded title"))?;
                        title = value.to_string();
                    }
                    Some("set_content") => {
                        content = object
                            .get("content")
                            .cloned()
                            .ok_or_else(|| Error::rejected("set_content requires content"))?;
                        validate_content(&kind, &content)?;
                    }
                    _ => return Err(Error::rejected("works operation is unsupported")),
                }
            }
            validate_content(&kind, &content)?;
            let next = if_revision + 1;
            let digest = material_digest(&json!({"kind": kind, "title": title, "content": content}));
            let content_json =
                serde_json::to_string(&content).map_err(|e| Error::internal(e.to_string()))?;
            let changed = tx
                .execute(
                    "UPDATE app_works_items SET title=?,revision=?,reviewed_revision=reviewed_revision,review_status='none',pending_revision=NULL,updated=? WHERE context_id=? AND id=? AND revision=? AND trashed=0",
                    params![title, next, now(), context, id, if_revision],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected("works content revision is stale"));
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
                .ok_or_else(|| Error::invalid("app_works_missing", "works content was not found"))?;
            if item.4 != if_revision {
                return Err(Error::rejected("works content revision is stale"));
            }
            let changed = tx
                .execute(
                    "UPDATE app_works_items SET trashed=?,updated=? WHERE context_id=? AND id=? AND revision=?",
                    params![if trashed { 1 } else { 0 }, now(), context, id, if_revision],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected("works content revision is stale"));
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
                .ok_or_else(|| Error::invalid("app_works_missing", "works content was not found"))?;
            if item.8 {
                return Err(Error::invalid("app_works_missing", "works content was not found"));
            }
            if item.4 != revision {
                return Err(Error::rejected("works content revision is stale"));
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
        if !matches!(decision, "approve" | "changes") {
            return Err(Error::rejected("works decision must be approve or changes"));
        }
        if let Some(note) = note {
            if note.len() > 2_000 {
                return Err(Error::rejected("works review note exceeds its bound"));
            }
        }
        self.write_tx(|tx| {
            let item = self
                .works_item(tx, context, id)?
                .ok_or_else(|| Error::invalid("app_works_missing", "works content was not found"))?;
            if item.8 {
                return Err(Error::invalid("app_works_missing", "works content was not found"));
            }
            if item.4 != revision {
                return Err(Error::rejected("works content revision is stale"));
            }
            if item.6 != "pending" || item.7 != Some(revision) {
                return Err(Error::rejected("works review is not pending for this revision"));
            }
            let author: String = tx
                .query_row(
                    "SELECT author FROM app_works_revisions WHERE context_id=? AND id=? AND revision=?",
                    params![context, id, revision],
                    |row| row.get(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if author == actor {
                return Err(Error::rejected(
                    "an author cannot approve their own revision",
                ));
            }
            if decision == "approve" {
                tx.execute(
                    "UPDATE app_works_items SET review_status='approved',reviewed_revision=?,pending_revision=NULL,updated=? WHERE context_id=? AND id=?",
                    params![revision, now(), context, id],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            } else {
                tx.execute(
                    "UPDATE app_works_items SET review_status='changes_requested',pending_revision=NULL,updated=? WHERE context_id=? AND id=?",
                    params![now(), context, id],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            }
            self.works_audit(
                tx,
                context,
                actor,
                on_behalf_of,
                if decision == "approve" { "review_approve" } else { "review_changes" },
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
        if format.trim().is_empty() || format.len() > 40 {
            return Err(Error::rejected(
                "works export format is required and bounded",
            ));
        }
        self.write_tx(|tx| {
            let item = self
                .works_item(tx, context, id)?
                .ok_or_else(|| Error::invalid("app_works_missing", "works content was not found"))?;
            if item.8 {
                return Err(Error::invalid("app_works_missing", "works content was not found"));
            }
            if item.4 != revision {
                return Err(Error::rejected("works content revision is stale"));
            }
            if item.6 != "approved" || item.5 != revision {
                return Err(Error::rejected("works export requires an approved current revision"));
            }
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
