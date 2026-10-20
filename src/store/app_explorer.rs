//! CAD-1129 store rows: per-user favourites (H8), install requests
//! (H7) and cached update checks (H4). Operator-owned, never authority:
//! a favourites row grants nothing, a request is a notification, and a
//! cached check is a hint the update route re-proves.
use super::StoreConn;
use super::*;
use rusqlite::params;
use serde_json::{json, Value};
use sha2::Digest;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_favorites(
 owner TEXT NOT NULL, install_id TEXT NOT NULL, position INTEGER NOT NULL,
 opened_at REAL, saved INTEGER NOT NULL DEFAULT 1,
 PRIMARY KEY(owner, install_id));
CREATE TABLE IF NOT EXISTS app_install_requests(
 id TEXT PRIMARY KEY, catalog_id TEXT NOT NULL, requested_by TEXT NOT NULL,
 at REAL NOT NULL, state TEXT NOT NULL CHECK(state IN ('open','installed','dismissed')),
 UNIQUE(catalog_id, requested_by));
CREATE TABLE IF NOT EXISTS app_update_checks(
 install_id TEXT PRIMARY KEY, checked_at REAL NOT NULL,
 proposal_json TEXT NOT NULL, has_update INTEGER NOT NULL DEFAULT 0,
 version TEXT, access_change_json TEXT, digest TEXT);
";

/// Longest favourites list a PUT can write (decision: a small grid,
/// not an inbox).
pub const FAVORITES_MAX: usize = 24;

/// The synthetic owner whose row is the workspace default — reserved;
/// a real owner's name can never be `*` (member handles are
/// `[A-Za-z0-9_-]` and `operator` is its own word).
pub const DEFAULT_OWNER: &str = "*";

fn owner_ok(owner: &str) -> Result<()> {
    if owner.is_empty()
        || owner.len() > 128
        || owner == DEFAULT_OWNER
        || !owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::rejected("invalid favorites owner"));
    }
    Ok(())
}

fn install_id_ok(install: &str) -> Result<()> {
    crate::issue::app_catalog::InstallationId::parse(install)?;
    Ok(())
}

/// `sha256:<hex>` over the ordered install ids as canonical JSON: the bounded
/// digest a favorites receipt stores instead of the list.
fn ids_digest(ids: &[String]) -> Result<String> {
    let bytes = serde_json::to_vec(ids).map_err(|e| Error::internal(e.to_string()))?;
    Ok(format!("sha256:{:x}", sha2::Sha256::digest(bytes)))
}

/// The canonical receipt of one explorer write (CAD-1374), written by
/// `Self::event` in the write's own transaction. `company` and `human` come only
/// from a verified daemon human; without one no company is named, so the trail
/// never claims it. One server-minted UUID serves as request and correlation
/// ID; causation is explicitly null because no external cause is recorded.
fn receipt(
    kind: &str,
    human: Option<&Value>,
    resource: &str,
    digest: Option<String>,
    mut body: Value,
) -> Result<Value> {
    let (action, outcome, reason) = match kind {
        "app_favorites_saved" => ("favorites_saved", "saved", "saved the favorites list"),
        "app_favorites_default_saved" => (
            "favorites_default_saved",
            "saved",
            "saved the workspace default favorites",
        ),
        "app_install_requested" => (
            "install_request_created",
            "open",
            "created an install request",
        ),
        "app_install_request_reopened" => (
            "install_request_reopened",
            "open",
            "reopened a dismissed install request",
        ),
        "app_install_request_dismissed" => (
            "install_request_dismissed",
            "dismissed",
            "dismissed an open install request",
        ),
        _ => return Err(Error::internal("unknown explorer receipt")),
    };
    let id = uuid::Uuid::new_v4().simple().to_string();
    body["schema_version"] = json!(1);
    body["action"] = json!(action);
    body["outcome"] = json!(outcome);
    body["reason"] = json!(reason);
    body["resource"] = json!(resource);
    body["digest"] = json!(digest);
    body["request_id"] = json!(id);
    body["correlation_id"] = json!(id);
    body["causation_id"] = Value::Null;
    if let Some(human) = human {
        body["company"] = human["company"].clone();
        body["human"] = human.clone();
    }
    Ok(body)
}

impl Store {
    /// Whether `owner` ever saved a list — an explicit empty list is
    /// kept and must NOT fall back to the default.
    fn favorites_saved_in(conn: &impl super::StoreConn, owner: &str) -> Result<bool> {
        Ok(conn
            .query_opt(
                "SELECT 1 FROM app_favorites WHERE owner=? AND saved=1 LIMIT 1",
                [owner],
                |r| r.get::<_, i64>(0),
            )?
            .is_some())
    }

    fn favorites_rows_in(conn: &impl super::StoreConn, owner: &str) -> Result<Vec<Value>> {
        Ok(conn.query_vec(
            "SELECT install_id,position,opened_at FROM app_favorites
                 WHERE owner=? AND saved=1 ORDER BY position,install_id",
            [owner],
            |r| {
                Ok(json!({
                    "install_id": r.get::<_, String>(0)?,
                    "position": r.get::<_, i64>(1)?,
                    "opened_at": r.get::<_, Option<f64>>(2)?,
                }))
            },
        )?)
    }

    /// `GET /api/app-favorites`: the caller's own saved list (possibly
    /// an explicit empty list), else the workspace default.
    /// `live` filters ids that are no longer live installs.
    pub fn app_favorites_get(&self, owner: &str, live: &dyn Fn(&str) -> bool) -> Result<Value> {
        owner_ok(owner)?;
        self.read_tx(|conn| {
            let mine = Self::favorites_saved_in(conn, owner)?;
            let source = if mine { owner } else { DEFAULT_OWNER };
            let rows: Vec<Value> = Self::favorites_rows_in(conn, source)?
                .into_iter()
                .filter(|r| live(r["install_id"].as_str().unwrap_or_default()))
                .collect();
            Ok(json!({
                "owner": owner,
                "is_default": !mine,
                "favorites": rows,
            }))
        })
    }

    /// `PUT /api/app-favorites`: replace the caller's ordered list.
    /// `install_ids` is ≤24 live install ids; `live` is the daemon's
    /// check. The write is all-or-nothing inside the sealed tx.
    pub fn app_favorites_put(&self, owner: &str, install_ids: &[String]) -> Result<Value> {
        self.app_favorites_put_as(owner, install_ids, None)
    }

    /// [`Self::app_favorites_put`] carrying the verified human behind the
    /// save (CAD-1374); the receipt lands in the same transaction.
    pub fn app_favorites_put_as(
        &self,
        owner: &str,
        install_ids: &[String],
        human: Option<&Value>,
    ) -> Result<Value> {
        owner_ok(owner)?;
        if install_ids.len() > FAVORITES_MAX {
            return Err(Error::rejected(format!(
                "a favorites list holds at most {FAVORITES_MAX} apps"
            )));
        }
        for id in install_ids {
            install_id_ok(id)?;
        }
        let digest = ids_digest(install_ids)?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            tx.execute("DELETE FROM app_favorites WHERE owner=?", [owner])?;
            for (position, id) in install_ids.iter().enumerate() {
                tx.execute(
                    "INSERT INTO app_favorites(owner,install_id,position,opened_at,saved)
                     VALUES(?,?,?,NULL,1)",
                    params![owner, id, position as i64],
                )?;
            }
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                "app_favorites_saved",
                receipt(
                    "app_favorites_saved",
                    human,
                    &format!("favorites:{owner}"),
                    Some(digest.clone()),
                    json!({"owner": owner}),
                )?,
            )?;
            Ok(json!({"owner": owner, "favorites": Self::favorites_rows_in(&tx, owner)?}))
        })
    }

    /// `PUT /api/app-favorites/default` — operator-only (the RPC gate).
    pub fn app_favorites_put_default(&self, install_ids: &[String]) -> Result<Value> {
        self.app_favorites_put_default_as(install_ids, None)
    }

    /// [`Self::app_favorites_put_default`] carrying the verified human (CAD-1374).
    pub fn app_favorites_put_default_as(
        &self,
        install_ids: &[String],
        human: Option<&Value>,
    ) -> Result<Value> {
        if install_ids.len() > FAVORITES_MAX {
            return Err(Error::rejected(format!(
                "a favorites list holds at most {FAVORITES_MAX} apps"
            )));
        }
        for id in install_ids {
            install_id_ok(id)?;
        }
        let digest = ids_digest(install_ids)?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            tx.execute("DELETE FROM app_favorites WHERE owner=?", [DEFAULT_OWNER])?;
            for (position, id) in install_ids.iter().enumerate() {
                tx.execute(
                    "INSERT INTO app_favorites(owner,install_id,position,opened_at,saved)
                     VALUES(?,?,?,NULL,1)",
                    params![DEFAULT_OWNER, id, position as i64],
                )?;
            }
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                "app_favorites_default_saved",
                receipt(
                    "app_favorites_default_saved",
                    human,
                    "favorites:default",
                    Some(digest.clone()),
                    json!({}),
                )?,
            )?;
            Ok(json!({"owner": DEFAULT_OWNER, "favorites": Self::favorites_rows_in(&tx, DEFAULT_OWNER)?}))
        })
    }

    /// `POST /api/app-favorites/opened` — refresh "Recently used".
    /// Touching an install not on the list is a no-op (it stays off).
    pub fn app_favorites_opened(&self, owner: &str, install_id: &str) -> Result<()> {
        owner_ok(owner)?;
        install_id_ok(install_id)?;
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE app_favorites SET opened_at=? WHERE owner=? AND install_id=?",
                params![now(), owner, install_id],
            )?;
            Ok(())
        })
    }

    // ---------- install requests (H7) ----------

    /// `POST /api/app-catalog/<id>/request` — idempotent per requester
    /// and entry: a second open request returns the existing row.
    pub fn app_install_request(&self, catalog_id: &str, requested_by: &str) -> Result<Value> {
        self.app_install_request_as(catalog_id, requested_by, None)
    }

    /// [`Self::app_install_request`] carrying the verified human behind the
    /// request (CAD-1374); the receipt lands in the same transaction.
    pub fn app_install_request_as(
        &self,
        catalog_id: &str,
        requested_by: &str,
        human: Option<&Value>,
    ) -> Result<Value> {
        if !crate::issue::model::valid_tag(catalog_id) {
            return Err(Error::rejected("invalid catalog id"));
        }
        owner_ok(requested_by)?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            if let Some((id, state)) = tx.query_opt(
                "SELECT id,state FROM app_install_requests WHERE catalog_id=? AND requested_by=?",
                params![catalog_id, requested_by],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )? {
                // A dismissed request may be re-asked — flip it back.
                if state == "dismissed" {
                    tx.execute(
                        "UPDATE app_install_requests SET state='open',at=? WHERE id=?",
                        params![now(), id],
                    )?;
                    Self::event(
                        tx,
                        Self::DAEMON_STREAM,
                        "app_install_request_reopened",
                        receipt(
                            "app_install_request_reopened",
                            human,
                            &format!("request:{id}"),
                            None,
                            json!({"id": id, "catalog_id": catalog_id, "requested_by": requested_by}),
                        )?,
                    )?;
                }
                return Ok(json!({"id": id, "catalog_id": catalog_id,
                    "requested_by": requested_by, "state": "open", "idempotent": true}));
            }
            let id = format!("req-{}", uuid::Uuid::new_v4().simple());
            tx.execute(
                "INSERT INTO app_install_requests(id,catalog_id,requested_by,at,state)
                 VALUES(?,?,?,?,'open')",
                params![id, catalog_id, requested_by, now()],
            )?;
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                "app_install_requested",
                receipt(
                    "app_install_requested",
                    human,
                    &format!("request:{id}"),
                    None,
                    json!({"id": id, "catalog_id": catalog_id, "requested_by": requested_by}),
                )?,
            )?;
            Ok(json!({"id": id, "catalog_id": catalog_id,
                "requested_by": requested_by, "state": "open"}))
        })
    }

    /// Open requests by requester (the Explorer card's `requested_by_me`).
    pub fn app_install_requests_of(&self, requested_by: &str) -> Result<Vec<String>> {
        owner_ok(requested_by)?;
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT catalog_id FROM app_install_requests WHERE requested_by=? AND state='open'",
                [requested_by],
                |r| r.get::<_, String>(0),
            )?)
        })
    }

    /// Open requests the operator sees (Needs-you + counts).
    pub fn app_install_requests_open(&self) -> Result<Vec<Value>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT id,catalog_id,requested_by,at FROM app_install_requests WHERE state='open' ORDER BY at,id",
                [],
                |r| Ok(json!({"id": r.get::<_,String>(0)?, "catalog_id": r.get::<_,String>(1)?,
                              "requested_by": r.get::<_,String>(2)?, "at": r.get::<_,f64>(3)?})),
            )?)
        })
    }

    /// Per-catalog open request count, for the operator's card.
    pub fn app_install_request_count(&self, catalog_id: &str) -> Result<i64> {
        self.read_tx(|conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM app_install_requests WHERE catalog_id=? AND state='open'",
                [catalog_id],
                |r| r.get(0),
            )?)
        })
    }

    /// Close the open requests for one catalog id on install.
    pub fn app_install_requests_close(&self, catalog_id: &str) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE app_install_requests SET state='installed' WHERE catalog_id=? AND state='open'",
                [catalog_id],
            )?;
            Ok(())
        })
    }

    /// Operator dismissal (Needs-you).
    pub fn app_install_request_dismiss(&self, id: &str) -> Result<()> {
        self.app_install_request_dismiss_as(id, None)
    }

    /// [`Self::app_install_request_dismiss`]: the receipt is written only when
    /// an open request actually changes state, in the same transaction.
    pub fn app_install_request_dismiss_as(&self, id: &str, human: Option<&Value>) -> Result<()> {
        crate::proto::identifier(id, "install request id")?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let open: Option<String> = tx.query_opt(
                "SELECT catalog_id FROM app_install_requests WHERE id=? AND state='open'",
                [id],
                |r| r.get::<_, String>(0),
            )?;
            if let Some(catalog_id) = open {
                tx.execute(
                    "UPDATE app_install_requests SET state='dismissed' WHERE id=? AND state='open'",
                    [id],
                )?;
                Self::event(
                    tx,
                    Self::DAEMON_STREAM,
                    "app_install_request_dismissed",
                    receipt(
                        "app_install_request_dismissed",
                        human,
                        &format!("request:{id}"),
                        None,
                        json!({"id": id, "catalog_id": catalog_id}),
                    )?,
                )?;
            }
            Ok(())
        })
    }

    // ---------- cached update checks (H4) ----------

    /// Record a check's outcome. `has_update` keeps "update ready" off
    /// when the check found none.
    pub fn app_update_check_save(
        &self,
        install_id: &str,
        has_update: bool,
        version: Option<&str>,
        access_change: Option<&Value>,
        digest: Option<&str>,
        proposal: &Value,
    ) -> Result<()> {
        install_id_ok(install_id)?;
        self.write_tx(|conn| {
            conn.execute(
                "INSERT INTO app_update_checks(install_id,checked_at,proposal_json,has_update,version,access_change_json,digest)
                 VALUES(?,?,?,?,?,?,?)
                 ON CONFLICT(install_id) DO UPDATE SET checked_at=excluded.checked_at,
                   proposal_json=excluded.proposal_json, has_update=excluded.has_update,
                   version=excluded.version, access_change_json=excluded.access_change_json,
                   digest=excluded.digest",
                params![
                    install_id,
                    now(),
                    serde_json::to_string(proposal).map_err(|e| Error::internal(e.to_string()))?,
                    has_update as i64,
                    version,
                    access_change.map(|a| serde_json::to_string(a).unwrap_or_default()),
                    digest,
                ],
            )?;
            Ok(())
        })
    }

    /// Every install with a cached "update ready" (the home badge).
    pub fn app_updates_pending(&self) -> Result<Vec<String>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT install_id FROM app_update_checks WHERE has_update=1",
                [],
                |r| r.get::<_, String>(0),
            )?)
        })
    }
}
