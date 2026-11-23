//! CAD-2101: `cadence agent profile get|create|edit|archive`.
//!
//! The CLI sends what was asked and prints what the board answered. It holds
//! no field list and no limits: a field name is the board's, a value is
//! forwarded as given, and the board's refusal keeps its code. Hosted org:
//! `agent_profile_show|create|update` through the relay. Native board: only
//! `create` has a daemon path (`agent_profile_create`, the route behind
//! `POST /api/agent-profiles`); the rest refuse and name what is missing.

use super::*;
use std::io::Read;

/// Transport safety bound for one `--file` value read from disk or stdin.
/// It is not a field limit: the board owns those and refuses what is too long.
const READ_CAP: u64 = 1024 * 1024;

#[derive(Subcommand, Clone, Debug)]
pub(crate) enum ProfileAction {
    /// Print one stored agent profile, including its `revision`. An archived
    /// profile is readable by the workspace owner only.
    Get {
        /// The profile id the board prints (`ap_…` hosted).
        id: String,
    },
    /// Save a new profile from a catalogue role (the owner's act; an agent
    /// is refused). Registers and launches nothing.
    Create {
        /// The profile's alias (lowercase letters, digits and dashes).
        alias: String,
        /// A catalogue role id (`cadence agent roles`).
        #[arg(long)]
        catalogue: String,
        /// File with a job tailored to the ask (`-` for stdin); omitted, the
        /// role's own job is used.
        #[arg(long, value_name = "PATH|-")]
        job_file: Option<String>,
        /// A registered model; omitted, the profile inherits.
        #[arg(long)]
        model: Option<String>,
    },
    /// Change named fields of a profile and leave the rest as stored. Needs
    /// the revision you read: `--expect-rev N`, or `--latest` to read it
    /// first (said on stderr). A stale revision exits 5 and writes nothing.
    /// Field names are the board's (`name`, `role`, `responsibilities`,
    /// `skills`, `model`, `status`); the board refuses an unknown one.
    Edit {
        /// The profile id.
        id: String,
        /// `FIELD=VALUE`, the value taken as text. Repeatable.
        #[arg(long = "set", value_name = "FIELD=VALUE")]
        set: Vec<String>,
        /// `FIELD=JSON`, for a value that is not text, such as
        /// `skills=["a","b"]`. Repeatable.
        #[arg(long = "set-json", value_name = "FIELD=JSON")]
        set_json: Vec<String>,
        /// `FIELD=PATH`, the file's text as the value (`FIELD=-` for
        /// stdin), sent byte for byte. Repeatable.
        #[arg(long, value_name = "FIELD=PATH|-")]
        file: Vec<String>,
        /// The profile revision this edit is based on.
        #[arg(long, conflicts_with = "latest", required_unless_present = "latest")]
        expect_rev: Option<u64>,
        /// Read the current revision first and write against it. Closes the
        /// fence to "whatever is stored now"; it says so on stderr.
        #[arg(long)]
        latest: bool,
    },
    /// Archive a profile: an edit of `status`. Same revision fence as `edit`.
    Archive {
        /// The profile id.
        id: String,
        /// The profile revision this archive is based on.
        #[arg(long, conflicts_with = "latest", required_unless_present = "latest")]
        expect_rev: Option<u64>,
        /// Read the current revision first and write against it.
        #[arg(long)]
        latest: bool,
    },
}

impl ProfileAction {
    pub(crate) fn is_write(&self) -> bool {
        !matches!(self, Self::Get { .. })
    }
    fn verb(&self) -> &'static str {
        match self {
            Self::Get { .. } => "agent profile get",
            Self::Create { .. } => "agent profile create",
            Self::Edit { .. } => "agent profile edit",
            Self::Archive { .. } => "agent profile archive",
        }
    }
}

/// A write is a live human owner's act. The hosted board refuses a service or
/// agent principal itself, but it cannot see `CADENCE_ALIAS`, so the client
/// refuses first. The refusal is the client's own and stands in no one's place.
pub(crate) fn refuse_managed_write(action: &ProfileAction, pinned_identity: bool) -> Result<()> {
    if action.is_write()
        && (pinned_identity
            || std::env::var_os("CADENCE_ALIAS").is_some()
            || std::env::var_os("CADENCE_GRANT_TOKEN").is_some())
    {
        return Err(Error::invalid(
            "managed_caller_refused",
            format!(
                "`cadence {}` is refused for a managed agent: a managed agent can't edit \
                 profiles; propose the change (`cadence agent propose-hire`, or ask the \
                 owner). Nothing was sent.",
                action.verb()
            ),
        ));
    }
    Ok(())
}

/// A profile command that resolved to a hosted org the command line did not
/// name (a saved default, or `CADENCE_ORG`) is refused, reads included.
pub(crate) fn require_named_org(
    org_flag: Option<&str>,
    command: &Commands,
    resolved_org: &str,
) -> Result<()> {
    if org_flag.is_none()
        && matches!(
            command,
            Commands::Agent {
                action: AgentAction::Profile { .. }
            }
        )
    {
        return Err(Error::invalid(
            "org_not_named",
            format!(
                "`cadence agent profile` acts on one workspace and a saved default is not \
                 naming it: this would have used `{resolved_org}`. Pass `--org <name>` on this \
                 command. Nothing was sent."
            ),
        ));
    }
    Ok(())
}

/// One text value from a file or stdin, unchanged. Larger than the transport
/// bound or not UTF-8 is refused rather than truncated or repaired.
fn read_text(source: &str, flag: &str) -> Result<String> {
    let mut buf = Vec::new();
    if source == "-" {
        if atty_stdin() {
            return Err(Error::usage(format!(
                "{flag} -: stdin is a terminal; pipe the text in"
            )));
        }
        std::io::stdin().take(READ_CAP + 1).read_to_end(&mut buf)?;
    } else {
        std::fs::File::open(source)
            .map_err(|e| Error::rejected(format!("{flag}: cannot read {source}: {e}")))?
            .take(READ_CAP + 1)
            .read_to_end(&mut buf)?;
    }
    if buf.len() as u64 > READ_CAP {
        return Err(Error::rejected(format!(
            "{flag}: more than {READ_CAP} bytes; nothing was sent"
        )));
    }
    String::from_utf8(buf).map_err(|_| Error::rejected(format!("{flag}: not UTF-8")))
}

fn split_pair<'a>(pair: &'a str, flag: &str) -> Result<(&'a str, &'a str)> {
    let (field, value) = pair
        .split_once('=')
        .filter(|(field, _)| !field.is_empty())
        .ok_or_else(|| Error::usage(format!("{flag} takes FIELD=VALUE, got `{pair}`")))?;
    // The id and the revision fence are carried by the command's own
    // arguments; a field of the same name would shadow them.
    if matches!(field, "id" | "expectedRevision") {
        return Err(Error::usage(format!(
            "{flag}: `{field}` is set by the command, not as a field"
        )));
    }
    Ok((field, value))
}

type Request = (&'static str, serde_json::Map<String, Value>);

/// The relay verb and arguments for one profile command. `revision` is the
/// fence for `edit`/`archive`: `--expect-rev`, or what `--latest` read.
/// `read` supplies a file's or stdin's text; tests pass their own.
pub(crate) fn request_with(
    action: &ProfileAction,
    read: &mut dyn FnMut(&str, &str) -> Result<String>,
) -> Result<Request> {
    let mut args = serde_json::Map::new();
    let verb = match action {
        ProfileAction::Get { id } => {
            args.insert("id".into(), json!(id));
            "agent_profile_show"
        }
        ProfileAction::Create {
            alias,
            catalogue,
            job_file,
            model,
        } => {
            args.insert("alias".into(), json!(alias));
            args.insert("catalogue".into(), json!(catalogue));
            if let Some(path) = job_file {
                args.insert("job".into(), json!(read(path, "--job-file")?));
            }
            if let Some(model) = model {
                args.insert("model".into(), json!(model));
            }
            "agent_profile_create"
        }
        ProfileAction::Edit {
            id,
            set,
            set_json,
            file,
            expect_rev,
            latest: _,
        } => {
            let mut fields = serde_json::Map::new();
            let mut put = |field: &str, value: Value, flag: &str| -> Result<()> {
                if fields.insert(field.to_string(), value).is_some() {
                    return Err(Error::usage(format!(
                        "{flag}: field `{field}` is given more than once"
                    )));
                }
                Ok(())
            };
            for pair in set {
                let (field, value) = split_pair(pair, "--set")?;
                put(field, json!(value), "--set")?;
            }
            for pair in set_json {
                let (field, value) = split_pair(pair, "--set-json")?;
                let value = serde_json::from_str(value).map_err(|e| {
                    Error::usage(format!("--set-json: `{field}` is not valid JSON: {e}"))
                })?;
                put(field, value, "--set-json")?;
            }
            let stdin_uses = file
                .iter()
                .filter(|pair| pair.split_once('=').is_some_and(|(_, path)| path == "-"))
                .count();
            if stdin_uses > 1 {
                return Err(Error::usage(
                    "--file: stdin (`-`) can be read once; give the second field a path",
                ));
            }
            for pair in file {
                let (field, path) = split_pair(pair, "--file")?;
                put(field, json!(read(path, "--file")?), "--file")?;
            }
            if fields.is_empty() {
                return Err(Error::usage(
                    "edit names no field: give --set, --set-json or --file",
                ));
            }
            args.insert("id".into(), json!(id));
            args.insert("expectedRevision".into(), fence(*expect_rev)?);
            args.extend(fields);
            "agent_profile_update"
        }
        ProfileAction::Archive {
            id,
            expect_rev,
            latest: _,
        } => {
            args.insert("id".into(), json!(id));
            args.insert("expectedRevision".into(), fence(*expect_rev)?);
            args.insert("status".into(), json!("archived"));
            "agent_profile_update"
        }
    };
    Ok((verb, args))
}

/// The same, reading real files and stdin.
pub(crate) fn request(action: &ProfileAction) -> Result<Request> {
    request_with(action, &mut |source, flag| read_text(source, flag))
}

fn fence(revision: Option<u64>) -> Result<Value> {
    revision
        .map(|r| json!(r))
        .ok_or_else(|| Error::usage("a write needs --expect-rev <N> or --latest"))
}

/// The revision a `--latest` write fences on, from the board's `{profile}`.
fn latest_revision(body: &Value) -> Result<u64> {
    body.pointer("/profile/revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            Error::rejected(
                "the board's profile read carried no revision, so --latest has nothing to \
                 fence on; nothing was written",
            )
        })
}

/// `--latest`: read the revision now and turn it into an explicit
/// `--expect-rev`, so one mapping builds the write. Says so on stderr.
pub(crate) fn resolve_latest(
    target: &cadence_agent::remote_cli::RemoteTarget,
    action: &mut ProfileAction,
    wake_timeout: u64,
) -> Result<()> {
    let (id, latest, expect_rev) = match action {
        ProfileAction::Edit {
            id,
            latest,
            expect_rev,
            ..
        }
        | ProfileAction::Archive {
            id,
            latest,
            expect_rev,
        } => (id.clone(), latest, expect_rev),
        _ => return Ok(()),
    };
    if !*latest {
        return Ok(());
    }
    let mut args = serde_json::Map::new();
    args.insert("id".into(), json!(id));
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid wake timeout"))?;
    let body = match cadence_agent::remote_cli::call_verb(
        target,
        "agent_profile_show",
        args,
        deadline,
        wake_timeout,
        |line| eprintln!("{line}"),
    )? {
        cadence_agent::remote_cli::RemoteAnswer::Ok(body) => body,
        cadence_agent::remote_cli::RemoteAnswer::Conflict(_) => {
            return Err(Error::rejected(
                "the board answered a conflict to a read; nothing was written",
            ))
        }
    };
    let revision = latest_revision(&body)?;
    eprintln!(
        "--latest: read revision {revision} of {id}; writing against it, so a change made \
         between this read and the write is still caught"
    );
    *expect_rev = Some(revision);
    *latest = false;
    Ok(())
}

/// Native board. `create` rides the daemon's `agent_profile_create` (the
/// route behind `POST /api/agent-profiles`); everything else has no daemon
/// path and says what is missing instead of touching another store.
pub(crate) fn run_native(state_dir: PathBuf, action: &ProfileAction) -> Result<i32> {
    refuse_managed_write(action, false)?;
    match action {
        ProfileAction::Get { .. } => Err(Error::rejected(
            "a native board has no route that returns one stored profile with its revision; \
             `cadence agent profiles` lists them and `cadence agent context <alias>` prints \
             one's context. Nothing was read.",
        )),
        ProfileAction::Edit { .. } | ProfileAction::Archive { .. } => Err(Error::rejected(
            "a native board has no route that edits or archives a profile, and no revision \
             to fence it on (`POST /api/agents/<alias>/instructions` changes a runtime's \
             standing instructions, not a profile). Edit it on a hosted org. Nothing was \
             changed.",
        )),
        ProfileAction::Create { .. } => {
            let (_, params) = request(action)?;
            let profile = client::rpc(&state_dir, "agent_profile_create", Value::Object(params))?;
            print_json(&json!({"profile": profile}));
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(text: &'static str) -> impl FnMut(&str, &str) -> Result<String> {
        move |_, _| Ok(text.to_string())
    }

    fn edit(set: &[&str], set_json: &[&str], file: &[&str]) -> ProfileAction {
        ProfileAction::Edit {
            id: "ap_synthetic-0001".into(),
            set: set.iter().map(|s| s.to_string()).collect(),
            set_json: set_json.iter().map(|s| s.to_string()).collect(),
            file: file.iter().map(|s| s.to_string()).collect(),
            expect_rev: Some(3),
            latest: false,
        }
    }

    #[test]
    fn long_text_goes_through_unchanged() {
        // 8,000 and 8,001 wide characters are the same to the client: it holds
        // no limit, so both cross the wire and the board decides.
        for n in [8000, 8001] {
            let text: String = "漢".repeat(n);
            let leaked: &'static str = Box::leak(text.clone().into_boxed_str());
            let (verb, args) = request_with(
                &edit(&[], &[], &["responsibilities=-"]),
                &mut reader(leaked),
            )
            .unwrap();
            assert_eq!(verb, "agent_profile_update");
            assert_eq!(args["responsibilities"].as_str().unwrap(), text);
        }
    }

    #[test]
    fn set_text_and_json_values_are_not_guessed() {
        let (_, args) = request_with(
            &edit(&["role=true", "name=7"], &["skills=[\"a\",\"b\"]"], &[]),
            &mut reader(""),
        )
        .unwrap();
        assert_eq!(args["role"], json!("true"));
        assert_eq!(args["name"], json!("7"));
        assert_eq!(args["skills"], json!(["a", "b"]));
        assert_eq!(args["expectedRevision"], json!(3));
        assert_eq!(args["id"], json!("ap_synthetic-0001"));
    }

    #[test]
    fn only_named_fields_are_sent() {
        let (_, args) = request_with(&edit(&["role=Planner"], &[], &[]), &mut reader("")).unwrap();
        let mut keys: Vec<_> = args.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["expectedRevision", "id", "role"]);
    }

    #[test]
    fn bad_edits_refuse_before_anything_is_sent() {
        for (set, json_, file, why) in [
            (vec![], vec![], vec![], "names no field"),
            (vec!["role"], vec![], vec![], "FIELD=VALUE"),
            (vec!["=x"], vec![], vec![], "FIELD=VALUE"),
            (vec!["id=ap_other"], vec![], vec![], "set by the command"),
            (
                vec!["expectedRevision=1"],
                vec![],
                vec![],
                "set by the command",
            ),
            (vec!["role=a", "role=b"], vec![], vec![], "more than once"),
            (vec!["role=a"], vec!["role=\"b\""], vec![], "more than once"),
            (vec![], vec!["skills=[oops"], vec![], "not valid JSON"),
            (vec![], vec![], vec!["a=-", "b=-"], "read once"),
        ] {
            let err = request_with(&edit(&set, &json_, &file), &mut reader("")).unwrap_err();
            assert_eq!(err.kind(), "usage", "{set:?} {json_:?} {file:?}: {err}");
            assert!(err.to_string().contains(why), "{set:?}: {err}");
        }
    }

    #[test]
    fn latest_fences_on_the_revision_the_board_returned() {
        assert_eq!(
            latest_revision(&json!({"profile": {"id": "ap_x", "revision": 7}})).unwrap(),
            7
        );
        for bad in [
            json!({}),
            json!({"profile": {}}),
            json!({"profile": {"revision": "7"}}),
        ] {
            assert!(latest_revision(&bad).is_err());
        }
    }

    #[test]
    fn archive_is_an_edit_of_status_and_needs_a_fence() {
        let (verb, args) = request_with(
            &ProfileAction::Archive {
                id: "ap_synthetic-0001".into(),
                expect_rev: Some(9),
                latest: false,
            },
            &mut reader(""),
        )
        .unwrap();
        assert_eq!(verb, "agent_profile_update");
        assert_eq!(
            Value::Object(args),
            json!({"id": "ap_synthetic-0001", "expectedRevision": 9, "status": "archived"})
        );
        let err = request_with(
            &ProfileAction::Archive {
                id: "ap_synthetic-0001".into(),
                expect_rev: None,
                latest: false,
            },
            &mut reader(""),
        )
        .unwrap_err();
        assert!(err.to_string().contains("--expect-rev"), "{err}");
    }

    #[test]
    fn get_and_create_map_to_their_verbs() {
        let (verb, args) = request_with(
            &ProfileAction::Get {
                id: "ap_synthetic-0001".into(),
            },
            &mut reader(""),
        )
        .unwrap();
        assert_eq!(
            (verb, Value::Object(args)),
            ("agent_profile_show", json!({"id": "ap_synthetic-0001"}))
        );
        let (verb, args) = request_with(
            &ProfileAction::Create {
                alias: "ledger".into(),
                catalogue: "accountant".into(),
                job_file: Some("-".into()),
                model: None,
            },
            &mut reader("Keep the ledger."),
        )
        .unwrap();
        assert_eq!(verb, "agent_profile_create");
        assert_eq!(
            Value::Object(args),
            json!({"alias": "ledger", "catalogue": "accountant", "job": "Keep the ledger."})
        );
    }

    fn managed_env<T>(f: impl FnOnce() -> T) -> T {
        let _lock = crate::cli::org::tests::RESOLVE_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CADENCE_ALIAS", "synthetic-worker");
        let out = f();
        std::env::remove_var("CADENCE_ALIAS");
        out
    }

    #[test]
    fn a_managed_agent_is_refused_a_write_but_may_read() {
        let write = edit(&["role=x"], &[], &[]);
        let read = ProfileAction::Get { id: "ap_x".into() };
        let create = ProfileAction::Create {
            alias: "a".into(),
            catalogue: "c".into(),
            job_file: None,
            model: None,
        };
        let (refused, allowed, created, pinned) = managed_env(|| {
            (
                refuse_managed_write(&write, false),
                refuse_managed_write(&read, false),
                refuse_managed_write(&create, false),
                refuse_managed_write(&write, true),
            )
        });
        let err = refused.unwrap_err();
        assert_eq!(err.code(), Some("managed_caller_refused"));
        assert!(err
            .to_string()
            .contains("can't edit profiles; propose the change"));
        assert!(allowed.is_ok());
        assert!(created.is_err());
        assert!(pinned.is_err());
    }

    /// Through the real hosted call site: removing the guard from
    /// `remote::run` makes this fail, because the unreachable endpoint and
    /// missing login would then give a different error.
    #[test]
    fn hosted_run_refuses_a_managed_write_before_any_request() {
        let target = || cadence_agent::remote_cli::RemoteTarget {
            org: "acme".into(),
            endpoint: "https://127.0.0.1:1".into(),
            org_id: "ws_synthetic".into(),
            identity: None,
        };
        let cmd = |action| Commands::Agent {
            action: AgentAction::Profile { action },
        };
        let out = managed_env(|| {
            [
                super::remote::run(
                    target(),
                    cmd(ProfileAction::Archive {
                        id: "ap_x".into(),
                        expect_rev: None,
                        latest: true,
                    }),
                    1,
                    None,
                    None,
                ),
                super::remote::run(target(), cmd(edit(&["role=x"], &[], &[])), 1, None, None),
            ]
        });
        for result in out {
            let err = result.unwrap_err();
            assert_eq!(err.code(), Some("managed_caller_refused"), "{err}");
        }
    }

    #[test]
    fn a_saved_default_org_is_not_naming_the_org() {
        use crate::cli::org::tests::{remote_registry, EnvGuard, EP_A, RESOLVE_ENV};
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("acme", EP_A, "ws_alpha", true));
        // The saved remote default is selected, with no --org: refused by the
        // dispatcher before any credential or request, reads and writes alike.
        for args in [
            vec!["cadence", "agent", "profile", "get", "ap_x"],
            vec![
                "cadence",
                "agent",
                "profile",
                "archive",
                "ap_x",
                "--expect-rev",
                "1",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            let err = super::action_dispatch::run_cli(cli).unwrap_err();
            assert_eq!(err.code(), Some("org_not_named"), "{err}");
            assert!(err.to_string().contains("--org"), "{err}");
        }
        // CADENCE_ORG selects the same org and is not naming it either.
        std::env::set_var("CADENCE_ORG", "acme");
        let cli = Cli::try_parse_from(["cadence", "agent", "profile", "get", "ap_x"]).unwrap();
        let err = super::action_dispatch::run_cli(cli).unwrap_err();
        assert_eq!(err.code(), Some("org_not_named"), "{err}");
        std::env::remove_var("CADENCE_ORG");
    }

    #[test]
    fn a_client_only_install_refuses_an_unnamed_org_too() {
        use crate::cli::org::tests::{remote_registry, EnvGuard, EP_A, RESOLVE_ENV};
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("acme", EP_A, "ws_alpha", true));
        let go = || {
            let cli = Cli::try_parse_from(["cadence", "agent", "profile", "get", "ap_x"]).unwrap();
            super::client_update::route_client_only(cli)
        };
        let err = go().unwrap_err();
        assert_eq!(err.code(), Some("org_not_named"), "{err}");
        std::env::set_var("CADENCE_ORG", "acme");
        let err = go().unwrap_err();
        std::env::remove_var("CADENCE_ORG");
        assert_eq!(err.code(), Some("org_not_named"), "{err}");
    }

    #[test]
    fn a_native_target_without_org_still_reaches_the_command() {
        use crate::cli::org::tests::{EnvGuard, RESOLVE_ENV};
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::bare();
        let cli = Cli::try_parse_from(["cadence", "agent", "profile", "get", "ap_x"]).unwrap();
        let err = super::action_dispatch::run_cli(cli).unwrap_err();
        // The native refusal, not the org one.
        assert_ne!(err.code(), Some("org_not_named"), "{err}");
        assert!(err.to_string().contains("native board"), "{err}");
    }
}
