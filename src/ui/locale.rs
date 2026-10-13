//! Server-backed locale preferences for the hosted board. The board's
//! configured company is the organization boundary; the verified public
//! session supplies the user subject and role. No caller identity is
//! accepted from request data.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tiny_http::{Header, Request};

use super::{coded_response, err_response, json_response, read_body, HttpResp, ServeOpts};
use crate::operator_auth::BoardUser;

const FILE: &str = "locale-preferences.json";
const BODY_CAP: u64 = 2048;
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preferences {
    #[serde(default)]
    organizations: BTreeMap<String, String>,
    #[serde(default)]
    users: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    User,
    Organization,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    scope: Scope,
    #[serde(deserialize_with = "required_nullable_string")]
    locale: Option<String>,
}

fn required_nullable_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

fn supported(value: &str) -> bool {
    matches!(value, "en" | "zh-TW")
}

fn private_preferences(state_dir: &Path) -> Result<Preferences, HttpResp> {
    let path = crate::operator_auth::dir(state_dir).join(FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
        Err(error) => Err(err_response(
            500,
            &format!("locale preferences unavailable: {error}"),
        )),
        Ok(_) => {
            let bytes = crate::operator_auth::read_private(state_dir, FILE).map_err(|error| {
                err_response(500, &format!("locale preferences unavailable: {error}"))
            })?;
            serde_json::from_slice(&bytes).map_err(|error| {
                err_response(500, &format!("locale preferences are corrupt: {error}"))
            })
        }
    }
}

fn company(opts: &ServeOpts) -> Result<&str, HttpResp> {
    opts.public
        .as_ref()
        .map(|board| board.company.as_str())
        .ok_or_else(|| {
            err_response(
                404,
                "organization locale preferences are available only on a hosted board",
            )
        })
}

fn named_public_user(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
    write: bool,
) -> Result<BoardUser, HttpResp> {
    match super::operator::board_caller(request, state_dir, opts, write, None)? {
        super::operator::Caller::Named(_) => {}
        _ => {
            return Err(super::guard_fail(
                "board_session_required",
                "sign in to the hosted board to manage locale preferences",
            ))
        }
    }
    if opts.public.is_none() {
        return Err(super::guard_fail(
            "board_session_required",
            "sign in to the hosted board to manage locale preferences",
        ));
    }
    let session = super::operator::public_session(request, state_dir, opts)?.ok_or_else(|| {
        super::guard_fail(
            "board_session_required",
            "sign in to the hosted board to manage locale preferences",
        )
    })?;
    serde_json::from_value(session["user"].clone()).map_err(|_| {
        super::guard_fail(
            "board_session_required",
            "verified hosted user is unavailable",
        )
    })
}

fn no_store(mut response: HttpResp) -> HttpResp {
    response.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    response
}

fn locale_view(state_dir: &Path, organization: &str, user: &BoardUser) -> Result<Value, HttpResp> {
    let preferences = private_preferences(state_dir)?;
    let org_locale = preferences
        .organizations
        .get(organization)
        .filter(|locale| supported(locale))
        .cloned();
    let user_locale = preferences
        .users
        .get(organization)
        .and_then(|users| users.get(&user.sub))
        .filter(|locale| supported(locale))
        .cloned();
    Ok(json!({"organization_locale": org_locale, "user_locale": user_locale}))
}

pub(crate) fn get(request: &Request, state_dir: &Path, opts: &ServeOpts) -> HttpResp {
    let organization = match company(opts) {
        Ok(organization) => organization,
        Err(response) => return response,
    };
    let user = match named_public_user(request, state_dir, opts, false) {
        Ok(user) => user,
        Err(response) => return response,
    };
    match locale_view(state_dir, organization, &user) {
        Ok(value) => no_store(json_response(value)),
        Err(response) => response,
    }
}

pub(crate) fn post(request: &mut Request, state_dir: &Path, opts: &ServeOpts) -> HttpResp {
    let organization = match company(opts) {
        Ok(organization) => organization.to_string(),
        Err(response) => return response,
    };
    let user = match named_public_user(request, state_dir, opts, true) {
        Ok(user) => user,
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
    if update
        .locale
        .as_deref()
        .is_some_and(|locale| !supported(locale))
    {
        return coded_response(
            400,
            "unsupported_locale",
            "locale must be en, zh-TW, or null for a user preference",
            None,
        );
    }
    if matches!(update.scope, Scope::Organization) && update.locale.is_none() {
        return coded_response(
            400,
            "invalid_request",
            "organization locale cannot be cleared",
            None,
        );
    }
    if matches!(update.scope, Scope::Organization) && !user.is_operator() {
        return super::guard_fail(
            "member_role",
            "only an organization operator can change the organization locale",
        );
    }

    let _lock = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut preferences = match private_preferences(state_dir) {
        Ok(preferences) => preferences,
        Err(response) => return response,
    };
    match update.scope {
        Scope::User => {
            let users = preferences.users.entry(organization.clone()).or_default();
            if let Some(locale) = update.locale {
                users.insert(user.sub.clone(), locale);
            } else {
                users.remove(&user.sub);
                if users.is_empty() {
                    preferences.users.remove(&organization);
                }
            }
        }
        Scope::Organization => {
            preferences
                .organizations
                .insert(organization.clone(), update.locale.unwrap());
        }
    }
    if let Err(error) = crate::operator_auth::write_private(
        state_dir,
        FILE,
        &match serde_json::to_vec_pretty(&preferences) {
            Ok(bytes) => bytes,
            Err(error) => {
                return err_response(
                    500,
                    &format!("could not encode locale preferences: {error}"),
                )
            }
        },
    ) {
        return err_response(500, &format!("could not save locale preferences: {error}"));
    }
    match locale_view(state_dir, &organization, &user) {
        Ok(value) => no_store(json_response(value)),
        Err(response) => response,
    }
}
