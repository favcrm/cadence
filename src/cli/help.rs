// CAD-888: root help scoped to the caller.
//
// `cadence --help` lists the core verbs in groups; the rest sit behind
// `cadence help operator` and `cadence help all`. This is presentation
// only: every verb still parses and runs, and no gate looks at what is
// listed here. clap 4 has no headings for subcommands, so the root
// command carries a rendered `before_help` and a template that leaves
// out clap's flat `{subcommands}` list; every per-verb help is clap's
// own.
use super::Cli;
use clap::{Command, CommandFactory};

/// The default root help: group title and its core verbs.
const CORE: &[(&str, &[(&str, &str)])] = &[
    (
        "Me",
        &[
            ("self", "Your alias, running message and report token"),
            ("done", "Report your running turn's result"),
            ("inbox", "Read an inbox agent's durable queue"),
        ],
    ),
    (
        "Work",
        &[
            ("issue", "The issue board: the one public unit of work"),
            ("plan", "Propose, decide and track an epic with its tickets"),
        ],
    ),
    (
        "Agents",
        &[
            ("send", "Enqueue a durable message to an agent"),
            ("dispatch", "Start an issue and send its worker the kickoff"),
            ("join", "Join a new worker agent to a group"),
            ("agent", "List and manage registered agents"),
        ],
    ),
    (
        "Build",
        &[
            ("build-slot", "Bounded cargo build and test scheduling"),
            ("secret", "Credential scan before anything is written"),
        ],
    ),
    (
        "Fleet",
        &[
            ("status", "One-screen fleet overview"),
            ("doctor", "Check environment, storage and provider CLIs"),
        ],
    ),
    (
        "Knowledge",
        &[
            ("memory", "Reviewed, scoped project facts"),
            ("wiki", "Shared, scoped knowledge pages"),
        ],
    ),
];

/// The operator's verbs, listed by `cadence help operator`. Every other
/// non-core verb is "advanced" and listed after them.
const OPERATOR: &[&str] = &[
    "daemon",
    "update",
    "backup",
    "restore",
    "export",
    "ui",
    "master",
    "connection",
    "platform",
    "app",
    "audit",
    "review",
    "sandbox",
    "session",
    "staging",
];

/// Visible verbs that are neither core nor operator.
const ADVANCED: &[&str] = &[
    "agent-uid",
    "attach",
    "auth",
    "claude",
    "codex",
    "cursor",
    "delivery",
    "devin",
    "events",
    "idea",
    "intake",
    "interrupt",
    "job",
    "login",
    "message",
    "milestone",
    "monitor",
    "overview",
    "project",
    "remote",
    "report",
    "resume",
    "rollout",
    "setup",
    "skill",
    "stop",
    "test",
    "thread",
    "upgrade",
    "workflow",
];

/// Verbs clap hides on purpose: plumbing no help view lists.
#[cfg(test)]
const INTERNAL: &[&str] = &["confine", "mcp-agent", "mcp-permission"];

const MORE_OUTSIDE_PANE: &str = "More: cadence help operator | cadence help all";
/// Inside a pane the operator list is left out of the pointers.
const MORE_IN_PANE: &str = "More: cadence help all";

fn is_core(name: &str) -> bool {
    CORE.iter().any(|(_, v)| v.iter().any(|(n, _)| *n == name))
}

/// True inside a cadence-owned pane. Presentation only.
pub(super) fn in_pane() -> bool {
    std::env::var_os("CADENCE_ALIAS").is_some_and(|v| !v.is_empty())
}

/// The default `cadence --help` verb listing.
pub(super) fn root_help_text() -> String {
    let mut out = String::new();
    for (title, verbs) in CORE {
        out.push_str(&format!("{title}:\n"));
        for (name, blurb) in *verbs {
            out.push_str(&format!("  {name:<14} {blurb}\n"));
        }
        out.push('\n');
    }
    out.push_str("Details for one verb: cadence help <verb>");
    out
}

/// The last line of the root help.
fn more_line(in_pane: bool) -> &'static str {
    if in_pane {
        MORE_IN_PANE
    } else {
        MORE_OUTSIDE_PANE
    }
}

/// The clap command for parsing and root help. Non-core verbs are
/// hidden from clap's own list (they still parse); the grouped text
/// replaces it.
pub(super) fn root_command(in_pane: bool) -> Command {
    let mut cmd = Cli::command();
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    for name in names.into_iter().filter(|n| !is_core(n)) {
        cmd = cmd.mut_subcommand(name, |c| c.hide(true));
    }
    cmd.before_help(root_help_text())
        .after_help(more_line(in_pane))
        .help_template(
            "{about}\n\n{usage-heading} {usage}\n\n{before-help}Options:\n{options}{after-help}",
        )
}

/// A one-line summary: the first sentence of the verb's description.
fn summary(cmd: &Command) -> String {
    let about = cmd.get_about().map(|a| a.to_string()).unwrap_or_default();
    let flat = about.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = match flat.find(". ") {
        Some(i) => &flat[..i],
        None => flat.trim_end_matches('.'),
    };
    if first.chars().count() <= 90 {
        return first.to_string();
    }
    let cut: String = first.chars().take(87).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(h, _)| h);
    format!("{cut}...")
}

fn verb_line(out: &mut String, cmd: &Command) {
    out.push_str(&format!("  {:<14} {}\n", cmd.get_name(), summary(cmd)));
}

/// Every non-hidden descendant path of `cmd`, one per line. Hiding is
/// judged on the plain command tree, so deliberately hidden plumbing
/// never shows.
fn tree_lines(out: &mut String, cmd: &Command, path: &str) {
    for sub in cmd.get_subcommands().filter(|c| !c.is_hide_set()) {
        let here = format!("{path} {}", sub.get_name());
        out.push_str(&format!("    {here}\n"));
        tree_lines(out, sub, &here);
    }
}

/// `cadence help operator`: the operator's verbs, then the advanced
/// ones.
pub(super) fn operator_help_text() -> String {
    let cmd = Cli::command();
    let mut out = String::from("Operator commands:\n");
    for name in OPERATOR {
        if let Some(c) = cmd.find_subcommand(name) {
            verb_line(&mut out, c);
        }
    }
    out.push_str("\nAdvanced commands:\n");
    let mut adv: Vec<&str> = ADVANCED.to_vec();
    adv.sort_unstable();
    for name in adv {
        if let Some(c) = cmd.find_subcommand(name) {
            verb_line(&mut out, c);
        }
    }
    out.push_str("\nDetails for one verb: cadence help <verb>\n");
    out.push_str("Every command path: cadence help all\n");
    out
}

/// `cadence help all`: the full tree, one line per command path.
pub(super) fn all_help_text() -> String {
    let cmd = Cli::command();
    let mut out = String::from("All commands (one line per path):\n");
    let core = CORE.iter().flat_map(|(_, v)| v.iter().map(|(n, _)| *n));
    let mut adv: Vec<&str> = ADVANCED.to_vec();
    adv.sort_unstable();
    for name in core.chain(OPERATOR.iter().copied()).chain(adv) {
        if let Some(c) = cmd.find_subcommand(name) {
            verb_line(&mut out, c);
            tree_lines(&mut out, c, name);
        }
    }
    out.push_str("\nDetails for one verb: cadence help <verb> [<subcommand>]\n");
    out
}

/// `cadence help operator|advanced|all` as typed (with an optional
/// `--state-dir <dir>` anywhere): the text to print, else `None` and
/// clap handles the line — `cadence help <verb>` stays clap's own.
pub(super) fn help_section(args: &[String]) -> Option<String> {
    // The global `--state-dir` may sit anywhere: drop it and its value.
    let mut rest: Vec<&str> = Vec::new();
    let mut it = args.iter().skip(1).map(String::as_str);
    while let Some(a) = it.next() {
        if a == "--state-dir" {
            it.next()?;
        } else if !a.starts_with("--state-dir=") {
            rest.push(a);
        }
    }
    match rest.as_slice() {
        ["help", "operator" | "advanced"] => Some(operator_help_text()),
        ["help", "all"] => Some(all_help_text()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::Parser;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cli")
            .join(name)
    }

    /// Compare against a committed snapshot; `CAD_UPDATE_SNAPSHOTS=1`
    /// rewrites it.
    fn snapshot(name: &str, actual: &str) {
        let path = fixture(name);
        if std::env::var_os("CAD_UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&path, actual).unwrap();
        }
        let want = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual, want, "snapshot {name} differs");
    }

    fn render_root(in_pane: bool) -> String {
        root_command(in_pane).render_help().to_string()
    }

    #[test]
    fn root_help_is_small_and_ends_with_the_pointer_line() {
        let help = render_root(false);
        assert!(help.len() < 4096, "{} bytes", help.len());
        assert_eq!(help.trim_end().lines().last().unwrap(), MORE_OUTSIDE_PANE);
        for (_, verbs) in CORE {
            for (name, _) in *verbs {
                assert!(help.contains(&format!("  {name} ")), "{name} missing");
            }
        }
        // Operator and advanced verbs stay off the default list.
        for hidden in ["daemon", "rollout", "backup", "delivery", "job"] {
            assert!(!help.contains(&format!("  {hidden} ")), "{hidden} listed");
        }
    }

    #[test]
    fn help_views_match_their_snapshots() {
        snapshot("help-root.txt", &render_root(false));
        snapshot("help-root-pane.txt", &render_root(true));
        snapshot("help-operator.txt", &operator_help_text());
        snapshot("help-all.txt", &all_help_text());
    }

    #[test]
    fn pane_help_leaves_the_operator_pointer_out() {
        let help = render_root(true);
        assert!(help.trim_end().ends_with(MORE_IN_PANE));
        assert!(!help.contains("help operator"));
    }

    #[test]
    fn section_requests_are_recognised_exactly() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(help_section(&a(&["cadence", "help", "operator"])).is_some());
        assert!(help_section(&a(&["cadence", "help", "all"])).is_some());
        assert!(help_section(&a(&["cadence", "--state-dir", "/x", "help", "all"])).is_some());
        assert!(help_section(&a(&["cadence", "--state-dir=/x", "help", "operator"])).is_some());
        // A verb's own help stays clap's.
        assert!(help_section(&a(&["cadence", "help", "all", "--state-dir", "/x"])).is_some());
        assert!(help_section(&a(&["cadence", "help", "--state-dir=/x", "operator"])).is_some());
        assert!(help_section(&a(&["cadence", "help", "issue"])).is_none());
        assert!(help_section(&a(&["cadence", "help", "all", "x"])).is_none());
        assert!(help_section(&a(&["cadence", "issue", "help", "all"])).is_none());
        assert!(help_section(&a(&["cadence"])).is_none());
    }

    #[test]
    fn every_listed_verb_exists() {
        let cmd = Cli::command();
        for name in OPERATOR
            .iter()
            .copied()
            .chain(CORE.iter().flat_map(|(_, v)| v.iter().map(|(n, _)| *n)))
        {
            assert!(cmd.find_subcommand(name).is_some(), "{name} is not a verb");
        }
    }

    /// The pre-CAD-888 tree, generated from the base build's `--help`
    /// and committed: no verb may be removed, renamed or fail to parse.
    #[test]
    fn every_pre_change_command_path_still_parses_help() {
        let paths = std::fs::read_to_string(fixture("command-paths.txt")).unwrap();
        let mut n = 0;
        for line in paths.lines().filter(|l| !l.is_empty()) {
            let mut argv = vec!["cadence"];
            argv.extend(line.split(' '));
            argv.push("--help");
            let err = root_command(false)
                .try_get_matches_from(&argv)
                .expect_err(line);
            assert_eq!(err.kind(), ErrorKind::DisplayHelp, "{line}");
            n += 1;
        }
        assert!(n > 300, "fixture too small: {n}");
    }

    #[test]
    fn hidden_verbs_still_parse_and_run_paths() {
        // A hidden verb with real arguments parses, not only `--help`.
        assert!(Cli::try_parse_from(["cadence", "backup", "--reason", "x"]).is_ok());
        assert!(Cli::try_parse_from(["cadence", "daemon", "status"]).is_ok());
    }

    #[test]
    fn ls_and_list_are_interchangeable_on_every_lister() {
        fn walk(cmd: &Command, path: &str, found: &mut usize) {
            let names: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
            for sub in cmd.get_subcommands() {
                let here = format!("{path} {}", sub.get_name());
                let other = match sub.get_name() {
                    "ls" => Some("list"),
                    "list" => Some("ls"),
                    _ => None,
                };
                if let Some(other) = other {
                    *found += 1;
                    assert!(
                        sub.get_all_aliases().any(|a| a == other) || names.contains(&other),
                        "{here} lacks the `{other}` spelling"
                    );
                }
                walk(sub, &here, found);
            }
        }
        let mut found = 0;
        walk(&Cli::command(), "cadence", &mut found);
        assert!(found >= 20, "only {found} listers found");
        for argv in [
            ["cadence", "issue", "list"],
            ["cadence", "issue", "ls"],
            ["cadence", "agent", "ls"],
            ["cadence", "agent", "list"],
            ["cadence", "memory", "list"],
            ["cadence", "memory", "ls"],
            ["cadence", "wiki", "list"],
            ["cadence", "plan", "list"],
            ["cadence", "job", "ls"],
        ] {
            assert!(Cli::try_parse_from(argv).is_ok(), "{argv:?}");
        }
    }

    #[test]
    fn send_takes_the_recipient_positionally_or_with_to_but_not_both() {
        let ok = |v: &[&str]| {
            let mut a = vec!["cadence", "send"];
            a.extend(v);
            Cli::try_parse_from(a)
        };
        assert!(ok(&["pm", "--text", "hi"]).is_ok());
        assert!(ok(&["--to", "pm", "--text", "hi"]).is_ok());
        assert!(ok(&["pm", "--to", "pm", "--text", "hi"]).is_err());
        assert!(ok(&["--text", "hi"]).is_err());
    }

    #[test]
    fn platform_has_a_description() {
        let cmd = Cli::command();
        let p = cmd.find_subcommand("platform").unwrap();
        assert!(p.get_about().is_some());
    }

    /// Paths of every command clap hides on purpose, at any depth.
    fn originally_hidden() -> Vec<String> {
        fn walk(cmd: &Command, path: &str, out: &mut Vec<String>) {
            for sub in cmd.get_subcommands() {
                let here = if path.is_empty() {
                    sub.get_name().to_string()
                } else {
                    format!("{path} {}", sub.get_name())
                };
                if sub.is_hide_set() {
                    out.push(here);
                } else {
                    walk(sub, &here, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&Cli::command(), "", &mut out);
        out
    }

    #[test]
    fn deliberately_hidden_commands_appear_in_no_help_view() {
        let hidden = originally_hidden();
        assert_eq!(hidden.len(), 6, "{hidden:?}");
        for view in [
            render_root(false),
            render_root(true),
            operator_help_text(),
            all_help_text(),
        ] {
            for line in view.lines().map(str::trim) {
                for h in &hidden {
                    assert!(
                        line != h && !line.starts_with(&format!("{h} ")),
                        "hidden `{h}` listed: {line}"
                    );
                }
            }
        }
        // They still parse.
        for h in &hidden {
            let mut argv = vec!["cadence"];
            argv.extend(h.split(' '));
            argv.push("--help");
            let err = root_command(false).try_get_matches_from(&argv).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::DisplayHelp, "{h}");
        }
    }

    /// Core, operator, advanced and internal are an exact partition of
    /// the top-level verbs: a new verb fails here until it is filed.
    #[test]
    fn buckets_partition_the_top_level_verbs() {
        let mut filed: Vec<&str> = CORE
            .iter()
            .flat_map(|(_, v)| v.iter().map(|(n, _)| *n))
            .chain(OPERATOR.iter().copied())
            .chain(ADVANCED.iter().copied())
            .chain(INTERNAL.iter().copied())
            .collect();
        filed.sort_unstable();
        let mut dup = filed.clone();
        dup.dedup();
        assert_eq!(dup, filed, "a verb is filed in two buckets");
        let mut verbs: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        verbs.sort_unstable();
        assert_eq!(
            verbs, filed,
            "file each new top-level verb in CORE, OPERATOR, ADVANCED or INTERNAL"
        );
        let mut hidden: Vec<String> = Cli::command()
            .get_subcommands()
            .filter(|c| c.is_hide_set())
            .map(|c| c.get_name().to_string())
            .collect();
        hidden.sort_unstable();
        let mut internal: Vec<String> = INTERNAL.iter().map(|s| s.to_string()).collect();
        internal.sort_unstable();
        assert_eq!(hidden, internal);
    }

    #[test]
    fn help_all_lists_nested_paths() {
        let all = all_help_text();
        assert!(all.contains("    issue epic stage\n"), "{all}");
    }
}
