//! Offline command search and input schema (CAD-881).
//!
//! The clap tree is the only catalogue. Nothing here reads state, org,
//! credentials, or defaults, and a hit is syntax documentation, not authority.

use super::*;
use clap::builder::StyledStr;
use clap::ArgAction;
use clap::CommandFactory;

struct Entry {
    path: Vec<String>,
    aliases: Vec<String>,
    summary: String,
    /// Lowercased long help, path, and aliases, for token search only.
    haystack: String,
}

pub(super) fn run(command: &Commands) -> Result<i32> {
    match command {
        Commands::Search { query, limit, all } => {
            let index = public_index();
            let value = if *all {
                let mut refs: Vec<&Entry> = index.iter().collect();
                refs.sort_by(|a, b| a.path.cmp(&b.path));
                catalogue(&refs, None)
            } else if let Some(query) = query {
                let tokens = tokens(query);
                if tokens.is_empty() {
                    return Err(Error::usage("commands query is empty"));
                }
                catalogue(&search(&index, &tokens), Some(limit.unwrap_or(5) as usize))
            } else {
                core_catalogue(&index)
            };
            print_json(&value);
            Ok(0)
        }
        Commands::Schema { path } => {
            print_json(&schema(path)?);
            Ok(0)
        }
        _ => Err(Error::usage("discovery is only commands and schema")),
    }
}

fn tokens(query: &str) -> Vec<String> {
    query.split_whitespace().map(|t| t.to_lowercase()).collect()
}

fn plain(styled: StyledStr) -> String {
    styled.to_string()
}

fn public_index() -> Vec<Entry> {
    let root = Cli::command();
    let mut out = Vec::new();
    for sub in root.get_subcommands().filter(|c| !c.is_hide_set()) {
        walk(sub, Vec::new(), &mut out);
    }
    out
}

fn walk(cmd: &clap::Command, mut path: Vec<String>, out: &mut Vec<Entry>) {
    path.push(cmd.get_name().to_string());
    let aliases: Vec<String> = cmd.get_visible_aliases().map(str::to_string).collect();
    let about = cmd.get_long_about().or_else(|| cmd.get_about());
    let summary = path
        .first()
        .filter(|_| path.len() == 1)
        .and_then(|name| help::core_blurb(name))
        .map(str::to_string)
        .unwrap_or_else(|| first_sentence(&about.map(ToString::to_string).unwrap_or_default()));
    let mut help_text = plain(cmd.clone().render_long_help()).to_lowercase();
    help_text.push(' ');
    help_text.push_str(&path.join(" ").to_lowercase());
    help_text.push(' ');
    help_text.push_str(&aliases.join(" ").to_lowercase());
    out.push(Entry {
        path,
        aliases,
        summary,
        haystack: help_text,
    });
    let here = out.last().map(|e| e.path.clone()).unwrap_or_default();
    for sub in cmd.get_subcommands().filter(|c| !c.is_hide_set()) {
        walk(sub, here.clone(), out);
    }
}

fn first_sentence(about: &str) -> String {
    let flat = about.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = match flat.find(". ") {
        Some(i) => &flat[..i],
        None => flat.trim_end_matches('.'),
    };
    first.to_string()
}

fn search<'a>(index: &'a [Entry], tokens: &[String]) -> Vec<&'a Entry> {
    let normalized = tokens.join(" ");
    let mut hits: Vec<&Entry> = index
        .iter()
        .filter(|e| tokens.iter().all(|t| e.haystack.contains(t)))
        .collect();
    hits.sort_by(|a, b| {
        let exact = |e: &Entry| e.path.join(" ") == normalized;
        let hits_in_name = |e: &Entry| {
            let names = format!("{} {}", e.path.join(" "), e.aliases.join(" ")).to_lowercase();
            tokens.iter().filter(|t| names.contains(t.as_str())).count()
        };
        exact(b)
            .cmp(&exact(a))
            .then(hits_in_name(b).cmp(&hits_in_name(a)))
            .then(a.path.cmp(&b.path))
    });
    hits
}

fn row(entry: &Entry) -> Value {
    json!({
        "path": entry.path,
        "aliases": entry.aliases,
        "summary": entry.summary,
    })
}

fn catalogue(entries: &[&Entry], limit: Option<usize>) -> Value {
    let total = entries.len();
    let shown = match limit {
        Some(n) => &entries[..n.min(total)],
        None => entries,
    };
    json!({
        "schema_version": 1,
        "commands": shown.iter().copied().map(row).collect::<Vec<_>>(),
        "total": total,
        "truncated": shown.len() < total,
    })
}

fn core_catalogue(index: &[Entry]) -> Value {
    let commands: Vec<Value> = help::core_names()
        .filter_map(|name| index.iter().find(|e| e.path == [name]))
        .map(row)
        .collect();
    let total = commands.len();
    json!({
        "schema_version": 1,
        "commands": commands,
        "total": total,
        "truncated": false,
    })
}

fn schema(path: &[String]) -> Result<Value> {
    let root = Cli::command();
    let mut cmd = &root;
    let mut canonical = Vec::new();
    let mut chain = Vec::new();
    for part in path {
        let next = cmd.get_subcommands().find(|sub| {
            !sub.is_hide_set()
                && (sub.get_name().eq_ignore_ascii_case(part)
                    || sub
                        .get_visible_aliases()
                        .any(|a| a.eq_ignore_ascii_case(part)))
        });
        let Some(next) = next else {
            return Err(Error::usage(format!(
                "unknown command path: {}",
                path.join(" ")
            )));
        };
        canonical.push(next.get_name().to_string());
        chain.push(next.clone());
        cmd = next;
    }
    let cmd = chain.last().expect("schema path is non-empty");
    let aliases: Vec<&str> = cmd.get_visible_aliases().collect();
    let description = cmd
        .get_long_about()
        .or_else(|| cmd.get_about())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let mut arguments = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for arg in root.get_arguments().filter(|arg| arg.is_global_set()) {
        push_arg(&mut arguments, &mut seen, arg);
    }
    for (i, node) in chain.iter().enumerate() {
        let leaf = i + 1 == chain.len();
        for arg in node.get_arguments() {
            if arg.is_global_set() || leaf {
                push_arg(&mut arguments, &mut seen, arg);
            }
        }
    }
    Ok(json!({
        "schema_version": 1,
        "path": canonical,
        "usage": plain(cmd.clone().render_usage()),
        "description": description,
        "aliases": aliases,
        "arguments": arguments,
        "note": "The parser and handler remain authoritative. This is syntax only, not a permission, readiness, or response schema.",
    }))
}

fn push_arg(out: &mut Vec<Value>, seen: &mut std::collections::BTreeSet<String>, arg: &clap::Arg) {
    if arg.is_hide_set()
        || matches!(
            arg.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        )
    {
        return;
    }
    let id = arg.get_id().as_str().to_string();
    if !seen.insert(id) {
        return;
    }
    out.push(argument(arg));
}

fn argument(arg: &clap::Arg) -> Value {
    let arity = arg.get_num_args().map(|range| {
        json!({
            "min": range.min_values(),
            "max": range.max_values(),
        })
    });
    let action = match arg.get_action() {
        ArgAction::Set => "set",
        ArgAction::Append => "append",
        ArgAction::SetTrue => "set_true",
        ArgAction::SetFalse => "set_false",
        ArgAction::Count => "count",
        other => {
            let _ = other;
            "other"
        }
    };
    let possible: Vec<String> = arg
        .get_possible_values()
        .into_iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| v.get_name().to_string())
        .collect();
    let mut aliases: Vec<String> = arg
        .get_visible_aliases()
        .unwrap_or_default()
        .into_iter()
        .map(|n| format!("--{n}"))
        .collect();
    if let Some(shorts) = arg.get_visible_short_aliases() {
        aliases.extend(shorts.into_iter().map(|c| format!("-{c}")));
    }
    json!({
        "id": arg.get_id().as_str(),
        "long": arg.get_long().map(|n| format!("--{n}")),
        "short": arg.get_short().map(|c| format!("-{c}")),
        "index": arg.get_index(),
        "aliases": aliases,
        "required": arg.is_required_set(),
        "arity": arity,
        "action": action,
        "possible_values": if possible.is_empty() { Value::Null } else { json!(possible) },
        "help": arg.get_help().map(|s| s.to_string()).unwrap_or_default(),
        "global": arg.is_global_set(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_issue_finds_the_canonical_path_first() {
        let index = public_index();
        let hits = search(&index, &tokens("comment issue"));
        assert!(!hits.is_empty());
        assert_eq!(hits[0].path, ["issue", "comment"]);
        assert!(hits.iter().all(|e| !e.path.iter().any(|p| p == "rollout")));
    }

    #[test]
    fn alias_resolves_to_the_canonical_path() {
        let value = schema(&["sandbox".into(), "up".into()]).unwrap();
        assert_eq!(value["path"], json!(["dev", "up"]));
    }

    #[test]
    fn hidden_path_is_a_usage_error() {
        let err = schema(&["rollout".into()]).unwrap_err();
        assert_eq!(err.kind(), "usage");
    }
}
