//! Authoritative bound-view reads (CAD-867): the operator-only RPC the
//! descriptor-driven view surface uses for *live* rows.
//!
//! Unlike the per-source reads, this entry point re-proves the request
//! against the ONE verified installed snapshot `with_runtime_snapshot`
//! produces — the bundle digest, the descriptor digest and the binding
//! digest are compared before any row is read, so a stale or
//! hand-edited descriptor/binding can never authorize a read the
//! installed bundle no longer declares. The `with_runtime_snapshot`
//! callback holds the PM lock for the *entire* read span — digest
//! checks AND the typed producer read — so an upgrade can never slip
//! between authorization and data fetch. Lock order stays the existing
//! PM -> release -> store order; this handler never calls
//! `rpc_app_record`/`rpc_app_context`/`rpc_app_runs` (those re-take the
//! PM lock and would deadlock/nest).
//!
//! The caller never names a host `source`, a SQL query, an actor, a
//! URL or a record id outside the bound scope: it carries the
//! installation, the descriptor `view` id, the read `op`, the three
//! expected digests and — where the view's source needs it — the
//! `context_id` and `record_id`. The `source` and the allowed `op`
//! come from the installed binding, never the request.
//!
//! Projection is TYPED and CLOSED, never a generic raw-JSON walk: each
//! source deserializes its real producer shape (`CustomerProfile`,
//! the `app_runs` snapshot receipt) into a fixed field→cell map keyed
//! by the contract's projection keys, then the binding's declared
//! fields pick from that closed map. Required producer fields refuse
//! on a wrong shape; only the contract's optional cells (`email`,
//! `phone`, `source`, `consent.sms`, run `subject`/`context_id`) may
//! omit — and omit they do, never a raw `null`. Every corrupt producer
//! row fails the WHOLE read, never a partial-success skip.

use super::*;
use crate::issue::app_catalog::workspace;
use crate::issue::{app_binding, app_view};
use crate::store::app_records::{CustomerProfile, RecordStore};
use serde_json::Map;
use std::collections::BTreeMap;

/// Request fields the bound read admits.
const REQUEST_KEYS: &[&str] = &[
    "install_id",
    "view_id",
    "op",
    "digest",
    "view_descriptor_digest",
    "view_binding_digest",
    "context_id",
    "record_id",
    "limit",
    "cursor",
    "query",
];

/// `list`/`show` — the only two ops `app-bindings/v1` knows.
const READ_OPS: &[&str] = &["list", "show"];

/// A digest claim is `sha256:<64 lowercase hex>` — anything else is a
/// schema refusal, never a silent skip.
fn digest_shape(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

/// The typed, validated scope a request resolves to after the schema
/// gate. `source`-independent fields are proven first; the per-source
/// readers then enforce which selectors that source actually admits.
/// `install`/`view` are consumed at the call site (proven against the
/// snapshot), so they are not stored here.
struct ViewRead<'a> {
    op: &'a str,
    context: Option<&'a str>,
    record: Option<&'a str>,
    query: Option<&'a str>,
    limit: Option<i64>,
    cursor: Option<&'a str>,
}

impl Shared {
    /// The bound read. `params` is the caller's request; `peer_pid`
    /// proves the operator connection before any field is trusted.
    pub(super) fn rpc_app_view_read(
        self: &Arc<Self>,
        _method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app view read", params, peer_pid)?;
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app view read payload must be an object"))?;
        if fields.keys().any(|k| !REQUEST_KEYS.contains(&k.as_str())) {
            return Err(Error::rejected(
                "app view read payload has unsupported fields",
            ));
        }
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation ID")?;
        let view = required_str(params, "view_id")?;
        let op = required_str(params, "op")?;
        if !READ_OPS.contains(&op) {
            return Err(Error::rejected("app view read op is not a bound read"));
        }
        // Every digest claim is a required `sha256:`-shaped string; a
        // missing, wrong-typed or malformed digest is a schema refusal.
        for field in ["digest", "view_descriptor_digest", "view_binding_digest"] {
            let value = required_str(params, field)?;
            if !digest_shape(value) {
                return Err(Error::rejected("app view read digest pin is invalid"));
            }
        }
        let digest = required_str(params, "digest")?;
        let descriptor_digest = required_str(params, "view_descriptor_digest")?;
        let binding_digest = required_str(params, "view_binding_digest")?;
        // Optional scope fields: never null, always strings.
        for field in ["context_id", "record_id", "cursor", "query"] {
            if let Some(value) = fields.get(field) {
                if value.is_null() {
                    return Err(Error::rejected("app view read scope fields are never null"));
                }
                if !value.is_string() {
                    return Err(Error::rejected(
                        "app view read scope fields must be strings",
                    ));
                }
            }
        }
        let context = optional_str(params, "context_id");
        let record = optional_str(params, "record_id");
        let cursor = optional_str(params, "cursor");
        let query = optional_str(params, "query");
        if let Some(c) = context {
            crate::proto::identifier(c, "context ID")?;
        }
        if let Some(r) = record {
            crate::proto::identifier(r, "record ID")?;
        }
        if let Some(c) = cursor {
            crate::proto::identifier(c, "record cursor")?;
        }
        if let Some(q) = query {
            if q.is_empty() || q.len() > 120 || q.chars().any(char::is_control) {
                return Err(Error::rejected("app view read query is out of bounds"));
            }
        }
        // Bounded pagination — only a `list` may carry `limit`.
        let limit = match fields.get("limit") {
            None => None,
            Some(Value::Number(n)) => Some(
                n.as_u64()
                    .and_then(|v| i64::try_from(v).ok())
                    .filter(|v| (1..=crate::store::app_records::RECORD_LIMIT).contains(v))
                    .ok_or_else(|| Error::rejected("app view read page limit is out of bounds"))?,
            ),
            Some(_) => return Err(Error::rejected("app view read page limit is out of bounds")),
        };
        // A `show` op never pages/queries; a `list` op never names a row.
        if op == "show" && (limit.is_some() || cursor.is_some() || query.is_some()) {
            return Err(Error::rejected("a show read does not paginate or query"));
        }
        if op == "list" && record.is_some() {
            return Err(Error::rejected("a list read does not name one record"));
        }
        let read = ViewRead {
            op,
            context,
            record,
            query,
            limit,
            cursor,
        };

        let pm = self.pm_at(&self.pm_dir()?)?;
        // `with_runtime_snapshot` holds the PM lock for the whole
        // callback — digest re-proof AND the typed producer read both
        // happen under it, so a racing upgrade is ordered before or
        // after the entire read, never torn inside it.
        workspace::with_runtime_snapshot(&pm, install, |row, files| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // All three expected digests must equal THIS verified
            // snapshot's — a stale or hand-edited pair never authorizes
            // a read the installed bundle no longer serves.
            if row["digest"].as_str() != Some(digest) {
                return Err(Error::rejected("installation digest is stale"));
            }
            if row["view_descriptor_digest"].as_str() != Some(descriptor_digest) {
                return Err(Error::rejected("view descriptor digest is stale"));
            }
            if row["view_binding_digest"].as_str() != Some(binding_digest) {
                return Err(Error::rejected("view binding digest is stale"));
            }
            // Re-parse the SAME snapshot's descriptor + binding and
            // re-prove the pair with `validate_against` — the op/source/
            // field selection comes from the parsed contract, never the
            // receipt's served bytes. The manifest comes from the
            // verified snapshot's app.md.
            let descriptor = view_descriptor(files)?;
            let binding_doc = view_binding_doc(files)?;
            let manifest = snapshot_manifest(files)?;
            app_binding::validate_against(&binding_doc, &manifest, Some(&descriptor))
                .map_err(|e| Error::rejected(format!("installed binding/descriptor pair: {e}")))?;
            let declared = descriptor
                .views
                .iter()
                .find(|v| v.id == view)
                .ok_or_else(|| Error::rejected("view is not in the installed descriptor"))?;
            // A form view is a disabled preview — it can never carry a
            // binding or serve a read.
            if declared.kind == "form" {
                return Err(Error::rejected("a form view serves no live rows"));
            }
            let vb = binding_doc
                .bindings
                .iter()
                .find(|b| b.view == view)
                .ok_or_else(|| {
                    Error::rejected("view has no installed binding — it serves no live rows")
                })?;
            if !vb.ops.iter().any(|o| o == op) {
                return Err(Error::rejected(
                    "the installed binding does not admit this read op",
                ));
            }
            // The source is host-derived from the installed binding,
            // never caller-named.
            let mut out = match vb.source.as_str() {
                "customers" => self.read_customers(install, vb, &read)?,
                "caption-runs" => self.read_caption_runs(install, vb, &read)?,
                other => {
                    return Err(Error::rejected(format!(
                        "installed binding names unsupported source '{other}'"
                    )))
                }
            };
            // Echo the pins the read was authorized under plus the
            // selected view/op — a stale reply is detectable by a later
            // staleness check.
            out["view_id"] = json!(view);
            out["op"] = json!(op);
            out["digest"] = json!(digest);
            out["view_descriptor_digest"] = json!(descriptor_digest);
            out["view_binding_digest"] = json!(binding_digest);
            Ok(out)
        })
    }

    /// `customers` — a real live context owned by the install is
    /// required and proven before the record store opens; the records
    /// live in that install's own file. `list` pages/searches bounded
    /// via `app_record_list_paged`; `show` needs `record_id`.
    fn read_customers(
        &self,
        install_id: &str,
        vb: &app_binding::ViewBinding,
        read: &ViewRead<'_>,
    ) -> Result<Value> {
        let context = read
            .context
            .ok_or_else(|| Error::rejected("a customers read needs its live context"))?;
        // Prove the context belongs to THIS install and is active
        // before any record file opens.
        let _ = self.store.app_context_proof(install_id, context)?;
        let records = RecordStore::open(&self.state_dir, install_id)?;
        match read.op {
            "list" => {
                let page = records.app_record_list_paged(
                    context,
                    read.query,
                    read.limit
                        .unwrap_or(crate::store::app_records::RECORD_LIMIT),
                    read.cursor,
                )?;
                let raw_records = page["records"].as_array().ok_or_else(|| {
                    Error::rejected("record list producer returned no records array")
                })?;
                let mut rows = Vec::with_capacity(raw_records.len());
                for record in raw_records {
                    rows.push(project_customer(record, vb)?);
                }
                // Page metadata is typed, never defaulted-on-error.
                let truncated = page["truncated"].as_bool().ok_or_else(|| {
                    Error::rejected("record list producer returned no truncated flag")
                })?;
                Ok(json!({
                    "rows": rows,
                    "truncated": truncated,
                    "next_cursor": page["next_cursor"].clone(),
                }))
            }
            "show" => {
                let record_id = read
                    .record
                    .ok_or_else(|| Error::rejected("a customers show needs its record id"))?;
                let record = records.app_record_show(context, record_id)?;
                let revision = record["record"]["revision"]
                    .as_i64()
                    .filter(|revision| *revision > 0)
                    .ok_or_else(|| {
                        Error::rejected("customer show returned no host record revision")
                    })?;
                Ok(json!({
                    "rows": [project_customer(&record["record"], vb)?],
                    "record_revision": revision,
                }))
            }
            other => Err(Error::rejected(format!(
                "customers does not admit op '{other}'"
            ))),
        }
    }

    /// `caption-runs` — runs may be contextless; every list/show is
    /// bound to the verified installation and the supplied optional
    /// context. A shown run must belong to both the install and the
    /// requested context; a supplied context must itself be a live,
    /// owned context of the install (`app_context_proof`), not merely
    /// a string that happens to match the run's `context_id`.
    fn read_caption_runs(
        &self,
        install_id: &str,
        vb: &app_binding::ViewBinding,
        read: &ViewRead<'_>,
    ) -> Result<Value> {
        if read.limit.is_some() || read.cursor.is_some() || read.query.is_some() {
            return Err(Error::rejected(
                "a caption-runs read does not paginate or query",
            ));
        }
        // A supplied context must be a real, live, owned context of
        // this install — proven before any run row is read.
        if let Some(context) = read.context {
            let _ = self.store.app_context_proof(install_id, context)?;
        }
        match read.op {
            "list" => {
                let list = self
                    .store
                    .app_run_list_filtered(Some(install_id), read.context)?;
                let raw_runs = list["runs"]
                    .as_array()
                    .ok_or_else(|| Error::rejected("run list producer returned no runs array"))?;
                let mut rows = Vec::with_capacity(raw_runs.len());
                for run in raw_runs {
                    rows.push(project_run(run, vb)?);
                }
                Ok(json!({"rows": rows}))
            }
            "show" => {
                let run_id = read
                    .record
                    .ok_or_else(|| Error::rejected("a caption-runs show needs its run id"))?;
                let run = self.store.app_run_show(run_id)?;
                if run["install_id"].as_str() != Some(install_id) {
                    return Err(Error::rejected("run does not belong to this installation"));
                }
                if let Some(context) = read.context {
                    if run["context_id"].as_str() != Some(context) {
                        return Err(Error::rejected("run does not belong to this context"));
                    }
                }
                Ok(json!({"rows": [project_run(&run, vb)?]}))
            }
            other => Err(Error::rejected(format!(
                "caption-runs does not admit op '{other}'"
            ))),
        }
    }
}

/// Parse the installed descriptor from the verified snapshot — the
/// same parse `describe` runs, never the receipt's served bytes.
fn view_descriptor(files: &BTreeMap<String, String>) -> Result<app_view::Descriptor> {
    app_view::parse_descriptor(
        files
            .get(app_view::REL_PATH)
            .ok_or_else(|| Error::rejected("installed descriptor is absent from this bundle"))?,
    )
    .map_err(|e| Error::rejected(format!("installed descriptor: {e}")))
}

/// Parse the installed binding from the verified snapshot. An install
/// with no binding file (a descriptor-only package) has no live read.
fn view_binding_doc(files: &BTreeMap<String, String>) -> Result<app_binding::Binding> {
    app_binding::parse_binding(
        files
            .get(app_binding::REL_PATH)
            .ok_or_else(|| Error::rejected("installed binding is absent from this bundle"))?,
    )
    .map_err(|e| Error::rejected(format!("installed binding: {e}")))
}

/// The bundle's manifest (`app.md`) re-parsed from the SAME verified
/// snapshot files, for the `validate_against` pair proof.
fn snapshot_manifest(files: &BTreeMap<String, String>) -> Result<crate::issue::app::Manifest> {
    crate::issue::app::parse_manifest(
        files
            .get("app.md")
            .ok_or_else(|| Error::rejected("installed bundle carries no app.md"))?,
    )
    .map_err(|e| Error::rejected(format!("installed manifest: {e}")))
}

/* ---------------- typed closed projections ---------------- */

/// A closed field→cell map. The typed producer is deserialized once,
/// then each contract projection key maps to a `Some(cell)` /
/// `None` (omit) — never a generic raw-JSON traversal. `record_id`
/// keys the host's row `id` handle (the adapter rename).
type CellMap = BTreeMap<&'static str, Option<Value>>;

/// Emit a row keyed by descriptor field id from a closed cell map:
/// each bound field's `key` selects a projected producer cell; `None`
/// (an absent optional) omits the field, `Some` emits it. A bound key
/// outside the closed map is a contract-violation refusal.
fn emit_row(vb: &app_binding::ViewBinding, cells: &CellMap) -> Result<Value> {
    let mut out = Map::new();
    for field in &vb.fields {
        match cells.get(field.key.as_str()) {
            // Bound key outside this source's closed projection — a
            // contract violation (validate_against should have caught
            // it; refuse loudly rather than silently omit).
            None => {
                return Err(Error::rejected(format!(
                    "binding key '{}' is outside the source projection",
                    field.key
                )))
            }
            // Absent optional cell — omit, never emit a raw null.
            Some(None) => continue,
            Some(Some(cell)) => {
                out.insert(field.field.clone(), cell.clone());
            }
        }
    }
    Ok(Value::Object(out))
}

/// A cell string must be a real produced string; a wrong shape is a
/// producer-integrity refusal.
fn cell_text(v: &Value) -> Result<Value> {
    v.as_str()
        .map(|s| Value::String(s.to_string()))
        .ok_or_else(|| Error::rejected("a scalar producer emitted a non-string value"))
}

/// A tags cell must be a real list of strings.
fn cell_tags(v: &Value) -> Result<Value> {
    let items = v
        .as_array()
        .ok_or_else(|| Error::rejected("a list producer emitted a non-array value"))?;
    let mut tags = Vec::with_capacity(items.len());
    for item in items {
        let s = item
            .as_str()
            .ok_or_else(|| Error::rejected("a list cell emitted a non-string item"))?;
        tags.push(Value::String(s.to_string()));
    }
    Ok(Value::Array(tags))
}

/// An enum/consent cell must be a produced string inside its domain.
fn cell_enum(v: &Value, domain: &[&str]) -> Result<Value> {
    let text = v
        .as_str()
        .ok_or_else(|| Error::rejected("an enum producer emitted a non-string value"))?;
    if !domain.contains(&text) {
        return Err(Error::rejected(
            "an enum producer emitted a value outside its domain",
        ));
    }
    Ok(Value::String(text.to_string()))
}

/// The `customers` projection: deserialize the typed `CustomerProfile`
/// from the record's `profile`, then build the closed cell map. Every
/// required field (`record_id`/`display_name`/`tags`/`consent.email`)
/// must be present and correctly shaped; the optional fields
/// (`email`/`phone`/`source`/`consent.sms`) are `Some(cell)`/`None`.
/// A corrupt profile (non-object, wrong shape, or body that fails
/// `CustomerProfile::parse`) refuses the whole read — never a skip.
fn project_customer(record: &Value, vb: &app_binding::ViewBinding) -> Result<Value> {
    // The producer's row `id` is the `record_id` handle.
    let id = record["id"]
        .as_str()
        .ok_or_else(|| Error::rejected("record row carries no id"))?;
    // Deserialize the typed profile — a raw `email:null` still parses
    // to `None` and omits, which raw traversal would not prove.
    let profile: CustomerProfile = serde_json::from_value(record["profile"].clone())
        .map_err(|_| Error::rejected("record integrity refused"))?;
    let consent = &profile.consent;
    let mut cells: CellMap = BTreeMap::new();
    cells.insert("record_id", Some(json!(id)));
    cells.insert(
        "display_name",
        Some(cell_text(&json!(profile.display_name))?),
    );
    cells.insert("tags", Some(cell_tags(&json!(profile.tags))?));
    cells.insert(
        "consent.email",
        Some(cell_enum(
            &json!(consent.email.as_str()),
            &["granted", "denied", "unknown"],
        )?),
    );
    // Optional text cells — `None` omits the field entirely.
    for (key, opt) in [
        ("email", &profile.email),
        ("phone", &profile.phone),
        ("source", &profile.source),
    ] {
        cells.insert(
            key,
            match opt {
                Some(v) => Some(cell_text(&json!(v))?),
                None => None,
            },
        );
    }
    cells.insert(
        "consent.sms",
        match &consent.sms {
            Some(state) => Some(cell_enum(
                &json!(state.as_str()),
                &["granted", "denied", "unknown"],
            )?),
            None => None,
        },
    );
    emit_row(vb, &cells)
}

/// The `caption-runs` projection: the run row's `snapshot` must be a
/// well-formed produced object — a real schema/`inputs`/`workflow`
/// shape whose `material_digest` equals the row's stored
/// `snapshot_digest` — before any bound field is projected. A corrupt
/// snapshot (non-object, wrong shape, or a digest that does not match
/// the snapshot bytes) refuses the whole read, never a skip.
fn project_run(run: &Value, vb: &app_binding::ViewBinding) -> Result<Value> {
    // Required producer fields, correctly typed.
    let id = run["id"]
        .as_str()
        .ok_or_else(|| Error::rejected("run row carries no id"))?;
    let state = run["state"]
        .as_str()
        .ok_or_else(|| Error::rejected("run row carries no state"))?;
    if ![
        "awaiting_approval",
        "approved",
        "running",
        "succeeded",
        "failed",
        "cancelled",
    ]
    .contains(&state)
    {
        return Err(Error::rejected("run row carries an unknown state"));
    }
    let snapshot_digest = run["snapshot_digest"]
        .as_str()
        .ok_or_else(|| Error::rejected("run row carries no snapshot digest"))?;
    // `snapshot` must be an object — a string/scalar there is the
    // corrupt-producer shape the strict list test refuses on.
    let snapshot = &run["snapshot"];
    if !snapshot.is_object() {
        return Err(Error::rejected("run snapshot is not a produced object"));
    }
    // Integrity: the stored digest must equal the snapshot's real
    // material digest — a forged/tampered snapshot refuses.
    if crate::store::app_runs::material_digest(snapshot) != snapshot_digest {
        return Err(Error::rejected("run snapshot integrity digest mismatch"));
    }
    // `workflow`/`inputs`/`context` are optional objects, but when
    // PRESENT they must be real objects — a wrong-typed value is a
    // corrupt producer shape, never a silent omission. Nested optional
    // leaves (`title`,`subject`,`context.id`,`context_id`) are `None`
    // only when absent-or-null; a present wrong-typed value refuses.
    let obj_at = |v: &Value, key: &str| -> Result<Option<Map<String, Value>>> {
        match v.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(m)) => Ok(Some(m.clone())),
            Some(_) => Err(Error::rejected(format!(
                "run snapshot '{key}' is not a produced object"
            ))),
        }
    };
    // Optional string at a key: absent/null -> None; present non-string
    // -> refusal (never a silent-omit of a wrong-typed value).
    let opt_str = |v: &Value, key: &str| -> Result<Option<Value>> {
        match v.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(json!(s))),
            Some(_) => Err(Error::rejected(format!(
                "an optional producer leaf '{key}' emitted a non-string value"
            ))),
        }
    };
    let workflow = obj_at(snapshot, "workflow")?;
    let inputs = obj_at(snapshot, "inputs")?;
    let context = obj_at(snapshot, "context")?;

    let mut cells: CellMap = BTreeMap::new();
    cells.insert("id", Some(json!(id)));
    cells.insert("state", Some(json!(state)));
    cells.insert("snapshot_digest", Some(json!(snapshot_digest)));
    cells.insert("context_id", opt_str(run, "context_id")?);
    cells.insert(
        "snapshot.context.id",
        match &context {
            Some(c) => opt_str(&Value::Object(c.clone()), "id")?,
            None => None,
        },
    );
    cells.insert(
        "snapshot.workflow.title",
        match &workflow {
            Some(w) => opt_str(&Value::Object(w.clone()), "title")?,
            None => None,
        },
    );
    cells.insert(
        "snapshot.inputs.subject",
        match &inputs {
            Some(i) => opt_str(&Value::Object(i.clone()), "subject")?,
            None => None,
        },
    );
    emit_row(vb, &cells)
}
