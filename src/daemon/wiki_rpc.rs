//! CAD-580: `cadence daemon` wiki RPC handlers — `src/wiki/` is the
//! store; this file binds the caller and relays. `RULES` names every
//! method `Rule::Handler` and the handler calls [`Shared::wiki_caller`]
//! first — `agent_caller` derives the connection's identity (operator,
//! or the agent its pane/endpoint proves) and refuses the unproven.
//!
//! `wiki_as` is the one field the handlers accept: an OPERATOR
//! connection may carry it to stand in for a relayed caller — the
//! board's `agent:<alias>`/`user:<handle>`/`public` — because the
//! board's own connection can only prove operator. On an agent
//! connection `wiki_as` is refused unless it names that same agent:
//! an agent never acts as another, and the operator's own relay
//! never claims to be one.

use super::*;

use crate::wiki::{self, Caller};

// A transfer write spans staged bytes, Git and Store progress. Serialize these
// bounded writes so completion/cleanup cannot race an in-flight chunk.
static TRANSFER_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct WikiHuman {
    realm: crate::operator_auth::PublicRealm,
    sub: String,
    session: String,
    display: String,
    role: crate::board_identity::Role,
}

impl WikiHuman {
    fn receipt(&self) -> Value {
        json!({
            "company": self.realm.company,
            "sub": self.sub,
            "session": self.session,
            "display": self.display,
            "role": self.role.as_str(),
        })
    }

    fn binding(&self) -> Value {
        json!({
            "issuer": self.realm.issuer,
            "organization_id": self.realm.company,
            "audience": self.realm.host,
            "subject": self.sub,
        })
    }
}

impl Shared {
    /// The wiki caller: connection-derived, then reconciled with an
    /// optional `wiki_as` claim (module doc). An agent's project — the
    /// project whose repo contains its registered cwd — rides the
    /// caller so `projects/<key>/` writes check against it.
    fn wiki_caller(&self, params: &Value, peer_pid: u32, verb: &str) -> Result<Caller> {
        let conn = self.agent_caller(peer_pid, verb)?;
        let claim = optional_str(params, "wiki_as");
        match &conn {
            AgentCaller::Operator => self.wiki_as(claim, verb),
            AgentCaller::Agent(alias) => match claim {
                None => Ok(self.agent_wiki_caller(alias)),
                Some(c) if c == format!("agent:{alias}") => Ok(self.agent_wiki_caller(alias)),
                Some(c) => Err(Error::rejected(format!(
                    "{verb} refused: agent '{alias}' is attributed to itself — \
                     'wiki_as' names {c}, and an agent never acts as another \
                     agent, a user or the operator (caller rule, CAD-580)"
                ))),
            },
        }
    }

    /// `wiki_as` on an operator connection: the operator itself, or a
    /// relayed caller the board proves (`agent:<a>`, `user:<u>`,
    /// `public`). Any other string is a refusal, never a silent
    /// fall-back.
    fn wiki_as(&self, claim: Option<&str>, verb: &str) -> Result<Caller> {
        match claim {
            None | Some("operator") => Ok(Caller::Operator),
            Some("public") => Ok(Caller::Public),
            Some(c) => match c.split_once(':') {
                Some(("agent", alias)) if !alias.is_empty() => Ok(self.agent_wiki_caller(alias)),
                Some(("user", handle)) if valid_user_handle(handle) => {
                    Ok(Caller::User(handle.to_string()))
                }
                _ => Err(Error::rejected(format!(
                    "{verb} refused: 'wiki_as' claims '{c}' — one of 'operator', \
                     'public', 'agent:<alias>' or 'user:<handle>'"
                ))),
            },
        }
    }

    /// An agent's [`Caller`]: its alias plus the tracker project its
    /// current turn belongs to (`projects/<key>` write check, CAD-1752).
    fn agent_wiki_caller(&self, alias: &str) -> Caller {
        let project = self
            .store
            .agent_opt(alias)
            .ok()
            .flatten()
            .and_then(|agent| self.turn_project(&agent))
            .map(|turn| turn.key);
        Caller::Agent {
            alias: alias.to_string(),
            project,
        }
    }

    /// CAD-1374: the human a wiki write is attributed to. Absent
    /// `public_session` is the legacy path (`None`). A present one must be a
    /// non-empty token that is live, fresh and owner-or-member as the caller
    /// allows: a named `User` must be the session's own handle, an operator
    /// caller must be the owner, and an agent or public caller takes no proof.
    fn wiki_human(&self, params: &Value, caller: &Caller, verb: &str) -> Result<Option<WikiHuman>> {
        let token = match params.get("public_session") {
            None => return Ok(None),
            Some(Value::String(token)) if !token.is_empty() => token,
            Some(_) => {
                return Err(Error::rejected(format!(
                    "{verb} refused: the board session proof is malformed"
                )))
            }
        };
        if matches!(caller, Caller::Agent { .. } | Caller::Public) {
            return Err(Error::rejected(format!(
                "{verb} refused: an agent or public caller carries no board session"
            )));
        }
        let (role, realm, view, sub, display) = match self.current_public_session(token)? {
            crate::daemon::public_membership::Authority::Granted {
                role,
                realm,
                view,
                sub,
                display,
                ..
            } => (role, realm, view, sub, display),
            crate::daemon::public_membership::Authority::Denied => {
                return Err(Error::rejected(format!(
                    "{verb} refused: the board session is not live"
                )))
            }
        };
        match caller {
            Caller::User(who) => {
                let handle = view.user.as_ref().map(|u| u.handle.as_str());
                if handle != Some(who.as_str()) {
                    return Err(Error::rejected(format!(
                        "{verb} refused: the board session is not user '{who}'"
                    )));
                }
            }
            Caller::Operator if !role.is_operator() => {
                return Err(Error::rejected(format!(
                    "{verb} refused: the board owner session proof is missing or is a member's"
                )))
            }
            _ => {}
        }
        Ok(Some(WikiHuman {
            realm,
            sub,
            session: view.id,
            display,
            role,
        }))
    }

    /// Owner authority for collection workflows comes from a verified session,
    /// never the shared Wiki writer handle. Keep ordinary Wiki ACLs unchanged.
    fn knowledge_actor(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
    ) -> Result<(Caller, Option<WikiHuman>)> {
        let mut caller = self.wiki_caller(params, peer_pid, verb)?;
        let human = self.wiki_human(params, &caller, verb)?;
        if human.as_ref().is_some_and(|human| human.role.is_operator()) {
            caller = Caller::Operator;
        }
        Ok((caller, human))
    }

    /// CAD-1374: before a wiki mutation touches the tree, check the EXISTING
    /// allowlist for every path it touches (`wiki::allowed`), then — only for
    /// a proven human — prepare the activity receipt and attach it to the PM.
    /// A forbidden mutation never reaches the receipt. A legacy write with no
    /// proof returns the PM unchanged.
    fn wiki_receipt(
        &self,
        pm: crate::issue::Pm,
        caller: &Caller,
        human: Option<&Value>,
        action: &str,
        paths: &[(&str, wiki::Op)],
    ) -> Result<crate::issue::Pm> {
        let mut resource = String::new();
        for (path, op) in paths {
            let norm = wiki::normalize(path)?;
            let segs: Vec<&str> = norm.split('/').collect();
            wiki::allowed(caller, *op, &segs)?;
            if !resource.is_empty() {
                resource.push_str(" -> ");
            }
            resource.push_str(&format!("wiki:{norm}"));
        }
        let Some(human) = human else {
            return Ok(pm);
        };
        let receipt = self.prepare_pm_activity_receipt(action, &resource, human)?;
        pm.with_activity_receipt(&receipt)
    }

    /// `reject_identity_fields` plus `wiki_as` — the fields the table
    /// checks are connection-bound, and `wiki_as` is admitted only as
    /// the caller rule describes (handled in [`Self::wiki_caller`],
    /// so nothing else may name it).
    fn reject_wiki_fields(params: &Value) -> Result<()> {
        reject_identity_fields(params, "wiki")
    }

    fn wiki_path(params: &Value) -> Result<&str> {
        required_str(params, "path")
    }

    pub(super) fn rpc_wiki_ls(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki ls")?;
        let path = optional_str(params, "path").unwrap_or("");
        wiki::ls(&self.pm()?, &caller, path)
    }

    pub(super) fn rpc_wiki_read(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki read")?;
        let path = Self::wiki_path(params)?;
        let offset = wiki::json_u64(params, "offset")?;
        let bytes = wiki::json_u64(params, "bytes")?;
        if params.get("at").is_some() {
            return Err(Error::rejected(
                "wiki read refused: a historical selector uses wiki_read_revision and is never read as the live page",
            ));
        }
        let rev = match params.get("rev") {
            None | Some(Value::Null) => None,
            Some(Value::String(rev)) => Some(rev.as_str()),
            Some(_) => return Err(Error::rejected("wiki read refused: 'rev' must be a string")),
        };
        if offset.is_none() && bytes.is_none() && rev.is_none() {
            return wiki::read(&self.pm()?, &caller, path);
        }
        wiki::read_range(
            &self.pm()?,
            &caller,
            path,
            wiki::BodyRange {
                offset: offset.unwrap_or(0),
                bytes,
                rev,
            },
        )
    }

    pub(super) fn rpc_wiki_write(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki write")?;
        let path = Self::wiki_path(params)?;
        let text = required_str(params, "text")?;
        let if_rev = optional_str(params, "if_rev");
        let human = self
            .wiki_human(params, &caller, "wiki write")?
            .map(|h| h.receipt());
        let pm = self.wiki_receipt(
            self.pm()?,
            &caller,
            human.as_ref(),
            "wiki_written",
            &[(path, wiki::Op::Write)],
        )?;
        let out = wiki::write(&pm, &caller, path, text, if_rev)?;
        // CAD-719: a committed write schedules the coalesced refresh —
        // an `if_rev` conflict (no commit) kicks nothing.
        if out["committed"] == true {
            self.wiki_index.kick();
        }
        Ok(out)
    }

    /// The board's upload relay: `{path, tmp, sha256?, if_rev?}` —
    /// `tmp` is the staged file under `<state>/wiki-uploads/` the
    /// store verifies, hashes and moves; `if_rev` guards the pointer
    /// write. The tracker write lock is taken only for the pointer
    /// commit, never while the file is checked.
    pub(super) fn rpc_wiki_put_blob(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki put_blob")?;
        let path = Self::wiki_path(params)?;
        let tmp = PathBuf::from(required_str(params, "tmp")?);
        let sha256 = optional_str(params, "sha256");
        let if_rev = optional_str(params, "if_rev");
        let human = self
            .wiki_human(params, &caller, "wiki put_blob")?
            .map(|h| h.receipt());
        let pm = self.wiki_receipt(
            self.pm()?,
            &caller,
            human.as_ref(),
            "wiki_blob_uploaded",
            &[(path, wiki::Op::Write)],
        )?;
        let mut out = wiki::put_blob(&pm, &self.state_dir, &caller, path, &tmp, sha256, if_rev)?;
        let committed = out["rev"].is_string();
        if out["mime"] == "application/pdf" {
            if let Some(sha) = out["sha256"].as_str() {
                out["extraction"] = wiki::ingest::pdf_result(&pm, &caller, path, sha);
            }
        }
        // CAD-719: the pointer commit (and any extraction page it
        // spawned) moved the wiki tree — schedule the refresh.
        if committed {
            self.wiki_index.kick();
        }
        Ok(out)
    }

    pub(super) fn rpc_wiki_mkdir(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki mkdir")?;
        let path = Self::wiki_path(params)?;
        let human = self
            .wiki_human(params, &caller, "wiki mkdir")?
            .map(|h| h.receipt());
        let pm = self.wiki_receipt(
            self.pm()?,
            &caller,
            human.as_ref(),
            "wiki_directory_created",
            &[(path, wiki::Op::Write)],
        )?;
        let out = wiki::mkdir(&pm, &caller, path)?;
        // CAD-719: a created dir commits a .gitkeep — the tree moved.
        if out["created"] == true {
            self.wiki_index.kick();
        }
        Ok(out)
    }

    pub(super) fn rpc_wiki_mv(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki mv")?;
        let from = required_str(params, "from")?;
        let to = required_str(params, "to")?;
        let human = self
            .wiki_human(params, &caller, "wiki mv")?
            .map(|h| h.receipt());
        // A `.trash/` source is an operator-only restore that `wiki::mv`
        // gates itself; only a real source path needs the ACL check here.
        let sources: Vec<(&str, wiki::Op)> = if from.starts_with(".trash/") {
            vec![(to, wiki::Op::Write)]
        } else {
            vec![(from, wiki::Op::Remove), (to, wiki::Op::Write)]
        };
        let pm = self.wiki_receipt(self.pm()?, &caller, human.as_ref(), "wiki_moved", &sources)?;
        let mut out = wiki::mv(&pm, &caller, from, to)?;
        let moved_flag = out["moved"] == true;
        let destination = out["to"].as_str().unwrap_or(to).to_string();
        let moved = match wiki::read(&pm, &caller, &destination) {
            Ok(page) => page,
            Err(e) => {
                out["extraction"] = json!({"status":"failed","reason":e.to_string()});
                if moved_flag {
                    self.wiki_index.kick();
                }
                return Ok(out);
            }
        };
        if moved["kind"] == "dir" {
            let source = out["from"].as_str().unwrap_or(from).to_string();
            let old_path = if let Some(rest) = source.strip_prefix(".trash/") {
                rest.split_once('/')
                    .map(|(_, original)| original)
                    .unwrap_or(&source)
            } else {
                &source
            };
            out["extraction"] = wiki::ingest::relocate_tree(&pm, &caller, old_path, &destination)
                .unwrap_or_else(|e| json!({"status":"failed","reason":e.to_string()}));
        } else if moved["mime"] == "application/pdf" {
            if let Some(sha) = moved["sha256"].as_str() {
                out["extraction"] = wiki::ingest::pdf_result(&pm, &caller, &destination, sha);
            }
            if !from.starts_with(".trash/") {
                if let Err(e) = wiki::ingest::remove_generated(&pm, &caller, from) {
                    out["extraction_cleanup_error"] = json!(e.to_string());
                }
            }
        }
        // CAD-719: the move committed (plus any relocate/extraction
        // writes) — schedule the refresh once.
        if moved_flag {
            self.wiki_index.kick();
        }
        Ok(out)
    }

    pub(super) fn rpc_wiki_rm(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki rm")?;
        let path = Self::wiki_path(params)?;
        let human = self
            .wiki_human(params, &caller, "wiki rm")?
            .map(|h| h.receipt());
        let pm = self.wiki_receipt(
            self.pm()?,
            &caller,
            human.as_ref(),
            "wiki_removed",
            &[(path, wiki::Op::Remove)],
        )?;
        let pdf = wiki::read(&pm, &caller, path)
            .ok()
            .is_some_and(|v| v["mime"] == "application/pdf");
        let mut out = wiki::rm(&pm, &caller, path)?;
        let removed = out["removed"] == true;
        if pdf {
            if let Err(e) = wiki::ingest::remove_generated(&pm, &caller, path) {
                out["extraction_cleanup_error"] = json!(e.to_string());
            }
        }
        // CAD-719: the removal committed — schedule the refresh so the
        // old hits are dropped promptly, not on the next search.
        if removed {
            self.wiki_index.kick();
        }
        Ok(out)
    }

    pub(super) fn rpc_wiki_search(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki search")?;
        let q = required_str(params, "q")?;
        let path = optional_str(params, "path").unwrap_or("");
        wiki::search(&self.pm()?, &self.state_dir, &caller, q, path)
    }

    pub(super) fn rpc_wiki_history(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki history")?;
        let path = Self::wiki_path(params)?;
        let limit = optional_u64(params, "limit").unwrap_or(50) as usize;
        wiki::history(&self.pm()?, &caller, path, limit)
    }

    pub(super) fn rpc_wiki_collection_get(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::allow_wiki_fields(
            params,
            &["path", "wiki_as", "public_session"],
            "wiki collection",
        )?;
        let (caller, _) = self.knowledge_actor(params, peer_pid, "wiki collection")?;
        let path = Self::wiki_path(params)?;
        wiki::collection::get(&self.pm()?, &self.state_dir, &caller, path)
    }

    pub(super) fn rpc_wiki_collection_put(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::allow_wiki_fields(
            params,
            &["path", "if_rev", "metadata", "wiki_as", "public_session"],
            "wiki collection",
        )?;
        let (caller, human) = self.knowledge_actor(params, peer_pid, "wiki collection")?;
        let path = Self::wiki_path(params)?;
        let if_rev = required_str(params, "if_rev")?;
        let metadata = params
            .get("metadata")
            .ok_or_else(|| Error::rejected("wiki collection refused: metadata is required"))?;
        let provenance = human
            .as_ref()
            .map(|human| human.sub.clone())
            .filter(|sub| !sub.is_empty())
            .unwrap_or_else(|| caller.actor());
        wiki::collection::put(
            &self.pm()?,
            &self.state_dir,
            &caller,
            path,
            if_rev,
            metadata,
            &provenance,
        )
    }

    pub(super) fn rpc_wiki_read_revision(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::allow_wiki_fields(
            params,
            &["path", "at", "offset", "bytes", "wiki_as", "public_session"],
            "wiki revision",
        )?;
        if params.get("rev").is_some() {
            return Err(Error::rejected(
                "wiki revision refused: the live 'rev' check is not a historical selector",
            ));
        }
        let caller = self.wiki_caller(params, peer_pid, "wiki revision")?;
        let path = Self::wiki_path(params)?;
        let at = required_str(params, "at")?;
        let offset = wiki::json_u64(params, "offset")?;
        let bytes = wiki::json_u64(params, "bytes")?;
        let range = if offset.is_some() || bytes.is_some() {
            Some(wiki::BodyRange {
                offset: offset.unwrap_or(0),
                bytes,
                rev: None,
            })
        } else {
            None
        };
        wiki::read_revision(&self.pm()?, &caller, path, at, range)
    }

    pub(super) fn rpc_wiki_corrections_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::allow_wiki_fields(
            params,
            &["path", "wiki_as", "public_session"],
            "wiki correction",
        )?;
        let (caller, human) = self.knowledge_actor(params, peer_pid, "wiki correction")?;
        let mut listed = wiki::correction::list(&self.pm()?, &caller, Self::wiki_path(params)?)?;
        // Permissions must describe what the review path will actually allow:
        // without a verified human subject both are refused, so do not offer
        // the form.
        if !Self::correction_subject(human.as_ref()) {
            if let Some(permissions) = listed.get_mut("permissions").and_then(Value::as_object_mut)
            {
                permissions.insert("submit".to_string(), Value::Bool(false));
                permissions.insert("review".to_string(), Value::Bool(false));
            }
        }
        Ok(listed)
    }

    pub(super) fn rpc_wiki_corrections_submit(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        Self::allow_wiki_fields(
            params,
            &["path", "correction", "wiki_as", "public_session"],
            "wiki correction",
        )?;
        let (caller, human) = self.knowledge_actor(params, peer_pid, "wiki correction")?;
        let human = Self::correction_human(human, "wiki correction")?;
        wiki::correction::submit(
            &self.pm()?,
            &caller,
            Self::wiki_path(params)?,
            params.get("correction").ok_or_else(|| {
                Error::rejected("wiki correction refused: correction report is required")
            })?,
            &human,
        )
    }

    pub(super) fn rpc_wiki_corrections_review(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        Self::allow_wiki_fields(
            params,
            &["path", "correction", "wiki_as", "public_session"],
            "wiki correction",
        )?;
        let (caller, human) = self.knowledge_actor(params, peer_pid, "wiki correction")?;
        let human = Self::correction_human(human, "wiki correction")?;
        wiki::correction::review(
            &self.pm()?,
            &caller,
            Self::wiki_path(params)?,
            params.get("correction").ok_or_else(|| {
                Error::rejected("wiki correction refused: correction review is required")
            })?,
            &human,
        )
    }

    /// Same subject grammar as `wiki::correction`'s review gate: absent, empty,
    /// or the `operator` alias is not a human the review path will accept.
    fn correction_subject(human: Option<&WikiHuman>) -> bool {
        let Some(human) = human else {
            return false;
        };
        let sub = human.sub.as_str();
        !sub.is_empty()
            && sub.len() <= 64
            && !sub.eq_ignore_ascii_case("operator")
            && sub
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    }

    fn correction_human(human: Option<WikiHuman>, verb: &str) -> Result<wiki::correction::Human> {
        let Some(human) = human else {
            return Err(Error::rejected(format!(
                "{verb} refused: a verified human session is required"
            )));
        };
        let display = human
            .display
            .chars()
            .filter(|ch| !ch.is_control())
            .take(200)
            .collect::<String>();
        let display = if display.trim().is_empty() {
            human.sub.clone()
        } else {
            display
        };
        Ok(wiki::correction::Human {
            sub: human.sub,
            display,
        })
    }

    fn allow_wiki_fields(params: &Value, allowed: &[&str], verb: &str) -> Result<()> {
        Self::reject_wiki_fields(params)?;
        let Some(fields) = params.as_object() else {
            return Err(Error::rejected(format!(
                "{verb} refused: the request must be an object"
            )));
        };
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Error::rejected(format!(
                "{verb} refused: the request has an unsupported field"
            )));
        }
        Ok(())
    }

    /// CAD-719: the operator's read-only index health — schema, committed
    /// vs indexed tree, freshness and its reason, indexed counts and the
    /// last refresh outcome. Never rebuilds; operator-scope only.
    pub(super) fn rpc_wiki_index_status(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("wiki index status", params, peer_pid)?;
        self.wiki_index.status()
    }

    /// CAD-719: the operator's explicit refresh — one rebuild through the
    /// same serialized write path the worker and the query-time check
    /// share, so it never doubles an in-flight rebuild.
    pub(super) fn rpc_wiki_index_refresh(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("wiki index refresh", params, peer_pid)?;
        self.wiki_index.refresh()
    }

    /// CAD-914: resumable wiki transfer. Caller identity is the connection.
    /// `_transfer_binding` is the operator board's verified envelope tuple
    /// `{issuer,organization_id,audience,subject}`. An agent connection that
    /// supplies it is refused. A request without it uses the local owner
    /// namespace. Export `path` must equal `root`.
    pub(super) fn rpc_wiki_transfer(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        const ALLOWED: &[&str] = &[
            "op",
            "manifest",
            "job_id",
            "root",
            "entry_id",
            "offset",
            "data_base64",
            "limit",
            "path",
            "bytes",
            "rev",
            "at",
            "wiki_as",
            "public_session",
            "_transfer_binding",
        ];
        let Some(fields) = params.as_object() else {
            return Err(Error::rejected(
                "wiki transfer refused: the request must be an object",
            ));
        };
        if fields.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
            return Err(Error::rejected(
                "wiki transfer refused: the request has an unsupported field",
            ));
        }
        let connection = self.agent_caller(peer_pid, "wiki transfer")?;
        if params.get("_transfer_binding").is_some() && !matches!(connection, AgentCaller::Operator)
        {
            return Err(Error::rejected(
                "wiki transfer refused: agent-provided authority is refused",
            ));
        }
        let op = required_str(params, "op")?;
        let _write_guard = if matches!(op, "begin" | "chunk" | "apply" | "finish") {
            Some(
                TRANSFER_WRITE_LOCK
                    .lock()
                    .map_err(|_| Error::internal("wiki transfer write lock is poisoned"))?,
            )
        } else {
            None
        };
        let caller = self.wiki_caller(params, peer_pid, "wiki transfer")?;
        let human = self.wiki_human(params, &caller, "wiki transfer")?;
        let public_binding = human.as_ref().map(WikiHuman::binding);
        let envelope_binding = params.get("_transfer_binding");
        if let (Some(public), Some(envelope)) = (&public_binding, envelope_binding) {
            if public != envelope {
                return Err(Error::rejected(
                    "wiki transfer refused: conflicting authority bindings",
                ));
            }
        }
        let owner = wiki::transfer::owner_for(
            &caller.actor(),
            public_binding.as_ref().or(envelope_binding),
        )?;
        let receipt = if matches!(op, "apply" | "finish") {
            human.as_ref().map(WikiHuman::receipt)
        } else {
            None
        };
        match op {
            "begin" => self.transfer_begin(params, &caller, &owner),
            "chunk" => self.transfer_chunk(params, &caller, &owner),
            "apply" => self.transfer_apply(params, &caller, &owner, receipt.as_ref()),
            "finish" => self.transfer_finish(params, &caller, &owner, receipt.as_ref()),
            "status" => self.transfer_status(params, &caller, &owner),
            "export" => self.transfer_export(params, &caller),
            _ => Err(Error::rejected(format!(
                "wiki transfer refused: unknown op '{op}'"
            ))),
        }
    }

    fn transfer_begin(&self, params: &Value, caller: &wiki::Caller, owner: &str) -> Result<Value> {
        let root = required_str(params, "root")?;
        let raw = params
            .get("manifest")
            .ok_or_else(|| Error::rejected("wiki transfer refused: begin needs a manifest"))?;
        let raw_len = serde_json::to_string(raw)?.len();
        let manifest: wiki::transfer::Manifest =
            serde_json::from_value(raw.clone()).map_err(|error| {
                Error::rejected(format!(
                    "wiki transfer refused: manifest is invalid: {error}"
                ))
            })?;
        if manifest.root != root {
            return Err(Error::rejected(
                "wiki transfer refused: manifest root does not match the request root",
            ));
        }
        let job_id = match params.get("job_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => Some(id.as_str()),
            Some(_) => {
                return Err(Error::rejected(
                    "wiki transfer begin refused: job_id must be a string",
                ))
            }
        };
        wiki::transfer::begin(
            &self.store,
            &self.pm()?,
            caller,
            owner,
            job_id,
            &manifest,
            raw_len,
        )
    }

    fn transfer_chunk(&self, params: &Value, caller: &wiki::Caller, owner: &str) -> Result<Value> {
        let offset = wiki::json_u64(params, "offset")?
            .ok_or_else(|| Error::rejected("wiki transfer chunk refused: offset is required"))?;
        wiki::transfer::chunk(
            &self.store,
            &self.state_dir,
            &wiki::transfer::JobRequest {
                caller,
                owner,
                job_id: required_str(params, "job_id")?,
                root: required_str(params, "root")?,
            },
            required_str(params, "entry_id")?,
            offset,
            required_str(params, "data_base64")?,
        )
    }

    fn transfer_apply(
        &self,
        params: &Value,
        caller: &wiki::Caller,
        owner: &str,
        human: Option<&Value>,
    ) -> Result<Value> {
        let job_id = required_str(params, "job_id")?;
        let root = required_str(params, "root")?;
        let entry_id = required_str(params, "entry_id")?;
        let job = self
            .store
            .wiki_transfer_job(job_id)?
            .filter(|job| job.owner_key == owner && job.root == root)
            .ok_or_else(|| Error::rejected("wiki transfer job not found"))?;
        let entry = self
            .store
            .wiki_transfer_entry(job_id, entry_id)?
            .filter(|entry| {
                entry.path.starts_with(&format!("{}/", job.root)) || entry.path == job.root
            })
            .ok_or_else(|| Error::rejected("wiki transfer job not found"))?;
        let action = match entry.kind.as_str() {
            "text" => "wiki_written",
            "blob" => "wiki_blob_uploaded",
            "dir" => "wiki_directory_created",
            _ => return Err(Error::internal("wiki transfer entry has an invalid kind")),
        };
        let pm = self.wiki_receipt(
            self.pm()?,
            caller,
            human,
            action,
            &[(entry.path.as_str(), wiki::Op::Write)],
        )?;
        wiki::transfer::apply(
            &self.store,
            &pm,
            &self.state_dir,
            &wiki::transfer::JobRequest {
                caller,
                owner,
                job_id,
                root,
            },
            entry_id,
        )
    }

    fn transfer_finish(
        &self,
        params: &Value,
        caller: &wiki::Caller,
        owner: &str,
        human: Option<&Value>,
    ) -> Result<Value> {
        let root = required_str(params, "root")?;
        let pm = self.wiki_receipt(
            self.pm()?,
            caller,
            human,
            "wiki_directory_created",
            &[(root, wiki::Op::Write)],
        )?;
        let (body, kick) = wiki::transfer::finish(
            &self.store,
            &pm,
            &self.state_dir,
            &wiki::transfer::JobRequest {
                caller,
                owner,
                job_id: required_str(params, "job_id")?,
                root: required_str(params, "root")?,
            },
        )?;
        if kick {
            self.wiki_index.kick();
        }
        Ok(body)
    }

    fn transfer_status(&self, params: &Value, caller: &wiki::Caller, owner: &str) -> Result<Value> {
        let offset = wiki::json_u64(params, "offset")?.unwrap_or(0);
        let limit = wiki::json_u64(params, "limit")?.unwrap_or(wiki::transfer::STATUS_PAGE_MAX);
        wiki::transfer::status(
            &self.store,
            &self.pm()?,
            &self.state_dir,
            &wiki::transfer::JobRequest {
                caller,
                owner,
                job_id: required_str(params, "job_id")?,
                root: required_str(params, "root")?,
            },
            offset,
            limit,
        )
    }

    fn transfer_export(&self, params: &Value, caller: &wiki::Caller) -> Result<Value> {
        let root = required_str(params, "root")?;
        let path = required_str(params, "path")?;
        if path != root {
            return Err(Error::rejected(
                "wiki transfer export refused: path does not match the request root",
            ));
        }
        let offset = wiki::json_u64(params, "offset")?.unwrap_or(0);
        let bytes = wiki::json_u64(params, "bytes")?;
        let rev = match params.get("rev") {
            None | Some(Value::Null) => None,
            Some(Value::String(rev)) => Some(rev.as_str()),
            Some(_) => {
                return Err(Error::rejected(
                    "wiki transfer export refused: rev must be a string",
                ))
            }
        };
        let at = export_at(params)?;
        wiki::transfer::export(&self.pm()?, caller, path, offset, bytes, rev, at)
    }
}

/// The `at` historical selector for a transfer export. Only an omitted key
/// selects the live export; a present value must be a non-empty revision
/// selector, never an implicit live read.
fn export_at(params: &Value) -> Result<Option<&str>> {
    match params.get("at") {
        None => Ok(None),
        Some(Value::String(at)) if !at.is_empty() => Ok(Some(at.as_str())),
        Some(_) => Err(Error::rejected(
            "wiki transfer export refused: at must be a non-empty revision selector",
        )),
    }
}

/// A named board user's handle — the `[A-Za-z0-9._-]` grammar, 1-40
/// chars (the same shape a board session's `author` carries).
fn valid_user_handle(handle: &str) -> bool {
    !handle.is_empty()
        && handle.len() <= 40
        && handle
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only an omitted `at` selects the live export: a present null, empty
    /// or non-string is refused rather than silently reading live bytes.
    #[test]
    fn export_selector_refuses_a_present_non_string() {
        assert_eq!(
            export_at(&serde_json::json!({})).unwrap(),
            None,
            "an omitted selector selects the live export"
        );
        assert_eq!(
            export_at(&serde_json::json!({"at": "fnv1a:0123456789abcdef"})).unwrap(),
            Some("fnv1a:0123456789abcdef")
        );
        for bad in [
            serde_json::json!({"at": null}),
            serde_json::json!({"at": ""}),
            serde_json::json!({"at": 7}),
            serde_json::json!({"at": ["a"]}),
        ] {
            assert!(
                export_at(&bad).is_err(),
                "{bad} was read as the live export"
            );
        }
    }
}
