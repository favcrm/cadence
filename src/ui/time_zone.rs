//! CAD-2378: the company time zone and a member's display zone.
//!
//! The company zone lives in the daemon store (`company_zone_get` /
//! `company_zone_set`): every schedule runs in it, and only the operator may
//! change it. A person's display zone changes only how times are shown to
//! them. It is a per-person preference kept beside the locale preferences, in
//! its own file so an older binary that does not know it keeps reading the
//! locale file unharmed. The identity comes from the verified session, never
//! from request data.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tiny_http::{Header, Request};

use super::operator::Caller;
use super::{coded_response, err_response, json_response, read_body, HttpResp, ServeOpts};
use crate::client;
use crate::store::workspace_settings::parse_zone;

const FILE: &str = "time-zone-preferences.json";
const BODY_CAP: u64 = 2048;
/// The native board has one operator and no member list.
const NATIVE_KEY: &str = "operator";
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Deserialize, Serialize)]
struct Preferences {
    #[serde(default)]
    users: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Company,
    User,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    scope: Scope,
    timezone: Option<String>,
}

/// Who is asking: the preference key and whether they own the workspace.
struct Viewer {
    key: String,
    owner: bool,
}

fn viewer(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
    write: bool,
) -> Result<Viewer, HttpResp> {
    match super::operator::board_caller(request, state_dir, opts, write, None)? {
        Caller::Named(named) => Ok(Viewer {
            key: named.sub,
            owner: named.operator,
        }),
        Caller::Operator(_) => Ok(Viewer {
            key: NATIVE_KEY.to_string(),
            owner: true,
        }),
        Caller::Agent(_) => Err(super::guard_fail(
            "board_session_required",
            "sign in to the board to manage time zones",
        )),
    }
}

fn preferences(state_dir: &Path) -> Result<Preferences, HttpResp> {
    let path = crate::operator_auth::dir(state_dir).join(FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
        Err(error) => Err(err_response(
            500,
            &format!("time zone preferences unavailable: {error}"),
        )),
        Ok(_) => {
            let bytes = crate::operator_auth::read_private(state_dir, FILE).map_err(|error| {
                err_response(500, &format!("time zone preferences unavailable: {error}"))
            })?;
            serde_json::from_slice(&bytes).map_err(|error| {
                err_response(500, &format!("time zone preferences are corrupt: {error}"))
            })
        }
    }
}

fn no_store(mut response: HttpResp) -> HttpResp {
    response.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    response
}

/// The reply: the company zone (null when the daemon predates the verb), the
/// caller's own display preference and the zone times are shown in. A stored
/// preference that no longer parses is ignored, never trusted.
fn view(state_dir: &Path, viewer: &Viewer) -> Result<Value, HttpResp> {
    let company = client::rpc(state_dir, "company_zone_get", json!({})).ok();
    let company_zone = company
        .as_ref()
        .and_then(|c| c["timezone"].as_str())
        .map(str::to_string);
    let display = preferences(state_dir)?
        .users
        .get(&viewer.key)
        .filter(|zone| parse_zone(zone).is_ok())
        .cloned();
    let effective = display.clone().or_else(|| company_zone.clone());
    Ok(json!({
        "company_timezone": company_zone,
        "company_timezone_source": company.as_ref().and_then(|c| c["source"].as_str()),
        "display_timezone": display,
        "effective_display_timezone": effective,
        "can_set_company": viewer.owner,
    }))
}

pub(crate) fn get(request: &Request, state_dir: &Path, opts: &ServeOpts) -> HttpResp {
    let viewer = match viewer(request, state_dir, opts, false) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    match view(state_dir, &viewer) {
        Ok(value) => no_store(json_response(value)),
        Err(response) => response,
    }
}

pub(crate) fn post(request: &mut Request, state_dir: &Path, opts: &ServeOpts) -> HttpResp {
    let viewer = match viewer(request, state_dir, opts, true) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let body = match read_body(request, BODY_CAP) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let update: Update = match super::parse_json(&body) {
        Ok(update) => update,
        Err(response) => return response,
    };
    let zone = match update.timezone.as_deref().map(parse_zone).transpose() {
        Ok(zone) => zone,
        Err(error) => {
            return coded_response(400, "invalid_timezone", &error.to_string(), None);
        }
    };
    match update.scope {
        Scope::Company => {
            if !viewer.owner {
                return super::guard_fail(
                    "member_role",
                    "only an organization operator can change the company time zone",
                );
            }
            let Some(zone) = zone else {
                return coded_response(
                    400,
                    "invalid_request",
                    "the company time zone cannot be cleared",
                    None,
                );
            };
            if let Err(error) = client::rpc(
                state_dir,
                "company_zone_set",
                json!({"timezone": zone.name()}),
            ) {
                return super::home::rpc_err(&error, "company_zone_set");
            }
        }
        Scope::User => {
            let _lock = WRITE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stored = match preferences(state_dir) {
                Ok(stored) => stored,
                Err(response) => return response,
            };
            match zone {
                Some(zone) => {
                    stored
                        .users
                        .insert(viewer.key.clone(), zone.name().to_string());
                }
                None => {
                    stored.users.remove(&viewer.key);
                }
            }
            let bytes = match serde_json::to_vec_pretty(&stored) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return err_response(
                        500,
                        &format!("could not encode time zone preferences: {error}"),
                    )
                }
            };
            if let Err(error) = crate::operator_auth::write_private(state_dir, FILE, &bytes) {
                return err_response(
                    500,
                    &format!("could not save time zone preferences: {error}"),
                );
            }
        }
    }
    match view(state_dir, &viewer) {
        Ok(value) => no_store(json_response(value)),
        Err(response) => response,
    }
}
