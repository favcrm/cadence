//! CAD-1098: per-app conversations — `conversation_list`,
//! `conversation_create`, and the operator-chat caller proof they share
//! with `thread_send`.
//!
//! A conversation is a thread of the master scoped to one app
//! installation (and optionally one subject, v1: a campaign). Both verbs
//! are the operator's chat: an agent pane, a managed endpoint's tool
//! process and a detached child are refused, exactly like `thread_send`
//! (I1). Neither reads a client field as authority: the installation and
//! the context are proven against the daemon's own store, a campaign
//! subject against the installation's own content rows, and an unknown
//! installation answers "Unknown app installation" before anything is
//! written.

use super::*;
use crate::master;
use crate::store::app_records::RecordStore;
use crate::store::ConversationKind;

/// The campaign id of a `campaign:<id>` subject, in the id grammar the
/// daemon enforces everywhere else.
fn campaign_of(subject: &str) -> Result<&str> {
    let id = subject
        .strip_prefix(store::SUBJECT_CAMPAIGN)
        .ok_or_else(|| Error::rejected("subject must be campaign:<id>"))?;
    crate::proto::identifier(id, "campaign ID")?;
    Ok(id)
}

/// A conversation's identity for the session-switch rule: every home
/// message (or one with no thread) is one key.
fn session_key(thread: Option<&store::Thread>) -> String {
    match thread {
        Some(t) if !t.is_home() => t.id.clone(),
        _ => "home".to_string(),
    }
}

impl Shared {
    /// CAD-1098 I7: a provider session serves ONE conversation. Before
    /// `message` is delivered, compare its conversation with the one the
    /// last delivered message belonged to (a stored fact, so it holds
    /// across a daemon restart too). On a difference the session ends and
    /// a new one opens, with:
    ///
    /// - the tool profile chosen from the message's stored conversation
    ///   (Gate 1 — never a prompt or param; `App` for an app
    ///   conversation, `Home` otherwise),
    /// - no resumed provider history (`thread_id` cleared) and a
    ///   continuity pack due, built from that conversation only,
    /// - the master's tmp dir emptied (I11).
    ///
    /// Returns whether a switch happened. A switch that cannot complete
    /// is an error — the caller fences rather than delivering into the
    /// wrong session.
    pub(super) fn switch_session_if_needed(
        &self,
        alias: &str,
        adapter: &Arc<dyn ProviderAdapter>,
        message: &Message,
    ) -> Result<bool> {
        if !master::is_master(alias) || message.is_nudge() {
            return Ok(false);
        }
        let target = self.store.message_conversation(alias, &message.id)?;
        let previous = match self.store.last_delivered_message(alias, &message.id)? {
            Some(id) => self.store.message_conversation(alias, &id)?,
            None => None,
        };
        let profile = match &target {
            Some(t) if !t.is_home() => master::Profile::App,
            _ => master::Profile::Home,
        };
        // Same conversation as the last delivered message AND a session
        // already running this profile: keep it. A fresh actor opens Home,
        // so an app conversation resumed after a restart still switches
        // (its pack carries the history).
        if session_key(previous.as_ref()) == session_key(target.as_ref())
            && adapter.session_profile() == profile
        {
            return Ok(false);
        }
        self.revoke_endpoint(alias, "conversation switch");
        adapter.close();
        if let Err(e) = master::clear_tmp(&self.state_dir) {
            return Err(Error::rejected(format!(
                "conversation switch: the master's tmp dir could not be cleared: {e}"
            )));
        }
        adapter.set_session_profile(profile);
        let mut fresh = self.store.agent(alias)?;
        fresh.thread_id = None;
        let identity = adapter.open(&fresh)?;
        self.store
            .set_identity_with_quota(alias, &identity, adapter.quota_snapshot())?;
        self.continuity_due
            .lock()
            .unwrap()
            .insert(alias.to_string(), crate::continuity::Reason::New);
        self.enroll_endpoint(alias);
        adapter.post_enrollment_ready(&fresh)?;
        let _ = self.store.event_public(
            alias,
            "conversation_session_switched",
            json!({"message": message.id, "profile": format!("{profile:?}")}),
        );
        self.wake();
        Ok(true)
    }

    /// The operator's chat caller rule, shared by `thread_send`,
    /// `conversation_list` and `conversation_create`: a connection the
    /// daemon attributes to a pane or managed endpoint is refused, so is
    /// one whose identity cannot be derived (fail closed), and a
    /// connection tied to no agent must still be provably the operator
    /// (CAD-276, CAD-339) — a detached child of an agent derives none.
    pub(super) fn operator_chat(&self, verb: &str, peer_pid: u32) -> Result<()> {
        match self.caller_identity(peer_pid) {
            Ok(Caller::NoAgentIdentity) => self.proven_operator(verb, peer_pid),
            Ok(Caller::Agent(v)) => Err(Error::rejected(format!(
                "{verb} is the operator's chat — this connection is agent '{}'; \
                 agents message each other with `cadence send`",
                v.agent.alias
            ))),
            Err(e) => Err(Error::rejected(format!(
                "{verb} refused: caller identity underivable — {e}"
            ))),
        }
    }

    /// The installation exists: it has a context in the store, or the
    /// catalog (the installed apps) names it — an app with no context yet
    /// (Social Content before a brand) is still an installation.
    pub(super) fn known_install(&self, install: &str) -> Result<()> {
        crate::proto::identifier(install, "installation ID")?;
        if self.store.app_install_known(install)? {
            return Ok(());
        }
        let in_catalog = self
            .pm_dir()
            .and_then(|dir| self.pm_at(&dir))
            .and_then(|pm| {
                crate::issue::app_catalog::workspace::with_runtime_read(&pm, install, |_, _| Ok(()))
            })
            .is_ok();
        if !in_catalog {
            return Err(Error::rejected("Unknown app installation"));
        }
        Ok(())
    }

    /// A campaign subject exists in this installation and the proven
    /// context: the installation's own content row, never a client claim.
    pub(super) fn verify_campaign_subject(
        &self,
        install: &str,
        context: &str,
        subject: &str,
    ) -> Result<()> {
        let campaign = campaign_of(subject)?;
        RecordStore::open(&self.state_dir, install)?
            .app_content_show(context, campaign)
            .map(|_| ())
            .map_err(|_| {
                Error::rejected("that campaign does not exist in this installation and context")
            })
    }

    /// `thread_send`'s selector: the named conversation must exist, be
    /// this alias's, unarchived, belong to the binding's installation,
    /// and — for a campaign — still verify against the installation's own
    /// content in the binding's context (I2, I5). Read-only: the enqueue
    /// transaction decides again.
    pub(super) fn verify_conversation_selector(
        &self,
        alias: &str,
        app: &Value,
        conversation: &str,
    ) -> Result<()> {
        let install = app["install_id"].as_str().unwrap_or_default();
        let context = app["context_id"].as_str();
        let thread = self
            .store
            .thread_by_id(conversation)?
            .filter(|t| t.alias == alias && t.install_id.as_deref() == Some(install))
            .ok_or_else(|| Error::rejected("Unknown conversation for this app installation"))?;
        if thread.archived {
            return Err(Error::rejected(
                "this conversation is archived and read-only",
            ));
        }
        if let Some(subject) = &thread.subject {
            let Some(context) = context.filter(|c| thread.context_id.as_deref() == Some(*c)) else {
                return Err(Error::rejected(
                    "this campaign conversation serves only the context it was created in",
                ));
            };
            self.verify_campaign_subject(install, context, subject)?;
        }
        Ok(())
    }

    /// `conversation_list {alias, install_id}` — the installation's
    /// conversations of `alias`: General first (ensured), then by
    /// creation. An unknown installation is refused and creates nothing.
    pub(super) fn rpc_conversation_list(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_unknown_fields(params, &["alias", "install_id"], "conversation list")?;
        self.operator_chat("conversation list", peer_pid)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let install = required_str(params, "install_id")?;
        self.known_install(install)?;
        // General always exists once an installation is proven.
        self.store.conversation_ensure_general(&alias, install)?;
        let rows = self.store.conversation_list(&alias, install)?;
        let general = rows.iter().find(|t| t.is_general).map(|t| t.id.clone());
        Ok(json!({
            "alias": alias,
            "install_id": install,
            "general": general,
            "conversations": rows.iter().map(store::Thread::to_json).collect::<Vec<_>>(),
        }))
    }

    /// `conversation_create {alias, install_id, context_id, subject?,
    /// general?}` — make or find a conversation:
    ///
    /// - `general: true` — the app's General conversation, idempotent;
    /// - `subject: "campaign:<id>"` — idempotent per (install, subject);
    ///   the campaign must exist in this installation and context;
    /// - neither — "New conversation": always a fresh row.
    ///
    /// Answers `{conversation, created}`. Nothing is written when the
    /// installation, the context or the campaign does not prove.
    pub(super) fn rpc_conversation_create(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["alias", "install_id", "context_id", "subject", "general"],
            "conversation create",
        )?;
        self.operator_chat("conversation create", peer_pid)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let install = required_str(params, "install_id")?;
        self.known_install(install)?;
        // A context is proven when given; a campaign subject needs one.
        let context = optional_text(params, "context_id")?;
        if let Some(context) = context {
            self.store.app_context_proof(install, context)?;
        }
        let general = match params.get("general") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(Error::rejected("general must be a boolean")),
        };
        let subject = optional_text(params, "subject")?;
        let kind = match (general, subject) {
            (true, Some(_)) => {
                return Err(Error::rejected(
                    "a conversation is General or has a subject, not both",
                ))
            }
            (true, None) => ConversationKind::General,
            (false, Some(subject)) => {
                let context = context.ok_or_else(|| {
                    Error::rejected("a campaign conversation needs the context it belongs to")
                })?;
                self.verify_campaign_subject(install, context, subject)?;
                ConversationKind::Subject(subject)
            }
            (false, None) => ConversationKind::Fresh,
        };
        let (thread, created) = self
            .store
            .conversation_create(&alias, install, context, kind)?;
        self.wake();
        Ok(json!({"conversation": thread.to_json(), "created": created}))
    }
}

/// Refuse any request field outside `allowed` — a wire peer is
/// untrusted, and an unlisted field is never silently dropped.
fn reject_unknown_fields(params: &Value, allowed: &[&str], verb: &str) -> Result<()> {
    let object = params
        .as_object()
        .ok_or_else(|| Error::rejected(format!("{verb} takes an object")))?;
    if let Some(field) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::rejected(format!(
            "{verb} takes {} only; field '{field}' is not accepted",
            allowed.join(", ")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::store::app_content::Draft;
    use crate::store::app_contexts::ContextConfig;
    use crate::store::NewAgent;
    use crate::test_seam::{scoped, Asserted};

    struct Fx {
        dir: tempfile::TempDir,
        shared: Arc<Shared>,
        /// install-1: two contexts (two "companies"); install-2: one.
        crm_a: String,
        crm_b: String,
        social: String,
    }

    fn pid() -> u32 {
        std::process::id()
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("no-pm").to_str().unwrap());
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
        let ctx = |install: &str, label: &str| {
            let config = ContextConfig::new(label, BTreeMap::new()).unwrap();
            shared
                .store
                .app_context_create(
                    install,
                    &config,
                    &format!("req-{install}-{}", label.to_lowercase()),
                )
                .unwrap()["context"]["id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let crm_a = ctx("install-1", "Acme");
        let crm_b = ctx("install-1", "Beta");
        let social = ctx("install-2", "Social");
        let fx = Fx {
            dir,
            shared,
            crm_a,
            crm_b,
            social,
        };
        fx.campaign("install-1", &fx.crm_a.clone(), "cmp-a");
        fx.campaign("install-1", &fx.crm_b.clone(), "cmp-b");
        fx
    }

    impl Fx {
        fn campaign(&self, install: &str, context: &str, id: &str) {
            let draft = Draft::parse(
                "Hello",
                "Preheader",
                &[json!({"type": "paragraph", "text": "Body."})],
            )
            .unwrap();
            RecordStore::open(self.dir.path(), install)
                .unwrap()
                .app_content_save(context, id, None, &draft)
                .unwrap();
        }

        fn as_operator(&self, method: &str, params: Value) -> Result<Value> {
            scoped(Asserted::Operator, || {
                self.shared.dispatch(method, &params, pid())
            })
        }

        fn create(&self, install: &str, context: &str, extra: Value) -> Result<Value> {
            let mut params = json!({"alias": "master", "install_id": install,
                                    "context_id": context});
            for (k, v) in extra.as_object().unwrap() {
                params[k] = v.clone();
            }
            self.as_operator("conversation_create", params)
        }

        fn send(
            &self,
            id: &str,
            install: &str,
            context: &str,
            conversation: Option<&str>,
        ) -> Result<Value> {
            let mut params = json!({"alias": "master", "text": format!("hello {id}"),
                "message": id, "app": {"install_id": install, "context_id": context}});
            if let Some(c) = conversation {
                params["conversation"] = json!(c);
            }
            self.as_operator("thread_send", params)
        }

        fn entries_of(&self, thread: &str) -> Vec<store::ThreadEntry> {
            self.shared.store.thread_entries_of(thread, 0, 100).unwrap()
        }
    }

    fn conv_id(created: &Value) -> String {
        created["conversation"]["id"].as_str().unwrap().to_string()
    }

    /// I2: a verified app send lands in the conversation the server
    /// resolves — General without a selector, the named conversation
    /// with one — and never in home; an unbound send is home.
    #[test]
    fn scoped_send_lands_only_in_its_conversation() {
        let fx = fx();
        // General, made on first use.
        let receipt = fx.send("m-g", "install-1", &fx.crm_a, None).unwrap();
        let general = receipt["thread"]["id"].as_str().unwrap().to_string();
        assert_eq!(receipt["thread"]["general"], json!(true));
        assert_eq!(receipt["thread"]["install_id"], json!("install-1"));
        // A campaign conversation, selected by id.
        let campaign = conv_id(
            &fx.create("install-1", &fx.crm_a, json!({"subject": "campaign:cmp-a"}))
                .unwrap(),
        );
        let receipt = fx
            .send("m-c", "install-1", &fx.crm_a, Some(&campaign))
            .unwrap();
        assert_eq!(receipt["thread"]["id"], json!(campaign));
        // An unbound send is the home thread.
        let receipt = fx
            .as_operator(
                "thread_send",
                json!({"alias": "master", "text": "plain", "message": "m-h"}),
            )
            .unwrap();
        let home = receipt["thread"]["id"].as_str().unwrap().to_string();
        assert_ne!(home, general);
        assert_ne!(home, campaign);
        let texts =
            |t: &str| -> Vec<String> { fx.entries_of(t).iter().map(|e| e.text.clone()).collect() };
        assert_eq!(texts(&general), vec!["hello m-g"]);
        assert_eq!(texts(&campaign), vec!["hello m-c"]);
        assert_eq!(texts(&home), vec!["plain"]);
        // The home read (no selector) shows none of the app turns.
        let page = fx
            .as_operator("thread_read", json!({"alias": "master", "after": 0}))
            .unwrap();
        assert_eq!(page["entries"].as_array().unwrap().len(), 1);
        // The conversation read shows only its own.
        let page = fx
            .as_operator(
                "thread_read",
                json!({"alias": "master", "after": 0, "conversation": campaign}),
            )
            .unwrap();
        assert_eq!(page["entries"][0]["text"], json!("hello m-c"));
    }

    /// I2/I5: a forged or unproven binding is refused before any
    /// I5: a campaign conversation serves only the context it was
    /// I5: a campaign subject must exist in the installation's own
    /// I2: every later entry of a message follows the message's
    /// conversation, not "the alias's thread" — including a turn that
    /// I1: an agent, a detached child (no proof) and an unproven caller
    /// The verbs list, make "New conversation" rows and refuse a
    /// private path in the id grammar.
    #[test]
    fn conversation_list_orders_general_first_and_new_is_always_fresh() {
        let fx = fx();
        let list = |install: &str| {
            fx.as_operator(
                "conversation_list",
                json!({"alias": "master", "install_id": install}),
            )
            .unwrap()
        };
        // General is always listed, made on first list.
        assert_eq!(
            list("install-1")["conversations"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            list("install-1")["conversations"][0]["is_general"],
            json!(true)
        );
        let n1 = conv_id(&fx.create("install-1", &fx.crm_a, json!({})).unwrap());
        let n2 = conv_id(&fx.create("install-1", &fx.crm_a, json!({})).unwrap());
        assert_ne!(n1, n2, "New conversation is never idempotent");
        let g = fx
            .create("install-1", &fx.crm_a, json!({"general": true}))
            .unwrap();
        let g2 = fx
            .create("install-1", &fx.crm_b, json!({"general": true}))
            .unwrap();
        assert_eq!(conv_id(&g), conv_id(&g2), "General is one per install");
        assert_eq!(g2["created"], json!(false));
        let listed = list("install-1");
        assert_eq!(listed["general"], json!(conv_id(&g)));
        assert_eq!(g["created"], json!(false), "list had already made General");
        assert_eq!(listed["conversations"][0]["general"], json!(true));
        assert_eq!(listed["conversations"].as_array().unwrap().len(), 3);
        // Another installation sees none of them (only its own General).
        assert_eq!(
            list("install-2")["conversations"].as_array().unwrap().len(),
            1
        );
        assert_ne!(list("install-2")["general"], list("install-1")["general"]);
        let social = conv_id(&fx.create("install-2", &fx.social, json!({})).unwrap());
        assert_eq!(
            list("install-2")["conversations"].as_array().unwrap().len(),
            2
        );
        assert!(fx.entries_of(&social).is_empty());
    }
}
