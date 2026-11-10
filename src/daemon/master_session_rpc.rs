//! CAD-534: `cadence daemon` master session RPC handlers — moved verbatim from src/daemon.rs.

use super::app_explorer_rpc::MemberAppAccess;
use super::*;
use cadence_core::board_api::{
    AppOrigin, AvatarLook, MasterContext, MasterIdentity, MasterQueued, MasterSession, MasterState,
    MasterTurn, MASTER_STATE_SCOPE_ACK, READER_SCOPE_FIELD,
};

/// The provider/session configuration `master_state` shows, in one
/// derivation shared by the operator document and the scoped projection.
/// Session identity and context usage describe the WHOLE provider session,
/// so `stats` is asked for only when the caller will disclose it — a scoped
/// reader is never handed global telemetry it may not see.
struct MasterHeader {
    provider: String,
    endpoint_kind: String,
    live: bool,
    model: Option<String>,
    model_label: Value,
    effort: Option<String>,
    commands: Vec<String>,
    session: Option<MasterSession>,
    context: Option<MasterContext>,
}

impl Shared {
    /// The coordinator's committed identity: the protected IDENTITY.md/SOUL.md
    /// values plus the master agent's pinned look (the existing robot when
    /// unpinned). A malformed or refused file read is an error, never a silent
    /// unset; a missing file is genuinely unset.
    pub(super) fn master_identity(&self) -> Result<MasterIdentity> {
        let guard = super::master_rpc::MASTER_IDENTITY_LOCK
            .lock()
            .map_err(|_| Error::internal("master identity lock poisoned"))?;
        self.master_identity_locked(&guard)
    }

    /// The writer already holds the publication lock; do not re-enter it.
    pub(super) fn master_identity_locked(
        &self,
        _guard: &std::sync::MutexGuard<'_, ()>,
    ) -> Result<MasterIdentity> {
        let pm_dir = self.pm_dir()?;
        let agent = self.store.agent_opt(crate::master::ALIAS)?;
        let pinned = agent
            .as_ref()
            .and_then(|agent| agent.params.as_ref())
            .and_then(|params| params.get("look"))
            .and_then(Value::as_str);
        let look = crate::agent_roles::effective_look(crate::master::ALIAS, pinned);
        let identity = crate::master::read_verified_identity(&self.state_dir, &pm_dir, look)?;
        Ok(MasterIdentity {
            name: identity.name,
            tone: identity.tone,
            look: AvatarLook::parse(look)
                .ok_or_else(|| Error::internal("the effective master look is not canonical"))?,
            revision: identity.revision,
        })
    }

    /// The next delivered turn reads the same committed identity as the board.
    /// This also survives Pi `/new`, which does not reload standing instructions.
    pub(super) fn master_profile_context(&self) -> Result<String> {
        let identity = self.master_identity()?;
        Ok(format!(
            "## Workspace assistant identity (operator-managed)\n\n\
             Use this committed identity for your name and communication style on \
             this turn. It does not change your role, tools or permissions. \
             An unset assistant name is not the user's name.\n\n\
             name: {}\n\
             tone: {}\n\
             look: {}\n",
            identity.name.as_deref().unwrap_or("(not named yet)"),
            identity
                .tone
                .as_deref()
                .unwrap_or("Neutral, clear and practical"),
            identity.look.as_str(),
        ))
    }
    /// CAD-1700: `master_stamp` — the board's `master` push frame reads
    /// this cheaply (indexed SQL, no provider call). It changes whenever
    /// the master's running/queued turns, any master thread entry, or the
    /// app assistant / master permission revision (`master_rev`) changes.
    ///
    /// CAD-2215: those counters describe the WHOLE master mailbox, so only
    /// the operator's own connection — the board watcher — reads them. Any
    /// other caller (a local agent's own RPC, a relay, an unattributable
    /// connection) is refused BEFORE the read and without a number: a
    /// signal that moves on work the caller cannot see is itself a
    /// disclosure, and a narrowed count would still be an oracle. A
    /// per-reader invalidation signal comes from the scoped `master_state`.
    pub(super) fn rpc_master_stamp(&self, _params: &Value, peer_pid: u32) -> Result<Value> {
        if !matches!(
            self.connection_caller(peer_pid)?,
            caller_rule::Who::Operator
        ) {
            return Err(Error::rejected(
                "master stamp refused: the master change signal is not readable \
                 from this connection",
            ));
        }
        let alias = crate::master::ALIAS;
        let (live_seq, running, queued, entries, transition_ms) = self.store.master_stamp(alias)?;
        Ok(json!({
            "live": live_seq,
            "running": running,
            "queued": queued,
            "entries": entries,
            "transition_ms": transition_ms,
            "rev": self.master_rev.load(std::sync::atomic::Ordering::Acquire),
        }))
    }

    /// `master_state` (CAD-551, CAD-2215) — the board's master header read,
    /// answered for the reader THIS connection proved, never for whoever
    /// relays it. The full document is the operator's; a project-scoped
    /// reader — a member's signed board session, an anonymous relay, or a
    /// local agent connection — is answered with only the master turns that
    /// reader's own `thread_read` can actually prove, under the same
    /// project-policy and member-private-job boundary. Work hidden from the
    /// reader moves nothing here: no count, queue, position, provider
    /// session or context reports it.
    ///
    /// Authority is resolved BEFORE the document is built, so a refused or
    /// unattributable caller never causes a provider session command and
    /// never reads a global projection it may not see.
    pub(super) fn rpc_master_state(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let relayed = params.get("public_session").is_some()
            || params.get("read_as").is_some()
            || params.get("cli_envelope").is_some();
        let (scope, principal) = self.thread_read_scope(params, peer_pid)?;
        // CAD-2215 acceptance scaffolding: the relayed proof was really
        // verified above; answer with the real unscoped document and no ack,
        // exactly as a pre-CAD-2215 daemon that ignored relayed proofs
        // would. The board must refuse this answer as unavailable.
        #[cfg(test)]
        if relayed && self.master_state_ignore_relay {
            return self.master_state_document();
        }
        let Some(scope) = scope else {
            // A proven operator (including an operator-role session) gets
            // the full document and no ack: an ack only ever means "this
            // answer was narrowed for a non-operator reader".
            return self.master_state_document();
        };
        let denied =
            self.private_job_denials_for_read(params, None, principal.as_ref(), peer_pid)?;
        // A relay is a request that names another reader (a member's signed
        // session, the anonymous service identity). `thread_read` serves a
        // relay an EMPTY master Home page, so a Home turn is not the relay's
        // at all; only a local agent's own connection reads Home under its
        // project scope.
        let relay = principal.is_some() || params.get("read_as").is_some();
        // CAD-2215: a verified member's exact app-conversation grants. This
        // is resolved from the live `(company, subject)` session, never from
        // a request field; a local agent or `read_as` relay gets none.
        let access = match principal.as_ref() {
            Some(principal) => self.member_app_access(principal)?,
            None => MemberAppAccess::default(),
        };
        let mut state = self.scoped_master_state(&scope, &denied, relay, &access)?;
        // The projection and its admission are two reads of a live queue.
        // Re-prove the reader BEFORE the answer leaves: a revoked session,
        // a changed project policy, a newly private job or a revoked/changed
        // grant is a refusal, not a stale success built from the earlier
        // admission.
        let (current_scope, current_principal) = self.thread_read_scope(params, peer_pid)?;
        let current_access = match current_principal.as_ref() {
            Some(principal) => self.member_app_access(principal)?,
            None => MemberAppAccess::default(),
        };
        if current_scope.as_ref() != Some(&scope)
            || current_principal != principal
            || current_access != access
            || self.private_job_denials_for_read(
                params,
                None,
                current_principal.as_ref(),
                peer_pid,
            )? != denied
        {
            return Err(Error::rejected(
                "master state refused: reader access changed",
            ));
        }
        // The relay gets the ack that this daemon applied the relayed-reader
        // boundary. A direct local read has no relay to check it; the field
        // is never part of a client's document and the board strips it.
        if relayed {
            state[READER_SCOPE_FIELD] = json!(MASTER_STATE_SCOPE_ACK);
        }
        Ok(state)
    }

    /// The `master_state` a project-scoped reader may see: the same
    /// provider/session configuration every reader gets, plus only the
    /// master turns that reader's own `thread_read` can actually prove.
    /// A relay is served an EMPTY master Home page, and a local agent's
    /// connection reads Home under its own project scope. A verified
    /// member additionally receives the exact app conversations an
    /// operator explicitly granted them (CAD-2215), and only those:
    /// `conversation` and `app_origin` are emitted only when the turn's
    /// conversation is one of the resolved exact bindings. `summary`,
    /// `session` and `context` describe the operator's text and the whole
    /// provider session, so they are never reported here.
    fn scoped_master_state(
        &self,
        scope: &super::identity::ProjectScope,
        denied: &store::PrivateJobDenials,
        relay: bool,
        access: &MemberAppAccess,
    ) -> Result<Value> {
        let alias = crate::master::ALIAS;
        let header = self.master_header(false)?;
        let (readable, projects) = scope;
        // The issue bindings this reader's policy admits — the same set the
        // scoped thread read hands its filter. The granted conversations are
        // a separate, exact set, never an issue/project substitute.
        let allowed = projects
            .iter()
            .filter(|(_, holders)| holders.iter().any(|project| readable.contains(project)))
            .map(|(issue, _)| issue.as_str())
            .collect::<Vec<_>>();
        let conversations = access.conversations();
        let mut threads: std::collections::HashMap<String, (bool, bool)> = Default::default();
        let mut waiting = Vec::new();
        for (id, state, created, thread) in self.store.queued_metadata_scoped_with_conversations(
            alias,
            &allowed,
            denied,
            &conversations,
        )? {
            // No enqueue note in a thread of this alias: no page can prove
            // this turn, so it is not reported at all.
            let Some(thread) = thread else { continue };
            let (home, granted) = match threads.get(&thread) {
                Some(cached) => *cached,
                None => {
                    let home = self
                        .store
                        .thread_by_id(&thread)?
                        .is_some_and(|t| t.runs_as_home());
                    let granted = access.conversation(&thread);
                    threads.insert(thread.clone(), (home, granted));
                    (home, granted)
                }
            };
            let conversation = (granted && !home).then(|| thread.clone());
            if !self.turn_page_readable(home, relay) && conversation.is_none() {
                continue;
            }
            waiting.push((id, state == "queued", created, conversation));
        }
        let running = match self.running_message_with_grants(alias, &allowed, denied, access)? {
            Some((message, thread)) => {
                let home = thread.as_ref().is_some_and(|t| t.runs_as_home());
                let conversation = thread
                    .as_ref()
                    .map(|t| t.id.clone())
                    .filter(|id| access.conversation(id));
                let readable = self.turn_page_readable(home, relay) || conversation.is_some();
                readable.then(|| {
                    let working = message.state == "running";
                    (
                        message.id.clone(),
                        working,
                        if working {
                            message.started.unwrap_or(message.created)
                        } else {
                            message.created
                        },
                        conversation,
                    )
                })
            }
            None => None,
        };
        let turn = match &running {
            Some((id, working, since, conversation)) => Some(MasterTurn {
                state: if *working {
                    "working".to_string()
                } else {
                    "queued".to_string()
                },
                message: id.clone(),
                conversation: (*conversation).clone(),
                app_origin: conversation
                    .as_ref()
                    .and_then(|_| self.granted_turn_app_origin(access, id)),
                // CAD-1767: the operator's own text is the operator's.
                summary: None,
                since: *since,
                // A global position is not this reader's data.
                position: None,
            }),
            None => waiting
                .first()
                .map(|(id, _, created, conversation)| MasterTurn {
                    state: "queued".to_string(),
                    message: id.clone(),
                    conversation: (*conversation).clone(),
                    app_origin: conversation
                        .as_ref()
                        .and_then(|_| self.granted_turn_app_origin(access, id)),
                    summary: None,
                    since: *created,
                    position: None,
                }),
        };
        let queue = waiting
            .iter()
            .map(|(id, _, _, conversation)| MasterQueued {
                message: id.clone(),
                conversation: (*conversation).clone(),
            })
            .collect::<Vec<_>>();
        let queued = waiting.iter().filter(|(_, queued, _, _)| *queued).count() as i64;
        Ok(serde_json::to_value(MasterState {
            alias: alias.to_string(),
            identity: self.master_identity()?,
            actor_identity: None,
            provider: header.provider,
            endpoint_kind: header.endpoint_kind,
            live: header.live,
            model: header.model,
            model_label: header.model_label,
            effort: header.effort,
            // Withheld, not narrowed: these describe the whole session.
            context: None,
            session: None,
            commands: header.commands,
            turn,
            queue,
            queued,
        })
        .expect("master state serializes"))
    }

    /// The reader's in-flight master turn together with the conversation its
    /// enqueue note landed in. Admission comes from the one scoped query,
    /// whose granted-conversation branch enforces every private-job, project
    /// and cloud denial itself.
    fn running_message_with_grants(
        &self,
        alias: &str,
        issues: &[&str],
        denied: &store::PrivateJobDenials,
        access: &MemberAppAccess,
    ) -> Result<Option<(store::Message, Option<store::Thread>)>> {
        let conversations = access.conversations();
        let Some(message) = self.store.running_message_scoped_with_conversations(
            alias,
            issues,
            denied,
            &conversations,
        )?
        else {
            return Ok(None);
        };
        let thread = self.store.message_conversation(alias, &message.id)?;
        Ok(Some((message, thread)))
    }

    /// The app origin of a granted master turn, but only when the derived
    /// origin is one of the exact bindings the grant covers. No install or
    /// project visibility is ever inferred into an origin.
    fn granted_turn_app_origin(
        &self,
        access: &MemberAppAccess,
        message_id: &str,
    ) -> Option<AppOrigin> {
        let origin = self.master_turn_app_origin(message_id)?;
        access.allows_origin(&origin).then_some(origin)
    }

    /// Can this reader's OWN `thread_read` reach the page a master turn
    /// landed in? Only the master's Home page, and only for a local agent:
    /// a relay is served an EMPTY Home page. An app conversation is
    /// admitted only when it is an exact conversation a verified member's
    /// grant covers (CAD-2215); a project-readable issue and an existing
    /// installation are not a grant, so an ungranted app turn is withheld
    /// WHOLE — not just its `app_origin`.
    fn turn_page_readable(&self, home: bool, relay: bool) -> bool {
        home && !relay
    }

    /// The one derivation of the master's provider/session configuration,
    /// shared by the operator document and the scoped projection so the two
    /// cannot drift. `with_stats` is the caller's disclosure: a scoped
    /// reader never receives `context`, so the whole-session `stats` read is
    /// not made for it at all.
    fn master_header(&self, with_stats: bool) -> Result<MasterHeader> {
        let alias = crate::master::ALIAS;
        let agent = self.store.agent(alias).map_err(|_| {
            Error::rejected("the master is not started — `cadence master start` registers it")
        })?;
        let adapter = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .get(alias)
            .and_then(|ctl| ctl.adapter.lock().unwrap().clone());
        let commands = adapter
            .as_ref()
            .map(|a| a.session_commands())
            .unwrap_or(&[]);
        // Live session fields win over the stored launch params when the
        // provider answers `state`/`stats` (Pi does; a stopped or
        // answering-less provider leaves the stored values standing).
        let live = adapter.is_some();
        let session = commands
            .contains(&"state")
            .then(|| {
                adapter
                    .as_ref()
                    .and_then(|a| a.session_command("state", None).ok())
            })
            .flatten();
        let stats = with_stats
            .then(|| {
                commands
                    .contains(&"stats")
                    .then(|| {
                        adapter
                            .as_ref()
                            .and_then(|a| a.session_command("stats", None).ok())
                    })
                    .flatten()
            })
            .flatten();
        let model = session
            .as_ref()
            .and_then(|s| s.get("model"))
            .cloned()
            .filter(|m| !m.is_null())
            .map(|m| {
                json!({
                    "id": m.get("id").or_else(|| m.get("name")).cloned().unwrap_or(Value::Null),
                    "name": m.get("name").cloned().unwrap_or(Value::Null),
                    "provider": m.get("provider").cloned().unwrap_or(Value::Null),
                })
            });
        let configured = |key: &str| {
            agent
                .params
                .as_ref()
                .and_then(|p| p.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let model_id = model
            .as_ref()
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.model.clone())
            .or_else(|| configured("model"));
        let effort = session
            .as_ref()
            .and_then(|s| s.get("thinkingLevel"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.effort.clone())
            .or_else(|| configured("effort"));
        let context = stats
            .as_ref()
            .and_then(|s| s.get("contextUsage"))
            .map(|u| MasterContext {
                tokens: u.get("tokens").cloned().unwrap_or(Value::Null),
                window: u.get("contextWindow").cloned().unwrap_or(Value::Null),
                percent: u.get("percent").cloned().unwrap_or(Value::Null),
            });
        let session = session.as_ref().map(|s| MasterSession {
            id: s.get("sessionId").cloned().unwrap_or(Value::Null),
            messages: s.get("messageCount").cloned().unwrap_or(Value::Null),
            streaming: s.get("isStreaming").cloned().unwrap_or(Value::Null),
            compacting: s.get("isCompacting").cloned().unwrap_or(Value::Null),
        });
        Ok(MasterHeader {
            provider: agent.provider.clone(),
            endpoint_kind: agent.endpoint_kind.clone(),
            live,
            model: model_id,
            model_label: model
                .as_ref()
                .and_then(|m| m.get("name").or_else(|| m.get("id")))
                .cloned()
                .unwrap_or(Value::Null),
            effort,
            commands: commands.iter().map(|c| (*c).to_string()).collect(),
            session,
            context,
        })
    }

    /// The unscoped `master_state` document: what the board's master header
    /// shows — the provider, the live session's model/effort/context (when
    /// the provider answers — Pi's `get_state`/`get_session_stats`), the
    /// verbs its session commands accept, and the turn in flight or
    /// queued. Read-only like `agent_show`; a master without a live
    /// endpoint still answers with its stored fields. Operator-only: the
    /// settings-gated `master.status` reuses it, and [`Self::rpc_master_state`]
    /// is the only request path that can reach a non-operator, always
    /// through its scoped projection.
    pub(super) fn master_state_document(&self) -> Result<Value> {
        let alias = crate::master::ALIAS;
        let header = self.master_header(true)?;
        // The turn the working row mirrors: `running` first, else the
        // queue head. `since` is epoch seconds — `started` while it
        // runs, `created` while it waits.
        // CAD-1098: the conversation the turn belongs to, so a panel can
        // say "queued" while another conversation's turn runs.
        let conversation_of = |id: &str| -> Option<String> {
            self.store
                .message_conversation(alias, id)
                .ok()
                .flatten()
                .map(|t| t.id)
        };
        let running = self.store.running_message(alias)?;
        // CAD-1700: the complete ordered queue behind the board's master
        // pane, as routing metadata only — never message bodies, never a
        // 20-row page. `queued_metadata` uses `queued_head`'s delivery
        // order, so the head is position 1.
        let queued = self.store.queued_metadata(alias)?;
        // Only a queued turn needs the head's body and start time; one row
        // read, never the whole queue's.
        let head = match &running {
            Some(_) => None,
            None => self.store.queued_head(alias)?,
        };
        let turn = match &running {
            Some(m) => Some(MasterTurn {
                state: "working".into(),
                message: m.id.clone(),
                conversation: conversation_of(&m.id),
                app_origin: self.master_turn_app_origin(&m.id),
                summary: Some(m.body.clone()),
                since: m.started.unwrap_or(m.created),
                position: None,
            }),
            None => head.as_ref().map(|m| {
                let conversation = match queued.iter().find(|(id, _)| id == &m.id) {
                    Some((_, thread)) => thread.clone(),
                    // The head left the queue between the two reads:
                    // report its own conversation, never a neighbour's.
                    None => conversation_of(&m.id),
                };
                MasterTurn {
                    state: "queued".into(),
                    message: m.id.clone(),
                    conversation,
                    app_origin: self.master_turn_app_origin(&m.id),
                    summary: Some(m.body.clone()),
                    since: m.created,
                    position: Some(1),
                }
            }),
        };
        let queue = queued
            .iter()
            .map(|(id, conversation)| MasterQueued {
                message: id.clone(),
                conversation: conversation.clone(),
            })
            .collect::<Vec<_>>();
        Ok(serde_json::to_value(MasterState {
            alias: alias.to_string(),
            identity: self.master_identity()?,
            actor_identity: None,
            provider: header.provider,
            endpoint_kind: header.endpoint_kind,
            live: header.live,
            model: header.model,
            model_label: header.model_label,
            effort: header.effort,
            context: header.context,
            session: header.session,
            commands: header.commands,
            turn,
            queue,
            queued: self.store.queued_count(alias)?,
        })
        .expect("master state serializes"))
    }

    /// Best-effort navigation metadata for the exact persisted master turn.
    /// This projection proves the current app and conversation while the
    /// runtime snapshot is held; it is not an authorization or revocation-atomic grant.
    fn master_turn_app_origin(&self, message_id: &str) -> Option<AppOrigin> {
        let alias = crate::master::ALIAS;
        let thread = self
            .store
            .message_conversation(alias, message_id)
            .ok()
            .flatten()?;
        let install = thread
            .install_id
            .as_deref()
            .filter(|install| !install.is_empty())?;
        if thread.alias != alias
            || thread.archived
            || thread.runs_as_home()
            || thread.id.is_empty()
            || thread.subject.is_some()
        {
            return None;
        }

        let pm_dir = self.pm_dir().ok()?;
        let pm = self.pm_at(&pm_dir).ok()?;
        let conversation_id = thread.id.clone();
        crate::issue::app_catalog::workspace::with_runtime_read(&pm, install, |row, _files| {
            let Some(digest) = row.get("digest").and_then(Value::as_str) else {
                return Ok(None);
            };
            if row.get("install_id").and_then(Value::as_str) != Some(install) {
                return Ok(None);
            }
            let status = self.store.app_capability_status(install, digest)?;
            if status.get("state").and_then(Value::as_str) != Some("approved") {
                return Ok(None);
            }

            // Re-read both edges under the held runtime snapshot. The
            // first lookup binds this exact message to the same live
            // master conversation. Subject/campaign conversations were
            // refused above: this selector reaches RecordStore::open for
            // those, which may initialize or migrate persistent storage.
            let current_turn = match self.store.running_message(alias)? {
                Some(message) => Some(message),
                None => self.store.queued_head(alias)?,
            };
            if current_turn.as_ref().map(|message| message.id.as_str()) != Some(message_id) {
                return Ok(None);
            }
            let Some(current_thread) = self.store.message_conversation(alias, message_id)? else {
                return Ok(None);
            };
            if current_thread.id != conversation_id
                || current_thread.alias != alias
                || current_thread.archived
                || current_thread.runs_as_home()
                || current_thread.subject.is_some()
                || current_thread.install_id.as_deref() != Some(install)
            {
                return Ok(None);
            }
            // Persisted provenance: the enqueue entry's own send-time
            // stamp. A context stamp is re-proved against the live store;
            // an installation-only stamp (an app chat with no context) is
            // a valid destination with an empty context.
            let Some(stamp) = self.store.message_app_stamp(message_id)? else {
                return Ok(None);
            };
            if stamp.get("verified") != Some(&Value::Bool(true))
                || stamp.get("install_id").and_then(Value::as_str) != Some(install)
            {
                return Ok(None);
            }
            let context = if stamp.get("context_id").is_some() {
                let Some(app) = self.store.message_app(message_id)? else {
                    return Ok(None);
                };
                let Some(context) = app
                    .get("context_id")
                    .and_then(Value::as_str)
                    .filter(|context| !context.is_empty())
                else {
                    return Ok(None);
                };
                if app.get("install_id").and_then(Value::as_str) != Some(install)
                    || (current_thread.context_id.as_deref() != Some(context)
                        && !(current_thread.is_general && current_thread.context_id.is_none()))
                {
                    return Ok(None);
                }
                self.store.app_context_proof(install, context)?;
                context.to_string()
            } else {
                String::new()
            };
            let app = json!({
                "install_id": install,
                "context_id": (!context.is_empty()).then_some(context.as_str()),
            });
            self.verify_conversation_selector(alias, &app, &conversation_id)?;
            Ok(Some(AppOrigin {
                install_id: install.to_string(),
                context_id: context,
                conversation_id: conversation_id.clone(),
            }))
        })
        .ok()
        .flatten()
    }

    /// `master_models` (CAD-575) — the operator's per-role read of the
    /// master's model vocabulary, backing the board's model picker:
    /// the live session's model/effort, every model pi offers (plus
    /// the policy-listed ids it did not offer) annotated with which
    /// roles may run it (`allowed_for`), the Devin cost tier, and the
    /// session's thinking-effort levels. Operator-only by connection;
    /// the verb takes no parameters at all — an identity-shaped field
    /// is refused by [`Self::operator_connection`], anything else here,
    /// so a read that grows knobs later never inherits silent
    /// authority.
    ///
    /// It degrades, never blocks: a stopped (or never registered)
    /// master answers from its stored row with `efforts` empty and
    /// `models` drawn from the policy lists alone; the Devin catalog
    /// comes from the pi-devin cache file or a bounded, memoized
    /// `devin models list` — every miss is `cost_tier: "unknown"`.
    pub(super) fn rpc_master_models(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("master models", params, peer_pid)?;
        if let Some(extra) = params.as_object().and_then(|o| o.keys().next()) {
            return Err(Error::rejected(format!(
                "master models takes no parameters — '{extra}' is refused (CAD-575)"
            )));
        }
        let alias = crate::master::ALIAS;
        let agent = self.store.agent(alias).ok();
        let adapter = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .get(alias)
            .and_then(|ctl| ctl.adapter.lock().unwrap().clone());
        let live = adapter.is_some();
        let commands = adapter
            .as_ref()
            .map(|a| a.session_commands())
            .unwrap_or(&[]);
        let ask = |command: &str| {
            commands
                .contains(&command)
                .then(|| {
                    adapter
                        .as_ref()
                        .and_then(|a| a.session_command(command, None).ok())
                })
                .flatten()
        };
        let session = ask("state");
        let offered = ask("models");
        let levels = ask("levels");
        // The policy's per-role vocabularies annotate every offered
        // model and supply the rows the provider never offered — a
        // missing `[pi]` is two empty lists, like the launch gates.
        let policy = self.stored_pi_policy()?;
        let (master_list, worker_list) = policy
            .as_ref()
            .map(|p| {
                (
                    p.models.allow_for("master").to_vec(),
                    p.models.allow_for("worker").to_vec(),
                )
            })
            .unwrap_or_default();
        let allowed_for = |id: &str| -> Vec<&'static str> {
            let mut roles = Vec::new();
            if crate::pi_policy::require_allowed(policy.as_ref(), "master", id).is_ok() {
                roles.push("master");
            }
            if crate::pi_policy::require_allowed(policy.as_ref(), "worker", id).is_ok() {
                roles.push("worker");
            }
            roles
        };
        // The session's own list first — it is what the master could
        // switch to live — then the policy-listed ids the provider did
        // not offer; the allowlist stays the vocabulary the gate
        // enforces even when the provider's list is shorter.
        let mut order: Vec<String> = Vec::new();
        let mut labels: HashMap<String, String> = HashMap::new();
        for m in offered
            .as_ref()
            .and_then(|o| o.get("models"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = m.get("id").and_then(Value::as_str) else {
                continue;
            };
            let provider = m
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let full = if provider.is_empty() {
                id.to_string()
            } else {
                format!("{provider}/{id}")
            };
            if !order.contains(&full) {
                order.push(full.clone());
            }
            if let Some(name) = m.get("name").and_then(Value::as_str) {
                labels.entry(full).or_insert_with(|| name.to_string());
            }
        }
        for id in master_list.iter().chain(worker_list.iter()) {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        let catalog = self.devin_catalog.catalog(&self.provider_env);
        let entries: Vec<Value> = order
            .iter()
            .map(|id| {
                let (provider, uid) = id.split_once('/').unwrap_or(("", id.as_str()));
                let (cost_tier, catalog_label) = match provider {
                    "devin" => catalog
                        .as_ref()
                        .and_then(|c| c.lookup(uid))
                        .map(|(tier, label)| (tier.clone(), label.clone()))
                        .unwrap_or(("unknown".to_string(), None)),
                    "openrouter" => ("Paid".to_string(), None),
                    _ => ("unknown".to_string(), None),
                };
                json!({
                    "id": id,
                    "label": labels
                        .get(id)
                        .cloned()
                        .or(catalog_label)
                        .unwrap_or_else(|| id.clone()),
                    "cost_tier": cost_tier,
                    "allowed_for": allowed_for(id),
                })
            })
            .collect();
        // Live session fields win over the stored launch params, the
        // same precedence `master_state` uses — but `model` here is the
        // full `provider/id` the allowlists speak.
        let live_model = session.as_ref().and_then(|s| s.get("model")).and_then(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("name"))
                .and_then(Value::as_str)?;
            let provider = m.get("provider").and_then(Value::as_str);
            Some(match provider {
                Some(p) if !p.is_empty() => format!("{p}/{id}"),
                _ => id.to_string(),
            })
        });
        let configured = |key: &str| {
            agent
                .as_ref()
                .and_then(|a| a.params.as_ref())
                .and_then(|p| p.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let current_model = live_model
            .or_else(|| agent.as_ref().and_then(|a| a.model.clone()))
            .or_else(|| configured("model"));
        let effort = session
            .as_ref()
            .and_then(|s| s.get("thinkingLevel"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.as_ref().and_then(|a| a.effort.clone()))
            .or_else(|| configured("effort"));
        let efforts: Vec<Value> = levels
            .as_ref()
            .and_then(|l| l.get("levels"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(json!({
            "current": {"model": current_model, "effort": effort},
            "models": entries,
            "efforts": efforts,
            "live": live,
        }))
    }

    /// `master_command` (CAD-551) — the operator's provider-session
    /// commands for the master, behind the board's slash menu and its
    /// Stop control. The daemon owns the verb set
    /// ([`MASTER_COMMANDS`]): `stop` is the ordinary `interrupt` path,
    /// every other verb maps through the adapter's declared
    /// [`ProviderAdapter::session_commands`] — never a raw provider
    /// command passthrough. Operator-only by connection; identity
    /// fields are refused. Every call — refused included — leaves a
    /// `master_command` event; a mutation also writes a durable
    /// `system` line on the master's thread.
    pub(super) fn rpc_master_command(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("master command", params, peer_pid)?;
        let command = required_str(params, "command")?;
        let arg = optional_str(params, "arg")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let alias = crate::master::ALIAS;
        let audit = |outcome: &str, error: Option<&str>| {
            let mut payload = json!({"command": command, "by": "operator", "by_kind": "operator",
                                     "outcome": outcome});
            if let Some(arg) = arg {
                payload["arg"] = json!(arg);
            }
            if let Some(error) = error {
                payload["error"] = json!(error);
            }
            let _ = self.store.event_public(alias, "master_command", payload);
        };
        // CAD-1700: `message` scopes `stop` and `cancel` to one master turn.
        // It is refused on every other verb, so it cannot widen one. Each
        // refusal leaves a `master_command` event, like every other call.
        let message = match params.get("message") {
            None => None,
            Some(value) => {
                if command != "stop" && command != "cancel" {
                    let error =
                        format!("'/{command}' takes no message - only /stop and /cancel do");
                    audit("refused", Some(&error));
                    return Err(Error::invalid("message_not_allowed", error));
                }
                match value.as_str() {
                    Some(id) if !id.is_empty() && id.chars().count() <= 128 => Some(id),
                    _ => {
                        let error = "message must be a non-empty string of at most 128 characters";
                        audit("refused", Some(error));
                        return Err(Error::invalid("message_invalid", error));
                    }
                }
            }
        };
        if command == "help" {
            audit("ok", None);
            return Ok(json!({"command": command, "ok": true,
                             "result": {"commands": MASTER_COMMANDS}}));
        }
        if !MASTER_COMMANDS.contains(&command) {
            audit("refused", Some("unknown command"));
            return Err(Error::invalid(
                "unknown_command",
                format!(
                    "unknown master command '/{command}' — the board allows: {}",
                    MASTER_COMMANDS
                        .iter()
                        .map(|c| format!("/{c}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            ));
        }
        if command == "stop" {
            // The daemon's interrupt path: same authorization, audit and
            // stale-turn protection `cadence interrupt` has. `wait`
            // bounds how long the answer waits for the settle. With a
            // `message`, the stop is bound to that exact running turn (CAD-1700).
            let mut p = json!({"alias": alias});
            if let Some(wait) = params.get("wait") {
                p["wait"] = wait.clone();
            }
            let out = match message {
                Some(message) => {
                    let running = self.store.running_message(alias);
                    if running
                        .as_ref()
                        .ok()
                        .and_then(|m| m.as_ref())
                        .map(|m| m.id.as_str())
                        != Some(message)
                    {
                        let error = format!(
                            "message {message} is not the master's running turn — nothing was stopped"
                        );
                        audit("refused", Some(&error));
                        return Err(Error::invalid("turn_not_running", error));
                    }
                    self.rpc_interrupt_bound(&p, peer_pid, Some(message))
                }
                None => self.rpc_interrupt(&p, peer_pid),
            };
            match &out {
                Ok(_) => audit("ok", None),
                Err(e) => audit("refused", Some(&e.to_string())),
            }
            return out.map(|v| json!({"command": command, "ok": true, "result": v}));
        }
        if command == "cancel" {
            // CAD-1700: cancel exactly one still-queued operator message to the
            // master. A running or submitting turn is never cancelled here.
            let Some(id) = message else {
                audit("refused", Some("message_required"));
                return Err(Error::invalid(
                    "message_required",
                    "cancel needs the queued message id - `message` is required",
                ));
            };
            let refuse = |code: &'static str, text: String| {
                audit("refused", Some(&text));
                Err(Error::invalid(code, text))
            };
            let found = match self.store.message(id)? {
                // CAD-1700: the source string alone is caller-supplied, so
                // it is not provenance. The store's canonical sender label is
                // the proof: only `Sender::OperatorChat` records `operator`;
                // an agent records its alias (with a uid), a member or CLI
                // principal its own label.
                Some(m)
                    if m.alias == alias
                        && m.source == "operator"
                        && m.sender.as_deref() == Some("operator")
                        // An agent may literally be named `operator`; the
                        // canonical uid is what tells it from the operator's
                        // own chat (only the agent row has one).
                        && m.sender_uid.is_none()
                        && m.reply_to.is_none() =>
                {
                    m
                }
                _ => {
                    return refuse(
                        "message_not_cancellable",
                        format!("message {id} is not an operator message queued to the master"),
                    )
                }
            };
            match found.state.as_str() {
                "queued" => {}
                "running" | "submitting" => {
                    return refuse(
                        "message_running",
                        format!("message {id} is already running - use Stop"),
                    )
                }
                other => {
                    return refuse(
                        "message_not_queued",
                        format!(
                            "message {id} is '{other}' - only a queued message can be cancelled"
                        ),
                    )
                }
            }
            let cancelled = self
                .store
                .cancel(id, "operator", Some("cancelled from the board"))?;
            if let Some(result) = cancelled.result.clone() {
                self.notify_routed_target(&cancelled, &result);
            }
            audit("ok", None);
            self.wake();
            return Ok(json!({"command": command, "ok": true,
                             "result": {"message": cancelled.to_json()}}));
        }
        let agent = match self.store.agent(alias) {
            Ok(a) => a,
            Err(_) => {
                audit("refused", Some("the master is not started"));
                return Err(Error::rejected(
                    "the master is not started — no agent 'master'",
                ));
            }
        };
        let adapter = match self.adapter_for(alias) {
            Ok(a) => a,
            Err(e) => {
                audit("refused", Some(&e.to_string()));
                return Err(e);
            }
        };
        if !adapter.session_commands().contains(&command) {
            let error = format!(
                "the '/{command}' command is not supported by {}",
                agent.provider
            );
            audit("refused", Some(&error));
            return Err(Error::rejected(error));
        }
        match adapter.session_command(command, arg) {
            Ok(result) => {
                audit("ok", None);
                if command == "model" {
                    // CAD-559: the verified switch is the launch model
                    // from now on — the next open's re-check must see it
                    // in params.model, and the reported column agrees.
                    if let Some(model) = result.get("requested").and_then(Value::as_str) {
                        if let Err(e) = self.store.set_params(alias, &json!({"model": model})) {
                            eprintln!("master_command params.model for '{alias}' failed: {e}");
                        }
                        if let Err(e) = self.store.set_model_reported(alias, model) {
                            eprintln!("master_command model report for '{alias}' failed: {e}");
                        }
                    }
                }
                if let Some(line) = master_command_line(command, &result) {
                    if let Err(e) = self.store.thread_append(
                        alias,
                        store::NewEntry {
                            role: store::ROLE_SYSTEM,
                            kind: store::KIND_MESSAGE,
                            text: &line,
                            payload: Some(json!({
                                "command": format!("/{command}"),
                                "arg": arg,
                            })),
                            message_id: None,
                        },
                    ) {
                        eprintln!("master_command thread line for '{alias}' failed: {e}");
                    }
                }
                self.wake();
                Ok(json!({"command": command, "ok": true, "result": result}))
            }
            Err(e) => {
                audit("refused", Some(&e.to_string()));
                Err(e)
            }
        }
    }
}

/// The `master_command` verbs (CAD-551) — the only session commands the
/// operator can run against the master, named in the daemon so a relay
/// (the board's `/api/master/command`) can never widen them. `stop`
/// runs the `interrupt` path; `help` answers the list itself; the rest
/// are adapter verbs.
const MASTER_COMMANDS: &[&str] = &[
    "state", "stats", "models", "model", "levels", "effort", "compact", "new", "stop", "cancel",
    "help",
];

/// The one-line `system` thread note a `master_command` mutation
/// leaves — the durable "who changed what" beside the `master_command`
/// audit event. Queries write nothing to the thread.
fn master_command_line(command: &str, result: &Value) -> Option<String> {
    match command {
        "model" => {
            let model = result
                .pointer("/model/id")
                .or_else(|| result.pointer("/model/name"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let was = result.get("was").and_then(Value::as_str);
            Some(match was {
                Some(old) => format!("Model → {model} (was {old})"),
                None => format!("Model → {model}"),
            })
        }
        "effort" => result
            .get("level")
            .and_then(Value::as_str)
            .map(|level| format!("Effort → {level}")),
        "compact" => Some("Session context compacted".to_string()),
        "new" => Some("Provider session restarted".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod cad1700_projection_tests {
    use super::*;
    use crate::store::NewAgent;
    use crate::test_seam::{scoped, Asserted};

    fn fixture() -> (tempfile::TempDir, Arc<Shared>) {
        let dir = tempfile::tempdir().unwrap();
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("no-pm").to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "master",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        (dir, shared)
    }

    /// The master pane looks for its own queued message and what is ahead
    /// of it in `master_state.queue`. The queue is complete in exact
    /// delivery order, so the 21st queued message and its predecessor are
    /// still represented, and the head row keeps its turn semantics
    /// (state, message, summary, since, position). Red before CAD-1700 P2:
    /// the projection stopped at 20 rows.
    #[test]
    fn cad1700_master_state_queue_is_complete_past_twenty() {
        let (_dir, shared) = fixture();
        let pid = std::process::id();
        // Send admission (launch/model gates) is not under test: seed the
        // canonical rows a Home send produces. `enqueue` itself writes the
        // operator thread note when the alias has a home thread (the same
        // `thread_note_enqueued` path every queued message takes), and that
        // note carries the message id, so each row resolves its home
        // conversation.
        shared.store.ensure_thread("master").unwrap();
        for i in 1..=25 {
            let id = format!("m{i}");
            shared
                .store
                .enqueue("master", &format!("hi {id}"), None, &id, "operator")
                .unwrap();
        }
        let state = scoped(Asserted::Operator, || {
            shared.dispatch("master_state", &json!({}), pid)
        })
        .unwrap();
        let queue = state["queue"].as_array().unwrap();
        let ids = queue
            .iter()
            .map(|row| row["message"].as_str().unwrap())
            .collect::<Vec<_>>();
        let expected = (1..=25).map(|i| format!("m{i}")).collect::<Vec<_>>();
        assert_eq!(
            ids,
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "the queue is complete and in delivery order"
        );
        assert_eq!(state["queued"], json!(25));
        assert!(ids.contains(&"m20"), "the 20th message is represented");
        assert!(ids.contains(&"m21"), "the 21st message is represented");
        assert_eq!(state["turn"]["state"], json!("queued"));
        assert_eq!(state["turn"]["message"], json!("m1"));
        assert_eq!(state["turn"]["summary"], json!("hi m1"));
        assert_eq!(state["turn"]["position"], json!(1));
        let home = shared.store.thread("master").unwrap().unwrap().id;
        for row in queue {
            assert_eq!(
                row["conversation"],
                json!(home),
                "each row's conversation comes from the one projection"
            );
        }
    }
}
