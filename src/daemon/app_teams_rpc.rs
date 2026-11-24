//! CAD-1123 HP2: the default team of a workspace installation, set once by
//! the operator in Settings → Apps. `app_run_start` takes the owner PM and
//! every workflow worker role from here, so an app (or a forged request) can
//! never name them. Stored beside the daemon's other state as one small
//! file; a corrupt file refuses (it is never read as "no team").
use super::app_team_state::{load, parse, path, team_of, Team, LOCK};
use super::*;
use crate::issue::app_catalog::workspace;
use std::collections::BTreeMap;

const ROLES_MAX: usize = 8;

fn view(team: Option<&Team>) -> Value {
    match team {
        None => json!({"team": null}),
        Some(t) => {
            json!({"team": {"owner_pm": t.owner_pm, "roles": t.roles, "revision": t.revision}})
        }
    }
}

impl Shared {
    pub(super) fn rpc_app_team(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app team", params, peer_pid)?;
        let allowed: &[&str] = match method {
            "app_install_team_show" => &["install_id"],
            "app_install_team_set" => &["install_id", "owner_pm", "roles", "expected_revision"],
            _ => return Err(Error::rejected("unknown app team method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app team payload must be an object"))?;
        if fields.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::rejected("app team payload has unsupported fields"));
        }
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation ID")?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_read(&pm, install, |_, _| Ok(()))?;
        if method == "app_install_team_show" {
            return Ok(view(team_of(&self.state_dir, install)?.as_ref()));
        }
        let owner = required_str(params, "owner_pm")?;
        if self.store.agent(owner)?.role != "pm" {
            return Err(Error::rejected("team owner must be an existing PM"));
        }
        let roles: BTreeMap<String, String> = serde_json::from_value(
            params
                .get("roles")
                .cloned()
                .ok_or_else(|| Error::rejected("roles are required"))?,
        )
        .map_err(|_| Error::rejected("roles must be a string map"))?;
        if roles.is_empty() || roles.len() > ROLES_MAX {
            return Err(Error::rejected("a team needs 1 to 8 roles"));
        }
        for (role, alias) in &roles {
            crate::proto::identifier(role, "team role")?;
            if self.store.agent(alias)?.role != "worker" || alias == owner {
                return Err(Error::rejected("team roles must name registered workers"));
            }
        }
        let expected = params
            .get("expected_revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::rejected("expected_revision must be an unsigned integer"))?;
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut teams = load(&self.state_dir)?;
        let current = teams.get(install).map(parse).transpose()?;
        if current.as_ref().map_or(0, |t| t.revision) != expected {
            return Err(Error::rejected("team revision is stale"));
        }
        let team = Team {
            owner_pm: owner.to_string(),
            roles,
            revision: expected + 1,
        };
        teams.insert(
            install.to_string(),
            json!({"owner_pm": team.owner_pm, "roles": team.roles, "revision": team.revision}),
        );
        let text = serde_json::to_string(&json!({"schema": 1, "teams": teams}))
            .map_err(|e| Error::internal(e.to_string()))?;
        let target = path(&self.state_dir);
        let tmp = target.with_extension("json.tmp");
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| Error::internal(e.to_string()))?;
            file.write_all(text.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|e| Error::internal(e.to_string()))?;
        }
        std::fs::rename(&tmp, &target).map_err(|e| Error::internal(e.to_string()))?;
        Ok(view(Some(&team)))
    }
}
