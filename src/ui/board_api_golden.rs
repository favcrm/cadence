//! CAD-1792: the typed board-API bodies serialize to the same bytes the
//! `json!` builders did. Each golden is the pretty body captured from the
//! pre-typing code; `CADENCE_WRITE_GOLDEN=1` rewrites them.
use super::member_view::{member_view, redact_agents_payload, redact_master_state};
use super::serve::agents_payload_from;
use serde_json::Value;
use std::path::PathBuf;

pub(super) fn golden(name: &str, actual: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/ui/board_api_golden")
        .join(name);
    let text = serde_json::to_string_pretty(actual).unwrap() + "\n";
    if std::env::var_os("CADENCE_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(committed, text, "{name} differs from the committed golden");
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// One inbox row, one worker with a bound task and a running message, one
/// fenced worker missing `quota`/`usage_limit`, and an agent the roster
/// reports without its board slice.
fn roster() -> Value {
    parse(
        r#"{"agents": [
        {"alias": "obs", "provider": "inbox", "endpoint_kind": "inbox",
         "role": null, "team_role": null, "model_selection": null,
         "model_lookup_role": null, "model": null, "model_reported": null,
         "model_configured": null, "model_source": null, "effort": null,
         "effort_reported": null, "effort_source": null, "effort_applicable": null,
         "quota": null, "usage_limit": null, "thread_id": "", "session_id": "",
         "endpoint": "inbox://obs", "params": {"upstream": "lead"}, "dead": false,
         "inbox": {"queued": 3, "oldest_age_secs": 42.0}},
        {"alias": "lead", "provider": "claude", "endpoint_kind": "pty",
         "role": "worker", "team_role": "dev", "model_selection": {"source": "role"},
         "model_lookup_role": "dev", "model": "m-1", "model_reported": "m-1",
         "model_configured": "m-1", "model_source": "reported", "effort": "high",
         "effort_reported": "high", "effort_source": "reported", "effort_applicable": true,
         "quota": {"used": 1}, "usage_limit": null,
         "thread_id": "th-1", "session_id": "s-1", "endpoint": "e",
         "state": "running", "dead": false, "silent_secs": 4, "stalled": false,
         "state_label": null, "tasks": ["t1", "t-unbound"],
         "params": {"catalogue": "pm", "look": "bean", "kind": "hire"},
         "board": {"queued": 2, "unknown": 0, "parked": 1, "event_cursor": 9,
           "messages": [
             {"id": "m1", "state": "running", "task_id": "t1", "created": 1790562108.64,
              "started": "2026-09-29T03:00:00+02:00", "body": "do the thing"},
             {"id": "m2", "state": "done", "result": {"via": "pty_render_miss"},
              "completed": 1790000000.5}]}},
        {"alias": "stuck", "provider": "claude", "endpoint_kind": "pty",
         "state": "attention", "error": "run cadence agent unfence stuck",
         "thread_id": "", "session_id": "", "endpoint": "", "dead": true,
         "tasks": [], "params": null, "board": {"unknown": 1, "messages": []}},
        {"alias": "bare", "provider": "fake", "endpoint_kind": "fake",
         "state": "idle", "params": null}
    ]}"#,
    )
}

fn jobs() -> Value {
    parse(
        r#"{"jobs": [
        {"id": "j1", "issue": "CAD-7", "title": "Ship", "state": "running",
         "task_list": [{"id": "t1", "state": "running", "title": "Build"}]},
        {"id": "j2", "issue": "", "title": "No issue", "state": "idle", "task_list": []}
    ]}"#,
    )
}

#[test]
fn agents_payload_bytes_are_unchanged() {
    let dir = tempfile::TempDir::new().unwrap();
    let full = agents_payload_from(dir.path(), Some(roster()), Some(&jobs()));
    golden("agents_operator.json", &full);
    let mut member = full.clone();
    redact_agents_payload(&mut member);
    golden("agents_member.json", &member);
    golden(
        "agents_unreachable.json",
        &agents_payload_from(dir.path(), None, None),
    );
    golden(
        "agents_no_jobs.json",
        &agents_payload_from(dir.path(), Some(roster()), None),
    );
}

/// CAD-1795: what a member sees of the daemon's `master_state` and
/// `thread_read` replies, redacted from the operator goldens the daemon
/// producers are pinned to (`daemon::board_producer_golden`).
#[test]
fn member_views_of_master_state_and_threads_are_unchanged() {
    let read = |name: &str| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/ui/board_api_golden")
            .join(name);
        parse(&std::fs::read_to_string(path).unwrap())
    };
    for name in ["idle", "queued", "running"] {
        let mut state = read(&format!("master_state_{name}.json"));
        redact_master_state(&mut state);
        golden(&format!("master_state_{name}_member.json"), &state);
    }
    for name in ["forward", "tail"] {
        let mut page = read(&format!("thread_page_{name}.json"));
        member_view(&mut page);
        golden(&format!("thread_page_{name}_member.json"), &page);
    }
    // A worker's thread: members keep its entries, minus the operator-only kinds.
    let mut worker = read("thread_page_forward.json");
    worker["alias"] = Value::from("w1");
    let mut hire = worker["entries"][0].clone();
    hire["kind"] = Value::from("hire_proposal");
    worker["entries"].as_array_mut().unwrap().push(hire);
    member_view(&mut worker);
    golden("thread_page_worker_member.json", &worker);
}

/// CAD-1795: `GET /api/issues/:id` and `issue show --json`, byte for byte.
/// A detailed issue (links both ways, a missing link, comments, artifacts,
/// refs, a claim, acceptance items) and a bare one.
#[test]
fn issue_detail_bytes_are_unchanged() {
    use crate::issue::board::{self, Comment, Issue};
    use crate::issue::model::{Claim, CommentFront, Front};
    use crate::issue::work;
    use std::collections::HashMap;
    let new = |id: &str, parent: Option<&str>| {
        let mut front = Front::new(id, id, "2026-09-01T00:00:00Z");
        front.parent = parent.map(str::to_string);
        Issue {
            project: "cadence".into(),
            dir: PathBuf::from("/nonexistent").join(id),
            front,
            body: String::new(),
            comments: vec![],
            artifacts: vec![],
        }
    };
    let mut epic = new("CAD-1", None);
    epic.front.item_type = Some("epic".into());
    let mut rich = new("CAD-3", Some("CAD-1"));
    rich.front.status = "doing".into();
    rich.front.owner = Some("dev-1".into());
    rich.front.component = Some("ui".into());
    rich.front.size = Some("S".into());
    rich.front.tags = vec!["ui".into(), "m1".into()];
    rich.front.paths = vec!["src/ui/**".into()];
    rich.front.blocked_by = vec!["CAD-9".into(), "CAD-77".into()];
    rich.front.relates = vec!["CAD-2".into()];
    rich.front.duplicate_of = Some("CAD-2".into());
    rich.front.claim = Some(Claim {
        by: "pm".into(),
        at: "2026-09-02T00:00:00Z".into(),
        session: Some("s-1".into()),
        last_seen: None,
        note: Some("mine".into()),
        stale: None,
    });
    rich.front.refs = vec![serde_json::from_value(
        serde_json::json!({"kind": "pr", "url": "https://example.test/pr/1"}),
    )
    .unwrap()];
    rich.body = "Intro.\n\n## Acceptance\n- [x] first\n- [ ] second\n".into();
    rich.comments = vec![
        Comment {
            name: "20260903-000000-pm".into(),
            front: CommentFront {
                author: "pm".into(),
                at: "2026-09-03T00:00:00Z".into(),
                kind: Some("note".into()),
            },
            body: "hello".into(),
        },
        Comment {
            name: "20260904-000000-dev".into(),
            front: CommentFront {
                author: "dev-1".into(),
                at: "2026-09-04T00:00:00Z".into(),
                kind: None,
            },
            body: "world".into(),
        },
    ];
    rich.artifacts = vec![("out.txt".into(), 12)];
    let mut dup = new("CAD-2", Some("CAD-1"));
    dup.front.status = "done".into();
    let views = board::views(
        std::path::Path::new("/no-notes"),
        vec![epic, rich, dup, new("CAD-9", None)],
    );
    let by_id: HashMap<String, &board::View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let ctx = work::Ctx {
        by_id: &by_id,
        configs: HashMap::from([(
            "cadence".to_string(),
            work::ProjectWork {
                cfg: work::WorkConfig::default(),
                error: None,
                unapproved: None,
            },
        )]),
        now: 1_790_000_000,
    };
    let pm_dir = std::path::Path::new("/nonexistent");
    // The claim's age is measured from the wall clock.
    let details: Vec<Value> = views
        .iter()
        .map(|v| {
            let mut detail = work::detail_json(pm_dir, &ctx, v);
            if detail["claim"]["age_secs"].is_number() {
                detail["claim"]["age_secs"] = Value::from(0);
            }
            detail
        })
        .collect();
    golden("issue_details.json", &Value::Array(details));
}

/// CAD-1798: the drawer of a plan epic and of its ticket against a real PM
/// repo: the `git log` history and merged activity, the notes chain, the
/// plan block, and the epic's stage, progress and health. Commit and note
/// times are pinned so the abbreviated hashes are stable.
#[test]
fn issue_detail_with_history_notes_and_plan_bytes_are_unchanged() {
    use crate::issue::board::{self, Issue};
    use crate::issue::model::{Front, Plan};
    use crate::issue::work;
    use std::collections::HashMap;
    use std::process::Command;
    let root = tempfile::Builder::new().prefix("c98g").tempdir().unwrap();
    let pm = root.path().join("pm");
    let notes = root.path().join("notes");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::create_dir_all(&notes).unwrap();
    let git = |args: &[&str], date: &str| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(&pm)
            .args(args)
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
    };
    git(&["init", "-q"], "2026-09-01T00:00:00+0000");
    let new = |id: &str, parent: Option<&str>| {
        let dir = pm.join("cadence").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut front = Front::new(id, id, "2026-09-01T00:00:00Z");
        front.parent = parent.map(str::to_string);
        Issue {
            project: "cadence".into(),
            dir,
            front,
            body: String::new(),
            comments: vec![],
            artifacts: vec![],
        }
    };
    let mut epic = new("CAD-1", None);
    epic.front.item_type = Some("epic".into());
    epic.front.plan = Some(Plan {
        state: "approved".into(),
        proposed_by: "pm".into(),
        proposed_at: "2026-09-01T00:00:00Z".into(),
        tickets: vec!["CAD-3".into()],
        decided_by: Some("operator".into()),
        decided_at: Some("2026-09-02T00:00:00Z".into()),
        reason: None,
        workflow: None,
    });
    let mut task = new("CAD-3", Some("CAD-1"));
    task.front.size = Some("S".into());
    task.front.status = "doing".into();
    for (i, (issue, day)) in [(&epic, 1), (&task, 2)].into_iter().enumerate() {
        let file = issue.dir.join("issue.md");
        for (n, text) in ["first\n", "second\n"].into_iter().enumerate() {
            std::fs::write(&file, text).unwrap();
            let date = format!("2026-09-0{}T0{}:00:00+0000", day, n + 1);
            git(&["add", "-A"], &date);
            git(
                &[
                    "commit",
                    "-q",
                    "-m",
                    &format!("edit {} #{i}{n}", issue.front.id),
                ],
                &date,
            );
        }
    }
    std::fs::write(
        notes.join("20260905-120000-pm-kickoff.md"),
        "# Kickoff: start CAD-3\n> Issue: CAD-3\n\nGo.\n",
    )
    .unwrap();
    std::fs::write(
        notes.join("20260906-120000-rev-verdict.md"),
        "# Verdict: CAD-3 - pass\n> Issue: CAD-3\n\n## Verdict\npass\n",
    )
    .unwrap();
    let views = board::views(&notes, vec![epic, task]);
    let by_id: HashMap<String, &board::View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let ctx = work::Ctx {
        by_id: &by_id,
        configs: HashMap::from([(
            "cadence".to_string(),
            work::ProjectWork {
                cfg: work::WorkConfig::default(),
                error: None,
                unapproved: None,
            },
        )]),
        now: 1_790_000_000,
    };
    let details: Vec<Value> = views
        .iter()
        .map(|v| {
            let mut detail = work::detail_json(&pm, &ctx, v);
            // The temp root is the only machine-specific part.
            let path = detail["path"]
                .as_str()
                .unwrap()
                .replace(pm.to_str().unwrap(), "PM");
            detail["path"] = Value::from(path);
            detail
        })
        .collect();
    // Newer git prints a UTC `%aI` as `Z`, older git as `+00:00`. Only the
    // git-derived stamps are normalized: `history[].at` and the commit
    // entries of `activity`.
    let mut details = details;
    for detail in &mut details {
        for key in ["history", "activity"] {
            for entry in detail[key].as_array_mut().into_iter().flatten() {
                let git = key == "history" || entry["kind"] == "commit";
                if let Some(at) = entry["at"].as_str().and_then(|a| a.strip_suffix("+00:00")) {
                    if git {
                        entry["at"] = Value::from(format!("{at}Z"));
                    }
                }
            }
        }
    }
    golden("issue_details_history.json", &Value::Array(details));
}

/// `/api/meta`: the typed body equals the `json!` literal it replaced
/// (kept here as the oracle), for a signed-in and an unknown session.
#[test]
fn meta_bytes_are_unchanged() {
    use cadence_core::board_api::{DeliverySyncMeta, MetaDaemon, MetaResponse};
    use serde_json::json;
    let session = json!({"id": "s1", "origin": "loopback", "user": null, "created": 1});
    let proof = json!({"proven": false, "check": "peer", "why": "no proxy"});
    let cases = [
        (Some(true), session.clone(), proof.clone(), Some(true), true),
        (None, Value::Null, Value::Null, None, false),
    ];
    for (i, (signed_in, session, tailnet_proof, operator, full)) in cases.into_iter().enumerate() {
        let daemon =
            full.then(|| json!({"build_commit": "abc", "build_time": "t", "started_at": 7}));
        let sync = full.then(|| json!({"gh": "/usr/bin/gh", "every_secs": 60}));
        let oracle = json!({
            "read_only": false,
            "anonymous_reads": true,
            "signed_in": signed_in,
            "hosted": false,
            "session": session,
            "login_hint": "cadence ui login",
            "device_login": full,
            "tab_signed_out": false,
            "actor": "ui",
            "tailnet_proof": tailnet_proof,
            "operator": operator,
            "platform_account_configured": false,
            "tailnet_url": full.then_some("https://x.ts.net"),
            "version": "1.2.3",
            "build_commit": "deadbeef",
            "build_time": "now",
            "daemon": daemon,
            "delivery_sync": sync,
        });
        let typed = MetaResponse {
            read_only: false,
            anonymous_reads: true,
            signed_in,
            hosted: false,
            session,
            login_hint: "cadence ui login".into(),
            device_login: full,
            tab_signed_out: false,
            actor: "ui".into(),
            tailnet_proof,
            operator,
            platform_account_configured: false,
            tailnet_url: full.then(|| "https://x.ts.net".to_string()),
            version: "1.2.3".into(),
            build_commit: "deadbeef".into(),
            build_time: "now".into(),
            daemon: full.then(|| MetaDaemon {
                build_commit: json!("abc"),
                build_time: json!("t"),
                started_at: json!(7),
            }),
            delivery_sync: full.then(|| DeliverySyncMeta {
                gh: Some("/usr/bin/gh".into()),
                gh_error: None,
                every_secs: 60,
            }),
        };
        let typed = serde_json::to_value(typed).unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&typed).unwrap(),
            serde_json::to_string_pretty(&oracle).unwrap(),
            "case {i}"
        );
    }
    // A failing `gh` reports its error and no path.
    let err = DeliverySyncMeta {
        gh: None,
        gh_error: Some("no gh".into()),
        every_secs: 5,
    };
    assert_eq!(
        serde_json::to_value(err).unwrap(),
        json!({"gh": null, "gh_error": "no gh", "every_secs": 5})
    );
}

/// The committed core and board-API schemas match the Rust types. The core
/// crate checks this too, but only in the path-filtered core-wasm workflow;
/// this copy runs in the required `test` job on every PR and in the queue.
#[test]
fn committed_contract_schemas_match_core_types() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (path, generated) in cadence_core::contracts::committed_schemas() {
        let committed = std::fs::read_to_string(repo.join(path)).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "{path} is stale; regenerate with CADENCE_CORE_WRITE_SCHEMA=1 cargo test -p cadence-core, then bun run --cwd ui gen:board-api"
        );
    }
}
