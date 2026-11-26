//! Hosted issue-list filters and the older board's limit compatibility.

use super::{eprint_line, unsupported, Error, Result};
use cadence_agent::issue::cli::IssueAction;
use cadence_agent::remote_cli::{RemoteAnswer, ISSUE_LS_LIMIT_UNSUPPORTED};
use serde_json::{json, Map, Value};

pub(super) const VERB: &str = "issue_ls";
pub(in crate::cli) const UNSUPPORTED_FILTER_REASON: &str =
    "the remote route's filter does not implement it";

/// The presence checks and names used by both hosted routing and offline discovery.
#[derive(Default)]
struct UnsupportedFilters {
    stage: bool,
    health: bool,
    since: bool,
    until: bool,
    ready: bool,
    blocked: bool,
    at: bool,
    sort: bool,
    fields: bool,
    summary: bool,
}

impl UnsupportedFilters {
    fn restrictions(self) -> [(&'static str, bool); 10] {
        [
            ("--stage", self.stage),
            ("--health", self.health),
            ("--since", self.since),
            ("--until", self.until),
            ("--ready", self.ready),
            ("--blocked", self.blocked),
            ("--at", self.at),
            ("--sort", self.sort),
            ("--fields", self.fields),
            ("--summary", self.summary),
        ]
    }
}

pub(in crate::cli) fn unsupported_flags() -> [&'static str; 10] {
    UnsupportedFilters::default()
        .restrictions()
        .map(|(flag, _)| flag)
}

pub(super) fn args(action: &IssueAction) -> Result<Option<Map<String, Value>>> {
    encode(action, true)
}

fn encode(action: &IssueAction, include_limit: bool) -> Result<Option<Map<String, Value>>> {
    let IssueAction::Ls {
        project,
        status,
        tags,
        epic,
        owner,
        component,
        priority,
        types,
        milestone,
        stage,
        health,
        plan,
        since,
        until,
        open,
        ready,
        blocked,
        at,
        sort,
        limit,
        fields,
        summary,
        json: _,
    } = action
    else {
        return Ok(None);
    };
    let unsupported_filters = UnsupportedFilters {
        stage: !stage.is_empty(),
        health: !health.is_empty(),
        since: since.is_some(),
        until: until.is_some(),
        ready: *ready,
        blocked: *blocked,
        at: at.is_some(),
        sort: sort.is_some(),
        fields: !fields.is_empty(),
        summary: *summary,
    };
    for (flag, present) in unsupported_filters.restrictions() {
        if present {
            return Err(unsupported("issue ls", flag, UNSUPPORTED_FILTER_REASON));
        }
    }
    let mut args = Map::new();
    if let Some(n) = limit {
        if !(1..=5000).contains(n) {
            return Err(Error::rejected(
                "remote `issue ls` --limit must be 1 to 5000 — nothing was sent",
            ));
        }
        if include_limit {
            args.insert("limit".into(), json!(n));
        }
    }
    // The hosted route accepts one project. Never silently keep just one.
    if project.len() > 1 {
        return Err(Error::rejected(
            "remote `issue ls` filters one --project at a time — the \
             hosted route takes a single key; nothing was sent",
        ));
    }
    if let Some(key) = project.first() {
        args.insert("project".into(), json!(key));
    }
    for (key, values) in [
        ("status", status),
        ("tag", tags),
        ("epic", epic),
        ("owner", owner),
        ("component", component),
        ("priority", priority),
        ("type", types),
        ("milestone", milestone),
        ("plan", plan),
    ] {
        if !values.is_empty() {
            args.insert(key.into(), json!(values));
        }
    }
    if *open {
        args.insert("open".into(), json!(true));
    }
    Ok(Some(args))
}

/// CAD-2277: only the typed old-board refusal earns a second request.
/// Rebuild the filters on that path, not on every successful limited read.
pub(super) fn call(
    action: &IssueAction,
    args: Map<String, Value>,
    org: &str,
    mut send: impl FnMut(Map<String, Value>) -> Result<RemoteAnswer>,
) -> Result<RemoteAnswer> {
    let limit = args.get("limit").and_then(Value::as_u64);
    match (limit, send(args)) {
        (Some(n), Err(error)) if error.code() == Some(ISSUE_LS_LIMIT_UNSUPPORTED) => {
            let Some(without_limit) = encode(action, false)? else {
                return Err(error);
            };
            let mut answer = send(without_limit)?;
            if let RemoteAnswer::Ok(body) = &mut answer {
                if let Some(issues) = body.get_mut("issues").and_then(Value::as_array_mut) {
                    issues.truncate(n as usize);
                }
                eprint_line(&format!(
                    "remote board on `{org}` predates server-side `issue_ls` limit; \
                     truncated to {n} locally."
                ));
            }
            Ok(answer)
        }
        (_, answer) => answer,
    }
}
