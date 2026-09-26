//! CAD-535: `cadence overview` — moved verbatim from src/main.rs.

use super::*;

/// `cadence overview` — the needs-me list, deploy drift and per-project
/// summary, rendered as an aligned list (or the raw payload with
/// `--json`). Read-only: every source degrades rather than failing the
/// screen.
pub(super) fn run_overview(
    state_dir: &Path,
    json_out: bool,
    watch: Option<u64>,
    scope: cadence_agent::overview::Scope,
) -> Result<i32> {
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let pm_dir = cadence_agent::issue::default_dir().unwrap_or_default();
    let opts = cadence_agent::overview::Options {
        scope,
        ..cadence_agent::overview::Options::cli()
    };
    loop {
        let view = cadence_agent::overview::overview_with(state_dir, &pm_dir, &opts)
            .map_err(Error::rejected)?;
        if json_out {
            print_json(&view);
        } else {
            print_overview(&view);
        }
        let Some(secs) = watch else {
            return Ok(0);
        };
        std::thread::sleep(Duration::from_secs(secs));
        if tty && !json_out {
            print!("\x1b[2J\x1b[H");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    }
}

/// `needs_me` sections in render order, keyed by the server-resolved
/// `audience` (CAD-253) — the CLI never maps kinds to audiences.
pub(super) const NEED_SECTIONS: [(&str, &str); 4] = [
    ("operator", "needs your decision"),
    ("team", "team handling"),
    ("dependency", "waiting on dependency"),
    ("info", "information"),
];

/// Aligned-list rendering of the overview payload — the TTY default.
pub(super) fn print_overview(view: &Value) {
    let needs = view["needs_me"].as_array().cloned().unwrap_or_default();
    println!("NEEDS ME");
    let mut widths = [0usize; 5];
    let mut rows: Vec<(String, [String; 6])> = Vec::new();
    for n in &needs {
        let title = n["title"].as_str().unwrap_or_default();
        let title: String = title.chars().take(52).collect();
        // One row per subject: every cause, most severe first.
        let causes: Vec<&str> = n["causes"]
            .as_array()
            .map(|cs| cs.iter().filter_map(|c| c["cause"].as_str()).collect())
            .unwrap_or_default();
        let kind = if causes.is_empty() {
            n["kind"].as_str().unwrap_or_default().to_string()
        } else {
            causes.join("+")
        };
        let row = [
            kind,
            fmt_age(n["age"].as_i64().unwrap_or(0)),
            n["project"].as_str().unwrap_or_default().to_string(),
            title,
            n["audience_reason"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            n["command"].as_str().unwrap_or_default().to_string(),
        ];
        for (i, c) in row[..5].iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
        // A row without a known `audience` (an older server) reads as
        // team work.
        let audience = n["audience"]
            .as_str()
            .filter(|a| NEED_SECTIONS.iter().any(|(k, _)| k == a))
            .unwrap_or("team");
        rows.push((audience.to_string(), row));
    }
    for (key, label) in NEED_SECTIONS {
        let section: Vec<&[String; 6]> = rows
            .iter()
            .filter(|(a, _)| a == key)
            .map(|(_, r)| r)
            .collect();
        // The decision section always renders — an empty one is an
        // answer, not an omission.
        if section.is_empty() && key != "operator" {
            continue;
        }
        println!("  {label}");
        if section.is_empty() {
            println!("    nothing needs your decision");
        }
        for r in section {
            println!(
                "    {:<w0$}  {:>w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}",
                r[0],
                r[1],
                r[2],
                r[3],
                r[4],
                r[5],
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2],
                w3 = widths[3],
                w4 = widths[4],
            );
        }
    }
    let drift = &view["drift"];
    println!();
    println!("DRIFT");
    if drift["known"].as_bool().unwrap_or(false) {
        let n = drift["count"].as_i64().unwrap_or(0);
        let commit = drift["build_commit"]
            .as_str()
            .map(|c| c.chars().take(10).collect::<String>())
            .unwrap_or_else(|| "?".to_string());
        if n == 0 {
            println!(
                "  {} is running the latest on {}",
                drift["project"].as_str().unwrap_or("?"),
                drift["ref"].as_str().unwrap_or("?")
            );
        } else {
            println!(
                "  {}: {n} commit(s) past build {} on {}",
                drift["project"].as_str().unwrap_or("?"),
                commit,
                drift["ref"].as_str().unwrap_or("?")
            );
            // `pr` is parsed from the subject's own `(#N)` tail, so the
            // subject already carries it.
            for c in drift["commits"].as_array().cloned().unwrap_or_default() {
                println!("    · {}", c["subject"].as_str().unwrap_or(""));
            }
        }
    } else {
        println!("  {}", drift["reason"].as_str().unwrap_or("cannot tell"));
    }
    let projects = view["projects"].as_array().cloned().unwrap_or_default();
    if !projects.is_empty() {
        println!();
        println!("PROJECTS");
        for p in &projects {
            let counts = p["open_by_status"]
                .as_object()
                .map(|m| {
                    let mut pairs: Vec<(&String, &Value)> = m.iter().collect();
                    pairs.sort_by(|a, b| a.0.cmp(b.0));
                    pairs
                        .iter()
                        .map(|(k, v)| format!("{k}:{}", v.as_i64().unwrap_or(0)))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_else(|| "-".to_string());
            let review = p["oldest_review_age"]
                .as_i64()
                .map(|a| format!("  oldest review {}", fmt_age(a)))
                .unwrap_or_default();
            println!(
                "  {:<14} {:<40}{}",
                p["key"].as_str().unwrap_or(""),
                counts,
                review
            );
            // CAD-383: who holds each in-flight issue, and since when.
            for c in p["claims"].as_array().cloned().unwrap_or_default() {
                let by = c["by"].as_str().unwrap_or("?");
                let owner = c["owner"]
                    .as_str()
                    .filter(|o| *o != by)
                    .map(|o| format!(" (owner {o})"))
                    .unwrap_or_default();
                let age = c["age_secs"]
                    .as_i64()
                    .map(|a| format!("claimed {} ago", fmt_age(a)))
                    .unwrap_or_else(|| "claim age unknown".to_string());
                println!(
                    "    {:<10} {:<7} {by}{owner} — {age}",
                    c["issue"].as_str().unwrap_or(""),
                    c["status"].as_str().unwrap_or(""),
                );
            }
        }
    }
    if view["github"]["state"].as_str() == Some("unavailable") {
        println!();
        println!("github: unavailable — PR and CI rows absent");
    }
    if !view["daemon"]["reachable"].as_bool().unwrap_or(false) {
        println!("daemon: unreachable — agent, approval and drift rows absent");
    }
    // CAD-249: sources that missed their bound — the screen narrowed.
    for d in view["degraded"].as_array().cloned().unwrap_or_default() {
        let subject = d["subject"].as_str().filter(|s| !s.is_empty());
        println!(
            "degraded: {}{}: {}",
            d["source"].as_str().unwrap_or("?"),
            subject.map(|s| format!(" {s}")).unwrap_or_default(),
            d["detail"].as_str().unwrap_or("")
        );
    }
}

pub(super) fn run(
    state_dir: PathBuf,
    json: bool,
    watch: Option<u64>,
    project: Option<String>,
    group: Option<String>,
) -> Result<i32> {
    let scope = cadence_agent::overview::Scope { project, group };
    run_overview(&state_dir, json, watch, scope)
}
