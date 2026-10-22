//! Print helpers and stdin body reading for the issue CLI.

use super::*;

pub(crate) fn read_stdin_body(flag: &str) -> Result<String> {
    const MAX: u64 = 4 << 20;
    if atty_stdin() {
        return Err(Error::rejected(format!(
            "{flag} -: stdin is a terminal — pipe the body in"
        )));
    }
    let mut buf = String::new();
    std::io::stdin().take(MAX + 1).read_to_string(&mut buf)?;
    if buf.len() as u64 > MAX {
        return Err(Error::rejected(format!("{flag} -: stdin is over 4 MiB")));
    }
    Ok(buf)
}

pub(crate) fn atty_stdin() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

/// Print rows as an aligned table — every column padded to its widest
/// cell except the last, which runs free.
pub(crate) fn print_table(rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|i| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .map(|c| c.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(cell);
            } else {
                let pad = widths[i] - cell.chars().count() + 2;
                line.push_str(cell);
                line.push_str(&" ".repeat(pad));
            }
        }
        println!("{}", line.trim_end());
    }
}

/// `issue ls --summary` (CAD-552): the one-call cross-project status —
/// per-project status counts plus the P0/P1 issues in `doing`/`review`,
/// so "how are the projects" costs the master one call, not a sweep of
/// `issue ls`/`issue show` reads.
pub(crate) fn ls_summary(views: &[&board::View]) -> Value {
    let mut projects: std::collections::BTreeMap<
        String,
        (serde_json::Map<String, Value>, Vec<Value>),
    > = Default::default();
    for v in views {
        let f = &v.issue.front;
        let entry = projects.entry(v.issue.project.clone()).or_default();
        let counts = entry.0.entry(v.status.clone()).or_insert(json!(0));
        *counts = json!(counts.as_u64().unwrap_or(0) + 1);
        if matches!(f.priority.as_str(), "P0" | "P1")
            && matches!(v.status.as_str(), "doing" | "review")
        {
            entry.1.push(json!({
                "id": f.id,
                "title": f.title,
                "status": v.status,
                "priority": f.priority,
                "owner": f.owner,
            }));
        }
    }
    json!({
        "issues": views.len(),
        "projects": projects
            .into_iter()
            .map(|(key, (counts, p0p1))| {
                (key, json!({"counts": counts, "p0p1": p0p1}))
            })
            .collect::<serde_json::Map<String, Value>>(),
    })
}

/// Compact human table for `issue ls` on a TTY.
pub(crate) fn print_ls_table(views: &[&board::View]) {
    if views.is_empty() {
        eprintln!("no issues — `cadence issue new \"title\"` creates one");
        return;
    }
    let mut rows = vec![[
        "ID",
        "STATUS",
        "PRI",
        "FLAGS",
        "OWNER",
        "CLAIM AGE",
        "TAGS",
        "TITLE",
    ]
    .map(str::to_string)
    .to_vec()];
    let now = crate::issue::time::now_epoch();
    for v in views {
        let f = &v.issue.front;
        let mut flags = String::new();
        if v.blocked {
            flags.push('B');
        }
        if v.status_source != "file" {
            flags.push('~'); // derived
        }
        if v.container {
            flags.push('C');
        }
        // CAD-755: S — the daemon marked this claim stale (holder's
        // agent dead past grace); it no longer blocks take-over.
        if f.claim.as_ref().is_some_and(|c| c.stale.is_some()) {
            flags.push('S');
        }
        rows.push(vec![
            f.id.clone(),
            v.status.clone(),
            f.priority.clone(),
            flags,
            f.owner.clone().unwrap_or_default(),
            f.claim
                .as_ref()
                .and_then(|c| crate::issue::time::parse_iso(&c.at))
                .map(|at| crate::inbox::fmt_age((now - at).max(0) as u64))
                .unwrap_or_default(),
            f.tags.join(","),
            f.title.clone(),
        ]);
    }
    print_table(&rows);
    eprintln!(
        "{} issues · B=blocked ~=derived-status C=container S=stale-claim",
        views.len()
    );
}

/// The stage line (exit criterion, next stage) and each health reason
/// of one epic's `work` block.
pub(crate) fn print_stage_and_health(w: &Value) {
    let st = &w["stage"];
    if st.is_null() {
        return;
    }
    let since = st["since"]
        .as_str()
        .map(|s| format!(" since {s}"))
        .unwrap_or_default();
    println!(
        "stage: {} ({}){since}",
        st["id"].as_str().unwrap_or("?"),
        st["source"].as_str().unwrap_or("?")
    );
    if let Some(exit) = st["exit"].as_str().filter(|e| !e.is_empty()) {
        println!("  exit: {exit}");
    }
    if let Some(next) = st["next"].as_str() {
        let who = if st["next_needs_operator"].as_bool() == Some(true) {
            " (operator)"
        } else {
            ""
        };
        println!("  next: {next}{who}");
    }
    let h = &w["health"];
    println!(
        "health: {}",
        h["state"].as_str().unwrap_or("?").replace('_', " ")
    );
    for r in h["reasons"].as_array().into_iter().flatten() {
        println!(
            "  {} — {} (owner {}; next: {})",
            r["cause"].as_str().unwrap_or("?"),
            r["detail"].as_str().unwrap_or(""),
            r["owner"].as_str().unwrap_or("-"),
            r["next"].as_str().unwrap_or("")
        );
    }
    if let Some(e) = w["config_error"].as_str() {
        println!("config: {e} — using the defaults");
    }
}

pub(crate) fn print_show(
    view: &board::View,
    by_id: &std::collections::HashMap<String, &board::View>,
) {
    let f = &view.issue.front;
    println!("{} — {}", f.id, f.title);
    println!(
        "status: {} (from {})  priority: {}  project: {}",
        view.status, view.status_source, f.priority, view.issue.project
    );
    if let Some(o) = &f.owner {
        println!("owner: {o}");
    }
    if let Some(c) = &f.component {
        println!("component: {c}");
    }
    if !f.tags.is_empty() {
        println!("tags: {}", f.tags.join(", "));
    }
    if view.ready {
        println!("ready: yes");
    }
    if view.blocked {
        println!("blocked: waits on {}", f.blocked_by.join(", "));
    }
    if let Some(p) = &f.parent {
        let title = by_id
            .get(p)
            .map(|v| v.issue.front.title.as_str())
            .unwrap_or("?");
        println!("parent: {p} — {title}");
    }
    if !view.children.is_empty() {
        println!("children: {}", view.children.join(", "));
    }
    if !view.blocks.is_empty() {
        println!("blocks: {}", view.blocks.join(", "));
    }
    if !f.relates.is_empty() {
        println!("relates: {}", f.relates.join(", "));
    }
    if let Some(d) = &f.duplicate_of {
        println!("duplicate_of: {d}");
    }
    for r in &f.refs {
        let target = r.url.as_deref().or(r.path.as_deref()).unwrap_or_default();
        let label = r.label.as_deref().unwrap_or(target);
        println!("ref {}: {} ({})", r.kind, label, target);
    }
    for (name, size) in &view.issue.artifacts {
        println!("artifact: {name} ({size} B)");
    }
    if view.checks_total > 0 {
        println!("checks: {}/{}", view.checks_done, view.checks_total);
    }
    for r in crate::issue::task_report::list(&view.issue.dir, &f.id) {
        let s = |k: &str| r[k].as_str().unwrap_or("?").to_string();
        match r["error"].as_str() {
            Some(e) => println!("report {} (malformed: {e})", s("path")),
            None => println!(
                "report {} by {} [{}]: {}",
                s("kind"),
                s("agent"),
                s("at"),
                s("path")
            ),
        }
    }
    for c in &view.issue.comments {
        println!("comment {} [{}]:", c.front.author, c.front.at);
        for line in c.body.trim().lines() {
            println!("    {line}");
        }
    }
    if !view.issue.body.trim().is_empty() {
        println!("\n{}", view.issue.body.trim());
    }
}
