//! Saved segments, exclusion lists, suppressions and frozen audiences (CAD-780).
//!
//! The audience engine over CAD-779 customer rows inside one
//! installation's record file. Exactly one base mode per audience —
//! all eligible customers, one saved segment, or a custom stable-ID
//! set — plus a saved exclusion list on any base, plus a final
//! exclusion union (invalid address, no consent, unsubscribed,
//! suppressed) that always applies, even to custom-selected IDs.
//!
//! Segments use a small allowlisted field/operator grammar evaluated
//! in Rust over loaded profiles — parameterized by construction, no
//! SQL string is ever built from a predicate, and cross-install or
//! cross-context references are unrepresentable. Preview returns
//! exact counts plus a bounded sample, never the full member list;
//! prepare freezes member IDs, digest, installation/context and
//! revision pins with a recipient ceiling. Any later drift (segment
//! edit, consent change, suppression change) invalidates the freeze.

use super::app_records::{
    email_shape_valid, source_shape_valid, tag_valid, CustomerProfile, ProfileRefused, RecordStore,
};
use super::app_runs::material_digest;
use super::StoreConn;
use super::*;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SEGMENT_PREDICATES_MAX: usize = 8;
pub const SEGMENT_NAME_BYTES: usize = 80;
pub const PREDICATE_VALUE_BYTES: usize = 120;
pub const EXCLUSION_MEMBERS_MAX: usize = 100;
pub const CUSTOM_IDS_MAX: usize = 200;
pub const SUPPRESSION_REASON_BYTES: usize = 80;
/// Frozen audiences never exceed this many recipients; prepare takes
/// a per-freeze ceiling at or under it.
pub const AUDIENCE_MAX: i64 = 500;
pub const SAMPLE_MAX: usize = 10;

const SEGMENT_FIELDS: &[&str] = &["tag", "source", "consent_email", "email_domain"];
const SEGMENT_OPS: &[&str] = &["eq", "ne"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    pub field: String,
    pub op: String,
    pub value: String,
}

impl Predicate {
    /// Parse an untrusted predicate. Refusals name the grammar, never
    /// the value — a wrong value must not echo private data.
    pub fn parse(body: &Value) -> std::result::Result<Self, ProfileRefused> {
        let predicate: Self = serde_json::from_value(body.clone()).map_err(|_| ProfileRefused)?;
        predicate.validate().map_err(|_| ProfileRefused)?;
        Ok(predicate)
    }

    fn validate(&self) -> Result<()> {
        const REFUSED: &str = "segment predicate exceeds its supported grammar";
        if !SEGMENT_FIELDS.contains(&self.field.as_str())
            || !SEGMENT_OPS.contains(&self.op.as_str())
        {
            return Err(Error::rejected(REFUSED));
        }
        if self.value.is_empty()
            || self.value.len() > PREDICATE_VALUE_BYTES
            || self.value.chars().any(char::is_control)
        {
            return Err(Error::rejected(REFUSED));
        }
        // Values that could only be markup or query fragments are
        // refused outright; the grammar has no use for them.
        if self.value.contains(['<', '>', '\'', '"', ';', '\\'])
            || self.value.contains("--")
            || self.value.to_lowercase().contains("select ")
        {
            return Err(Error::rejected(REFUSED));
        }
        match self.field.as_str() {
            "tag" => {
                if !tag_valid(&self.value) {
                    return Err(Error::rejected(REFUSED));
                }
            }
            "source" => {
                if !source_shape_valid(&self.value) {
                    return Err(Error::rejected(REFUSED));
                }
            }
            "consent_email" => {
                if !matches!(self.value.as_str(), "granted" | "denied" | "unknown") {
                    return Err(Error::rejected(REFUSED));
                }
            }
            "email_domain" => {
                let domain = self.value.to_lowercase();
                if !domain.contains('.')
                    || domain
                        .bytes()
                        .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.')))
                {
                    return Err(Error::rejected(REFUSED));
                }
            }
            _ => return Err(Error::rejected(REFUSED)),
        }
        Ok(())
    }

    fn matches(&self, profile: Option<&CustomerProfile>) -> bool {
        let Some(profile) = profile else {
            // An unparseable row never matches — not even a `ne`
            // predicate — so legacy rows cannot leak into a segment.
            return false;
        };
        let hit = match self.field.as_str() {
            "tag" => profile.tags.iter().any(|tag| tag == &self.value),
            "source" => profile.source.as_deref() == Some(self.value.as_str()),
            "consent_email" => profile.consent.email.as_str() == self.value.as_str(),
            "email_domain" => profile
                .email
                .as_deref()
                .and_then(|address| address.rsplit('@').next())
                .is_some_and(|domain| domain.to_lowercase() == self.value.to_lowercase()),
            _ => false,
        };
        if self.op == "ne" {
            !hit
        } else {
            hit
        }
    }
}

#[derive(Clone, Debug)]
pub enum AudienceBase {
    All,
    Segment { segment_id: String },
    Custom { customer_ids: Vec<String> },
}

impl AudienceBase {
    /// Parse an untrusted base. Exactly one mode; unknown modes,
    /// mixed fields and unshaped IDs are refused.
    pub fn parse(body: &Value) -> std::result::Result<Self, ProfileRefused> {
        let fields = body.as_object().ok_or(ProfileRefused)?;
        let mode = fields
            .get("mode")
            .and_then(Value::as_str)
            .ok_or(ProfileRefused)?;
        let base = match mode {
            "all" => {
                if fields.len() != 1 {
                    return Err(ProfileRefused);
                }
                Self::All
            }
            "segment" => {
                if fields.len() != 2 {
                    return Err(ProfileRefused);
                }
                let segment = fields
                    .get("segment_id")
                    .and_then(Value::as_str)
                    .ok_or(ProfileRefused)?;
                crate::proto::identifier(segment, "segment ID").map_err(|_| ProfileRefused)?;
                Self::Segment {
                    segment_id: segment.to_string(),
                }
            }
            "custom" => {
                if fields.len() != 2 {
                    return Err(ProfileRefused);
                }
                let ids = fields
                    .get("customer_ids")
                    .and_then(Value::as_array)
                    .ok_or(ProfileRefused)?;
                if ids.is_empty() || ids.len() > CUSTOM_IDS_MAX {
                    return Err(ProfileRefused);
                }
                let mut members = Vec::with_capacity(ids.len());
                for id in ids {
                    let id = id.as_str().ok_or(ProfileRefused)?;
                    crate::proto::identifier(id, "record ID").map_err(|_| ProfileRefused)?;
                    members.push(id.to_string());
                }
                members.sort();
                members.dedup();
                Self::Custom {
                    customer_ids: members,
                }
            }
            _ => return Err(ProfileRefused),
        };
        Ok(base)
    }

    fn canonical(&self) -> Value {
        match self {
            Self::All => json!({"mode": "all"}),
            Self::Segment { segment_id } => {
                json!({"mode": "segment", "segment_id": segment_id})
            }
            Self::Custom { customer_ids } => {
                json!({"mode": "custom", "customer_ids": customer_ids})
            }
        }
    }
}

fn segment_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SEGMENT_NAME_BYTES
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

fn suppression_reason_valid(reason: &str) -> bool {
    !reason.is_empty()
        && reason.len() <= SUPPRESSION_REASON_BYTES
        && !reason.chars().any(char::is_control)
}

struct CustomerRow {
    id: String,
    revision: i64,
    /// `None` for rows whose body no longer parses (legacy data
    /// predating shape checks or corruption): they never match a
    /// segment and always fall into the final invalid-address
    /// exclusion, so preview/prepare stay fail-closed instead of
    /// refusing the whole audience.
    profile: Option<CustomerProfile>,
}

struct Computation {
    base_ids: Vec<String>,
    exclusion_count: i64,
    excluded_invalid: i64,
    excluded_no_consent: i64,
    excluded_unsubscribed: i64,
    excluded_suppressed: i64,
    final_ids: Vec<String>,
    digest: String,
    pins: Value,
}

impl RecordStore {
    fn customers_in(conn: &impl super::StoreConn, context: &str) -> Result<Vec<CustomerRow>> {
        let found = conn
            .query_vec(
                "SELECT id,revision,body FROM app_records WHERE context_id=? ORDER BY id",
                [context],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut rows = Vec::new();
        for (id, revision, body) in found {
            let profile = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|stored| CustomerProfile::parse(&stored).ok());
            rows.push(CustomerRow {
                id,
                revision,
                profile,
            });
        }
        Ok(rows)
    }

    fn suppressions_in(
        conn: &impl super::StoreConn,
        context: &str,
    ) -> Result<(
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
    )> {
        let found = conn
            .query_vec(
                "SELECT kind,key FROM app_suppressions WHERE context_id=?",
                [context],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut emails = std::collections::HashSet::new();
        let mut customers = std::collections::HashSet::new();
        for (kind, key) in found {
            if kind == "email" {
                emails.insert(key);
            } else {
                customers.insert(key);
            }
        }
        Ok((emails, customers))
    }

    fn suppression_digest_in(conn: &impl super::StoreConn, context: &str) -> Result<String> {
        let rows = conn
            .query_vec(
                "SELECT kind,key FROM app_suppressions WHERE context_id=? ORDER BY kind,key",
                [context],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        Ok(material_digest(
            &json!({"domain": "cadence-app-suppressions-v1", "rows": rows}),
        ))
    }

    fn segment_in(
        conn: &impl super::StoreConn,
        context: &str,
        segment_id: &str,
    ) -> Result<(i64, String, Vec<Predicate>, String)> {
        let (revision, name, definition, digest): (i64, String, String, String) = conn
            .query_row(
                "SELECT revision,name,definition,digest FROM app_segments WHERE context_id=? AND id=?",
                params![context, segment_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
            .ok_or_else(|| Error::rejected("audience segment is unavailable for this installation and context"))?;
        let raw: Value = serde_json::from_str(&definition)
            .map_err(|_| Error::rejected("audience segment definition is corrupt"))?;
        let items = raw
            .as_array()
            .ok_or_else(|| Error::rejected("audience segment definition is corrupt"))?;
        let mut predicates = Vec::with_capacity(items.len());
        for item in items {
            predicates.push(
                Predicate::parse(item)
                    .map_err(|_| Error::rejected("audience segment definition is corrupt"))?,
            );
        }
        Ok((revision, name, predicates, digest))
    }

    fn exclusion_in(
        conn: &impl super::StoreConn,
        context: &str,
        list_id: &str,
    ) -> Result<(i64, String, Vec<String>, String)> {
        let (revision, name, members, digest): (i64, String, String, String) = conn
            .query_row(
                "SELECT revision,name,member_ids,digest FROM app_exclusions WHERE context_id=? AND id=?",
                params![context, list_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
            .ok_or_else(|| Error::rejected("audience exclusion list is unavailable for this installation and context"))?;
        let raw: Value = serde_json::from_str(&members)
            .map_err(|_| Error::rejected("audience exclusion list is corrupt"))?;
        let items = raw
            .as_array()
            .ok_or_else(|| Error::rejected("audience exclusion list is corrupt"))?;
        let mut ids = Vec::with_capacity(items.len());
        for item in items {
            let id = item
                .as_str()
                .ok_or_else(|| Error::rejected("audience exclusion list is corrupt"))?;
            crate::proto::identifier(id, "record ID")
                .map_err(|_| Error::rejected("audience exclusion list is corrupt"))?;
            ids.push(id.to_string());
        }
        Ok((revision, name, ids, digest))
    }

    /// Resolve base, saved exclusion list and the final exclusion
    /// union into deduplicated final IDs plus the digest and pins a
    /// freeze binds. Pure read; every guard lives here so preview,
    /// prepare and show-verification share one computation.
    fn compute_audience(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        base: &AudienceBase,
        exclusion_list_id: Option<&str>,
    ) -> Result<Computation> {
        let customers = Self::customers_in(conn, context)?;
        let by_id: std::collections::HashMap<&str, &CustomerRow> =
            customers.iter().map(|row| (row.id.as_str(), row)).collect();
        let (mut base_ids, segment_pin) = match base {
            AudienceBase::All => (
                customers
                    .iter()
                    .map(|row| row.id.clone())
                    .collect::<Vec<_>>(),
                Value::Null,
            ),
            AudienceBase::Segment { segment_id } => {
                let (revision, _, predicates, digest) =
                    Self::segment_in(conn, context, segment_id)?;
                let mut ids = Vec::new();
                for row in &customers {
                    if predicates
                        .iter()
                        .all(|rule| rule.matches(row.profile.as_ref()))
                    {
                        ids.push(row.id.clone());
                    }
                }
                (
                    ids,
                    json!({"segment_id": segment_id, "revision": revision, "digest": digest}),
                )
            }
            AudienceBase::Custom { customer_ids } => {
                for id in customer_ids {
                    if !by_id.contains_key(id.as_str()) {
                        return Err(Error::rejected(
                            "audience custom selection names an unknown customer",
                        ));
                    }
                }
                (customer_ids.clone(), Value::Null)
            }
        };
        base_ids.sort();
        base_ids.dedup();
        let (excluded_set, exclusion_pin) = match exclusion_list_id {
            None => (std::collections::HashSet::new(), Value::Null),
            Some(list_id) => {
                let (revision, _, ids, digest) = Self::exclusion_in(conn, context, list_id)?;
                (
                    ids.into_iter().collect::<std::collections::HashSet<_>>(),
                    json!({"exclusion_list_id": list_id, "revision": revision, "digest": digest}),
                )
            }
        };
        let (suppressed_emails, suppressed_customers) = Self::suppressions_in(conn, context)?;
        let mut exclusion_count = 0;
        let mut excluded_invalid = 0;
        let mut excluded_no_consent = 0;
        let mut excluded_unsubscribed = 0;
        let mut excluded_suppressed = 0;
        let mut final_ids = Vec::new();
        for id in &base_ids {
            if excluded_set.contains(id) {
                exclusion_count += 1;
                continue;
            }
            let row = by_id.get(id.as_str()).ok_or_else(|| {
                Error::internal("audience membership diverged from customer rows")
            })?;
            // The final union always applies — even to IDs the
            // operator selected explicitly. Rows whose body no
            // longer parses count as invalid addresses.
            let Some(profile) = row.profile.as_ref() else {
                excluded_invalid += 1;
                continue;
            };
            let address = profile.email.clone().unwrap_or_default();
            if profile
                .email
                .as_deref()
                .is_none_or(|email| !email_shape_valid(email))
            {
                excluded_invalid += 1;
                continue;
            }
            match profile.consent.email {
                crate::store::app_records::ConsentState::Denied => {
                    excluded_unsubscribed += 1;
                    continue;
                }
                crate::store::app_records::ConsentState::Unknown => {
                    excluded_no_consent += 1;
                    continue;
                }
                crate::store::app_records::ConsentState::Granted => {}
            }
            if suppressed_emails.contains(&address.to_lowercase())
                || suppressed_customers.contains(id)
            {
                excluded_suppressed += 1;
                continue;
            }
            final_ids.push(id.clone());
        }
        final_ids.sort();
        let suppression_digest = Self::suppression_digest_in(conn, context)?;
        let mut customer_revisions = serde_json::Map::new();
        for row in &customers {
            customer_revisions.insert(row.id.clone(), json!(row.revision));
        }
        let pins = json!({
            "base": base.canonical(),
            "segment": segment_pin,
            "exclusion": exclusion_pin,
            "customer_revisions": customer_revisions,
            "suppression_digest": suppression_digest,
        });
        let digest = material_digest(&json!({
            "domain": "cadence-app-audience-v1",
            "install_id": self.install(),
            "context_id": context,
            "pins": pins,
            "final_ids": final_ids,
        }));
        Ok(Computation {
            base_ids,
            exclusion_count,
            excluded_invalid,
            excluded_no_consent,
            excluded_unsubscribed,
            excluded_suppressed,
            final_ids,
            digest,
            pins,
        })
    }

    fn sample_in(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        ids: &[String],
    ) -> Result<Vec<Value>> {
        let mut sample = Vec::new();
        for id in ids.iter().take(SAMPLE_MAX) {
            let name: String = conn
                .query_row(
                    "SELECT body FROM app_records WHERE context_id=? AND id=?",
                    params![context, id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| Error::internal(e.to_string()))?
                .ok_or_else(|| {
                    Error::internal("audience membership diverged from customer rows")
                })?;
            let body: Value = serde_json::from_str(&name).unwrap_or(Value::Null);
            let display = body
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or("");
            sample.push(json!({"id": id, "display_name": display}));
        }
        Ok(sample)
    }

    fn preview_json(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        base: &AudienceBase,
        exclusion_list_id: Option<&str>,
        computed: &Computation,
    ) -> Result<Value> {
        let sample = self.sample_in(conn, context, &computed.final_ids)?;
        Ok(json!({
            "base": base.canonical(),
            "base_count": computed.base_ids.len(),
            "exclusion_list_id": exclusion_list_id,
            "exclusion_count": computed.exclusion_count,
            "final_excluded": {
                "invalid_email": computed.excluded_invalid,
                "no_consent": computed.excluded_no_consent,
                "unsubscribed": computed.excluded_unsubscribed,
                "suppressed": computed.excluded_suppressed,
            },
            "final_count": computed.final_ids.len(),
            "sample": sample,
            "digest": computed.digest,
        }))
    }

    pub fn app_segment_save(
        &self,
        context: &str,
        segment_id: &str,
        expected_revision: Option<i64>,
        name: &str,
        predicates: &[Predicate],
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(segment_id, "segment ID")?;
        if !segment_name_valid(name) {
            return Err(Error::rejected("audience segment name exceeds its bounds"));
        }
        if predicates.is_empty() || predicates.len() > SEGMENT_PREDICATES_MAX {
            return Err(Error::rejected(
                "audience segment predicates exceed their bound",
            ));
        }
        for rule in predicates {
            rule.validate()?;
        }
        let definition = Value::Array(
            predicates
                .iter()
                .map(|rule| json!({"field": rule.field, "op": rule.op, "value": rule.value}))
                .collect(),
        );
        let digest = material_digest(&json!({
            "domain": "cadence-app-segment-v1",
            "install_id": self.install(),
            "context_id": context,
            "segment_id": segment_id,
            "name": name,
            "definition": definition,
        }));
        let stored =
            serde_json::to_string(&definition).map_err(|e| Error::internal(e.to_string()))?;
        // `write_tx` holds `BEGIN IMMEDIATE` across the check + writes:
        // a racing saver blocks on the write lock first, so exactly one
        // revision wins and the loser is stale.
        self.write_tx(|tx| {
            let current: Option<i64> = tx
                .query_opt(
                    "SELECT revision FROM app_segments WHERE context_id=? AND id=?",
                    params![context, segment_id],
                    |r| r.get(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            let revision = match (current, expected_revision) {
                (None, None) => 1,
                (None, Some(_)) => {
                    return Err(Error::rejected(
                        "audience segment is unknown; save without an expected revision",
                    ));
                }
                (Some(_), None) => {
                    return Err(Error::rejected(
                        "audience segment already exists; name the observed revision",
                    ));
                }
                (Some(current), Some(expected)) => {
                    if current != expected {
                        return Err(Error::rejected("audience segment revision is stale"));
                    }
                    current
                        .checked_add(1)
                        .ok_or_else(|| Error::rejected("audience segment revision exhausted"))?
                }
            };
            if current.is_none() {
                tx.execute(
                    "INSERT INTO app_segments(context_id,id,revision,name,definition,digest,created,updated) VALUES(?,?,?,?,?,?,?,?)",
                    params![context, segment_id, revision, name, stored, digest, now(), now()],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            } else {
                let changed = tx
                    .execute(
                        "UPDATE app_segments SET revision=?,name=?,definition=?,digest=?,updated=? WHERE context_id=? AND id=? AND revision=?",
                        params![revision, name, stored, digest, now(), context, segment_id, current],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                if changed != 1 {
                    return Err(Error::rejected("audience segment revision is stale"));
                }
            }
            tx.execute(
                "INSERT INTO app_segment_revisions(context_id,segment_id,revision,definition,digest,actor,at) VALUES(?,?,?,?,?,'operator',?)",
                params![context, segment_id, revision, stored, digest, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok(revision)
        })?;
        Ok(json!({"segment": self.app_segment_show(context, segment_id)?["segment"]}))
    }

    pub fn app_segment_show(&self, context: &str, segment_id: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(segment_id, "segment ID")?;
        let conn = self.conn();
        let (revision, name, predicates, digest) = Self::segment_in(&conn, context, segment_id)?;
        Ok(json!({"segment": {
            "id": segment_id,
            "install_id": self.install(),
            "context_id": context,
            "revision": revision,
            "name": name,
            "predicates": predicates.iter().map(|rule| json!({"field": rule.field, "op": rule.op, "value": rule.value})).collect::<Vec<_>>(),
            "digest": digest,
        }}))
    }

    pub fn app_segment_list(&self, context: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id FROM app_segments WHERE context_id=? ORDER BY id")
            .map_err(|e| Error::internal(e.to_string()))?;
        let found = stmt
            .query_map([context], |r| r.get::<_, String>(0))
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut ids = Vec::new();
        for row in found {
            ids.push(row.map_err(|e| Error::internal(e.to_string()))?);
        }
        drop(stmt);
        let mut segments = Vec::with_capacity(ids.len());
        for id in &ids {
            let (revision, name, predicates, digest) = Self::segment_in(&conn, context, id)?;
            segments.push(json!({
                "id": id, "install_id": self.install(), "context_id": context,
                "revision": revision, "name": name,
                "predicates": predicates.iter().map(|rule| json!({"field": rule.field, "op": rule.op, "value": rule.value})).collect::<Vec<_>>(),
                "digest": digest,
            }));
        }
        Ok(json!({"segments": segments}))
    }

    pub fn app_exclusion_save(
        &self,
        context: &str,
        list_id: &str,
        expected_revision: Option<i64>,
        name: &str,
        customer_ids: &[String],
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(list_id, "exclusion list ID")?;
        if !segment_name_valid(name) {
            return Err(Error::rejected(
                "audience exclusion name exceeds its bounds",
            ));
        }
        if customer_ids.len() > EXCLUSION_MEMBERS_MAX {
            return Err(Error::rejected(
                "audience exclusion members exceed their bound",
            ));
        }
        for id in customer_ids {
            crate::proto::identifier(id, "record ID")?;
        }
        let mut members = customer_ids.to_vec();
        members.sort();
        members.dedup();
        let digest = material_digest(&json!({
            "domain": "cadence-app-exclusion-v1",
            "install_id": self.install(),
            "context_id": context,
            "list_id": list_id,
            "name": name,
            "member_ids": members,
        }));
        let stored = serde_json::to_string(&members).map_err(|e| Error::internal(e.to_string()))?;
        // `write_tx` holds `BEGIN IMMEDIATE` across the check + writes:
        // a racing saver blocks on the write lock first, so exactly one
        // revision wins and the loser is stale.
        self.write_tx(|tx| {
            let current: Option<i64> = tx
                .query_opt(
                    "SELECT revision FROM app_exclusions WHERE context_id=? AND id=?",
                    params![context, list_id],
                    |r| r.get(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            let revision = match (current, expected_revision) {
                (None, None) => 1,
                (None, Some(_)) => {
                    return Err(Error::rejected(
                        "audience exclusion list is unknown; save without an expected revision",
                    ));
                }
                (Some(_), None) => {
                    return Err(Error::rejected(
                        "audience exclusion list already exists; name the observed revision",
                    ));
                }
                (Some(current), Some(expected)) => {
                    if current != expected {
                        return Err(Error::rejected("audience exclusion revision is stale"));
                    }
                    current
                        .checked_add(1)
                        .ok_or_else(|| Error::rejected("audience exclusion revision exhausted"))?
                }
            };
            if current.is_none() {
                tx.execute(
                    "INSERT INTO app_exclusions(context_id,id,revision,name,member_ids,digest,created,updated) VALUES(?,?,?,?,?,?,?,?)",
                    params![context, list_id, revision, name, stored, digest, now(), now()],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            } else {
                let changed = tx
                    .execute(
                        "UPDATE app_exclusions SET revision=?,name=?,member_ids=?,digest=?,updated=? WHERE context_id=? AND id=? AND revision=?",
                        params![revision, name, stored, digest, now(), context, list_id, current],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                if changed != 1 {
                    return Err(Error::rejected("audience exclusion revision is stale"));
                }
            }
            tx.execute(
                "INSERT INTO app_exclusion_revisions(context_id,list_id,revision,member_ids,digest,actor,at) VALUES(?,?,?,?,?,'operator',?)",
                params![context, list_id, revision, stored, digest, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok(revision)
        })?;
        Ok(json!({"exclusion": self.app_exclusion_show(context, list_id)?["exclusion"]}))
    }

    pub fn app_exclusion_show(&self, context: &str, list_id: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(list_id, "exclusion list ID")?;
        let conn = self.conn();
        let (revision, name, ids, digest) = Self::exclusion_in(&conn, context, list_id)?;
        Ok(json!({"exclusion": {
            "id": list_id,
            "install_id": self.install(),
            "context_id": context,
            "revision": revision,
            "name": name,
            "member_ids": ids,
            "digest": digest,
        }}))
    }

    pub fn app_exclusion_list(&self, context: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id FROM app_exclusions WHERE context_id=? ORDER BY id")
            .map_err(|e| Error::internal(e.to_string()))?;
        let found = stmt
            .query_map([context], |r| r.get::<_, String>(0))
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut ids = Vec::new();
        for row in found {
            ids.push(row.map_err(|e| Error::internal(e.to_string()))?);
        }
        drop(stmt);
        let mut lists = Vec::with_capacity(ids.len());
        for id in &ids {
            let (revision, name, members, digest) = Self::exclusion_in(&conn, context, id)?;
            lists.push(json!({
                "id": id, "install_id": self.install(), "context_id": context,
                "revision": revision, "name": name, "member_ids": members, "digest": digest,
            }));
        }
        Ok(json!({"exclusions": lists}))
    }

    pub fn app_suppression_add(
        &self,
        context: &str,
        email: Option<&str>,
        customer_id: Option<&str>,
        reason: &str,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        if !suppression_reason_valid(reason) {
            return Err(Error::rejected(
                "audience suppression reason exceeds its bounds",
            ));
        }
        let (kind, key) = match (email, customer_id) {
            (Some(address), None) => {
                if !email_shape_valid(address) {
                    return Err(Error::rejected(
                        "audience suppression address exceeds its bounds",
                    ));
                }
                ("email", address.to_lowercase())
            }
            (None, Some(id)) => {
                crate::proto::identifier(id, "record ID")?;
                ("customer", id.to_string())
            }
            _ => {
                return Err(Error::rejected(
                    "audience suppression names exactly one of email or customer",
                ));
            }
        };
        let conn = self.conn();
        conn.execute(
            "INSERT INTO app_suppressions(context_id,kind,key,reason,at) VALUES(?,?,?,?,?) ON CONFLICT(context_id,kind,key) DO UPDATE SET reason=excluded.reason,at=excluded.at",
            params![context, kind, key, reason, now()],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        Ok(
            json!({"suppression": {"context_id": context, "kind": kind, "key": key, "reason": reason}}),
        )
    }

    pub fn app_suppression_remove(
        &self,
        context: &str,
        email: Option<&str>,
        customer_id: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let (kind, key) = match (email, customer_id) {
            (Some(address), None) => ("email", address.to_lowercase()),
            (None, Some(id)) => {
                crate::proto::identifier(id, "record ID")?;
                ("customer", id.to_string())
            }
            _ => {
                return Err(Error::rejected(
                    "audience suppression names exactly one of email or customer",
                ));
            }
        };
        let conn = self.conn();
        let removed = conn
            .execute(
                "DELETE FROM app_suppressions WHERE context_id=? AND kind=? AND key=?",
                params![context, kind, key],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        if removed != 1 {
            return Err(Error::rejected(
                "audience suppression is unknown for this installation and context",
            ));
        }
        Ok(json!({"removed": {"context_id": context, "kind": kind, "key": key}}))
    }

    pub fn app_suppression_list(&self, context: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT kind,key,reason FROM app_suppressions WHERE context_id=? ORDER BY kind,key",
            )
            .map_err(|e| Error::internal(e.to_string()))?;
        let found = stmt
            .query_map([context], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut rows = Vec::new();
        for row in found {
            let (kind, key, reason) = row.map_err(|e| Error::internal(e.to_string()))?;
            rows.push(json!({"context_id": context, "kind": kind, "key": key, "reason": reason}));
        }
        Ok(json!({"suppressions": rows}))
    }

    pub fn app_audience_preview(
        &self,
        context: &str,
        base: &AudienceBase,
        exclusion_list_id: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        if let Some(list) = exclusion_list_id {
            crate::proto::identifier(list, "exclusion list ID")?;
        }
        let conn = self.conn();
        let computed = self.compute_audience(&conn, context, base, exclusion_list_id)?;
        self.preview_json(&conn, context, base, exclusion_list_id, &computed)
    }

    /// Freeze the computed membership: member IDs, digest,
    /// installation/context and revision pins plus the recipient
    /// ceiling. A reused freeze ID behind identical bytes and ceiling
    /// replays; behind different bytes or a different ceiling it
    /// refuses before anything mutates.
    pub fn app_audience_prepare(
        &self,
        context: &str,
        freeze_id: &str,
        base: &AudienceBase,
        exclusion_list_id: Option<&str>,
        max_recipients: i64,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(freeze_id, "freeze ID")?;
        if let Some(list) = exclusion_list_id {
            crate::proto::identifier(list, "exclusion list ID")?;
        }
        if !(1..=AUDIENCE_MAX).contains(&max_recipients) {
            return Err(Error::rejected(
                "audience maximum recipients is out of bounds",
            ));
        }
        let conn = self.conn();
        let computed = self.compute_audience(&conn, context, base, exclusion_list_id)?;
        if computed.final_ids.len() as i64 > max_recipients {
            return Err(Error::rejected("audience exceeds its maximum recipients"));
        }
        if let Some(stored) = conn
            .query_row(
                "SELECT base,exclusion_list_id,member_ids,digest,max_recipients,pins,created FROM app_audience_freezes WHERE context_id=? AND freeze_id=?",
                params![context, freeze_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, f64>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
        {
            if stored.3 == computed.digest {
                // A replay binds the ceiling too: the same freeze ID
                // behind a different maximum is a different approval,
                // never a silent reuse of the prior ceiling.
                if stored.4 != max_recipients {
                    return Err(Error::rejected(
                        "audience freeze ceiling differs from the frozen ceiling",
                    ));
                }
                let sample = self.sample_in(&conn, context, &computed.final_ids)?;
                return Ok(json!({"freeze": {
                    "freeze_id": freeze_id, "install_id": self.install(), "context_id": context,
                    "base": base.canonical(), "exclusion_list_id": exclusion_list_id,
                    "final_count": computed.final_ids.len(), "max_recipients": stored.4,
                    "digest": stored.3, "sample": sample, "replayed": true,
                }}));
            }
            return Err(Error::rejected("audience freeze ID is already used"));
        }
        let members = serde_json::to_string(&computed.final_ids)
            .map_err(|e| Error::internal(e.to_string()))?;
        let base_text =
            serde_json::to_string(&base.canonical()).map_err(|e| Error::internal(e.to_string()))?;
        let pins_text =
            serde_json::to_string(&computed.pins).map_err(|e| Error::internal(e.to_string()))?;
        let created = now();
        conn.execute(
            "INSERT INTO app_audience_freezes(context_id,freeze_id,base,exclusion_list_id,member_ids,digest,max_recipients,pins,created) VALUES(?,?,?,?,?,?,?,?,?)",
            params![context, freeze_id, base_text, exclusion_list_id, members, computed.digest, max_recipients, pins_text, created],
        )
        .map_err(|e| Error::internal(e.to_string()))?;
        let sample = self.sample_in(&conn, context, &computed.final_ids)?;
        Ok(json!({"freeze": {
            "freeze_id": freeze_id, "install_id": self.install(), "context_id": context,
            "base": base.canonical(), "exclusion_list_id": exclusion_list_id,
            "final_count": computed.final_ids.len(), "max_recipients": max_recipients,
            "digest": computed.digest, "sample": sample, "replayed": false, "created": created,
        }}))
    }

    /// Show a freeze with its live validity: recompute over the same
    /// base and exclusion and compare digest plus every pin. Any
    /// segment edit, exclusion edit, consent change, suppression
    /// change or membership drift reports `valid: false` with the
    /// first cause — a stale approval must never read as current.
    /// The full member list is never returned; delivery reads the
    /// frozen row server-side in a later slice.
    pub fn app_audience_show(&self, context: &str, freeze_id: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(freeze_id, "freeze ID")?;
        let conn = self.conn();
        let (base_text, exclusion_list_id, members, digest, max_recipients, pins_text, created): (
            String,
            Option<String>,
            String,
            String,
            i64,
            String,
            f64,
        ) = conn
            .query_row(
                "SELECT base,exclusion_list_id,member_ids,digest,max_recipients,pins,created FROM app_audience_freezes WHERE context_id=? AND freeze_id=?",
                params![context, freeze_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
            .ok_or_else(|| Error::rejected("audience freeze is unavailable for this installation and context"))?;
        let base_raw: Value = serde_json::from_str(&base_text)
            .map_err(|_| Error::rejected("audience freeze is corrupt"))?;
        let base = AudienceBase::parse(&base_raw)
            .map_err(|_| Error::rejected("audience freeze is corrupt"))?;
        let frozen_pins: Value = serde_json::from_str(&pins_text)
            .map_err(|_| Error::rejected("audience freeze is corrupt"))?;
        let frozen_ids: Vec<String> = serde_json::from_str(&members)
            .map_err(|_| Error::rejected("audience freeze is corrupt"))?;
        let current = self.compute_audience(&conn, context, &base, exclusion_list_id.as_deref())?;
        let mut drift: Option<&str> = None;
        if current.pins["segment"] != frozen_pins["segment"] {
            drift = Some("segment revision changed");
        } else if current.pins["exclusion"] != frozen_pins["exclusion"] {
            drift = Some("exclusion list revision changed");
        } else if current.pins["suppression_digest"] != frozen_pins["suppression_digest"] {
            drift = Some("suppression set changed");
        } else if current.pins["customer_revisions"] != frozen_pins["customer_revisions"] {
            drift = Some("customer records changed");
        } else if current.digest != digest {
            drift = Some("audience membership changed");
        }
        let sample = self.sample_in(&conn, context, &frozen_ids)?;
        Ok(json!({"freeze": {
            "freeze_id": freeze_id, "install_id": self.install(), "context_id": context,
            "base": base.canonical(), "exclusion_list_id": exclusion_list_id,
            "final_count": frozen_ids.len(), "max_recipients": max_recipients,
            "digest": digest, "sample": sample, "created": created,
        },
        "current_digest": current.digest,
        "current_final_count": current.final_ids.len(),
        "valid": drift.is_none(),
        "drift": drift,
        }))
    }
}

impl Store {
    /// Best-effort audit event for an audience write that already
    /// committed inside its installation file. Advisory like
    /// `note_app_record`: counts and digests only, never member IDs.
    pub fn note_app_audience(&self, install: &str, context: &str, action: &str, digest: &str) {
        if let Err(e) = self.write_tx(|tx| {
                Self::event(
                    &*tx,
                    Self::DAEMON_STREAM,
                    "app_audience_changed",
                    json!({"install_id": install, "context_id": context, "action": action, "digest": digest, "actor": "operator"}),
                )
            })
        {
            eprintln!("audience audit event skipped: {e}");
        }
    }
}
