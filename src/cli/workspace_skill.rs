//! CAD-2396: `cadence workspace-skill list|show|check|draft|approve|revoke`.
//!
//! A workspace skill is a hosted workspace's own `SKILL.md` package (agenticos
//! AOS-626). Anyone with `cli.write`, agents included, may save a draft; only a
//! live human owner approves one exact revision by its digest, or revokes an
//! approval. The board owns the package rules and refuses with a named reason;
//! the client only reads a package directory and sends what it holds.
//!
//! This is not `cadence skill`, which installs this build's bundled public
//! guidance on this machine.

use super::*;
use std::io::Read;
use std::path::Path;

/// Transport safety bound for one package file. Not a package limit: the board
/// owns those (16 KiB a file, 32 KiB a package) and names the reason.
const READ_CAP: u64 = 1024 * 1024;

#[derive(Subcommand, Clone, Debug)]
pub(crate) enum WorkspaceSkillAction {
    /// List this workspace's skills: the latest revision of each and the
    /// revision an owner approved, if any.
    #[command(alias = "ls")]
    List,
    /// Print one revision's files (the latest unless --revision), framed as
    /// untrusted guidance, and whether it is the approved one.
    Show {
        /// The skill name.
        name: String,
        /// A saved revision number.
        #[arg(long)]
        revision: Option<u64>,
    },
    /// Validate a package directory and print the digest a draft of it would
    /// store. Saves nothing.
    Check {
        /// The skill name (lowercase letters, digits and dashes).
        name: String,
        /// Package directory: `SKILL.md` and optional `references/<file>`.
        dir: PathBuf,
    },
    /// Save a package directory as the skill's next draft revision. It is not
    /// used by any agent until a workspace owner approves it.
    Draft {
        /// The skill name (lowercase letters, digits and dashes).
        name: String,
        /// Package directory: `SKILL.md` and optional `references/<file>`.
        dir: PathBuf,
        /// The latest revision you read, or 0 to create the skill. A stale
        /// revision exits 5 and writes nothing.
        #[arg(long)]
        expect_rev: u64,
    },
    /// Approve one saved revision by the digest you read (`show`). The owner's
    /// act: a managed agent is refused before anything is sent.
    Approve {
        /// The skill name.
        name: String,
        /// The revision to approve.
        #[arg(long)]
        revision: u64,
        /// That revision's digest, exactly as `show` printed it.
        #[arg(long)]
        digest: String,
    },
    /// Withdraw the approval, fenced on the approved digest. The owner's act:
    /// a managed agent is refused before anything is sent.
    Revoke {
        /// The skill name.
        name: String,
        /// The approved digest, exactly as `list` or `show` printed it.
        #[arg(long)]
        digest: String,
    },
    /// Delete an unapproved skill and all its revisions (revoke an approved one
    /// first). The owner's act: a managed agent is refused before anything is
    /// sent. A stale revision exits 5 and deletes nothing.
    Delete {
        /// The skill name.
        name: String,
        /// The latest revision you read (`show` or `list`).
        #[arg(long)]
        expect_rev: u64,
    },
}

impl WorkspaceSkillAction {
    fn verb(&self) -> &'static str {
        match self {
            Self::List => "workspace-skill list",
            Self::Show { .. } => "workspace-skill show",
            Self::Check { .. } => "workspace-skill check",
            Self::Draft { .. } => "workspace-skill draft",
            Self::Approve { .. } => "workspace-skill approve",
            Self::Revoke { .. } => "workspace-skill revoke",
            Self::Delete { .. } => "workspace-skill delete",
        }
    }
    /// Approve, revoke and delete decide what every agent in the workspace reads.
    fn is_owner_decision(&self) -> bool {
        matches!(
            self,
            Self::Approve { .. } | Self::Revoke { .. } | Self::Delete { .. }
        )
    }
}

/// The board cannot tell a managed agent on its owner's saved login from the
/// owner, so the client refuses an owner decision first. A draft stays open to
/// agents: proposing a skill is the point; approving it is not theirs.
pub(crate) fn refuse_managed_decision(
    action: &WorkspaceSkillAction,
    pinned_identity: bool,
) -> Result<()> {
    if action.is_owner_decision()
        && (pinned_identity
            || std::env::var_os("CADENCE_ALIAS").is_some()
            || std::env::var_os("CADENCE_GRANT_TOKEN").is_some())
    {
        return Err(Error::invalid(
            "managed_caller_refused",
            format!(
                "`cadence {}` is refused for a managed agent: approving, revoking or \
                 deleting a workspace skill is a workspace owner's decision. Save a draft and \
                 ask the owner to review it. Nothing was sent.",
                action.verb()
            ),
        ));
    }
    Ok(())
}

/// A workspace-skill command that resolved to an org the command line did not
/// name (a saved default, or `CADENCE_ORG`) is refused, reads included.
pub(crate) fn require_named_org(
    org_flag: Option<&str>,
    command: &Commands,
    resolved_org: &str,
) -> Result<()> {
    if org_flag.is_none() && matches!(command, Commands::WorkspaceSkill { .. }) {
        return Err(Error::invalid(
            "org_not_named",
            format!(
                "`cadence workspace-skill` acts on one workspace and a saved default is not \
                 naming it: this would have used `{resolved_org}`. Pass `--org <name>` on this \
                 command. Nothing was sent."
            ),
        ));
    }
    Ok(())
}

/// The board's refusal names, used for what the client can see first.
fn refused(reason: &'static str, detail: &str) -> Error {
    Error::invalid(reason, format!("{detail} ({reason}); nothing was sent"))
}

fn read_file(path: &Path) -> Result<String> {
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", path.display())))?
        .take(READ_CAP + 1)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > READ_CAP {
        return Err(Error::rejected(format!(
            "{}: more than {READ_CAP} bytes; nothing was sent",
            path.display()
        )));
    }
    String::from_utf8(buf)
        .map_err(|_| Error::rejected(format!("{}: not UTF-8; nothing was sent", path.display())))
}

fn entries(dir: &Path) -> Result<Vec<(String, std::fs::FileType)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", dir.display())))?
    {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            refused(
                "member_not_accepted",
                "a file name in the package is not UTF-8",
            )
        })?;
        out.push((name, entry.file_type()?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// A package directory as the board's `files` map: `SKILL.md` and
/// `references/<file>`. Anything else — `skill.json` (the board generates it),
/// `scripts/`, a nested reference, a symlink or a stray file — is refused here
/// with the board's own reason name, so nothing partial is ever sent.
pub(crate) fn read_package(dir: &Path) -> Result<serde_json::Map<String, Value>> {
    let mut files = serde_json::Map::new();
    for (name, kind) in entries(dir)? {
        match name.as_str() {
            "skill.json" => {
                return Err(refused(
                    "member_reserved",
                    "skill.json is generated by the board; remove it from the package",
                ))
            }
            "scripts" => {
                return Err(refused(
                    "scripts_not_accepted",
                    "a workspace skill carries no scripts",
                ))
            }
            "SKILL.md" if kind.is_file() => {
                files.insert(name.clone(), json!(read_file(&dir.join(&name))?));
            }
            "references" if kind.is_dir() => {
                let sub = dir.join("references");
                for (leaf, leaf_kind) in entries(&sub)? {
                    if !leaf_kind.is_file() {
                        return Err(refused(
                            "reference_path",
                            "a reference must be one plain file directly under references/",
                        ));
                    }
                    files.insert(
                        format!("references/{leaf}"),
                        json!(read_file(&sub.join(&leaf))?),
                    );
                }
            }
            _ => {
                return Err(refused(
                    "member_not_accepted",
                    &format!("`{name}` is not SKILL.md or references/; a package holds only those"),
                ))
            }
        }
    }
    if !files.contains_key("SKILL.md") {
        return Err(refused("entry_missing", "the package has no SKILL.md"));
    }
    Ok(files)
}

type Request = (&'static str, serde_json::Map<String, Value>);

/// The relay verb and arguments for one command. `read` supplies a package
/// directory's files; tests pass their own.
pub(crate) fn request_with(
    action: &WorkspaceSkillAction,
    read: &mut dyn FnMut(&Path) -> Result<serde_json::Map<String, Value>>,
) -> Result<Request> {
    let mut args = serde_json::Map::new();
    let verb = match action {
        WorkspaceSkillAction::List => "workspace_skill_list",
        WorkspaceSkillAction::Show { name, revision } => {
            args.insert("name".into(), json!(name));
            if let Some(revision) = revision {
                args.insert("revision".into(), json!(revision));
            }
            "workspace_skill_show"
        }
        WorkspaceSkillAction::Check { name, dir } => {
            args.insert("name".into(), json!(name));
            args.insert("files".into(), Value::Object(read(dir)?));
            "workspace_skill_check"
        }
        WorkspaceSkillAction::Draft {
            name,
            dir,
            expect_rev,
        } => {
            args.insert("name".into(), json!(name));
            args.insert("files".into(), Value::Object(read(dir)?));
            args.insert("expectedRevision".into(), json!(expect_rev));
            "workspace_skill_draft"
        }
        WorkspaceSkillAction::Approve {
            name,
            revision,
            digest,
        } => {
            args.insert("name".into(), json!(name));
            args.insert("revision".into(), json!(revision));
            args.insert("digest".into(), json!(digest));
            "workspace_skill_approve"
        }
        WorkspaceSkillAction::Revoke { name, digest } => {
            args.insert("name".into(), json!(name));
            args.insert("digest".into(), json!(digest));
            "workspace_skill_revoke"
        }
        WorkspaceSkillAction::Delete { name, expect_rev } => {
            args.insert("name".into(), json!(name));
            args.insert("expectedRevision".into(), json!(expect_rev));
            "workspace_skill_delete"
        }
    };
    Ok((verb, args))
}

/// The same, reading real directories.
pub(crate) fn request(action: &WorkspaceSkillAction) -> Result<Request> {
    request_with(action, &mut |dir| read_package(dir))
}

/// Native board: workspace skills live in a hosted workspace only.
pub(crate) fn run_native(action: &WorkspaceSkillAction) -> Result<i32> {
    Err(Error::rejected(format!(
        "`cadence {}` needs a hosted workspace: a native board has no workspace skill store. \
         Pass `--org <hosted workspace>`. Nothing was sent.",
        action.verb()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(layout: &[(&str, Option<&str>)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in layout {
            let full = dir.path().join(path);
            match body {
                Some(text) => {
                    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                    std::fs::write(full, text).unwrap();
                }
                None => std::fs::create_dir_all(full).unwrap(),
            }
        }
        dir
    }

    #[test]
    fn reads_skill_md_and_flat_references_only() {
        let dir = package(&[
            ("SKILL.md", Some("---\ndescription: d\n---\nBody")),
            ("references/format.md", Some("# Format")),
            ("references/b.md", Some("b")),
        ]);
        let files = read_package(dir.path()).unwrap();
        let keys: Vec<_> = files.keys().cloned().collect();
        assert_eq!(
            keys,
            ["SKILL.md", "references/b.md", "references/format.md"]
        );
        assert_eq!(files["references/format.md"], json!("# Format"));
    }

    #[test]
    fn refuses_what_the_board_would_refuse_before_sending() {
        for (layout, reason) in [
            (
                vec![("SKILL.md", Some("x")), ("skill.json", Some("{}"))],
                "member_reserved",
            ),
            (
                vec![("SKILL.md", Some("x")), ("scripts/run.sh", Some("echo"))],
                "scripts_not_accepted",
            ),
            (
                vec![("SKILL.md", Some("x")), ("notes.md", Some("x"))],
                "member_not_accepted",
            ),
            (
                vec![("SKILL.md", Some("x")), ("references/sub/x.md", Some("x"))],
                "reference_path",
            ),
            (vec![("references/x.md", Some("x"))], "entry_missing"),
            (vec![("SKILL.md", None)], "member_not_accepted"),
        ] {
            let dir = package(&layout);
            let err = read_package(dir.path()).unwrap_err();
            assert!(err.to_string().contains(reason), "{layout:?}: {err}");
        }
    }

    #[test]
    fn maps_each_command_to_its_board_verb_and_arguments() {
        let mut read = |_: &Path| {
            let mut files = serde_json::Map::new();
            files.insert("SKILL.md".into(), json!("x"));
            Ok(files)
        };
        let cases = [
            (
                WorkspaceSkillAction::List,
                "workspace_skill_list",
                json!({}),
            ),
            (
                WorkspaceSkillAction::Show {
                    name: "weekly".into(),
                    revision: Some(2),
                },
                "workspace_skill_show",
                json!({"name": "weekly", "revision": 2}),
            ),
            (
                WorkspaceSkillAction::Check {
                    name: "weekly".into(),
                    dir: "pkg".into(),
                },
                "workspace_skill_check",
                json!({"name": "weekly", "files": {"SKILL.md": "x"}}),
            ),
            (
                WorkspaceSkillAction::Draft {
                    name: "weekly".into(),
                    dir: "pkg".into(),
                    expect_rev: 0,
                },
                "workspace_skill_draft",
                json!({"name": "weekly", "files": {"SKILL.md": "x"}, "expectedRevision": 0}),
            ),
            (
                WorkspaceSkillAction::Approve {
                    name: "weekly".into(),
                    revision: 1,
                    digest: "sha256:ab".into(),
                },
                "workspace_skill_approve",
                json!({"name": "weekly", "revision": 1, "digest": "sha256:ab"}),
            ),
            (
                WorkspaceSkillAction::Revoke {
                    name: "weekly".into(),
                    digest: "sha256:ab".into(),
                },
                "workspace_skill_revoke",
                json!({"name": "weekly", "digest": "sha256:ab"}),
            ),
            (
                WorkspaceSkillAction::Delete {
                    name: "weekly".into(),
                    expect_rev: 3,
                },
                "workspace_skill_delete",
                json!({"name": "weekly", "expectedRevision": 3}),
            ),
        ];
        for (action, verb, args) in cases {
            let (got_verb, got_args) = request_with(&action, &mut read).unwrap();
            assert_eq!(got_verb, verb);
            assert_eq!(Value::Object(got_args), args, "{verb}");
            assert!(
                cadence_agent::remote_cli::REMOTE_VERBS.contains(&verb),
                "{verb}"
            );
        }
    }

    #[test]
    fn a_managed_caller_cannot_approve_or_revoke_but_can_draft() {
        let approve = WorkspaceSkillAction::Approve {
            name: "weekly".into(),
            revision: 1,
            digest: "sha256:ab".into(),
        };
        let revoke = WorkspaceSkillAction::Revoke {
            name: "weekly".into(),
            digest: "sha256:ab".into(),
        };
        let draft = WorkspaceSkillAction::Draft {
            name: "weekly".into(),
            dir: "pkg".into(),
            expect_rev: 0,
        };
        let delete = WorkspaceSkillAction::Delete {
            name: "weekly".into(),
            expect_rev: 1,
        };
        // A pinned identity is a managed caller whatever the environment says.
        for action in [&approve, &revoke, &delete] {
            let err = refuse_managed_decision(action, true).unwrap_err();
            assert_eq!(err.kind(), "rejected", "{err}");
            assert!(err.to_string().contains("managed agent"), "{err}");
        }
        assert!(refuse_managed_decision(&draft, true).is_ok());
        assert!(refuse_managed_decision(&WorkspaceSkillAction::List, true).is_ok());
    }

    #[test]
    fn cadence_alias_alone_makes_a_managed_caller() {
        let _lock = crate::cli::org::tests::RESOLVE_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let approve = WorkspaceSkillAction::Approve {
            name: "weekly".into(),
            revision: 1,
            digest: "sha256:ab".into(),
        };
        assert!(refuse_managed_decision(&approve, false).is_ok());
        std::env::set_var("CADENCE_ALIAS", "synthetic-worker");
        let refused = refuse_managed_decision(&approve, false);
        std::env::remove_var("CADENCE_ALIAS");
        assert!(refused.unwrap_err().to_string().contains("managed agent"));
    }
}
