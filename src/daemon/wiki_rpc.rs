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
        wiki::read(&self.pm()?, &caller, Self::wiki_path(params)?)
    }

    pub(super) fn rpc_wiki_write(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki write")?;
        let path = Self::wiki_path(params)?;
        let text = required_str(params, "text")?;
        let if_rev = optional_str(params, "if_rev");
        wiki::write(&self.pm()?, &caller, path, text, if_rev)
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
        wiki::put_blob(
            &self.pm()?,
            &self.state_dir,
            &caller,
            path,
            &tmp,
            sha256,
            if_rev,
        )
    }

    pub(super) fn rpc_wiki_mkdir(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki mkdir")?;
        wiki::mkdir(&self.pm()?, &caller, Self::wiki_path(params)?)
    }

    pub(super) fn rpc_wiki_mv(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki mv")?;
        let from = required_str(params, "from")?;
        let to = required_str(params, "to")?;
        wiki::mv(&self.pm()?, &caller, from, to)
    }

    pub(super) fn rpc_wiki_rm(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki rm")?;
        wiki::rm(&self.pm()?, &caller, Self::wiki_path(params)?)
    }

    pub(super) fn rpc_wiki_search(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki search")?;
        let q = required_str(params, "q")?;
        let path = optional_str(params, "path").unwrap_or("");
        wiki::search(&self.pm()?, &caller, q, path)
    }

    pub(super) fn rpc_wiki_history(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_wiki_fields(params)?;
        let caller = self.wiki_caller(params, peer_pid, "wiki history")?;
        let path = Self::wiki_path(params)?;
        let limit = optional_u64(params, "limit").unwrap_or(50) as usize;
        wiki::history(&self.pm()?, &caller, path, limit)
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
