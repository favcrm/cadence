//! CAD-2026 / SOC-55 acceptance, authored from the tickets by the check
//! author; implementers must not edit it.
//!
//! A source-backed social draft is accepted only when the receipt it cites is
//! this installation's own retained `social.source.posts` read, retained in
//! this exact context, and contains the selected post. Everything runs through
//! the real daemon action (`app_social_draft_create` over `Shared::dispatch`,
//! behind the live operator session, approved installation and mounted screen)
//! and the real per-installation record store; the receipts are synthetic
//! fixture rows written the way the source fetch writes them, so no provider
//! call, spend or production data is involved.
//!
//! Contract asserted, each refusal leaving no draft behind:
//! - a valid scoped receipt backs a draft and the draft keeps that exact
//!   receipt and post;
//! - a missing receipt, another installation's receipt, another context's
//!   receipt, a wrong-kind receipt, and a receipt that does not contain the
//!   selected post are refused;
//! - a manual draft stays manual.

use crate::daemon::Shared;
use crate::error::Result;
use crate::store::app_records::RecordStore;
use crate::store::app_tools::{AppToolClaim, AppToolRecord};
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

const HANDLE: &str = "essentialfoods.demo";
const POST: &str = "111";
const OTHER_POST: &str = "222";
const SOURCE_RECEIPT: &str = "toolrcpt-cad2026source0000000000000001";
const OTHER_RECEIPT: &str = "toolrcpt-cad2026source0000000000000002";
const WRONG_KIND_RECEIPT: &str = "toolrcpt-cad2026source0000000000000003";
const FOREIGN_INSTALL_RECEIPT: &str = "toolrcpt-cad2026source0000000000000004";
const FOREIGN_CONTEXT_RECEIPT: &str = "toolrcpt-cad2026source0000000000000005";
const MISSING_RECEIPT: &str = "toolrcpt-cad2026source0000000000000006";

fn pid() -> u32 {
    std::process::id()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The synthetic page the source read retained: one post on one handle.
fn posts() -> Value {
    json!([{
        "id": POST,
        "caption": "Synthetic: fresh bread today",
        "media_kind": "image",
        "permalink": "https://www.instagram.com/p/AbC_1/",
        "preview_url": "https://scontent.cdninstagram.com/a.jpg",
        "published_at": "2026-10-20T01:00:00Z",
        "published_at_unix": 1_792_458_000_i64,
    }])
}

/// The source tool result the real fetch would have kept for `handle`.
fn source_result(kind: &str) -> Value {
    json!({
        "schema": 1,
        "kind": kind,
        "provider": "agenticos_external",
        "source_tool": "read_instagram_posts",
        "profile_verified": true,
        "handle": HANDLE,
        "empty_reason": null,
        "more_available": false,
        "repeated": false,
        "posts": posts(),
        "charge": null,
    })
}

/// One retained tool receipt, exactly as `app_tool_record` keeps it: the
/// pre-call claim first, then the result. `owner` is the installation the
/// receipt names, which is not always the installation that retained it.
fn retain_receipt(shared: &Arc<Shared>, receipt: &str, request: &str, owner: &str, kind: &str) {
    let input = json!({"handle": HANDLE});
    let result = source_result(kind);
    shared
        .store
        .app_tool_claim(AppToolClaim {
            request,
            install: owner,
            alias: "instagram.read",
            slot: "source",
            binding_digest: "sha256:synthetic-source-binding",
            input_digest: "sha256:synthetic-source-input",
            call_id: receipt,
        })
        .unwrap();
    shared
        .store
        .app_tool_record(AppToolRecord {
            id: receipt,
            request,
            install: owner,
            alias: "instagram.read",
            slot: "source",
            binding_digest: "sha256:synthetic-source-binding",
            input_digest: "sha256:synthetic-source-input",
            input: &input,
            result: &result,
            asset: None,
        })
        .unwrap();
}

struct Fixture {
    _root: tempfile::TempDir,
    shared: Arc<Shared>,
    install: String,
    context: String,
    action_token: String,
    token: String,
    key: String,
}

fn operator(shared: &Arc<Shared>, method: &str, params: Value) -> Value {
    scoped(Asserted::Operator, || {
        shared.dispatch(method, &params, pid())
    })
    .unwrap_or_else(|error| panic!("{method} failed: {error}"))
}

/// The real daemon, the real Social Content bundle with its tools screen, an
/// approved installation, an active context, a live operator session and a
/// consumed screen mount — the exact state the app's own draft call runs in.
fn boot() -> Fixture {
    let root = tempfile::Builder::new()
        .prefix("cad2026-source-guard")
        .tempdir()
        .unwrap();
    let pm = root.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(root.path(), &opts).unwrap();

    // The app's own bundle plus the tools screen fixture, named for this app
    // and declaring the draft alias on the app's draft-effect slot.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apps");
    let app = root.path().join("app");
    copy_dir(&fixtures.join("social-content"), &app);
    copy_dir(
        &fixtures.join("ig-tools-fixture/screens"),
        &app.join("screens"),
    );
    let screen = app.join("screens/feed/screens.json");
    let text = std::fs::read_to_string(&screen).unwrap();
    let text = text
        .replace("\"ig-tools-fixture\"", "\"social-content\"")
        .replace(
            "\"instagram.read\": \"source\"",
            "\"social.draft\": \"image\"",
        );
    assert!(text.contains("\"social.draft\": \"image\""), "{text}");
    std::fs::write(&screen, text).unwrap();

    let installed = operator(
        &shared,
        "app_workspace_install",
        json!({"source": app.to_str().unwrap()}),
    );
    let install = installed["install_id"]
        .as_str()
        .expect("installed app id")
        .to_owned();
    let digest = installed["digest"]
        .as_str()
        .expect("installed digest")
        .to_owned();
    operator(
        &shared,
        "app_local_install_approve",
        json!({"install_id": install.clone(), "digest": digest.clone()}),
    );
    let created = operator(
        &shared,
        "app_context_create",
        json!({
            "install_id": install.clone(),
            "label": "EF",
            "input_defaults": {},
            "request_id": "ctx-cad2026",
        }),
    );
    let context = created["context"]["id"]
        .as_str()
        .or_else(|| created["id"].as_str())
        .expect("context id")
        .to_owned();

    crate::operator_auth::ensure_secret(root.path()).unwrap();
    let secret = crate::operator_auth::read_secret(root.path()).unwrap();
    let nonce = operator(
        &shared,
        "operator_link_mint",
        json!({"secret": secret, "origin": "loopback"}),
    )["nonce"]
        .clone();
    let session = operator(
        &shared,
        "operator_session_open",
        json!({"nonce": nonce, "origin": "loopback"}),
    );
    let token = session["token"].as_str().expect("session token").to_owned();
    let key = session["key"].as_str().unwrap_or("").to_owned();

    let mint = operator(
        &shared,
        "app_screen_mint",
        json!({
            "install_id": install.clone(),
            "context_id": context.clone(),
            "tag": "feed",
            "token": token.clone(),
            "key": key.clone(),
            "origin": "loopback",
            "generation": 1,
        }),
    );
    let nonce = mint["mount"]
        .as_str()
        .expect("mount nonce")
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    operator(&shared, "app_screen_consume", json!({"nonce": nonce}));
    let action_token = mint["action_token"]
        .as_str()
        .expect("mounted action token")
        .to_owned();

    // The saved source handle the guard requires for every source draft.
    RecordStore::open(&shared.state_dir, &install)
        .unwrap()
        .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], "src-cad2026")
        .unwrap();

    Fixture {
        _root: root,
        shared,
        install,
        context,
        action_token,
        token,
        key,
    }
}

impl Fixture {
    fn action(&self) -> Value {
        json!({
            "action_token": self.action_token.clone(),
            "tool_alias": "social.draft",
            "token": self.token.clone(),
            "key": self.key.clone(),
            "origin": "loopback",
        })
    }

    fn create(&self, request: &str, caption: &str, source: Value) -> Result<Value> {
        let mut params = self.action();
        params["request_id"] = json!(request);
        params["caption"] = json!(caption);
        params["source"] = source;
        scoped(Asserted::Operator, || {
            self.shared
                .dispatch("app_social_draft_create", &params, pid())
        })
    }

    fn drafts(&self) -> Vec<Value> {
        let listed = operator(&self.shared, "app_social_draft_list", self.action());
        listed["drafts"].as_array().cloned().unwrap_or_default()
    }

    /// A refused create leaves the inventory exactly as it was, never a row
    /// carrying this case's caption, and never echoes the retained post.
    fn refused(&self, request: &str, caption: &str, source: Value) {
        let before = self.drafts().len();
        let error = match self.create(request, caption, source) {
            Ok(draft) => panic!("{caption} was accepted as a source-backed draft: {draft}"),
            Err(error) => error.to_string(),
        };
        assert!(
            !error.contains("Synthetic: fresh bread today") && !error.contains("/p/AbC_1/"),
            "{caption} leaked retained receipt content: {error}"
        );
        let after = self.drafts();
        assert_eq!(after.len(), before, "{caption} left a draft behind");
        assert!(
            !after.iter().any(|row| row["caption"] == json!(caption)),
            "{caption} left a draft behind"
        );
    }

    /// Attach a receipt to the current context, as the fetch path does.
    fn attach(&self, receipt: &str, kind: &str) {
        RecordStore::open(&self.shared.state_dir, &self.install)
            .unwrap()
            .app_social_tool_receipt_attach(&self.context, receipt, 1_000.0, kind)
            .unwrap();
    }
}

#[test]
fn cad2026_valid_scoped_source_receipt_backs_a_draft() {
    let fx = boot();
    retain_receipt(
        &fx.shared,
        SOURCE_RECEIPT,
        "read-cad2026-valid",
        &fx.install,
        "social.source.posts",
    );
    fx.attach(SOURCE_RECEIPT, "social.source.posts");

    let draft = fx
        .create(
            "draft-cad2026-valid",
            "[synthetic] valid source draft",
            json!({"kind": "tool_receipt", "receipt_id": SOURCE_RECEIPT, "post_id": POST}),
        )
        .expect("a valid scoped source receipt must back a draft");
    assert_eq!(
        draft["source"],
        json!({"kind": "tool_receipt", "receipt_id": SOURCE_RECEIPT, "post_id": POST}),
        "the draft must keep the exact receipt and post it cited"
    );
    let listed = fx.drafts();
    assert!(
        listed
            .iter()
            .any(|row| row["draft_id"] == draft["draft_id"] && row["source"] == draft["source"]),
        "the accepted draft must list with its own receipt: {listed:?}"
    );
}

#[test]
fn cad2026_missing_receipt_is_refused() {
    let fx = boot();
    fx.refused(
        "draft-cad2026-missing",
        "[synthetic] missing receipt",
        json!({"kind": "tool_receipt", "receipt_id": MISSING_RECEIPT, "post_id": POST}),
    );
}

#[test]
fn cad2026_receipt_of_another_installation_is_refused() {
    let fx = boot();
    // Retained in this context, but the tool result names another installation.
    retain_receipt(
        &fx.shared,
        FOREIGN_INSTALL_RECEIPT,
        "read-cad2026-foreign-install",
        "install-elsewhere",
        "social.source.posts",
    );
    fx.attach(FOREIGN_INSTALL_RECEIPT, "social.source.posts");
    fx.refused(
        "draft-cad2026-foreign-install",
        "[synthetic] other installation receipt",
        json!({
            "kind": "tool_receipt",
            "receipt_id": FOREIGN_INSTALL_RECEIPT,
            "post_id": POST,
        }),
    );
}

#[test]
fn cad2026_receipt_of_another_context_is_refused() {
    let fx = boot();
    retain_receipt(
        &fx.shared,
        FOREIGN_CONTEXT_RECEIPT,
        "read-cad2026-foreign-context",
        &fx.install,
        "social.source.posts",
    );
    // This installation retained it, but for a different context.
    RecordStore::open(&fx.shared.state_dir, &fx.install)
        .unwrap()
        .app_social_tool_receipt_attach(
            "other-context",
            FOREIGN_CONTEXT_RECEIPT,
            1_000.0,
            "social.source.posts",
        )
        .unwrap();
    fx.refused(
        "draft-cad2026-foreign-context",
        "[synthetic] other context receipt",
        json!({
            "kind": "tool_receipt",
            "receipt_id": FOREIGN_CONTEXT_RECEIPT,
            "post_id": POST,
        }),
    );
}

#[test]
fn cad2026_wrong_kind_receipt_is_refused() {
    let fx = boot();
    // A real retained receipt of this install and context, but not a source read.
    retain_receipt(
        &fx.shared,
        WRONG_KIND_RECEIPT,
        "read-cad2026-wrong-kind",
        &fx.install,
        "text.generate.result",
    );
    fx.attach(WRONG_KIND_RECEIPT, "text.generate.result");
    fx.refused(
        "draft-cad2026-wrong-kind",
        "[synthetic] wrong kind receipt",
        json!({
            "kind": "tool_receipt",
            "receipt_id": WRONG_KIND_RECEIPT,
            "post_id": POST,
        }),
    );
}

#[test]
fn cad2026_receipt_without_the_selected_post_is_refused() {
    let fx = boot();
    retain_receipt(
        &fx.shared,
        OTHER_RECEIPT,
        "read-cad2026-post",
        &fx.install,
        "social.source.posts",
    );
    fx.attach(OTHER_RECEIPT, "social.source.posts");
    // The receipt is a real source read of this context, but it kept only
    // `POST`; naming another post must fail closed.
    fx.refused(
        "draft-cad2026-post",
        "[synthetic] post not in receipt",
        json!({
            "kind": "tool_receipt",
            "receipt_id": OTHER_RECEIPT,
            "post_id": OTHER_POST,
        }),
    );
}

#[test]
fn cad2026_manual_draft_stays_manual() {
    let fx = boot();
    let draft = fx
        .create(
            "draft-cad2026-manual",
            "[synthetic] manual draft",
            json!({"kind": "manual"}),
        )
        .expect("a manual draft needs no receipt");
    assert_eq!(draft["source"], json!({"kind": "manual"}));
    assert!(fx
        .drafts()
        .iter()
        .any(|row| row["source"] == json!({"kind": "manual"})));
}
