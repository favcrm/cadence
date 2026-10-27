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

/// The core-facing read boundary: caller-selected scope that the host
/// verifies (installed package + live context) before any row is read.
fn read(
    native: &Native,
    install: &str,
    context: &str,
    campaign: &str,
    expected: Option<i64>,
) -> crate::Result<Value> {
    let mut params = json!({
        "install_id": install,
        "context_id": context,
        "campaign_id": campaign,
    });
    if let Some(revision) = expected {
        params["expected_revision"] = json!(revision);
    }
    native.rpc("crm_campaign_work_read", params)
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

    // A reason-only write against this blocked row must still carry the
    // blocked status itself; the row's current status is not the request's
    // claim. The refusal changes nothing.
    let omitted_status = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.save",
        json!({"campaign_id": CAMPAIGN, "expected_revision": 2, "blocked_reason": "waiting on tax"}),
    )
    .expect_err("an omitted blocked status must refuse");
    assert_eq!(omitted_status.kind(), "rejected");
    let unchanged_reason = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("show after the refusal");
    assert_eq!(unchanged_reason["work"]["revision"], 2);
    assert_eq!(
        unchanged_reason["work"]["blocked_reason"],
        "waiting on finance"
    );

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

    // The same stale refusal over the board's HTTP peer keeps its own code:
    // the board route must not collapse `stale_revision` into `conflict`.
    let (status, body) = native.fx.http(
        "operator",
        &owner,
        &call(
            &action,
            "campaign-work.save",
            json!({"campaign_id": CAMPAIGN, "expected_revision": 0, "due_at": "2026-12-09"}),
        ),
    );
    assert_eq!(status, 409, "the HTTP peer answers a conflict status");
    assert_eq!(
        body["code"], "stale_revision",
        "the stale code survives HTTP"
    );
    let after_http = service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.show",
        json!({"campaign_id": CAMPAIGN}),
    )
    .expect("show after the HTTP refusal");
    assert_eq!(after_http["work"]["revision"], 4);
    assert_eq!(after_http["work"]["due_at"], "2026-11-05");

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

/// Corrupt one installed screen asset under the PM's bundle directory, the
/// way a hand-edited or half-written install looks: app.md and the catalog
/// stay valid, the declaration's asset digest no longer matches.
fn invalidate_installed_screen(native: &Native, install: &str) {
    let root = native
        .fx
        .root
        .path()
        .join("pm/.apps/installations")
        .join(install);
    let mut stack = vec![root];
    let mut target = None;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.ends_with("screens/main/client.js") {
                target = Some(path);
            }
        }
    }
    let path = target.expect("an installed screen asset is on disk");
    let mut bytes = std::fs::read(&path).expect("installed screen asset bytes");
    bytes.extend_from_slice(b"\n");
    std::fs::write(&path, bytes).expect("invalidate the installed screen asset");
}

#[test]
fn cad1827_campaign_work_core_read_is_authorized_and_stateful() {
    let native = Native::new();
    let scope = install(&native, "core-read", OPERATIONS, true).expect("paired install");
    let (owner, _) = native.owner();
    // Every discovery fixture exists before the query: a healthy paired
    // source, a valid non-source, and a paired source whose installed screen
    // asset is then invalidated on disk.
    let plain = native.fx.install("core-read-plain", &["records.list"]);
    let broken = install(&native, "core-read-broken", OPERATIONS, true).expect("paired install");
    invalidate_installed_screen(&native, &broken.install);

    let sources = native
        .rpc("crm_campaign_work_sources", json!({}))
        .expect("source discovery");
    assert_eq!(sources["contract"], "crm-campaign-work/v1");
    assert_eq!(
        sources["state"], "partial",
        "a source that cannot be inspected makes the inventory partial"
    );
    let rows = sources["sources"].as_array().expect("a source list");
    let row = rows
        .iter()
        .find(|row| row["install_id"] == scope.install)
        .expect("the healthy paired install is a source");
    assert_eq!(row["app"], "crm-cw-core-read");
    assert!(
        row["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|context| context == &json!(scope.context)),
        "the live context is named"
    );
    // The valid non-source was installed before this query: it must be absent
    // from the same response, so a bypassed filter fails this test.
    assert!(
        rows.iter().all(|row| row["install_id"] != plain.install),
        "a package without the operations is not a source"
    );
    // A declared source whose package no longer passes integrity is reported
    // as unavailable, never silently dropped, and carries no values.
    let gaps = sources["unavailable"]
        .as_array()
        .expect("an unavailable list");
    let gap = gaps
        .iter()
        .find(|row| row["install_id"] == broken.install)
        .expect("the invalidated source stays visible");
    assert_eq!(gap["app"], "crm-cw-core-read-broken");
    assert!(
        gap["reason"].as_str().unwrap().contains("integrity"),
        "the gap names the failed inspection: {}",
        gap["reason"]
    );
    assert!(
        gap.get("contexts").is_none(),
        "an uninspectable source reports no contexts"
    );

    // No recorded work: unavailable, never an inferred row.
    let empty = read(&native, &scope.install, &scope.context, CAMPAIGN, None).expect("empty read");
    assert_eq!(empty["state"], "unavailable");
    assert_eq!(empty["revision"], Value::Null);
    assert_eq!(empty["assignment"], Value::Null);
    assert!(empty["unavailable"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "assignee"));

    // Record a row through the screen relay, then read it at the core
    // boundary. The host cannot read the CRM readiness chain, so the read is
    // partial with no decision — never a lifecycle shortcut.
    let action = native.mount(&owner, "public", &scope, 1);
    service(
        &native,
        &owner,
        "public",
        &action,
        "campaign-work.save",
        json!({"campaign_id": CAMPAIGN, "expected_revision": 0, "assignee": "Ada Chen",
            "status": "blocked", "blocked_reason": "waiting on legal"}),
    )
    .expect("screen write");
    let partial =
        read(&native, &scope.install, &scope.context, CAMPAIGN, None).expect("partial read");
    assert_eq!(partial["state"], "partial");
    assert_eq!(partial["next_decision"], Value::Null);
    // The decision and the unrecorded due date are the only gaps.
    assert_eq!(partial["unavailable"], json!(["next_decision", "due_at"]));
    assert_eq!(partial["revision"], 1);
    assert_eq!(partial["assignment"]["assignee"], "Ada Chen");
    assert_eq!(
        partial["identity"],
        format!(
            "crm-cw-core-read:{}:{}:{}",
            scope.install, scope.context, CAMPAIGN
        )
    );

    // A stale expected revision returns the revision tokens only.
    let stale =
        read(&native, &scope.install, &scope.context, CAMPAIGN, Some(0)).expect("stale read");
    assert_eq!(stale["state"], "stale");
    assert_eq!(stale["refusal"]["code"], "stale_revision");
    assert_eq!(stale["revision"], 1);
    for key in [
        "assignment",
        "due_at",
        "lifecycle",
        "content",
        "next_decision",
    ] {
        assert_eq!(stale[key], Value::Null, "{key} must not leak on stale");
    }

    // A source that does not serve the operation is forbidden, and reads no
    // row: the shape carries no values at all.
    let forbidden =
        read(&native, &plain.install, &plain.context, CAMPAIGN, None).expect("forbidden read");
    assert_eq!(forbidden["state"], "forbidden");
    assert_eq!(forbidden["refusal"]["code"], "forbidden");
    for key in [
        "revision",
        "content_revision",
        "assignment",
        "due_at",
        "lifecycle",
        "content",
        "next_decision",
    ] {
        assert_eq!(
            forbidden[key],
            Value::Null,
            "{key} must not leak on forbidden"
        );
    }

    // An invalidated live source is refused the same way: the read re-proves
    // the package declaration inside its fence instead of trusting discovery.
    let invalid = read(&native, &broken.install, &broken.context, CAMPAIGN, None)
        .expect("invalid source read");
    assert_eq!(invalid["state"], "forbidden");
    assert_eq!(invalid["revision"], Value::Null);

    // Real revocation: archive the live context, then the same read is
    // forbidden with no values — not a never-existing id.
    let shown = native
        .rpc(
            "app_context_show",
            json!({"install_id": scope.install, "context_id": scope.context}),
        )
        .expect("context show");
    let revision = shown["context"]["revision"]
        .as_i64()
        .expect("context revision");
    native
        .rpc(
            "app_context_archive",
            json!({"install_id": scope.install, "context_id": scope.context,
                "expected_revision": revision}),
        )
        .expect("context archive");
    let archived = read(&native, &scope.install, &scope.context, CAMPAIGN, None)
        .expect("archived context read");
    assert_eq!(archived["state"], "forbidden");
    for key in [
        "revision",
        "content_revision",
        "assignment",
        "due_at",
        "lifecycle",
    ] {
        assert_eq!(
            archived[key],
            Value::Null,
            "{key} must not leak after archival"
        );
    }

    // The source itself must exist: an unknown installation is a request
    // error, not a source that could be read or labelled forbidden.
    let unknown = native.rpc(
        "crm_campaign_work_read",
        json!({"install_id": "inst-missing", "context_id": "ctx-1", "campaign_id": CAMPAIGN}),
    );
    assert!(unknown.is_err(), "an unknown installation must refuse");
}
