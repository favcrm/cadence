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
    if json {
        print_json(&scope);
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
