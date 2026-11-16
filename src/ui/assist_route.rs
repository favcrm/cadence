//! CAD-1380/AOS-216: `POST /api/assist/<verb>` — the container end of the
//! member-assistant remote surface (AgenticOS's
//! `POST /__platform/assistant/call` forwards here). A distinct route family
//! from `/api/cli/` so the member-plane verb allowlist and single-use
//! replay memory stay separate from the shared remote-CLI surface.
//!
//! The bearer is the credential: `Authorization: Bearer wikienv_<env>`
//! where `<env>` is the compact assistant actor envelope the issuer minted
//! ([`crate::assistant_actor`]). The route re-verifies it independently —
//! signature against the platform JWKS, `iss`/`aud`/org/time pins — then
//! the per-verb `assist.*` scope. Cookie sessions never satisfy this route
//! and this bearer satisfies no other route: `handle` diverts
//! `/api/assist/*` here before any session or write-gate check, and only on
//! this board's own public host.
//!
//! Member authority stays member-bound end to end: the signed envelope
//! is forwarded verbatim to the daemon as `member_envelope`, which
//! re-verifies the Ed25519 signature and every claim pin against its own
//! `board-identity.json` + the platform JWKS (the member proof is
//! daemon-verifiable, not a relayed field the relay could widen). The
//! daemon then re-binds the envelope's `{org,sub,assistant}` to the
//! stored assistant row it owns — never to operator authority and never
//! to a request field.
//!
//! Fail closed: a missing, forged, expired, wrong-audience or wrong-org
//! envelope, an unlisted verb, or an insufficient scope is a 401/403 with
//! the contract's `error.code` and no side effect. Operator-only and
//! cli/wiki verbs are not in the table at all.

use std::path::Path;
use std::sync::Mutex;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, StatusCode};

use super::write_path::{self, header_value, parse_json, read_body, write_err, HttpResp};
use super::ServeOpts;
use crate::assistant_actor::{self, AssistActor, AssistVerb};
use crate::board_identity;
use crate::client;

/// One process-wide JWKS cache for this route family (independent of the
/// cli family's). Keys on the issuer — a stale or foreign-issuer entry
/// refetches.
static ASSIST_JWKS: Mutex<board_identity::JwksCache> = Mutex::new(board_identity::JwksCache::new());

/// Serializes `Spent::load`+`consume` for the assist family.
static ASSIST_SPENT_LOCK: Mutex<()> = Mutex::new(());

/// A refusal in the contract's `error.code` shape.
fn assist_fail(status: u16, code: &str, message: &str) -> HttpResp {
    let body = serde_json::to_vec(&json!({
        "ok": false,
        "error": {"code": code, "message": message},
    }))
    .unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

fn status_of(code: &str) -> u16 {
    match code {
        "capability_unavailable" => 503,
        _ => 401,
    }
}

/// POST body → bounded JSON object; identity fields are refused outright —
/// a request field can never name the member, the assistant or authority.
fn read_args(request: &mut Request) -> std::result::Result<Value, HttpResp> {
    let bytes = read_body(request, write_path::JSON_CAP)?;
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(json!({}));
    }
    let value: Value = parse_json(&bytes)?;
    if !value.is_object() {
        return Err(assist_fail(
            400,
            "invalid_request",
            "assist call arguments must be a JSON object",
        ));
    }
    for field in [
        "actor",
        "role",
        "org",
        "user",
        "as",
        "principal",
        "operator",
        "assistant",
        "assistant_id",
        "wiki_as",
        "member_as",
        "member_envelope",
    ] {
        if value.get(field).is_some() {
            return Err(assist_fail(
                400,
                "invalid_request",
                "assist call arguments cannot carry an identity field",
            ));
        }
    }
    Ok(value)
}

/// Peek the signed header's `kid` for the JWKS lookup only.
fn peek_kid(envelope: &str) -> Result<String, HttpResp> {
    assistant_actor::peek_kid(envelope).ok_or_else(|| {
        assist_fail(
            401,
            "bad_header",
            "envelope header is not the contract shape",
        )
    })
}

/// Verify an assistant envelope into an [`AssistActor`] or answer the
/// refusal. This is the single verification the `/api/assist` bearer and
/// the CAD-1485 member-chat surface both run: signature against the
/// platform JWKS, `iss`/`aud`/org/time pins — identical pins either way.
/// The caller supplies the raw signed envelope; how it was obtained
/// (Authorization bearer vs a server-minted board-authorize answer) is
/// the caller's concern.
pub(super) fn verify_envelope(
    envelope: &str,
    public: &crate::ui::PublicBoard,
) -> Result<AssistActor, HttpResp> {
    let config = board_identity::Config {
        host: public.host.clone(),
        issuer: public.issuer.clone(),
        company: public.company.clone(),
    };
    let now = crate::issue::time::now_epoch();
    let kid = peek_kid(envelope)?;
    let key = {
        let mut cache = ASSIST_JWKS.lock().unwrap_or_else(|e| e.into_inner());
        match cache.key(&config.issuer, &kid, now) {
            Ok(k) => k,
            Err(e) => {
                let (code, status) = match e.code() {
                    Some("assertion_invalid") => ("assertion_invalid", 401),
                    _ => ("capability_unavailable", 503),
                };
                return Err(assist_fail(status, code, &e.to_string()));
            }
        }
    };
    assistant_actor::verify(envelope, &config, &key, now)
        .map_err(|r| assist_fail(status_of(r.code), r.code, &r.message))
}

/// Verify the bearer into a [`AssistActor`] or answer the refusal.
fn assist_caller(
    request: &Request,
    public: &crate::ui::PublicBoard,
) -> Result<AssistActor, HttpResp> {
    let bearer = header_value(request, "Authorization");
    let envelope = match assistant_actor::envelope_from_bearer(bearer.as_deref()) {
        Ok(e) => e,
        Err(r) => return Err(assist_fail(status_of(r.code), r.code, &r.message)),
    };
    verify_envelope(&envelope, public)
}

/// `POST /api/assist/<tail>` — diverted on the public host ahead of the
/// cookie-session gates, exactly as `/api/cli/`. One path segment names the
/// verb; a nested or unknown one is refused before body or credential.
pub(super) fn post(
    request: &mut Request,
    method: &Method,
    tail: &str,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    if *method != Method::Post {
        return assist_fail(405, "invalid_request", "assist calls are POST");
    }
    if tail.is_empty() || tail.contains('/') {
        return assist_fail(404, "not_found", "no such assist route");
    }
    let Some(verb) = AssistVerb::of(tail) else {
        return assist_fail(403, "assist_verb_refused", "the verb is not allowlisted");
    };
    let Some(public) = opts.public.as_ref() else {
        return assist_fail(
            503,
            "capability_unavailable",
            "assistant authority is not configured",
        );
    };
    let caller = match assist_caller(request, public) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if !assistant_actor::scope_covers(&caller, verb.scope()) {
        return assist_fail(
            403,
            "insufficient_scope",
            "the envelope's granted scopes do not cover this verb",
        );
    }
    let args = match read_args(request) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    // Recover the raw signed envelope to forward — the daemon re-verifies
    // it, so member proof travels signed, never as a bare relayed field.
    let bearer = header_value(request, "Authorization");
    let envelope = match assistant_actor::envelope_from_bearer(bearer.as_deref()) {
        Ok(e) => e,
        Err(r) => return assist_fail(status_of(r.code), r.code, &r.message),
    };
    run_member_verb(verb, &args, &caller, &envelope, state_dir, pm_dir, opts)
}

/// Consume the envelope's single-use jti and run the verified member
/// verb. This is the ONE defined consumption point (kickoff §3) — the
/// `/api/assist` bearer path and the CAD-1485 member-chat path both go
/// through here, so either way the jti is spent exactly once and the raw
/// signed envelope is forwarded to the daemon for cryptographic
/// re-verification (the daemon does NOT re-consume the jti). Callers
/// must have already run [`verify_envelope`] and, on the bearer path,
/// [`assistant_actor::scope_covers`].
pub(super) fn run_member_verb(
    verb: AssistVerb,
    args: &Value,
    caller: &AssistActor,
    envelope: &str,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    let spent = {
        let _guard = ASSIST_SPENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut set = assistant_actor::Spent::load(state_dir);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        set.consume(&caller.jti, caller.exp, now)
    };
    if !spent {
        return assist_fail(
            401,
            "assertion_replayed",
            "the envelope's jti was already consumed",
        );
    }
    dispatch(verb, args, caller, envelope, state_dir, pm_dir, opts)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, HttpResp> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        _ => Err(assist_fail(
            400,
            "invalid_request",
            &format!("assist argument '{key}' must be a string"),
        )),
    }
}

/// Do not forward a malformed or misbound private delivery even from an
/// acknowledged daemon. The signed envelope, not response data, names its owner.
fn valid_private_deliveries(value: &Value, actor: &AssistActor) -> bool {
    fn digest(value: &Value) -> bool {
        value.as_str().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
    }

    let Some(rows) = value.as_array() else {
        return false;
    };
    rows.iter().all(|row| {
        let (Some(id), Some(origin), Some(state), Some(results), Some(publications)) = (
            row.get("id").and_then(Value::as_str),
            row.get("origin"),
            row.get("state").and_then(Value::as_str),
            row.get("results").and_then(Value::as_array),
            row.get("publications").and_then(Value::as_array),
        ) else {
            return false;
        };
        let (Some(job), Some(task), Some(revision), Some(dispatch), Some(issue), Some(project)) = (
            origin
                .get("job")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            origin
                .get("task")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            origin
                .get("revision")
                .and_then(Value::as_i64)
                .filter(|n| *n > 0),
            origin
                .get("dispatch")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            origin
                .get("issue")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            origin
                .get("project")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        ) else {
            return false;
        };
        let id_revision = id
            .strip_prefix("job:")
            .and_then(|s| s.strip_prefix(job))
            .and_then(|s| s.strip_prefix(":task:"))
            .and_then(|s| s.strip_prefix(task))
            .and_then(|s| s.strip_prefix(":rev:"))
            .and_then(|s| s.strip_suffix(dispatch))
            .and_then(|s| s.strip_suffix(":dispatch:"))
            .filter(|s| !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<i64>().ok());
        if id_revision != Some(revision)
            || origin.get("owner_org").and_then(Value::as_str) != Some(actor.org.as_str())
            || origin.get("owner_sub").and_then(Value::as_str) != Some(actor.sub.as_str())
            || row.get("opened_at").and_then(Value::as_i64).is_none()
            || !matches!(state, "running" | "completed" | "failed" | "unknown")
            || (state == "completed") != !results.is_empty()
        {
            return false;
        }
        if !results.iter().all(|result| {
            let (Some(message), Some(body), Some(recorded)) = (
                result
                    .get("message")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty()),
                result
                    .get("body")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty()),
                result.get("digest").and_then(Value::as_str),
            ) else {
                return false;
            };
            let sha = match result.get("sha") {
                Some(Value::Null) => None,
                Some(Value::String(sha)) => Some(sha.as_str()),
                _ => return false,
            };
            message == dispatch
                && result.get("revision").and_then(Value::as_i64) == Some(revision)
                && result.get("at").and_then(Value::as_i64).is_some()
                && digest(&result["digest"])
                && crate::delivery::private::result_digest(message, sha, body) == recorded
        }) {
            return false;
        }
        publications.iter().all(|receipt| {
            let Some(selected) = receipt.get("results").and_then(Value::as_array) else {
                return false;
            };
            let outcome = &receipt["outcome"];
            let settled = match outcome.get("state").and_then(Value::as_str) {
                Some("pending") => true,
                Some("published") => outcome
                    .get("comment")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()),
                _ => false,
            };
            receipt.get("delivery_id").and_then(Value::as_str) == Some(id)
                && receipt.get("issue").and_then(Value::as_str) == Some(issue)
                && receipt.get("project").and_then(Value::as_str) == Some(project)
                && receipt.get("actor").and_then(Value::as_str) == Some("operator")
                && receipt.get("at").and_then(Value::as_i64).is_some()
                && receipt
                    .get("attempt")
                    .and_then(Value::as_str)
                    .is_some_and(|s| {
                        s.len() == 32
                            && s.bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    })
                && digest(&receipt["digest"])
                && digest(&receipt["body_digest"])
                && !selected.is_empty()
                && selected.iter().all(|item| {
                    item.as_str().is_some_and(|message| {
                        results
                            .iter()
                            .any(|result| result["message"].as_str() == Some(message))
                    })
                })
                && settled
        })
    })
}

/// CAD-2215: a member job-event page is relayed only when its opaque shape
/// is exact. Every visible row carries a stable keyed `id` and its own
/// forward opaque `cursor`, never a global `seq`; the page `cursor` is the
/// last visible row's cursor, the exact token this request supplied on an
/// empty page, or null on an initial empty page. A numeric/legacy page, a
/// raw numeric feed or any malformed projection is unavailable — never
/// relayed and never converted into an empty success.
fn valid_member_job_events_page(page: &Value, after: Option<&str>) -> bool {
    let Some(body) = page.as_object() else {
        return false;
    };
    let Some(events) = body.get("events").and_then(Value::as_array) else {
        return false;
    };
    if body.get("has_older").is_some_and(|flag| !flag.is_boolean()) {
        return false;
    }
    let mut last: Option<&str> = None;
    for event in events {
        let Some(row) = event.as_object() else {
            return false;
        };
        if row.contains_key("seq") || row.contains_key("event_seq") {
            return false;
        }
        if row
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return false;
        }
        let Some(cursor) = row
            .get("cursor")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty())
        else {
            return false;
        };
        last = Some(cursor);
    }
    match body.get("cursor") {
        None | Some(Value::Null) => last.is_none() && after.is_none(),
        Some(Value::String(cursor)) => {
            !cursor.is_empty() && last.or(after) == Some(cursor.as_str())
        }
        Some(_) => false,
    }
}

/// CAD-2215: the relay gate for one `member_job_events` answer. It removes
/// the protocol-only acknowledgement field and proves the page may be
/// relayed: the exact scoped
/// [`cadence_core::board_api::JOB_EVENTS_SCOPE_ACK`] plus the opaque page
/// shape. `false` means unavailable — never a page, never an empty success.
/// Exposed to this crate's UI acceptance tests so they exercise the real
/// predicate instead of a copy.
pub(super) fn member_job_events_relay_gate(page: &mut Value, after: Option<&str>) -> bool {
    let ack = page
        .as_object_mut()
        .and_then(|body| body.remove(cadence_core::board_api::READER_SCOPE_FIELD));
    ack.as_ref().and_then(Value::as_str) == Some(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK)
        && valid_member_job_events_page(page, after)
}

/// Refuse legacy or malformed member-history pages before any data is relayed.
pub(super) fn member_assist_history_relay_gate(
    page: &mut Value,
    after: Option<&str>,
    before: Option<&str>,
    backwards: bool,
) -> bool {
    let Some(body) = page.as_object_mut() else {
        return false;
    };
    let ack = body.remove(cadence_core::board_api::READER_SCOPE_FIELD);
    if ack.as_ref().and_then(Value::as_str)
        != Some(cadence_core::board_api::MEMBER_ASSIST_HISTORY_SCOPE_ACK)
    {
        return false;
    }
    let Some(conversation) = body.get("conversation").and_then(Value::as_object) else {
        return false;
    };
    if conversation.len() != 2
        || !["id", "alias"].iter().all(|field| {
            conversation
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
    {
        return false;
    }
    let Some(rows) = body.get("entries").and_then(Value::as_array) else {
        return false;
    };
    if rows.iter().any(|row| {
        let Some(row) = row.as_object() else {
            return true;
        };
        row.contains_key("seq")
            || !row
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("csrid_") && id.len() > "csrid_".len())
    }) {
        return false;
    }
    let position = |field: &str| match body.get(field) {
        Some(Value::Null) => Some(None),
        Some(Value::String(token)) if token.starts_with("csc1_") && token.len() > "csc1_".len() => {
            Some(Some(token.as_str()))
        }
        _ => None,
    };
    let (Some(forward), Some(backward)) = (position("cursor"), position("before_cursor")) else {
        return false;
    };
    if rows.is_empty() {
        if forward != after || backward != before {
            return false;
        }
    } else if forward.is_none() || backward.is_none() {
        return false;
    }
    if backwards {
        body.get("more_before").is_some_and(Value::is_boolean)
            && body.get("more_after").is_some_and(Value::is_null)
    } else {
        body.get("more_after").is_some_and(Value::is_boolean)
            && body.get("more_before").is_some_and(Value::is_null)
    }
}

/// Advisory compatibility only; every scoped page still needs its own ack.
fn daemon_supports(state_dir: &Path, capability: &str) -> Option<bool> {
    // A bounded best-effort read: classification must never turn a member
    // refusal into a hung request. Failure means "cannot tell" -> unavailable.
    let health = client::rpc_timeout(
        state_dir,
        "health",
        json!({}),
        std::time::Duration::from_secs(5),
    )
    .ok()?;
    Some(
        health["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|cap| cap.as_str() == Some(capability))),
    )
}

/// Relay the member-bound call to the daemon on a dedicated `member_*`
/// method. The member proof rides as the SIGNED envelope itself
/// (`member_envelope`) — the daemon re-verifies the Ed25519 signature and
/// every claim pin against its own `board-identity.json` + JWKS, so the
/// member identity is daemon-verifiable and a relayed `member_as`-style
/// field is never the proof. The relay channel is a daemon-proven
/// operator/board connection, but the daemon never trusts the connection
/// to name the member — only the signature does.
fn member_rpc(
    state_dir: &Path,
    method: &str,
    actor: &AssistActor,
    envelope: &str,
    args: &Value,
) -> HttpResp {
    let mut params = args.clone();
    params["member_envelope"] = json!(envelope);
    match client::rpc(state_dir, method, params) {
        Ok(mut v) => {
            if method == "member_job_list" {
                let Some(body) = v.as_object_mut() else {
                    return assist_fail(503, "unavailable", "member job list unavailable");
                };
                let ack = body.remove(cadence_core::board_api::READER_SCOPE_FIELD);
                // The acknowledgement is the only proof that the daemon that
                // answered applied the member/project/share boundary to THIS
                // answer. Without it the whole payload is unproven — its
                // `jobs`/`requests` may be an older daemon's unscoped list —
                // so nothing from it is relayed, not even as an empty list.
                if ack.as_ref().and_then(Value::as_str)
                    != Some(cadence_core::board_api::MEMBER_JOB_SCOPE_ACK)
                {
                    return assist_fail(503, "unavailable", "member job list unavailable");
                }
                // An acknowledged daemon may still return a malformed or
                // misbound private projection; drop only that projection and
                // keep the ordinary, already-scoped jobs/requests.
                let state = match body.get("private_deliveries_state").and_then(Value::as_str) {
                    Some("available") => Some("available"),
                    Some("unavailable") => Some("unavailable"),
                    _ => None,
                };
                let proven = state.is_some()
                    && body
                        .get("private_deliveries")
                        .is_some_and(|rows| valid_private_deliveries(rows, actor));
                if !proven {
                    body.remove("private_deliveries");
                }
                body.insert(
                    "private_deliveries_state".into(),
                    json!(if proven {
                        state.unwrap_or("unavailable")
                    } else {
                        "unavailable"
                    }),
                );
            }
            if method == "member_job_events" {
                // A tail page ignores `after`, so only a forward page must
                // echo the exact supplied token.
                let tail = args.get("tail").and_then(Value::as_bool).unwrap_or(false);
                let after = if tail {
                    None
                } else {
                    args.get("after").and_then(Value::as_str)
                };
                if !member_job_events_relay_gate(&mut v, after) {
                    // The acknowledgement is the only proof that the daemon
                    // that answered applied the member/project boundary AND
                    // the opaque cursor protocol to THIS page. Without the
                    // exact scoped value nothing from it is relayed — an
                    // older daemon's raw numeric feed included — and a
                    // malformed projection is not converted into an empty
                    // success either.
                    return assist_fail(503, "unavailable", "member job events unavailable");
                }
            }
            if method == "member_assist_history" {
                let after = args.get("after").and_then(Value::as_str);
                let before = args.get("before").and_then(Value::as_str);
                let backwards =
                    before.is_some() || args.get("tail").and_then(Value::as_bool).unwrap_or(false);
                if !member_assist_history_relay_gate(&mut v, after, before, backwards) {
                    return assist_fail(503, "unavailable", "member assistant history unavailable");
                }
            }
            json_ok(v)
        }
        // A `member_*` daemon refusal is an authorization failure — the
        // member lacks the scope, the binding, or the job ACL — so the
        // HTTP surface answers 403 (forbidden), not the generic 400
        // `write_err` maps `Rejected` to. Member-boundary authorization
        // is never a malformed-request 400: malformed args are refused
        // earlier at `read_args`/the verb's own checks.
        Err(crate::error::Error::Rejected(m)) if method == "member_job_events" => {
            // CAD-2215 compatibility: an older daemon refuses the injected
            // `cursor_format` as an unknown field, and that refusal is a plain
            // `rejected` — indistinguishable by shape from a genuine
            // authorization refusal. The daemon's advisory health capability
            // is the source-bound way to tell them apart; a health failure is
            // unavailable, and neither outcome can relay a page (the opaque
            // per-page acknowledgement still gates every answer).
            if daemon_supports(
                state_dir,
                crate::adapter::registry::JOB_EVENTS_OPAQUE_CAPABILITY,
            ) == Some(true)
            {
                assist_fail(403, "forbidden", &m)
            } else {
                assist_fail(503, "unavailable", "member job events unavailable")
            }
        }
        Err(crate::error::Error::Rejected(m)) if method == "member_assist_history" => {
            if daemon_supports(
                state_dir,
                crate::adapter::registry::MEMBER_ASSIST_HISTORY_OPAQUE_CAPABILITY,
            ) == Some(true)
            {
                assist_fail(403, "forbidden", &m)
            } else {
                assist_fail(503, "unavailable", "member assistant history unavailable")
            }
        }
        Err(crate::error::Error::Rejected(m)) => assist_fail(403, "forbidden", &m),
        // CAD-2215: the daemon's opaque-cursor refusals carry a stable code
        // the scoped client must see. Missing key material is unavailable;
        // every malformed, stale or foreign token is the same generic
        // invalid-cursor refusal. Only `member_job_events` may take this
        // branch — every other verb keeps the mapping above.
        Err(e @ crate::error::Error::Structured(_)) if method == "member_job_events" => {
            match e.code() {
                Some(code)
                    if matches!(
                        code,
                        "job_events_cursor_format"
                            | "job_events_cursor_numeric"
                            | "job_events_cursor_invalid"
                    ) =>
                {
                    assist_fail(400, code, &e.to_string())
                }
                Some(code) if code == "scoped_cursor_unavailable" => {
                    assist_fail(503, code, &e.to_string())
                }
                _ => write_err(&e),
            }
        }
        Err(e @ crate::error::Error::Structured(_)) if method == "member_assist_history" => {
            match e.code() {
                Some(code)
                    if matches!(
                        code,
                        "member_assist_history_cursor_format"
                            | "member_assist_history_cursor_numeric"
                            | "member_assist_history_cursor_invalid"
                    ) =>
                {
                    assist_fail(400, code, &e.to_string())
                }
                Some("scoped_cursor_unavailable") => {
                    assist_fail(503, "scoped_cursor_unavailable", &e.to_string())
                }
                _ => write_err(&e),
            }
        }
        Err(e) => write_err(&e),
    }
}

/// CAD-1420: invoke the dedicated `platform_assistant_provision` daemon
/// RPC on the same proven relay before the member verb. Params are the
/// closed pair — the signed `member_envelope` (daemon re-verifies it) and
/// the server-derived `operation`; no caller field is forwarded. A refusal
/// maps to the same 403 contract as a member_* refusal; a `provisioned:`
/// false answer (existing active binding) is the idempotent no-op.
fn provision(state_dir: &Path, operation: &str, envelope: &str) -> Result<(), HttpResp> {
    let params = json!({"member_envelope": envelope, "operation": operation});
    match client::rpc(state_dir, "platform_assistant_provision", params) {
        Ok(_) => Ok(()),
        Err(crate::error::Error::Rejected(m)) => Err(assist_fail(403, "forbidden", &m)),
        Err(e) => Err(write_err(&e)),
    }
}

fn json_ok(v: Value) -> HttpResp {
    let mut resp = Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
        .with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

/// The verb → the member-bound daemon call. Every verb carries the
/// member's verified identity via `member_rpc`; none reach a global RPC
/// or a request-named authority.
fn dispatch(
    verb: AssistVerb,
    args: &Value,
    actor: &AssistActor,
    envelope: &str,
    state_dir: &Path,
    _pm_dir: &Path,
    _opts: &ServeOpts,
) -> HttpResp {
    match verb {
        // CAD-1420: the three assist.* member verbs first re-validate their
        // argument shape (unknown fields and a negative history cursor
        // refuse here, before any projection can create a binding from
        // malformed input), then invoke the dedicated platform-projection
        // RPC on the same proven relay, then run the ordinary member_* RPC.
        // The member verb's own binding check still runs — a member with no
        // binding can never reach member_caller's ordinary path; only the
        // verified-envelope + live-authority projection mints one.
        AssistVerb::AssistState => {
            // The daemon accepts NO fields for state — any argument key
            // refuses before the projection can create a binding.
            if let Some(field) = args.as_object().and_then(|o| o.keys().next()) {
                return assist_fail(
                    400,
                    "invalid_request",
                    &format!("assist_state takes no arguments; field '{field}' is not accepted"),
                );
            }
            if let Err(resp) = provision(state_dir, "assist_state", envelope) {
                return resp;
            }
            member_rpc(state_dir, "member_assist_state", actor, envelope, args)
        }
        AssistVerb::AssistHistory => {
            // The caller supplies only paging arguments. The relay selects
            // the opaque protocol and refuses numeric positions before any
            // provisioning or read can occur.
            if let Some(field) = args.as_object().and_then(|o| {
                o.keys()
                    .find(|k| !["after", "before", "tail", "limit"].contains(&k.as_str()))
            }) {
                return assist_fail(
                    400,
                    "invalid_request",
                    &format!(
                        "assist_history takes after, before, tail, limit only; \
                         field '{field}' is not accepted"
                    ),
                );
            }
            for field in ["after", "before"] {
                match args.get(field) {
                    None | Some(Value::String(_)) => {}
                    Some(Value::Number(_)) => {
                        return assist_fail(
                            400,
                            "member_assist_history_cursor_numeric",
                            "member assist history requires an opaque cursor",
                        );
                    }
                    Some(_) => {
                        return assist_fail(
                            400,
                            "member_assist_history_cursor_invalid",
                            "member assist history cursor is invalid",
                        );
                    }
                }
            }
            if args.get("tail").is_some_and(|value| !value.is_boolean())
                || args.get("limit").is_some_and(|value| !value.is_number())
            {
                return assist_fail(400, "invalid_request", "invalid member history page");
            }
            let tail = args.get("tail").and_then(Value::as_bool).unwrap_or(false);
            if (args.get("after").is_some() && args.get("before").is_some())
                || (tail && (args.get("after").is_some() || args.get("before").is_some()))
            {
                return assist_fail(
                    400,
                    "member_assist_history_cursor_invalid",
                    "member assist history takes one paging direction",
                );
            }
            if let Err(resp) = provision(state_dir, "assist_history", envelope) {
                return resp;
            }
            let mut args = args.clone();
            args["cursor_format"] =
                json!(cadence_core::board_api::MEMBER_ASSIST_HISTORY_CURSOR_FORMAT);
            member_rpc(state_dir, "member_assist_history", actor, envelope, &args)
        }
        AssistVerb::AssistSend => {
            // The daemon accepts `text` and the optional idempotent `message`
            // id — any other argument key refuses before the projection can
            // create a binding. `message` is validated here (identifier
            // grammar, never a `sys-` daemon id). The shared verb path
            // (`run_member_verb`) spends the envelope's jti BEFORE calling
            // `dispatch`, so a malformed send is refused before any binding
            // write or effect but costs that jti — the same as every other
            // field check on this path.
            if let Some(field) = args.as_object().and_then(|o| {
                o.keys()
                    .find(|k| !["text", "message"].contains(&k.as_str()))
            }) {
                return assist_fail(
                    400,
                    "invalid_request",
                    &format!("assist_send takes text and optional message; field '{field}' is not accepted"),
                );
            }
            match arg_str(args, "text") {
                Ok(Some(text)) if !text.trim().is_empty() => {
                    match arg_str(args, "message") {
                        Err(resp) => return resp,
                        Ok(Some(mid)) => {
                            if crate::proto::identifier(mid, "Message ID").is_err()
                                || mid.starts_with(crate::proto::DAEMON_MESSAGE_PREFIX)
                            {
                                return assist_fail(
                                    400,
                                    "invalid_request",
                                    "assist argument 'message' is not a usable message id",
                                );
                            }
                        }
                        Ok(None) => {}
                    }
                    if let Err(resp) = provision(state_dir, "assist_send", envelope) {
                        return resp;
                    }
                    member_rpc(state_dir, "member_assist_send", actor, envelope, args)
                }
                _ => assist_fail(400, "invalid_request", "assist_send needs arguments.text"),
            }
        }
        // CAD-2215: a job request must name the tracker issue it is bound
        // to. The shape check runs here so a malformed call never reaches
        // the daemon; the daemon re-checks the issue against the verified
        // member's readable projects before any durable write.
        AssistVerb::JobRequest => match (
            arg_str(args, "title"),
            arg_str(args, "request_key"),
            arg_str(args, "issue"),
        ) {
            (Ok(Some(_)), Ok(Some(_)), Ok(Some(issue))) if !issue.trim().is_empty() => {
                member_rpc(state_dir, "member_job_request", actor, envelope, args)
            }
            (Ok(Some(_)), Ok(Some(_)), Ok(Some(_))) => assist_fail(
                400,
                "invalid_request",
                "job_request needs a nonempty arguments.issue",
            ),
            _ => assist_fail(
                400,
                "invalid_request",
                "job_request needs arguments.title, arguments.request_key and arguments.issue",
            ),
        },
        AssistVerb::JobList => member_rpc(state_dir, "member_job_list", actor, envelope, args),
        AssistVerb::JobEvidence => match arg_str(args, "job") {
            Ok(Some(_)) => member_rpc(state_dir, "member_job_evidence", actor, envelope, args),
            _ => assist_fail(400, "invalid_request", "job_evidence needs arguments.job"),
        },
        AssistVerb::JobEvents => match arg_str(args, "job") {
            Ok(Some(_)) => {
                // CAD-2215: a present `after` is an opaque string, never a
                // number, and the fixed cursor format is injected here so a
                // member can never ask for a raw numeric feed.
                match args.get("after") {
                    None | Some(Value::String(_)) => {}
                    Some(Value::Number(_)) => {
                        return assist_fail(
                            400,
                            "job_events_cursor_numeric",
                            "job_events refused: a scoped reader must send an opaque cursor, \
                             never a numeric position",
                        )
                    }
                    Some(_) => {
                        return assist_fail(
                            400,
                            "job_events_cursor_invalid",
                            "job_events refused: after must be an opaque cursor string",
                        )
                    }
                }
                match args.get("tail") {
                    None | Some(Value::Bool(_)) => {}
                    Some(_) => {
                        return assist_fail(
                            400,
                            "invalid_request",
                            "job_events argument 'tail' must be a boolean",
                        )
                    }
                }
                if args.get("cursor_format").is_some_and(|format| {
                    format.as_str() != Some(cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT)
                }) {
                    return assist_fail(
                        400,
                        "job_events_cursor_format",
                        "job_events refused: cursor_format must be \
                         \"opaque-v1\"",
                    );
                }
                let mut args = args.clone();
                args["cursor_format"] = json!(cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT);
                member_rpc(state_dir, "member_job_events", actor, envelope, &args)
            }
            _ => assist_fail(400, "invalid_request", "job_events needs arguments.job"),
        },
        AssistVerb::JobSubscribe => match arg_str(args, "job") {
            Ok(Some(_)) => member_rpc(state_dir, "member_job_subscribe", actor, envelope, args),
            _ => assist_fail(400, "invalid_request", "job_subscribe needs arguments.job"),
        },
        AssistVerb::JobShare => match arg_str(args, "job") {
            Ok(Some(_)) => member_rpc(state_dir, "member_job_share", actor, envelope, args),
            _ => assist_fail(400, "invalid_request", "job_share needs arguments.job"),
        },
        AssistVerb::JobReconcile => {
            match (
                arg_str(args, "job"),
                arg_str(args, "expected_authority_rev"),
            ) {
                (Ok(Some(_)), Ok(Some(_))) => {
                    member_rpc(state_dir, "member_job_reconcile", actor, envelope, args)
                }
                _ => assist_fail(
                    400,
                    "invalid_request",
                    "job_reconcile needs arguments.job and arguments.expected_authority_rev",
                ),
            }
        }
    }
}
