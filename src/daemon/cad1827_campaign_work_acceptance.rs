//! CAD-1827 acceptance: the paired `crm-campaign-work/v1` contract.
//!
//! Every case crosses the real screen-service relay, the real consumed
//! mount and the installation's real record file — no fake dispatcher,
//! permission store or clock. The synthetic package declares the two
//! campaign-work operations *and* the contract its manifest requires, so
//! the pairing rule is exercised on both sides: the paired package mounts
//! and serves, the unpaired one refuses to mount.
//!
//! The ticket's outcome is what is asserted: recorded facts only, a
//! revision CAS that refuses stale writes, caller-derived attribution, a
//! content revision bound inside the write transaction, context/install
//! isolation, and a read-only member that may show but never save.

use super::independent_public_owner_attribution::Native;
use super::{call, Scope, Session};
use crate::board_identity::Role;
use crate::operator_auth::{BoardUser, PublicRealm};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CAMPAIGN: &str = "welcome";
const OTHER_CAMPAIGN: &str = "second";
const OPERATIONS: &[&str] = &["campaign-work.show", "campaign-work.save"];
const OWNER: &str = "synthetic-c811-owner";
const MEMBER: &str = "c811-member";
const COMPANY: &str = "co_c811_owner";

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Install one synthetic package. `paired` controls whether the manifest
/// requires `crm-campaign-work`, which is what the screen package's
/// operations must pair with.
fn install(
    native: &Native,
    label: &str,
    operations: &[&str],
    paired: bool,
) -> crate::Result<Scope> {
    let source = native.fx.root.path().join(format!("source-{label}"));
    let screen = source.join("screens/main");
    std::fs::create_dir_all(&screen).unwrap();
    let app = format!("crm-cw-{label}");
    let mut contracts = String::from("      app-screens: [1]\n      screen-records: [1]\n");
    if paired {
        contracts.push_str("      crm-campaign-work: [1]\n");
    }
    let manifest = format!(
        "---\napp: {app}\ntitle: Synthetic campaign work {label}\nversion: '0.1.0'\n\
         summary: Isolated CAD-1827 pairing fixture\nneeds:\n  connections: []\n  requires:\n    \
         contracts:\n{contracts}---\n\nSynthetic only.\n"
    );
    std::fs::write(source.join("app.md"), manifest).unwrap();
    // Existing local consent requires a supported workflow. This authored
    // inert template is admitted but never dispatched.
    std::fs::create_dir_all(source.join("workflows")).unwrap();
    std::fs::write(
        source.join("workflows/synthetic.md"),
        "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: \
         { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, \
         reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: \
         local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic \
         text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: \
         local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic \
         text only\n",
    )
    .unwrap();
    let js = format!("(()=>{{document.getElementById('root').textContent='cw-{label}';}})();");
    std::fs::write(screen.join("client.js"), &js).unwrap();
    let declaration = json!({
        "contract": "app-screens/v2", "app": app, "entry": "client.js",
        "host_contract": "screen-records.v1", "may": ["records.call", "records.slot"],
        "operations": operations, "tools": {},
        "assets": [{"name": "client.js", "media_type": "text/javascript", "size": js.len(), "sha256": hash(js.as_bytes())}],
        "provenance": {"source_digest": hash(label.as_bytes()), "sdk_digest": hash(b"synthetic-sdk"), "toolchain_digest": hash(b"synthetic-toolchain")}
    });
    std::fs::write(screen.join("screens.json"), declaration.to_string()).unwrap();
    let installed = native.rpc(
        "app_workspace_install",
        json!({"source": source.to_str().unwrap()}),
    )?;
    assert_eq!(installed["approved"], true, "{label}: install consent");
    let install = installed["install_id"]
        .as_str()
        .expect("install receipt")
        .to_owned();
    let digest = installed["digest"].clone();
    let context = native.fx.context(&install, label);
    Ok(Scope {
        install,
        context,
        digest,
    })
}

/// Save one operator content revision and return its revision — the value a
/// campaign-work write must bind.
fn content(native: &Native, scope: &Scope, subject: &str) -> i64 {
    let observed = native
        .rpc(
            "app_content_show",
            json!({
                "install_id": scope.install,
                "context_id": scope.context,
                "campaign_id": CAMPAIGN,
            }),
        )
        .ok()
        .and_then(|shown| shown["content"]["revision"].as_i64());
    let mut body = json!({
        "install_id": scope.install,
        "context_id": scope.context,
        "campaign_id": CAMPAIGN,
        "subject": subject,
        "preheader": "Short preheader",
        "blocks": [{"type": "paragraph", "text": "Hello"}],
    });
    if let Some(revision) = observed {
        body["expected_revision"] = json!(revision);
    }
    let saved = native
        .rpc("app_content_save", body)
        .expect("operator content save");
    saved["content"]["revision"]
        .as_i64()
        .expect("content revision")
}

fn service(
    native: &Native,
    session: &Session,
    origin: &str,
    action: &str,
    operation: &str,
    args: Value,
) -> crate::Result<Value> {
    native.rpc(
        "app_screen_service_call",
        native.credentials(session, origin, call(action, operation, args)),
    )
}

fn member_session(native: &Native) -> Session {
    let user = BoardUser {
        sub: MEMBER.into(),
        email: format!("{MEMBER}@example.test"),
        name: "Synthetic member".into(),
        role: "member".into(),
        handle: MEMBER.into(),
    };
    let now = native.shared.operator_now();
    let opened = native
        .shared
        .operator_auth()
        .open_public_bound(
            user,
            "cad1827-member-jti",
            now + 60,
            "cad1827-isolated-acceptance",
            PublicRealm {
                issuer: native.issuer.clone(),
                company: COMPANY.into(),
                host: native.fx.host.clone(),
            },
            now,
        )
        .expect("member public session must open")
        .expect("fresh JTI yields a live member session");
    Session {
        cookie: format!("__Host-aos-board-session={}", opened.token),
        token: opened.token,
        key: String::new(),
    }
}

#[test]
fn cad1827_campaign_work_pairs_and_cas_writes_through_the_relay() {
    let native = Native::new();
    let scope = install(&native, "paired", OPERATIONS, true).expect("paired install");
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);

    // Nothing recorded yet: no work row, no content, and no invented value.
    let empty = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("paired show must be served");
    assert_eq!(empty["contract"], "crm-campaign-work/v1");
    assert_eq!(empty["campaign_id"], CAMPAIGN);
    assert_eq!(empty["source"]["install_id"], scope.install);
    assert_eq!(empty["source"]["context_id"], scope.context);
    assert_eq!(empty["source"]["app"], format!("crm-cw-{}", "paired"));
    assert_eq!(empty["work"], Value::Null, "no row is never inferred");
    assert_eq!(
        empty["content"],
        Value::Null,
        "no content is never inferred"
    );

    let first_content = content(&native, &scope, "Spring launch");
    let shown = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("show after content");
    assert_eq!(shown["content"]["revision"], first_content);
    assert!(shown["content"]["digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(shown["work"], Value::Null);

    // The first write sends 0. Attribution comes from the verified caller,
    // never from the body, and the content revision is bound in-transaction.
    let saved = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.save",
        json!({
            "campaign_id": CAMPAIGN,
            "expected_revision": 0,
            "assignee": "Ada Chen",
            "due_at": "2026-11-01",
            "status": "blocked",
            "blocked_reason": "waiting on legal",
        }),
    )
    .expect("first save");
    assert_eq!(saved["work"]["revision"], 1);
    assert_eq!(saved["work"]["assignee"], "Ada Chen");
    assert_eq!(
        saved["work"]["assigned_by"],
        format!("human:{COMPANY}:{OWNER}"),
        "attribution is the authenticated caller"
    );
    assert_eq!(saved["work"]["due_at"], "2026-11-01");
    assert_eq!(saved["work"]["status"], "blocked");
    assert_eq!(saved["work"]["blocked_reason"], "waiting on legal");
    assert_eq!(saved["work"]["content_revision"], first_content);
    assert_eq!(saved["content"]["revision"], first_content);

    // The same token is stale: nothing is overwritten and no row is invented.
    let stale = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.save",
        json!({"campaign_id": CAMPAIGN, "expected_revision": 0, "due_at": "2026-12-01"}),
    )
    .expect_err("a stale token must refuse");
    assert_eq!(stale.kind(), "conflict");
    assert_eq!(stale.code(), Some("stale_revision"));
    let unchanged = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("show after refusal");
    assert_eq!(unchanged["work"]["revision"], 1);
    assert_eq!(unchanged["work"]["due_at"], "2026-11-01");

    // A caller can never forge attribution, revisions or the decision.
    for forged in [
        json!({"campaign_id": CAMPAIGN, "expected_revision": 1, "assigned_by": "someone", "assignee": "Eve"}),
        json!({"campaign_id": CAMPAIGN, "expected_revision": 1, "revision": 9, "due_at": "2026-12-02"}),
        json!({"campaign_id": CAMPAIGN, "expected_revision": 1, "next_decision": {"key": "send"}}),
    ] {
        let refused = service(
            &native,
            &owner,
            "public",
            &action,
            "campaign-work.save",
            forged.clone(),
        )
        .expect_err("a forged field must refuse");
        assert!(
            matches!(refused.kind(), "rejected" | "invalid"),
            "forged {forged} refused as {}",
            refused.kind()
        );
    }

    // A reason-only edit of an already-blocked row is a valid CAS write.
    let reason_only = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.save",
        json!({
            "campaign_id": CAMPAIGN,
            "expected_revision": 1,
            "status": "blocked",
            "blocked_reason": "waiting on finance",
        }),
    )
    .expect("reason-only edit");
    assert_eq!(reason_only["work"]["revision"], 2);
    assert_eq!(reason_only["work"]["blocked_reason"], "waiting on finance");

    // Resuming clears the block text; a later orphan reason refuses.
    let resumed = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.save",
        json!({"campaign_id": CAMPAIGN, "expected_revision": 2, "status": "in_progress"}),
    )
    .expect("resume");
    assert_eq!(resumed["work"]["revision"], 3);
    assert_eq!(resumed["work"]["blocked_reason"], Value::Null);
    let orphan = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.save",
        json!({"campaign_id": CAMPAIGN, "expected_revision": 3, "blocked_reason": "late"}),
    )
    .expect_err("a reason without the blocked status must refuse");
    assert_eq!(orphan.kind(), "rejected");

    // A new content revision re-binds the row to what the write saw.
    let second_content = content(&native, &scope, "Spring launch follow-up");
    assert_ne!(second_content, first_content);
    let rebound = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.save",
        json!({"campaign_id": CAMPAIGN, "expected_revision": 3, "due_at": "2026-11-05"}),
    )
    .expect("rebind");
    assert_eq!(rebound["work"]["content_revision"], second_content);
    assert_eq!(rebound["content"]["revision"], second_content);

    // The same install's second context sees no row, and its own write never
    // touches the first context's row.
    let other_context = native.fx.context(&scope.install, "paired-second");
    let other_scope = Scope {
        install: scope.install.clone(),
        context: other_context,
        digest: scope.digest.clone(),
    };
    let other_action = native.mount(&owner, "public", &other_scope, 1);
    let isolated = service(
        &native,
        &owner,
        "public",
        &other_action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("second context show");
    assert_eq!(isolated["work"], Value::Null);
    assert_eq!(isolated["content"], Value::Null);
    let other_saved = service(
        &native,
        &owner,
        "public",
        &other_action,
        "campaign-work.save",
        json!({"campaign_id": OTHER_CAMPAIGN, "expected_revision": 0, "assignee": "Grace"}),
    )
    .expect("second context first write");
    assert_eq!(
        other_saved["work"]["revision"], 1,
        "a sibling context starts its own CAS chain"
    );
    let untouched = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("first context unchanged");
    assert_eq!(untouched["work"]["assignee"], "Ada Chen");
    assert_eq!(untouched["work"]["revision"], 4);

    // A package that lists the operations without requiring the contract is
    // not installable: the host refuses it outright instead of installing a
    // screen whose work actions could not run.
    let unpaired = install(&native, "unpaired", OPERATIONS, false)
        .err()
        .expect("an unpaired campaign-work package must refuse to install");
    assert!(
        unpaired.to_string().contains("crm-campaign-work"),
        "the refusal names the missing pairing: {unpaired}"
    );
}

#[test]
fn cad1827_campaign_work_member_reads_but_never_saves() {
    let native = Native::new_with_roles(&[(OWNER, Role::Operator), (MEMBER, Role::Member)]);
    let scope = install(&native, "member", OPERATIONS, true).expect("paired install");
    let (owner, _) = native.owner();
    let owner_action = native.mount(&owner, "public", &scope, 1);
    service(
        &native,
        &owner,
        "public",
        &owner_action,
        "campaign-work.save",
        json!({"campaign_id": CAMPAIGN, "expected_revision": 0, "assignee": "Ada Chen"}),
    )
    .expect("operator write before the member reads");

    let member = member_session(&native);
    let member_action = native.mount(&member, "public", &scope, 1);
    // `campaign-work.show` is a read: the read-only member mount serves it.
    let shown = service(
        &native,
        &member,
        "public",
        &member_action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("a member may read recorded campaign work");
    assert_eq!(shown["work"]["revision"], 1);
    assert_eq!(shown["work"]["assignee"], "Ada Chen");
    // `campaign-work.save` is a write: the member mount never admits it, and
    // the refusal happens before the store.
    let refused = service(
        &native,
        &member,
        "public",
        &member_action,
        "campaign-work.save",
        json!({"campaign_id": CAMPAIGN, "expected_revision": 1, "due_at": "2026-12-01"}),
    )
    .expect_err("a read-only member mount must refuse the work write");
    assert!(
        matches!(refused.kind(), "rejected" | "invalid"),
        "member save refused as {}",
        refused.kind()
    );
    // The refused write changed nothing.
    let after = service(
        &native,
        &owner,
        "public",
        &owner_action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("operator read after the member refusal");
    assert_eq!(after["work"]["revision"], 1);
    assert_eq!(after["work"]["due_at"], Value::Null);
}
