//! Hosted local-assignment verbs behind `agent` (CAD-1460).
//!
//! These commands talk to
//! `/__platform/hosted-cadence/:organizationId/local-assignments`.
//! They do not forward `issue start` or `dispatch` into the container.
//! A claim is not execution until the configured local actor accepts the
//! issue note through the existing daemon send.

#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

use cadence_agent::local_assignment::delivery::deliver_kickoff;
#[cfg(test)]
use cadence_agent::local_assignment::delivery::{redact_secrets, write_note};
use cadence_agent::local_assignment::http::BoardHttp;
use cadence_agent::local_assignment::{Coordinator, Kickoff};
use cadence_agent::remote_cli::RemoteTarget;
use clap::Subcommand;
use serde_json::{json, Value};

use super::{print_json, Error, Result};

#[derive(Subcommand)]
pub(crate) enum LocalAgentAction {
    /// List one hosted directory page with server-authored presence and registration state.
    Directory {
        /// Continue from the previous page's after_id; null means exhausted.
        #[arg(long)]
        after_id: Option<String>,
    },
    /// Store an FYI receipt only: never start inference, send work, or copy referenced content.
    Notify {
        /// Stable lag_<UUID> from the selected hosted workspace's directory.
        agent_id: String,
        /// Literal FYI body (1..4000 UTF-16 code units).
        #[arg(long)]
        text: String,
        /// Explicit idempotency key (1..120 units). Retry identical content with this same key.
        #[arg(long)]
        operation_id: String,
        /// Issue id pointer only; repeat up to 16 total references.
        #[arg(long = "issue")]
        issues: Vec<String>,
        /// Wiki id/path pointer only; no file or page is read.
        #[arg(long = "wiki")]
        wiki: Vec<String>,
    },
    /// Save this machine's local actor and claim queued work for you.
    Connect {
        /// Local daemon alias that will read the issue.
        #[arg(long)]
        alias: Option<String>,
        /// Local git checkout. Without it the alias is only saved.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Lane slug. Defaults to `work`.
        #[arg(long)]
        name: Option<String>,
        /// Register this explicit agent for FYI only; preserve the assignment default.
        #[arg(long)]
        fyi_only: bool,
    },
    /// Observe the daemon's durable workspace inbox; never claim work.
    Watch,
    /// Stop local pickup without stopping actors or cancelling their claims.
    Disconnect,
    /// Show this machine's alias and the assignments the board returned.
    Status,
    /// Display durable FYI notices. --clear removes displayed informational notices only.
    Inbox {
        #[arg(long)]
        clear: bool,
    },
    /// Report a claim this machine already holds.
    Report {
        assignment: String,
        /// `running`, `blocked`, `failed`, or `result`.
        #[arg(long)]
        state: String,
        #[arg(long)]
        text: Option<String>,
        /// 40 or 64 lowercase hex commit characters. Required for a dev
        /// result and refused for non-code work, where a digest is not a
        /// commit.
        #[arg(long)]
        head: Option<String>,
        /// The actual output of non-code work: `artifact:<ref>` or
        /// `link:<https url>`. Only a `result` carries one. The board stores
        /// it as reported and says separately whether it resolved it.
        #[arg(long, value_name = "TYPE:REF")]
        deliverable: Option<String>,
    },
    /// Move or cancel a live assignment.
    Reassign {
        assignment: String,
        /// Directory user id that should take the work.
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        cancel: bool,
    },
    /// Save one optional legacy tool/publisher credential; does not activate tooling.
    Tool {
        /// Legacy `provider.draft` or `publish.send`; normal account images need neither.
        #[arg(long)]
        lane: String,
    },
}

fn coordinator(target: &RemoteTarget) -> Result<Coordinator<BoardHttp>> {
    BoardHttp::open(target)?.into_coordinator()
}
pub(super) fn hosted_finish(
    target: &RemoteTarget,
    issue: Option<&str>,
    worktree: Option<&Path>,
) -> Error {
    match coordinator(target) {
        Ok(coord) => cadence_agent::local_assignment::finish_guidance::refusal(&coord, issue, worktree),
        Err(_) => Error::rejected(
            "Hosted issue finish does not release lanes; the lane state cannot be confirmed; leave it for the reconcile sweep",
        ),
    }
}

pub(super) fn run(
    target: &RemoteTarget,
    action: &LocalAgentAction,
    _wake_timeout: u64,
    local_state: Option<PathBuf>,
) -> Result<i32> {
    // These explicit hosted reads/writes need neither a coordinator ledger nor
    // a running daemon. BoardHttp retains saved-login renewal and identity pins.
    match action {
        LocalAgentAction::Directory { after_id } => {
            return finish(&cadence_agent::local_assignment::notifications::directory(
                target,
                after_id.as_deref(),
            )?);
        }
        LocalAgentAction::Notify {
            agent_id,
            operation_id,
            text,
            issues,
            wiki,
        } => {
            let value = cadence_agent::local_assignment::notifications::notify(
                target,
                agent_id,
                operation_id,
                text,
                issues,
                wiki,
            )?;
            print_json(&value);
            return Ok(match value["status"].as_str() {
                Some("stored") => 0,
                Some("refused") => 5,
                _ => 1,
            });
        }
        _ => {}
    }
    let mut coord = coordinator(target)?;
    let value = match action {
        LocalAgentAction::Directory { .. } | LocalAgentAction::Notify { .. } => {
            unreachable!("handled before opening the coordinator")
        }
        LocalAgentAction::Connect {
            repo,
            name,
            alias,
            fyi_only,
        } => {
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            ensure_report_driver(&state);
            let alias = alias
                .clone()
                .or(coord.configured_alias()?)
                .ok_or_else(|| Error::usage("connect requires --alias <explicit local agent>"))?;
            cadence_agent::client::rpc_timeout(
                &state,
                "hosted_local_agent_connect",
                json!({
                    "org": target.org, "organization_id": target.org_id, "endpoint": target.endpoint,
                    "alias": alias, "repo": repo, "name": name, "fyi_only": fyi_only,
                }),
                std::time::Duration::from_secs(30),
            )?
        }
        LocalAgentAction::Watch => {
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            ensure_report_driver(&state);
            let mut previous: Option<Value> = None;
            loop {
                let value = cadence_agent::client::rpc_timeout(
                    &state,
                    "hosted_local_agent_observe",
                    json!({
                        "organization_id": target.org_id,
                        "after": previous.as_ref().and_then(|value| value["cursor"].as_u64()),
                        "wait": previous.is_some(),
                    }),
                    std::time::Duration::from_secs(35),
                )?;
                if previous.as_ref() != Some(&value) {
                    if !cadence_agent::output::stream_json(&value) {
                        return Ok(0);
                    }
                    previous = Some(value);
                }
            }
        }
        LocalAgentAction::Disconnect => {
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            ensure_report_driver(&state);
            let value = cadence_agent::client::rpc(
                &state,
                "hosted_local_agent_disconnect",
                json!({"organization_id": target.org_id}),
            )?;
            coord.disconnect()?;
            value
        }
        LocalAgentAction::Status => {
            let mut value = coord.status()?;
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            value["inbox"] = assignment_inbox(&state, &target.org_id)?;
            value
        }
        LocalAgentAction::Inbox { clear } => {
            let state = local_state
                .clone()
                .map(Ok)
                .unwrap_or_else(cadence_agent::client::state_dir)?;
            // Observation/clear never authorizes starting actors or pickup.
            // A stopped daemon is reported by the observer RPC unchanged.
            cadence_agent::client::rpc(
                &state,
                "hosted_local_agent_observe",
                json!({"organization_id": target.org_id, "clear": clear}),
            )?
        }
        LocalAgentAction::Report {
            assignment,
            state,
            text,
            head,
            deliverable,
        } => {
            let deliverable = deliverable
                .as_deref()
                .map(cadence_agent::local_assignment::Deliverable::parse)
                .transpose()?;
            let value = coord.report(
                assignment,
                state,
                text.as_deref(),
                head.as_deref(),
                deliverable.as_ref(),
            )?;
            if *state == "result" {
                // The assignment ends here; the ticket does not move on its own.
                // Say the next step instead of leaving the session to infer it.
                cadence_agent::output::eprint_line(
                    "cadence: assignment complete — close the ticket with \
                     `cadence issue set <ID> status=done` once the work is accepted",
                );
            }
            value
        }
        LocalAgentAction::Reassign {
            assignment,
            to,
            cancel,
        } => coord.reassign(assignment, to.as_deref(), *cancel)?,
        LocalAgentAction::Tool { lane } => coord.authorize_tool(lane)?,
    };
    finish(&value)
}

pub(super) fn dispatch(
    target: &RemoteTarget,
    issue: &str,
    to: &str,
    kind: Option<&str>,
) -> Result<i32> {
    let kind = requested_kind(kind)?;
    finish(&coordinator(target)?.dispatch(issue, Some(to), kind)?)
}

/// `--kind` names a kind this client can run. `dev` is the default and is
/// never sent, so a dev request keeps the create body an older board accepts.
/// Every other token is refused here, before any request.
fn requested_kind(kind: Option<&str>) -> Result<Option<cadence_agent::local_assignment::WorkKind>> {
    use cadence_agent::local_assignment::WorkKind;
    match kind {
        None => Ok(None),
        Some(token) => match WorkKind::parse(token) {
            Some(WorkKind::Dev) => Ok(None),
            Some(kind) => Ok(Some(kind)),
            // Review is a named unsupported capability: the board must first
            // prove PR/head binding and a self-review refusal, and until it
            // does this client will not create or pick up review work.
            None if token == "review" => Err(Error::invalid(
                "unsupported_work_kind",
                "review work is not supported by this client: the board does not yet prove exact PR/head binding and a self-review refusal, so nothing was created, claimed or sent",
            )),
            None => Err(Error::usage(
                "--kind must be dev, research, design, business or manual; nothing was sent",
            )),
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start(
    target: &RemoteTarget,
    id: &str,
    repo: Option<&Path>,
    name: Option<&str>,
    profile: Option<&str>,
    base: Option<&str>,
    kind: Option<&str>,
    take_over: Option<&str>,
    wake_timeout: u64,
    local_state: Option<PathBuf>,
) -> Result<i32> {
    // No early --repo refusal: the board's work kind decides, and `--kind`
    // asks for a repo-less kind on creation. Dev work still needs a checkout
    // and is refused before any claim; repo-less work opens no lane at all.
    let kind = requested_kind(kind)?;
    let mut coord = coordinator(target)?;
    // CAD-2061: an explicit take-over replaces another session's live claim.
    // A reason is required, and the claim refuses rather than drop it when the
    // board cannot record it.
    if let Some(reason) = take_over {
        coord.request_take_over(reason)?;
    }
    if let Some(profile) = profile {
        // CAD-1964: the caller is the actor, so this claims under the
        // profile's session id and opens the lane without a local alias,
        // a daemon handoff or any other waiting assignment.
        return finish(&coord.start_session(id, repo, name, profile, base, kind)?);
    }
    if base.is_some() {
        return Err(Error::usage(
            "hosted issue start accepts --base only with --profile; the alias path uses the              project's own base. Nothing was sent",
        ));
    }
    let mut deliver =
        |kickoff: &Kickoff| deliver_kickoff(target, local_state.clone(), kickoff, wake_timeout);
    let value = coord.start_here(id, repo, name, kind, &mut deliver)?;
    if let Some(dir) = local_state.or_else(|| cadence_agent::client::state_dir().ok()) {
        ensure_report_driver(&dir);
    }
    finish(&value)
}

/// An absent local state cannot hide a successful remote assignment read.
fn assignment_inbox(state: &Path, org: &str) -> Result<Value> {
    match std::fs::symlink_metadata(state) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(json!({"available": false, "reason": "local_state_not_initialized"}))
        }
        Err(error) => Err(error.into()),
        Ok(_) => cadence_agent::local_assignment::events::observe(state, org, false),
    }
}

fn ensure_report_driver(state: &Path) {
    if cadence_agent::client::rpc_timeout(
        state,
        "health",
        json!({}),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
    {
        return;
    }
    let _ = cadence_agent::client::daemon_start(state);
}

fn finish(value: &Value) -> Result<i32> {
    print_json(value);
    if value
        .get("refused")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
    {
        return Ok(5);
    }
    Ok(0)
}

#[cfg(test)]
mod assignment_status_tests {
    use super::*;

    #[test]
    fn uninitialized_inbox_is_unavailable_without_blocking_assignment_status() {
        let root = tempfile::tempdir().expect("scratch");
        let state = root.path().join("state");
        let unavailable = assignment_inbox(&state, "ws_synthetic").expect("missing inbox");
        assert_eq!(unavailable["available"], false);
        assert!(!state.exists(), "a read must not initialize local state");

        fs::create_dir(&state).expect("initialize state");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).expect("private state");
        }
        let empty = assignment_inbox(&state, "ws_synthetic").expect("readable empty inbox");
        assert_eq!(empty["cursor"], 0);
        assert_eq!(empty["entries"], json!([]));

        #[cfg(unix)]
        {
            let link = root.path().join("dangling");
            std::os::unix::fs::symlink(root.path().join("missing"), &link).expect("link");
            assert!(assignment_inbox(&link, "ws_synthetic").is_err());
        }
    }
}

#[cfg(test)]
mod context_snapshots {
    use super::*;

    #[test]
    fn a_new_generation_cannot_overwrite_an_older_actor_context() {
        let dir = tempfile::tempdir().expect("state");
        let mut kickoff = Kickoff {
            alias: "worker",
            issue_id: "CAD1460",
            assignment_id: "hla_00000000000000000000000000000001",
            worktree: "/tmp/cad1460-work",
            message_id: "0123456789abcdef0123456789abcdef",
            claim_id: "hlc_00000000000000000000000000000001",
            generation: 1,
            organization_id: "ws_cad1460",
            audience: "https://board.test",
            subject_id: None,
            issuer: None,
            work_kind: None,
        };
        let first = write_note(dir.path(), &kickoff, &json!({"issue": "original work"}))
            .expect("first context");
        let original = fs::read(&first).expect("original snapshot");
        kickoff.message_id = "abcdef0123456789abcdef0123456789";
        kickoff.claim_id = "hlc_00000000000000000000000000000002";
        kickoff.generation = 2;
        let second = write_note(dir.path(), &kickoff, &json!({"issue": "reassigned work"}))
            .expect("replacement context");
        assert_ne!(first, second);
        assert_eq!(fs::read(first).expect("older actor context"), original);
    }

    #[test]
    fn redaction_preserves_org_selectors_but_hides_complete_credentials() {
        let credential = format!("hct_{}", "a".repeat(43));
        let redacted = redact_secrets(format!(
            "org=hct_company\ncredential={credential}\nwiki=global/hct_notes.md"
        ));
        assert!(redacted.contains("org=hct_company\n"));
        assert!(redacted.contains("wiki=global/hct_notes.md"));
        assert!(!redacted.contains(&credential));
    }
}

// Independently authored CAD-2009 consumer acceptance; do not weaken.
#[cfg(all(test, unix))]
mod cad2009_notify_acceptance {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::process::{Command, Stdio};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const AGENT: &str = "lag_00000000-0000-4000-8000-000000000003";
    const NOTICE: &str = "lan_00000000-0000-4000-8000-000000000004";
    const OP: &str = "independent-notify-operation";
    const CHILD: &str = "CAD2009_NOTIFY_ACCEPTANCE_CHILD";
    const TEST: &str = "cad2009_notify_consumer_preserves_uncertainty_and_identical_key_recovery";

    fn action() -> LocalAgentAction {
        LocalAgentAction::Notify {
            agent_id: AGENT.into(),
            operation_id: OP.into(),
            text: "Independent FYI only".into(),
            issues: vec!["SYN-1".into()],
            wiki: vec!["global/synthetic".into()],
        }
    }

    fn target(endpoint: String) -> RemoteTarget {
        RemoteTarget {
            org: "synthetic".into(),
            org_id: "ws_synthetic".into(),
            endpoint,
            identity: None,
        }
    }

    fn child(case: &str) {
        // Parent supplies a fresh private HOME/XDG_CONFIG_HOME in this child
        // only. No global environment changes can race another acceptance.
        let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
        let auth = config.join("cadence/remote-auth");
        fs::create_dir_all(&auth).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).unwrap();
        if case == "predispatch" {
            let outcome = run(&target("http://127.0.0.1:1".into()), &action(), 1, None);
            // No credential file exists, so no HTTP can be attempted. A local
            // refusal must not become an `unknown` successful JSON answer.
            match outcome {
                Err(error) => {
                    assert_eq!(error.kind(), "rejected");
                    std::process::exit(cadence_agent::error::exit_code_for_kind(error.kind()));
                }
                Ok(code) => std::process::exit(code),
            }
        }

        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", server.server_addr());
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let credential = json!({
            "version": "hosted-cadence-auth.v1", "organization_id": "ws_synthetic",
            "organization_slug": "synthetic", "audience": endpoint,
            "issuer": "https://issuer.board.localhost", "subject_id": "user_sender",
            "capabilities": ["cli.read", "cli.write"],
            "access_token": format!("hct_{}", "a".repeat(43)), "expires_at": expires
        });
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(auth.join("cli-synthetic.json"))
            .unwrap();
        file.write_all(&serde_json::to_vec(&credential).unwrap())
            .unwrap();
        drop(file);
        let case_owned = case.to_owned();
        let worker = std::thread::spawn(move || {
            let count = if case_owned == "reconcile" { 2 } else { 1 };
            let mut requests = Vec::new();
            for index in 0..count {
                let mut req = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .expect("bounded notify dispatch");
                assert_eq!(req.method().as_str(), "POST");
                assert_eq!(
                    req.url(),
                    format!("/__platform/hosted-cadence/ws_synthetic/local-agents/{AGENT}/notify")
                );
                let mut bytes = Vec::new();
                req.as_reader()
                    .take(32_769)
                    .read_to_end(&mut bytes)
                    .unwrap();
                assert!(bytes.len() <= 32_768);
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    body,
                    json!({"version": "hosted-cadence-agent-directory.v1",
                    "operation_id": OP, "kind": "fyi", "text": "Independent FYI only",
                    "references": [{"kind": "issue", "id": "SYN-1"}, {"kind": "wiki", "id": "global/synthetic"}]})
                );
                requests.push(body);
                let receipt = json!({"version": "hosted-cadence-agent-directory.v1",
                    "operation_id": OP, "notice_id": NOTICE, "agent_id": AGENT,
                    "kind": "fyi", "status": "stored"});
                let (status, body) = if index == 1 || case_owned == "stored" {
                    // One persisted receipt, returned unchanged for the explicit
                    // same-member/agent/content/key recovery. Not a second notice.
                    (200, receipt)
                } else {
                    // Producer rechecks humanActor after commitLocalEvents;
                    // these exact unproved authorization envelopes can follow it.
                    (401, json!({"ok": false, "code": "unauthorized"}))
                };
                req.respond(
                    tiny_http::Response::from_string(body.to_string()).with_status_code(status),
                )
                .unwrap();
            }
            // Completion is event-driven: notify returned before this probe.
            // An automatic replay would have occupied the next expected request
            // or be pending here, rather than being silently accepted.
            (server, requests)
        });
        let target = target(endpoint);
        let first = run(&target, &action(), 1, None);
        let second = if case == "reconcile" {
            Some(run(&target, &action(), 1, None))
        } else {
            None
        };
        let (server, requests) = worker.join().unwrap();
        assert!(
            server.try_recv().unwrap().is_none(),
            "unexpected automatic replay"
        );
        assert_eq!(requests.len(), if case == "reconcile" { 2 } else { 1 });
        if case == "reconcile" {
            assert_eq!(
                requests[0], requests[1],
                "recovery must not replace the operation id or content"
            );
            assert_eq!(second.unwrap().unwrap(), 0);
        }
        let code = match first {
            Ok(code) => code,
            Err(error) => cadence_agent::error::exit_code_for_kind(error.kind()),
        };
        std::process::exit(code);
    }

    #[test]
    fn cad2009_notify_consumer_preserves_uncertainty_and_identical_key_recovery() {
        if let Ok(case) = std::env::var(CHILD) {
            child(&case);
            return;
        }
        for (case, expected_exit) in [
            ("predispatch", 3),
            ("unknown", 1),
            ("stored", 0),
            ("reconcile", 1),
        ] {
            let home = tempfile::tempdir().unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([TEST, "--nocapture", "--test-threads=1"])
                .env_clear()
                .env(CHILD, case)
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path().join("config"))
                .env("XDG_STATE_HOME", home.path().join("state"))
                .env("PATH", "/usr/bin:/bin")
                .current_dir(home.path())
                .stdin(Stdio::null())
                .output()
                .unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            // libtest adds a prefix; deserialize real print_json documents from
            // the first opening brace, not test-authored echoed expected output.
            let values: Vec<Value> = stdout
                .find('{')
                .map(|offset| {
                    serde_json::Deserializer::from_str(&stdout[offset..])
                        .into_iter::<Value>()
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .unwrap()
                })
                .unwrap_or_default();
            assert_eq!(
                output.status.code(),
                Some(expected_exit),
                "{case}: stdout={stdout}; stderr={stderr}"
            );
            if case == "predispatch" {
                assert!(
                    !values.iter().any(|value| value["status"] == "unknown"),
                    "proven credential refusal was masked"
                );
            } else {
                assert_eq!(values.len(), if case == "reconcile" { 2 } else { 1 });
                assert_eq!(
                    values[0]["status"],
                    if case == "stored" {
                        "stored"
                    } else {
                        "unknown"
                    }
                );
                if case != "stored" {
                    assert_eq!(values[0]["operation_id"], OP);
                    assert_eq!(values[0]["agent_id"], AGENT);
                    let guidance = values[0]["retry"]
                        .as_str()
                        .expect("same-operation-id recovery guidance");
                    for pin in [
                        "same workspace",
                        "member",
                        "agent",
                        "--operation-id",
                        "text",
                        "references",
                    ] {
                        assert!(guidance.contains(pin), "recovery lost {pin}: {guidance}");
                    }
                    assert!(
                        guidance.contains("does not mean delivered"),
                        "FYI storage is not execution"
                    );
                }
                if case == "reconcile" {
                    assert_eq!(values[1]["status"], "stored");
                    assert_eq!(values[1]["notice_id"], NOTICE);
                }
            }
        }
    }
}
