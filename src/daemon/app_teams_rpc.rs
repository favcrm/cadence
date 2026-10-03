//! CAD-1123 HP2: the default team of a workspace installation, set once by
//! the operator in Settings → Apps. `app_run_start` takes the owner PM and
//! every workflow worker role from here, so an app (or a forged request) can
//! never name them. Stored beside the daemon's other state as one small
//! file; a corrupt file refuses (it is never read as "no team").
use super::*;
use crate::issue::app_catalog::workspace;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

const FILE: &str = "app-teams.json";
const ROLES_MAX: usize = 8;
static LOCK: StdMutex<()> = StdMutex::new(());

#[derive(Clone, Debug)]
pub(super) struct Team {
    pub owner_pm: String,
    pub roles: BTreeMap<String, String>,
    pub revision: u64,
}

fn path(state: &Path) -> PathBuf {
    state.join(FILE)
}

fn load(state: &Path) -> Result<serde_json::Map<String, Value>> {
    match std::fs::read_to_string(path(state)) {
        Ok(text) => {
            let doc: Value = serde_json::from_str(&text)
                .map_err(|_| Error::rejected("app team store is corrupt"))?;
            if doc["schema"] != 1 {
                return Err(Error::rejected("app team store is corrupt"));
            }
            doc["teams"]
                .as_object()
                .cloned()
                .ok_or_else(|| Error::rejected("app team store is corrupt"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(Error::internal(format!("app team store unreadable: {e}"))),
    }
}

fn parse(value: &Value) -> Result<Team> {
    let bad = || Error::rejected("app team store is corrupt");
    Ok(Team {
        owner_pm: value["owner_pm"].as_str().ok_or_else(bad)?.to_string(),
        roles: serde_json::from_value(value["roles"].clone()).map_err(|_| bad())?,
        revision: value["revision"].as_u64().ok_or_else(bad)?,
    })
}

pub(super) fn team_of(state: &Path, install: &str) -> Result<Option<Team>> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load(state)?.get(install).map(parse).transpose()
}

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
        workspace::with_runtime_snapshot(&pm, install, |_, _| Ok(()))?;
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
