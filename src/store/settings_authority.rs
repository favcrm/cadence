//! One SQLite authority for model defaults, Pi policy, and profile text.
//!
//! Model defaults stay the `model_defaults` document. Pi policy and profile
//! text stay the `settings_authority` row created by schema 52. Both commit
//! in the same `write_tx`. `pi` NULL is an absent policy (`None`): every Pi
//! launch denies. The table is not created from a request handler.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rusqlite::params;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::model_defaults::ModelDefaults;
use crate::pi_policy::PiPolicy;

use super::{Store, StoreConn};

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileBody {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct SettingsDoc {
    pub model_defaults: ModelDefaults,
    /// `None` is an absent policy and denies every Pi launch. `Some`,
    /// including an empty allow list, is a stored policy.
    pub pi: Option<PiPolicy>,
    pub profiles: Vec<ProfileBody>,
}

#[derive(Clone, Debug)]
pub struct SettingsSnapshot {
    pub revision: i64,
    pub doc: SettingsDoc,
    pub legacy_imported: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct AuthorityBlob {
    pub pi: Option<PiPolicy>,
    pub profiles: Vec<ProfileBody>,
    pub legacy_imported: bool,
}

/// Schema 52 DDL, applied after the independent v51 artifact-fetch migration.
/// Request handlers never execute it.
pub(super) const SCHEMA_V52: &str = "
CREATE TABLE IF NOT EXISTS settings_authority(
    id INTEGER PRIMARY KEY CHECK (id = 1),
    pi TEXT,
    profiles TEXT NOT NULL DEFAULT '[]',
    legacy_imported INTEGER NOT NULL DEFAULT 0
);
INSERT INTO settings_authority(id, pi, profiles, legacy_imported)
  SELECT 1, NULL, '[]', 0
  WHERE NOT EXISTS (SELECT 1 FROM settings_authority WHERE id = 1);
";

/// Move a sibling `authority` object out of the model-defaults document.
/// A document without that key is left unchanged. NULL `pi` stays absent.
pub(super) fn lift_embedded_authority(conn: &rusqlite::Connection) -> Result<()> {
    let document: String = match conn.query_row(
        "SELECT document FROM model_defaults WHERE id=1",
        [],
        |row| row.get(0),
    ) {
        Ok(document) => document,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let mut raw: serde_json::Value = serde_json::from_str(&document)
        .map_err(|err| Error::internal(format!("stored model defaults are invalid: {err}")))?;
    let Some(auth) = raw.as_object_mut().and_then(|obj| obj.remove("authority")) else {
        return Ok(());
    };
    let pi = match auth.get("pi") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(serde_json::to_string(value)?),
    };
    let profiles = serde_json::to_string(auth.get("profiles").unwrap_or(&json!([])))?;
    let imported = auth
        .get("legacy_imported")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    conn.execute(
        "UPDATE settings_authority SET pi=?, profiles=?, legacy_imported=? WHERE id=1",
        params![pi, profiles, imported],
    )?;
    conn.execute(
        "UPDATE model_defaults SET document=? WHERE id=1",
        params![serde_json::to_string(&raw)?],
    )?;
    Ok(())
}

impl Store {
    pub fn settings(&self) -> Result<SettingsSnapshot> {
        self.read_tx(|conn| {
            let tx = &mut *conn;
            read_settings(tx)
        })
    }

    /// Fresh launch policy without decoding unrelated profile bodies.
    pub fn pi_policy(&self) -> Result<Option<PiPolicy>> {
        self.read_tx(|conn| {
            let (pi, imported): (Option<String>, i64) = conn.query_row(
                "SELECT pi, legacy_imported FROM settings_authority WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if imported != 1 {
                return Err(Error::rejected(
                    "Pi settings have not been imported into the settings authority",
                ));
            }
            parse_pi_document(pi.as_deref())
        })
    }

    /// Copy `pm.yaml [pi]` and `agents/*/AGENT.md` once. A row with
    /// `legacy_imported` set is returned unchanged.
    pub fn import_legacy_settings(&self, pm_dir: &Path) -> Result<SettingsSnapshot> {
        let current = self.settings()?;
        if current.legacy_imported {
            return Ok(current);
        }
        let pi = read_legacy_pi(pm_dir)?;
        if let Some(policy) = &pi {
            policy.validate_public()?;
        }
        let profiles = read_legacy_profiles(pm_dir)?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let current = read_settings(&*tx)?;
            if current.legacy_imported {
                return Ok(current);
            }
            let next = current
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::invalid("invalid_request", "settings revision overflowed"))?;
            let mut defaults = current.doc.model_defaults.clone();
            if let Some(policy) = &pi {
                merge_policy_projection(&mut defaults, policy);
            }
            let stored = model_document(&defaults)?;
            write_authority(
                &tx,
                &AuthorityBlob {
                    pi: pi.clone(),
                    profiles: profiles.clone(),
                    legacy_imported: true,
                },
            )?;
            tx.execute(
                "UPDATE model_defaults SET revision=?, document=? WHERE id=1",
                params![next, stored],
            )?;
            Self::event(
                &tx,
                Self::DAEMON_STREAM,
                "settings_legacy_imported",
                json!({
                    "revision": next,
                    "profiles": profiles.len(),
                    "changes": profile_changes(&[], &profiles),
                }),
            )?;
            let mut doc = current.doc;
            doc.model_defaults = defaults;
            doc.pi = pi;
            doc.profiles = profiles;
            Ok(SettingsSnapshot {
                revision: next,
                doc,
                legacy_imported: true,
            })
        })
    }

    /// Compare-and-swap the model-defaults document, Pi policy, and
    /// profile bodies. `doc` is stored as given: omitting a provider or
    /// role removes it. `pi: None` stays an absent policy.
    pub fn save_settings(
        &self,
        expected: i64,
        mut doc: SettingsDoc,
        actor: &str,
    ) -> Result<SettingsSnapshot> {
        guard_launch_ids(&doc)?;
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let current = read_settings(&*tx)?;
            if current.revision != expected {
                return Err(Error::conflict(
                    current.revision,
                    format!("settings revision is {}, not {expected}", current.revision),
                ));
            }
            reject_nested_authority(&doc.model_defaults)?;
            crate::model_defaults::validate_config(&mut doc.model_defaults)?;
            let stored = model_document(&doc.model_defaults)?;
            write_authority(
                &tx,
                &AuthorityBlob {
                    pi: doc.pi.clone(),
                    profiles: doc.profiles.clone(),
                    legacy_imported: true,
                },
            )?;
            if stored.len() > 256 * 1024 {
                return Err(Error::invalid(
                    "invalid_config",
                    "settings document exceeds 256 KiB",
                ));
            }
            let next = current
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::invalid("invalid_request", "settings revision overflowed"))?;
            tx.execute(
                "UPDATE model_defaults SET revision=?, document=? WHERE id=1",
                params![next, stored],
            )?;
            Self::event(
                &tx,
                Self::DAEMON_STREAM,
                "settings_updated",
                json!({
                    "revision": next,
                    "actor": actor,
                    "changes": profile_changes(&current.doc.profiles, &doc.profiles),
                }),
            )?;
            Ok(SettingsSnapshot {
                revision: next,
                doc,
                legacy_imported: true,
            })
        })
    }
}

pub(super) fn load_model_defaults_row(
    conn: &impl StoreConn,
) -> Result<(i64, ModelDefaults, AuthorityBlob)> {
    let (revision, document): (i64, String) = conn
        .query_row(
            "SELECT revision, document FROM model_defaults WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::internal("model defaults row is missing")
            }
            other => Error::from(other),
        })?;
    let config = parse_model_document(&document)?;
    let authority = read_authority(conn)?;
    Ok((revision, config, authority))
}

fn read_settings(conn: &impl StoreConn) -> Result<SettingsSnapshot> {
    let (revision, config, authority) = load_model_defaults_row(conn)?;
    Ok(SettingsSnapshot {
        revision,
        doc: SettingsDoc {
            model_defaults: config,
            pi: authority.pi,
            profiles: authority.profiles,
        },
        legacy_imported: authority.legacy_imported,
    })
}

fn parse_model_document(document: &str) -> Result<ModelDefaults> {
    let raw: serde_json::Value = serde_json::from_str(document)
        .map_err(|err| Error::internal(format!("stored model defaults are invalid: {err}")))?;
    if raw.get("authority").is_some() {
        return Err(Error::internal(
            "model defaults document still embeds settings authority",
        ));
    }
    let mut config: ModelDefaults = serde_json::from_value(raw)
        .map_err(|err| Error::internal(format!("stored model defaults are invalid: {err}")))?;
    crate::model_defaults::validate_config(&mut config)?;
    Ok(config)
}

pub(super) fn join_document(config: &ModelDefaults, _authority: &AuthorityBlob) -> Result<String> {
    model_document(config)
}

fn model_document(config: &ModelDefaults) -> Result<String> {
    reject_nested_authority(config)?;
    Ok(serde_json::to_string(config)?)
}

fn parse_pi_document(document: Option<&str>) -> Result<Option<PiPolicy>> {
    let Some(document) = document else {
        return Ok(None);
    };
    let policy = serde_json::from_str::<PiPolicy>(document)
        .map_err(|err| Error::internal(format!("stored pi policy is invalid: {err}")))?;
    policy.validate_public()?;
    Ok(Some(policy))
}

fn read_authority(conn: &impl StoreConn) -> Result<AuthorityBlob> {
    // Missing table or row is a schema error. Do not create the table
    // and do not invent an empty policy for a store that never migrated.
    let (pi, profiles, imported): (Option<String>, String, i64) = conn
        .query_row(
            "SELECT pi, profiles, legacy_imported FROM settings_authority WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::internal("settings_authority row is missing; schema 52 is required")
            }
            other => Error::from(other),
        })?;
    let pi = parse_pi_document(pi.as_deref())?;
    let raw: Vec<serde_json::Value> = serde_json::from_str(&profiles)
        .map_err(|err| Error::internal(format!("stored profiles are invalid: {err}")))?;
    let mut bodies = Vec::new();
    for row in raw {
        let id = row.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        bodies.push(ProfileBody {
            id: id.to_string(),
            text: row
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        });
    }
    Ok(AuthorityBlob {
        pi,
        profiles: bodies,
        legacy_imported: imported != 0,
    })
}

fn write_authority(conn: &impl StoreConn, authority: &AuthorityBlob) -> Result<()> {
    if let Some(policy) = &authority.pi {
        policy.validate_public()?;
    }
    let pi = match &authority.pi {
        None => None,
        Some(policy) => Some(serde_json::to_string(policy)?),
    };
    let profiles = serde_json::to_string(
        &authority
            .profiles
            .iter()
            .map(|p| json!({"id": p.id, "text": p.text}))
            .collect::<Vec<_>>(),
    )?;
    conn.execute(
        "UPDATE settings_authority SET pi=?, profiles=?, legacy_imported=? WHERE id=1",
        params![pi, profiles, authority.legacy_imported],
    )?;
    Ok(())
}

pub(super) fn reject_nested_authority(config: &ModelDefaults) -> Result<()> {
    for key in ["pi", "profiles", "authority", "model_defaults"] {
        if config.agent_settings.get(key).is_some() {
            return Err(Error::invalid(
                "invalid_config",
                "agent_settings cannot carry Pi policy, profiles, or the settings authority",
            ));
        }
    }
    Ok(())
}

/// Allowlisted ids and stored selectors are checked with the existing
/// safe-transport rule before a settings transaction starts.
pub(crate) fn guard_launch_ids(doc: &SettingsDoc) -> Result<()> {
    if let Some(policy) = &doc.pi {
        policy.validate_public()?;
        for model in policy
            .models
            .allow
            .iter()
            .chain(policy.models.master_allow.iter().flatten())
            .chain(policy.models.worker_allow.iter().flatten())
            .chain(policy.models.default.master.iter())
            .chain(policy.models.default.worker.iter())
        {
            crate::pi_policy::require_safe_transport(model)?;
        }
    }
    // Cursor-through-Pi is a Pi transport rule. Native provider selectors,
    // including cursor/<model>, are not Pi launches.
    if let Some(provider) = doc.model_defaults.providers.get("pi") {
        for selector in std::iter::once(&provider.default).chain(provider.roles.values()) {
            if let crate::model_defaults::ModelSelector::Model { model } = selector {
                crate::pi_policy::require_safe_transport(model)?;
            }
        }
    }
    Ok(())
}

fn profile_changes(before: &[ProfileBody], after: &[ProfileBody]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for next in after {
        match before.iter().find(|row| row.id == next.id) {
            None => out.push(json!({
                "id": next.id,
                "what": "created",
                "before": Value::Null,
                "after": next.text,
            })),
            Some(prev) if prev.text != next.text => out.push(json!({
                "id": next.id,
                "what": "edited",
                "before": prev.text,
                "after": next.text,
                "tombstone": false,
            })),
            Some(_) => {}
        }
    }
    for prev in before {
        if after.iter().all(|row| row.id != prev.id) {
            out.push(json!({
                "id": prev.id,
                "what": "deleted",
                "before": prev.text,
                "after": Value::Null,
                "tombstone": true,
            }));
        }
    }
    out
}

/// Copy configured Pi ids, pins, and role defaults into the editable
/// overlay. Existing overlay entries stay. Empty context fields are not
/// catalog metadata.
fn merge_policy_projection(defaults: &mut crate::model_defaults::ModelDefaults, policy: &PiPolicy) {
    let mut overlay = if defaults.agent_settings.is_object() {
        defaults.agent_settings.clone()
    } else {
        json!({
            "harnesses": [],
            "providers": [],
            "models": [],
            "scope": {},
            "roleDefaults": {},
            "pins": [],
        })
    };
    let scopes = policy_scopes(policy);
    if !scopes.is_empty() || !policy.providers.is_empty() {
        let providers = overlay.get_mut("providers").and_then(Value::as_array_mut);
        let has_pi = providers.as_ref().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["kind"].as_str() == Some("Pi extension"))
        });
        if !has_pi {
            if let Some(rows) = overlay
                .as_object_mut()
                .and_then(|obj| obj.get_mut("providers"))
                .and_then(Value::as_array_mut)
            {
                rows.push(json!({
                    "id": "pi",
                    "label": "Pi",
                    "kind": "Pi extension",
                    "pin": "",
                    "gate": true,
                    "paid": false,
                    "note": "",
                }));
            }
        }
    }
    if let Some(models) = overlay.get_mut("models").and_then(Value::as_array_mut) {
        for id in scopes.keys() {
            if models.iter().any(|row| row["id"].as_str() == Some(id)) {
                continue;
            }
            models.push(json!({
                "id": id,
                "provider": "pi",
                "ctx": "",
                "out": "",
                "thinking": null,
                "images": null,
            }));
        }
    }
    if let Some(scope) = overlay.get_mut("scope").and_then(Value::as_object_mut) {
        for (id, mode) in &scopes {
            scope.entry(id.clone()).or_insert(json!(mode));
        }
    }
    if let Some(pins) = overlay.get_mut("pins").and_then(Value::as_array_mut) {
        for spec in &policy.providers {
            let Some((pkg, pinned)) = spec.rsplit_once('@') else {
                continue;
            };
            if pins.iter().any(|row| row["pkg"].as_str() == Some(pkg)) {
                continue;
            }
            pins.push(json!({"pkg": pkg, "pinned": pinned}));
        }
    }
    if let Some(roles) = overlay
        .get_mut("roleDefaults")
        .and_then(Value::as_object_mut)
    {
        if let Some(model) = policy.models.default.worker.as_deref() {
            if scopes
                .get(model)
                .is_some_and(|mode| matches!(mode.as_str(), "allow" | "worker"))
            {
                let slot = roles.entry("worker".to_string()).or_insert(json!(""));
                if slot.as_str().unwrap_or("").is_empty() {
                    *slot = json!(model);
                }
            }
        }
    }
    defaults.agent_settings = overlay;
}

fn policy_scopes(policy: &PiPolicy) -> BTreeMap<String, String> {
    let mut ids: Vec<&str> = policy
        .models
        .allow
        .iter()
        .chain(policy.models.master_allow.iter().flatten())
        .chain(policy.models.worker_allow.iter().flatten())
        .chain(policy.models.default.master.iter())
        .chain(policy.models.default.worker.iter())
        .map(String::as_str)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .map(|id| {
            let master = policy
                .models
                .allow_for("master")
                .iter()
                .any(|row| row == id);
            let worker = policy
                .models
                .allow_for("worker")
                .iter()
                .any(|row| row == id);
            let mode = match (master, worker) {
                (true, true) => "allow",
                (true, false) => "master",
                (false, true) => "worker",
                (false, false) => "off",
            };
            (id.to_string(), mode.to_string())
        })
        .collect()
}

enum AtKind {
    Missing,
    Symlink,
    Dir,
    File,
    Other,
}

fn import_refused(message: impl Into<String>) -> Error {
    Error::rejected(message.into())
}

fn c_name(name: &str) -> Result<CString> {
    CString::new(name)
        .map_err(|_| import_refused("path component contains an interior nul; import refused"))
}

/// The configured PM directory may itself be a symlink the operator chose.
/// Everything opened underneath it is no-follow.
fn open_pm_root(pm_dir: &Path) -> Result<Option<OwnedFd>> {
    let name = CString::new(pm_dir.as_os_str().as_bytes())
        .map_err(|_| import_refused("pm dir contains an interior nul; import refused"))?;
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(import_refused(format!(
            "cannot open {}: {err}; import refused",
            pm_dir.display()
        )));
    }
    Ok(Some(unsafe { OwnedFd::from_raw_fd(fd) }))
}

fn at_kind(dir: &OwnedFd, name: &str) -> Result<AtKind> {
    let c_name = c_name(name)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            return Ok(AtKind::Missing);
        }
        return Err(import_refused(format!(
            "cannot stat {name}: {err}; import refused"
        )));
    }
    Ok(match st.st_mode & libc::S_IFMT {
        mode if mode == libc::S_IFLNK => AtKind::Symlink,
        mode if mode == libc::S_IFDIR => AtKind::Dir,
        mode if mode == libc::S_IFREG => AtKind::File,
        _ => AtKind::Other,
    })
}

fn open_dir_at(dir: &OwnedFd, name: &str) -> Result<OwnedFd> {
    let c_name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(import_refused(format!(
            "{name} is not a real directory; import refused"
        )));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_file_at(dir: &OwnedFd, name: &str) -> Result<File> {
    let c_name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        return Err(import_refused(format!(
            "cannot open {name}: {err}; import refused"
        )));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file
        .metadata()
        .map_err(|err| import_refused(format!("cannot stat {name}: {err}; import refused")))?;
    if !meta.file_type().is_file() {
        return Err(import_refused(format!(
            "{name} is not a regular file; import refused"
        )));
    }
    Ok(file)
}

fn read_limited(mut file: File, label: &str, cap: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    Read::take(&mut file, cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| import_refused(format!("cannot read {label}: {err}; import refused")))?;
    if bytes.len() as u64 > cap {
        return Err(import_refused(format!(
            "{label} exceeds the import size cap; import refused"
        )));
    }
    Ok(bytes)
}

fn dir_names(dir: &OwnedFd) -> Result<Vec<String>> {
    let dup = unsafe { libc::dup(dir.as_raw_fd()) };
    if dup < 0 {
        return Err(import_refused("cannot list agents; import refused"));
    }
    let ptr = unsafe { libc::fdopendir(dup) };
    if ptr.is_null() {
        unsafe { libc::close(dup) };
        return Err(import_refused("cannot list agents; import refused"));
    }
    let mut names = Vec::new();
    loop {
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        let ent = unsafe { libc::readdir(ptr) };
        if ent.is_null() {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error().unwrap_or(0) != 0 {
                unsafe { libc::closedir(ptr) };
                return Err(import_refused(format!(
                    "cannot list agents: {err}; import refused"
                )));
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        let Ok(name) = name.to_str() else {
            unsafe { libc::closedir(ptr) };
            return Err(import_refused(
                "agents entry is not valid UTF-8; import refused",
            ));
        };
        if name == "." || name == ".." {
            continue;
        }
        names.push(name.to_string());
    }
    unsafe { libc::closedir(ptr) };
    Ok(names)
}

/// Only a missing `pm.yaml` is absence. Permission errors, symlinks,
/// invalid UTF-8, and malformed YAML refuse without importing.
fn read_legacy_pi(pm_dir: &Path) -> Result<Option<PiPolicy>> {
    let Some(root) = open_pm_root(pm_dir)? else {
        return Ok(None);
    };
    match at_kind(&root, "pm.yaml")? {
        AtKind::Missing => return Ok(None),
        AtKind::Symlink => return Err(import_refused("pm.yaml is a symlink; import refused")),
        AtKind::File => {}
        AtKind::Dir | AtKind::Other => {
            return Err(import_refused(
                "pm.yaml is not a regular file; import refused",
            ))
        }
    }
    let file = open_file_at(&root, "pm.yaml")?;
    let bytes = read_limited(file, "pm.yaml", 1024 * 1024)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| import_refused("pm.yaml is not valid UTF-8; import refused"))?;
    crate::pi_policy::parse_text(&text)
}

fn read_legacy_profiles(pm_dir: &Path) -> Result<Vec<ProfileBody>> {
    let Some(root) = open_pm_root(pm_dir)? else {
        return Ok(Vec::new());
    };
    match at_kind(&root, "agents")? {
        AtKind::Missing => return Ok(Vec::new()),
        AtKind::Symlink => return Err(import_refused("agents is a symlink; import refused")),
        AtKind::Dir => {}
        AtKind::File | AtKind::Other => {
            return Err(import_refused("agents is not a directory; import refused"))
        }
    }
    let agents = open_dir_at(&root, "agents")?;
    let mut out = Vec::new();
    for slug in dir_names(&agents)? {
        // Master SOUL/AGENT/permissions stay on the digest gate.
        if slug == "master" {
            continue;
        }
        match at_kind(&agents, &slug)? {
            AtKind::Symlink => {
                return Err(import_refused(format!(
                    "agents/{slug} is a symlink; import refused"
                )))
            }
            AtKind::Dir => {}
            AtKind::Missing => continue,
            AtKind::File | AtKind::Other => {
                return Err(import_refused(format!(
                    "agents/{slug} is not a profile directory; import refused"
                )))
            }
        }
        let dir = open_dir_at(&agents, &slug)?;
        let label = format!("agents/{slug}/AGENT.md");
        match at_kind(&dir, "AGENT.md")? {
            AtKind::Missing => continue,
            AtKind::Symlink => {
                return Err(import_refused(format!(
                    "{label} is a symlink; import refused"
                )))
            }
            AtKind::File => {}
            AtKind::Dir | AtKind::Other => {
                return Err(import_refused(format!(
                    "{label} is not a regular file; import refused"
                )))
            }
        }
        if slug.is_empty()
            || slug.len() > 64
            || !slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(import_refused(format!(
                "{label} has an unsupported name; import refused"
            )));
        }
        let file = open_file_at(&dir, "AGENT.md")?;
        let bytes = read_limited(file, &label, 256 * 1024)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| import_refused(format!("{label} is not valid UTF-8; import refused")))?;
        if let Err(err) = crate::master::check_file("AGENT.md", &text) {
            return Err(import_refused(format!("{label} cannot be imported: {err}")));
        }
        crate::secret::guard(&label, &text)?;
        out.push(ProfileBody { id: slug, text });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

impl Store {
    /// Latest settings commits. A database that has never saved settings
    /// returns an empty list; each `save_settings` appends one row.
    pub fn settings_history(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        let limit = limit.clamp(1, 50);
        self.read_tx(|conn| {
            let rows = conn.query_vec(
                "SELECT seq, kind, payload, at FROM events
                 WHERE kind IN ('settings_updated', 'settings_legacy_imported')
                 ORDER BY seq DESC LIMIT ?1",
                params![limit],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                },
            )?;
            let mut out = Vec::new();
            for (seq, kind, payload, at) in rows {
                let mut value: serde_json::Value =
                    serde_json::from_str(&payload).unwrap_or_else(|_| json!({"payload": payload}));
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("seq".to_string(), json!(seq));
                    obj.insert("kind".to_string(), json!(kind));
                    obj.insert("at".to_string(), json!(at));
                }
                out.push(value);
            }
            Ok(out)
        })
    }
}
