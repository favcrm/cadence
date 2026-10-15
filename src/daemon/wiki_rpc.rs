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
    /// registered cwd belongs to (`projects/<key>` write check).
    fn agent_wiki_caller(&self, alias: &str) -> Caller {
        let project = self
            .store
            .agent_opt(alias)
            .ok()
            .flatten()
            .and_then(|agent| {
                self.pm_dir()
                    .ok()
                    .and_then(|dir| crate::issue::project::key_for_cwd(&dir, Path::new(&agent.cwd)))
            });
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
    fn wiki_human(&self, params: &Value, caller: &Caller, verb: &str) -> Result<Option<Value>> {
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
        Ok(Some(json!({
            "company": realm.company,
            "sub": sub,
            "session": view.id,
            "display": display,
            "role": role.as_str(),
        })))
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
        paths: &[&str],
    ) -> Result<crate::issue::Pm> {
        let mut resource = String::new();
        for path in paths {
            let norm = wiki::normalize(path)?;
            let segs: Vec<&str> = norm.split('/').collect();
            wiki::allowed(caller, wiki::Op::Write, &segs)?;
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
        let human = self.wiki_human(params, &caller, "wiki write")?;
        let pm = self.wiki_receipt(self.pm()?, &caller, human.as_ref(), "wiki_written", &[path])?;
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
        let human = self.wiki_human(params, &caller, "wiki put_blob")?;
        let pm = self.wiki_receipt(
            self.pm()?,
            &caller,
            human.as_ref(),
            "wiki_blob_uploaded",
            &[path],
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
        let human = self.wiki_human(params, &caller, "wiki mkdir")?;
        let pm = self.wiki_receipt(
            self.pm()?,
            &caller,
            human.as_ref(),
            "wiki_directory_created",
            &[path],
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
        let human = self.wiki_human(params, &caller, "wiki mv")?;
        // A `.trash/` source is an operator-only restore that `wiki::mv`
        // gates itself; only a real source path needs the ACL check here.
        let sources: Vec<&str> = if from.starts_with(".trash/") {
            vec![to]
        } else {
            vec![from, to]
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
        let human = self.wiki_human(params, &caller, "wiki rm")?;
        let pm = self.wiki_receipt(self.pm()?, &caller, human.as_ref(), "wiki_removed", &[path])?;
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
