//! CAD-1128 ELIGIBLE app-quote — REAL SMTP builtin-toggle counterexample.
//! Mount as a direct child of `app_capabilities_rpc` (`mod
//! early_app_quote_tests;`). Requires `Rig::start_with_platforms` + a
//! test-only `SmtpQuoteAdapter` (below) injected as `opts.platforms["smtp"]`
//! — replacing the production `smtp::attach` for this rig only. Owned by
//! cad1128-confidentiality-swe.
//!
//! PM counterexample, independently assessable, no production widening:
//! the adapter's `connection_descriptor()` initially declares `builtin_accounts
//! :[]` — a REAL non-builtin SMTP enroll proceeds and writes a REAL
//! `encrypted-sqlite-v1:` `platform_credentials(smtp,account)` row. An
//! `AtomicBool` toggle then declares `builtin_accounts:[account]` — an
//! explicit UNSUPPORTED-REGISTRATION-HISTORY precondition (the same account
//! became builtin AFTER its real encrypted enroll, never a supported route).
//! `app_capability_credential` on the builtin path returns `Ok(Vec::new())`
//! (a real current `connection_list_locked` receipt, not a decrypt), so the
//! adapter's `quote_app_capability` callback REALLY fires — counted.
//!
//! RED: `refuse_encrypted_binding` fires BEFORE `platforms.get(provider)` →
//! `app_capability_credential`. Removing it lets the call reach the builtin
//! credential path (`Ok(Vec::new())`) and then the REAL quote callback —
//! `hits > 0` is the behavioral red, NOT a helper-removal masked by a late
//! loader (the builtin path never loads encrypted bytes).
#![cfg(all(unix, feature = "test-seam"))]

use crate::daemon::cad1128_canary_fx as fx;
use crate::store::app_bindings::BindingProof;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

const A: &str = "cad1128-custody-a";
const B: &str = "cad1128-custody-b";

// ---------------------------------------------------------------------------
// Test-only `SmtpQuoteAdapter`: delegates everything to the real
// `SmtpAdapter` EXCEPT (a) `connection_descriptor()` — `builtin_accounts`
// rides an `AtomicBool` so the same account can be declared builtin after a
// real encrypted enroll (the unsupported-history precondition), (b)
// `app_credentialless_account` — reports the account credential-less when
// toggled builtin, and (c) `quote_app_capability` — counts the callback and
// returns a real `AppCapabilityQuote`. Registered under `"smtp"` so the real
// `platform_credentials(smtp,account)` row + `provider`/`account` lookups all
// resolve genuinely.
// ---------------------------------------------------------------------------

struct SmtpQuoteAdapter {
    inner: crate::platform::smtp::SmtpAdapter,
    /// Custom tool table: the real `smtp` platform + `smtp-connections/1`
    /// manifest pin, the real `email.send` (`send`) tool, PLUS the eligible
    /// `read_instagram_posts` (`read`, `provider.read`) — so
    /// `app_capability_quote`'s `classify_call`/`read|draft` check passes.
    table: crate::contract_fixture::ToolTable,
    account: String,
    builtin: Arc<AtomicBool>,
    quote_calls: Arc<AtomicUsize>,
}

impl crate::platform::PlatformAdapter for SmtpQuoteAdapter {
    fn table(&self) -> &crate::contract_fixture::ToolTable {
        &self.table
    }
    fn connection_descriptor(&self) -> Option<crate::platform::connections::ProviderDescriptor> {
        let mut d = self.inner.connection_descriptor()?;
        if self.builtin.load(Ordering::SeqCst) {
            d.builtin_accounts = vec![self.account.clone()];
        }
        Some(d)
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
    }
    fn app_credentialless_account(&self, account: &str) -> bool {
        self.builtin.load(Ordering::SeqCst) && account == self.account
    }
    fn quote_app_capability(
        &self,
        _credential: &[u8],
        _binding: &serde_json::Value,
    ) -> std::result::Result<crate::platform::AppCapabilityQuote, String> {
        self.quote_calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::platform::AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: 4200,
            units: 1,
            total_price_micros: 4200,
            price_revision: "price-v1".into(),
        })
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.table.manifest_version.clone()
    }
    fn preview(&self, account: &str, tool: &str, input: &serde_json::Value) -> String {
        self.inner.preview(account, tool, input)
    }
    fn execute(
        &self,
        credential: &[u8],
        tool: &str,
        input: &serde_json::Value,
        key: &str,
        hash: Option<&str>,
    ) -> std::result::Result<serde_json::Value, String> {
        self.inner.execute(credential, tool, input, key, hash)
    }
    fn execute_app_artifact(
        &self,
        credential: &[u8],
        tool: &str,
        input: &serde_json::Value,
        idempotency_key: &str,
        expected_hash: Option<&str>,
    ) -> std::result::Result<serde_json::Value, crate::platform::AppArtifactError> {
        self.inner
            .execute_app_artifact(credential, tool, input, idempotency_key, expected_hash)
    }
    fn read_back(
        &self,
        tool: &str,
        input: &serde_json::Value,
    ) -> crate::contract_fixture::Verified {
        self.inner.read_back(tool, input)
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.inner.source_hash(agent, source)
    }
    fn implied_source(&self, agent: &str, tool: &str, input: &serde_json::Value) -> Option<String> {
        self.inner.implied_source(agent, tool, input)
    }
}

/// Build `opts.platforms`/`provider_deployments` for `start_with_platforms`:
/// inject `SmtpQuoteAdapter` under `"smtp"` (replacing `smtp::attach` for
/// this rig) + empty `providers` (never reads production trusted file).
/// Returns `(platforms, deployments, builtin_toggle, quote_counter)`.
fn smtp_quote_platforms(
    account: &str,
) -> (
    crate::daemon::effect_rpc::PlatformMap,
    crate::platform::deployments::DeploymentMetadata,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
) {
    let builtin = Arc::new(AtomicBool::new(false));
    let quote_calls = Arc::new(AtomicUsize::new(0));
    // Custom table: real smtp platform + pin + email.send + the eligible read
    // tool the adapter's `quote_app_capability` serves.
    let table = crate::contract_fixture::ToolTable::from_json(&json!({
        "platform": "smtp",
        "manifest_version": "smtp-connections/1",
        "tools": [
            {"tool": "email.send", "effect": "send", "scopes": ["email:send"]},
            {"tool": "read_instagram_posts", "effect": "read", "scopes": ["provider.read"]}
        ]
    }))
    .expect("smtp quote table parse failed");
    let adapter = SmtpQuoteAdapter {
        inner: crate::platform::smtp::SmtpAdapter::default(),
        table,
        account: account.to_string(),
        builtin: builtin.clone(),
        quote_calls: quote_calls.clone(),
    };
    let mut platforms = crate::daemon::effect_rpc::PlatformMap::new();
    platforms.insert("smtp".into(), std::sync::Arc::new(adapter));
    let deployments =
        crate::platform::deployments::DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#)
            .expect("empty deployments parse failed");
    (platforms, deployments, builtin, quote_calls)
}

/// The eligible read binding for a builtin `smtp`/`account`. `provider`/
/// `account` resolve to the real `platform_credentials(smtp,account)` row —
/// `refuse_encrypted_target` reads its custody tag independent of
/// `connection_id`. `connection_kind:"builtin"` + `connection_id`=the
/// builtin row's derived id (stamped from `connection_list` — the
/// `connection_list_locked` builtin row's `id`, not the enrolled
/// `connection_id`). `mapping` declares the eligible `read` tool the
/// adapter's `quote_app_capability` serves.
fn smtp_read_proof(account: &str) -> BindingProof {
    BindingProof {
        id: "proof-smtp-read".into(),
        revision: 1,
        digest: "bd-smtp-read".into(),
        config: json!({
            "provider": "smtp", "account": account,
            "connection_kind": "builtin",
            "mapping": {
                "capability": "social.read", "version": 1,
                "action": "list_posts", "tool": "read_instagram_posts",
                "effect": "read", "resource_kind": "connection_account",
                "scopes": ["provider.read"]
            }
        }),
    }
}

/// Stamp the real `connection_list` receipt fields onto the proof config —
/// `app_capability_credential`'s builtin path re-validates
/// `workspace_id`/`descriptor_revision`/`sink_registration`/`connection_id`/
/// `connection_revision`/`registration_digest`/`reviewed_pin`/`reported_pin`
/// against the live `connection_list_locked` row. Reads the real row for
/// `account` (must be `kind=="builtin"` post-toggle).
fn stamp_builtin_receipt(
    rig: &crate::daemon::custody_acceptance_fixture::Rig,
    account: &str,
    proof: &mut BindingProof,
) {
    let list = fx::op(rig, "connection_list", json!({}));
    let row = list["connections"]
        .as_array()
        .or_else(|| list.as_array())
        .expect("connection_list returned no rows")
        .iter()
        .find(|r| r["account"] == account && r["provider"] == "smtp" && r["kind"] == "builtin")
        .expect("no builtin smtp row for account");
    let shared = fx::shared(rig);
    let ws = shared
        .store
        .connection_workspace_id()
        .expect("workspace id missing");
    let cfg = &mut proof.config;
    cfg["schema"] = json!(1);
    cfg["workspace_id"] = json!(ws);
    cfg["connection_id"] = row["id"].clone();
    cfg["connection_revision"] = row["revision"].clone();
    cfg["registration_digest"] = row["registration_digest"].clone();
    cfg["descriptor_revision"] = row["descriptor"]["revision"].clone();
    cfg["sink_registration"] = json!(crate::platform::smtp::REGISTRATION);
    cfg["reviewed_pin"] = row["status"]["reviewed_pin"].clone();
    cfg["reported_pin"] = row["status"]["reported_pin"].clone();
}

fn n(rig: &crate::daemon::custody_acceptance_fixture::Rig, sql: &str) -> i64 {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row(sql, [], |r| r.get(0))
        .expect("residue count query failed")
}

// NOTE: The frozen receipt fields (`workspace_id`/`descriptor_revision`/
// `sink_registration`/`connection_revision`/`registration_digest`/
// `reviewed_pin`/`reported_pin`) must be read from the REAL
// `connection_list_locked` row for the account and stamped into the proof
// config at quote time — `app_capability_credential`'s builtin path
// re-validates them. The driver below stamps them after the toggle.

/// POSITIVE — a NON-encrypted `smtp`/`account` builtin binding: after the
/// toggle, `app_capability_quote` reaches the real `SmtpQuoteAdapter` and
/// returns a REAL `Ok` quote; `quote_calls >= 1` proves the callback fired.
#[test]
fn smtp_builtin_quote_legacy_returns_real_quote() {
    let dir = fx::new_root();
    let (platforms, deployments, builtin, quote_calls) = smtp_quote_platforms("legacy-acct");
    let rig = crate::daemon::custody_acceptance_fixture::Rig::start_with_platforms(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
        false, // legacy custody — positive control
        platforms,
        Some(deployments),
    )
    .expect("rig start failed");
    // Real non-encrypted SMTP enroll while `builtin_accounts` is still [] —
    // the credential row is genuine (legacy custody = no encrypted tag).
    let conn = fx::enroll_smtp(&rig, "legacy-acct", A);
    assert!(!fx::is_encrypted(&rig, &conn).unwrap());
    // Toggle the registration-history precondition: the same account is now
    // declared builtin (an UNSUPPORTED-history residual, never a supported
    // route — the real encrypted enroll already wrote its row).
    builtin.store(true, Ordering::SeqCst);
    let mut proof = smtp_read_proof("legacy-acct");
    stamp_builtin_receipt(&rig, "legacy-acct", &mut proof);
    let shared = fx::shared(&rig);
    // Match the real `rpc_app_capability` path: hold the custody section +
    // the release lock before `app_capability_quote` (it reads
    // `platform_credentials` under the serialized custody view).
    let _custody = shared.custody_operation().expect("custody op failed");
    let _release = shared
        .app_release_lock
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let quote = shared
        .app_capability_quote(&proof)
        .expect("legacy smtp builtin quote failed");
    assert!(quote.total_price_micros > 0, "quote has no price");
    assert_eq!(
        quote_calls.load(Ordering::SeqCst),
        1,
        "quote callback did not fire"
    );
    // Release the held custody/release locks BEFORE fixture teardown —
    // `rig.finish()` must not run while the custody section is held.
    drop(_release);
    drop(_custody);
    rig.finish().unwrap();
}

/// NEGATIVE — a REAL `encrypted-sqlite-v1:` `platform_credentials(smtp,
/// account)` row (a REAL `connection_create` enroll on the encrypted rig)
/// refuses `unsupported_custody_use` BEFORE `platforms.get(provider)` /
/// `app_capability_credential` — zero claims/results/effects residue AND
/// `quote_calls == 0` (the gate, not a late loader, blocks the callback).
///
/// CURRENT-DEFECT RED: removing `refuse_encrypted_binding` lets the call
/// reach `app_capability_credential`'s builtin path (`Ok(Vec::new())` —
/// the real current receipt validates after the toggle) and then the REAL
/// `quote_app_capability` callback — `quote_calls > 0` is the behavioral
/// red, NOT a helper-removal masked by a late loader (the builtin path
/// never loads encrypted bytes).
#[test]
fn smtp_builtin_quote_encrypted_refuses_zero_callback() {
    let dir = fx::new_root();
    let (platforms, deployments, builtin, quote_calls) = smtp_quote_platforms("enc-acct");
    let rig = crate::daemon::custody_acceptance_fixture::Rig::start_with_platforms(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
        true, // encrypted custody — the refusal target
        platforms,
        Some(deployments),
    )
    .expect("rig start failed");
    fx::unlock_vault(&rig);
    // Real `connection_create` SMTP enroll on ENCRYPTED custody — the
    // `platform_credentials(smtp,enc-acct)` row carries the REAL
    // `encrypted-sqlite-v1:` tag. `builtin_accounts` is still [] at enroll.
    let conn = fx::enroll_smtp(&rig, "enc-acct", A);
    assert!(fx::is_encrypted(&rig, &conn).unwrap());
    // Toggle: the same account is now builtin — the unsupported-history
    // residual that lets `app_capability_credential` take the builtin path.
    builtin.store(true, Ordering::SeqCst);
    let mut proof = smtp_read_proof("enc-acct");
    stamp_builtin_receipt(&rig, "enc-acct", &mut proof);
    let shared = fx::shared(&rig);
    // Match the real `rpc_app_capability` path: hold the custody section +
    // the release lock before `app_capability_quote`.
    let _custody = shared.custody_operation().expect("custody op failed");
    let _release = shared
        .app_release_lock
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let r = shared.app_capability_quote(&proof);
    // Observe the callback/residue BEFORE extracting the refusal — a failed
    // refusal assertion must not hide the residue evidence.
    assert_eq!(
        quote_calls.load(Ordering::SeqCst),
        0,
        "encrypted binding reached the quote callback"
    );
    assert_eq!(
        n(&rig, "SELECT COUNT(*) FROM app_capability_claims"),
        0,
        "a claim row was written for an encrypted binding"
    );
    assert_eq!(
        n(&rig, "SELECT COUNT(*) FROM app_capability_results"),
        0,
        "a result row was written for an encrypted binding"
    );
    assert_eq!(
        r.expect_err("encrypted smtp builtin quote did not refuse")
            .code(),
        Some(fx::UNSUPPORTED),
        "encrypted smtp builtin quote must refuse unsupported_custody_use before quote"
    );
    // Release the held custody/release locks BEFORE fixture teardown.
    drop(_release);
    drop(_custody);
    rig.finish().unwrap();
}
