//! Minimal module-local helpers for the CAD-1128 unsupported-consumer
//! canaries — REV 4. ONLY the functions the executable tests use: no unused
//! helpers, no unused imports. Real [`Rig`], real operator RPC socket, real
//! `Result`-propagating DB reads. Owned by cad1128-confidentiality-swe.
#![cfg(all(unix, feature = "test-seam"))]

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use crate::daemon::custody_acceptance_fixture::Rig;
use crate::daemon::Shared;
use crate::error::{Error, Result};
use crate::store::app_records::RecordStore;
use crate::store::app_sends::{send_digest, SendDraft};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};

/// The admission code every unsupported encrypted consumer must surface.
pub const UNSUPPORTED: &str = "unsupported_custody_use";
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

// --- operator RPC over the real socket -------------------------------------

/// Fresh rig root with strict-owner mode. `TempDir::new()` defaults to
/// 0755, which the daemon's config/vault ownership check refuses — the rig
/// root must be 0700 like `custody_security_r2`'s (freshly owned, never a
/// chmod on an existing/production dir).
pub fn new_root() -> tempfile::TempDir {
    let root = tempfile::TempDir::new().expect("canary temp root failed");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("canary temp root mode failed");
    root
}

fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, RPC_TIMEOUT)
    })
}

/// Require success; panic with the refusal code otherwise.
pub fn op(rig: &Rig, method: &str, params: Value) -> Value {
    rpc(rig, method, params)
        .unwrap_or_else(|e| panic!("{method} refused unexpectedly: {}", e.code().unwrap_or("?")))
}

/// Require a refusal; return its code.
pub fn refused(rig: &Rig, method: &str, params: Value) -> Option<String> {
    match rpc(rig, method, params) {
        Err(e) => e.code().map(|c| c.to_string()),
        Ok(v) => panic!("{method} unexpectedly succeeded: {v}"),
    }
}

// --- custody lifecycle: a cold vault must initialize + unlock first --------

/// `Rig::start` leaves the vault cold. Provision the header then unlock —
/// required before any `enroll_smtp` (which fails `locked` otherwise).
pub fn unlock_vault(rig: &Rig) {
    op(
        rig,
        "connection_custody_initialize",
        json!({"accept_same_uid_risk": true}),
    );
    op(
        rig,
        "connection_custody_unlock",
        json!({"accept_same_uid_risk": true}),
    );
}

// --- the live daemon Shared (in-crate entry points) -------------------------

pub fn shared(rig: &Rig) -> Arc<Shared> {
    rig.shared()
}

// --- real enrollment: SMTP-shape with the rig's real auth secret -----------

/// Enroll an SMTP connection with `secret` = rig `A`/`B` (the only creds the
/// TLS peer authenticates). Encrypted rig -> `encrypted-sqlite-v1:` row;
/// legacy rig -> `FILE` row. Returns `connection_id`.
pub fn enroll_smtp(rig: &Rig, account: &str, secret: &str) -> String {
    let out = op(
        rig,
        "connection_create",
        json!({
            "provider": "smtp", "account": account, "shape": "smtp",
            "host": "localhost", "port": rig.smtp_port, "tls_mode": "implicit",
            "username": "fixture-user", "secret": secret,
            "sender": "fixture@fixture.cadence", "sender_name": "Fixture",
            "scopes": ["email:send"], "accept_same_uid_risk": true
        }),
    );
    out["connection"]["id"]
        .as_str()
        .or_else(|| out["connection_id"].as_str())
        .expect("enroll returned no connection_id")
        .to_string()
}

/// Deterministic builtin `local` connection id — credential-less, `builtin`
/// kind, never a `platform_credentials` row (over-refusal control).
pub fn builtin_connection_id(rig: &Rig) -> String {
    format!(
        "builtin-{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{}:local:local", rig.workspace_id).as_bytes()
        )
        .simple()
    )
}

// --- direct-DB observable reads — Result-propagating, never a disguised 0 --

fn core(rig: &Rig) -> Result<Connection> {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| Error::internal(format!("canary core DB unreadable: {e}")))
}

fn records(rig: &Rig, install_id: &str) -> Result<Connection> {
    let path = rig
        .state_dir
        .join("app-records")
        .join(format!("{install_id}.sqlite3"));
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| Error::internal(format!("canary records DB unreadable: {e}")))
}

pub fn is_encrypted(rig: &Rig, connection_id: &str) -> Result<bool> {
    let tag: Option<String> = core(rig)?
        .query_row(
            "SELECT custody FROM platform_credentials WHERE connection_id=?",
            [connection_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| Error::internal(format!("custody read failed: {e}")))?;
    Ok(tag.is_some_and(|t| t.starts_with(crate::platform::encrypted_sqlite::TAG_PREFIX)))
}

/// Delivery state (`Ok(None)` = row absent; read error propagates).
pub fn delivery_state(
    rig: &Rig,
    install_id: &str,
    context: &str,
    send_id: &str,
    customer_id: &str,
) -> Result<Option<String>> {
    records(rig, install_id)?
        .query_row(
            "SELECT state FROM app_campaign_deliveries WHERE context_id=? AND send_id=? AND customer_id=?",
            rusqlite::params![context, send_id, customer_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| Error::internal(format!("delivery read failed: {e}")))
}

/// `app_unsubscribe_tokens` rows for `send_id`.
pub fn unsubscribe_tokens(rig: &Rig, install_id: &str, send_id: &str) -> Result<usize> {
    records(rig, install_id)?
        .query_row(
            "SELECT count(*) FROM app_unsubscribe_tokens WHERE send_id=?",
            [send_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as usize)
        .map_err(|e| Error::internal(format!("unsubscribe token read failed: {e}")))
}

/// `crm_unsubscribe_index` rows for `(install, context)`.
pub fn unsubscribe_index_count(rig: &Rig, install_id: &str, context: &str) -> Result<usize> {
    core(rig)?
        .query_row(
            "SELECT count(*) FROM crm_unsubscribe_index WHERE install_id=? AND context_id=?",
            rusqlite::params![install_id, context],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as usize)
        .map_err(|e| Error::internal(format!("unsubscribe index read failed: {e}")))
}

// --- real verb helpers ------------------------------------------------------

/// Fresh context + one consenting customer -> `(context_id, customer_id, email)`.
pub fn context_with_customer(rig: &Rig, install: &str, tag: &str) -> (String, String, String) {
    let context = op(
        rig,
        "app_context_create",
        json!({"install_id": install, "label": format!("canary-{tag}"),
            "input_defaults": {}, "request_id": format!("ctx-{tag}")}),
    )["context"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let email = format!("{tag}@example.test");
    op(
        rig,
        "app_record_create",
        json!({"install_id": install, "context_id": context, "record_id": format!("cust-{tag}"),
            "profile": {"schema":1,"display_name":"Canary","email":email,
                "tags":[],"consent":{"email":"granted"}}}),
    );
    (context, format!("cust-{tag}"), email)
}

/// Save + approve content, prepare the audience freeze.
pub fn content_and_audience(rig: &Rig, install: &str, context: &str, campaign: &str, freeze: &str) {
    op(
        rig,
        "app_content_save",
        json!({"install_id": install, "context_id": context, "campaign_id": campaign,
            "subject": "Canary", "preheader": "Pre",
            "blocks": [{"type":"paragraph","text":"Body."}]}),
    );
    let revision = op(
        rig,
        "app_content_show",
        json!({"install_id": install, "context_id": context, "campaign_id": campaign}),
    )["content"]["revision"]
        .as_u64()
        .unwrap();
    op(
        rig,
        "app_content_approve",
        json!({"install_id": install, "context_id": context, "campaign_id": campaign,
            "expected_revision": revision}),
    );
    op(
        rig,
        "app_audience_prepare",
        json!({"install_id": install, "context_id": context, "freeze_id": freeze,
            "base": {"mode": "all"}, "max_recipients": 50}),
    );
}

/// `crm_smtp_bind` — link `(install, context)` to `connection_id`.
pub fn smtp_bind(rig: &Rig, install: &str, context: &str, connection_id: &str) -> Result<()> {
    rpc(
        rig,
        "crm_smtp_bind",
        json!({"install_id": install, "context_id": context,
            "connection_id": connection_id, "request_id": format!("bind-{connection_id}")}),
    )
    .map(|_| ())
}

/// Configure the unsubscribe origin — a real `crm_send_origin_set`. Every
/// campaign prepare requires it; absent it the origin precondition fails
/// before any custody classification. Call before `prepare_and_approve`.
pub fn send_origin(rig: &Rig) {
    op(
        rig,
        "crm_send_origin_set",
        json!({"unsubscribe_origin": "https://unsub.example.test"}),
    );
}

/// `crm_send_prepare` + `crm_send_approve`; returns `send_id` or the refusal.
pub fn prepare_and_approve(
    rig: &Rig,
    install: &str,
    context: &str,
    campaign: &str,
    freeze: &str,
) -> Result<String> {
    let prep = rpc(
        rig,
        "crm_send_prepare",
        json!({"install_id": install, "context_id": context, "campaign_id": campaign,
            "audience_freeze_id": freeze, "request_id": format!("prep-{campaign}")}),
    )?;
    let send_id = prep["send"]["send_id"]
        .as_str()
        .or_else(|| prep["send_id"].as_str())
        .ok_or_else(|| Error::internal("prepare returned no send_id"))?
        .to_string();
    let digest = prep["send_digest"]
        .as_str()
        .ok_or_else(|| Error::internal("no digest"))?
        .to_string();
    rpc(
        rig,
        "crm_send_approve",
        json!({"install_id": install, "context_id": context, "send_id": send_id, "send_digest": digest}),
    )?;
    Ok(send_id)
}

/// Seed a STALE `sending` campaign whose `connection_id` resolves to a
/// now-encrypted record — the residual window the worker gates close (a send
/// prepared before the tag became encrypted). Real `SendDraft`→real
/// `send_digest`→`app_campaign_send_prepare`+`approve` on the real
/// `RecordStore`. Setup seed, not a canned outcome.
#[allow(clippy::too_many_arguments)]
pub fn seed_stale_encrypted_send(
    rig: &Rig,
    install: &str,
    context: &str,
    campaign: &str,
    freeze: &str,
    connection_id: &str,
    customer_id: &str,
    customer_email: &str,
) -> Result<String> {
    let records = RecordStore::open(&rig.state_dir, install)?;
    let send_id = format!("send-stale-{}", uuid::Uuid::new_v4().simple());
    let mut draft = SendDraft {
        send_id: send_id.clone(),
        campaign_id: campaign.to_string(),
        request_id: format!("req-{send_id}"),
        content_revision: 0,
        content_digest: "stale-content".into(),
        audience_freeze_id: freeze.into(),
        audience_digest: "stale-audience".into(),
        connection_id: connection_id.to_string(),
        auth_revision: 1,
        link_revision: 1,
        link_digest: "link-d".into(),
        max_recipients: 50,
        unsubscribe_origin: "https://unsub.example.test".into(),
        send_digest: String::new(),
    };
    draft.send_digest = send_digest(install, context, &draft);
    records.app_campaign_send_prepare(context, &draft)?;
    records.app_campaign_send_approve(
        context,
        &send_id,
        &[(
            customer_id.to_string(),
            customer_email.to_string(),
            format!("idem-{send_id}"),
        )],
    )?;
    Ok(send_id)
}

/// Wait for a delivery row to leave `queued`/`submitting` (bounded).
pub fn wait_delivery_state(
    rig: &Rig,
    install_id: &str,
    context: &str,
    send_id: &str,
    customer_id: &str,
    secs: u64,
) -> Result<Option<String>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let s = delivery_state(rig, install_id, context, send_id, customer_id)?;
        if let Some(state) = &s {
            if state != "queued" && state != "submitting" {
                return Ok(s);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
