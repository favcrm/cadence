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

impl Shared {
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

    fn known_install(&self, install: &str) -> Result<()> {
        crate::proto::identifier(install, "installation ID")?;
        if !self.store.app_install_known(install)? {
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
        let context = app["context_id"].as_str().unwrap_or_default();
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
            if thread.context_id.as_deref() != Some(context) {
                return Err(Error::rejected(
                    "this campaign conversation serves only the context it was created in",
                ));
            }
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
        let context = required_str(params, "context_id")?;
        // The context must be active in this installation.
        self.store.app_context_proof(install, context)?;
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

    use rusqlite::Connection;

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

        fn as_who(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
            scoped(who, || self.shared.dispatch(method, &params, pid()))
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

        fn db(&self) -> Connection {
            Connection::open(crate::rollout::db_file(self.dir.path())).unwrap()
        }

        fn count(&self, sql: &str) -> i64 {
            self.db().query_row(sql, [], |r| r.get(0)).unwrap()
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

    /// I2: no client field decides scope, subject or install.
    #[test]
    fn thread_send_refuses_client_scope_subject_fields() {
        let fx = fx();
        for field in [
            "scope",
            "subject",
            "install_id",
            "thread",
            "thread_id",
            "general",
            "title",
        ] {
            let mut params = json!({"alias": "master", "text": "x", "message": "m-f",
                "app": {"install_id": "install-1", "context_id": fx.crm_a}});
            params[field] = json!("whatever");
            let err = fx
                .as_operator("thread_send", params)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!("field '{field}' is not accepted")),
                "{field}: {err}"
            );
        }
        // A forged key inside the binding is refused too.
        for key in ["conversation", "subject", "verified", "scope"] {
            let mut app = json!({"install_id": "install-1", "context_id": fx.crm_a});
            app[key] = json!("x");
            let err = fx
                .as_operator(
                    "thread_send",
                    json!({"alias": "master", "text": "x", "message": "m-f", "app": app}),
                )
                .unwrap_err()
                .to_string();
            assert!(err.contains("is not accepted"), "{key}: {err}");
        }
        // A selector without a verified binding is not a way into a conversation.
        let general = conv_id(
            &fx.create("install-1", &fx.crm_a, json!({"general": true}))
                .unwrap(),
        );
        let err = fx
            .as_operator(
                "thread_send",
                json!({"alias": "master", "text": "x", "message": "m-f", "conversation": general}),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs a verified app binding"), "{err}");
        assert_eq!(fx.count("SELECT count(*) FROM messages"), 0);
        assert!(fx.entries_of(&general).is_empty());
    }

    /// I2/I5: a forged or unproven binding is refused before any
    /// conversation row is made.
    #[test]
    fn forged_app_binding_creates_no_conversation() {
        let fx = fx();
        let rows = || fx.count("SELECT count(*) FROM threads");
        let before = rows();
        // Unknown installation, unknown context, another install's context.
        for (install, context) in [
            ("install-9", fx.crm_a.as_str()),
            ("install-1", "ctx-nope"),
            ("install-1", fx.social.as_str()),
        ] {
            assert!(
                fx.send("m-x", install, context, None).is_err(),
                "{install} {context}"
            );
        }
        assert_eq!(rows(), before, "a refused send made a conversation");
        assert_eq!(fx.count("SELECT count(*) FROM messages"), 0);
        // The create verb: unknown installation is "Unknown app installation".
        let err = fx
            .create("install-9", &fx.crm_a, json!({"general": true}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Unknown app installation"), "{err}");
        let err = fx
            .create("install-1", &fx.social, json!({"general": true}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("context is unavailable"), "{err}");
        let err = fx
            .as_operator(
                "conversation_list",
                json!({"alias": "master", "install_id": "install-9"}),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("Unknown app installation"), "{err}");
        assert_eq!(rows(), before);
    }

    /// I2: a conversation of another installation is never a target.
    #[test]
    fn conversation_of_other_install_refused() {
        let fx = fx();
        let social = conv_id(
            &fx.create("install-2", &fx.social, json!({"general": true}))
                .unwrap(),
        );
        let err = fx
            .send("m-x", "install-1", &fx.crm_a, Some(&social))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Unknown conversation"), "{err}");
        assert!(fx.entries_of(&social).is_empty());
        assert_eq!(fx.count("SELECT count(*) FROM messages"), 0);
        // Nor the other alias's conversation under this alias.
        let err = fx
            .as_operator(
                "thread_read",
                json!({"alias": "w1", "after": 0, "conversation": social}),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("Unknown conversation"), "{err}");
    }

    /// I5: a campaign conversation serves only the context it was
    /// proven under.
    #[test]
    fn campaign_conversation_not_usable_for_other_campaign_or_company() {
        let fx = fx();
        let a = conv_id(
            &fx.create("install-1", &fx.crm_a, json!({"subject": "campaign:cmp-a"}))
                .unwrap(),
        );
        // Another company (context B) of the same installation.
        let err = fx
            .send("m-x", "install-1", &fx.crm_b, Some(&a))
            .unwrap_err()
            .to_string();
        assert!(err.contains("only the context it was created in"), "{err}");
        assert!(fx.entries_of(&a).is_empty());
        // Its own context works.
        fx.send("m-ok", "install-1", &fx.crm_a, Some(&a)).unwrap();
        assert_eq!(fx.entries_of(&a).len(), 1);
        // The same subject in another context is another conversation.
        let b = conv_id(
            &fx.create("install-1", &fx.crm_b, json!({"subject": "campaign:cmp-b"}))
                .unwrap(),
        );
        assert_ne!(a, b);
    }

    /// I5: a campaign subject must exist in the installation's own
    /// content for the proven context.
    #[test]
    fn campaign_subject_must_exist_in_context() {
        let fx = fx();
        let before = fx.count("SELECT count(*) FROM threads");
        // Nonexistent; another company's campaign; a malformed subject.
        for (context, subject) in [
            (fx.crm_a.clone(), "campaign:cmp-nope"),
            (fx.crm_a.clone(), "campaign:cmp-b"),
            (fx.crm_b.clone(), "campaign:cmp-a"),
            (fx.crm_a.clone(), "customer:cmp-a"),
            (fx.crm_a.clone(), "campaign:"),
        ] {
            let err = fx
                .create("install-1", &context, json!({"subject": subject}))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("does not exist")
                    || err.contains("subject must be")
                    || err.contains("campaign ID must be"),
                "{subject}: {err}"
            );
        }
        // The other installation cannot claim install-1's campaign.
        assert!(fx
            .create(
                "install-2",
                &fx.social,
                json!({"subject": "campaign:cmp-a"})
            )
            .is_err());
        assert_eq!(
            fx.count("SELECT count(*) FROM threads"),
            before,
            "a refused create left a row"
        );
        // General and subject together is refused.
        assert!(fx
            .create(
                "install-1",
                &fx.crm_a,
                json!({"general": true, "subject": "campaign:cmp-a"})
            )
            .is_err());
    }

    /// I2: every later entry of a message follows the message's
    /// conversation, not "the alias's thread" — including a turn that
    /// finishes after a home message was queued.
    #[test]
    fn turn_output_follows_the_messages_conversation() {
        let fx = fx();
        let a = conv_id(
            &fx.create("install-1", &fx.crm_a, json!({"subject": "campaign:cmp-a"}))
                .unwrap(),
        );
        fx.send("m-app", "install-1", &fx.crm_a, Some(&a)).unwrap();
        let home = fx
            .as_operator(
                "thread_send",
                json!({"alias": "master", "text": "home q", "message": "m-home"}),
            )
            .unwrap()["thread"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        fx.shared.store.mark_running("m-app", "turn-1").unwrap();
        // Provider output arrives keyed only by the alias: it must land
        // with the running message.
        fx.shared
            .store
            .thread_append_running(
                "master",
                store::ROLE_AGENT,
                store::KIND_TOOL_CALL,
                "Bash: ls",
                None,
            )
            .unwrap();
        let message = fx.shared.store.message("m-app").unwrap().unwrap();
        fx.shared
            .store
            .finish(
                &message,
                "completed",
                &json!({"turn_id": "turn-1", "status": "completed", "text": "done", "stop_reason": "end_turn", "error": null}),
                None,
            )
            .unwrap();
        let kinds =
            |t: &str| -> Vec<String> { fx.entries_of(t).iter().map(|e| e.kind.clone()).collect() };
        assert_eq!(kinds(&a), vec!["message", "tool_call", "turn_result"]);
        assert_eq!(kinds(&home), vec!["message"], "app turn output leaked home");
    }

    /// I1: an agent, a detached child (no proof) and an unproven caller
    /// can neither send into nor create nor list conversations.
    #[test]
    fn agent_cannot_send_or_create_conversation() {
        let fx = fx();
        let rows = fx.count("SELECT count(*) FROM threads");
        for who in [
            Asserted::Agent("w1".into()),
            Asserted::Agent("master".into()),
            Asserted::Unproven,
        ] {
            let err = fx
                .as_who(
                    who.clone(),
                    "conversation_create",
                    json!({"alias": "master", "install_id": "install-1",
                           "context_id": fx.crm_a, "general": true}),
                )
                .unwrap_err()
                .to_string();
            assert!(err.contains("operator"), "{who:?}: {err}");
            assert!(fx
                .as_who(
                    who.clone(),
                    "conversation_list",
                    json!({"alias": "master", "install_id": "install-1"}),
                )
                .is_err());
            assert!(fx
                .as_who(
                    who.clone(),
                    "thread_send",
                    json!({"alias": "master", "text": "x", "message": "m-a",
                           "app": {"install_id": "install-1", "context_id": fx.crm_a}}),
                )
                .is_err());
        }
        assert_eq!(fx.count("SELECT count(*) FROM threads"), rows);
        assert_eq!(fx.count("SELECT count(*) FROM messages"), 0);
    }

    /// I2: concurrent first creates and first sends make one row each.
    #[test]
    fn concurrent_first_send_to_campaign_makes_one_conversation() {
        let fx = Arc::new(fx());
        let barrier = Arc::new(std::sync::Barrier::new(6));
        let mut handles = Vec::new();
        for n in 0..6 {
            let fx = Arc::clone(&fx);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let created = fx
                    .create("install-1", &fx.crm_a, json!({"subject": "campaign:cmp-a"}))
                    .unwrap();
                // And a General first send racing the same way.
                let sent = fx
                    .send(&format!("m-{n}"), "install-1", &fx.crm_a, None)
                    .unwrap();
                (created, sent)
            }));
        }
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            fx.count("SELECT count(*) FROM threads WHERE subject='campaign:cmp-a'"),
            1
        );
        assert_eq!(
            fx.count("SELECT count(*) FROM threads WHERE is_general=1"),
            1
        );
        let created_flags = results
            .iter()
            .filter(|(c, _)| c["created"] == json!(true))
            .count();
        assert_eq!(created_flags, 1, "exactly one create made the row");
        let ids: std::collections::HashSet<_> = results.iter().map(|(c, _)| conv_id(c)).collect();
        assert_eq!(ids.len(), 1);
        // Every send landed in the one General conversation: none lost.
        let general: String = fx
            .db()
            .query_row("SELECT id FROM threads WHERE is_general=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fx.entries_of(&general).len(), 6);
    }

    /// I2: one message id cannot be replayed into another conversation.
    #[test]
    fn retry_same_message_other_conversation_refused() {
        let fx = fx();
        let a = conv_id(
            &fx.create("install-1", &fx.crm_a, json!({"subject": "campaign:cmp-a"}))
                .unwrap(),
        );
        let n = conv_id(&fx.create("install-1", &fx.crm_a, json!({})).unwrap());
        fx.send("m-1", "install-1", &fx.crm_a, Some(&a)).unwrap();
        // The same retry is a duplicate, not a second entry.
        let again = fx.send("m-1", "install-1", &fx.crm_a, Some(&a)).unwrap();
        assert_eq!(again["duplicate"], json!(true));
        // The same id aimed elsewhere (another conversation, General, or
        // none named) is refused whole.
        for other in [Some(n.as_str()), None] {
            let err = fx
                .send("m-1", "install-1", &fx.crm_a, other)
                .unwrap_err()
                .to_string();
            assert!(err.contains("already used with different content"), "{err}");
        }
        assert_eq!(fx.entries_of(&a).len(), 1);
        assert!(fx.entries_of(&n).is_empty());
    }

    /// I2: an archived conversation is kept and read-only.
    #[test]
    fn archived_conversation_is_read_only() {
        let fx = fx();
        let a = conv_id(&fx.create("install-1", &fx.crm_a, json!({})).unwrap());
        fx.send("m-1", "install-1", &fx.crm_a, Some(&a)).unwrap();
        fx.db()
            .execute("UPDATE threads SET archived=1 WHERE id=?", [&a])
            .unwrap();
        let err = fx
            .send("m-2", "install-1", &fx.crm_a, Some(&a))
            .unwrap_err()
            .to_string();
        assert!(err.contains("archived"), "{err}");
        assert_eq!(fx.entries_of(&a).len(), 1);
        assert_eq!(fx.count("SELECT count(*) FROM messages"), 1);
        // Still readable.
        let page = fx
            .as_operator(
                "thread_read",
                json!({"alias": "master", "after": 0, "conversation": a}),
            )
            .unwrap();
        assert_eq!(page["entries"].as_array().unwrap().len(), 1);
        // Removing the agent archives every conversation, keeping rows.
        fx.db()
            .execute(
                "UPDATE agents SET state='stopped',endpoint=NULL WHERE alias='master'",
                [],
            )
            .unwrap();
        fx.shared
            .store
            .remove_agent("master", true, &json!({}))
            .unwrap();
        assert_eq!(
            fx.count(
                "SELECT count(*) FROM threads WHERE alias IS NOT NULL AND install_id IS NOT NULL"
            ),
            0
        );
        assert_eq!(
            fx.count("SELECT count(*) FROM threads WHERE archived=1 AND install_id IS NOT NULL"),
            1
        );
        assert!(fx.entries_of(&a).len() == 1);
    }

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
        // An unknown field is refused, not dropped.
        assert!(fx
            .as_operator(
                "conversation_create",
                json!({"alias": "master", "install_id": "install-1", "context_id": fx.crm_a, "scope": "home"}),
            )
            .is_err());
    }
}
