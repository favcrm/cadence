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
            (
                "my-work",
                "Issues assigned to your signed-in hosted identity",
            ),
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
        "Ship",
        &[
            (
                "update",
                "Update this installation (client or native host); --to <sha> pins",
            ),
            (
                "dev",
                "Your disposable dev Cadence: up, reload, down, status, env",
            ),
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
    "backup",
    "restore",
    "export",
    "inspect-portable",
    "ui",
    "master",
    "connection",
    "platform",
    "app",
    "audit",
    "review",
    "session",
    "staging",
    "org",
    "team",
    "settings",
];

/// Visible verbs that are neither core nor operator.
const ADVANCED: &[&str] = &[
    "agent-uid",
    "attach",
    "artifact",
    "attachment",
    "auth",
    "claude",
    "codex",
    "commands",
    "cursor",
    "delivery",
    "devin",
    "document",
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
    "register",
    "remote",
    "report",
    "resume",
    "schema",
    "setup",
    "skill",
    "stop",
    "template",
    "test",
    "thread",
    "topic",
    "web",
    "workflow",
];

/// Verbs clap hides on purpose: plumbing no help view lists.
/// `rollout` and `upgrade` (CAD-1187) are recovery internals behind
/// `cadence update`; docs/CLI.md "Recovery / internals" documents them.
#[cfg(test)]
const INTERNAL: &[&str] = &[
    "confine",
    "local-agent",
    "mcp-agent",
    "mcp-permission",
    "rollout",
    "upgrade",
];

const MORE_OUTSIDE_PANE: &str = "More: cadence help operator | cadence help all";
/// Inside a pane the operator list is left out of the pointers.
const MORE_IN_PANE: &str = "More: cadence help all";

fn is_core(name: &str) -> bool {
    CORE.iter().any(|(_, v)| v.iter().any(|(n, _)| *n == name))
}

/// The core root verb's existing one-line summary, when `name` is core.
pub(super) fn core_blurb(name: &str) -> Option<&'static str> {
    CORE.iter()
        .flat_map(|(_, verbs)| verbs.iter())
        .find(|(n, _)| *n == name)
        .map(|(_, blurb)| *blurb)
}

/// Core root command names in help order.
pub(super) fn core_names() -> impl Iterator<Item = &'static str> {
    CORE.iter()
        .flat_map(|(_, verbs)| verbs.iter().map(|(n, _)| *n))
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
    out.push_str("Find a command: cadence commands <query>\n");
    out.push_str("Inspect one path: cadence schema <command-path>\n");
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

/// CAD-1432: the master's `cadence help`. The Pi guard already admits
/// only a literal admitted command path; this renders it — the profile's
/// admitted paths only, never the whole clap tree — so a Claude master
/// (no JS guard) and every operator tool sees the same bounded text.
/// `None` for every caller but the master's own argv, so operator help is
/// untouched. A malformed or disallowed master help form is a usage error
/// (nonzero), never help text: refusal text must not exit 0.
pub(super) fn master_help(args: &[String]) -> Option<cadence_agent::error::Result<String>> {
    use cadence_agent::error::Error;
    if !cadence_agent::master::caller_is_master() {
        return None;
    }
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let help_verb = rest.first() == Some(&"help");
    // Same probe as the Pi guard: `--help`, or a single-dash cluster with
    // an `h` before any standalone `--` (everything after `--` is payload).
    let probe = help_probe(&rest);
    if !help_verb && !probe {
        return None;
    }
    // A scoped turn has no help route at all (CAD-1432).
    if cadence_agent::master::in_app_conversation() {
        return Some(Err(Error::rejected(
            "help and --help are not available in a scoped chat turn — report the exact refusal and stop",
        )));
    }
    if !help_verb {
        // Every master --help/-h probe refuses, the root one included:
        // the exact ticket contract admits only `help` and `help <path>`.
        return Some(Err(Error::usage(
            "--help and -h are not available — run cadence help <command-path> for the exact usage of a command you may run",
        )));
    }
    let paths = cadence_agent::master::help_paths(cadence_agent::master::Profile::Home);
    let target = &rest[1..];
    if target.is_empty() {
        return Some(Ok(master_listing()));
    }
    let path = target.join(" ");
    if !target.iter().all(|t| is_token(t)) || !paths.iter().any(|p| p == &path) {
        return Some(Err(Error::usage(format!(
            "`{path}` is not a command path you may run — use cadence help, or cadence help <command-path> for one of: {}; flags, argument values and --help are refused",
            paths.join(", ")
        ))));
    }
    // Parent (other admitted paths sit under it): list only those.
    let children: Vec<&String> = paths
        .iter()
        .filter(|p| p.starts_with(&format!("{path} ")))
        .collect();
    if !children.is_empty() {
        let mut out = format!("`{path}` paths you may run:\n");
        for c in &children {
            out.push_str(&format!("  cadence {c}\n"));
        }
        return Some(Ok(out));
    }
    // Leaf: clap's usage for the command, built from the top tree so it
    // keeps the full `cadence <path>` prefix. The global options and
    // -h/--help are hidden: the guard admits none of them.
    let mut cmd = Cli::command();
    cmd.build();
    let mut node = &cmd;
    for tok in target {
        match node.find_subcommand(tok) {
            Some(sub) => node = sub,
            None => {
                return Some(Err(Error::usage(format!(
                    "`{path}` is not a command path you may run"
                ))));
            }
        }
    }
    let mut leaf = node.clone().mut_args(|a| {
        let hide = matches!(
            a.get_id().as_str(),
            "help" | "state_dir" | "org" | "wake_timeout"
        );
        a.hide(hide)
    });
    let mut out = leaf.render_help().to_string().trim_end().to_string();
    out.push('\n');
    if let Some(limits) = master_leaf_limits(&path) {
        out.push_str(&format!("\nLimits for you: {limits}\n"));
    }
    Some(Ok(out))
}

/// The master's bounded command listing: exactly the Home profile's
/// admitted commands, never the whole clap tree.
fn master_listing() -> String {
    let mut out = String::from("Commands you may run (exact forms; anything else is refused):\n");
    for stem in cadence_agent::master::allowed_tools(cadence_agent::master::Profile::Home) {
        let Some(inner) = stem
            .strip_prefix("Bash(cadence ")
            .and_then(|t| t.strip_suffix(')'))
        else {
            continue;
        };
        let (head, args_ok) = match inner.strip_suffix(" *") {
            Some(h) => (h, true),
            None => (inner, false),
        };
        if head == "help" {
            continue;
        }
        out.push_str(&format!(
            "  cadence {head}{}\n",
            if args_ok { " …" } else { "" }
        ));
    }
    out.push_str("\nUsage of one path: cadence help <command-path>\n");
    out
}

/// CAD-1432: a help probe is `--help`, or a single-dash option cluster
/// whose leading letters include `h` (`-h`, `-hh`, `-vh`, `-h=x`, `-h1`),
/// before any standalone `--`. Everything after `--` is payload.
fn help_probe(rest: &[&str]) -> bool {
    for t in rest {
        if *t == "--" {
            return false;
        }
        if *t == "--help" {
            return true;
        }
        if let Some(cluster) = t.strip_prefix('-') {
            if cluster.starts_with("-") {
                continue;
            }
            let letters: String = cluster
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            if !letters.is_empty() && letters.contains('h') {
                return true;
            }
        }
    }
    false
}

fn is_token(t: &str) -> bool {
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The guard rules a leaf's usage line cannot show — stated so a fresh
/// master reads the bounds it actually runs under, not clap's full flag
/// grammar.
fn master_leaf_limits(path: &str) -> Option<&'static str> {
    Some(match path {
        "issue new" => {
            "backlog tickets only: never --owner or --id; --status only as `--status backlog`; \
             --file names a file under master/tmp"
        }
        "wiki put" => {
            "path must be under agents/master/knowledge/; body via --file <file under master/tmp>; \
             -m/--text refused"
        }
        "project new" => {
            "--goal and each --agent require --repo and take one plain token (letters, digits and . _ / = : -; no spaces, quotes or commas); repeat --agent for each agent"
        }
        "plan propose" | "report file" | "master escalate" => {
            "--file names a file under master/tmp (stage it with the write tool first)"
        }
        _ => return None,
    })
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
        for hidden in ["daemon", "rollout", "upgrade", "backup", "delivery", "job"] {
            assert!(!help.contains(&format!("  {hidden} ")), "{hidden} listed");
        }
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
        // `skill sync` is the hidden implementation `cadence update` calls.
        // `cadence skill install` stays the visible manual repair.
        assert_eq!(hidden.len(), 13, "{hidden:?}");
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
