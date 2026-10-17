//! CAD-535: `cadence setup` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(state_dir: PathBuf, json: bool, port: Option<u16>, _: bool) -> Result<i32> {
    let code = cadence_agent::setup::cli(&state_dir, port, json, cli_verbs())?;
    emit_scope(&state_dir, json)?;
    Ok(code)
}

/// Folder, project, state dir, tracker and org are different things.
/// Reporting them does not create a repo, register a project, or change
/// the saved org. A `cwd/skills/<bundle>` snapshot is named and left as it is.
fn emit_scope(state_dir: &Path, json: bool) -> Result<()> {
    let cwd = std::env::current_dir()
        .unwrap_or_else(|error| PathBuf::from(format!("unreadable ({error})")));
    let pm_dir = cadence_agent::issue::default_dir()?;
    let named = std::env::var("CADENCE_PROJECT")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let project = cadence_agent::issue::project::observe(&pm_dir, &cwd, named.as_deref());
    let unmanaged = cadence_agent::skill::unmanaged_snapshots(&cwd);
    let org = match org::selected_org_readonly() {
        Ok(Some(name)) => json!({"selected": name, "changed": false}),
        Ok(None) => json!({"selected": Value::Null, "changed": false, "detail": "no saved org"}),
        Err(error) => json!({
            "selected": Value::Null,
            "changed": false,
            "detail": format!("saved org unreadable: {error}"),
        }),
    };
    let scope = json!({
        "check": "scope",
        "status": "ok",
        "detail": "folder, project, state dir, tracker and org are separate",
        "fix": Value::Null,
        "cwd": cwd,
        "project": project,
        "state_dir": state_dir,
        "tracker": pm_dir,
        "org": org,
        "unmanaged_snapshots": unmanaged,
        "changed_org": false,
    });
    let scope = if let Ok(home) = super::home_dir() {
        let status = cadence_agent::skill::status(&home);
        let mut scope = scope;
        scope["file_match"] = status["file_match"].clone();
        scope["provider_discovery"] = status["provider_discovery"].clone();
        scope["guidance"] = status["guidance"].clone();
        scope["guidance_attention"] = status["attention"].clone();
        scope["live_session"] = status["live_session"].clone();
        scope
    } else {
        scope
    };
    if json {
        // The checks above print one compact object per line. `print_json`
        // pretty-prints on a terminal or `CADENCE_JSON=pretty`, which would
        // split this added row across lines.
        println!("{}", serde_json::to_string(&scope).unwrap_or_default());
        return Ok(());
    }
    println!(
        "\nscope    cwd          {}",
        scope["cwd"].as_str().unwrap_or("?")
    );
    let project_line = if scope["project"]["resolved"] == true {
        format!(
            "{} ({})",
            scope["project"]["key"].as_str().unwrap_or("?"),
            scope["project"]["source"].as_str().unwrap_or("?")
        )
    } else {
        format!(
            "unresolved — {} Register with `{}`.",
            scope["project"]["detail"].as_str().unwrap_or("no project"),
            scope["project"]["register"]
                .as_str()
                .unwrap_or("cadence issue project add")
        )
    };
    println!("         project      {project_line}");
    println!("         state_dir    {}", state_dir.display());
    println!(
        "         tracker      {}",
        scope["tracker"].as_str().unwrap_or("?")
    );
    let org_line = scope["org"]["selected"]
        .as_str()
        .map(str::to_string)
        .or_else(|| scope["org"]["detail"].as_str().map(str::to_string))
        .unwrap_or_else(|| "no saved org".into());
    println!("         org          {org_line}");
    if let Some(guidance) = scope["guidance"].as_str() {
        println!(
            "         guidance     {guidance} (file match {}, provider discovery {})",
            scope["file_match"].as_str().unwrap_or("not-checked"),
            scope["provider_discovery"]
                .as_str()
                .unwrap_or("not-checked")
        );
    }
    if let Some(note) = scope["guidance_attention"].as_str() {
        println!("         guidance     {note}");
    }
    if let Some(rows) = scope["unmanaged_snapshots"].as_array() {
        for row in rows {
            println!(
                "         unmanaged    {} is not the active install and was left unchanged",
                row["path"].as_str().unwrap_or("?")
            );
        }
    }
    Ok(())
}
