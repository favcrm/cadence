//! CAD-534: `cadence daemon` threads RPC handlers — moved verbatim from src/daemon.rs.

use super::app_explorer_rpc::MemberAppAccess;
use super::scoped_cursor::{CursorBinding, CursorDirection};
use super::*;
use cadence_core::board_api::{
    ThreadEntryRow, ThreadPage, READER_SCOPE_FIELD, THREAD_CURSOR_FORMAT, THREAD_OPAQUE_SCOPE_ACK,
    THREAD_OPERATOR_SCOPE_ACK, THREAD_SCOPE_ACK,
};

struct AgentThreadRead<'a> {
    alias: &'a str,
    scope: &'a super::identity::ProjectScope,
    issues: &'a [&'a str],
    denials: &'a store::PrivateJobDenials,
    projection: super::cloud_agent_rpc::CloudProjection,
    conversation: Option<&'a store::Thread>,
}

impl Shared {
    /// The entries as JSON. A message-linked entry whose stored message
    /// belongs to this alias and to the entry's own thread carries
    /// `payload.delivery` (`state`, `started`, `completed`); its other
    /// payload keys are kept. Anything else gets no delivery.
    fn entries_json(
        &self,
        alias: &str,
        entries: &[store::ThreadEntry],
    ) -> Result<Vec<ThreadEntryRow>> {
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut row = entry.row();
            let Some(id) = &entry.message_id else {
                out.push(row);
                continue;
            };
            let Some(message) = self.store.message(id)?.filter(|m| m.alias == alias) else {
                out.push(row);
                continue;
            };
            let same_thread = self
                .store
                .message_conversation(alias, id)?
                .is_some_and(|t| t.id == entry.thread_id);
            let mut payload = match &entry.payload {
                None | Some(Value::Null) => serde_json::Map::new(),
                Some(Value::Object(obj)) => obj.clone(),
                Some(_) => {
                    out.push(row);
                    continue;
                }
            };
            if same_thread {
                payload.insert(
                    "delivery".into(),
                    json!({"state": message.state, "started": message.started, "completed": message.completed}),
                );
                row.payload = Some(Value::Object(payload));
            }
            out.push(row);
        }
        Ok(out)
    }

    /// CAD-1374: the verified human behind an owner write. The board
    /// sends `public_session` only for a public-host request, and refuses
    /// a public-host request that carries none, so an absent bearer is the
    /// operator's local write and gets no human. A present bearer is
    /// re-checked against the daemon's own live row, which must map to the
    /// owner role; the subject/session/company come from that row only.
    /// `public_session` is never a source of authority on its own.
    pub(super) fn verified_owner_human(&self, params: &Value) -> Result<Option<Value>> {
        // Only an ABSENT field means the local operator chat. A present
        // null, non-string or empty proof is malformed and refused, never
        // read as absent (that would downgrade a hosted request).
        let token = match params.get("public_session") {
            None => return Ok(None),
            Some(Value::String(token)) if !token.is_empty() => token.as_str(),
            Some(_) => {
                return Err(Error::rejected(
                    "thread send refused: the board session proof is malformed",
                ))
            }
        };
        let granted = match self.current_public_session(token)? {
            crate::daemon::public_membership::Authority::Granted {
                sub,
                role,
                realm,
                display,
                view,
                ..
            } => (sub, role, realm, display, view),
            crate::daemon::public_membership::Authority::Denied => {
                return Err(Error::rejected(
                    "thread send refused: the board session is not live",
                ))
            }
        };
        let (sub, role, realm, display, view) = granted;
        if !role.is_operator() {
            return Err(Error::rejected(
                "thread send is the board owner's chat; this session is a member",
            ));
        }
        Ok(Some(json!({
            "sub": sub,
            "company": realm.company,
            "session": view.id,
            "role": role.as_str(),
            "display": display,
        })))
    }

    fn authorize_thread_read_target(
        &self,
        alias: &str,
        params: &Value,
        principal: Option<&super::identity::PublicReadPrincipal>,
        peer_pid: u32,
    ) -> Result<()> {
        let withheld =
            self.member_agent_row_withheld_for_relay(alias, params, None, principal, peer_pid)?;
        if withheld {
            return Err(Error::rejected("Unknown managed agent"));
        }
        Ok(())
    }

    /// CAD-2215: does the live grant snapshot admit this exact master app
    /// conversation — the same installation, conversation id and pinned
    /// context? Nothing is inferred from the alias, the project scope or a
    /// sibling conversation of the same app, and no thread string is copied
    /// to ask.
    fn member_app_thread_admitted(
        access: &MemberAppAccess,
        thread: Option<&store::Thread>,
    ) -> bool {
        thread.is_some_and(|thread| {
            !thread.runs_as_home()
                && thread.install_id.as_deref().is_some_and(|install_id| {
                    access.admits(
                        &thread.id,
                        install_id,
                        thread.context_id.as_deref().unwrap_or(""),
                    )
                })
        })
    }

    /// CAD-2215 review: a scoped relay's read of a non-Home master app
    /// conversation is admitted only by that exact live grant. Without one —
    /// absent, revoked, or drifted in installation/digest/context — the read
    /// answers the same opaque refusal an unknown conversation gets, so no
    /// page, ack or selected-conversation detail leaks. This covers a
    /// verified member and a `read_as` relay (whose access is empty without a
    /// session, so an anonymous relay never widens into an app conversation),
    /// while an unscoped proven operator and a direct local agent keep their
    /// existing paths.
    fn require_scoped_app_read(
        alias: &str,
        thread: Option<&store::Thread>,
        access: &MemberAppAccess,
        scoped_relay: bool,
    ) -> Result<()> {
        if !scoped_relay || !crate::master::is_master(alias) {
            return Ok(());
        }
        match thread {
            Some(thread) if !thread.runs_as_home() => {
                if Self::member_app_thread_admitted(access, Some(thread)) {
                    Ok(())
                } else {
                    Err(Error::rejected("Unknown conversation"))
                }
            }
            _ => Ok(()),
        }
    }

    /// `thread_read` — a page of an agent's thread after `after`,
    /// optionally long-polling up to `wait` seconds (≤ 30) for the next
    /// entry, the way `agent_events` does. Read-only.
    pub(super) fn rpc_thread_read(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let (scope, principal) = self.thread_read_scope(params, peer_pid)?;
        // CAD-2215: a scoped relay gets the per-response ack the board
        // requires before it trusts any page (`thread_scope_confirmed`).
        // Every return path carries it, so no polled page is unproven.
        // An operator-role relay has `scope == None`, never gets an ack, and
        // the board exempts it (a full page is correct for it).
        let member_relay =
            scope.is_some() && (principal.is_some() || params.get("read_as").is_some());
        self.authorize_thread_read_target(&alias, params, principal.as_ref(), peer_pid)?;
        // CAD-2215 acceptance scaffolding: the real proof and target
        // admission above already ran; an armed flag now answers exactly as a
        // pre-CAD-2215 daemon that ignored the relayed proof would — the
        // genuine unscoped page, no ack. It is read on every call, so a live
        // member SSE can be toggled into refusal/EOF before any unscoped page
        // is forwarded. Operator reads never reach it (scope is None).
        #[cfg(test)]
        if member_relay
            && self
                .thread_read_ignore_relay
                .as_ref()
                .is_some_and(|armed| armed.load(std::sync::atomic::Ordering::Acquire))
        {
            let mut unscoped = params.clone();
            if let Some(fields) = unscoped.as_object_mut() {
                fields.remove("public_session");
                fields.remove("read_as");
            }
            return self.rpc_thread_read(&unscoped, peer_pid);
        }
        let private_denials = if scope.is_some() {
            self.private_job_denials_for_read(params, None, principal.as_ref(), peer_pid)?
        } else {
            Default::default()
        };
        let allowed_issues = scope.as_ref().map(|(readable, issues)| {
            issues
                .iter()
                .filter(|(_, project)| readable.contains(*project))
                .map(|(issue, _)| issue.as_str())
                .collect::<Vec<_>>()
        });
        // CAD-1853 review: a cloud target's request/reply text reaches this
        // projection, so it demands the same source/owner authority as the
        // dedicated cloud read — current READ authority for the caller's own
        // source, and only that source's rows — so a peer with no grant is
        // refused and an enrolled peer never sees another source's reply.
        let mut projection = self.admit_cloud_projection(&alias, peer_pid, "thread read")?;
        // CAD-1098: `conversation` selects one thread of this alias by
        // id (validated; unknown answers an error and creates nothing).
        // Without it the read is the home thread, unchanged.
        let conversation = match optional_text(params, "conversation")? {
            None => None,
            Some(id) => {
                crate::proto::identifier(id, "conversation ID")?;
                Some(
                    self.store
                        .thread_by_id(id)?
                        .filter(|t| t.alias == alias)
                        .ok_or_else(|| Error::rejected("Unknown conversation"))?,
                )
            }
        };
        // CAD-2215: exact app conversations this verified member may read.
        // A local agent, `read_as` relay or CLI principal never gets one.
        let member_access = match principal.as_ref() {
            Some(principal) => self.member_app_access(principal)?,
            None => MemberAppAccess::default(),
        };
        // Direct agent peers have no board relay to hide global thread
        // positions. Their contract is opaque and scoped at the daemon.
        if let (Some(scope), None, None) =
            (scope.as_ref(), principal.as_ref(), params.get("read_as"))
        {
            return self.rpc_agent_thread_opaque(
                params,
                peer_pid,
                AgentThreadRead {
                    alias: &alias,
                    scope,
                    issues: allowed_issues.as_deref().unwrap_or(&[]),
                    denials: &private_denials,
                    projection,
                    conversation: conversation.as_ref(),
                },
            );
        }
        if scope.is_none() && params.get("cursor_format").is_some() {
            if params.get("cursor_format").and_then(Value::as_str) != Some(THREAD_CURSOR_FORMAT) {
                return Err(Error::invalid(
                    "thread_cursor_format",
                    "unsupported thread cursor format",
                ));
            }
            for field in ["after", "before"] {
                if params.get(field).is_some_and(|value| !value.is_i64()) {
                    return Err(Error::invalid(
                        "thread_cursor_invalid",
                        "operator thread position must be numeric",
                    ));
                }
            }
        }
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Thread cursor must be nonnegative"));
        }
        let limit = optional_i64(params, "limit").unwrap_or(100);
        if !(1..=store::THREAD_PAGE_MAX).contains(&limit) {
            return Err(Error::rejected(format!(
                "Thread page limit must be 1-{}",
                store::THREAD_PAGE_MAX
            )));
        }
        // CAD-328: `tail` (the newest page) or `before` (the page below a
        // seq) read backwards for a chat view; neither waits.
        let tail = params.get("tail").and_then(Value::as_bool).unwrap_or(false);
        let before = optional_i64(params, "before");
        if tail || before.is_some() {
            if params.get("after").is_some() || params.get("wait").is_some() {
                return Err(Error::rejected(
                    "Thread read takes either after/wait (forward) or tail/before (backward)",
                ));
            }
            if before.is_some_and(|b| b < 1) {
                return Err(Error::rejected("Thread 'before' must be a positive seq"));
            }
            let thread = match &conversation {
                Some(thread) => Some(thread.clone()),
                None => self.store.thread(&alias)?,
            };
            Self::require_scoped_app_read(&alias, thread.as_ref(), &member_access, member_relay)?;
            let home = member_relay
                && crate::master::is_master(&alias)
                && thread.as_ref().is_none_or(store::Thread::runs_as_home);
            let (raw, more) = if home {
                (Vec::new(), false)
            } else {
                match (&thread, allowed_issues.as_deref()) {
                    (Some(thread), _) if member_access.conversation(&thread.id) => {
                        self.store.thread_entries_before_granted(
                            &thread.id,
                            before,
                            limit,
                            allowed_issues.as_deref().unwrap_or(&[]),
                            &private_denials,
                            member_relay,
                        )?
                    }
                    (Some(thread), Some(issues)) => self.store.thread_entries_before_scoped(
                        &thread.id,
                        before,
                        limit,
                        issues,
                        &private_denials,
                        member_relay,
                    )?,
                    (Some(thread), None) => self
                        .store
                        .thread_entries_before_of(&thread.id, before, limit)?,
                    (None, _) => (Vec::new(), false),
                }
            };
            // The page is observed before it is projected, so anything the
            // admission snapshot does not cover is re-resolved here — before
            // the boundary and the rows are decided, never after. A backward
            // page can carry a routed cloud result or a cloud operation that
            // resolved after this read was admitted, and authority was never
            // proven for it.
            if projection.needs_readmission(
                &self.store,
                raw.iter().filter_map(|entry| entry.message_id.as_deref()),
            )? {
                projection = self.admit_cloud_projection(&alias, peer_pid, "thread read")?;
            }
            // The cursor is the last row this page SCANNED, never the last
            // row the projection kept. A projection filters another source's
            // cloud entries out; taking the kept page's last seq would pin
            // the caller's cursor short of rows it must move past, and the
            // filtered content is exactly what must not be exposed.
            let cursor = raw.last().map(|e| e.seq).unwrap_or(0);
            // A backward scan needs the same honesty in the other direction:
            // the LOWEST seq this page scanned is the continuation point. A
            // partially (or wholly) filtered page keeps fewer rows than it
            // scanned, so `entries[0]` may not move at all; without this
            // boundary the consumer would re-read the same hidden page
            // forever and never reach the history below it.
            let scanned_before = raw.first().map(|e| e.seq);
            let entries = projection.entries(&self.store, raw)?;
            // CAD-1700: the complete watermark of the authorized thread,
            // never just this (bounded) page or the cursor's window.
            let reply_seq = if home {
                0
            } else {
                match (&thread, allowed_issues.as_deref()) {
                    (Some(thread), _) if member_access.conversation(&thread.id) => {
                        self.store.thread_reply_seq_granted(
                            &thread.id,
                            allowed_issues.as_deref().unwrap_or(&[]),
                            &private_denials,
                        )?
                    }
                    (Some(thread), Some(issues)) => {
                        self.store
                            .thread_reply_seq_scoped(&thread.id, issues, &private_denials)?
                    }
                    (Some(thread), None) => self.store.thread_reply_seq(&thread.id)?,
                    (None, _) => 0,
                }
            };
            let (current_scope, current_principal) = self.thread_read_scope(params, peer_pid)?;
            let current_access = match current_principal.as_ref() {
                Some(principal) => self.member_app_access(principal)?,
                None => MemberAppAccess::default(),
            };
            if current_scope != scope
                || current_principal != principal
                || current_access != member_access
                || (scope.is_some()
                    && self.private_job_denials_for_read(
                        params,
                        None,
                        current_principal.as_ref(),
                        peer_pid,
                    )? != private_denials)
            {
                return Err(Error::rejected(
                    "thread read refused: reader access changed",
                ));
            }
            Self::require_scoped_app_read(&alias, thread.as_ref(), &current_access, member_relay)?;
            self.authorize_thread_read_target(
                &alias,
                params,
                current_principal.as_ref(),
                peer_pid,
            )?;
            let mut page = serde_json::to_value(ThreadPage {
                alias: alias.clone(),
                thread: if scope.is_some() {
                    None
                } else {
                    thread.as_ref().map(store::Thread::info)
                },
                entries: self.entries_json(&alias, &entries)?,
                cursor,
                more_before: Some(more),
                scanned_before,
                more_after: None,
                cloud_withheld: if private_denials.exclude_cloud {
                    None
                } else {
                    projection.withheld
                },
                reply_seq,
            })
            .expect("thread page serializes");
            if member_relay && crate::master::is_master(&alias) && !home {
                page["_member_app"] = json!(true);
            }
            if member_relay {
                page[READER_SCOPE_FIELD] = json!(THREAD_SCOPE_ACK);
            }
            if scope.is_none() && params.get("cursor_format").is_some() {
                page[READER_SCOPE_FIELD] = json!(THREAD_OPERATOR_SCOPE_ACK);
            }
            return Ok(page);
        }
        // CAD-2215 acceptance seam: a test can pause here after the initial
        // exact grant snapshot and selected conversation are resolved, revoke
        // the grant from another thread, then resume; the forward branch's
        // re-proof must refuse the stale page. Test builds only, scoped member
        // forward reads only, and only for a genuinely admitted selected app
        // conversation.
        #[cfg(test)]
        if member_relay {
            if let Some(thread) = conversation
                .as_ref()
                .filter(|thread| Self::member_app_thread_admitted(&member_access, Some(*thread)))
            {
                if let Some(hook) = self.app_grant_forward_snapshot_hook.as_ref() {
                    hook(&thread.id);
                }
            }
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        // At most one re-proof per call, so a wait cannot turn a revoked or
        // unsettled operation into a readback loop.
        let mut readmitted = false;
        loop {
            let thread = match &conversation {
                Some(thread) => Some(thread.clone()),
                None => self.store.thread(&alias)?,
            };
            Self::require_scoped_app_read(&alias, thread.as_ref(), &member_access, member_relay)?;
            let home = member_relay
                && crate::master::is_master(&alias)
                && thread.as_ref().is_none_or(store::Thread::runs_as_home);
            let raw = if home {
                Vec::new()
            } else {
                match (&thread, allowed_issues.as_deref()) {
                    (Some(thread), _) if member_access.conversation(&thread.id) => {
                        self.store.thread_entries_after_granted(
                            &thread.id,
                            after,
                            limit,
                            allowed_issues.as_deref().unwrap_or(&[]),
                            &private_denials,
                            member_relay,
                        )?
                    }
                    (Some(thread), Some(issues)) => self.store.thread_entries_after_scoped(
                        &thread.id,
                        after,
                        limit,
                        issues,
                        &private_denials,
                        member_relay,
                    )?,
                    (Some(thread), None) => {
                        self.store.thread_entries_of(&thread.id, after, limit)?
                    }
                    (None, _) => Vec::new(),
                }
            };
            // A read that WAITED can observe content that settled after its
            // admission was taken — its own queued turn woken by another
            // connection, for instance — and, on an ordinary alias, a routed
            // cloud result that resolved after admission. Neither was
            // admitted, so re-resolve this page's whole admission once before
            // projecting it: the new snapshot either covers each row (under
            // the platform's own answer, for cloud content) or refuses /
            // withholds it. Projecting under the old admission would serve a
            // row authority was never proven for.
            if !readmitted
                && projection.needs_readmission(
                    &self.store,
                    raw.iter().filter_map(|entry| entry.message_id.as_deref()),
                )?
            {
                projection = self.admit_cloud_projection(&alias, peer_pid, "thread read")?;
                readmitted = true;
            }
            // The cursor is the last row this page SCANNED, never the last
            // row the projection kept. Filtering another source's cloud
            // entries out must not leave the caller's `after` on a seq it has
            // already passed, or a consumer would re-read the same hidden
            // page forever; the filtered content itself is never exposed.
            let scanned = raw.last().map(|e| e.seq).unwrap_or(after);
            // Whether the scan reached the page's end is a property of the
            // RAW page, never of the kept one: a projection can hide every
            // row of a full page, so the visible count says nothing about
            // exhaustion.
            let more_after = raw.len() as i64 == limit;
            let entries = projection.entries(&self.store, raw)?;
            if !entries.is_empty()
                || Instant::now() >= deadline
                || self.closing.load(Ordering::SeqCst)
            {
                // CAD-1700: the same complete watermark whatever the
                // cursor — a cold tail or a long catch-up page carries it.
                let reply_seq = if home {
                    0
                } else {
                    match (&thread, allowed_issues.as_deref()) {
                        (Some(thread), _) if member_access.conversation(&thread.id) => {
                            self.store.thread_reply_seq_granted(
                                &thread.id,
                                allowed_issues.as_deref().unwrap_or(&[]),
                                &private_denials,
                            )?
                        }
                        (Some(thread), Some(issues)) => self.store.thread_reply_seq_scoped(
                            &thread.id,
                            issues,
                            &private_denials,
                        )?,
                        (Some(thread), None) => self.store.thread_reply_seq(&thread.id)?,
                        (None, _) => 0,
                    }
                };
                // A grant can be revoked or its install/digest/context can
                // drift while a long poll waits. Re-prove the reader and the
                // exact grants before any page leaves, exactly like the
                // backward branch; a changed answer is a refusal.
                let (current_scope, current_principal) =
                    self.thread_read_scope(params, peer_pid)?;
                let current_access = match current_principal.as_ref() {
                    Some(principal) => self.member_app_access(principal)?,
                    None => MemberAppAccess::default(),
                };
                if current_scope != scope
                    || current_principal != principal
                    || current_access != member_access
                    || (scope.is_some()
                        && self.private_job_denials_for_read(
                            params,
                            None,
                            current_principal.as_ref(),
                            peer_pid,
                        )? != private_denials)
                {
                    return Err(Error::rejected(
                        "thread read refused: reader access changed",
                    ));
                }
                Self::require_scoped_app_read(
                    &alias,
                    thread.as_ref(),
                    &current_access,
                    member_relay,
                )?;
                self.authorize_thread_read_target(
                    &alias,
                    params,
                    current_principal.as_ref(),
                    peer_pid,
                )?;
                let mut page = serde_json::to_value(ThreadPage {
                    alias: alias.clone(),
                    thread: if scope.is_some() {
                        None
                    } else {
                        thread.as_ref().map(store::Thread::info)
                    },
                    entries: self.entries_json(&alias, &entries)?,
                    cursor: scanned,
                    more_before: None,
                    scanned_before: None,
                    more_after: Some(more_after),
                    cloud_withheld: if private_denials.exclude_cloud {
                        None
                    } else {
                        projection.withheld
                    },
                    reply_seq,
                })
                .expect("thread page serializes");
                if member_relay && crate::master::is_master(&alias) && !home {
                    page["_member_app"] = json!(true);
                }
                if member_relay {
                    page[READER_SCOPE_FIELD] = json!(THREAD_SCOPE_ACK);
                }
                if scope.is_none() && params.get("cursor_format").is_some() {
                    page[READER_SCOPE_FIELD] = json!(THREAD_OPERATOR_SCOPE_ACK);
                }
                return Ok(page);
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// A directly connected agent pages only rows its current project and
    /// cloud-source proofs admit. Hidden rows never become page boundaries.
    fn rpc_agent_thread_opaque(
        &self,
        params: &Value,
        peer_pid: u32,
        mut context: AgentThreadRead<'_>,
    ) -> Result<Value> {
        if params.get("cursor_format").and_then(Value::as_str) != Some(THREAD_CURSOR_FORMAT) {
            return Err(Error::invalid(
                "thread_cursor_format",
                "scoped thread reads require opaque-v1 cursors",
            ));
        }
        let supplied = |field: &str| -> Result<Option<&str>> {
            match params.get(field) {
                None => Ok(None),
                Some(Value::String(token)) if !token.is_empty() => Ok(Some(token)),
                Some(Value::Number(_)) => Err(Error::invalid(
                    "thread_cursor_numeric",
                    "scoped thread positions must be opaque",
                )),
                Some(_) => Err(Error::invalid(
                    "thread_cursor_invalid",
                    "thread cursor is invalid",
                )),
            }
        };
        let after = supplied("after")?;
        let before = supplied("before")?;
        let tail = match params.get("tail") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(Error::invalid(
                    "thread_cursor_invalid",
                    "thread direction is invalid",
                ))
            }
        };
        let backward = tail || before.is_some();
        if (tail && before.is_some())
            || (backward && (after.is_some() || params.get("wait").is_some()))
        {
            return Err(Error::invalid(
                "thread_cursor_invalid",
                "thread read takes one paging direction",
            ));
        }
        let limit = optional_i64(params, "limit").unwrap_or(100);
        if !(1..=store::THREAD_PAGE_MAX).contains(&limit) {
            return Err(Error::rejected(format!(
                "Thread page limit must be 1-{}",
                store::THREAD_PAGE_MAX
            )));
        }
        let principal = super::job_events_opaque::agent_read_principal(
            self,
            params,
            peer_pid,
            None,
            None,
            "thread read",
        )?
        .ok_or_else(|| Error::rejected("thread read refused: agent identity is unavailable"))?;
        let target_uid = self.store.agent(context.alias)?.uid;
        let codec = self.scoped_cursor_codec()?;
        let filters = super::agent_events_opaque::project_set_filters(&context.scope.0);
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let thread = match context.conversation {
                Some(thread) => Some(thread.clone()),
                None => self.store.thread(context.alias)?,
            };
            let Some(thread) = thread else {
                if after.is_some() || before.is_some() {
                    return Err(Error::invalid(
                        "thread_cursor_invalid",
                        "thread cursor is invalid",
                    ));
                }
                if !backward && Instant::now() < deadline && !self.closing.load(Ordering::SeqCst) {
                    self.changed
                        .wait_until(deadline.min(Instant::now() + Duration::from_secs(1)));
                    continue;
                }
                let (current, public) = self.thread_read_scope(params, peer_pid)?;
                let current_principal = super::job_events_opaque::agent_read_principal(
                    self,
                    params,
                    peer_pid,
                    None,
                    public.as_ref(),
                    "thread read",
                )?;
                if current.as_ref().map(|scope| &scope.0) != Some(&context.scope.0)
                    || current_principal.as_deref() != Some(principal.as_str())
                    || self.store.agent(context.alias)?.uid != target_uid
                {
                    return Err(Error::rejected(
                        "thread read refused: reader access changed",
                    ));
                }
                let mut page = json!({
                    "alias": context.alias, "thread": null, "entries": [],
                    "cursor_format": THREAD_CURSOR_FORMAT,
                    "cursor": null, "before_cursor": null,
                    "more_before": backward.then_some(false),
                    "more_after": (!backward).then_some(false),
                });
                page[READER_SCOPE_FIELD] = json!(THREAD_OPAQUE_SCOPE_ACK);
                return Ok(page);
            };
            let created = format!("{:016x}", thread.created.to_bits());
            let resource = super::job_events_opaque::framed(&[
                "agent-thread",
                context.alias,
                &target_uid,
                &thread.id,
                &created,
            ]);
            let binding = |direction| CursorBinding {
                domain: "agent-thread",
                direction,
                principal: &principal,
                resource: &resource,
                filters: &filters,
            };
            let token = if backward { before } else { after };
            let direction = if backward {
                CursorDirection::Backward
            } else {
                CursorDirection::Forward
            };
            let start = token
                .map(|token| {
                    codec.open(token, &binding(direction)).map_err(|_| {
                        Error::invalid("thread_cursor_invalid", "thread cursor is invalid")
                    })
                })
                .transpose()?
                .unwrap_or(if backward { i64::MAX } else { 0 });

            // Select one extra VISIBLE row for exhaustion; a foreign cloud
            // row can neither consume a page slot nor advance its boundary.
            let scan = |projection: &mut super::cloud_agent_rpc::CloudProjection,
                        issues: &[&str],
                        denials: &store::PrivateJobDenials,
                        _phase: &str|
             -> Result<(Vec<store::ThreadEntry>, Vec<String>)> {
                let mut selected = Vec::with_capacity(limit as usize + 1);
                let mut denied = Vec::new();
                let mut position = start;
                loop {
                    let batch = (limit + 1).clamp(32, store::THREAD_PAGE_MAX);
                    let (raw, more) = if backward {
                        self.store.thread_entries_before_scoped(
                            &thread.id,
                            Some(position),
                            batch,
                            issues,
                            denials,
                            false,
                        )?
                    } else {
                        let rows = self.store.thread_entries_after_scoped(
                            &thread.id, position, batch, issues, denials, false,
                        )?;
                        let more = rows.len() as i64 == batch;
                        (rows, more)
                    };
                    if raw.is_empty() {
                        break;
                    }
                    if projection.needs_readmission(
                        &self.store,
                        raw.iter().filter_map(|entry| entry.message_id.as_deref()),
                    )? {
                        // Re-admit this batch without restarting or counting
                        // foreign arrivals. The complete visible selection is
                        // independently re-scanned under fresh authority
                        // before it can be returned.
                        *projection =
                            self.admit_cloud_projection(context.alias, peer_pid, "thread read")?;
                        #[cfg(feature = "test-seam")]
                        if let Some(observer) = &self.agent_thread_batch_observer {
                            let phase = match _phase {
                                "initial" => "initial-refresh",
                                "confirmation" => "confirmation-refresh",
                                _ => "recheck-refresh",
                            };
                            observer(context.alias, phase, selected.len());
                        }
                        let retained = selected.len();
                        selected = projection.entries(&self.store, selected)?;
                        if selected.len() != retained {
                            return Err(Error::rejected(
                                "thread read refused: reader access changed",
                            ));
                        }
                    }
                    for id in raw.iter().filter_map(|entry| entry.message_id.as_deref()) {
                        if projection.scoped_denial(id).is_some() {
                            denied.push(id.to_owned());
                        }
                    }
                    let boundary = if backward { raw.first() } else { raw.last() }
                        .expect("nonempty thread batch")
                        .seq;
                    let mut visible = projection.entries(&self.store, raw)?;
                    if backward {
                        visible.reverse();
                    }
                    for entry in visible {
                        selected.push(entry);
                        if selected.len() > limit as usize {
                            break;
                        }
                    }
                    #[cfg(feature = "test-seam")]
                    if let Some(observer) = &self.agent_thread_batch_observer {
                        observer(context.alias, _phase, selected.len());
                    }
                    if selected.len() > limit as usize || !more {
                        break;
                    }
                    position = boundary;
                }
                Ok((selected, denied))
            };

            let (selected, _) = scan(
                &mut context.projection,
                context.issues,
                context.denials,
                "initial",
            )?;
            if selected.is_empty()
                && !backward
                && Instant::now() < deadline
                && !self.closing.load(Ordering::SeqCst)
            {
                self.changed
                    .wait_until(deadline.min(Instant::now() + Duration::from_secs(1)));
                continue;
            }
            let (current_scope, current_public) = self.thread_read_scope(params, peer_pid)?;
            let current_scope = current_scope
                .ok_or_else(|| Error::rejected("thread read refused: reader access changed"))?;
            let current_principal = super::job_events_opaque::agent_read_principal(
                self,
                params,
                peer_pid,
                None,
                current_public.as_ref(),
                "thread read",
            )?;
            if current_scope.0 != context.scope.0
                || current_principal.as_deref() != Some(principal.as_str())
                || self.store.agent(context.alias)?.uid != target_uid
            {
                return Err(Error::rejected(
                    "thread read refused: reader access changed",
                ));
            }
            let current_thread = if context.conversation.is_some() {
                self.store.thread_by_id(&thread.id)?
            } else {
                self.store.thread(context.alias)?
            };
            if current_thread.as_ref().is_none_or(|current| {
                current.id != thread.id
                    || current.alias != thread.alias
                    || current.created.to_bits() != thread.created.to_bits()
            }) {
                return Err(Error::rejected("thread read refused: conversation changed"));
            }
            self.authorize_thread_read_target(context.alias, params, None, peer_pid)?;
            let current_denials =
                self.private_job_denials_for_read(params, None, None, peer_pid)?;
            let current_issues: Vec<&str> = current_scope
                .1
                .iter()
                .filter(|(_, project)| current_scope.0.contains(*project))
                .map(|(issue, _)| issue.as_str())
                .collect();
            let mut final_projection =
                self.admit_cloud_projection(context.alias, peer_pid, "thread read")?;
            let (mut confirmed, mut denied) = scan(
                &mut final_projection,
                &current_issues,
                &current_denials,
                "confirmation",
            )?;
            if selected
                .iter()
                .map(|entry| entry.seq)
                .ne(confirmed.iter().map(|entry| entry.seq))
            {
                return Err(Error::rejected(
                    "thread read refused: reader access changed",
                ));
            }
            let (last_scope, last_public) = self.thread_read_scope(params, peer_pid)?;
            let last_principal = super::job_events_opaque::agent_read_principal(
                self,
                params,
                peer_pid,
                None,
                last_public.as_ref(),
                "thread read",
            )?;
            let last_scope = last_scope
                .ok_or_else(|| Error::rejected("thread read refused: reader access changed"))?;
            if last_scope.0 != current_scope.0
                || last_principal.as_deref() != Some(principal.as_str())
            {
                return Err(Error::rejected(
                    "thread read refused: reader access changed",
                ));
            }
            let last_denials = self.private_job_denials_for_read(params, None, None, peer_pid)?;
            if last_scope.1 != current_scope.1 || last_denials != current_denials {
                // A hidden issue or private job may have appeared while
                // reading. Refuse only if the VISIBLE page changed.
                let last_issues: Vec<&str> = last_scope
                    .1
                    .iter()
                    .filter(|(_, project)| last_scope.0.contains(*project))
                    .map(|(issue, _)| issue.as_str())
                    .collect();
                let (rechecked, rechecked_denied) = scan(
                    &mut final_projection,
                    &last_issues,
                    &last_denials,
                    "recheck",
                )?;
                if confirmed
                    .iter()
                    .map(|entry| entry.seq)
                    .ne(rechecked.iter().map(|entry| entry.seq))
                {
                    return Err(Error::rejected(
                        "thread read refused: reader access changed",
                    ));
                }
                confirmed = rechecked;
                denied = rechecked_denied;
            }
            // The confirmation scan can take long enough for a grant or
            // credential to change. Re-prove every selected row, including
            // the extra exhaustion witness, under one last cloud admission.
            let proved_projection =
                self.admit_cloud_projection(context.alias, peer_pid, "thread read")?;
            let retained = confirmed.len();
            confirmed = proved_projection.entries(&self.store, confirmed)?;
            if confirmed.len() != retained
                || denied
                    .iter()
                    .any(|id| proved_projection.scoped_denial(id).is_none())
            {
                return Err(Error::rejected(
                    "thread read refused: reader access changed",
                ));
            }
            let warning = denied
                .iter()
                .find_map(|id| proved_projection.scoped_denial(id));
            let more = confirmed.len() > limit as usize;
            if more {
                confirmed.pop();
            }
            if backward {
                confirmed.reverse();
            }
            let seal = |seq, direction| {
                codec
                    .seal(seq, &binding(direction))
                    .map_err(|_| super::scoped_cursor_config::unavailable())
            };
            let forward = confirmed
                .last()
                .map(|row| seal(row.seq, CursorDirection::Forward))
                .transpose()?
                .or_else(|| (!backward).then(|| after.map(str::to_owned)).flatten());
            let previous = confirmed
                .first()
                .map(|row| seal(row.seq, CursorDirection::Backward))
                .transpose()?
                .or_else(|| backward.then(|| before.map(str::to_owned)).flatten());
            let rows = self
                .entries_json(context.alias, &confirmed)?
                .into_iter()
                .zip(&confirmed)
                .map(|(row, entry)| {
                    let id = codec
                        .stable_row_id("agent-thread-entry", &resource, &entry.seq.to_be_bytes())
                        .map_err(|_| super::scoped_cursor_config::unavailable())?;
                    let mut value = serde_json::to_value(row)?;
                    let object = value.as_object_mut().expect("thread entry object");
                    object.remove("seq");
                    object.insert("id".to_string(), json!(id));
                    Ok(value)
                })
                .collect::<Result<Vec<_>>>()?;
            let mut page = json!({
                "alias": context.alias, "thread": null, "entries": rows,
                "cursor_format": THREAD_CURSOR_FORMAT,
                "cursor": forward, "before_cursor": previous,
                "more_before": backward.then_some(more),
                "more_after": (!backward).then_some(more),
            });
            if let Some(reason) = warning {
                page["cloud_withheld"] = json!(reason);
            }
            page[READER_SCOPE_FIELD] = json!(THREAD_OPAQUE_SCOPE_ACK);
            return Ok(page);
        }
    }

    /// `thread_send` — the operator's chat message to an agent: starts
    /// the alias's thread on first use and queues the text exactly like
    /// `agent_send`, recorded as an `operator` entry. CAD-1591 adds one
    /// optional field, `expected_recipient_uid`: the contextual-reply
    /// recipient constraint — a nonempty uid, never authority, fenced
    /// against the alias's live row inside the enqueue transaction, so
    /// a removed or replaced participant can never silently receive the
    /// send. An omitted field is the ordinary path, unchanged.
    ///
    /// It instructs an agent, so an agent must never reach it: a
    /// connection the daemon attributes to a pane or managed endpoint is
    /// refused, and so is one whose identity cannot be derived (fail
    /// closed), and a connection tied to no agent must still be provably
    /// the operator (CAD-276's positive proof, CAD-339): the board relays
    /// browser writes from its own process, so the browser's identity is
    /// the board's to establish (CAD-313). Identity-shaped and routing fields are refused rather
    /// than read.
    pub(super) fn rpc_thread_send(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        if let Some(obj) = params.as_object() {
            if let Some(field) = obj.keys().find(|k| {
                !matches!(
                    k.as_str(),
                    "alias"
                        | "text"
                        | "message"
                        | "refs"
                        | "app"
                        | "conversation"
                        | "attachments"
                        | "media_refs"
                        | "expected_recipient_uid"
                        | "public_session"
                        | "session"
                )
            }) {
                return Err(Error::rejected(format!(
                    "thread send takes alias, text, message, refs, app, conversation, attachments, \
                     media_refs, expected_recipient_uid and the server-held session proofs only; \
                     field '{field}' is not accepted"
                )));
            }
        }
        // CAD-339 (review round 1): no agent identity is not enough — a
        // detached child of an agent derives none. The connection must
        // be provably the operator (CAD-276); the board relays browser
        // writes from its own operator process (CAD-313 gap).
        self.operator_chat("thread send", peer_pid)?;
        let human = self.verified_owner_human(params)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        // CAD-1591: the identity constraint is parsed BEFORE the cloud
        // branch so a cloud target can never bypass it.
        let cloud_expected_uid = match params.get("expected_recipient_uid") {
            None | Some(Value::Null) => None,
            Some(Value::String(uid)) if !uid.is_empty() => Some(uid.as_str()),
            Some(_) => {
                return Err(Error::rejected(
                    "expected_recipient_uid must be a nonempty recipient uid string",
                ));
            }
        };
        if self
            .store
            .agent_opt(&alias)?
            .is_some_and(|agent| agent.provider == crate::cloud_agent::PROVIDER)
        {
            let conversation = optional_str(params, "conversation").map(str::to_string);
            let mut routed = params.clone();
            routed["alias"] = json!(alias);
            return self.cloud_send_from(
                &alias,
                &routed,
                peer_pid,
                conversation.as_deref(),
                cloud_expected_uid,
                "cloud agent thread send",
            );
        }
        // CAD-1591: the contextual-reply recipient constraint — an
        // optional nonempty uid, checked against the alias's live row
        // inside the enqueue transaction (never a precheck that can
        // race). It is a typed argument like `human`: stripped from the
        // forwarded params, so `expected_recipient_uid` is never a send
        // request field on any verb.
        // The uid is borrowed from the request, never copied: the
        // forwarded params below carry their own copy, and this typed
        // argument only has to outlive the enqueue call.
        let expected_uid = match params.get("expected_recipient_uid") {
            None | Some(Value::Null) => None,
            Some(Value::String(uid)) if !uid.is_empty() => Some(uid.as_str()),
            Some(_) => {
                return Err(Error::rejected(
                    "expected_recipient_uid must be a nonempty recipient uid string",
                ));
            }
        };
        // CAD-802: a raw `app` binding rides through; `send_as`
        // proves it against the store and stamps the normalized form
        // on the entry — one proving place for every caller.
        let mut send = params.clone();
        send["alias"] = json!(alias);
        send["source"] = json!("operator");
        // The thread starts inside the enqueue transaction: a refused
        // message leaves no thread and no `thread_created` event. The
        // daemon-derived human is a typed argument, never a field of `send`.
        if let Some(obj) = send.as_object_mut() {
            obj.remove("public_session");
            obj.remove("expected_recipient_uid");
        }
        let mut receipt = self.send_with_human_expected(
            &send,
            &|_| Ok(store::Sender::OperatorChat),
            &|verb| {
                Err(Error::rejected(format!(
                    "{verb} refused: this send carries no caller to steer with"
                )))
            },
            human.as_ref(),
            expected_uid,
            None,
        )?;
        // The thread the message landed in: its conversation, which is
        // home for an unbound send (CAD-1098).
        let landed = match receipt.get("message").and_then(Value::as_str) {
            Some(id) => self.store.message_conversation(&alias, id)?,
            None => None,
        };
        receipt["thread"] = landed
            .or(self.store.thread(&alias)?)
            .as_ref()
            .map(store::Thread::to_json)
            .unwrap_or(Value::Null);
        Ok(receipt)
    }
}

#[cfg(test)]
mod cad1700_projection_tests {
    use super::*;
    use crate::store::{
        ConversationKind, NewAgent, NewEntry, KIND_ASSISTANT_TEXT, KIND_MESSAGE, KIND_TOOL_CALL,
        KIND_TURN_RESULT, ROLE_AGENT, ROLE_OPERATOR, ROLE_SYSTEM,
    };
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
        // `thread_append` writes the alias's existing home thread; it
        // never creates one (the send path does).
        for alias in ["master", "w1"] {
            shared.store.ensure_thread(alias).unwrap();
        }
        (dir, shared)
    }

    fn append(shared: &Arc<Shared>, alias: &str, role: &str, kind: &str, text: &str) -> i64 {
        shared
            .store
            .thread_append(
                alias,
                NewEntry {
                    role,
                    kind,
                    text,
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .expect("the entry is stored")
    }

    fn read(shared: &Arc<Shared>, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            shared.dispatch("thread_read", &params, std::process::id())
        })
        .expect("thread_read answers")
    }

    /// The newest page is bounded: after 210 non-reply rows the last real
    /// assistant reply is off it, so a client recomputing the unread
    /// watermark from the page loses it. `reply_seq` must be the complete,
    /// authorized thread watermark regardless of the tail/forward/older
    /// cursor, and non-reply, blank and foreign rows must not advance it.
    /// Red before CAD-1700 P2: the page had no `reply_seq` at all.
    #[test]
    fn cad1700_thread_read_tail_keeps_the_complete_reply_watermark() {
        let (_dir, shared) = fixture();
        append(
            &shared,
            "master",
            ROLE_OPERATOR,
            KIND_MESSAGE,
            "Please review",
        );
        append(&shared, "master", ROLE_AGENT, KIND_TOOL_CALL, "git status");
        let reply = append(
            &shared,
            "master",
            ROLE_AGENT,
            KIND_ASSISTANT_TEXT,
            "The review is ready",
        );
        append(
            &shared,
            "master",
            ROLE_SYSTEM,
            KIND_TURN_RESULT,
            "A system notice is not a reply",
        );
        append(
            &shared,
            "master",
            ROLE_OPERATOR,
            KIND_ASSISTANT_TEXT,
            "An operator note is not a reply",
        );
        for blank in ["", " \t\r\n", "\u{00a0}\u{2003}\u{feff}"] {
            append(&shared, "master", ROLE_AGENT, KIND_ASSISTANT_TEXT, blank);
            append(&shared, "master", ROLE_AGENT, KIND_TURN_RESULT, blank);
        }
        // Another agent's real reply belongs to that agent's own thread.
        append(
            &shared,
            "w1",
            ROLE_AGENT,
            KIND_ASSISTANT_TEXT,
            "A foreign reply",
        );
        for i in 0..210 {
            append(
                &shared,
                "master",
                ROLE_AGENT,
                KIND_TOOL_CALL,
                &format!("step {i}"),
            );
        }
        let tail = read(
            &shared,
            json!({"alias": "master", "tail": true, "limit": 200}),
        );
        let entries = tail["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 200, "the page itself stays bounded");
        assert!(
            entries[0]["seq"].as_i64().unwrap() > reply,
            "the real reply is off the newest page"
        );
        assert_eq!(
            tail["reply_seq"],
            json!(reply),
            "a cold tail page still reports the complete reply watermark"
        );
        let forward = read(&shared, json!({"alias": "master", "after": 0, "limit": 5}));
        assert_eq!(
            forward["reply_seq"],
            json!(reply),
            "a forward page reports the same complete watermark"
        );
        let older = read(
            &shared,
            json!({"alias": "master", "before": reply + 1, "limit": 200}),
        );
        assert_eq!(
            older["reply_seq"],
            json!(reply),
            "an older page reports the same complete watermark"
        );
    }

    /// `reply_seq` is the watermark of the thread the read authorized,
    /// never another conversation's or another alias's reply.
    #[test]
    fn cad1700_thread_read_reply_seq_is_scoped_to_the_read_thread() {
        let (_dir, shared) = fixture();
        let home_reply = append(
            &shared,
            "master",
            ROLE_AGENT,
            KIND_ASSISTANT_TEXT,
            "Home reply",
        );
        let (conversation, _) = shared
            .store
            .conversation_create("master", "install-fixture", None, ConversationKind::Fresh)
            .unwrap();
        let conv_reply = shared
            .store
            .thread_append_to(
                &conversation.id,
                NewEntry {
                    role: ROLE_AGENT,
                    kind: KIND_ASSISTANT_TEXT,
                    text: "Conversation reply",
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .unwrap();
        assert!(conv_reply > home_reply, "the conversation reply is newer");
        let conv = read(
            &shared,
            json!({"alias": "master", "conversation": conversation.id,
                   "tail": true, "limit": 200}),
        );
        assert_eq!(conv["reply_seq"], json!(conv_reply));
        assert_eq!(conv["thread"]["id"], json!(conversation.id));
        let home = read(
            &shared,
            json!({"alias": "master", "tail": true, "limit": 200}),
        );
        assert_eq!(
            home["reply_seq"],
            json!(home_reply),
            "another conversation's reply never advances Home"
        );
    }
}
