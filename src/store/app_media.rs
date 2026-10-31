//! CAD-1365: the native workspace media catalog — metadata only.
//!
//! One row per retained logical asset the workspace's governed media
//! pipeline produced. The table is a bounded PROJECTION: it carries the
//! catalog fields a mounted `screen-media.v1` screen may list
//! (`id`/`title`/`kind`/`lifecycle`/`revision`/`creator`/timestamps) —
//! never original locators, custody paths, provider credentials,
//! prompts or bytes. Those stay behind their own admitted custody
//! doors.
//!
//! Boundary: the authenticated workspace's own daemon store is the
//! tenant — the caller's mount proves the workspace; no request field
//! selects one. `install_id` records which installation produced the
//! row as provenance, not a read partition: permitted collaborators in
//! the same workspace read the same catalog. `creator` is a bounded
//! tagged-JSON attribution (`{kind:"user",id:<verified native
//! BoardUser.sub>,label?}`, `{kind:"operator"}` or `{kind:"unknown"}`)
//! the producing writer records from verified identity — never a bare
//! user id, token, hash, UUID or email, and never read as authority.
//! `lifecycle` is the draft/published curation lifecycle, not a
//! private/shared vault boundary.
//!
//! The only write admitted here is `app_media_publish`'s atomic
//! accepted-intent step (`app_media_publish_in`): a draft→published
//! metadata move (lifecycle, revision +1, `updated`) plus the frozen
//! accepted receipt, in one sealed tx. Generation writes land with
//! their own admitted acceptance. The row is re-validated on read — a
//! malformed stored row refuses the page rather than surfacing forged
//! fields to a frame.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::{Store, StoreConn, WriteTxn};
use crate::error::{Error, Result};

/// The schema (v45): one row per cataloged workspace media asset,
/// plus the accepted publish intents — one ACCEPTED request id, its
/// frozen `(asset_id, expected_revision)` terms and the exact accepted
/// result JSON. Metadata only — no locator, byte, secret or prompt
/// column exists. A refused attempt never reaches this table, so a
/// request id is never reserved by a failure.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_media_assets(
    id TEXT PRIMARY KEY,
    install_id TEXT NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('image','video')),
    lifecycle TEXT NOT NULL CHECK(lifecycle IN ('draft','published')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    creator TEXT NOT NULL,
    created REAL NOT NULL,
    updated REAL NOT NULL);
CREATE INDEX IF NOT EXISTS app_media_assets_created
    ON app_media_assets(created, id);
CREATE TABLE IF NOT EXISTS app_media_publish_requests(
    request_id TEXT PRIMARY KEY,
    asset_id TEXT NOT NULL,
    expected_revision INTEGER NOT NULL,
    result TEXT NOT NULL,
    created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS app_media_asset_origins(
    asset_id TEXT PRIMARY KEY REFERENCES app_media_assets(id),
    receipt_id TEXT NOT NULL UNIQUE REFERENCES app_capability_results(id));
";

/// Additive lineage belongs to a new migration, not the shipped v45 catalog.
pub(super) const SCHEMA_V50: &str = "
CREATE TABLE IF NOT EXISTS app_media_lineage(
    asset_id TEXT PRIMARY KEY REFERENCES app_media_assets(id),
    source_asset_id TEXT NOT NULL REFERENCES app_media_assets(id),
    source_sha256 TEXT NOT NULL);
";

/// The daemon-minted id grammar — `med-` + 32 lowercase hex. A stored
/// row carrying anything else is refused rather than projected.
pub fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("med-")
        && id[4..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// One cataloged asset row (metadata projection only).
pub struct MediaAsset {
    pub id: String,
    pub install_id: String,
    pub title: String,
    pub kind: String,
    pub lifecycle: String,
    pub revision: i64,
    pub creator: String,
    pub created: f64,
    pub updated: f64,
    pub source_asset_id: Option<String>,
}

const TITLE_MAX: usize = 200;
/// Bounds on a claimed `original_artifact` descriptor's `ref` and the
/// native media job id — bounded control-free text, never a locator a
/// reader may dereference.
const ORIGINAL_REF_MAX: usize = 256;
const MEDIA_JOB_ID_MAX: usize = 128;
/// Bound on the serialized tagged creator JSON a stored row may carry —
/// a small metadata cap, not an identity grammar: a real native
/// `BoardUser.sub` fits comfortably inside it, so canonical ids are
/// never truncated.
const CREATOR_JSON_MAX: usize = 4 * 1024;
const CREATOR_LABEL_MAX: usize = 200;
const INSTALL_MAX: usize = 128;

/// The closed wire form of a stored creator tag: exactly the app port's
/// `Creator` union — `{kind:"user",id,label?}`, `{kind:"operator"}`
/// or `{kind:"unknown"}`. `deny_unknown_fields` keeps the set closed,
/// so a stored row carrying anything else refuses rather than coerces.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatorWire {
    kind: String,
    id: Option<String>,
    label: Option<String>,
}

/// A validated stored creator attribution. `User.id` is the verified
/// native board subject (`BoardUser.sub`) the producing writer
/// recorded — the reader never invents, derives or truncates it, and a
/// legacy or malformed attribution stays `unknown`, never a person.
enum Creator {
    User { id: String, label: Option<String> },
    Operator,
    Unknown,
}

impl Creator {
    /// Parse the stored tagged-JSON creator under the closed grammar.
    /// Anything outside it — a bare user id, an unknown kind, an extra
    /// field — is malformed stored data, not a person.
    fn parse(stored: &str) -> Result<Self> {
        let malformed = || {
            Error::rejected("the media catalog holds a malformed creator — refusing to project it")
        };
        let wire: CreatorWire = serde_json::from_str(stored).map_err(|_| malformed())?;
        match wire.kind.as_str() {
            "user" => {
                let id = wire.id.ok_or_else(malformed)?;
                if id.is_empty()
                    || id.chars().any(char::is_control)
                    || wire.label.as_deref().is_some_and(|l| {
                        l.is_empty()
                            || l.chars().count() > CREATOR_LABEL_MAX
                            || l.chars().any(char::is_control)
                    })
                {
                    return Err(malformed());
                }
                Ok(Creator::User {
                    id,
                    label: wire.label,
                })
            }
            "operator" if wire.id.is_none() && wire.label.is_none() => Ok(Creator::Operator),
            "unknown" if wire.id.is_none() && wire.label.is_none() => Ok(Creator::Unknown),
            _ => Err(malformed()),
        }
    }

    /// The structured projection — the exact shape the app port's
    /// `Creator` union carries, never a serialized string.
    fn to_json(&self) -> Value {
        match self {
            Creator::User { id, label } => match label {
                Some(l) => json!({"kind": "user", "id": id, "label": l}),
                None => json!({"kind": "user", "id": id}),
            },
            Creator::Operator => json!({"kind": "operator"}),
            Creator::Unknown => json!({"kind": "unknown"}),
        }
    }

    /// The stored tagged-JSON column form — a validated attribution
    /// re-serialized under the same closed grammar `parse` enforces, so
    /// the row re-validates on every read.
    fn stored(&self) -> String {
        self.to_json().to_string()
    }

    /// The creator a frozen run snapshot declares. Any shape outside the
    /// closed union is malformed frozen data — the record refuses rather
    /// than cataloging a forged attribution. Absent is honest `unknown`.
    fn from_snapshot(snapshot: &Value) -> Result<Self> {
        match snapshot.get("creator") {
            None | Some(Value::Null) => Ok(Creator::Unknown),
            Some(creator) => Creator::parse(
                &serde_json::to_string(creator)
                    .map_err(|_| Error::rejected("run snapshot creator is malformed"))?,
            )
            .map_err(|_| Error::rejected("run snapshot creator is malformed")),
        }
    }
}

/// The closed wire form of a claimed `original_artifact` provenance
/// descriptor (schema 1). `deny_unknown_fields` keeps the set closed —
/// a descriptor carrying anything else refuses rather than coerces.
/// These five fields are metadata the receipt already freezes; they
/// never become a locator, a download authority or custody bytes.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalDescriptor {
    schema: u64,
    #[serde(rename = "ref")]
    reference: String,
    sha256: String,
    bytes: u64,
    media_type: String,
}

/// The native media job id grammar — the trusted adapter's `med_`
/// family, bounded control-free. A claimed job id outside it is
/// malformed provenance, never normalized.
fn valid_media_job_id(id: &str) -> bool {
    id.len() >= 4
        && id.len() <= MEDIA_JOB_ID_MAX
        && id.starts_with("med_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Canonical `sha256:` + 64 lowercase hex.
fn canonical_sha256(digest: &str) -> Option<&str> {
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    .then_some(hex)
}

fn shown_sha256(digest: &str) -> &str {
    if canonical_sha256(digest).is_some() {
        digest
    } else {
        "unrecognized"
    }
}

/// Names the pinned digest, the stored custody digest, and the original
/// producer digest the pin is required to use.
fn media_pin_digest_refusal(pin: &str, original: &str, custody: &str) -> String {
    let pin_shown = shown_sha256(pin);
    let original_shown = shown_sha256(original);
    let custody_shown = shown_sha256(custody);
    if pin == custody && canonical_sha256(pin).is_some() {
        format!(
            "media reference does not match the retained original: pin {pin_shown} is the stored custody digest {custody_shown}; the required digest is the original producer digest {original_shown}"
        )
    } else {
        format!(
            "media reference does not match the retained original: pin {pin_shown} matches neither digest; the required digest is the original producer digest {original_shown} (stored custody digest is {custody_shown})"
        )
    }
}

/// The validated provenance behind one eligible `media.generated.image`
/// receipt: the derivative descriptor agrees with the retained custody
/// BLOB the record carries, and the closed original descriptor's ref
/// agrees with the result's job id and original digest.
fn checked_image_origin(
    record: &super::app_capabilities::AppCapabilityRecord<'_>,
) -> Result<Option<()>> {
    let malformed = || {
        Error::rejected(
            "media.generated.image original provenance is malformed — \
             refusing the receipt",
        )
    };
    let result = record.result;
    if result["kind"] != "media.generated.image" {
        return Ok(None);
    }
    if result.get("original_artifact").is_none() {
        return Ok(None);
    }
    // Eligible: the claimed original descriptor must check end to end —
    // a present-but-null or malformed descriptor refuses, it is never
    // silently dropped.
    let descriptor: OriginalDescriptor =
        serde_json::from_value(result["original_artifact"].clone()).map_err(|_| malformed())?;
    let digest = canonical_sha256(&descriptor.sha256).ok_or_else(malformed)?;
    let Some((ref_job, ref_digest)) = descriptor.reference.split_once('.') else {
        return Err(malformed());
    };
    let job_id = result["job_id"].as_str().ok_or_else(malformed)?;
    let (asset_mime, asset_bytes) = record.asset.ok_or_else(malformed)?;
    if descriptor.schema != 1
        || descriptor.bytes == 0
        || descriptor.reference.is_empty()
        || descriptor.reference.len() > ORIGINAL_REF_MAX
        || descriptor.reference.chars().any(char::is_control)
        || !matches!(
            descriptor.media_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp"
        )
        || !valid_media_job_id(job_id)
        || ref_job != job_id
        || ref_digest != digest
        || descriptor.reference != format!("{job_id}.{digest}")
        // The derivative the receipt describes must be the retained one.
        || result["asset_media_type"] != json!(asset_mime)
        || !matches!(
            asset_mime,
            "image/png" | "image/jpeg" | "image/webp"
        )
        || result["asset_sha256"]
            != json!(super::app_runs::artifact_digest(asset_bytes))
    {
        return Err(malformed());
    }
    Ok(Some(()))
}

impl MediaAsset {
    /// Refuse a stored row that could not have been written by the
    /// catalog's own rules — the projection never surfaces malformed
    /// persisted data to a frame.
    fn checked(self) -> Result<Self> {
        if !valid_id(&self.id) {
            return Err(Error::rejected(
                "the media catalog holds a malformed asset id — refusing to project it",
            ));
        }
        if self.title.is_empty()
            || self.title.chars().count() > TITLE_MAX
            || self.title.chars().any(|c| c.is_control())
            || self.install_id.is_empty()
            || self.install_id.len() > INSTALL_MAX
            || self.creator.len() > CREATOR_JSON_MAX
            || !matches!(self.kind.as_str(), "image" | "video")
            || !matches!(self.lifecycle.as_str(), "draft" | "published")
            || self.revision <= 0
            || self
                .source_asset_id
                .as_deref()
                .is_some_and(|id| !valid_id(id))
            || !self.created.is_finite()
            || !self.updated.is_finite()
        {
            return Err(Error::rejected(
                "the media catalog holds a malformed asset row — refusing to project it",
            ));
        }
        Creator::parse(&self.creator)?;
        Ok(self)
    }

    /// The bounded catalog projection — the fields `app_media_read` may
    /// emit. No locator, secret, prompt or byte ever appears here.
    pub fn projection(&self) -> Value {
        let mut out = json!({
            "id": self.id,
            "title": self.title,
            "kind": self.kind,
            "lifecycle": self.lifecycle,
            "revision": self.revision,
            "creator": Creator::parse(&self.creator)
                .map(|c| c.to_json())
                .unwrap_or_else(|_| json!({"kind": "unknown"})),
            "created": self.created,
            "updated": self.updated,
        });
        if let Some(source) = &self.source_asset_id {
            out["lineage"] = json!([source]);
        }
        out
    }
}

/// The catalog pagination cursor: an opaque, bounded encoding of the
/// last emitted row's `(created, id)` keyset position. Callers treat it
/// as opaque; the daemon re-validates it on every page.
const CURSOR_MAX: usize = 160;

fn cursor_encode(created: f64, id: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{created}:{id}").as_bytes())
}

fn cursor_decode(cursor: &str) -> Result<(f64, String)> {
    use base64::Engine;
    if cursor.is_empty() || cursor.len() > CURSOR_MAX {
        return Err(Error::rejected(
            "media catalog cursor is not a valid cursor",
        ));
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_bytes())
        .map_err(|_| Error::rejected("media catalog cursor is not a valid cursor"))?;
    let text = String::from_utf8(raw)
        .map_err(|_| Error::rejected("media catalog cursor is not a valid cursor"))?;
    let (created, id) = text
        .split_once(':')
        .ok_or_else(|| Error::rejected("media catalog cursor is not a valid cursor"))?;
    let created: f64 = created
        .parse()
        .map_err(|_| Error::rejected("media catalog cursor is not a valid cursor"))?;
    if !created.is_finite() || !valid_id(id) {
        return Err(Error::rejected(
            "media catalog cursor is not a valid cursor",
        ));
    }
    Ok((created, id.to_string()))
}

impl Store {
    /// One bounded page of the workspace media catalog, newest first —
    /// keyset-paginated on `(created DESC, id ASC)`. `limit` is already
    /// contract-bounded by the caller (1..=20); a `cursor` resumes after
    /// the last emitted row. Returns the checked page and the opaque
    /// `next_cursor` (`None` when the page is the last). An empty
    /// catalog is a valid result, never a refusal.
    ///
    /// This is the STORE-WIDE page: every installation's rows, drafts
    /// included. A mounting app reads [`Self::app_media_catalog_for`].
    pub fn app_media_catalog(
        &self,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<(Vec<MediaAsset>, Option<String>)> {
        self.app_media_catalog_page(None, limit, cursor)
    }

    /// CAD-1962: the page one mounted installation may list — published
    /// rows from any installation, plus that installation's OWN drafts.
    /// The workspace library is the curated published subset; another
    /// app's unpublished draft is its work in progress, not a library
    /// entry, so it is not listed here. The store stays the workspace:
    /// a published row is visible regardless of which installation
    /// produced it, and `install_id` remains provenance.
    pub fn app_media_catalog_for(
        &self,
        install: &str,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<(Vec<MediaAsset>, Option<String>)> {
        self.app_media_catalog_page(Some(install), limit, cursor)
    }

    fn app_media_catalog_page(
        &self,
        install: Option<&str>,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<(Vec<MediaAsset>, Option<String>)> {
        let after = match cursor {
            Some(c) => Some(cursor_decode(c)?),
            None => None,
        };
        let conn = self.conn();
        let rows = match after {
            Some((created, id)) => conn.query_vec(
                "SELECT a.id,a.install_id,a.title,a.kind,a.lifecycle,a.revision,a.creator,a.created,a.updated,l.source_asset_id
                 FROM app_media_assets a LEFT JOIN app_media_lineage l ON l.asset_id=a.id
                 WHERE (?4 IS NULL OR a.lifecycle='published' OR a.install_id=?4)
                   AND (a.created < ?1 OR (a.created = ?1 AND a.id > ?2))
                 ORDER BY a.created DESC, a.id ASC LIMIT ?3",
                params![created, id, limit + 1, install],
                row_media_catalog,
            )?,
            None => conn.query_vec(
                "SELECT a.id,a.install_id,a.title,a.kind,a.lifecycle,a.revision,a.creator,a.created,a.updated,l.source_asset_id
                 FROM app_media_assets a LEFT JOIN app_media_lineage l ON l.asset_id=a.id
                 WHERE (?2 IS NULL OR a.lifecycle='published' OR a.install_id=?2)
                 ORDER BY a.created DESC, a.id ASC LIMIT ?1",
                params![limit + 1, install],
                row_media_catalog,
            )?,
        };
        let mut page = Vec::new();
        for row in rows {
            page.push(row.checked()?);
        }
        let next = if page.len() as i64 > limit {
            page.pop(); // the extra probe row, not emitted
            let edge = &page[page.len() - 1];
            Some(cursor_encode(edge.created, &edge.id))
        } else {
            None
        };
        Ok((page, next))
    }

    /// CAD-1962: one catalog row by id, with its lineage pointer — the
    /// detail read the `asset` input kind returns. Store-wide: the mount
    /// proved the workspace, so permitted collaborators read any
    /// workspace asset regardless of which installation produced it;
    /// `install_id` is provenance, not a read partition. An absent id is
    /// `None` (the caller refuses it as unknown), and a stored row that
    /// could not have been written by this module refuses rather than
    /// projecting. A mounting app uses [`Self::app_media_asset_for`].
    pub fn app_media_asset(&self, id: &str) -> Result<Option<MediaAsset>> {
        self.app_media_asset_page(None, id)
    }

    /// CAD-1962: the detail read one mounted installation may make —
    /// the same published-plus-own-drafts rule as
    /// [`Self::app_media_catalog_for`]. Another installation's draft
    /// resolves to `None`, exactly as an unknown id does, so the caller
    /// cannot tell an unpublished asset from a missing one.
    pub fn app_media_asset_for(&self, install: &str, id: &str) -> Result<Option<MediaAsset>> {
        self.app_media_asset_page(Some(install), id)
    }

    fn app_media_asset_page(&self, install: Option<&str>, id: &str) -> Result<Option<MediaAsset>> {
        if !valid_id(id) {
            return Ok(None);
        }
        let conn = self.conn();
        let row = conn.query_opt(
            "SELECT a.id,a.install_id,a.title,a.kind,a.lifecycle,a.revision,a.creator,a.created,a.updated,l.source_asset_id
             FROM app_media_assets a LEFT JOIN app_media_lineage l ON l.asset_id=a.id
             WHERE a.id=?1 AND (?2 IS NULL OR a.lifecycle='published' OR a.install_id=?2)",
            params![id, install],
            row_media_catalog,
        )?;
        row.map(MediaAsset::checked).transpose()
    }

    /// CAD-1365 atomic producer association, run inside
    /// `app_capability_record`'s already-open write tx AFTER the new
    /// immutable receipt row exists and BEFORE its event — one commit
    /// covers receipt, asset and origin, and a refusal here rolls the
    /// receipt back with them.
    ///
    /// Eligibility is narrow: only the trusted adapter's
    /// `kind:"media.generated.image"` result that carries its claimed
    /// `original_artifact` descriptor catalogs an original. Every other
    /// result — non-image kinds, image outcomes with no original
    /// provenance — records its receipt exactly as before and invents
    /// no catalog row. An eligible outcome whose provenance fails its
    /// closed checks refuses the whole record; it is never silently
    /// skipped. The replay path returns the existing receipt before
    /// this runs, so an identical record leaves ONE asset.
    ///
    /// The run row is read inside THIS tx: the stored snapshot must
    /// hash to both `snapshot_digest` and the approved digest before
    /// any attribution — a corrupt or post-freeze-rewritten snapshot
    /// refuses atomically. `creator` is taken ONLY from that verified
    /// frozen snapshot; an absent legacy attribution is `unknown`, and
    /// a malformed one refuses. No caller field, actor, frame or person
    /// is consulted. The new row is a draft metadata projection — title
    /// is the constant "Generated image" (never the prompt), the id is
    /// daemon-minted `med-<32 lowercase hex>`, and no locator, byte,
    /// ref or receipt field reaches it; the asset↔receipt link lives
    /// only in the private `app_media_asset_origins` table.
    pub(crate) fn app_media_record_origin_in(
        tx: &WriteTxn<'_>,
        record: &super::app_capabilities::AppCapabilityRecord<'_>,
    ) -> Result<()> {
        if checked_image_origin(record)?.is_none() {
            return Ok(());
        }
        let (install_id, snapshot, snapshot_digest, approved_digest): (
            String,
            String,
            String,
            Option<String>,
        ) = tx.query_row(
            "SELECT install_id,snapshot,snapshot_digest,approved_digest
                 FROM app_runs WHERE id=?1",
            params![record.run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let snapshot: Value = serde_json::from_str(&snapshot)
            .map_err(|_| Error::rejected("run snapshot is malformed"))?;
        if super::app_runs::material_digest(&snapshot) != snapshot_digest
            || approved_digest.as_deref() != Some(snapshot_digest.as_str())
        {
            return Err(Error::rejected(
                "run snapshot does not hash to its approved digest",
            ));
        }
        let creator = Creator::from_snapshot(&snapshot)?.stored();
        let at = super::now();
        let asset = MediaAsset {
            id: format!("med-{}", uuid::Uuid::new_v4().simple()),
            install_id,
            title: "Generated image".to_string(),
            kind: "image".to_string(),
            lifecycle: "draft".to_string(),
            revision: 1,
            creator,
            created: at,
            updated: at,
            source_asset_id: None,
        }
        .checked()?;
        tx.execute(
            "INSERT INTO app_media_assets(
                id,install_id,title,kind,lifecycle,revision,creator,created,updated)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                asset.id,
                asset.install_id,
                asset.title,
                asset.kind,
                asset.lifecycle,
                asset.revision,
                asset.creator,
                asset.created,
                asset.updated,
            ],
        )?;
        tx.execute(
            "INSERT INTO app_media_asset_origins(asset_id,receipt_id)
             VALUES(?1,?2)",
            params![asset.id, record.id],
        )?;
        if record.result.get("source_asset").is_some() {
            let malformed =
                || Error::rejected("claimed image lineage is malformed — refusing the receipt");
            let source = record
                .result
                .get("source_asset")
                .and_then(Value::as_object)
                .ok_or_else(malformed)?;
            if source.len() != 3
                || !source.contains_key("asset_id")
                || !source.contains_key("sha256")
                || !source.contains_key("ref")
            {
                return Err(malformed());
            }
            let source_id = source
                .get("asset_id")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            let source_sha = source
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            let source_ref = source
                .get("ref")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            let source_hex = canonical_sha256(source_sha).ok_or_else(malformed)?;
            let reference_ok = source_ref.split_once('.').is_some_and(|(job, digest)| {
                valid_media_job_id(job) && digest == source_hex && !source_ref.contains("://")
            });
            if !valid_id(source_id) || source_id == asset.id || !reference_ok {
                return Err(malformed());
            }
            let source_revision: i64 = tx
                .query_opt(
                    "SELECT revision FROM app_media_assets WHERE id=?1",
                    params![source_id],
                    |row| row.get(0),
                )?
                .ok_or_else(malformed)?;
            if source_revision <= 0 {
                return Err(malformed());
            }
            tx.execute(
                "INSERT INTO app_media_lineage(asset_id,source_asset_id,source_sha256)
                 VALUES(?1,?2,?3)",
                params![asset.id, source_id, source_sha],
            )?;
            let unchanged: i64 = tx.query_row(
                "SELECT revision FROM app_media_assets WHERE id=?1",
                params![source_id],
                |row| row.get(0),
            )?;
            if unchanged != source_revision {
                return Err(malformed());
            }
        }
        Ok(())
    }

    /// `app_media_publish`'s atomic accepted-intent step, run inside
    /// the caller's already-open sealed write tx so the replay check,
    /// the revision CAS and the receipt insert are one commit.
    ///
    /// Order is the contract's: an existing `request_id` row replays
    /// its stored result byte-for-byte on identical
    /// `(asset_id, expected_revision)` and refuses changed intent under
    /// it — before any asset row is read. A fresh id then loads the
    /// asset, refuses an absent row, a stale `expected_revision`
    /// (checked BEFORE lifecycle so a moved revision classifies as
    /// stale even on an already-published row), or a non-draft row,
    /// and only then runs the CAS — `WHERE revision = expected` keeps
    /// a concurrent mover honest — flips lifecycle, bumps revision by
    /// one and refreshes `updated`; every other column is untouched.
    /// The accepted result JSON is persisted in the SAME tx: a failed
    /// attempt rolls back together and never reserves its request id.
    /// `expected_revision`/`request_id`/`asset_id` are grammar-checked
    /// by the RPC layer before this runs; they are re-checked on the
    /// stored rows, never trusted from the request.
    /// CAD-1365: resolve the private provenance for `asset_id`'s
    /// retained original — fail closed at every hop, and never trust a
    /// request field or a bare id:
    ///
    /// - the catalog asset must exist and re-validate (id grammar,
    ///   bounded fields, closed creator tag);
    /// - `app_media_asset_origins` must link it to a receipt — an
    ///   unlinked legacy asset refuses;
    /// - `asset_material_in` re-proves the receipt end to end (result
    ///   JSON, derivative BLOB digest/bounds, material digest);
    /// - the result must be an eligible `media.generated.image` carrying
    ///   a closed `original_artifact` descriptor (schema 1, canonical
    ///   `job.<sha256>` ref agreeing with the job id and declared digest,
    ///   positive size, allowlisted image MIME);
    /// - the producing run must exist, its stored snapshot must hash to
    ///   `snapshot_digest` AND `approved_digest` must equal it (the
    ///   approved material), and `run.install_id` must equal the asset's
    ///   producing `install_id` — an inconsistent origin refuses;
    /// - the frozen snapshot's `capabilities` map must carry the
    ///   receipt's slot with a parseable binding proof — the frozen
    ///   slot/config/digest the daemon re-checks against the CURRENT
    ///   binding/connection at the producing install.
    ///
    /// No creator or mount-install partition applies here: the catalog
    /// is store-wide and the caller-side proof lives in the mount.
    /// CAD-1482: catalog revision plus the stored original digest, with no
    /// provider traffic. A missing asset, a stale revision or a digest that
    /// is not the retained original refuses. CAD-1508: a pin of the stored
    /// custody derivative names that digest and the original producer digest
    /// the pin must use.
    pub(crate) fn app_media_pin(
        &self,
        asset_id: &str,
        revision: i64,
        sha256: &str,
    ) -> Result<MediaOriginalProof> {
        let image = self.app_media_context_image(asset_id)?;
        if image.original.sha256 != sha256 {
            return Err(Error::rejected(media_pin_digest_refusal(
                sha256,
                &image.original.sha256,
                &image.digest,
            )));
        }
        if image.asset.revision != revision {
            return Err(Error::rejected(
                "media reference does not match the retained original",
            ));
        }
        Ok(image.original)
    }

    pub(crate) fn app_media_revision(&self, asset_id: &str) -> Result<i64> {
        self.read_tx(|conn| {
            conn.query_row(
                "SELECT revision FROM app_media_assets WHERE id=?1",
                params![asset_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("media asset is not in the workspace catalog"))
        })
    }

    /// The catalog child recorded for this run's receipt, not the newest
    /// child of the same source.
    pub(crate) fn app_media_lineage_for_run(&self, run_id: &str) -> Result<Option<Value>> {
        self.read_tx(|conn| {
            let row = conn
                .query_row(
                    "SELECT a.id, a.revision, l.source_asset_id, l.source_sha256
                     FROM app_capability_results r
                     JOIN app_media_asset_origins o ON o.receipt_id = r.id
                     JOIN app_media_assets a ON a.id = o.asset_id
                     LEFT JOIN app_media_lineage l ON l.asset_id = a.id
                     WHERE r.run_id=?1",
                    params![run_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()?;
            Ok(
                row.map(|(asset_id, revision, source_asset_id, source_sha256)| {
                    json!({
                        "asset_id": asset_id,
                        "revision": revision,
                        "source_asset_id": source_asset_id,
                        "source_sha256": source_sha256,
                    })
                }),
            )
        })
    }

    pub(crate) fn app_media_original(&self, asset_id: &str) -> Result<MediaOriginalProof> {
        self.app_media_context_image(asset_id)
            .map(|image| image.original)
    }

    /// One verified receipt read supplies the original pin and its retained
    /// display material. A custody derivative need not hash to the original:
    /// `asset_material_in` proves both through the immutable receipt instead.
    pub(crate) fn app_media_context_image(&self, asset_id: &str) -> Result<MediaContextImage> {
        self.read_tx(|conn| {
            let row: Option<MediaAsset> = conn.query_opt(
                "SELECT id,install_id,title,kind,lifecycle,revision,creator,created,updated
                 FROM app_media_assets WHERE id=?1",
                params![asset_id],
                row_media_asset,
            )?;
            let Some(row) = row else {
                return Err(Error::rejected(
                    "media asset is not in the workspace catalog",
                ));
            };
            let asset = row.checked()?;
            let receipt_id: Option<String> = conn.query_opt(
                "SELECT receipt_id FROM app_media_asset_origins WHERE asset_id=?1",
                params![asset.id],
                |r| r.get(0),
            )?;
            let Some(receipt_id) = receipt_id else {
                return Err(Error::rejected(
                    "media asset has no retained original provenance",
                ));
            };
            // Re-proves the receipt's material digest over result AND
            // derivative before a byte is trusted.
            let (mut receipt, derivative) =
                super::app_capabilities::asset_material_in(&conn, &receipt_id)?;
            let Value::String(material_digest) = receipt["asset"]["digest"].take() else {
                return Err(Error::rejected("retained image digest is malformed"));
            };
            let media_type = match receipt["asset"]["media_type"].as_str() {
                Some("image/png") => "image/png",
                Some("image/jpeg") => "image/jpeg",
                Some("image/webp") => "image/webp",
                _ => return Err(Error::rejected("retained image MIME is malformed")),
            };
            let result_text: String = conn.query_row(
                "SELECT result FROM app_capability_results WHERE id=?1",
                params![receipt_id],
                |r| r.get(0),
            )?;
            let result: Value = serde_json::from_str(&result_text)
                .map_err(|_| Error::rejected("media origin receipt is malformed"))?;
            let run_id = receipt["run_id"]
                .as_str()
                .ok_or_else(|| Error::rejected("media origin receipt is malformed"))?;
            let slot = receipt["slot"]
                .as_str()
                .ok_or_else(|| Error::rejected("media origin receipt is malformed"))?;
            // The same closed descriptor checks `app_media_record_origin_in`
            // enforced at write — a malformed stored record refuses, it is
            // never reinterpreted.
            let descriptor: OriginalDescriptor =
                serde_json::from_value(result["original_artifact"].clone()).map_err(|_| {
                    Error::rejected("media original provenance descriptor is malformed")
                })?;
            let digest = canonical_sha256(&descriptor.sha256)
                .ok_or_else(|| Error::rejected("media original digest is malformed"))?;
            let Some((ref_job, ref_digest)) = descriptor.reference.split_once('.') else {
                return Err(Error::rejected("media original ref is malformed"));
            };
            let job_id = result["job_id"].as_str().unwrap_or_default();
            if result["kind"] != "media.generated.image"
                || descriptor.schema != 1
                || descriptor.bytes == 0
                || descriptor.reference.is_empty()
                || descriptor.reference.len() > ORIGINAL_REF_MAX
                || descriptor.reference.chars().any(char::is_control)
                || !matches!(
                    descriptor.media_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp"
                )
                || !valid_media_job_id(job_id)
                || ref_job != job_id
                || ref_digest != digest
                || descriptor.reference != format!("{job_id}.{digest}")
            {
                return Err(Error::rejected(
                    "media original provenance descriptor is malformed",
                ));
            }
            let run = Self::app_run_show_in(&conn, run_id)?;
            if run["install_id"] != asset.install_id {
                return Err(Error::rejected(
                    "media asset origin does not match its producing run",
                ));
            }
            if super::app_runs::material_digest(&run["snapshot"]) != run["snapshot_digest"]
                || run["approved_digest"] != run["snapshot_digest"]
            {
                return Err(Error::rejected(
                    "media origin run has no approved immutable snapshot",
                ));
            }
            let frozen = &run["snapshot"]["capabilities"][slot];
            if frozen.is_null() {
                return Err(Error::rejected(
                    "media origin run froze no binding for its producing slot",
                ));
            }
            let binding: crate::store::app_bindings::BindingProof =
                serde_json::from_value(frozen.clone())
                    .map_err(|_| Error::rejected("media origin capability binding is malformed"))?;
            Ok(MediaContextImage {
                original: MediaOriginalProof {
                    install_id: asset.install_id.clone(),
                    context_id: run["context_id"].as_str().map(str::to_string),
                    slot: slot.to_string(),
                    proof: binding,
                    reference: descriptor.reference,
                    sha256: descriptor.sha256,
                    bytes: descriptor.bytes,
                    media_type: descriptor.media_type,
                },
                asset,
                media_type,
                bytes: derivative,
                digest: material_digest,
            })
        })
    }

    /// Sealed-write-tx entry for `app_media_publish`: opens the store's
    /// own write tx (closure latch + lease fence re-checked inside the
    /// conn lock) and runs [`Self::app_media_publish_in`].
    pub(crate) fn app_media_publish(
        &self,
        asset_id: &str,
        expected_revision: i64,
        request_id: &str,
        now: f64,
    ) -> Result<Value> {
        self.write_tx(|tx| {
            Self::app_media_publish_in(tx, asset_id, expected_revision, request_id, now)
        })
    }

    pub(crate) fn app_media_publish_in(
        tx: &WriteTxn<'_>,
        asset_id: &str,
        expected_revision: i64,
        request_id: &str,
        now: f64,
    ) -> Result<Value> {
        if let Some((terms_asset, terms_revision, result)) = tx.query_opt(
            "SELECT asset_id,expected_revision,result
             FROM app_media_publish_requests WHERE request_id=?1",
            params![request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )? {
            let terms_asset: String = terms_asset;
            let terms_revision: i64 = terms_revision;
            let result: String = result;
            if terms_asset != asset_id || terms_revision != expected_revision {
                return Err(Error::rejected(
                    "media publish request id was accepted under different \
                     intent — reuse with changed terms is refused",
                ));
            }
            return serde_json::from_str(&result).map_err(|_| {
                Error::rejected("a stored media publish receipt is malformed — refusing it")
            });
        }
        let Some(row) = tx.query_opt(
            "SELECT id,install_id,title,kind,lifecycle,revision,creator,created,updated
             FROM app_media_assets WHERE id=?1",
            params![asset_id],
            row_media_asset,
        )?
        else {
            return Err(Error::rejected(
                "media asset is not in the workspace catalog",
            ));
        };
        let row = row.checked()?;
        if row.revision != expected_revision {
            return Err(Error::rejected(format!(
                "media asset revision moved — expected {expected_revision}, \
                 catalog holds {}",
                row.revision
            )));
        }
        if row.lifecycle != "draft" {
            return Err(Error::rejected(
                "media asset is not a draft — only a draft publishes",
            ));
        }
        let revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| Error::rejected("media asset revision cannot advance"))?;
        let moved = tx.execute(
            "UPDATE app_media_assets
             SET lifecycle='published', revision=?2, updated=?3
             WHERE id=?1 AND revision=?4 AND lifecycle='draft'",
            params![asset_id, revision, now, expected_revision],
        )?;
        if moved != 1 {
            return Err(Error::rejected(
                "media asset revision moved during publish — retry refused",
            ));
        }
        let published = MediaAsset {
            lifecycle: "published".to_string(),
            revision,
            updated: now,
            ..row
        };
        let result = json!({
            "asset": published.projection(),
            "receipt": {
                "request_id": request_id,
                "asset_id": asset_id,
                "expected_revision": expected_revision,
                "revision": revision,
                "lifecycle": "published",
            }
        });
        tx.execute(
            "INSERT INTO app_media_publish_requests(
                request_id,asset_id,expected_revision,result,created)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                request_id,
                asset_id,
                expected_revision,
                result.to_string(),
                now
            ],
        )?;
        Ok(result)
    }
}

/// CAD-1365: the verified provenance behind one catalog asset's retained
/// original — resolved from the private `app_media_asset_origins` link,
/// never from a request field. The daemon layer re-proves the CURRENT
/// binding/connection material against `proof` before any provider
/// access; this value is the frozen record it is checked against.
pub(crate) struct MediaOriginalProof {
    /// The install that produced the asset — bound to the receipt's run,
    /// never to the caller's mount install (the catalog is store-wide).
    pub install_id: String,
    /// The producing run's context, for binding re-proof.
    pub context_id: Option<String>,
    /// The capability slot the producing binding occupies.
    pub slot: String,
    /// The run-frozen binding proof the CURRENT binding must still match.
    pub proof: crate::store::app_bindings::BindingProof,
    /// The closed stored `original_artifact` descriptor.
    pub reference: String,
    /// Canonical `sha256:<64 lowercase hex>` of the original.
    pub sha256: String,
    /// The declared original byte count.
    pub bytes: u64,
    /// The declared original MIME — validated bytes, allowlisted image.
    pub media_type: String,
}

/// Authorized display material and its immutable original provenance.
pub(crate) struct MediaContextImage {
    pub original: MediaOriginalProof,
    pub asset: MediaAsset,
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
    pub digest: String,
}

fn row_media_asset(r: &rusqlite::Row<'_>) -> rusqlite::Result<MediaAsset> {
    Ok(MediaAsset {
        id: r.get(0)?,
        install_id: r.get(1)?,
        title: r.get(2)?,
        kind: r.get(3)?,
        lifecycle: r.get(4)?,
        revision: r.get(5)?,
        creator: r.get(6)?,
        created: r.get(7)?,
        updated: r.get(8)?,
        source_asset_id: None,
    })
}

fn row_media_catalog(r: &rusqlite::Row<'_>) -> rusqlite::Result<MediaAsset> {
    let mut asset = row_media_asset(r)?;
    asset.source_asset_id = r.get(9)?;
    Ok(asset)
}
