//! One host-custodied SMTP sender link per CRM installation/context (CAD-785).
//!
//! The link row is the typed binding through a verified authorization
//! revision: `(install_id, context_id)` names at most one live
//! connection, and the row pins the credential (authorization)
//! revision the bind observed. Rotation bumps the credential
//! revision, so the link goes stale by construction — the send path
//! refuses until the operator rebinds (fresh CAS revision, fresh
//! digest). Revocation — of the link or of the underlying
//! credential — refuses the same way. The row carries the
//! non-secret sender/transport summary inside its digest so a
//! rebinding to different material is a different binding, never a
//! silent edit; the secret itself lives only in host custody and
//! never appears here.
//!
//! Every write is operator-attributed by the RPC layer before this
//! module is reached; CAS on `link_revision` makes concurrent
//! bind/rebind/revoke claims refuse instead of interleave.

use super::app_runs::material_digest;
use super::StoreConn;
use super::*;
use crate::platform::smtp::SmtpProjection;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS crm_smtp_links(
 install_id TEXT NOT NULL, context_id TEXT NOT NULL,
 connection_id TEXT NOT NULL, auth_revision INTEGER NOT NULL CHECK(auth_revision>0),
 link_revision INTEGER NOT NULL CHECK(link_revision>0),
 state TEXT NOT NULL CHECK(state IN ('live','revoked')),
 digest TEXT NOT NULL, request_id TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL,
 PRIMARY KEY(install_id,context_id),
 UNIQUE(install_id,request_id));
";

/// One link row: the connection it binds plus the authorization
/// revision observed at bind/rebind time.
pub struct SmtpLink {
    pub connection_id: String,
    pub auth_revision: i64,
    pub link_revision: i64,
    pub state: String,
    pub digest: String,
    pub request_id: String,
    pub created: f64,
    pub updated: f64,
}

fn link_digest(
    install: &str,
    context: &str,
    connection_id: &str,
    auth_revision: i64,
    projection: &SmtpProjection,
) -> String {
    material_digest(&json!({
        "domain": "cadence-crm-smtp-link-v1",
        "install_id": install,
        "context_id": context,
        "connection_id": connection_id,
        "auth_revision": auth_revision,
        "host": projection.host,
        "port": projection.port,
        "tls_mode": projection.tls_mode,
        "username": projection.username,
        "sender": projection.sender,
        "sender_name": projection.sender_name,
    }))
}

fn link_json(install: &str, context: &str, row: &SmtpLink, projection: &SmtpProjection) -> Value {
    json!({
        "install_id": install,
        "context_id": context,
        "connection_id": row.connection_id,
        "auth_revision": row.auth_revision,
        "link_revision": row.link_revision,
        "state": row.state,
        "digest": row.digest,
        "sender": {"name": projection.sender_name, "address": projection.sender},
        "transport": {"host": projection.host, "port": projection.port, "tls_mode": projection.tls_mode, "username": projection.username},
    })
}

/// Screen the exact prepared link write and hand back the existing
/// public response shape. The material is the serialized response plus
/// the row-only fields the response omits (persisted request ID and
/// timestamps) — including the derived digest no caller input
/// screening can see. It is built once, screened, then stripped of
/// only those row-private keys and returned as the same value.
fn screened_binding(
    install: &str,
    context: &str,
    prepared: &SmtpLink,
    projection: &SmtpProjection,
    screen: Option<&dyn Fn(&Value) -> Result<()>>,
) -> Result<Value> {
    // Build the normal public response by moving the one binding Value
    // into its envelope — no serialize round-trip, no second copy.
    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "binding".to_string(),
        link_json(install, context, prepared, projection),
    );
    let mut material = Value::Object(envelope);
    if let Some(screen) = screen {
        // Attach only the row-private fields the response omits, with
        // their exact persisted values; screen; then drop them again
        // so the caller gets the unchanged public shape.
        if let Value::Object(fields) = &mut material {
            fields.insert("request_id".to_string(), json!(prepared.request_id));
            fields.insert("created".to_string(), json!(prepared.created));
            fields.insert("updated".to_string(), json!(prepared.updated));
        }
        screen(&material)?;
        if let Value::Object(fields) = &mut material {
            fields.remove("request_id");
            fields.remove("created");
            fields.remove("updated");
        }
    }
    Ok(material)
}

impl Store {
    fn crm_smtp_row(
        &self,
        conn: &impl super::StoreConn,
        install: &str,
        context: &str,
    ) -> Result<Option<SmtpLink>> {
        conn.query_row(
            "SELECT connection_id,auth_revision,link_revision,state,digest,request_id,created,updated FROM crm_smtp_links WHERE install_id=? AND context_id=?",
            params![install, context],
            |row| {
                Ok(SmtpLink {
                    connection_id: row.get(0)?,
                    auth_revision: row.get(1)?,
                    link_revision: row.get(2)?,
                    state: row.get(3)?,
                    digest: row.get(4)?,
                    request_id: row.get(5)?,
                    created: row.get(6)?,
                    updated: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    /// Read one link row for the send/show path. `None` is "no sender
    /// is bound", never a default — the caller refuses.
    pub fn crm_smtp_link(&self, install: &str, context: &str) -> Result<Option<SmtpLink>> {
        self.read_tx(|conn| self.crm_smtp_row(&conn, install, context))
    }

    /// Bind one sender connection to an installation/context. At most
    /// one live link exists per pair: a live row refuses a second
    /// bind (rebind or revoke first); a revoked row re-binds only
    /// under a fresh request ID. Replaying the bind's own request ID
    /// against identical material returns the row; against different
    /// material it refuses — a request ID never retargets.
    ///
    /// `screen` sees the exact row/event/response material this call
    /// would persist — including the derived digest, the persisted
    /// request ID and the row timestamps no caller input screening can
    /// see — once, under the write transaction and before any SQL,
    /// event or replay response. Encrypted custody passes the
    /// secret-aware screen; legacy callers pass `None`.
    #[allow(clippy::too_many_arguments)]
    pub fn crm_smtp_bind(
        &self,
        install: &str,
        context: &str,
        connection_id: &str,
        auth_revision: i64,
        projection: &SmtpProjection,
        request_id: &str,
        screen: Option<&dyn Fn(&Value) -> Result<()>>,
    ) -> Result<Value> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(request_id, "binding request ID")?;
        if connection_id.is_empty() || auth_revision <= 0 {
            return Err(Error::rejected("SMTP sender binding is invalid"));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let digest = link_digest(install, context, connection_id, auth_revision, projection);
            match self.crm_smtp_row(&tx, install, context)? {
                None => {
                    let timestamp = now();
                    let prepared = SmtpLink {
                        connection_id: connection_id.to_string(),
                        auth_revision,
                        link_revision: 1,
                        state: "live".to_string(),
                        digest,
                        request_id: request_id.to_string(),
                        created: timestamp,
                        updated: timestamp,
                    };
                    let response =
                        screened_binding(install, context, &prepared, projection, screen)?;
                    tx.execute(
                        "INSERT INTO crm_smtp_links(install_id,context_id,connection_id,auth_revision,link_revision,state,digest,request_id,created,updated) VALUES(?,?,?,?,?, 'live',?,?,?,?)",
                        params![install, context, prepared.connection_id, prepared.auth_revision, prepared.link_revision, prepared.digest, prepared.request_id, prepared.created, prepared.updated],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                    Self::event(
                        &tx,
                        "crm_smtp",
                        "crm_smtp_bound",
                        json!({"install_id": install, "context_id": context, "connection_id": connection_id, "auth_revision": auth_revision, "link_revision": prepared.link_revision, "digest": &prepared.digest}),
                    )?;
                    Ok(response)
                }
                Some(row) if row.state == "live" => {
                    if row.request_id == request_id
                        && row.connection_id == connection_id
                        && row.auth_revision == auth_revision
                        && row.digest == digest
                    {
                        return screened_binding(install, context, &row, projection, screen);
                    }
                    if row.request_id == request_id {
                        return Err(Error::rejected(
                            "SMTP binding request ID is already used for different material",
                        ));
                    }
                    return Err(Error::rejected(
                        "this installation and context already has a live SMTP sender; rebind or revoke it first",
                    ));
                }
                Some(row) => {
                    // Revoked rows re-bind only under a fresh request ID,
                    // as a new live incarnation — the old request never
                    // resurrects.
                    if row.request_id == request_id {
                        return Err(Error::rejected(
                            "SMTP binding request ID is already used; bind with a fresh request ID",
                        ));
                    }
                    let prepared = SmtpLink {
                        connection_id: connection_id.to_string(),
                        auth_revision,
                        link_revision: row.link_revision + 1,
                        state: "live".to_string(),
                        digest,
                        request_id: request_id.to_string(),
                        created: row.created,
                        updated: now(),
                    };
                    let response =
                        screened_binding(install, context, &prepared, projection, screen)?;
                    let changed = tx
                        .execute(
                            "UPDATE crm_smtp_links SET connection_id=?,auth_revision=?,link_revision=?,state='live',digest=?,request_id=?,updated=? WHERE install_id=? AND context_id=? AND link_revision=? AND state='revoked'",
                            params![prepared.connection_id, prepared.auth_revision, prepared.link_revision, prepared.digest, prepared.request_id, prepared.updated, install, context, row.link_revision],
                        )
                        .map_err(|e| Error::internal(e.to_string()))?;
                    if changed != 1 {
                        return Err(Error::rejected("SMTP sender binding changed under claim"));
                    }
                    Self::event(
                        &tx,
                        "crm_smtp",
                        "crm_smtp_bound",
                        json!({"install_id": install, "context_id": context, "connection_id": connection_id, "auth_revision": auth_revision, "link_revision": prepared.link_revision, "digest": &prepared.digest}),
                    )?;
                    Ok(response)
                }
            }
        })
    }

    /// Rebind under CAS: the expected link revision must be the live
    /// one. Post-rotate rebinds (same connection, fresh authorization
    /// revision) and sender switches (different connection) share
    /// this path — either way the digest changes and prior authority
    /// ends. A revoked link refuses: bind again instead.
    ///
    /// `screen` sees the exact row/event/response material this rebind
    /// would persist (derived digest, retained request ID and
    /// timestamps included), once, before the CAS update or event.
    /// Encrypted custody passes the secret-aware screen; legacy
    /// callers pass `None`.
    #[allow(clippy::too_many_arguments)]
    pub fn crm_smtp_rebind(
        &self,
        install: &str,
        context: &str,
        connection_id: &str,
        auth_revision: i64,
        projection: &SmtpProjection,
        expected_revision: i64,
        screen: Option<&dyn Fn(&Value) -> Result<()>>,
    ) -> Result<Value> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(context, "context ID")?;
        if expected_revision <= 0 || connection_id.is_empty() || auth_revision <= 0 {
            return Err(Error::rejected("SMTP sender rebinding is invalid"));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let row = self.crm_smtp_row(&tx, install, context)?.ok_or_else(|| {
                Error::rejected("no SMTP sender is bound to this installation and context")
            })?;
            if row.state != "live" {
                return Err(Error::rejected(
                    "SMTP sender binding is revoked; bind it again instead",
                ));
            }
            if row.link_revision != expected_revision {
                return Err(Error::rejected("SMTP sender binding revision is stale"));
            }
            let digest = link_digest(install, context, connection_id, auth_revision, projection);
            let prepared = SmtpLink {
                connection_id: connection_id.to_string(),
                auth_revision,
                link_revision: row.link_revision + 1,
                state: "live".to_string(),
                digest,
                created: row.created,
                updated: now(),
                request_id: row.request_id,
            };
            let response = screened_binding(install, context, &prepared, projection, screen)?;
            let changed = tx
                .execute(
                    "UPDATE crm_smtp_links SET connection_id=?,auth_revision=?,link_revision=?,digest=?,updated=? WHERE install_id=? AND context_id=? AND link_revision=? AND state='live'",
                    params![prepared.connection_id, prepared.auth_revision, prepared.link_revision, prepared.digest, prepared.updated, install, context, expected_revision],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected("SMTP sender binding changed under claim"));
            }
            Self::event(
                &tx,
                "crm_smtp",
                "crm_smtp_rebound",
                json!({"install_id": install, "context_id": context, "connection_id": connection_id, "auth_revision": auth_revision, "link_revision": prepared.link_revision, "digest": &prepared.digest}),
            )?;
            Ok(response)
        })
    }

    /// Revoke the live link under CAS. The credential itself is
    /// untouched — this ends the installation/context binding only.
    /// Test sends refuse immediately; a later bind starts a fresh
    /// incarnation.
    pub fn crm_smtp_revoke(
        &self,
        install: &str,
        context: &str,
        expected_revision: i64,
    ) -> Result<Value> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(context, "context ID")?;
        if expected_revision <= 0 {
            return Err(Error::rejected(
                "expected binding revision must be a positive integer",
            ));
        }
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let row = self.crm_smtp_row(&tx, install, context)?.ok_or_else(|| {
                        Error::rejected("no SMTP sender is bound to this installation and context")
                    })?;
                    if row.state != "live" {
                        return Err(Error::rejected("SMTP sender binding is already revoked"));
                    }
                    if row.link_revision != expected_revision {
                        return Err(Error::rejected("SMTP sender binding revision is stale"));
                    }
                    let revision = row.link_revision + 1;
                    let changed = tx
                        .execute(
                            "UPDATE crm_smtp_links SET link_revision=?,state='revoked',updated=? WHERE install_id=? AND context_id=? AND link_revision=? AND state='live'",
                            params![revision, now(), install, context, expected_revision],
                        )
                        .map_err(|e| Error::internal(e.to_string()))?;
                    if changed != 1 {
                        return Err(Error::rejected("SMTP sender binding changed under claim"));
                    }
                    Self::event(
                        &tx,
                        "crm_smtp",
                        "crm_smtp_revoked",
                        json!({"install_id": install, "context_id": context, "connection_id": row.connection_id, "link_revision": revision}),
                    )?;
                    Ok(json!({"revoked": true, "link_revision": revision}))
        })
    }

    /// Best-effort audit for a test send: digests and the SMTP
    /// verdict only — never addresses, content or secrets.
    pub fn note_crm_smtp_test(&self, install: &str, context: &str, receipt: &Value) {
        if let Err(e) = self.write_tx(|tx| {
            Self::event(
                &*tx,
                Self::DAEMON_STREAM,
                "crm_smtp_test_sent",
                json!({"install_id": install, "context_id": context,
                   "connection_id": receipt["connection_id"],
                   "auth_revision": receipt["auth_revision"],
                   "link_revision": receipt["link_revision"],
                   "content_digest": receipt["content_digest"],
                   "payload_digest": receipt["payload_digest"],
                   "accepted": receipt["accepted"],
                   "smtp_code": receipt["smtp_code"]}),
            )
        }) {
            eprintln!("store: best-effort CRM SMTP test audit failed: {e}");
        }
    }
}
