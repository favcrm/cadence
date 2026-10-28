// CAD-535: CLI parse tests — moved verbatim from src/main.rs.
use super::*;

#[test]
fn scoped_live_count_uses_only_visible_rows() {
    let rows = json!({"agents": [
        {"alias": "group-a", "state": "busy", "dead": false},
        {"alias": "child-a", "state": "attention", "dead": false},
        {"alias": "child-b", "state": "busy", "dead": true}
    ], "live": 99});
    assert_eq!(count_live_agents(&rows), 1);
}

#[test]
fn placement_filter_recomputes_live_and_footer_counts() {
    let mut view = json!({"agents": [
        {"alias": "a", "provider": "pi", "endpoint_kind": "managed", "state": "busy", "dead": false},
        {"alias": "b", "provider": "devin", "endpoint_kind": "cloud", "state": "idle", "dead": false}
    ], "live": 2, "footer": {"live": 2}});
    retain_by_placement(&mut view, registry::Placement::Cloud);
    assert_eq!(view["agents"].as_array().unwrap().len(), 1);
    assert_eq!(view["agents"][0]["alias"], "b");
    assert_eq!(view["live"], json!(1));
    assert_eq!(view["footer"]["live"], json!(1));
}

#[test]
fn send_wait_refuses_flags_that_the_ask_path_cannot_carry() {
    let cases: &[&[&str]] = &[
        &["cadence", "send", "w", "--wait", "--priority", "urgent"],
        &["cadence", "send", "w", "--wait", "--supersedes", "m1"],
        &["cadence", "send", "w", "--wait", "--nudge"],
    ];
    for argv in cases {
        let err = Cli::try_parse_from(argv.iter().copied())
            .err()
            .expect("must conflict");
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "{argv:?}"
        );
    }
    assert!(Cli::try_parse_from(["cadence", "send", "w", "--wait"]).is_ok());
    assert!(Cli::try_parse_from(["cadence", "send", "w", "--wait", "30"]).is_ok());
}

#[test]
fn send_wait_and_message_ask_accept_the_same_wait_values() {
    for secs in ["0", "1", "600", "601"] {
        assert!(
            Cli::try_parse_from(["cadence", "send", "w", "--wait", secs]).is_ok(),
            "send --wait {secs}"
        );
        assert!(
            Cli::try_parse_from(["cadence", "message", "ask", "w", "--text", "hi", "--wait", secs])
                .is_ok(),
            "message ask --wait {secs}"
        );
    }
}

#[test]
fn dispatch_parses_without_to_for_the_master_route() {
    assert!(Cli::try_parse_from(["cadence", "dispatch", "CAD-1"]).is_ok());
    assert!(Cli::try_parse_from(["cadence", "dispatch", "CAD-1", "--to", "w"]).is_ok());
}

#[test]
fn hosted_enrollment_cli_requires_explicit_trust_and_has_no_secret_argv() {
    let args = [
        "cadence",
        "remote",
        "enrollment",
        "bootstrap",
        "--issuer",
        "https://api.example.test",
        "--org",
        "ws_real",
        "--audience",
        "https://real.board.example.test",
        "--client-agent",
        "worker",
        "--enrollment-dir",
        "/tmp/cad717-enroll",
    ];
    assert!(Cli::try_parse_from(args).is_ok());
    assert!(Cli::try_parse_from([
        "cadence",
        "remote",
        "enrollment",
        "bootstrap",
        "--org",
        "ws_real",
        "--audience",
        "https://real.board.example.test",
        "--client-agent",
        "worker",
        "--enrollment-dir",
        "/tmp/cad717-enroll"
    ])
    .is_err());
    assert!(Cli::try_parse_from(args.into_iter().chain(["--token", "hcs_secret"])).is_err());
}

#[test]
fn remote_auth_cli_supports_hosted_portal_and_never_accepts_token_argv() {
    assert!(Cli::try_parse_from(["cadence", "login"]).is_ok());
    assert!(Cli::try_parse_from(["cadence", "login", "fav-limited"]).is_ok());
    assert!(
        Cli::try_parse_from(["cadence", "login", "--issuer", "https://api.example.com"]).is_ok()
    );
    assert!(Cli::try_parse_from([
        "cadence",
        "login",
        "--issuer",
        "https://api.example.com",
        "--org",
        "ws_test",
        "--token-stdin"
    ])
    .is_ok());
    assert!(Cli::try_parse_from([
        "cadence",
        "login",
        "--issuer",
        "https://api.example.com",
        "--token-stdin"
    ])
    .is_err());
    assert!(
        Cli::try_parse_from(["cadence", "login", "--org", "ws_test", "--token-stdin"]).is_err()
    );
    assert!(Cli::try_parse_from(["cadence", "login", "--token", "secret"]).is_err());
    assert!(Cli::try_parse_from(["cadence", "login", "--token-stdin"]).is_err());
    assert!(Cli::try_parse_from(["cadence", "register"]).is_ok());
    assert!(Cli::try_parse_from([
        "cadence",
        "register",
        "--name",
        "Acme Labs",
        "--slug",
        "acme-labs",
        "--use",
        "--no-open",
        "--json"
    ])
    .is_ok());
    assert!(Cli::try_parse_from([
        "cadence",
        "auth",
        "status",
        "--issuer",
        "https://api.example.com",
        "--org",
        "ws_test"
    ])
    .is_ok());
    assert!(Cli::try_parse_from([
        "cadence",
        "auth",
        "logout",
        "--auth-dir",
        "/tmp/isolated-auth"
    ])
    .is_ok());
}

#[test]
fn cad314_backup_export_restore_parse() {
    let cli = Cli::try_parse_from(["cadence", "backup"]).unwrap();
    match cli.command {
        Commands::Backup { dir, keep, reason } => {
            assert_eq!(dir, None);
            assert_eq!(keep, 7, "the nightly default keeps a week");
            assert_eq!(reason, "manual");
        }
        _ => panic!("expected backup"),
    }
    assert!(Cli::try_parse_from(["cadence", "backup", "--keep", "0"]).is_err());
    let cli = Cli::try_parse_from(["cadence", "export", "--out", "/tmp/b"]).unwrap();
    assert!(matches!(cli.command, Commands::Export { .. }));
    assert!(Cli::try_parse_from(["cadence", "export"]).is_err());
    let cli = Cli::try_parse_from([
        "cadence", "restore", "/tmp/b", "--repo", "/a", "--repo", "/b", "--force",
    ])
    .unwrap();
    match cli.command {
        Commands::Restore {
            source,
            repos,
            force,
            ..
        } => {
            assert_eq!(source, Some(PathBuf::from("/tmp/b")));
            assert_eq!(repos, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
            assert!(force);
        }
        _ => panic!("expected restore"),
    }
}

#[test]
fn cad561_progress_lines_are_written_once_when_stdout_is_the_log() {
    // The board starts the detached helper with stdout pointing at
    // the progress log; `line` must not print there too (CAD-561 r3).
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("update-progress.jsonl");
    std::fs::write(&log, "").unwrap();
    let file = std::fs::File::open(&log).unwrap();
    use std::os::fd::AsRawFd;
    assert!(fd_is_path(file.as_raw_fd(), &log), "an open fd is its file");
    let other = dir.path().join("other");
    std::fs::write(&other, "").unwrap();
    assert!(!fd_is_path(file.as_raw_fd(), &other));
    assert!(!fd_is_path(libc::STDOUT_FILENO, &log));
}

#[test]
fn devin_resume_parses_like_native() {
    let cli = Cli::try_parse_from(["cadence", "devin", "-r", "cookie-cesium"]).unwrap();
    match cli.command {
        Commands::Devin { resume, detach, .. } => {
            assert_eq!(resume.as_deref(), Some("cookie-cesium"));
            assert!(!detach);
        }
        _ => panic!("expected devin subcommand"),
    }
}

#[test]
fn devin_detach_opts_out() {
    let cli = Cli::try_parse_from(["cadence", "devin", "--detach"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Devin {
            resume: None,
            detach: true,
            ..
        }
    ));
}

#[test]
fn devin_permission_flags_parse() {
    let cli = Cli::try_parse_from(["cadence", "devin", "--permission-mode", "smart"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Devin {
            permission_mode: Some(m),
            bypass: false,
            ..
        } if m == "smart"
    ));
    let cli = Cli::try_parse_from(["cadence", "devin", "--bypass"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Devin {
            permission_mode: None,
            bypass: true,
            ..
        }
    ));
}

#[test]
fn devin_cloud_parses_repo_and_refuses_pty_permission_flags() {
    parsed_cloud_repo();
    cloud_permission_flags_are_refused();
}

#[inline(never)]
fn parsed_cloud_repo() {
    let cli = Cli::try_parse_from([
        "cadence",
        "--state-dir",
        "/tmp/cadence",
        "devin",
        "--cloud",
        "--cloud-params",
        "repo=favcrm/cadence;devin_mode=fast",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Devin {
            cloud: true,
            cloud_params: Some(params),
            ..
        } if params == "repo=favcrm/cadence;devin_mode=fast"
    ));
    assert!(Cli::try_parse_from(["cadence", "devin", "--cloud-params", "repo=x/y",]).is_err());
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    let help = cmd
        .find_subcommand_mut("devin")
        .unwrap()
        .render_help()
        .to_string();
    assert!(help.contains("--cloud"), "{help}");
    assert!(help.contains("--cloud-params"), "{help}");
    assert!(Cli::try_parse_from(["cadence", "devin", "--", "--cloud"]).is_err());
}

#[inline(never)]
fn cloud_permission_flags_are_refused() {
    let mut refused = DevinOpts {
        permission_mode: Some("smart".into()),
        cloud: true,
        ..DevinOpts::default()
    };
    apply_cloud_params(&mut refused, &["repo=favcrm/cadence".into()]).unwrap();
    assert!(insert_devin_cloud_params(&mut serde_json::Map::new(), &refused).is_err());
    let bypassed = DevinOpts {
        bypass: true,
        cloud: true,
        ..DevinOpts::default()
    };
    assert!(insert_devin_cloud_params(&mut serde_json::Map::new(), &bypassed).is_err());
    let err = launch_endpoint_kind("devin", true, true).unwrap_err();
    assert!(err
        .to_string()
        .contains("--cloud cannot be combined with --tui"));
    assert!(launch_endpoint_kind("claude", true, false).is_err());
}

#[test]
fn join_devin_cloud_parses_repo() {
    parsed_join_cloud_repo();
}

#[inline(never)]
#[test]
fn cloud_bootstrap_prompt_inlines_role_without_a_host_path() {
    let body = cloud_session_prompt(
        "w",
        "pm",
        Some("Ship the widget. Read /secret/host/role.md before you start."),
        "do the work, then finish",
    );
    assert!(body.contains("Ship the widget."), "{body}");
    assert!(body.contains("before you start."), "{body}");
    assert!(!body.contains("/secret/host/role.md"), "{body}");
    assert!(body.contains("SHA:"), "{body}");
    assert!(!body.contains('/'), "{body}");
    assert!(!body.contains("cadence self"), "{body}");
    assert!(!body.contains("on disk"), "{body}");
}

fn parsed_join_cloud_repo() {
    let cli = Cli::try_parse_from([
        "cadence",
        "join",
        "pm",
        "devin",
        "--cloud",
        "--cloud-params=repo=favcrm/cadence",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Join {
            tui: false,
            cloud: true,
            cloud_params: Some(params),
            ..
        } if params == "repo=favcrm/cadence"
    ));
}

#[test]
fn cloud_param_maps_launch_keys() {
    let mut opts = DevinOpts::default();
    assert!(apply_cloud_params(
        &mut opts,
        &[
            "repo=favcrm/cadence".into(),
            "devin_mode=fast".into(),
            "max_acu_limit=4".into(),
            "knowledge_id=k1".into(),
            "tag=team".into(),
            "bypass_approval=true".into(),
        ],
    )
    .is_ok());
    assert_eq!(opts.repos, vec!["favcrm/cadence".to_string()]);
    assert_eq!(opts.devin_mode.as_deref(), Some("fast"));
    assert_eq!(opts.max_acu_limit, Some(4));
    assert_eq!(opts.knowledge_ids, vec!["k1".to_string()]);
    assert_eq!(opts.tags, vec!["team".to_string()]);
    assert!(opts.bypass_approval);
    assert!(apply_cloud_params(&mut opts, &["devin_mode=turbo".into()]).is_err());
    assert!(apply_cloud_params(&mut opts, &["nope=1".into()]).is_err());
    assert!(apply_cloud_params(&mut opts, &["max_acu_limit=0".into()]).is_err());
}

#[test]
fn devin_bypass_conflicts_with_permission_mode() {
    // Same rule as the claude verb — the shorthand must not fight
    // an explicit mode.
    assert!(
        Cli::try_parse_from(["cadence", "devin", "--permission-mode", "smart", "--bypass"])
            .is_err()
    );
}

#[test]
fn join_permission_flags_parse_for_devin() {
    let cli = Cli::try_parse_from([
        "cadence",
        "join",
        "pm",
        "devin",
        "--permission-mode",
        "dangerous",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Join {
            permission_mode: Some(m),
            ..
        } if m == "dangerous"
    ));
    let cli = Cli::try_parse_from(["cadence", "join", "pm", "devin", "--bypass"]).unwrap();
    assert!(matches!(cli.command, Commands::Join { bypass: true, .. }));
    assert!(Cli::try_parse_from([
        "cadence",
        "join",
        "pm",
        "devin",
        "--permission-mode",
        "auto",
        "--bypass"
    ])
    .is_err());
}

#[test]
fn audit_approval_readback_parses() {
    let head = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    let cli =
        Cli::try_parse_from(["cadence", "audit", "approval", "--pr", "7", "--head", head]).unwrap();
    let Commands::Audit {
        action: Some(AuditAction::Approval { pr, repo, .. }),
        ..
    } = cli.command
    else {
        panic!("audit approval must parse to AuditAction::Approval");
    };
    assert_eq!((pr, repo), (7, None));
    assert!(Cli::try_parse_from(["cadence", "audit", "approval", "--pr", "7"]).is_err());
}

#[test]
fn audit_scope_readback_parses() {
    let cli = Cli::try_parse_from(["cadence", "audit", "scope", "--issue", "CAD-9"]).unwrap();
    let Commands::Audit {
        action: Some(AuditAction::Scope { issue }),
        ..
    } = cli.command
    else {
        panic!("audit scope must parse to AuditAction::Scope");
    };
    assert_eq!(issue, "CAD-9");
    assert!(Cli::try_parse_from(["cadence", "audit", "scope"]).is_err());
}

#[test]
fn audit_approve_and_revoke_parse() {
    let head = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    let cli = Cli::try_parse_from([
        "cadence", "audit", "approve", "--pr", "84", "--head", head, "--source", "op",
    ])
    .unwrap();
    let Commands::Audit {
        action:
            Some(AuditAction::Approve {
                pr,
                action,
                id,
                repo,
                ..
            }),
        ..
    } = cli.command
    else {
        panic!("audit approve must parse to AuditAction::Approve");
    };
    assert_eq!(
        (pr, action.as_str(), id, repo),
        (Some(84), "merge", None, None)
    );
    assert!(Cli::try_parse_from(["cadence", "audit", "revoke", "ap-1", "--source", "op"]).is_err());
    assert!(matches!(
        Cli::try_parse_from(["cadence", "audit", "--json"])
            .unwrap()
            .command,
        Commands::Audit {
            action: None,
            json: true,
            ..
        }
    ));
    // Report flags and evidence verbs never mix.
    assert!(Cli::try_parse_from([
        "cadence", "audit", "--json", "approve", "--pr", "1", "--head", head, "--source", "op",
    ])
    .is_err());
}

#[test]
fn broker_approvals_flags_parse() {
    // `cadence claude` — broker parses; refused with --bypass/--tui;
    // the timeout requires the flag.
    let cli = Cli::try_parse_from([
        "cadence",
        "claude",
        "--broker-approvals",
        "--permission-timeout-secs",
        "60",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Claude {
            broker_approvals: true,
            permission_timeout_secs: Some(60),
            ..
        }
    ));
    assert!(Cli::try_parse_from(["cadence", "claude", "--broker-approvals", "--bypass"]).is_err());
    assert!(Cli::try_parse_from(["cadence", "claude", "--broker-approvals", "--tui"]).is_err());
    assert!(Cli::try_parse_from(["cadence", "claude", "--permission-timeout-secs", "60"]).is_err());
    // `join <pm> claude` carries the same surface.
    let cli = Cli::try_parse_from([
        "cadence",
        "join",
        "pm",
        "claude",
        "--broker-approvals",
        "--permission-timeout-secs",
        "30",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Join {
            broker_approvals: true,
            permission_timeout_secs: Some(30),
            ..
        }
    ));
    assert!(Cli::try_parse_from([
        "cadence",
        "join",
        "pm",
        "claude",
        "--broker-approvals",
        "--tui"
    ])
    .is_err());
    // `agent respond --reason` and the hidden MCP verb parse.
    let cli = Cli::try_parse_from([
        "cadence",
        "agent",
        "respond",
        "w1",
        "--request",
        "perm-1",
        "--decision",
        "decline",
        "--reason",
        "not safe",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Agent {
            action: AgentAction::Respond {
                reason: Some(r), ..
            },
            ..
        } if r == "not safe"
    ));
    assert!(Cli::try_parse_from(["cadence", "mcp-permission"]).is_ok());
}

#[test]
fn attach_flag_is_removed() {
    assert!(Cli::try_parse_from(["cadence", "devin", "--attach"]).is_err());
    assert!(Cli::try_parse_from(["cadence", "codex", "--attach"]).is_err());
}

#[test]
fn codex_shortcut_parses() {
    let cli = Cli::try_parse_from(["cadence", "codex", "--cwd", "/tmp"]).unwrap();
    assert!(matches!(cli.command, Commands::Codex { detach: false, .. }));
}

#[test]
fn codex_detach_parses() {
    let cli = Cli::try_parse_from(["cadence", "codex", "--detach"]).unwrap();
    assert!(matches!(cli.command, Commands::Codex { detach: true, .. }));
}

#[test]
fn join_parses_group_and_provider() {
    let cli = Cli::try_parse_from(["cadence", "join", "pm-alias", "devin"]).unwrap();
    match cli.command {
        Commands::Join {
            group,
            provider,
            detach,
            role,
            ..
        } => {
            assert_eq!(group, "pm-alias");
            assert_eq!(provider, "devin");
            assert!(!detach);
            assert_eq!(role, "worker");
        }
        _ => panic!("expected join"),
    }
}

#[test]
fn join_detach_and_resume_parse() {
    let cli =
        Cli::try_parse_from(["cadence", "join", "pm", "codex", "-r", "sess", "--detach"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Join {
            detach: true,
            resume: Some(r),
            ..
        } if r == "sess"
    ));
}

#[test]
fn attach_parses_optional_name() {
    let cli = Cli::try_parse_from(["cadence", "attach"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Attach {
            name: None,
            print: false
        }
    ));
    let cli = Cli::try_parse_from(["cadence", "attach", "devin", "--print"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Attach {
            name: Some(n),
            print: true
        } if n == "devin"
    ));
}

#[test]
fn self_command_parses() {
    let cli = Cli::try_parse_from(["cadence", "self"]).unwrap();
    assert!(matches!(cli.command, Commands::SelfInfo));
}

#[test]
fn job_command_tree_parses() {
    let cli = Cli::try_parse_from([
        "cadence",
        "job",
        "new",
        "--pm",
        "pm",
        "--spec",
        "s.md",
        "--issue",
        "CAD-26",
        "--max-revisions",
        "3",
        "--stall-secs",
        "45",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Job {
            action: JobAction::New {
                max_revisions: 3,
                stall_secs: Some(45),
                ..
            }
        }
    ));
    let cli =
        Cli::try_parse_from(["cadence", "job", "dispatch", "t1", "--to", "w2", "--ready"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Job {
            action: JobAction::Dispatch { ready: true, .. }
        }
    ));
    // A verdict with no flag is a parse-level error, not a default.
    assert!(Cli::try_parse_from(["cadence", "job", "verdict", "t1", "--sha", "x",]).is_err());
    let cli = Cli::try_parse_from([
        "cadence",
        "job",
        "verdict",
        "t1",
        "--sha",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--pass",
        "--reviewer",
        "rev",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Job {
            action: JobAction::Verdict { pass: true, .. }
        }
    ));
    let cli = Cli::try_parse_from([
        "cadence",
        "job",
        "task",
        "sha",
        "t1",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Job {
            action: JobAction::Task {
                action: TaskAction::Sha { .. }
            }
        }
    ));
    let cli =
        Cli::try_parse_from(["cadence", "send", "w1", "--text", "hi", "--task", "t1"]).unwrap();
    match cli.command {
        Commands::Send { task, .. } => {
            assert_eq!(task.as_deref(), Some("t1"));
        }
        _ => panic!(),
    }
}

#[test]
fn send_ready_parses() {
    let cli = Cli::try_parse_from([
        "cadence", "message", "send", "w1", "--text", "hi", "--ready",
    ])
    .unwrap();
    match cli.command {
        Commands::Message {
            action: MessageAction::Send { ready, alias, .. },
        } => {
            assert!(ready);
            assert_eq!(alias, "w1");
        }
        _ => panic!("expected message send"),
    }
    // Without --ready the flag defaults off.
    let cli = Cli::try_parse_from(["cadence", "message", "send", "w1", "--text", "hi"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Message {
            action: MessageAction::Send { ready: false, .. }
        }
    ));
}

/// CAD-158: `--priority` and comma-separated `--supersedes` parse on
/// both send forms; `--nudge` takes neither.
#[test]
fn send_steering_flags_parse() {
    let cli = Cli::try_parse_from([
        "cadence",
        "send",
        "w1",
        "--text",
        "current scope",
        "--priority",
        "urgent",
        "--supersedes",
        "m1,m2",
    ])
    .unwrap();
    match cli.command {
        Commands::Send { steer, .. } => {
            assert_eq!(steer.priority.as_deref(), Some("urgent"));
            assert_eq!(steer.supersedes, ["m1", "m2"]);
        }
        _ => panic!("expected send"),
    }
    let cli = Cli::try_parse_from([
        "cadence",
        "message",
        "send",
        "w1",
        "--text",
        "x",
        "--supersedes",
        "m3",
    ])
    .unwrap();
    match cli.command {
        Commands::Message {
            action: MessageAction::Send { steer, .. },
        } => {
            assert_eq!(steer.priority, None);
            assert_eq!(steer.supersedes, ["m3"]);
        }
        _ => panic!("expected message send"),
    }
    assert!(
        Cli::try_parse_from(["cadence", "send", "w1", "--text", "x", "--priority", "high"])
            .is_err()
    );
    assert!(Cli::try_parse_from([
        "cadence",
        "send",
        "w1",
        "--text",
        "x",
        "--nudge",
        "--priority",
        "urgent"
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "cadence",
        "send",
        "w1",
        "--text",
        "x",
        "--nudge",
        "--supersedes",
        "m1"
    ])
    .is_err());
}

#[test]
fn message_list_parses_both_spellings_and_rejects_a_bad_filter() {
    let cli = Cli::try_parse_from([
        "cadence",
        "message",
        "ls",
        "--to",
        "worker",
        "--state",
        "queued,running",
        "--limit",
        "5",
    ])
    .unwrap();
    match cli.command {
        Commands::Message {
            action: MessageAction::List {
                to, state, limit, ..
            },
        } => {
            assert_eq!(to.as_deref(), Some("worker"));
            assert_eq!(state, ["queued", "running"]);
            assert_eq!(limit, Some(5));
        }
        _ => panic!("expected message list"),
    }
    assert!(Cli::try_parse_from(["cadence", "message", "list", "worker"]).is_ok());
    let missing = super::message::list_params(None, None, None, None, vec![], None, None)
        .unwrap_err()
        .to_string();
    assert!(missing.contains("recipient"), "{missing}");
    let bad = super::message::list_params(
        Some("worker".into()),
        None,
        None,
        None,
        vec!["nope".into()],
        None,
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(bad.contains("unknown"), "{bad}");
    let wide = super::message::list_params(
        Some("worker".into()),
        None,
        None,
        None,
        vec![],
        Some(51),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(wide.contains("1 to 50"), "{wide}");
}

#[test]
fn message_cancel_parses() {
    let cli = Cli::try_parse_from([
        "cadence",
        "message",
        "cancel",
        "m1",
        "--by",
        "board-dev",
        "--reason",
        "wrong spec",
    ])
    .unwrap();
    match cli.command {
        Commands::Message {
            action:
                MessageAction::Cancel {
                    message,
                    by,
                    reason,
                },
        } => {
            assert_eq!(message, "m1");
            assert_eq!(by.as_deref(), Some("board-dev"));
            assert_eq!(reason.as_deref(), Some("wrong spec"));
        }
        _ => panic!("expected message cancel"),
    }
    // Flags optional — bare id parses with None defaults.
    let cli = Cli::try_parse_from(["cadence", "message", "cancel", "m2"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Message {
            action: MessageAction::Cancel {
                by: None,
                reason: None,
                ..
            }
        }
    ));
}

#[test]
fn send_verb_and_ask_flags_parse() {
    // Top-level `send` carries the same flag surface as
    // `message send`.
    let cli = Cli::try_parse_from(["cadence", "send", "w1", "--text", "hi", "--ready"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Send {
            ready: true,
            alias,
            ..
        } if alias.as_deref() == Some("w1")
    ));
    let cli = Cli::try_parse_from([
        "cadence",
        "message",
        "ask",
        "w1",
        "--text",
        "hi",
        "--ready",
        "--reply-to",
        "pm",
        "--wait",
        "30",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Message {
            action: MessageAction::Ask {
                ready: true,
                wait: 30,
                reply_to: Some(r),
                ..
            }
        } if r == "pm"
    ));
}

#[test]
fn worktree_flags_parse() {
    let cli = Cli::try_parse_from(["cadence", "devin", "--worktree", "feat-a"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Devin {
            worktree: Some(w),
            ..
        } if w == "feat-a"
    ));
    // All three launch paths carry the flag.
    let cli = Cli::try_parse_from(["cadence", "codex", "--worktree", "feat-b"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Codex {
            worktree: Some(w),
            ..
        } if w == "feat-b"
    ));
    let cli = Cli::try_parse_from([
        "cadence",
        "join",
        "pm",
        "devin",
        "--worktree",
        "wt1",
        "--no-bootstrap",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Join {
            worktree: Some(w),
            no_bootstrap: true,
            ..
        } if w == "wt1"
    ));
}

#[test]
fn agent_remove_and_gc_parse() {
    let cli = Cli::try_parse_from(["cadence", "agent", "remove", "w1"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Agent {
            action: AgentAction::Remove { alias, force: false }
        } if alias == "w1"
    ));
    let cli = Cli::try_parse_from(["cadence", "agent", "remove", "w1", "--force"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Agent {
            action: AgentAction::Remove { force: true, .. }
        }
    ));
    let cli = Cli::try_parse_from(["cadence", "agent", "gc", "--older-than", "2d"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Agent {
            action: AgentAction::Gc {
                older_than: Some(d),
                force: false,
            }
        } if d == "2d"
    ));
}

#[test]
fn bootstrap_flags_parse() {
    let cli = Cli::try_parse_from(["cadence", "devin", "--bootstrap", "--detach"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Devin {
            bootstrap: true,
            no_bootstrap: false,
            ..
        }
    ));
    // --bootstrap and --no-bootstrap conflict.
    assert!(Cli::try_parse_from(["cadence", "devin", "--bootstrap", "--no-bootstrap"]).is_err());
    let cli = Cli::try_parse_from(["cadence", "codex", "--no-bootstrap"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Codex {
            no_bootstrap: true,
            ..
        }
    ));
    let cli = Cli::try_parse_from(["cadence", "agent", "bootstrap", "w1"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Agent {
            action: AgentAction::Bootstrap { alias }
        } if alias == "w1"
    ));
}

#[test]
fn agents_block_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("AGENTS.md");
    // Created from nothing.
    ensure_agents_block(dir.path()).unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    assert!(first.contains(AGENTS_BEGIN) && first.contains(AGENTS_END));
    // Second run is a no-op — content byte-identical.
    ensure_agents_block(dir.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    // Existing file without markers keeps its content; block appends.
    std::fs::write(&path, "# My repo\n\ncustom notes — no trailing nl").unwrap();
    ensure_agents_block(dir.path()).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# My repo\n\ncustom notes — no trailing nl\n"));
    assert!(text.contains(AGENTS_BEGIN));
    assert_eq!(text.matches(AGENTS_BEGIN).count(), 1);
}

/// CAD-1530: a block an earlier build wrote is repaired, because a
/// repository carrying it verbatim never edited it. Anything else —
/// including a block a repository did edit — is left exactly as found.
#[test]
fn agents_block_repairs_only_a_block_it_recognises_as_its_own() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("AGENTS.md");
    let old = super::briefing::SUPERSEDED_AGENTS_BLOCKS[0];
    let current = super::briefing::agents_block();
    assert_ne!(old, current, "the superseded block is the current one");

    // The known older block, with repository text on both sides.
    std::fs::write(&path, format!("# My repo\n\n{old}\n## Notes\nafter\n")).unwrap();
    ensure_agents_block(dir.path()).unwrap();
    let repaired = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        repaired,
        format!("# My repo\n\n{current}\n## Notes\nafter\n")
    );

    // Repairing is idempotent.
    ensure_agents_block(dir.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), repaired);

    // A block that is neither the current one nor a superseded one is an
    // edit: markers present, and still nothing is touched.
    let edited = repaired.replacen("and discover peers", "and also discover peers", 1);
    std::fs::write(&path, &edited).unwrap();
    ensure_agents_block(dir.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    // A begin marker with no end marker is not an invitation to append a
    // second block: the file is left alone, as it was before CAD-1530.
    // The review caught this — the first version of this change appended
    // here, widening what cadence will write into someone else's repo.
    let unclosed = "# My repo\n\n<!-- cadence:begin -->\nstray\n";
    std::fs::write(&path, unclosed).unwrap();
    ensure_agents_block(dir.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), unclosed);
}

#[test]
fn group_lifecycle_commands_parse() {
    let cli = Cli::try_parse_from(["cadence", "resume", "pm1"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Resume {
            group: Some(g),
            all: false,
            detach: false,
        } if g == "pm1"
    ));
    let cli = Cli::try_parse_from(["cadence", "resume", "--all"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Resume {
            group: None,
            all: true,
            ..
        }
    ));
    // group and --all conflict.
    assert!(Cli::try_parse_from(["cadence", "resume", "pm1", "--all"]).is_err());
    // bare `resume` needs one of them.
    assert!(Cli::try_parse_from(["cadence", "resume"]).is_err());
}

#[test]
fn model_default_flags_parse_and_conflict() {
    let cli = Cli::try_parse_from([
        "cadence",
        "claude",
        "--team-role",
        "ops",
        "--provider-default-model",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Claude {
            team_role: Some(role),
            provider_default_model: true,
            model: None,
            ..
        } if role == "ops"
    ));
    assert!(Cli::try_parse_from([
        "cadence",
        "claude",
        "--model",
        "sonnet",
        "--provider-default-model",
    ])
    .is_err());
    assert!(Cli::try_parse_from(["cadence", "devin", "--provider-default-model"]).is_err());
    let cli = Cli::try_parse_from(["cadence", "devin", "--team-role", "qa"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Devin {
            team_role: Some(role),
            ..
        } if role == "qa"
    ));
    let cli = Cli::try_parse_from([
        "cadence",
        "join",
        "pm",
        "codex",
        "--team-role",
        "dev",
        "--provider-default-model",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Join {
            team_role: Some(role),
            provider_default_model: true,
            ..
        } if role == "dev"
    ));
    let cli = Cli::try_parse_from([
        "cadence",
        "agent",
        "register",
        "w1",
        "--provider",
        "claude",
        "--team-role",
        "qa",
        "--provider-default-model",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Agent {
            action: AgentAction::Register {
                team_role: Some(role),
                provider_default_model: true,
                ..
            }
        } if role == "qa"
    ));
}

#[test]
fn group_stop_and_daemon_start_parse() {
    let cli = Cli::try_parse_from(["cadence", "resume", "pm1", "--detach"]).unwrap();
    assert!(matches!(cli.command, Commands::Resume { detach: true, .. }));
    let cli = Cli::try_parse_from(["cadence", "stop", "pm1"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Stop { group } if group == "pm1"
    ));
    let cli = Cli::try_parse_from(["cadence", "daemon", "start", "--resume"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Daemon {
            action: DaemonAction::Start {
                resume: true,
                as_identity: None,
            }
        }
    ));
}

#[test]
fn session_mismatch_marks_unrecoverable_only() {
    // The two provider texts for "pane bound a different session".
    assert!(session_mismatch("pane owns session 'x', expected 'y'"));
    assert!(session_mismatch("pane acquired session 'x', expected 'y'"));
    // Nearby errors must NOT get the remove-and-rejoin hint.
    assert!(!session_mismatch("endpoint did not open within 15s"));
    assert!(!session_mismatch("Agent is still starting or stopping"));
    assert!(!session_mismatch("Unexpected provider completion status"));
}

#[test]
fn durations_parse() {
    assert_eq!(parse_duration("30").unwrap(), 30.0);
    assert_eq!(parse_duration("5m").unwrap(), 300.0);
    assert_eq!(parse_duration("2h").unwrap(), 7200.0);
    assert_eq!(parse_duration("1d").unwrap(), 86400.0);
    assert!(parse_duration("bogus").is_err());
    assert!(parse_duration("-1h").is_err());
}

/// Every `cadence …` command the overview can emit must parse —
/// a row carrying a command the CLI rejects is worse than no row.
#[test]
fn overview_commands_all_parse() {
    use cadence_agent::issue::{self, write};
    use cadence_agent::overview as ov;

    let assert_parses = |cmd: &str| {
        let argv = shlex::split(cmd).unwrap_or_else(|| panic!("'{cmd}' does not split"));
        Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("'{cmd}' does not parse: {e}"));
    };

    // Every command template, with realistic substitutions.
    for cmd in [
        ov::cmd_agent_unfence("w1"),
        ov::cmd_agent_show("w1"),
        ov::cmd_agent_resume("w1"),
        ov::cmd_inbox("pm"),
        ov::cmd_agent_respond("w1", "abc123", "cadence/approval"),
        ov::cmd_agent_respond("w1", "abc123", "item/commandExecution/requestApproval"),
        ov::cmd_agent_respond("w1", "abc123", "item/tool/requestUserInput"),
        ov::cmd_agent_respond("w1", "abc123", "session/request_permission"),
        ov::cmd_agent_respond("w1", "abc123", "totally/unknownMethod"),
        ov::cmd_issue_show("CAD-3"),
        ov::cmd_issue_set_ready("CAD-5"),
        ov::cmd_delivery_merge("CAD-6"),
        ov::cmd_delivery_decline("CAD-6"),
        ov::cmd_delivery_sync("CAD-6"),
        ov::CMD_ISSUE_SYNC.to_string(),
        ov::CMD_RESTART_WHEN_IDLE.to_string(),
        ov::CMD_UPGRADE_LATEST_MAIN.to_string(),
        cadence_agent::upgrade::restart_command(None),
        cadence_agent::upgrade::restart_command(Some("operator:name")),
    ] {
        assert_parses(&cmd);
    }

    // Rows emitted against a real tracker parse with real ids.
    let dir = tempfile::tempdir().unwrap();
    let pm_dir = dir.path().join("pm");
    let pm = issue::Pm::init(&pm_dir).unwrap();
    write::project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
    write::new_issue(
        &pm,
        dir.path(),
        Some("cadence"),
        "blocker",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "t",
    )
    .unwrap();
    write::new_issue(
        &pm,
        dir.path(),
        Some("cadence"),
        "blocked work",
        None,
        None,
        &["CAD-1".to_string()],
        None,
        None,
        &[],
        None,
        None,
        "t",
    )
    .unwrap();
    write::set_fields(
        &pm,
        &["CAD-1".to_string()],
        &["status=done".to_string()],
        "t",
        Some("test fixture"),
    )
    .unwrap();
    let view = ov::overview(&dir.path().join("state"), &pm_dir);
    let emitted: Vec<String> = view["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["command"].as_str().map(str::to_string))
        .filter(|c| c.starts_with("cadence "))
        .collect();
    assert!(
        emitted.iter().any(|c| c.contains("status=ready")),
        "{emitted:?}"
    );
    for cmd in emitted {
        assert_parses(&cmd);
    }
}

/// CAD-305: only the daemon's not-found answer reads as "absent";
/// every other lookup error fails closed.
#[test]
fn alias_lookup_fails_closed_on_anything_but_not_found() {
    let show = json!({"agent": {"alias": "a"}});
    assert_eq!(found_or_absent(Ok(show.clone())).unwrap(), Some(show));
    assert_eq!(
        found_or_absent(Err(Error::rejected(UNKNOWN_AGENT))).unwrap(),
        None
    );
    for err in [
        Error::rejected("Native session id matches more than one agent — use the alias"),
        Error::internal("Daemon is not reachable at /x — start it"),
        Error::unknown("connection reset"),
        Error::invalid("some_code", UNKNOWN_AGENT),
    ] {
        let text = err.to_string();
        assert!(found_or_absent(Err(err)).is_err(), "{text} read as absent");
    }
}

/// CAD-561 r4: only "not reachable" reads as no daemon — every
/// other `daemon_info` failure is real and propagates, or
/// `finish_restart` restarts a daemon that was merely slow.
#[test]
fn daemon_build_reads_only_unreachable_as_no_daemon() {
    let answered = daemon_build_or_absent(Ok(json!({"build_commit": "abc123"}))).unwrap();
    assert_eq!(answered.as_deref(), Some("abc123"));
    let absent = daemon_build_or_absent(Err(Error::internal(
        "Daemon is not reachable at /x — start it with `cadence daemon start`",
    )))
    .unwrap();
    assert_eq!(absent, None);
    for err in [
        Error::internal("Daemon returned a malformed response"),
        Error::rejected("daemon_info refused"),
        Error::unknown("connection reset"),
    ] {
        let text = err.to_string();
        assert!(
            daemon_build_or_absent(Err(err)).is_err(),
            "{text} read as no daemon"
        );
    }
}

/// CAD-305: a reopen must match provider AND endpoint kind.
#[test]
fn endpoint_mismatch_compares_provider_and_kind() {
    let agent = |p: &str, k: &str| json!({"provider": p, "endpoint_kind": k});
    assert!(refuse_endpoint_mismatch("a", &agent("devin", "pty"), "devin", "pty").is_ok());
    let err = refuse_endpoint_mismatch("a", &agent("fake", "fake"), "claude", "pty")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("'a' is already registered as a fake agent, not claude"),
        "{err}"
    );
    for (have, want) in [
        ("pty", "cloud"),
        ("cloud", "pty"),
        ("managed", "pty"),
        ("pty", "managed"),
    ] {
        let err = refuse_endpoint_mismatch("dv", &agent("devin", have), "devin", want)
            .unwrap_err()
            .to_string();
        for needle in [
            format!("'dv' is already registered as a devin {have} agent, not devin {want}"),
            "cadence agent remove dv".to_string(),
            "cadence agent resume dv".to_string(),
        ] {
            assert!(err.contains(&needle), "missing {needle:?}: {err}");
        }
    }
}

#[test]
fn cad103_review_flakes_query_and_existing_pr_syntax() {
    let cli = Cli::try_parse_from(["cadence", "review", "flakes", "--test", "a::b"]).unwrap();
    assert!(matches!(cli.command, Commands::Review {
        action: Some(ReviewAction::Flakes { test: Some(test) }), pr: None, ..
    } if test == "a::b"));
    let cli =
        Cli::try_parse_from(["cadence", "review", "42", "--repo", "O/R", "--no-full"]).unwrap();
    assert!(
        matches!(cli.command, Commands::Review { action: None, pr: Some(pr), no_full: true, .. } if pr == "42")
    );
    assert!(Cli::try_parse_from(["cadence", "review"]).is_err());
    assert!(Cli::try_parse_from(["cadence", "review", "42", "--test", "x"]).is_err());
    assert!(Cli::try_parse_from(["cadence", "review", "flakes", "--no-full"]).is_err());
}

/// CAD-800: sweep-only flags must not parse with a single ID — the
/// installed build accepted `finish <ID> --dry-run` and ran a real
/// finish. `requires = "merged"` alone does not reject when the
/// positional ID is present, so `--dry-run` and `--project` also
/// conflict with `id` and the dispatch fails closed (see
/// `src/issue/cli.rs`). `--json` is the deliberate exception: single-ID
/// output is already JSON, so it stays an accepted no-op there
/// (`finish_guard_per_worktree` depends on it) — and it can never
/// smuggle a preview past the guard because `dry_run` itself rejects.
#[test]
fn cad800_single_id_sweep_flags_rejected() {
    // Every mutating sweep-only flag, every position: reject with ID.
    for extra in [vec!["--dry-run"], vec!["--project", "demo"]] {
        let mut with_id_first = vec!["cadence", "issue", "finish", "CAD-1"];
        with_id_first.extend(extra.iter().copied());
        assert!(
            Cli::try_parse_from(with_id_first).is_err(),
            "single-ID finish must reject {extra:?}"
        );
        let mut with_id_last = vec!["cadence", "issue", "finish"];
        with_id_last.extend(extra.iter().copied());
        with_id_last.push("CAD-1");
        assert!(
            Cli::try_parse_from(with_id_last).is_err(),
            "single-ID finish must reject {extra:?} after the ID too"
        );
    }
    // `--dry-run` cannot ride along with `--json` either — the dry-run
    // flag itself conflicts, so there is no `--json` bypass.
    assert!(
        Cli::try_parse_from(["cadence", "issue", "finish", "CAD-1", "--dry-run", "--json"])
            .is_err()
    );
    // `--json` alone stays accepted with a single ID (accepted no-op).
    assert!(Cli::try_parse_from(["cadence", "issue", "finish", "CAD-1", "--json"]).is_ok());
    // A bare `--dry-run`/`--project` still needs `--merged`.
    assert!(Cli::try_parse_from(["cadence", "issue", "finish", "--dry-run"]).is_err());
    assert!(Cli::try_parse_from(["cadence", "issue", "finish", "--project", "demo"]).is_err());
    // The sweep preview itself still parses, with and without `--json`.
    assert!(Cli::try_parse_from(["cadence", "issue", "finish", "--merged", "--dry-run"]).is_ok());
    assert!(Cli::try_parse_from([
        "cadence",
        "issue",
        "finish",
        "--merged",
        "--dry-run",
        "--json"
    ])
    .is_ok());
    // A plain single-ID finish still parses.
    assert!(Cli::try_parse_from(["cadence", "issue", "finish", "CAD-1"]).is_ok());
}

/// Piped list output is already JSON, so `--fields` and `--summary` do
/// not require a redundant `--json` at parse time. Table mode still
/// refuses them in the handler.
#[test]
fn list_fields_parse_without_an_explicit_json_flag() {
    assert!(Cli::try_parse_from(["cadence", "issue", "ls", "--fields", "id"]).is_ok());
    assert!(Cli::try_parse_from(["cadence", "issue", "ls", "--summary"]).is_ok());
    assert!(Cli::try_parse_from(["cadence", "issue", "epic", "ls", "--fields", "id"]).is_ok());
    assert!(Cli::try_parse_from(["cadence", "milestone", "ls", "--fields", "id"]).is_ok());
    assert!(Cli::try_parse_from(["cadence", "memory", "ls", "--fields", "slug"]).is_ok());
}

/// CAD-879: `cadence self` asks the daemon for running turns only.
#[test]
fn cad879_self_requests_no_history() {
    assert_eq!(
        super::self_show_params("w1"),
        json!({"alias": "w1", "active_only": true})
    );
}

/// CAD-694 (review B1): the restart verdict reads the successor's
/// recovery record — cursor-independent drain evidence. Every
/// unverifiable shape must fail closed: absent, unreadable, bound to
/// another run than the one this restart started, a failed drain,
/// malformed fields, or fenced turns.
#[test]
fn recovery_record_verdict_fails_closed_on_every_bad_shape() {
    use cadence_agent::daemon::{INSTANCE_FILE, LAST_RECOVERY_FILE};
    let dir = tempfile::tempdir().unwrap();
    let verdict =
        |dir: &std::path::Path| super::daemon::recovery_record_verdict(dir, Some("run-b"));

    // No start answer at all cannot stand in for a binding.
    assert!(super::daemon::recovery_record_verdict(dir.path(), None).is_some());
    // No record at all — the predecessor's drain is unverifiable.
    assert!(verdict(dir.path()).is_some());
    // Unparseable JSON is evidence that never reached the writer.
    std::fs::write(dir.path().join(INSTANCE_FILE), "run-b").unwrap();
    std::fs::write(dir.path().join(LAST_RECOVERY_FILE), "{not json").unwrap();
    assert!(verdict(dir.path()).is_some());

    // An instance file naming a different run than the one started —
    // the stale-pair replay a double write failure leaves behind.
    std::fs::write(dir.path().join(INSTANCE_FILE), "run-a").unwrap();
    std::fs::write(
        dir.path().join(LAST_RECOVERY_FILE),
        json!({
            "instance": "run-a",
            "consumed": null,
            "fenced": [],
            "unevidenced": []
        })
        .to_string(),
    )
    .unwrap();
    assert!(verdict(dir.path()).is_some());
    // Restore the file; a record naming another run still fails — the
    // concurrent-restart overwrite this binding exists to reject.
    std::fs::write(dir.path().join(INSTANCE_FILE), "run-b").unwrap();
    assert!(verdict(dir.path()).is_some());

    // Parseable but wrongly shaped — each is unknown evidence, never clean.
    for record in [
        json!({"instance": "run-b"}), // missing members
        json!({"instance": "run-b", "consumed": null,
               "fenced": "unreadable", "unevidenced": []}), // wrong type
        json!({"instance": "run-b", "consumed": null,
               "fenced": [{"alias": "w1"}], "unevidenced": []}), // row missing message_id
        json!({"instance": "run-b", "consumed": {"instance": "run-a"},
               "fenced": [], "unevidenced": []}), // consumed missing failed
        json!({"instance": "run-b",
               "consumed": {"instance": "run-a", "stale": null, "failed": true},
               "fenced": [], "unevidenced": []}), // failed wrong type
        json!({"instance": "run-b", "consumed": "gone",
               "fenced": [], "unevidenced": []}), // consumed wrong type
    ] {
        std::fs::write(dir.path().join(LAST_RECOVERY_FILE), record.to_string()).unwrap();
        assert!(verdict(dir.path()).is_some(), "{record}");
    }

    // Bound to this run but reporting a failed drain — fails.
    std::fs::write(
        dir.path().join(LAST_RECOVERY_FILE),
        json!({
            "instance": "run-b",
            "consumed": {"instance": "run-a", "stale": null, "failed": "drain exploded"},
            "fenced": [],
            "unevidenced": [],
        })
        .to_string(),
    )
    .unwrap();
    let reason = verdict(dir.path()).unwrap();
    assert!(reason.contains("drain failed"), "{reason}");

    // Bound and clean-drained but fencing rows — fails.
    std::fs::write(
        dir.path().join(LAST_RECOVERY_FILE),
        json!({
            "instance": "run-b",
            "consumed": {"instance": "run-a", "stale": null, "failed": null},
            "fenced": [{"alias": "w1", "message_id": "m1"}],
            "unevidenced": [{"alias": "w1", "message_id": "m1"}],
        })
        .to_string(),
    )
    .unwrap();
    let reason = verdict(dir.path()).unwrap();
    assert!(reason.contains("fenced"), "{reason}");

    // Bound, clean, nothing fenced — the only passing shape.
    std::fs::write(
        dir.path().join(LAST_RECOVERY_FILE),
        json!({
            "instance": "run-b",
            "consumed": {"instance": "run-a", "stale": null, "failed": null},
            "fenced": [],
            "unevidenced": [],
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(verdict(dir.path()), None);
}

/// CAD-694: an `already_running` start answer names whichever daemon
/// held the socket, not the one this restart spawned — it must never
/// supply the instance the recovery verdict binds to.
#[test]
fn started_instance_binds_only_to_a_spawned_daemon() {
    let health = json!({"instance": "run-x", "pid": 7});
    assert_eq!(
        super::daemon::started_instance(&json!({"state": "started", "health": health})),
        Some("run-x".to_string())
    );
    assert_eq!(
        super::daemon::started_instance(&json!({"state": "already_running", "health": health})),
        None
    );
    assert_eq!(
        super::daemon::started_instance(&json!({"health": health})),
        None
    );
    assert_eq!(
        super::daemon::started_instance(&json!({"state": "started"})),
        None
    );
}

/// Every `"key":` token in `text`, tagged with the JSON nesting depth it
/// sits at (1 is the outermost object).
fn doc_keys(text: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            '"' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '"' {
                    j += 1;
                }
                let mut k = j + 1;
                while k < chars.len() && chars[k] == ' ' {
                    k += 1;
                }
                if k < chars.len() && chars[k] == ':' && depth > 0 {
                    out.push((depth, chars[start..j].iter().collect::<String>()));
                }
                i = j;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The distinct keys a document records at one nesting depth.
fn keys_at_depth(keys: &[(usize, String)], depth: usize) -> Vec<String> {
    let mut v: Vec<String> = keys
        .iter()
        .filter(|(d, _)| *d == depth)
        .map(|(_, k)| k.clone())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The sorted keys of a payload object.
fn payload_keys(value: &serde_json::Value) -> Vec<String> {
    let mut v: Vec<String> = value
        .as_object()
        .expect("a payload object")
        .keys()
        .cloned()
        .collect();
    v.sort();
    v
}

/// CAD-1514: the two `cadence self` shapes documented in
/// `skills/cadence/SKILL.md` must be the shapes the CLI builds. The inbox
/// half drifted once — it said `{"queued": N}` while the CLI emitted five
/// keys — and nothing caught it (CAD-884). Re-wording either example out
/// of recognition fails this test on purpose: the documents are the
/// contract an agent reads.
#[test]
fn documented_self_payload_keys_match_the_builders() {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/skills/cadence/SKILL.md"
    ))
    .expect("skills/cadence/SKILL.md");

    let actor_line = doc
        .lines()
        .find(|l| l.contains("cadence self") && l.contains("\"running\""))
        .expect("the actor `cadence self` example is gone from SKILL.md");
    let inbox_span = doc
        .split("answers `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .filter(|s| s.contains("oldest_unread_age_secs"))
        .expect("the inbox `cadence self` example is gone from SKILL.md");

    let show = json!({
        "agent": {"alias": "worker-a", "provider": "claude", "endpoint_kind": "pty"},
        "queued": 3,
        "inbox": {"queued": 2, "oldest_age_secs": 12.5},
        "messages": [
            {"id": "m1", "turn_id": "t1", "task_id": "job-a", "state": "running"},
            {"id": "m2", "turn_id": "t2", "task_id": null, "state": "running"},
            {"id": "m3", "turn_id": "t3", "task_id": null, "state": "done"}
        ]
    });

    let actor = super::action_dispatch::self_actor_payload("worker-a", &show);
    let actor_keys = doc_keys(actor_line);
    assert_eq!(
        keys_at_depth(&actor_keys, 1),
        payload_keys(&actor),
        "the documented actor keys and the built ones disagree"
    );
    assert_eq!(
        keys_at_depth(&actor_keys, 3),
        payload_keys(&actor["running"][0]),
        "the documented running-entry keys and the built ones disagree"
    );
    assert_eq!(
        actor["running"].as_array().map(Vec::len),
        Some(2),
        "only running turns belong in the actor payload"
    );

    let inbox = super::action_dispatch::self_inbox_payload(&show, "inbox");
    assert_eq!(
        keys_at_depth(&doc_keys(inbox_span), 1),
        payload_keys(&inbox),
        "the documented inbox keys and the built ones disagree"
    );
}

#[test]
fn duplicate_dispatch_names_the_existing_message_and_state() {
    let duplicate = json!({
        "duplicate": true,
        "message": "cadence-dispatch:t1:r2",
        "message_state": "running",
    });
    assert_eq!(
        super::job::duplicate_dispatch_line(&duplicate).as_deref(),
        Some("duplicate of cadence-dispatch:t1:r2 (state running)"),
    );
    let fresh = json!({"duplicate": false, "message": "cadence-dispatch:t1:r1"});
    assert_eq!(super::job::duplicate_dispatch_line(&fresh), None);
}

/// CAD-1894: the cloud connection is `agent cloud`, not a top-level command.
#[test]
fn cloud_connection_lives_under_agent() {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    assert!(
        cmd.find_subcommand("cloud-agent").is_none(),
        "cloud-agent is not a top-level command"
    );
    assert!(
        cmd.find_subcommand("cloud").is_none(),
        "cloud is not a top-level command"
    );
    let help = cmd
        .find_subcommand_mut("agent")
        .expect("agent")
        .find_subcommand_mut("cloud")
        .expect("agent cloud")
        .render_help()
        .to_string();
    for verb in [
        "ls",
        "show",
        "send",
        "enroll-peer",
        "read",
        "cancel",
        "disconnect",
    ] {
        assert!(
            help.contains(verb),
            "agent cloud help omits {verb}:\n{help}"
        );
    }
    assert!(
        cmd.find_subcommand_mut("agent")
            .expect("agent")
            .find_subcommand_mut("cloud")
            .expect("agent cloud")
            .find_subcommand("connect")
            .is_none(),
        "connecting is agent connect --cloud"
    );
    let connect = Cli::command()
        .find_subcommand_mut("agent")
        .expect("agent")
        .find_subcommand_mut("connect")
        .expect("agent connect")
        .render_help()
        .to_string();
    assert!(connect.contains("--cloud"), "{connect}");
    assert!(connect.contains("--local"), "{connect}");
    assert!(!connect.contains("accept-assistant-consent"), "{connect}");
}
