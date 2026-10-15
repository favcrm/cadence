//! CAD-1393: company document templates and their drafts.
//!
//! Company scope is the store's own connection workspace
//! (`connection_metadata.workspace_id`), read here and never taken from
//! a request. Every row carries `company_id` and every read filters on
//! it. Approved versions are immutable: a trigger refuses any update or
//! delete of an approved row, and approval only moves `draft` rows.
//! Versions are system-assigned (`MAX(version)+1`); a revision keeps
//! its base untouched.

use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use super::{now, Store, StoreConn};
use crate::business_templates::{
    build_draft, check_definition_approvable, content_digest, retained_source, sha256_hex,
    validate_definition, DocumentData, TemplateDefinition,
};
use crate::error::{Error, Result};

/// The schema (v40): template versions and the drafts pinned to them.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS business_template_versions(
    company_id TEXT NOT NULL,
    name TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version>0),
    based_on INTEGER,
    status TEXT NOT NULL CHECK(status IN ('draft','approved')),
    kind TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    created REAL NOT NULL,
    approved_source TEXT,
    approved_at REAL,
    source_sha256 TEXT NOT NULL,
    PRIMARY KEY(company_id,name,version));
CREATE TABLE IF NOT EXISTS business_template_sources(
    company_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    bytes BLOB NOT NULL,
    created REAL NOT NULL,
    PRIMARY KEY(company_id,sha256));
CREATE TRIGGER IF NOT EXISTS business_template_source_no_update
    BEFORE UPDATE ON business_template_sources
    BEGIN SELECT RAISE(ABORT,'retained template sources are immutable'); END;
CREATE TRIGGER IF NOT EXISTS business_template_source_no_delete
    BEFORE DELETE ON business_template_sources
    BEGIN SELECT RAISE(ABORT,'retained template sources are immutable'); END;
CREATE TRIGGER IF NOT EXISTS business_template_approved_no_update
    BEFORE UPDATE ON business_template_versions WHEN OLD.status='approved'
    BEGIN SELECT RAISE(ABORT,'approved template versions are immutable'); END;
CREATE TRIGGER IF NOT EXISTS business_template_approved_no_delete
    BEFORE DELETE ON business_template_versions WHEN OLD.status='approved'
    BEGIN SELECT RAISE(ABORT,'approved template versions are immutable'); END;
CREATE TABLE IF NOT EXISTS business_documents(
    company_id TEXT NOT NULL,
    id TEXT NOT NULL,
    template_name TEXT NOT NULL,
    template_version INTEGER NOT NULL,
    template_sha256 TEXT NOT NULL,
    kind TEXT NOT NULL,
    input_json TEXT NOT NULL,
    context_json TEXT NOT NULL,
    created REAL NOT NULL,
    PRIMARY KEY(company_id,id));
";

/// One template version. `spec` is present for a single-version read.
#[derive(Debug, Clone)]
pub struct TemplateVersion {
    pub name: String,
    pub version: i64,
    pub based_on: Option<i64>,
    pub status: String,
    pub kind: String,
    pub content_sha256: String,
    pub source_sha256: String,
    pub approved_source: Option<String>,
    pub created: f64,
    pub spec: TemplateDefinition,
}

impl TemplateVersion {
    /// The JSON shape the RPC and CLI return, without the spec body.
    pub fn summary(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "version": self.version,
            "based_on": self.based_on,
            "status": self.status,
            "kind": self.kind,
            "content_sha256": self.content_sha256,
            "source_sha256": self.source_sha256,
            "approved_source": self.approved_source,
            "created": self.created,
        })
    }
}

/// One draft pinned to the template version it was made from.
#[derive(Debug, Clone)]
pub struct BusinessDocument {
    pub id: String,
    pub template_name: String,
    pub template_version: i64,
    pub template_sha256: String,
    pub kind: String,
    pub input: Value,
    pub context: Value,
    pub created: f64,
}

impl BusinessDocument {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "template": self.template_name,
            "version": self.template_version,
            "template_sha256": self.template_sha256,
            "kind": self.kind,
            "state": "draft",
            "input": self.input,
            "context": self.context,
            "created": self.created,
        })
    }
}

fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        return Err(Error::rejected(
            "template name must be lowercase letters, digits or '-', starting with a letter",
        ));
    }
    Ok(())
}

fn version_in(
    tx: &impl StoreConn,
    company: &str,
    name: &str,
    version: i64,
) -> Result<Option<TemplateVersion>> {
    let row = tx
        .query_row(
            "SELECT name,version,based_on,status,kind,content_sha256,approved_source,created,spec_json,source_sha256
             FROM business_template_versions WHERE company_id=? AND name=? AND version=?",
            params![company, name, version],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, f64>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        name,
        version,
        based_on,
        status,
        kind,
        sha,
        approved_source,
        created,
        spec_json,
        source_sha,
    )) = row
    else {
        return Ok(None);
    };
    let spec: TemplateDefinition = serde_json::from_str(&spec_json)
        .map_err(|e| Error::internal(format!("stored template spec is unreadable: {e}")))?;
    Ok(Some(TemplateVersion {
        name,
        version,
        based_on,
        status,
        kind,
        content_sha256: sha,
        source_sha256: source_sha,
        approved_source,
        created,
        spec,
    }))
}

impl Store {
    /// Create version 1 of `name`, or revise it into the next version from
    /// `based_on`. A financial version retains its DOCX. An authoring pack
    /// retains an empty source blob; its guidelines and references live in
    /// `spec_json`. The version digest binds those retained bytes and the
    /// canonical definition. Both paths write a `draft`; no path touches
    /// an existing version.
    pub fn business_template_add_version(
        &self,
        name: &str,
        based_on: Option<i64>,
        source: &[u8],
        definition: &TemplateDefinition,
    ) -> Result<TemplateVersion> {
        check_name(name)?;
        validate_definition(definition)?;
        let source = retained_source(definition, source)?;
        let company = self.connection_workspace_id()?;
        let source_sha = sha256_hex(source);
        let digest = content_digest(&source_sha, definition)?;
        let spec_json =
            serde_json::to_string(definition).map_err(|e| Error::internal(e.to_string()))?;
        let kind = definition.kind();
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let latest: Option<i64> = tx.query_row(
                "SELECT MAX(version) FROM business_template_versions WHERE company_id=? AND name=?",
                params![company, name],
                |r| r.get(0),
            )?;
            match (based_on, latest) {
                (None, Some(_)) => {
                    return Err(Error::rejected(format!(
                        "template '{name}' exists: revise it from a base version"
                    )))
                }
                (Some(_), None) => {
                    return Err(Error::rejected(format!("template '{name}' does not exist")))
                }
                (Some(base), Some(_)) => {
                    if version_in(tx, &company, name, base)?.is_none() {
                        return Err(Error::rejected(format!(
                            "template '{name}' has no version {base}"
                        )));
                    }
                }
                (None, None) => {}
            }
            let version = latest.unwrap_or(0) + 1;
            let created = now();
            tx.execute(
                "INSERT OR IGNORE INTO business_template_sources(company_id,sha256,size,bytes,created)
                 VALUES(?,?,?,?,?)",
                params![company, source_sha, source.len() as i64, source, created],
            )?;
            tx.execute(
                "INSERT INTO business_template_versions(company_id,name,version,based_on,status,kind,spec_json,content_sha256,created,source_sha256)
                 VALUES(?,?,?,?,'draft',?,?,?,?,?)",
                params![company, name, version, based_on, kind, spec_json, digest, created, source_sha],
            )?;
            Ok(TemplateVersion {
                name: name.to_string(),
                version,
                based_on,
                status: "draft".into(),
                kind: kind.into(),
                content_sha256: digest,
                source_sha256: source_sha,
                approved_source: None,
                created,
                spec: definition.clone(),
            })
        })
    }

    /// One version of `name` in this company, `None` when absent.
    pub fn business_template_version(
        &self,
        name: &str,
        version: i64,
    ) -> Result<Option<TemplateVersion>> {
        check_name(name)?;
        let company = self.connection_workspace_id()?;
        let conn = self.conn();
        version_in(&conn, &company, name, version)
    }

    /// Summaries of every version of `name`, oldest first.
    pub fn business_template_versions(&self, name: &str) -> Result<Vec<Value>> {
        check_name(name)?;
        let company = self.connection_workspace_id()?;
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT version FROM business_template_versions WHERE company_id=? AND name=? ORDER BY version",
        )?;
        let versions: Vec<i64> = stmt
            .query_map(params![company, name], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::with_capacity(versions.len());
        for version in versions {
            if let Some(row) = version_in(&conn, &company, name, version)? {
                out.push(row.summary());
            }
        }
        Ok(out)
    }

    /// Approve exactly the content whose digest the approver read. Only a
    /// `draft` moves. Financial versions still need confirmed calculation
    /// rules; a structurally valid authoring pack has none. The caller has
    /// already proved an operator connection.
    pub fn business_template_approve(
        &self,
        name: &str,
        version: i64,
        digest: &str,
        source: &str,
    ) -> Result<TemplateVersion> {
        check_name(name)?;
        if source.trim().is_empty() || source.len() > 200 {
            return Err(Error::rejected(
                "approval source must name who decided, 1..=200 bytes",
            ));
        }
        let company = self.connection_workspace_id()?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let row = version_in(tx, &company, name, version)?
                .ok_or_else(|| Error::rejected(format!("template '{name}' has no version {version}")))?;
            if row.status != "draft" {
                return Err(Error::rejected(format!(
                    "template '{name}' version {version} is already approved and immutable"
                )));
            }
            // The digest must be the one the version was created with, and
            // it must still match the retained source and spec (the source
            // is immutable, so a mismatch means tampering or corruption).
            let recomputed = content_digest(&row.source_sha256, &row.spec)?;
            if row.content_sha256 != digest || recomputed != digest {
                return Err(Error::rejected(
                    "the digest does not match this version's content; show the version and approve its exact digest",
                ));
            }
            check_definition_approvable(&row.spec)?;
            tx.execute(
                "UPDATE business_template_versions SET status='approved', approved_source=?, approved_at=?
                 WHERE company_id=? AND name=? AND version=? AND status='draft'",
                params![source, now(), company, name, version],
            )?;
            version_in(tx, &company, name, version)?
                .ok_or_else(|| Error::internal("approved template version vanished"))
        })
    }

    /// Create a draft from an approved version. Validation and totals run
    /// in code; the input and computed context are retained with the draft.
    pub fn business_document_create(
        &self,
        name: &str,
        version: i64,
        data: &DocumentData,
    ) -> Result<BusinessDocument> {
        check_name(name)?;
        let company = self.connection_workspace_id()?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let row = version_in(tx, &company, name, version)?
                .ok_or_else(|| Error::rejected(format!("template '{name}' has no version {version}")))?;
            if row.status != "approved" {
                return Err(Error::rejected(format!(
                    "template '{name}' version {version} is not approved; approve it before creating documents"
                )));
            }
            let draft = build_draft(row.spec.financial()?, data)?;
            let id = format!("doc-{}", uuid::Uuid::new_v4().simple());
            let input_json =
                serde_json::to_string(data).map_err(|e| Error::internal(e.to_string()))?;
            let context = draft.context();
            let context_json =
                serde_json::to_string(&context).map_err(|e| Error::internal(e.to_string()))?;
            let created = now();
            tx.execute(
                "INSERT INTO business_documents(company_id,id,template_name,template_version,template_sha256,kind,input_json,context_json,created)
                 VALUES(?,?,?,?,?,?,?,?,?)",
                params![
                    company,
                    id,
                    name,
                    version,
                    row.content_sha256,
                    row.kind,
                    input_json,
                    context_json,
                    created
                ],
            )?;
            Ok(BusinessDocument {
                id,
                template_name: name.to_string(),
                template_version: version,
                template_sha256: row.content_sha256,
                kind: row.kind,
                input: serde_json::from_str(&input_json).unwrap_or(Value::Null),
                context,
                created,
            })
        })
    }

    /// The retained DOCX bytes of an approved version, `None` when this
    /// company has no such version. Only the approved source is read here:
    /// the retained source is immutable and keyed by its SHA-256.
    pub fn business_template_source(
        &self,
        name: &str,
        version: i64,
    ) -> Result<Option<(String, Vec<u8>)>> {
        self.template_source(name, version, true)
    }

    /// The retained DOCX bytes of a draft or approved version, for a sample
    /// preview only. Draft bytes are the immutable snapshot the draft was
    /// created from; a sample never creates a document from them.
    pub fn business_template_snapshot_source(
        &self,
        name: &str,
        version: i64,
    ) -> Result<Option<(String, Vec<u8>)>> {
        self.template_source(name, version, false)
    }

    fn template_source(
        &self,
        name: &str,
        version: i64,
        approved_only: bool,
    ) -> Result<Option<(String, Vec<u8>)>> {
        check_name(name)?;
        let company = self.connection_workspace_id()?;
        let conn = self.conn();
        let status_filter = if approved_only {
            " AND v.status='approved'"
        } else {
            ""
        };
        let sql = format!(
            "SELECT v.source_sha256,s.bytes FROM business_template_versions v
             JOIN business_template_sources s ON s.company_id=v.company_id AND s.sha256=v.source_sha256
             WHERE v.company_id=? AND v.name=? AND v.version=?{status_filter}"
        );
        let row: Option<(String, Vec<u8>)> = conn
            .query_row(&sql, params![company, name, version], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok(row)
    }

    /// One draft of this company, `None` when absent or another company's.
    pub fn business_document(&self, id: &str) -> Result<Option<BusinessDocument>> {
        let company = self.connection_workspace_id()?;
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT id,template_name,template_version,template_sha256,kind,input_json,context_json,created
                 FROM business_documents WHERE company_id=? AND id=?",
                params![company, id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, f64>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            template_name,
            template_version,
            template_sha256,
            kind,
            input,
            context,
            created,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(BusinessDocument {
            id,
            template_name,
            template_version,
            template_sha256,
            kind,
            input: serde_json::from_str(&input).unwrap_or(Value::Null),
            context: serde_json::from_str(&context).unwrap_or(Value::Null),
            created,
        }))
    }
}
