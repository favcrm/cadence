//! Board endpoints for installed apps (CAD-557) — the Apps page's
//! reads and the operator's one write:
//!
//! - `GET /api/apps[?project=<key>]` — every installed app, one row
//!   per `<project>/<app>` ([`app::ls`], the same rows `cadence app
//!   ls` prints): title, version, the declared connection slots with
//!   their effective bindings (an explicit unbind reads `bound:
//!   null`), the bundle's workflow names, the recorded source (a git
//!   install pins its commit SHA), the digest and the three-state
//!   `approval` — `approved` / `changed` / `unapproved` / `unknown`.
//! - `GET /api/apps/<project>/<name>` — one app's detail
//!   ([`app::show`]): the agent guide, each workflow's checked
//!   summary, the rubrics' bodies, the install record, plus this
//!   app's row of [`app::doctor`]'s slot findings.
//! - `GET /api/apps/<project>/<name>/runs` — the plans/epics proposed
//!   from this app's workflows (CAD-563), by the recorded
//!   `plan.workflow` provenance: the epic, its derived status and the
//!   plan's state, tickets and size-weighted progress ([`plan::plan_json`]),
//!   so the app page lists its runs without leaving the app.
//! - `GET /api/apps/<project>/<name>/outputs` — the `local` outbox
//!   items this app's runs produced (CAD-563), operator-only like
//!   `/api/outbox` (the same proof, the same relay): an item is
//!   attributed by its effect's recorded `task` — a ticket of one of
//!   the app's runs — or, for a send staged without a task, by the
//!   effect's agent owning one of those tickets.
//! - `POST /api/apps/<project>/<name>/approve` — relays the daemon's
//!   `app_approve` verbatim — the same call `cadence app approve`
//!   makes; the board keeps no second approval path.
//!
//! Approve is `OperatorOnly` in `operator::WRITE_ROUTES` (an unlisted
//! shape would fail the same way), because the board relays over its
//! own daemon connection: the daemon attributes the approval to
//! whoever that connection proves — the proven operator or nobody —
//! so an agent's or a member's HTTP request must never reach it.
//! `admit` runs the operator proof on the HTTP peer before the
//! handler runs; the daemon's own `operator_connection` gate still
//! refuses what the board must not do.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use tiny_http::Request;

use super::{err_response, home, json_response, read_body, HttpResp, ServeOpts};
use crate::client;
use crate::issue::{app, board, model, plan, Pm};

/// The approve body is `{}` — anything else is refused by shape.
const BODY_CAP: u64 = 4 * 1024;

/// `true` when the approve body is exactly an empty JSON object. An
/// approve request carries nothing — like the daemon, attribution is
/// the board's proven connection, so a body field is a forgery attempt.
/// A non-object body (`[]`, `null`, `1`) fails too: serde would read a
/// fieldless struct from `[]`, which is not the shape the board admits.
fn approve_body_ok(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .is_some_and(|v| v.as_object().is_some_and(|m| m.is_empty()))
}

/// What `/api/apps…` names — the list, one app's detail, its runs or
/// its outputs.
#[derive(Debug)]
pub(super) enum ReadRoute<'a> {
    List,
    Detail(&'a str, &'a str),
    Runs(&'a str, &'a str),
    Outputs(&'a str, &'a str),
}

/// The read route for a path under `/api/apps` — exactly `/api/apps`
/// (or `/api/apps/`), `/api/apps/<project>/<name>` (app names are
/// tag-shaped — a single segment), or one app's `/runs` or `/outputs`.
pub(super) fn read_route(path: &str) -> Option<ReadRoute<'_>> {
    let rest = path.strip_prefix("/api/apps")?;
    match rest {
        "" | "/" => Some(ReadRoute::List),
        tail => {
            let tail = tail.strip_prefix('/')?;
            let (project, rest) = tail.split_once('/')?;
            if project.is_empty() {
                return None;
            }
            let Some((name, sub)) = rest.split_once('/') else {
                return (!rest.is_empty() && !rest.contains('/'))
                    .then_some(ReadRoute::Detail(project, rest));
            };
            if name.is_empty() {
                return None;
            }
            match sub {
                "runs" => Some(ReadRoute::Runs(project, name)),
                "outputs" => Some(ReadRoute::Outputs(project, name)),
                _ => None,
            }
        }
    }
}

/// `(project, name)` for `POST /api/apps/<project>/<name>/approve` —
/// `None` when the path is not that route.
pub(super) fn approve_route(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/apps/")?;
    let (project, rest) = tail.split_once('/')?;
    let (name, verb) = rest.split_once('/')?;
    (verb == "approve" && !project.is_empty() && !name.is_empty()).then_some((project, name))
}

/// `GET` dispatch for the app reads.
pub(super) fn read(
    request: &Request,
    pm: &Pm,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    query: &dyn Fn(&str) -> Option<String>,
    route: ReadRoute<'_>,
) -> HttpResp {
    match route {
        ReadRoute::List => list(pm, state_dir, query),
        ReadRoute::Detail(project, name) => detail(pm, state_dir, project, name),
        ReadRoute::Runs(project, name) => runs(pm, state_dir, project, name),
        ReadRoute::Outputs(project, name) => outputs(request, pm, state_dir, opts, project, name),
    }
}

/// `GET /api/apps[?project=<key>]` — the `app ls` payload.
fn list(pm: &Pm, state_dir: &std::path::Path, query: &dyn Fn(&str) -> Option<String>) -> HttpResp {
    let project = query("project");
    if let Some(p) = &project {
        if !model::valid_key(p) {
            return err_response(400, "bad project key");
        }
    }
    match app::ls(pm, project.as_deref(), state_dir) {
        Ok(listed) => json_response(listed),
        Err(e) if e.to_string().starts_with("No project") => err_response(404, &e.to_string()),
        Err(e) => err_response(503, &e.to_string()),
    }
}

/// `GET /api/apps/<project>/<name>` — `app show` plus the doctor row.
/// An app that cannot be described (not installed, a broken record or
/// manifest, a refused walk) is a 404 — there is no detail to show.
fn detail(pm: &Pm, state_dir: &std::path::Path, key: &str, name: &str) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    match app::show(pm, key, name, state_dir) {
        Ok(mut out) => {
            if let Some(why) = out["error"].as_str() {
                let why = why.to_string();
                return err_response(404, &why);
            }
            out["doctor"] = doctor_row(&pm.dir, state_dir, key, name);
            json_response(out)
        }
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.starts_with("No project") {
                404
            } else {
                503
            };
            err_response(code, &msg)
        }
    }
}

/// `GET /api/apps/<project>/<name>/runs` — every plan/epic proposed
/// from this app's workflows, by the recorded `plan.workflow`
/// provenance (`<app>/<wf>`, CAD-547). Each row carries the epic's
/// derived status and the plan block `plan show` renders: state,
/// tickets and size-weighted progress. An app that is not installed
/// (or cannot be described) is a 404, like its detail.
fn runs(pm: &Pm, state_dir: &std::path::Path, key: &str, name: &str) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    if let Err(e) = app::digest(&pm.dir, key, name) {
        return err_response(404, &e.to_string());
    }
    json_response(json!({
        "project": key,
        "name": name,
        "runs": app_runs(pm, state_dir, key, name),
    }))
}

/// The app's runs from the board's read model — the same indexed
/// views `/api/issues` derives from, so a tracker write shows on the
/// next read.
fn app_runs(pm: &Pm, state_dir: &std::path::Path, key: &str, name: &str) -> Vec<Value> {
    let read = super::read_model::get(state_dir, &pm.dir).board(pm, Some(key));
    let by_id = read.by_id();
    read.views
        .iter()
        .filter(|v| run_app(v).is_some_and(|(a, _)| a == name))
        .map(|v| {
            json!({
                "epic": v.issue.front.id,
                "title": v.issue.front.title,
                "status": v.status,
                "workflow": v.issue.front.plan.as_ref().and_then(|p| p.workflow.clone()),
                "plan": plan::plan_json(v, &by_id),
            })
        })
        .collect()
}

/// The app a run records in `plan.workflow` — `<app>/<wf>` only; a
/// stored workflow's bare name is not an app's (the same split
/// `app remove` refuses on).
fn run_app(v: &board::View) -> Option<(&str, &str)> {
    app::split_ref(v.issue.front.plan.as_ref()?.workflow.as_deref()?)
}

/// `GET /api/apps/<project>/<name>/outputs` — the `local` outbox items
/// this app's runs produced (CAD-563): the ledger relayed exactly as
/// `/api/outbox` relays it, narrowed to the items attributed to the
/// app. Operator-only — the same proof `/api/outbox` runs, because the
/// ledger's previews and paths are the operator's — so the gate comes
/// before any tracker or ledger read.
fn outputs(
    request: &Request,
    pm: &Pm,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    key: &str,
    name: &str,
) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    if let Err(resp) = home::outbox_gate(
        request,
        state_dir,
        opts,
        &format!("GET /api/apps/{key}/{name}/outputs"),
    ) {
        return resp;
    }
    if let Err(e) = app::digest(&pm.dir, key, name) {
        return err_response(404, &e.to_string());
    }
    // What an item is attributed by: the runs' tickets, and the owners
    // the workflow bound to them (`agent:` per ticket) — an effect
    // staged without a task still names the agent that worked it.
    let read = super::read_model::get(state_dir, &pm.dir).board(pm, Some(key));
    let by_id = read.by_id();
    let mut tickets: HashSet<String> = HashSet::new();
    let mut owners: HashSet<String> = HashSet::new();
    for v in read.views.iter() {
        if run_app(v).is_none_or(|(a, _)| a != name) {
            continue;
        }
        for ticket in v.issue.front.plan.iter().flat_map(|p| p.tickets.iter()) {
            tickets.insert(ticket.clone());
            if let Some(owner) = by_id.get(ticket).and_then(|t| t.issue.front.owner.clone()) {
                owners.insert(owner);
            }
        }
    }
    let provenance = effect_provenance(state_dir);
    let out = match client::rpc(state_dir, "platform_outbox", json!({})) {
        Ok(out) => out,
        Err(e) => return home::rpc_err(&e, "platform_outbox"),
    };
    let items: Vec<Value> = out["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            let Some((agent, task)) = item["effect_id"].as_str().and_then(|id| provenance.get(id))
            else {
                return false;
            };
            task.as_deref().is_some_and(|t| tickets.contains(t)) || owners.contains(agent)
        })
        .cloned()
        .collect();
    json_response(json!({"project": key, "name": name, "items": items}))
}

/// `effect_id` → `(agent, task)` from the durable effect ledger — the
/// provenance an outbox item is attributed by. A read-only open, like
/// the `plan_proposed` fallback `open_plan_epics` reads; the caller
/// has already proven the operator, so nothing here widens a gate.
/// Unreadable ledger: no item is attributed, never an error.
fn effect_provenance(state_dir: &std::path::Path) -> HashMap<String, (String, Option<String>)> {
    let Ok(conn) = crate::store::open_read_only(&state_dir.join("cadence.sqlite3")) else {
        return HashMap::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT effect_id, agent, task FROM platform_effects") else {
        return HashMap::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    });
    match rows {
        Ok(rows) => rows
            .flatten()
            .map(|(id, agent, task)| (id, (agent, task)))
            .collect(),
        Err(_) => HashMap::new(),
    }
}

/// This app's row of `cadence app doctor` — its slot findings —
/// `null` when the app is absent from the scan. The connections the
/// daemon registers (plus the built-in `local`) decide what a binding
/// resolves to; unreachable, the check reports itself unavailable,
/// never "unknown connection" — the same rule `doctor` applies.
fn doctor_row(
    pm_dir: &std::path::Path,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
) -> Value {
    let known = client::rpc(state_dir, "daemon_info", json!({}))
        .ok()
        .and_then(|v| v["connections"].as_array().cloned())
        .map(|a| {
            let mut set: std::collections::HashSet<String> = a
                .iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect();
            set.insert(app::LOCAL_CONNECTION.to_string());
            set
        });
    app::doctor(pm_dir, known.as_ref())["apps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["project"].as_str() == Some(key) && r["app"].as_str() == Some(name))
        .cloned()
        .unwrap_or(Value::Null)
}

/// `POST /api/apps/<project>/<name>/approve` — the same `app_approve`
/// call `cadence app approve` makes. The board was already admitted
/// as the operator (`operator::admit`); the daemon's
/// `operator_connection` attributes the approval to the board's own
/// connection and re-checks the installed bundle before recording.
pub(super) fn approve(
    request: &mut Request,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    if !approve_body_ok(&bytes) {
        return err_response(400, "app approve takes no fields — send {}");
    }
    match client::rpc(
        state_dir,
        "app_approve",
        json!({"project": key, "name": name}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => home::rpc_err(&e, "app_approve"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The read route's shape: exactly the list, `<project>/<name>`, or
    /// one app's `runs` / `outputs` — no partial segment, no deeper
    /// path, no sibling prefix, and no other verb.
    #[test]
    fn read_route_is_exact() {
        assert!(matches!(read_route("/api/apps"), Some(ReadRoute::List)));
        assert!(matches!(read_route("/api/apps/"), Some(ReadRoute::List)));
        match read_route("/api/apps/demo/studio") {
            Some(ReadRoute::Detail(p, n)) => assert_eq!((p, n), ("demo", "studio")),
            other => panic!("detail: {other:?}"),
        }
        match read_route("/api/apps/demo/studio/runs") {
            Some(ReadRoute::Runs(p, n)) => assert_eq!((p, n), ("demo", "studio")),
            other => panic!("runs: {other:?}"),
        }
        match read_route("/api/apps/demo/studio/outputs") {
            Some(ReadRoute::Outputs(p, n)) => assert_eq!((p, n), ("demo", "studio")),
            other => panic!("outputs: {other:?}"),
        }
        for dead in [
            "/api/apps/demo",
            "/api/apps/demo/",
            "/api/apps/demo/studio/x",
            "/api/apps/demo/studio/runs/x",
            "/api/apps/demo/studio/approve",
            "/api/apps/demo//runs",
            "/api/apps//studio/runs",
            "/api/apps//studio",
            "/api/apps/demo//",
            "/api/appsx",
            "/api/app",
        ] {
            assert!(read_route(dead).is_none(), "{dead}");
        }
    }

    /// The approve route's shape: `POST /api/apps/<project>/<name>/approve`
    /// — the verb is the last segment, never a wildcard position.
    #[test]
    fn approve_route_is_exact() {
        assert_eq!(
            approve_route("/api/apps/demo/studio/approve"),
            Some(("demo", "studio"))
        );
        for dead in [
            "/api/apps",
            "/api/apps/demo/studio",
            "/api/apps/demo/studio/reject",
            "/api/apps/demo/approve",
            "/api/apps//studio/approve",
            "/api/apps/demo//approve",
            "/api/apps/demo/studio/approve/extra",
        ] {
            assert_eq!(approve_route(dead), None, "{dead}");
        }
    }

    /// The body admits exactly `{}` — a field, an empty array or a
    /// scalar fails the shape before the daemon is asked.
    #[test]
    fn approve_body_is_empty_object_only() {
        for ok in ["{}", " { } ", "{\n}"] {
            assert!(approve_body_ok(ok.as_bytes()), "{ok:?}");
        }
        for forged in [
            r#"{"actor":"operator"}"#,
            r#"{"operator":true}"#,
            r#"{"project":"x"}"#,
            r#"{"name":"y"}"#,
            "[]",
            "null",
            "1",
            "",
            "{",
        ] {
            assert!(!approve_body_ok(forged.as_bytes()), "{forged}");
        }
    }
}
