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
/// worker, two REAL installations (CRM and Social Content, installed and
/// approved through the daemon), the CRM with two companies and two
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
    let approve = json!({"install_id": out["install_id"], "digest": out["digest"]});
    scoped(Asserted::Operator, || {
        shared.dispatch("app_local_install_approve", &approve, pid())
    })
    .unwrap();
    out["install_id"].as_str().unwrap().to_string()
}

fn gx() -> Gx {
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
    let (crm_a, crm_b, social_ctx) = (ctx(&crm, "Acme"), ctx(&crm, "Beta"), ctx(&social, "Social"));
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
    assert!(
        !app.is_empty() && app.iter().all(|t| t.starts_with("Bash(cadence app ")),
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
/// read-only status for its own turn): the nine scoped verbs,
/// `message_report` and `thread_read` of its own conversation.
const APP_TURN_MAY_CALL: &[&str] = &[
    "app_content_assistant_draft",
    "app_content_assistant_proposal_show",
    "app_content_assistant_proposals",
    "app_record_csv_assistant_import",
    "app_record_csv_assistant_preview",
    "app_segment_assistant_list",
    "app_segment_assistant_preview",
    "app_segment_assistant_save",
    "app_segment_assistant_show",
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
/// eleven in `APP_TURN_MAY_CALL` pass, so the admin RPCs, the permission
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
        agent: ureq::Agent,
        base: String,
        host: String,
        token: String,
        cookie: String,
        key: String,
        _stop: Stop,
    }

    /// A reply: status and the JSON body.
    pub(super) type Reply = (u16, Value);

    impl Board {
        fn decorate<B>(&self, who: &str, b: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
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
            _social: social,
            crm_a,
            _crm_b: crm_b,
            _social_ctx: social_ctx,
            _dir: dir,
        };
        (board, state, rest)
    }

    pub(super) struct Gx2 {
        pub crm: String,
        pub _social: String,
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
/// is served on the same routes. NOT covered here: `GET
/// /api/threads/<alias>/conversations?install=` has no board gate (see the
/// PR report).
///
/// Guards: `WRITE_ROUTES` (ui/operator.rs) + `operator::admit` for the
/// POSTs; `admit_operator_read` (ui/serve.rs) for the installation GET.
#[test]
fn i1_board_http_refuses_agents_on_every_conversation_route_like_the_rpc() {
    let (board, state, rest) = http::start(gx());
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
        &format!("/api/threads/master/conversations?install={crm}"),
        None,
    );
    assert_eq!(status, 200, "{reply}");
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
