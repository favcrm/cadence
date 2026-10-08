//! CAD-1098 acceptance checks, written from the ticket's operator
//! decisions and the design contract (`docs/design/CAD-1098-per-app-
//! threads.md`) by someone other than the implementer. Each test proves
//! one bad case is REFUSED at the real daemon guard (`Shared::dispatch`
//! under the test seam, the board over real HTTP, the real adapters'
//! launch rules). A refusal is asserted by its reason, never by "is_err",
//! and each test also holds a positive control so it cannot pass by
//! refusing everything.
//!
//! Mutation record: every test names the guard it exercises in its doc
//! line; the PR description lists the file:line and the failure text
//! observed with that guard removed.

use std::collections::BTreeMap;

use super::*;
use crate::adapter::fake::FakeAdapter;
use crate::adapter::{AdapterHooks, Identity, ProviderAdapter};
use crate::master;
use crate::store::app_contexts::ContextConfig;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};

/// A fixture daemon: a master (managed Pi, so a turn token exists) and a
/// worker, two REAL installations (CRM and Social Content, installed
/// through the daemon), the CRM with two companies and two
/// campaigns.
struct Gx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    crm: String,
    social: String,
    crm_a: String,
    crm_b: String,
    social_ctx: String,
}

fn pid() -> u32 {
    std::process::id()
}

fn install(shared: &Arc<Shared>, app: &str) -> String {
    let source = format!("{}/workspace-apps/{app}", env!("CARGO_MANIFEST_DIR"));
    let out = scoped(Asserted::Operator, || {
        shared.dispatch("app_workspace_install", &json!({"source": source}), pid())
    })
    .unwrap();
    // Install is consent (CAD-1119): no separate approve step.
    out["install_id"].as_str().unwrap().to_string()
}

fn gx() -> Gx {
    gx_with(true)
}

/// `social_context: false` leaves Social Content installed but with no
/// context at all (an app before its first brand).
fn gx_with(social_context: bool) -> Gx {
    let dir = tempfile::Builder::new().prefix("c98a").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    for (alias, provider, kind) in [("master", "pi", "managed"), ("w1", "fake", "fake")] {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider,
                endpoint_kind: kind,
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }
    // The master's live endpoint generation, so a minted turn token is
    // current under the endpoint's own scheme.
    shared
        .store
        .set_identity_with_quota(
            "master",
            &Identity {
                thread_id: "t".into(),
                session_id: "s".into(),
                model: None,
                effort: None,
                pid: pid(),
                endpoint: None,
                generation: Some("0123456789abcdef0123456789abcdef".into()),
                attach: None,
            },
            None,
        )
        .unwrap();
    let crm = install(&shared, "crm");
    let social = install(&shared, "social-content");
    let ctx = |install: &str, label: &str| {
        let config = ContextConfig::new(label, BTreeMap::new()).unwrap();
        shared
            .store
            .app_context_create(install, &config, &format!("req-{}", label.to_lowercase()))
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let social_ctx = if social_context {
        ctx(&social, "Social")
    } else {
        String::new()
    };
    let (crm_a, crm_b) = (ctx(&crm, "Acme"), ctx(&crm, "Beta"));
    let gx = Gx {
        dir,
        shared,
        crm,
        social,
        crm_a,
        crm_b,
        social_ctx,
    };
    gx.campaign(&gx.crm_a.clone(), "cmp-a");
    gx.campaign(&gx.crm_a.clone(), "cmp-a2");
    gx.campaign(&gx.crm_b.clone(), "cmp-b");
    gx
}

impl Gx {
    fn campaign(&self, context: &str, id: &str) {
        use crate::store::app_content::Draft;
        use crate::store::app_records::RecordStore;
        let draft = Draft::parse(
            "Hello",
            "Pre",
            &[json!({"type": "paragraph", "text": "Body."})],
        )
        .unwrap();
        RecordStore::open(self.dir.path(), &self.crm)
            .unwrap()
            .app_content_save(context, id, None, &draft)
            .unwrap();
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || self.shared.dispatch(method, &params, pid()))
    }

    fn operator(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Operator, method, params)
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(crate::rollout::db_file(self.dir.path())).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// The conversation of an app, made as the operator.
    fn conversation(&self, install: &str, context: &str, extra: Value) -> String {
        let mut params = json!({"alias": "master", "install_id": install,
                                "context_id": context});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        let made = self.operator("conversation_create", params).unwrap();
        made["conversation"]["id"].as_str().unwrap().to_string()
    }

    /// The operator's app-scoped chat message, queued to the master.
    fn send(&self, id: &str, install: &str, context: &str, conversation: Option<&str>) {
        let mut params = json!({"alias": "master", "text": format!("hi {id}"),
            "message": id, "app": {"install_id": install, "context_id": context}});
        if let Some(c) = conversation {
            params["conversation"] = json!(c);
        }
        self.operator("thread_send", params).unwrap();
    }

    fn send_home(&self, id: &str) {
        let params = json!({"alias": "master", "text": format!("hi {id}"), "message": id});
        self.operator("thread_send", params).unwrap();
    }

    /// Make `id` the master's RUNNING turn and return its (current)
    /// turn token.
    fn run(&self, id: &str) -> String {
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS
            .mint("0123456789abcdef0123456789abcdef");
        self.db()
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
                [id],
            )
            .unwrap();
        self.shared.store.mark_running(id, &token).unwrap();
        token
    }

    /// Finish `id` (it was delivered), so it is "the last delivered".
    fn finish(&self, id: &str, started: f64) {
        self.db()
            .execute(
                "UPDATE messages SET state='completed', started=? WHERE id=?",
                rusqlite::params![started, id],
            )
            .unwrap();
    }

    /// A scoped assistant verb as the master's own connection.
    fn verb(&self, method: &str, mut params: Value, message: &str, token: &str) -> Result<Value> {
        params["message"] = json!(message);
        params["token"] = json!(token);
        self.call(Asserted::Agent("master".into()), method, params)
    }
}

fn err_text(r: Result<Value>) -> String {
    r.expect_err("expected a refusal").to_string()
}

impl Gx {
    /// Forge: move a message's enqueue entry (and with it the
    /// conversation `message_conversation` resolves) into `thread`,
    /// leaving its verified send-time stamp untouched.
    fn repoint(&self, message: &str, thread: &str) {
        let n = self
            .db()
            .execute(
                "UPDATE thread_entries SET thread_id=? WHERE message_id=?",
                rusqlite::params![thread, message],
            )
            .unwrap();
        assert!(n >= 1, "no entry for {message}");
    }

    fn general_of(&self, install: &str, context: &str) -> String {
        self.conversation(install, context, json!({"general": true}))
    }
}

/// I3: a turn token redeems only for a message whose OWN conversation
/// belongs to the requested installation. The message's verified stamp
/// names CRM, but its conversation is Social's: the CRM redeem is
/// refused. A home message with the same stamp is refused too.
///
/// Guard: `scoped_chat_assistant`, the I3 `message_conversation` filter.
#[test]
fn i3_redeem_refused_when_conversation_install_differs_from_requested() {
    let gx = gx();
    let list = |gx: &Gx, id: &str, token: &str| {
        gx.verb(
            "app_segment_assistant_list",
            json!({"install_id": gx.crm, "context_id": gx.crm_a}),
            id,
            token,
        )
    };
    // Control: an honest CRM conversation turn redeems.
    gx.send("m-ok", &gx.crm, &gx.crm_a, None);
    let token = gx.run("m-ok");
    assert!(list(&gx, "m-ok", &token).is_ok());
    gx.finish("m-ok", 2.0);
    // Forged: the same kind of turn, its conversation moved to Social's.
    gx.send("m-forged", &gx.crm, &gx.crm_a, None);
    let social_general = gx.general_of(&gx.social, &gx.social_ctx);
    gx.repoint("m-forged", &social_general);
    let token = gx.run("m-forged");
    let err = err_text(list(&gx, "m-forged", &token));
    assert!(
        err.contains("is not in a conversation of this installation"),
        "{err}"
    );
    gx.finish("m-forged", 3.0);
    // Forged: its conversation is the HOME thread.
    gx.send("m-home", &gx.crm, &gx.crm_a, None);
    let home = {
        gx.send_home("m-h0");
        gx.shared
            .store
            .message_conversation("master", "m-h0")
            .unwrap()
            .unwrap()
            .id
    };
    gx.repoint("m-home", &home);
    let token = gx.run("m-home");
    let err = err_text(list(&gx, "m-home", &token));
    assert!(
        err.contains("is not in a conversation of this installation"),
        "{err}"
    );
}

/// A fixture of the real `ProviderAdapter` seam the daemon drives: the
/// fake records the profile of every session it opens.
fn fake() -> (Arc<FakeAdapter>, Arc<dyn ProviderAdapter>) {
    let hooks = AdapterHooks {
        on_event: Box::new(|_, _| {}),
        on_request: Box::new(|_| {}),
    };
    let fake = Arc::new(FakeAdapter::new(hooks));
    let dynamic: Arc<dyn ProviderAdapter> = fake.clone();
    (fake, dynamic)
}

impl Gx {
    fn message(&self, id: &str) -> Message {
        self.shared.store.message(id).unwrap().unwrap()
    }

    fn switch(&self, adapter: &Arc<dyn ProviderAdapter>, id: &str) -> bool {
        let message = self.message(id);
        self.shared
            .switch_session_if_needed("master", adapter, &message)
            .unwrap()
    }

    fn threads(&self) -> i64 {
        self.count("SELECT count(*) FROM threads")
    }

    fn messages(&self) -> i64 {
        self.count("SELECT count(*) FROM messages")
    }
}

/// I1 on the daemon RPC: `conversation_create`, `conversation_list` and
/// the app-bound `thread_send` are the operator's chat. An agent pane, an
/// unproven caller (a detached child derives no identity) and the master
/// itself are refused, and nothing is written.
///
/// Guard: `Shared::operator_chat` (conversations_rpc.rs) and, for the
/// master's own connection, `master_policy`.
#[test]
fn i1_conversation_verbs_refuse_agents_and_unproven_callers() {
    let gx = gx();
    let (threads, messages) = (gx.threads(), gx.messages());
    let create = json!({"alias": "master", "install_id": gx.crm,
                        "context_id": gx.crm_a, "general": true});
    let list = json!({"alias": "master", "install_id": gx.crm});
    let send = json!({"alias": "master", "text": "x", "message": "m-x",
                      "app": {"install_id": gx.crm, "context_id": gx.crm_a}});
    for (verb, params) in [
        ("conversation_create", &create),
        ("conversation_list", &list),
        ("thread_send", &send),
    ] {
        let err = err_text(gx.call(Asserted::Agent("w1".into()), verb, params.clone()));
        assert!(
            err.contains("operator's chat") && err.contains("agent 'w1'"),
            "{verb}: {err}"
        );
        let err = err_text(gx.call(Asserted::Unproven, verb, params.clone()));
        assert!(err.contains("operator"), "{verb} unproven: {err}");
        let err = err_text(gx.call(Asserted::Agent("master".into()), verb, params.clone()));
        assert!(
            err.contains("only the scoped app verbs") || err.contains("master may not call"),
            "{verb} master: {err}"
        );
    }
    assert_eq!((gx.threads(), gx.messages()), (threads, messages));
    // Control: the operator is served.
    assert!(gx.operator("conversation_create", create).is_ok());
    assert!(gx.operator("conversation_list", list).is_ok());
    assert!(gx.operator("thread_send", send).is_ok());
}

/// An unknown installation creates nothing — not General, not a
/// conversation — and a campaign subject must exist in the PROVEN install
/// AND context: another company's campaign, another installation's
/// context and a missing campaign are all refused before any row.
///
/// Guards: `known_install` and `verify_campaign_subject`
/// (conversations_rpc.rs).
#[test]
fn unknown_install_and_unproven_subject_create_nothing() {
    let gx = gx();
    let before = gx.threads();
    let err = err_text(gx.operator(
        "conversation_create",
        json!({"alias": "master", "install_id": "no-such-install",
               "context_id": gx.crm_a, "general": true}),
    ));
    assert!(err.contains("Unknown app installation"), "{err}");
    let err = err_text(gx.operator(
        "conversation_list",
        json!({"alias": "master", "install_id": "no-such-install"}),
    ));
    assert!(err.contains("Unknown app installation"), "{err}");
    for (install, context, subject) in [
        (&gx.crm, &gx.crm_a, "campaign:cmp-nope"),
        (&gx.crm, &gx.crm_a, "campaign:cmp-b"), // another company's
        (&gx.crm, &gx.crm_b, "campaign:cmp-a"), // other direction
        (&gx.crm, &gx.crm_a, "customer:cmp-a"), // not a v1 subject kind
    ] {
        let err = err_text(gx.operator(
            "conversation_create",
            json!({"alias": "master", "install_id": install,
                   "context_id": context, "subject": subject}),
        ));
        assert!(
            err.contains("does not exist in this installation and context")
                || err.contains("subject must be campaign"),
            "{subject}: {err}"
        );
    }
    // Social's context under the CRM installation is not a proof either.
    let err = err_text(gx.operator(
        "conversation_create",
        json!({"alias": "master", "install_id": gx.crm,
               "context_id": gx.social_ctx, "subject": "campaign:cmp-a"}),
    ));
    assert!(!err.is_empty());
    assert_eq!(gx.threads(), before, "a refused call wrote a conversation");
    // Control: the proven campaign makes exactly one.
    let made = gx
        .operator(
            "conversation_create",
            json!({"alias": "master", "install_id": gx.crm,
                   "context_id": gx.crm_a, "subject": "campaign:cmp-a"}),
        )
        .unwrap();
    assert_eq!(made["created"], json!(true));
    assert_eq!(gx.threads(), before + 1);
}

/// `thread_send`'s `conversation` is a selector only: the server resolves
/// the scope. A conversation of another installation, a home thread id, a
/// made-up id, one used with another company's context, a selector with
/// no verified app binding and any client-named scope field are refused,
/// and no message is queued.
///
/// Guards: `verify_conversation_selector` (conversations_rpc.rs), the
/// `conversation needs a verified app binding` arm and the field
/// allowlist in `rpc_thread_send`.
#[test]
fn thread_send_refuses_forged_and_other_install_conversations() {
    let gx = gx();
    let social_general = gx.general_of(&gx.social, &gx.social_ctx);
    let campaign = gx.conversation(&gx.crm, &gx.crm_a, json!({"subject": "campaign:cmp-a"}));
    gx.send_home("m-h0");
    let home = gx
        .shared
        .store
        .message_conversation("master", "m-h0")
        .unwrap()
        .unwrap()
        .id;
    let base = gx.messages();
    let attempt = |conversation: &str, context: &str| {
        gx.operator(
            "thread_send",
            json!({"alias": "master", "text": "x", "message": "m-f",
                   "conversation": conversation,
                   "app": {"install_id": gx.crm, "context_id": context}}),
        )
    };
    for forged in [social_general.as_str(), home.as_str(), "conv-made-up"] {
        let err = err_text(attempt(forged, &gx.crm_a));
        assert!(
            err.contains("Unknown conversation for this app installation"),
            "{forged}: {err}"
        );
    }
    let err = err_text(attempt(&campaign, &gx.crm_b));
    assert!(err.contains("only the context it was created in"), "{err}");
    let err = err_text(gx.operator(
        "thread_send",
        json!({"alias": "master", "text": "x", "message": "m-f", "conversation": campaign}),
    ));
    assert!(err.contains("needs a verified app binding"), "{err}");
    for field in [
        "scope",
        "subject",
        "install_id",
        "thread",
        "thread_id",
        "general",
    ] {
        let mut params = json!({"alias": "master", "text": "x", "message": "m-f",
            "app": {"install_id": gx.crm, "context_id": gx.crm_a}});
        params[field] = json!("x");
        let err = err_text(gx.operator("thread_send", params));
        assert!(
            err.contains(&format!("field '{field}' is not accepted")),
            "{field}: {err}"
        );
    }
    assert_eq!(gx.messages(), base, "a refused send queued a message");
    // Control: its own proven conversation takes the message and the
    // receipt names the conversation it landed in.
    let receipt = gx
        .operator(
            "thread_send",
            json!({"alias": "master", "text": "x", "message": "m-ok",
                   "conversation": campaign,
                   "app": {"install_id": gx.crm, "context_id": gx.crm_a}}),
        )
        .unwrap();
    assert_eq!(receipt["thread"]["id"], json!(campaign));
    assert_eq!(gx.messages(), base + 1);
}

/// I4: delivery drops the App hint AND the turn-token slot when the
/// message's own conversation is not the stamped installation's; the
/// message itself still delivers, unscoped.
///
/// Guard: `Shared::delivery_hint` (daemon.rs), used by `delivery_body`
/// and `turn_slot`.
#[test]
fn i4_delivery_drops_hint_and_token_slot_on_scope_mismatch() {
    let gx = gx();
    let agent = gx.shared.store.agent("master").unwrap();
    gx.send("m-ok", &gx.crm, &gx.crm_a, None);
    let ok = gx.message("m-ok");
    let slot = gx.shared.turn_slot(&agent, &ok).expect("control: a slot");
    let body = gx
        .shared
        .delivery_body("master", "managed", &ok, Some(&slot));
    assert!(
        body.contains(&gx.crm) && body.contains(&slot),
        "control: {body}"
    );
    // The conversation moves to the other installation; the stamp stays.
    gx.send("m-bad", &gx.crm, &gx.crm_a, None);
    let social_general = gx.general_of(&gx.social, &gx.social_ctx);
    gx.repoint("m-bad", &social_general);
    let bad = gx.message("m-bad");
    assert!(gx.shared.delivery_hint(&bad).is_none());
    assert!(gx.shared.turn_slot(&agent, &bad).is_none());
    let body = gx
        .shared
        .delivery_body("master", "managed", &bad, Some("<<cadence-turn-slot:x>>"));
    assert_eq!(
        body, bad.body,
        "the message delivers unscoped, nothing added"
    );
}

const DRAFT: fn(&str, &str, &str, &str) -> Value = |install, context, campaign, proposal| {
    json!({"install_id": install, "context_id": context, "campaign_id": campaign,
           "proposal_id": proposal, "subject": "S", "preheader": "P",
           "blocks": [{"type": "paragraph", "text": "B"}]})
};

/// I5: a turn in campaign A's conversation refuses actions on campaign B
/// of the same company: drafting for B, listing B's proposals and showing
/// B's proposal by id (`app_content_assistant_proposal_show`). A turn in
/// the General conversation (no subject) is not narrowed.
///
/// Guards: `ScopedChat::require_campaign` and the `proposal_show` /
/// `proposals` arms of `rpc_app_assistant_read` (app_content_rpc.rs).
#[test]
fn i5_campaign_conversation_refuses_another_campaigns_actions() {
    let gx = gx();
    let conv_a = gx.conversation(&gx.crm, &gx.crm_a, json!({"subject": "campaign:cmp-a"}));
    let conv_a2 = gx.conversation(&gx.crm, &gx.crm_a, json!({"subject": "campaign:cmp-a2"}));
    let (crm, ctx) = (gx.crm.clone(), gx.crm_a.clone());
    let read = json!({"install_id": crm, "context_id": ctx});
    // A turn in a2's conversation makes a2's proposal.
    gx.send("m-a2", &crm, &ctx, Some(&conv_a2));
    let t = gx.run("m-a2");
    gx.verb(
        "app_content_assistant_draft",
        DRAFT(&crm, &ctx, "cmp-a2", "p-a2"),
        "m-a2",
        &t,
    )
    .unwrap();
    gx.finish("m-a2", 2.0);
    // A turn in a's conversation.
    gx.send("m-a", &crm, &ctx, Some(&conv_a));
    let t = gx.run("m-a");
    let other = "this conversation is about another campaign";
    let err = err_text(gx.verb(
        "app_content_assistant_draft",
        DRAFT(&crm, &ctx, "cmp-a2", "p-x"),
        "m-a",
        &t,
    ));
    assert!(err.contains(other), "draft: {err}");
    let mut named = read.clone();
    named["campaign_id"] = json!("cmp-a2");
    let err = err_text(gx.verb("app_content_assistant_proposals", named, "m-a", &t));
    assert!(err.contains(other), "proposals: {err}");
    let mut show = read.clone();
    show["proposal_id"] = json!("p-a2");
    let err = err_text(gx.verb(
        "app_content_assistant_proposal_show",
        show.clone(),
        "m-a",
        &t,
    ));
    assert!(err.contains(other), "proposal_show: {err}");
    // An unnamed list is its own campaign's only.
    let own_list = gx
        .verb("app_content_assistant_proposals", read.clone(), "m-a", &t)
        .unwrap();
    assert!(!own_list.to_string().contains("p-a2"), "{own_list}");
    // Control: its own campaign works end to end.
    gx.verb(
        "app_content_assistant_draft",
        DRAFT(&crm, &ctx, "cmp-a", "p-a"),
        "m-a",
        &t,
    )
    .unwrap();
    let mut own = read.clone();
    own["proposal_id"] = json!("p-a");
    assert!(gx
        .verb("app_content_assistant_proposal_show", own, "m-a", &t)
        .is_ok());
    gx.finish("m-a", 3.0);
    // Control: the General conversation is not narrowed to one campaign.
    gx.send("m-g", &crm, &ctx, None);
    let t = gx.run("m-g");
    assert!(gx
        .verb("app_content_assistant_proposal_show", show, "m-g", &t)
        .is_ok());
}

/// Gate 1, live: ONE actor goes Home -> app -> Home (and app -> another
/// app). Every switch closes the session and opens a new one, and each
/// session opens with the profile of ITS message's stored conversation,
/// so the App profile never carries into the next Home session. The
/// profile maps to the real rule sets (Claude `--allowedTools`, Pi guard
/// rules) in the adapter tests.
///
/// Guards: `switch_session_if_needed` (conversations_rpc.rs): the profile
/// choice and `adapter.set_session_profile`.
#[test]
fn gate1_live_actor_home_app_home_opens_each_session_with_its_profile() {
    use master::Profile::{App, Home};
    let gx = gx();
    let (fake, adapter) = fake();
    gx.send_home("h1");
    assert!(!gx.switch(&adapter, "h1"), "home -> home keeps the session");
    gx.finish("h1", 10.0);
    gx.send("a1", &gx.crm, &gx.crm_a, None);
    assert!(gx.switch(&adapter, "a1"), "home -> app opens a new session");
    assert_eq!(fake.opened_with(), vec![App]);
    gx.finish("a1", 11.0);
    gx.send("a2", &gx.crm, &gx.crm_a, None);
    assert!(
        !gx.switch(&adapter, "a2"),
        "same conversation keeps its session"
    );
    gx.finish("a2", 12.0);
    gx.send("s1", &gx.social, &gx.social_ctx, None);
    assert!(
        gx.switch(&adapter, "s1"),
        "another app's conversation is another session"
    );
    gx.finish("s1", 13.0);
    gx.send_home("h2");
    assert!(gx.switch(&adapter, "h2"), "app -> home opens a new session");
    assert_eq!(fake.opened_with(), vec![App, App, Home]);
    assert_eq!(
        adapter.session_profile(),
        Home,
        "the app profile carried over"
    );
    // What each profile is: the app one holds only scoped `cadence app`
    // verbs, the home one the full master set.
    let app = master::allowed_tools(App);
    // CAD-1168: `attachment read` rides both profiles.
    assert!(
        !app.is_empty()
            && app
                .iter()
                .all(|t| t.starts_with("Bash(cadence app ")
                    || *t == "Bash(cadence attachment read *)"),
        "{app:?}"
    );
    assert_eq!(master::allowed_tools(Home), master::CLAUDE_ALLOWED_TOOLS);
}

/// I11: the master's tmp dir (spilled output, Pi's read tool) is not
/// shared across a conversation switch. It survives a turn in the same
/// conversation and is empty after the switch.
///
/// Guard: `master::clear_tmp` call in `switch_session_if_needed`.
#[test]
fn i11_master_tmp_is_emptied_on_a_conversation_switch() {
    let gx = gx();
    let (_, adapter) = fake();
    let tmp = master::tmpdir(gx.dir.path());
    gx.send("a1", &gx.crm, &gx.crm_a, None);
    assert!(gx.switch(&adapter, "a1"));
    gx.finish("a1", 10.0);
    std::fs::create_dir_all(tmp.join("sub")).unwrap();
    std::fs::write(tmp.join("spill.txt"), "app A secret").unwrap();
    std::fs::write(tmp.join("sub/deep.txt"), "x").unwrap();
    gx.send("a2", &gx.crm, &gx.crm_a, None);
    assert!(!gx.switch(&adapter, "a2"));
    assert!(
        tmp.join("spill.txt").exists(),
        "control: same conversation keeps its files"
    );
    gx.finish("a2", 11.0);
    gx.send("s1", &gx.social, &gx.social_ctx, None);
    assert!(gx.switch(&adapter, "s1"));
    let left: Vec<_> = std::fs::read_dir(&tmp).unwrap().flatten().collect();
    assert!(left.is_empty(), "the next conversation sees {left:?}");
}

/// Every method name in `Shared::dispatch_method`'s match, parsed from
/// the source so a method added later is in the table.
fn dispatch_methods() -> Vec<String> {
    let src = include_str!("../daemon.rs");
    let start = src
        .find("    fn dispatch_method(\n")
        .expect("dispatch_method");
    let body = &src[start..];
    let body = &body[body.find("match method {").expect("match")..];
    let body = &body[..body
        .find("other => Err(Error::rejected(format!(\"Unknown method")
        .expect("end")];
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim_start();
        if line.len() - t.len() != 12 || !t.starts_with('"') {
            continue;
        }
        let Some((arms, _)) = t.split_once("=>") else {
            continue;
        };
        for arm in arms.split('|') {
            let name = arm.trim().trim_matches('"');
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// The ONLY daemon methods a master turn in an app conversation may call,
/// written out from the ticket's answer 1 (scoped app verbs plus
/// read-only status for its own turn): the twelve scoped verbs,
/// `message_report` and `thread_read` of its own conversation.
const APP_TURN_MAY_CALL: &[&str] = &[
    "app_assistant_actions",
    "app_assistant_invoke",
    "app_assistant_operation_show",
    "app_content_assistant_draft",
    "app_content_assistant_proposal_show",
    "app_content_assistant_proposals",
    "app_record_csv_assistant_import",
    "app_record_csv_assistant_preview",
    "app_segment_assistant_list",
    "app_segment_assistant_preview",
    "app_segment_assistant_save",
    "app_segment_assistant_show",
    // CAD-1168: the turn reads the retained file its message carried.
    "chat_file_read",
    "message_report",
    "thread_read",
];

impl Gx {
    fn policy(&self, method: &str, params: &Value) -> Result<()> {
        scoped(Asserted::Agent("master".into()), || {
            self.shared.master_policy(method, params, pid())
        })
    }

    /// The conversation id a message lives in.
    fn conversation_of(&self, message: &str) -> String {
        self.shared
            .store
            .message_conversation("master", message)
            .unwrap()
            .unwrap()
            .id
    }
}

/// Gate 2: while the master's running message is in an APP conversation,
/// `master_policy` is an allowlist over EVERY dispatch method: exactly the
/// fourteen in `APP_TURN_MAY_CALL` pass, so the admin RPCs, the permission
/// verbs (`master_ask_permission`, `master_peek_grant`,
/// `master_permission_use`), `wiki_write`, `agent_*`, `job_*` and
/// `monitor_*` are refused. A running message that cannot be resolved
/// fails closed to the same app allowlist, and a home turn keeps the
/// master's full set (the narrowing is by conversation, not global).
///
/// Guard: the `scope != MasterScope::Home` arm of
/// `Shared::master_policy` (master_rpc.rs) and `master_turn_scope`.
#[test]
fn gate2_app_conversation_turn_is_an_allowlist_over_every_method() {
    let gx = gx();
    let table = dispatch_methods();
    assert!(table.len() > 100, "method table parse: {}", table.len());
    for family in ["agent_", "job_", "monitor_", "wiki_", "master_permission_"] {
        assert!(
            table.iter().any(|m| m.starts_with(family)),
            "no {family}* method parsed"
        );
    }
    for named in [
        "master_ask_permission",
        "master_peek_grant",
        "master_permission_use",
        "wiki_write",
        "health",
    ] {
        assert!(
            table.contains(&named.to_string()),
            "{named} not in the table"
        );
    }
    // Unresolvable: no running message at all.
    let open_set = |gx: &Gx, own_message: &str, own_conversation: &str| -> Vec<String> {
        table
            .iter()
            .filter(|m| {
                let params = match m.as_str() {
                    "message_report" => json!({"message": own_message}),
                    "thread_read" => json!({"conversation": own_conversation}),
                    _ => json!({}),
                };
                gx.policy(m, &params).is_ok()
            })
            .cloned()
            .collect()
    };
    gx.send("m-q", &gx.crm, &gx.crm_a, None); // queued, not running
    let unresolved = open_set(&gx, "m-q", "none");
    let mut expected: Vec<String> = APP_TURN_MAY_CALL.iter().map(|m| m.to_string()).collect();
    expected.retain(|m| m != "thread_read"); // no conversation to read
    expected.sort();
    let mut got = unresolved.clone();
    got.sort();
    assert_eq!(
        got, expected,
        "no running message must fail closed to the app set"
    );
    // A running app turn.
    let conv = gx.conversation_of("m-q");
    gx.run("m-q");
    let mut got = open_set(&gx, "m-q", &conv);
    got.sort();
    let mut expected: Vec<String> = APP_TURN_MAY_CALL.iter().map(|m| m.to_string()).collect();
    expected.sort();
    assert_eq!(got, expected, "the exact set an app turn may call");
    // The refusals name the reason.
    for refused in [
        "master_ask_permission",
        "master_peek_grant",
        "master_permission_use",
        "wiki_write",
        "health",
        "agent_send",
        "job_new",
        "monitor_ls",
    ] {
        if !table.contains(&refused.to_string()) {
            continue;
        }
        let err = err_text(gx.policy(refused, &json!({})).map(|()| json!(null)));
        assert!(
            err.contains("an app conversation's turn may call only"),
            "{refused}: {err}"
        );
    }
    gx.finish("m-q", 5.0);
    // Control: a HOME turn keeps the master's own set.
    gx.send_home("m-h");
    gx.run("m-h");
    for allowed in [
        "wiki_write",
        "master_ask_permission",
        "master_permission_use",
    ] {
        assert!(
            gx.policy(allowed, &json!({})).is_ok(),
            "home turn: {allowed}"
        );
    }
}

/// Gate 2, permission path: a STANDING always-allow rule the operator
/// granted never lets an app turn run a command. The same
/// `master_permission_use` that applies the rule in the home thread is
/// refused in an app conversation and runs nothing.
///
/// Guard: the app allowlist in `Shared::master_policy`, which stops
/// `rpc_master_permission_use` (and with it `run_approved`).
#[test]
fn gate2_standing_always_allow_rule_does_not_run_from_an_app_turn() {
    let gx = gx();
    // A cadence verb (the only kind a standing rule can cover). If the
    // guard failed, `run_approved` would re-exec the test binary with
    // these args: an unmatched libtest filter and `--list` run nothing.
    let cwd = gx.dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = std::fs::canonicalize(&cwd).unwrap();
    let cwd = cwd.to_str().unwrap().to_string();
    let argv = json!(["cadence", "zz-no-such-verb", "--list"]);
    let ran = |gx: &Gx| gx.count("SELECT count(*) FROM events WHERE kind='permission_used'");
    // A standing rule, made the real way: the master asks in the home
    // thread, the operator answers "always".
    gx.send_home("h1");
    gx.run("h1");
    let ask = gx
        .call(
            Asserted::Agent("master".into()),
            "master_ask_permission",
            json!({"argv": argv, "cwd": cwd, "reason": "r"}),
        )
        .unwrap();
    gx.operator(
        "master_permission_always",
        json!({"id": ask["id"], "scope": "exact"}),
    )
    .unwrap();
    let peeked = gx
        .call(
            Asserted::Agent("master".into()),
            "master_peek_grant",
            json!({"argv": argv, "cwd": cwd}),
        )
        .unwrap();
    assert_eq!(
        peeked["allowed"],
        json!(true),
        "control: the standing rule covers it at home"
    );
    gx.finish("h1", 5.0);
    // The same call from an app conversation's turn.
    gx.send("a1", &gx.crm, &gx.crm_a, None);
    gx.run("a1");
    for verb in [
        "master_permission_use",
        "master_peek_grant",
        "master_ask_permission",
    ] {
        let err = err_text(gx.call(
            Asserted::Agent("master".into()),
            verb,
            json!({"argv": argv, "cwd": cwd, "reason": "r"}),
        ));
        assert!(
            err.contains("an app conversation's turn may call only"),
            "{verb}: {err}"
        );
    }
    assert_eq!(
        ran(&gx),
        0,
        "the standing rule ran a command from an app turn"
    );
}

/// `thread_read` from an app turn is limited to the app's OWN
/// conversation: its own id is served; the home thread (no selector),
/// another conversation of the same app and another installation's
/// conversation are refused.
///
/// Guard: the `thread_read` arm of `Shared::master_policy`.
#[test]
fn thread_read_is_limited_to_the_apps_own_conversation() {
    let gx = gx();
    let own = gx.conversation(&gx.crm, &gx.crm_a, json!({"general": true}));
    let sibling = gx.conversation(&gx.crm, &gx.crm_a, json!({}));
    let social = gx.general_of(&gx.social, &gx.social_ctx);
    gx.send("a1", &gx.crm, &gx.crm_a, Some(&own));
    gx.run("a1");
    let read = |conversation: Option<&str>| {
        let mut params = json!({"alias": "master", "after": 0});
        if let Some(c) = conversation {
            params["conversation"] = json!(c);
        }
        gx.call(Asserted::Agent("master".into()), "thread_read", params)
    };
    let page = read(Some(&own)).unwrap();
    assert_eq!(
        page["entries"][0]["text"],
        json!("hi a1"),
        "control: its own thread"
    );
    for (what, c) in [
        ("home", None),
        ("sibling", Some(sibling.as_str())),
        ("social", Some(social.as_str())),
    ] {
        let err = err_text(read(c));
        assert!(
            err.contains("reads only its own conversation"),
            "{what}: {err}"
        );
    }
}

/// The board's real HTTP surface over a real daemon, under the test seam
/// (the board forwards the asserted identity to the daemon it relays to).
mod http {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::test_seam::Seam;

    struct Stop(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            for thread in self.1.drain(..).rev() {
                let _ = thread.join();
            }
        }
    }

    pub(super) struct Board {
        pub(super) agent: ureq::Agent,
        pub(super) base: String,
        host: String,
        token: String,
        cookie: String,
        key: String,
        _stop: Stop,
    }

    /// A reply: status and the JSON body.
    pub(super) type Reply = (u16, Value);

    impl Board {
        pub(super) fn decorate<B>(
            &self,
            who: &str,
            b: ureq::RequestBuilder<B>,
        ) -> ureq::RequestBuilder<B> {
            let b = b.header("Host", &self.host).header("X-Cadence-Board", "1");
            let b = b.header("Origin", format!("http://{}", self.host));
            let b = b.header(crate::test_seam::AS_HEADER, who);
            let b = b.header(crate::test_seam::TOKEN_HEADER, &self.token);
            if who == "operator" {
                b.header("Cookie", &self.cookie)
                    .header("X-Cadence-Session", &self.key)
            } else {
                b
            }
        }

        /// `who`: `operator` (with the board session) or `agent:<alias>`.
        pub(super) fn call(
            &self,
            who: &str,
            method: &str,
            path: &str,
            body: Option<Value>,
        ) -> Reply {
            let url = format!("{}{path}", self.base);
            let mut response = match (method, body) {
                ("GET", _) => self.decorate(who, self.agent.get(&url)).call(),
                (_, body) => self
                    .decorate(who, self.agent.post(&url))
                    .header("Content-Type", "application/json")
                    .send(body.unwrap_or(Value::Null).to_string()),
            }
            .unwrap();
            let status = response.status().as_u16();
            let body = response.body_mut().read_json().unwrap_or(Value::Null);
            (status, body)
        }
    }

    /// Start a daemon over `gx`'s state (its installs, contexts and
    /// agents) and a board on 3110-3199 with an operator session.
    pub(super) fn start(gx: Gx) -> (Board, std::path::PathBuf, Gx2) {
        let Gx {
            dir,
            shared,
            crm,
            social,
            crm_a,
            crm_b,
            social_ctx,
        } = gx;
        drop(shared);
        let state = dir.path().to_path_buf();
        let pm = state.join("pm");
        let mut stop = Stop(Arc::new(AtomicBool::new(false)), Vec::new());
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.0.clone()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_state = state.clone();
        stop.1.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(std::time::Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut port = 3110 + (std::process::id() % 80) as u16;
        loop {
            let (startup, ready) = std::sync::mpsc::channel();
            let board = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.0.clone()),
                startup: Some(startup),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (state.clone(), pm.clone());
            let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
            match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
                Ok(()) => break stop.1.push(thread),
                Err(_) if port < 3199 => port += 1,
                Err(kind) => panic!("board could not bind: {kind:?}"),
            }
            thread.join().unwrap();
        }
        let token = Seam::token_at(&state).unwrap();
        let host = format!("cadence-{port}.localhost:{port}");
        crate::operator_auth::ensure_secret(&state).unwrap();
        let secret = crate::operator_auth::read_secret(&state).unwrap();
        let nonce = scoped(Asserted::Operator, || {
            crate::client::rpc(
                &state,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap()["nonce"]
            .clone();
        let config = ureq::Agent::config_builder().http_status_as_error(false);
        let agent: ureq::Agent = config.build().into();
        let base = format!("http://127.0.0.1:{port}");
        let session = agent
            .post(format!("{base}/api/session"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &token)
            .header("Content-Type", "application/json")
            .send(json!({"nonce": nonce}).to_string())
            .unwrap();
        let set = session.headers()["set-cookie"].to_str().unwrap();
        let cookie = set[..set.find(';').unwrap()].to_owned();
        let key: Value = session.into_body().read_json().unwrap();
        let board = Board {
            agent,
            base,
            host,
            token,
            cookie,
            key: key["session_key"].as_str().unwrap().to_string(),
            _stop: stop,
        };
        let rest = Gx2 {
            crm,
            social,
            crm_a,
            _crm_b: crm_b,
            _social_ctx: social_ctx,
            _dir: dir,
        };
        (board, state, rest)
    }

    impl Board {
        /// The `Cache-Control` of a GET as `who` (None when absent).
        pub(super) fn cache_control(&self, who: &str, path: &str) -> Option<String> {
            let url = format!("{}{path}", self.base);
            let response = self.decorate(who, self.agent.get(&url)).call().unwrap();
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        }
    }

    pub(super) struct Gx2 {
        pub crm: String,
        pub social: String,
        pub crm_a: String,
        pub _crm_b: String,
        pub _social_ctx: String,
        pub _dir: tempfile::TempDir,
    }
}

/// I1 on the board HTTP peer: the SAME operator proof as the daemon RPC.
/// An agent-asserted request to the conversation routes (both POSTs, the
/// installation GET list and a message POST naming a conversation) is
/// refused by the board as an operator-only decision (`check:
/// operator_only`, 403) and writes nothing, while the operator's session
/// is served on the same routes. The removed `GET
/// /api/threads/<alias>/conversations` must stay a 404.
///
/// Guards: `WRITE_ROUTES` (ui/operator.rs) + `operator::admit` for the
/// POSTs; `admit_operator_read` (ui/serve.rs) for the installation GET.
#[test]
fn i1_board_http_refuses_agents_on_every_conversation_route_like_the_rpc() {
    let (board, state, rest) = http::start(gx_with(false));
    let db = || rusqlite::Connection::open(crate::rollout::db_file(&state)).unwrap();
    let count = |sql: &str| -> i64 { db().query_row(sql, [], |r| r.get(0)).unwrap() };
    let (threads, messages) = (
        count("SELECT count(*) FROM threads"),
        count("SELECT count(*) FROM messages"),
    );
    let crm = &rest.crm;
    let agent = "agent:w1";
    let create = json!({"context_id": rest.crm_a, "general": true});
    let create_t = json!({"install_id": crm, "context_id": rest.crm_a, "general": true});
    let routes: Vec<(&str, String, Option<Value>)> = vec![
        (
            "POST",
            format!("/api/app-installations/{crm}/conversations"),
            Some(create.clone()),
        ),
        (
            "POST",
            "/api/threads/master/conversations".into(),
            Some(create_t.clone()),
        ),
        (
            "GET",
            format!("/api/app-installations/{crm}/conversations"),
            None,
        ),
        (
            "POST",
            "/api/threads/master/messages".into(),
            Some(json!({"text": "x", "message": "m-http",
                        "app": {"install_id": crm, "context_id": rest.crm_a},
                        "conversation": "whatever"})),
        ),
    ];
    for (method, path, body) in &routes {
        let (status, reply) = board.call(agent, method, path, body.clone());
        assert_eq!(status, 403, "{method} {path}: {reply}");
        assert_eq!(
            reply["check"],
            json!("operator_only"),
            "{method} {path}: {reply}"
        );
    }
    assert_eq!(
        (
            count("SELECT count(*) FROM threads"),
            count("SELECT count(*) FROM messages")
        ),
        (threads, messages),
        "a refused agent request wrote"
    );
    // Control: the operator's session is served on the list and create routes.
    let (status, reply) = board.call(
        "operator",
        "POST",
        &format!("/api/app-installations/{crm}/conversations"),
        Some(create),
    );
    assert_eq!(status, 200, "{reply}");
    let (status, reply) = board.call(
        "operator",
        "GET",
        &format!("/api/app-installations/{crm}/conversations"),
        None,
    );
    assert_eq!(status, 200, "{reply}");
    // A create with NO context_id is still the operator's alone: an agent
    // is refused at the board, the operator is served for a known install.
    let bare = format!("/api/app-installations/{}/conversations", rest.social);
    let (status, reply) = board.call(agent, "POST", &bare, Some(json!({"general": true})));
    assert_eq!(status, 403, "{reply}");
    assert_eq!(reply["check"], json!("operator_only"), "{reply}");
    let (status, reply) = board.call("operator", "POST", &bare, Some(json!({"general": true})));
    assert_eq!(status, 200, "{reply}");
    // The ungated `GET /api/threads/<alias>/conversations` was removed
    // (it relayed the operator-only list with no board gate): it must stay
    // absent for an agent and for the operator, so it cannot silently
    // return ungated.
    for who in [agent, "operator"] {
        let (status, reply) = board.call(
            who,
            "GET",
            &format!("/api/threads/master/conversations?install={crm}"),
            None,
        );
        assert_eq!(status, 404, "{who}: {reply}");
        assert_eq!(
            reply["error"],
            json!("no such thread route"),
            "{who}: {reply}"
        );
    }
}

impl Gx {
    /// The operator's message bound to an installation only (no context).
    fn send_install_only(&self, id: &str, app: Value, conversation: Option<&str>) -> Result<Value> {
        let mut params =
            json!({"alias": "master", "text": format!("hi {id}"), "message": id, "app": app});
        if let Some(c) = conversation {
            params["conversation"] = json!(c);
        }
        self.operator("thread_send", params)
    }
}

/// An installation-only binding `app:{install_id}` (no context) is proven
/// by the catalog or a context row, never by the client: an unknown or
/// forged install id is refused for `thread_send`, `conversation_create`
/// and `conversation_list`, and nothing is created. Positive control: a
/// KNOWN install with no context at all (Social Content before a brand)
/// lands in its own General.
///
/// Guards: `Shared::known_install` (conversations_rpc.rs, the catalog
/// fallback) and the install-only proof in `rpc_thread_send`
/// (messages_rpc.rs).
#[test]
fn install_only_binding_needs_a_known_install_and_creates_nothing_otherwise() {
    let gx = gx_with(false);
    assert!(gx.social_ctx.is_empty(), "fixture: Social has no context");
    let (threads, messages) = (gx.threads(), gx.messages());
    for forged in ["no-such-install", "install-1", &"a".repeat(64)] {
        let err = err_text(gx.send_install_only("m-f", json!({"install_id": forged}), None));
        assert!(err.contains("Unknown app installation"), "{forged}: {err}");
        let err = err_text(gx.operator(
            "conversation_create",
            json!({"alias": "master", "install_id": forged, "general": true}),
        ));
        assert!(err.contains("Unknown app installation"), "{forged}: {err}");
        let err = err_text(gx.operator(
            "conversation_list",
            json!({"alias": "master", "install_id": forged}),
        ));
        assert!(err.contains("Unknown app installation"), "{forged}: {err}");
    }
    assert_eq!((gx.threads(), gx.messages()), (threads, messages));
    // Control: the known no-context install lands in ITS General.
    let receipt = gx
        .send_install_only("m-ok", json!({"install_id": gx.social}), None)
        .unwrap();
    assert_eq!(receipt["thread"]["general"], json!(true), "{receipt}");
    assert_eq!(receipt["thread"]["install_id"], json!(gx.social));
    assert_eq!(gx.messages(), messages + 1);
    let general = gx.general_of_install_only(&gx.social);
    assert_eq!(receipt["thread"]["id"], json!(general));
}

impl Gx {
    fn general_of_install_only(&self, install: &str) -> String {
        let made = self
            .operator(
                "conversation_create",
                json!({"alias": "master", "install_id": install, "general": true}),
            )
            .unwrap();
        assert_eq!(made["created"], json!(false), "General already existed");
        made["conversation"]["id"].as_str().unwrap().to_string()
    }
}

/// An installation-only message gets NO delivery hint and NO turn-token
/// slot (the hint needs a proven context), so its prompt is the bare body
/// and its turn token cannot redeem a scoped CRM verb: I3/I4 still hold.
///
/// Guard: `Store::message_app` (store/threads.rs) requires a stamped
/// context; `delivery_hint`/`turn_slot` build on it.
#[test]
fn install_only_message_gets_no_hint_no_slot_and_cannot_redeem() {
    let gx = gx();
    let agent = gx.shared.store.agent("master").unwrap();
    gx.send_install_only(
        "m-ctx",
        json!({"install_id": gx.crm, "context_id": gx.crm_a}),
        None,
    )
    .unwrap();
    assert!(
        gx.shared.turn_slot(&agent, &gx.message("m-ctx")).is_some(),
        "control"
    );
    gx.finish("m-ctx", 2.0);
    gx.send_install_only("m-bare", json!({"install_id": gx.crm}), None)
        .unwrap();
    let bare = gx.message("m-bare");
    assert!(gx.shared.delivery_hint(&bare).is_none());
    assert!(gx.shared.turn_slot(&agent, &bare).is_none());
    let body = gx
        .shared
        .delivery_body("master", "managed", &bare, Some("<<slot>>"));
    assert_eq!(
        body, bare.body,
        "nothing is added to an install-only message"
    );
    // Its (genuinely current) token cannot redeem any scoped verb, even
    // naming the CRM installation and a real context.
    let token = gx.run("m-bare");
    let err = err_text(gx.verb(
        "app_segment_assistant_list",
        json!({"install_id": gx.crm, "context_id": gx.crm_a}),
        "m-bare",
        &token,
    ));
    assert!(err.contains("carries no verified App scope"), "{err}");
}

/// A campaign conversation selector or campaign subject needs a PROVEN
/// context: an installation-only binding naming a campaign conversation,
/// and a `conversation_create` with a campaign subject but no context, are
/// refused and write nothing.
///
/// Guards: the context check in `verify_conversation_selector` and the
/// store's enqueue check (two layers), and the `needs the context` arm of
/// `rpc_conversation_create`.
#[test]
fn campaign_conversation_without_a_proven_context_is_refused() {
    let gx = gx();
    let campaign = gx.conversation(&gx.crm, &gx.crm_a, json!({"subject": "campaign:cmp-a"}));
    let (threads, messages) = (gx.threads(), gx.messages());
    let err = err_text(gx.send_install_only("m-c", json!({"install_id": gx.crm}), Some(&campaign)));
    assert!(err.contains("only the context it was created in"), "{err}");
    let err = err_text(gx.operator(
        "conversation_create",
        json!({"alias": "master", "install_id": gx.crm, "subject": "campaign:cmp-a"}),
    ));
    assert!(err.contains("needs the context it belongs to"), "{err}");
    assert_eq!((gx.threads(), gx.messages()), (threads, messages));
    // Control: with its own proven context the same selector works.
    let ok = gx
        .send_install_only(
            "m-ok",
            json!({"install_id": gx.crm, "context_id": gx.crm_a}),
            Some(&campaign),
        )
        .unwrap();
    assert_eq!(ok["thread"]["id"], json!(campaign));
}

/// A forged extra key in an installation-only binding is refused like it
/// is in a full one: the client names the installation and nothing else.
///
/// Guard: the key allowlist in `thread_app` (daemon.rs).
#[test]
fn install_only_binding_refuses_forged_extra_keys() {
    let gx = gx();
    let messages = gx.messages();
    for key in [
        "verified",
        "conversation",
        "subject",
        "scope",
        "context_revision",
        "context_digest",
    ] {
        let mut app = json!({"install_id": gx.crm});
        app[key] = json!("x");
        let err = err_text(gx.send_install_only("m-k", app, None));
        assert!(
            err.contains(&format!("field '{key}' is not accepted")),
            "{key}: {err}"
        );
    }
    assert_eq!(gx.messages(), messages);
}

const ENV_CHILD: &str = "CAD1098_ENV_CHILD";

/// Runs inside the child the test below spawns: a daemon fixture in a
/// process whose environment CLAIMS a conversation scope. A normal run of
/// the suite does nothing here.
#[test]
fn conversation_env_child() {
    let Ok(claim) = std::env::var(ENV_CHILD) else {
        return;
    };
    assert_eq!(std::env::var("CADENCE_CONVERSATION").unwrap(), claim);
    let gx = gx();
    if claim == "app" {
        // The environment says "app", the running message is HOME's: the
        // master keeps its home powers.
        gx.send_home("m-h");
        gx.run("m-h");
        for method in [
            "wiki_write",
            "master_ask_permission",
            "master_permission_use",
        ] {
            assert!(
                gx.policy(method, &json!({})).is_ok(),
                "env=app narrowed a home turn: {method}"
            );
        }
    } else {
        // The environment says "home", the running message is an APP's:
        // the master is still narrowed.
        gx.send("m-a", &gx.crm, &gx.crm_a, None);
        gx.run("m-a");
        for method in [
            "wiki_write",
            "master_ask_permission",
            "master_permission_use",
        ] {
            let err = err_text(gx.policy(method, &json!({})).map(|()| json!(null)));
            assert!(
                err.contains("an app conversation's turn may call only"),
                "{method}: {err}"
            );
        }
    }
}

/// CAD-1098 I6: `CADENCE_CONVERSATION` is a CLI belt, never authority. The
/// daemon takes the scope from the running message's stored conversation
/// only: a process whose environment claims `app` while the turn is home
/// is not narrowed, and one claiming `home` while the turn is an app's is
/// not widened.
///
/// Guard: `master_turn_scope` reads the store, never the environment
/// (master_rpc.rs).
#[test]
fn the_daemon_never_reads_the_conversation_env() {
    for claim in ["app", "home"] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon::conversations_acceptance::conversation_env_child",
            ])
            .args(["--nocapture", "--test-threads", "1"])
            .env("CADENCE_CONVERSATION", claim)
            .env(ENV_CHILD, claim)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "claim {claim}: {text}");
        assert!(text.contains("1 passed"), "child did not run: {text}");
    }
}

/// CAD-1110: the app-chat descriptor read, at the real daemon guard and on
/// the board over real HTTP. The descriptor is served only to the operator,
/// only from the consented (installed) digest, install id from the path only, and every
/// "no descriptor" case is one 404 that names nothing. The agent caller is
/// refused by the same proof as the sibling app reads (relay parity, I13),
/// and refused BEFORE existence is decided, so the route is no oracle for
/// installation ids.
///
/// Guards: `Shared::operator_connection` in `rpc_app_chat_descriptor`
/// (daemon/app_chat_rpc.rs), `admit_operator_read` before
/// `app_chat::handle` (ui/serve.rs), the closed param set, and the
/// consent check (`app_capability_status == approved`) at the live digest.
#[test]
fn chat_descriptor_is_operator_only_pinned_to_the_installed_digest_and_one_404() {
    let gx = gx_with(true);
    let read =
        |who: Asserted, id: &str| gx.call(who, "app_chat_descriptor", json!({"install_id": id}));
    // Control: the operator reads the CRM's own descriptor and Social's.
    let crm = read(Asserted::Operator, &gx.crm).unwrap();
    assert_eq!(crm["app"], json!("crm"), "{crm}");
    assert_eq!(crm["descriptor"]["contract"], json!("app-chat/v1"));
    assert!(
        crm["digest"].as_str().unwrap().starts_with("sha256:"),
        "{crm}"
    );
    let social = read(Asserted::Operator, &gx.social).unwrap();
    assert_eq!(social["app"], json!("social-content"), "{social}");
    // An agent, another agent and an unproven peer get the sibling reads'
    // refusal at the daemon; nothing is read.
    for who in [
        Asserted::Agent("master".into()),
        Asserted::Agent("w1".into()),
        Asserted::Unproven,
    ] {
        let err = err_text(read(who, &gx.crm));
        // The master's own turn is stopped earlier still, by the app-turn
        // verb allowlist; everyone else by the operator proof.
        assert!(
            err.contains("operator action")
                || err.contains("not provably the operator")
                || err.contains("may call only the scoped app verbs"),
            "{err}"
        );
    }
    // The install id is the only input: any other field refuses.
    for extra in ["token", "digest", "path", "app"] {
        let err = err_text(gx.operator(
            "app_chat_descriptor",
            json!({"install_id": gx.crm, extra: "x"}),
        ));
        assert!(err.contains("admits only an install_id"), "{extra}: {err}");
    }
    // A forged or unknown install id is "no descriptor", not an error
    // that names anything.
    for forged in ["no-such-install", "install-1", &"a".repeat(64)] {
        let none = read(Asserted::Operator, forged).unwrap();
        assert_eq!(none, json!({"found": false}), "{forged}");
    }
    // Pinned: once the consent is withdrawn at the live digest, the bundle
    // on disk is no longer served.
    let digest = crm["digest"].clone();
    gx.operator(
        "app_local_install_revoke",
        json!({"install_id": gx.crm, "digest": digest}),
    )
    .unwrap();
    assert_eq!(
        read(Asserted::Operator, &gx.crm).unwrap(),
        json!({"found": false}),
        "an installation whose consent is withdrawn serves no descriptor"
    );
    assert!(read(Asserted::Operator, &gx.social).unwrap()["descriptor"].is_object());
}

/// CAD-1110 relay parity: the board's `GET .../chat-descriptor` refuses an
/// agent exactly like the daemon verb and like the sibling reads
/// (`check: operator_only`, 403) even for a forged id, 404s a forged id and
/// an installation whose consent is withdrawn for the operator with one
/// body, and serves the consented descriptor `no-store`.
///
/// Guard: `operator::admit_operator_read` ahead of `app_chat::handle`
/// (ui/serve.rs); `app_chat::route` takes the id from the path only.
#[test]
fn chat_descriptor_board_route_refuses_agents_and_404s_every_missing_case() {
    let (board, _state, rest) = http::start(gx_with(true));
    let crm_path = format!("/api/app-installations/{}/chat-descriptor", rest.crm);
    let forged = "/api/app-installations/no-such-install/chat-descriptor";
    for path in [crm_path.as_str(), forged] {
        let (status, reply) = board.call("agent:w1", "GET", path, None);
        assert_eq!(status, 403, "{path}: {reply}");
        assert_eq!(reply["check"], json!("operator_only"), "{path}: {reply}");
    }
    let (status, reply) = board.call("operator", "GET", &crm_path, None);
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["app"], json!("crm"), "{reply}");
    assert!(reply["descriptor"]["contexts"].is_array(), "{reply}");
    assert_eq!(
        board.cache_control("operator", &crm_path).as_deref(),
        Some("no-store")
    );
    let (status, none) = board.call("operator", "GET", forged, None);
    assert_eq!(status, 404, "{none}");
    assert_eq!(none["error"], json!("no chat descriptor"));
    // The id is never taken from anywhere but the path.
    let (status, _) = board.call(
        "operator",
        "GET",
        &format!("{forged}?install_id={}", rest.crm),
        None,
    );
    assert_eq!(status, 404);
    // Another HTTP method is not a read.
    let (status, _) = board.call("operator", "POST", &crm_path, Some(json!({})));
    assert_ne!(status, 200);
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// CAD-1110: the install validator refuses a package whose `app-chat.json`
/// breaks the grammar, at install time, by the grammar's own reason; and a
/// symlinked descriptor is refused by the confined resolver. Positive
/// control: the untouched package installs.
///
/// Guards: `app_chat::validate` called from `app::validate_contents`
/// (issue/app.rs) and the `snapshot` allowlist (app_catalog/workspace.rs).
#[test]
fn install_refuses_a_package_with_an_invalid_chat_descriptor() {
    let dir = tempfile::Builder::new().prefix("c10i").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let operator = |method: &str, params: Value| {
        scoped(Asserted::Operator, || {
            shared.dispatch(method, &params, pid())
        })
    };
    let root = tempfile::Builder::new().prefix("c1110").tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("workspace-apps/crm");
    let good: Value =
        serde_json::from_str(&std::fs::read_to_string(source.join("app-chat.json")).unwrap())
            .unwrap();
    let install = |name: &str, descriptor: Option<String>, link: bool| {
        let dir = root.path().join(name);
        copy_tree(&source, &dir);
        let file = dir.join("app-chat.json");
        if link {
            std::fs::remove_file(&file).unwrap();
            std::os::unix::fs::symlink("/etc/hostname", &file).unwrap();
        } else if let Some(text) = descriptor {
            std::fs::write(&file, text).unwrap();
        }
        operator(
            "app_workspace_install",
            json!({"source": dir.to_str().unwrap()}),
        )
    };
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut d = good.clone();
        edit(&mut d);
        Some(d.to_string())
    };
    let cases: Vec<(&str, Option<String>, &str)> = vec![
        (
            "forbidden",
            with(&|d| d["contexts"][0]["install_id"] = json!("x")),
            "forbidden descriptor key",
        ),
        (
            "run",
            with(&|d| {
                d["directives"][0]["card"]["buttons"] =
                    json!([{"label": "x", "run": "delete-everything", "view": "a"}])
            }),
            "not a host action",
        ),
        (
            "attach",
            with(&|d| d["attachments"][0]["id"] = json!("shell")),
            "not a host capability",
        ),
        (
            "app",
            with(&|d| d["app"] = json!("social-content")),
            "different app",
        ),
        (
            "tag",
            with(&|d| d["contract"] = json!("app-chat/v2")),
            "expected app-chat/v1",
        ),
        (
            "big",
            Some(format!(
                "{{\"contract\":\"app-chat/v1\",\"app\":\"crm\",\"pad\":\"{}\"}}",
                "x".repeat(17_000)
            )),
            "exceeds",
        ),
    ];
    for (name, text, reason) in cases {
        let err = err_text(install(name, text, false));
        assert!(
            err.contains("app-chat.json") && err.contains(reason),
            "{name}: {err}"
        );
    }
    let err = err_text(install("link", None, true));
    assert!(!err.is_empty(), "a symlinked descriptor must be refused");
    let ok = install("ok", None, false).unwrap();
    assert!(ok["install_id"].is_string(), "control: {ok}");
}

/// CAD-1111: a third app, shipped as a package alone, installs through the
/// same validator, is consented by its install like any app, serves its own descriptor
/// through the generic route, and its screen package passes the CAD-1006
/// integrity proof the mount runs: no host code names it.
///
/// Guards: `app_chat::validate` at install, `app_screen_pkg::extract` (the
/// mount's own re-proof) over the live bundle, the consented-digest pin.
#[test]
fn fixture_third_app_installs_and_serves_its_own_chat_descriptor_and_screen() {
    let dir = tempfile::Builder::new().prefix("c11f").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let source = format!(
        "{}/tests/fixtures/apps/notes-fixture",
        env!("CARGO_MANIFEST_DIR")
    );
    let operator = |method: &str, params: Value| {
        scoped(Asserted::Operator, || {
            shared.dispatch(method, &params, pid())
        })
    };
    let out = operator("app_workspace_install", json!({"source": source})).unwrap();
    let (id, digest) = (out["install_id"].clone(), out["digest"].clone());
    // Install is consent (CAD-1119): the descriptor serves at once, with no
    // separate approve step.
    let served = operator("app_chat_descriptor", json!({"install_id": id})).unwrap();
    assert_eq!(served["app"], json!("notes-fixture"), "{served}");
    assert_eq!(served["digest"], digest);
    assert_eq!(
        served["descriptor"]["directives"][1]["render"],
        json!("screen:post-preview")
    );
    let pm = shared.pm_at(&shared.pm_dir().unwrap()).unwrap();
    let pkg = crate::issue::app_catalog::workspace::with_runtime_snapshot(
        &pm,
        id.as_str().unwrap(),
        |_, files| crate::issue::app_screen_pkg::extract(files, "post-preview"),
    )
    .unwrap();
    assert_eq!(pkg.app, "notes-fixture");
}

/// CAD-1123 HP1 image channel: the board reads a generated image through
/// `GET /api/app-capability-results/<id>/asset`, relayed to the daemon's
/// `app_run_capability_asset` behind the same operator-read gate as the
/// other app-run reads. An agent caller and a request with no operator
/// session are refused at the board; the operator's session reaches the
/// daemon (an unknown receipt is the daemon's refusal, not a missing route).
#[test]
fn cad1123_board_capability_asset_route_is_an_operator_read() {
    let (board, _state, _rest) = http::start(gx_with(false));
    let path = "/api/app-capability-results/receipt-none/asset";
    let (status, reply) = board.call("agent:w1", "GET", path, None);
    assert!(
        matches!(status, 401 | 403),
        "agent read the asset route: {status} {reply}"
    );
    let (status, reply) = board.call("anonymous", "GET", path, None);
    assert!(
        matches!(status, 401 | 403),
        "sessionless read of the asset route: {status} {reply}"
    );
    let (status, reply) = board.call("operator", "GET", path, None);
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(
        status == 400 && error.contains("capability asset"),
        "operator did not reach app_run_capability_asset: {status} {reply}"
    );
}

// ---------- CAD-1168 slice 2: retained chat attachments ----------

/// Upload a real staged file as the operator; returns the minted row.
fn chat_upload(gx: &Gx, name: &str, bytes: &[u8]) -> Value {
    let dir = gx.dir.path().join(crate::wiki::UPLOAD_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, bytes).unwrap();
    gx.operator(
        "chat_file_upload",
        json!({"name": name, "tmp": tmp.to_string_lossy()}),
    )
    .unwrap()
}

/// CAD-1168: `chat_file_upload` is the operator's alone — an agent caller
/// is refused by `operator_chat` before the file is even named (the same
/// proof `thread_send` runs), and so is an underivable one; a caller-
/// supplied `scope`, `sha256` or path field is refused whole. A good
/// staged text file then lands as the control.
///
/// Guards: `rpc_chat_file_upload`'s `operator_chat` proof and the
/// `reject_chat_file_fields` allowlist (daemon/chat_files_rpc.rs).
#[test]
fn chat_file_upload_is_operator_only_and_field_strict() {
    let gx = gx();
    let staged = |bytes: &[u8]| {
        let dir = gx.dir.path().join(crate::wiki::UPLOAD_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&tmp, bytes).unwrap();
        tmp.to_string_lossy().into_owned()
    };
    // An agent caller is refused — the refusal names it.
    let err = err_text(gx.call(
        Asserted::Agent("w1".into()),
        "chat_file_upload",
        json!({"name": "n.txt", "tmp": staged(b"hi")}),
    ));
    assert!(
        err.contains("operator's chat") && err.contains("agent 'w1'"),
        "{err}"
    );
    // An underivable caller is refused too.
    let err = err_text(gx.call(
        Asserted::Unproven,
        "chat_file_upload",
        json!({"name": "n.txt", "tmp": staged(b"hi")}),
    ));
    assert!(err.contains("operator"), "{err}");
    // Identity/authority-shaped fields are refused, never silently dropped.
    for field in ["scope", "sha256", "path", "wiki_as", "uploader", "by"] {
        let mut params = json!({"name": "n.txt", "tmp": staged(b"hi")});
        params[field] = json!("x");
        let err = err_text(gx.operator("chat_file_upload", params));
        assert!(
            err.contains(&format!("field '{field}' is not accepted")) || err.contains("identity"),
            "{field}: {err}"
        );
    }
    // Control: the operator's own upload lands and names the stored row.
    let row = chat_upload(&gx, "notes.txt", b"hello");
    assert!(row["id"].as_str().unwrap().starts_with("chf-"), "{row}");
    assert_eq!(row["mime"], json!("text/plain"));
    assert_eq!(row["scope"], json!("home"));
    assert_eq!(row["uploader"], json!("operator"));
    // An agent cannot read it either — a worker caller is refused.
    let id = row["id"].as_str().unwrap();
    let err = err_text(gx.call(
        Asserted::Agent("w1".into()),
        "chat_file_read",
        json!({"id": id, "message": "m-x", "token": "t"}),
    ));
    assert!(err.contains("only the master"), "{err}");
    // And the operator reads the bounded text back.
    let read = gx.operator("chat_file_read", json!({"id": id})).unwrap();
    assert_eq!(read["text"], json!("hello"));
    assert_eq!(read["extractable"], json!(true));
}

/// CAD-1168: the board's `POST /api/chat/upload` is OperatorOnly like
/// the messages POST — an agent-attributed caller and a sessionless
/// request are refused at the board (`check: operator_only`), while an
/// oversized multipart body is refused before any daemon call.
///
/// Guards: `WRITE_ROUTES` + `operator::admit` (ui/operator.rs) and the
/// route's `read_body` cap (ui/threads.rs).
#[test]
fn board_chat_upload_is_operator_only_and_capped() {
    let (board, state, _rest) = http::start(gx_with(false));
    let db = || rusqlite::Connection::open(crate::rollout::db_file(&state)).unwrap();
    let rows = || -> i64 {
        db().query_row("SELECT count(*) FROM chat_files", [], |r| r.get(0))
            .unwrap_or(0)
    };
    let before = rows();
    // Agent-attributed and sessionless requests are refused alike.
    let boundary = "----t";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"n.txt\"\r\n\r\nhi\r\n--{boundary}--\r\n"
    );
    for (who, check) in [
        ("agent:w1", "operator_only"),
        ("unproven", "caller_identity"),
    ] {
        let url = format!("{}/api/chat/upload", board.base);
        let resp = board
            .decorate(who, board.agent.post(&url))
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send(body.clone().into_bytes())
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{who}: {:?}", resp.status());
        let v: Value = resp.into_body().read_json().unwrap_or(Value::Null);
        assert_eq!(v["check"], json!(check), "{who}: {v}");
    }
    // An oversized body (cap 10 MiB + envelope) is refused at the
    // board's own read — the daemon is never called.
    let big = vec![b'x'; (10 * 1024 * 1024 + 200 * 1024) as usize];
    let mut oversized = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.txt\"\r\n\r\n"
    )
    .into_bytes();
    oversized.extend_from_slice(&big);
    oversized.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let url = format!("{}/api/chat/upload", board.base);
    let resp = board
        .decorate("operator", board.agent.post(&url))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send(oversized)
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413, "oversize was not refused");
    assert_eq!(rows(), before, "a refused upload wrote a row");

    // Control: a real operator upload lands and the row is minted.
    let url = format!("{}/api/chat/upload", board.base);
    let resp = board
        .decorate("operator", board.agent.post(&url))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send(body.into_bytes())
        .unwrap();
    let status = resp.status().as_u16();
    let v: Value = resp.into_body().read_json().unwrap();
    assert_eq!(status, 200, "{v}");
    assert!(v["id"].as_str().unwrap().starts_with("chf-"), "{v}");
    assert_eq!(rows(), before + 1);
}
/// CAD-1168: a refused or forged attachment never reaches a queued
/// message — an off-allowlist or mismatched upload is refused, an
/// unknown or grammar-bad id refuses the send, and a retry naming
/// different attachments is the conflict it always was. A good
/// attachment then lands on the entry's payload and rides the
/// delivery envelope — control for the whole path.
///
/// Guards: `chat_file_put`'s kind/cap checks (store/chat_files.rs),
/// `thread_attachments` (daemon.rs) and `entry_attachments_in`'s
/// retry comparison (store/threads.rs).
#[test]
fn thread_send_attachments_resolve_refuse_and_ride() {
    let gx = gx();
    // A .pdf is refused by the current explicit unsupported-type
    // check before any kind sniff — the interim allowlist is text
    // only, and a magic prefix is not processing.
    let dir = gx.dir.path().join(crate::wiki::UPLOAD_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join("upload-fakepdf");
    std::fs::write(&tmp, b"plain text").unwrap();
    let err = err_text(gx.operator(
        "chat_file_upload",
        json!({"name": "fake.pdf", "tmp": tmp.to_string_lossy()}),
    ));
    assert!(err.contains("not available yet"), "{err}");
    // Off-allowlist extension refuses before sniffing.
    let tmp2 = dir.join("upload-exe");
    std::fs::write(&tmp2, b"MZ").unwrap();
    let err = err_text(gx.operator(
        "chat_file_upload",
        json!({"name": "a.exe", "tmp": tmp2.to_string_lossy()}),
    ));
    assert!(err.contains("not an attachable type"), "{err}");

    // A real upload, referenced on a send.
    let file = chat_upload(&gx, "brief.csv", b"name,email\nA,a@b.c");
    let id = file["id"].as_str().unwrap().to_string();
    let sent = gx
        .operator(
            "thread_send",
            json!({"alias": "master", "text": "review this", "message": "m-att",
                   "attachments": [{"id": id}]}),
        )
        .unwrap();
    assert_eq!(sent["state"], json!("queued"), "{sent}");
    let entry_payload = |gx: &Gx, id: &str| -> Value {
        let conn = gx.db();
        conn.query_row(
            "SELECT payload FROM thread_entries WHERE message_id=?",
            [id],
            |r| r.get::<_, String>(0),
        )
        .map(|p| serde_json::from_str(&p).unwrap())
        .unwrap()
    };
    let payload = entry_payload(&gx, "m-att");
    assert_eq!(
        payload["attachments"][0]["id"],
        json!(id),
        "the stored entry carries the resolved row: {payload}"
    );
    assert_eq!(payload["attachments"][0]["name"], json!("brief.csv"));
    assert_eq!(payload["attachments"][0]["mime"], json!("text/csv"));

    // The same message id with different attachments is the conflict
    // it always was — a retry cannot quietly re-scope a message.
    let other = chat_upload(&gx, "other.txt", b"second");
    let err = err_text(gx.operator(
        "thread_send",
        json!({"alias": "master", "text": "review this", "message": "m-att",
               "attachments": [{"id": other["id"]}]}),
    ));
    assert!(err.contains("already used with different content"), "{err}");

    // Forged/unknown ids refuse; the grammar refuses a non-chf id too.
    for bad in ["chf-0000000000000000000000000000dead", "not-an-id", ""] {
        let err = err_text(gx.operator(
            "thread_send",
            json!({"alias": "master", "text": "x", "message": "m-bad",
                   "attachments": [{"id": bad}]}),
        ));
        assert!(
            err.contains("attachment") || err.contains("unknown"),
            "{bad}: {err}"
        );
    }
    // A non-id field inside an attachment refuses whole.
    let err = err_text(gx.operator(
        "thread_send",
        json!({"alias": "master", "text": "x", "message": "m-bad2",
               "attachments": [{"id": id, "path": "/etc/passwd"}]}),
    ));
    assert!(err.contains("takes id only"), "{err}");

    // The delivery envelope names the file and its read verb — the
    // stored text is untouched.
    let message = gx.message("m-att");
    let body = gx.shared.delivery_body("master", "managed", &message, None);
    assert!(body.contains("brief.csv"), "{body}");
    assert!(body.contains("text/csv"), "{body}");
    assert!(
        body.contains(&format!("cadence attachment read {id}")),
        "{body}"
    );
    assert!(body.ends_with("review this"), "{body}");
}

// ---------- CAD-1168 independent acceptance check (reviewer-authored) ----------

/// CAD-1168 acceptance item 9, written by the Spec/security reviewer
/// (cc-rev-spec) from the ticket, not by the implementer — the
/// implementer must not edit or weaken it.
///
/// Control: a same-scope ready CSV uploaded into a CRM conversation is
/// referenced by a real app-bound `thread_send`, and the master's live
/// turn reads its bounded text back. Refused, each by its own reason
/// and without bytes or an effect (no new message, no new row):
/// cross-installation, cross-company (context) and cross-conversation
/// references; a home row on an app send and an app row on a home
/// send; an unknown id; a forged row that claims ready text without
/// custody bytes; a forged PDF row over real text bytes; a client
/// `ready`/`sha256` field; a master read of an id not on its turn's
/// envelope, under a finished turn, from a worker or an underivable
/// caller; and an agent (the master itself, a worker, a detached child)
/// trying the operator's upload or attach authority. The same cases are
/// then run on the board's HTTP peer.
///
/// Guards: `rpc_chat_file_upload` / `rpc_chat_file_read`
/// (daemon/chat_files_rpc.rs), `thread_attachments` (daemon.rs),
/// `ChatFile::scope_matches` / `home_scope` / readiness checks
/// (store/chat_files.rs), `WRITE_ROUTES` + `post_upload` /
/// `post_message` (ui/operator.rs, ui/threads.rs).
#[test]
fn cad1168_independent_attachment_scope_refusals() {
    const SECRETLESS: &str = "name,email\nZed Sentinel,zed@example.test\n";
    let gx = gx();
    let staged = |gx: &Gx, bytes: &[u8]| {
        let dir = gx.dir.path().join(crate::wiki::UPLOAD_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&tmp, bytes).unwrap();
        tmp.to_string_lossy().into_owned()
    };
    let upload = |gx: &Gx, name: &str, bytes: &[u8], scope: Option<(&str, &str, &str)>| {
        let mut params = json!({"name": name, "tmp": staged(gx, bytes)});
        if let Some((install, context, conversation)) = scope {
            params["app"] = json!({"install_id": install, "context_id": context});
            params["conversation"] = json!(conversation);
        }
        gx.operator("chat_file_upload", params).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let send = |gx: &Gx, msg: &str, scope: Option<(&str, &str, &str)>, ids: Value| {
        let mut params = json!({"alias": "master", "text": format!("read {msg}"),
                                "message": msg, "attachments": ids});
        if let Some((install, context, conversation)) = scope {
            params["app"] = json!({"install_id": install, "context_id": context});
            params["conversation"] = json!(conversation);
        }
        gx.operator("thread_send", params)
    };
    let (crm, social) = (gx.crm.clone(), gx.social.clone());
    let (crm_a, crm_b, social_ctx) = (gx.crm_a.clone(), gx.crm_b.clone(), gx.social_ctx.clone());
    let conv_a = gx.conversation(&crm, &crm_a, json!({"general": true}));
    let conv_a2 = gx.conversation(&crm, &crm_a, json!({}));
    let conv_b = gx.conversation(&crm, &crm_b, json!({"general": true}));
    let conv_s = gx.general_of(&social, &social_ctx);
    let scope_a = Some((crm.as_str(), crm_a.as_str(), conv_a.as_str()));

    // ---- Control: same-scope ready file, real send, guarded read. ----
    let file_a = upload(&gx, "leads.csv", SECRETLESS.as_bytes(), scope_a);
    let sent = send(&gx, "m-ctl", scope_a, json!([{"id": file_a}])).unwrap();
    assert_eq!(sent["state"], json!("queued"), "{sent}");
    let token = gx.run("m-ctl");
    let read = gx
        .verb("chat_file_read", json!({"id": file_a}), "m-ctl", &token)
        .unwrap();
    assert_eq!(read["text"], json!(SECRETLESS), "control read: {read}");
    assert_eq!(read["extractable"], json!(true), "{read}");

    // Fixture rows in other scopes — same bytes, so only scope differs.
    let file_a2 = upload(&gx, "other.txt", b"second file in A", scope_a);
    let file_b = upload(
        &gx,
        "leads.csv",
        SECRETLESS.as_bytes(),
        Some((&crm, &crm_b, &conv_b)),
    );
    let file_s = upload(
        &gx,
        "leads.csv",
        SECRETLESS.as_bytes(),
        Some((&social, &social_ctx, &conv_s)),
    );
    let file_conv2 = upload(
        &gx,
        "leads.csv",
        SECRETLESS.as_bytes(),
        Some((&crm, &crm_a, &conv_a2)),
    );
    let file_home = upload(&gx, "leads.csv", SECRETLESS.as_bytes(), None);
    for id in [&file_b, &file_s, &file_conv2, &file_home] {
        assert_ne!(id, &file_a, "a different scope must mint a different row");
    }

    let rows = || gx.count("SELECT count(*) FROM chat_files");
    let messages = || gx.count("SELECT count(*) FROM messages");
    let no_bytes = |what: &str, err: &str| {
        assert!(
            !err.contains("Zed Sentinel") && !err.contains("second file"),
            "{what}: a refusal exposed bytes: {err}"
        );
    };

    // ---- Forged rows: ready claimed without bytes; PDF over text. ----
    let forged_ready = "chf-00000000000000000000000000000f0e";
    let forged_pdf = "chf-00000000000000000000000000000f0f";
    let sha_a: String = gx
        .db()
        .query_row("SELECT sha256 FROM chat_files WHERE id=?", [&file_a], |r| {
            r.get(0)
        })
        .unwrap();
    // Real custody bytes that no scope-A row names yet (uploaded under
    // Home), so a forged scope-A PDF row can point at a present blob.
    let file_carrier = upload(&gx, "carrier.txt", b"carrier bytes", None);
    let sha_carrier: String = gx
        .db()
        .query_row(
            "SELECT sha256 FROM chat_files WHERE id=?",
            [&file_carrier],
            |r| r.get(0),
        )
        .unwrap();
    let scope_label = format!("app:{crm}@{conv_a}");
    for (id, sha, mime) in [
        (forged_ready, "ab".repeat(32), "text/csv"),
        (forged_pdf, sha_carrier, "application/pdf"),
    ] {
        gx.db()
            .execute(
                "INSERT INTO chat_files(id,sha256,size,mime,name,scope,context_id,uploader,created)
                 VALUES(?,?,?,?,?,?,?,'operator',1.0)",
                rusqlite::params![
                    id,
                    sha,
                    if mime == "application/pdf" {
                        13
                    } else {
                        SECRETLESS.len() as i64
                    },
                    mime,
                    "forged.csv",
                    scope_label,
                    crm_a
                ],
            )
            .unwrap();
    }

    let (rows0, messages0) = (rows(), messages());

    // ---- Daemon: refused references at send. ----
    let not_scoped = "is not scoped to this installation, context and conversation";
    /// (what, app scope or Home, attachments, refusal reason)
    type Scope<'a> = Option<(&'a str, &'a str, &'a str)>;
    let cases: Vec<(&str, Scope, Value, &str)> = vec![
        // cross-installation: a Social row on a CRM conversation
        (
            "cross-install",
            scope_a,
            json!([{"id": file_s}]),
            not_scoped,
        ),
        // cross-company: company A's row on company B's conversation
        (
            "cross-company",
            Some((&crm, &crm_b, &conv_b)),
            json!([{"id": file_a}]),
            not_scoped,
        ),
        // another conversation of the same installation and company
        (
            "cross-conversation",
            Some((&crm, &crm_a, &conv_a2)),
            json!([{"id": file_a}]),
            not_scoped,
        ),
        // a home row cannot ride an app conversation
        (
            "home-on-app",
            scope_a,
            json!([{"id": file_home}]),
            not_scoped,
        ),
        // an app row cannot ride the home thread
        (
            "app-on-home",
            None,
            json!([{"id": file_a}]),
            "is not a home-scope file",
        ),
        // never minted
        (
            "unknown",
            scope_a,
            json!([{"id": "chf-0123456789abcdef0123456789abcdef"}]),
            "unknown attachment",
        ),
        // a row claiming ready text with no custody bytes
        (
            "forged-ready-row",
            scope_a,
            json!([{"id": forged_ready}]),
            "blob is missing",
        ),
        // a non-text row over genuine bytes is never ready text
        (
            "forged-pdf-row",
            scope_a,
            json!([{"id": forged_pdf}]),
            "is not a ready text source",
        ),
        // client-forged readiness or digest fields refuse whole
        (
            "client-ready-field",
            scope_a,
            json!([{"id": file_a, "ready": true}]),
            "takes id only",
        ),
        (
            "client-sha-field",
            scope_a,
            json!([{"id": file_a, "sha256": sha_a}]),
            "takes id only",
        ),
    ];
    for (i, (what, scope, ids, reason)) in cases.into_iter().enumerate() {
        let err = err_text(send(&gx, &format!("m-bad-{i}"), scope, ids));
        assert!(err.contains(reason), "{what}: {err}");
        no_bytes(what, &err);
        assert_eq!(messages(), messages0, "{what}: a refused send queued");
    }
    // A forged ready state on upload refuses whole too.
    for field in ["ready", "state", "scope", "context_id"] {
        let mut params = json!({"name": "x.csv", "tmp": staged(&gx, b"a,b\n1,2\n")});
        params[field] = json!("ready");
        let err = err_text(gx.operator("chat_file_upload", params));
        assert!(
            err.contains(&format!("field '{field}' is not accepted")),
            "{field}: {err}"
        );
    }

    // ---- Daemon: refused reads. ----
    // An id not on THIS turn's envelope — even one in the same scope,
    // and even the forged rows — is refused under a live token.
    for id in [&file_a2, &file_b, &file_s, &file_home, &file_conv2] {
        let err = err_text(gx.verb("chat_file_read", json!({"id": id}), "m-ctl", &token));
        assert!(err.contains("not on your turn's envelope"), "{id}: {err}");
        no_bytes("off-envelope read", &err);
    }
    // A worker, an underivable caller and a forged token are refused.
    let err = err_text(gx.call(
        Asserted::Agent("w1".into()),
        "chat_file_read",
        json!({"id": file_a, "message": "m-ctl", "token": token}),
    ));
    assert!(err.contains("only the master"), "{err}");
    no_bytes("worker read", &err);
    let err = err_text(gx.call(Asserted::Unproven, "chat_file_read", json!({"id": file_a})));
    assert!(err.contains("not provably the operator"), "{err}");
    let err = err_text(gx.verb(
        "chat_file_read",
        json!({"id": file_a}),
        "m-ctl",
        "forged-token",
    ));
    assert!(err.contains("active assigned turn"), "{err}");
    no_bytes("forged-token read", &err);
    // Once the turn finishes, the same token no longer reads.
    gx.finish("m-ctl", 5.0);
    let err = err_text(gx.verb("chat_file_read", json!({"id": file_a}), "m-ctl", &token));
    assert!(err.contains("active assigned turn"), "{err}");
    no_bytes("finished-turn read", &err);

    // ---- Daemon: no agent acquires operator upload/attach authority. ----
    for who in [
        Asserted::Agent("master".into()),
        Asserted::Agent("w1".into()),
        Asserted::Unproven,
    ] {
        let label = format!("{who:?}");
        let err = err_text(gx.call(
            who.clone(),
            "chat_file_upload",
            json!({"name": "x.csv", "tmp": staged(&gx, b"a,b\n1,2\n")}),
        ));
        // The master is stopped earlier, by its own call policy (an app
        // turn may call only the scoped verbs) — still a refusal naming
        // the verb; workers and detached children by the operator proof.
        let refused = |err: &str, verb: &str| {
            err.contains("operator's chat")
                || err.contains("not provably the operator")
                || err.contains(&format!("not {verb}"))
        };
        assert!(refused(&err, "chat_file_upload"), "{label} upload: {err}");
        let err = err_text(gx.call(
            who,
            "thread_send",
            json!({"alias": "master", "text": "x", "message": "m-agent",
                   "attachments": [{"id": file_home}]}),
        ));
        assert!(refused(&err, "thread_send"), "{label} attach: {err}");
    }
    // A worker's plain `agent_send` cannot carry attachments either.
    let err = err_text(gx.call(
        Asserted::Agent("w1".into()),
        "agent_send",
        json!({"alias": "master", "text": "x", "attachments": [{"id": file_home}]}),
    ));
    assert!(
        err.contains("attachments") || err.contains("not accepted"),
        "agent_send: {err}"
    );
    assert_eq!(rows(), rows0, "a refused call minted a chat_files row");
    assert_eq!(messages(), messages0, "a refused call queued a message");
    drop(gx);

    // ---- The board's HTTP peer: the same control and refusals. ----
    let (board, state, rest) = http::start(gx_with(true));
    let db = || rusqlite::Connection::open(crate::rollout::db_file(&state)).unwrap();
    let count = |sql: &str| -> i64 { db().query_row(sql, [], |r| r.get(0)).unwrap() };
    let crm = rest.crm.clone();
    let make_conv = |context: &str| -> String {
        let (status, reply) = board.call(
            "operator",
            "POST",
            &format!("/api/app-installations/{crm}/conversations"),
            Some(json!({"context_id": context, "general": true})),
        );
        assert_eq!(status, 200, "{reply}");
        reply["conversation"]["id"].as_str().unwrap().to_string()
    };
    let conv_a = make_conv(&rest.crm_a);
    let conv_b = make_conv(&rest._crm_b);
    let boundary = "----cad1168rev";
    let multipart = |fields: &[(&str, &str)], filename: &str, bytes: &[u8]| -> Vec<u8> {
        let mut body = Vec::new();
        for (k, v) in fields {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
                )
                .as_bytes(),
            );
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    };
    let post_upload = |who: &str, body: Vec<u8>| -> (u16, Value) {
        let url = format!("{}/api/chat/upload", board.base);
        let resp = board
            .decorate(who, board.agent.post(&url))
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send(body)
            .unwrap();
        let status = resp.status().as_u16();
        (status, resp.into_body().read_json().unwrap_or(Value::Null))
    };
    let scoped_fields = |context: &str, conversation: &str| -> Vec<(String, String)> {
        vec![
            ("install_id".into(), crm.clone()),
            ("context_id".into(), context.to_string()),
            ("conversation".into(), conversation.to_string()),
        ]
    };
    fn as_refs(v: &[(String, String)]) -> Vec<(&str, &str)> {
        v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }
    let fields_a = scoped_fields(&rest.crm_a, &conv_a);
    // Control upload through the board, scoped to company A's conversation.
    let (status, row) = post_upload(
        "operator",
        multipart(&as_refs(&fields_a), "leads.csv", SECRETLESS.as_bytes()),
    );
    assert_eq!(status, 200, "{row}");
    let http_a = row["id"].as_str().unwrap().to_string();
    assert_eq!(row["scope"], json!(format!("app:{crm}@{conv_a}")), "{row}");
    let (rows0, messages0) = (
        count("SELECT count(*) FROM chat_files"),
        count("SELECT count(*) FROM messages"),
    );
    // Agents and a sessionless caller cannot upload through the board.
    for (who, check) in [
        ("agent:master", "operator_only"),
        ("agent:w1", "operator_only"),
        ("unproven", "caller_identity"),
    ] {
        let (status, reply) =
            post_upload(who, multipart(&as_refs(&fields_a), "x.csv", b"a,b\n1,2\n"));
        assert_eq!(status, 403, "{who}: {reply}");
        assert_eq!(reply["check"], json!(check), "{who}: {reply}");
    }
    // A forged readiness / scope field on the upload refuses whole.
    for field in ["ready", "scope", "sha256"] {
        let mut fields = as_refs(&fields_a);
        fields.push((field, "1"));
        let (status, reply) = post_upload("operator", multipart(&fields, "x.csv", b"a,b\n1,2\n"));
        assert_eq!(status, 400, "{field}: {reply}");
        assert!(
            reply["error"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!("field '{field}' is not accepted")),
            "{field}: {reply}"
        );
    }
    let post_msg = |who: &str, msg: &str, context: &str, conversation: &str, ids: Value| {
        board.call(
            who,
            "POST",
            "/api/threads/master/messages",
            Some(json!({"text": "read it", "message": msg,
                        "app": {"install_id": crm, "context_id": context},
                        "conversation": conversation, "attachments": ids})),
        )
    };
    // Agents cannot attach through the board.
    for who in ["agent:master", "agent:w1"] {
        let (status, reply) = post_msg(
            who,
            "h-agent",
            &rest.crm_a,
            &conv_a,
            json!([{"id": http_a}]),
        );
        assert_eq!(status, 403, "{who}: {reply}");
        assert_eq!(reply["check"], json!("operator_only"), "{who}: {reply}");
    }
    // Forged cross-company, unknown and client-ready references refuse.
    let http_cases: Vec<(&str, &str, &str, Value, &str)> = vec![
        (
            "cross-company",
            &rest._crm_b,
            &conv_b,
            json!([{"id": http_a}]),
            not_scoped,
        ),
        (
            "unknown",
            &rest.crm_a,
            &conv_a,
            json!([{"id": "chf-0123456789abcdef0123456789abcdef"}]),
            "unknown attachment",
        ),
        (
            "client-ready-field",
            &rest.crm_a,
            &conv_a,
            json!([{"id": http_a, "ready": true}]),
            "unknown field",
        ),
    ];
    for (i, (what, context, conversation, ids, reason)) in http_cases.into_iter().enumerate() {
        let (status, reply) = post_msg(
            "operator",
            &format!("h-bad-{i}"),
            context,
            conversation,
            ids,
        );
        assert_eq!(status, 400, "{what}: {reply}");
        let err = reply["error"].as_str().unwrap_or_default().to_string();
        assert!(err.contains(reason), "{what}: {reply}");
        no_bytes(what, &err);
    }
    assert_eq!(
        count("SELECT count(*) FROM chat_files"),
        rows0,
        "HTTP refusal minted a row"
    );
    assert_eq!(
        count("SELECT count(*) FROM messages"),
        messages0,
        "HTTP refusal queued"
    );
    // Control: the same-scope reference is accepted through the board.
    let (status, reply) = post_msg(
        "operator",
        "h-ok",
        &rest.crm_a,
        &conv_a,
        json!([{"id": http_a}]),
    );
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["state"], json!("queued"), "{reply}");
    assert_eq!(count("SELECT count(*) FROM messages"), messages0 + 1);
}

/// CAD-1168: the master reads an attachment with the id alone, on its own
/// live turn, in an APP conversation too — the CLI no longer calls
/// `agent_show` (which Gate 2 refuses under an app turn), so the daemon
/// resolves the running turn from the proven caller. An id on the turn's
/// envelope reads; an id of the same conversation that was never sent
/// with this turn is refused with the envelope reason, a Home turn
/// still reads its own file, and a master with no running turn reads
/// nothing.
///
/// Guards: `rpc_chat_file_read`'s agent arm (the live-turn resolution and
/// the envelope-membership check, daemon/chat_files_rpc.rs).
#[test]
fn master_reads_attachments_on_its_live_turn_by_id_alone() {
    let gx = gx();
    let staged = |bytes: &[u8]| {
        let dir = gx.dir.path().join(crate::wiki::UPLOAD_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&tmp, bytes).unwrap();
        tmp.to_string_lossy().into_owned()
    };
    let app = json!({"install_id": gx.crm, "context_id": gx.crm_a});
    let conv = gx.conversation(&gx.crm, &gx.crm_a, json!({"general": true}));
    let app_upload = |name: &str, bytes: &[u8]| -> String {
        gx.operator(
            "chat_file_upload",
            json!({"name": name, "tmp": staged(bytes), "app": app, "conversation": conv}),
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let sent = app_upload("sent.csv", b"name\nAnn");
    let unsent = app_upload("unsent.txt", b"never sent");
    let read = |id: &str| {
        gx.call(
            Asserted::Agent("master".into()),
            "chat_file_read",
            json!({"id": id}),
        )
    };

    // No running turn: nothing is readable.
    let err = err_text(read(&sent));
    assert!(err.contains("none is running"), "{err}");

    // An app turn: the id on its envelope reads (Gate 2 admits the verb
    // and no `agent_show` is involved) ...
    gx.operator(
        "thread_send",
        json!({"alias": "master", "text": "see file", "message": "m-app",
               "app": app, "conversation": conv, "attachments": [{"id": sent}]}),
    )
    .unwrap();
    gx.run("m-app");
    gx.policy("chat_file_read", &json!({"id": sent})).unwrap();
    assert!(gx
        .policy("agent_show", &json!({"alias": "master"}))
        .is_err());
    assert_eq!(read(&sent).unwrap()["text"], json!("name\nAnn"));
    // ... an id of the same conversation that this turn did not carry is
    // refused, and so is an id that does not exist.
    let err = err_text(read(&unsent));
    assert!(err.contains("not on your turn's envelope"), "{err}");
    let err = err_text(read("chf-0000000000000000000000000000dead"));
    assert!(err.contains("not on your turn's envelope"), "{err}");

    // A Home turn still reads its own file, and not the app one.
    gx.finish("m-app", 2.0);
    let home = chat_upload(&gx, "home.txt", b"home text");
    let home_id = home["id"].as_str().unwrap().to_string();
    gx.operator(
        "thread_send",
        json!({"alias": "master", "text": "home file", "message": "m-home",
               "attachments": [{"id": home_id}]}),
    )
    .unwrap();
    gx.run("m-home");
    assert_eq!(read(&home_id).unwrap()["text"], json!("home text"));
    let err = err_text(read(&sent));
    assert!(err.contains("not on your turn's envelope"), "{err}");
}
