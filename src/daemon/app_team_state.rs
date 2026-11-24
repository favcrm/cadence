use crate::error::{Error, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

const FILE: &str = "app-teams.json";
#[doc(hidden)]
pub static LOCK: StdMutex<()> = StdMutex::new(());

#[derive(Clone, Debug)]
pub struct Team {
    pub owner_pm: String,
    pub roles: BTreeMap<String, String>,
    pub revision: u64,
}

#[doc(hidden)]
pub fn path(state: &Path) -> PathBuf {
    state.join(FILE)
}

#[doc(hidden)]
pub fn load(state: &Path) -> Result<serde_json::Map<String, Value>> {
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

#[doc(hidden)]
pub fn parse(value: &Value) -> Result<Team> {
    let bad = || Error::rejected("app team store is corrupt");
    Ok(Team {
        owner_pm: value["owner_pm"].as_str().ok_or_else(bad)?.to_string(),
        roles: serde_json::from_value(value["roles"].clone()).map_err(|_| bad())?,
        revision: value["revision"].as_u64().ok_or_else(bad)?,
    })
}

pub fn team_of(state: &Path, install: &str) -> Result<Option<Team>> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load(state)?.get(install).map(parse).transpose()
}
