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
use crate::store::conversations::Scope as WorkScopeRef;
use crate::store::ConversationKind;

/// The stable id of the unknown/system bucket row (the legacy bucket).
const UNKNOWN_CONVERSATION: &str = "conv-legacy-unknown";

/// The daemon's resolved filter context for one work list or read. Owns
/// the uid and repo values so the borrowed store `Scope` can point at them.
struct WorkScope {
    include_system: bool,
    agent_uid: Option<String>,
    project_uids: Option<Vec<String>>,
    project_repos: Option<Vec<String>>,
}

impl WorkScope {
    fn as_scope(&self) -> WorkScopeRef<'_> {
        WorkScopeRef {
            include_system: self.include_system,
            agent_uid: self.agent_uid.as_deref(),
            project_uids: self.project_uids.as_deref(),
            project_repos: self.project_repos.as_deref(),
        }
    }
}

/// A boolean request field, absent meaning `false`; a non-boolean is refused.
fn optional_bool(params: &Value, field: &str) -> Result<bool> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(Error::rejected(format!("{field} must be a boolean"))),
    }
}

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
    /// A message's participant: an agent by its uid (live alias, or the
    /// stored label of that uid once removed), the operator by its label,
    /// and a legacy row with neither is `unknown` and never attributed.
    fn message_participant(&self, uid: Option<&str>, label: Option<&str>) -> Result<Value> {
        match (uid, label) {
            (Some(uid), _) => self.store.participant(uid),
            (None, Some("operator")) => {
                Ok(json!({"uid": null, "alias": "operator", "kind": "operator", "removed": false}))
            }
            _ => Ok(json!({"uid": null, "alias": null, "kind": "unknown", "removed": false})),
        }
    }

    /// A registered project's GitHub repos as lowercase `owner/name`, from
    /// the tracker's project list (the authority for membership). An
    /// unknown project is refused.
    fn project_repos_of(&self, key: &str) -> Result<Vec<String>> {
        let pm_dir = self.pm_dir()?;
        let project = crate::issue::project::list(&pm_dir)?
            .into_iter()
            .find(|p| p.key == key)
            .ok_or_else(|| Error::rejected(format!("Unknown project '{key}'")))?;
        Ok(project
            .repos
            .iter()
            .filter_map(|r| match (&r.remote, &r.path) {
                (Some(remote), _) => crate::issue::project::normalize_remote(remote)
                    .strip_prefix("github.com/")
                    .map(str::to_string),
                (None, Some(path)) => {
                    crate::audit::origin_slug(&crate::issue::project::expand_home(path))
                }
                _ => None,
            })
            .map(|s| s.to_ascii_lowercase())
            .collect())
    }

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

    /// CAD-1636: a subject of a known kind. `campaign:` must exist as the
    /// installation's own content; `whatsapp:` names a door conversation
    /// id and exists only on a hosted daemon. Any other kind refuses.
    pub(super) fn verify_subject(&self, install: &str, context: &str, subject: &str) -> Result<()> {
        if let Some(id) = subject.strip_prefix(store::SUBJECT_WHATSAPP) {
            crate::platform::hosted_whatsapp::check_conversation_id(id)?;
            self.whatsapp_door()?;
            return Ok(());
        }
        self.verify_campaign_subject(install, context, subject)
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
            self.verify_subject(install, context, subject)?;
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
        let rows = self.store.conversation_summaries(&alias, install)?;
        let general = rows
            .iter()
            .find(|row| row["is_general"] == true)
            .and_then(|row| row["id"].as_str());
        Ok(json!({
            "alias": alias,
            "install_id": install,
            "general": general,
            "conversations": rows,
        }))
    }

    /// The daemon's filter context for one list or read. Every value is
    /// resolved here from the daemon's own rows: the agent's uid from its
    /// alias, the agents whose registered checkout is in the project, and
    /// the project's repos. A client never names a uid or a repo.
    fn work_scope(
        &self,
        include_system: bool,
        agent: Option<&str>,
        project: Option<&str>,
    ) -> Result<WorkScope> {
        let agent_uid = match agent {
            Some(alias) => Some(
                self.store
                    .agent_opt(alias)?
                    .ok_or_else(|| Error::rejected(format!("Unknown agent '{alias}'")))?
                    .uid,
            ),
            None => None,
        };
        let (project_uids, project_repos) = match project {
            None => (None, None),
            Some(key) => {
                let repos = self.project_repos_of(key)?;
                // The authoritative cwd-to-project mapping (remote first,
                // then the canonical registered checkout root), so a
                // checkout with no origin still belongs to its project.
                let pm_dir = self.pm_dir()?;
                let mut uids = Vec::new();
                for agent in self.store.agents()? {
                    let in_project = crate::issue::project::key_for_cwd(
                        &pm_dir,
                        std::path::Path::new(&agent.cwd),
                    )
                    .as_deref()
                        == Some(key);
                    if in_project {
                        uids.push(agent.uid);
                    }
                }
                (Some(uids), Some(repos))
            }
        };
        Ok(WorkScope {
            include_system,
            agent_uid,
            project_uids,
            project_repos,
        })
    }

    /// CAD-819 `work_conversation_list {project?, agent?, include_system?}`
    /// — the operator's cross-agent view: pair rows, the operator's own
    /// agent row, group rows and the unknown/system bucket, each filtered
    /// by the same scope the read uses. Operator-only and read-only.
    pub(super) fn rpc_work_conversation_list(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["project", "agent", "include_system"],
            "work_conversation_list",
        )?;
        self.operator_chat("work_conversation_list", peer_pid)?;
        let include_system = optional_bool(params, "include_system")?;
        let agent = optional_str(params, "agent");
        let project = optional_str(params, "project");
        let scope = self.work_scope(include_system, agent, project)?;
        let scope_ref = scope.as_scope();
        let mut rows = Vec::new();
        for pair in self.store.work_conversation_pairs()? {
            if !self
                .store
                .work_conversation_in_scope(&pair.a_uid, &pair.b_uid, &scope_ref)?
            {
                continue;
            }
            let (counts, oldest) =
                self.store
                    .work_conversation_counts(&pair.a_uid, &pair.b_uid, &scope_ref)?;
            let preview =
                match self
                    .store
                    .work_conversation_latest(&pair.a_uid, &pair.b_uid, &scope_ref)?
                {
                    Some(m) => store::conversations::preview(&m.body)?,
                    None => String::new(),
                };
            // Every stored state is counted; the six named states always
            // appear, even at zero.
            let mut state_counts: serde_json::Map<String, Value> = [
                "queued",
                "claimed",
                "running",
                "completed",
                "failed",
                "unknown",
            ]
            .iter()
            .map(|s| (s.to_string(), json!(0)))
            .collect();
            for (state, n) in counts {
                state_counts.insert(state, json!(n));
            }
            let participants = [
                self.store.participant(&pair.a_uid)?,
                self.store.participant(&pair.b_uid)?,
            ];
            rows.push(json!({
                "id": pair.id,
                "kind": "pair",
                "participants": participants,
                "last": {"preview": preview, "at": pair.last_at},
                "counts": state_counts,
                "oldest_unhandled_at": oldest,
                "last_activity": pair.last_at,
            }));
        }
        // The operator's own chat with an agent: its home thread. Under an
        // agent filter only that agent's row; under a project scope only
        // agents whose checkout is in the project. Project-only filters keep
        // these rows, so they are not skipped by the project filter.
        for thread in self.store.home_threads()? {
            if agent.is_some_and(|f| f != thread.alias) {
                continue;
            }
            if let Some(uids) = &scope.project_uids {
                let uid = self.store.agent_opt(&thread.alias)?.map(|a| a.uid);
                if !uid.is_some_and(|u| uids.contains(&u)) {
                    continue;
                }
            }
            let uid = self.store.agent_opt(&thread.alias)?.map(|a| a.uid);
            rows.push(json!({
                "id": thread.id,
                "kind": "operator",
                "participants": [
                    {"uid": null, "alias": "operator", "kind": "operator", "removed": false},
                    match uid {
                        Some(uid) => self.store.participant(&uid)?,
                        None => json!({"uid": null, "alias": thread.alias, "kind": "agent", "removed": true}),
                    },
                ],
                "last_activity": thread.updated,
            }));
        }
        // Group rows: a topic whose source is the project (`project:<key>`)
        // or that has an in-scope subscriber. Read-only.
        for (topic, members, posts, last_at) in self.store.topic_listing()? {
            // CAD-1412: private human rooms never appear in the legacy list.
            if topic.kind == crate::store::human_threads::ROOM_TOPIC_TYPE {
                continue;
            }
            let subscribers = self.store.topic_subscribers(&topic.id)?;
            if let Some(key) = project {
                let by_source = topic.kind == "project" && topic.source == key;
                let by_member = scope
                    .project_uids
                    .as_ref()
                    .is_some_and(|uids| subscribers.iter().any(|u| uids.contains(u)));
                if !by_source && !by_member {
                    continue;
                }
            }
            if let Some(uid) = &scope.agent_uid {
                if !subscribers.iter().any(|u| u == uid) {
                    continue;
                }
            }
            let participants: Vec<Value> = subscribers
                .iter()
                .map(|uid| self.store.participant(uid))
                .collect::<Result<_>>()?;
            rows.push(json!({
                "id": topic.id,
                "kind": "group",
                "type": topic.kind,
                "source": topic.source,
                "members": members,
                "participants": participants,
                "posts": posts,
                "last_activity": last_at,
            }));
        }
        // The unknown/system bucket: rows with no recorded sender, one
        // explicit row, with the same scope as the read below it.
        let (count, latest_seq, latest_at) = self.store.work_unknown_summary(&scope_ref)?;
        if count > 0 {
            rows.push(json!({
                "id": UNKNOWN_CONVERSATION,
                "kind": "unknown",
                "participants": [],
                "last": {"preview": "", "at": latest_at},
                "counts": {"unknown": count},
                "last_activity": latest_at,
                "latest_seq": latest_seq,
            }));
        }
        Ok(json!({"conversations": rows}))
    }

    /// CAD-819 `work_conversation_read {conversation, tail?|before?|after?,
    /// limit?, project?, agent?, include_system?}` — one page of a pair, or of
    /// the unknown bucket, under the same scope the list used. Operator-only,
    /// read-only, never drains.
    pub(super) fn rpc_work_conversation_read(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_unknown_fields(
            params,
            &[
                "conversation",
                "tail",
                "before",
                "after",
                "limit",
                "project",
                "agent",
                "include_system",
            ],
            "work_conversation_read",
        )?;
        self.operator_chat("work_conversation_read", peer_pid)?;
        let id = required_str(params, "conversation")?;
        crate::proto::identifier(id, "conversation ID")?;
        let before = optional_i64(params, "before");
        let after = optional_i64(params, "after");
        if before.is_some() && after.is_some() {
            return Err(Error::rejected(
                "work_conversation_read takes before or after, not both",
            ));
        }
        let limit = optional_i64(params, "limit").unwrap_or(50);
        if !(1..=store::THREAD_PAGE_MAX).contains(&limit) {
            return Err(Error::rejected(format!(
                "Conversation page limit must be 1-{}",
                store::THREAD_PAGE_MAX
            )));
        }
        let include_system = optional_bool(params, "include_system")?;
        let agent = optional_str(params, "agent");
        let project = optional_str(params, "project");
        let scope = self.work_scope(include_system, agent, project)?;
        let scope_ref = scope.as_scope();

        // The unknown bucket: its own identity, recipient-scoped read.
        let unknown = id == UNKNOWN_CONVERSATION;
        let pair = if unknown {
            None
        } else {
            Some(
                self.store
                    .work_conversation_pairs()?
                    .into_iter()
                    .find(|p| p.id == id)
                    .ok_or_else(|| Error::rejected("Unknown conversation"))?,
            )
        };
        let messages = match &pair {
            Some(p) => self
                .store
                .work_conversation_messages(&p.a_uid, &p.b_uid, &scope_ref, before, after, limit)?,
            None => self
                .store
                .work_unknown_messages(&scope_ref, before, after, limit)?,
        };
        let cursor = messages.last().map(|m| m.seq).unwrap_or(0);
        let more_before = match (messages.first(), &pair) {
            (Some(first), Some(p)) => self
                .store
                .work_conversation_has_before(&p.a_uid, &p.b_uid, first.seq, &scope_ref)?,
            (Some(first), None) => !self
                .store
                .work_unknown_messages(&scope_ref, Some(first.seq), None, 1)?
                .is_empty(),
            (None, _) => false,
        };
        let mut out = Vec::with_capacity(messages.len());
        for m in messages
            .iter()
            .filter(|m| include_system || !store::conversations::is_system(m))
        {
            out.push(json!({
                "id": m.id,
                "seq": m.seq,
                "conversation": id,
                "system": store::conversations::is_system(m),
                "sender": self.message_participant(m.sender_uid.as_deref(), m.sender.as_deref())?,
                "recipient": self.message_participant(m.recipient_uid.as_deref(), m.recipient.as_deref())?,
                "body": store::conversations::body(&m.body),
                "source": m.source,
                "state": m.state,
                "result": m.result,
                "error": m.error,
                "origin": m.origin,
                "task_id": m.task_id,
                "issue": m.issue,
                "created": m.created,
                "started": m.started,
                "completed": m.completed,
            }));
        }
        // Routed copies nest under the message they answer (`origin`).
        if let Some(p) = &pair {
            let routed = self.store.work_conversation_routed_of(
                &p.a_uid,
                &p.b_uid,
                &messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            )?;
            for row in out.iter_mut() {
                let mid = row["id"].as_str().unwrap_or_default().to_string();
                let copies: Vec<Value> = routed
                    .iter()
                    .filter(|c| c["origin"].as_str() == Some(mid.as_str()))
                    .cloned()
                    .collect();
                row["routed"] = Value::Array(copies);
            }
        }
        let conversation = match &pair {
            Some(p) => json!({"id": p.id, "kind": "pair",
            "participants": [
                self.store.participant(&p.a_uid)?,
                self.store.participant(&p.b_uid)?,
            ]}),
            None => json!({"id": UNKNOWN_CONVERSATION, "kind": "unknown", "participants": []}),
        };
        Ok(json!({
            "conversation": conversation,
            "messages": out,
            "cursor": cursor,
            "more_before": more_before,
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
            &[
                "alias",
                "install_id",
                "context_id",
                "subject",
                "general",
                "public_session",
            ],
            "conversation create",
        )?;
        self.operator_chat("conversation create", peer_pid)?;
        // CAD-1374: the verified owner human, or `None` for the local operator.
        let human = self.verified_owner_human(params)?;
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
                self.verify_subject(install, context, subject)?;
                ConversationKind::Subject(subject)
            }
            (false, None) => ConversationKind::Fresh,
        };
        let (thread, created) =
            self.store
                .conversation_create_as(&alias, install, context, kind, human.as_ref())?;
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
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            ..Default::default()
        };
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
                    params: (alias == "master").then_some(r#"{"model":"fixture/model"}"#),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        let mut settings = shared.store.settings().unwrap();
        settings.doc.pi = Some(
            serde_json::from_value(serde_json::json!({
                "models": {"allow": ["fixture/model"], "default": {"master": "fixture/model"}}
            }))
            .unwrap(),
        );
        shared
            .store
            .save_settings(settings.revision, settings.doc, "conversations-rpc-fixture")
            .unwrap();
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
