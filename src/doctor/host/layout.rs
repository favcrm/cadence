//! CAD-584: `cadence doctor host` check `layout` — the CADENCE_HOME
//! resolution this host sees: which branch the resolver took
//! (`CADENCE_HOME`, the `~/.cadence/LAYOUT` marker, or the legacy
//! layout) and every path it yields. Informational on success; the
//! check fails only when resolution itself is refused — a
//! non-absolute `CADENCE_HOME` breaks every cadence path lookup.

use super::*;

/// Resolve each directory the resolver answers for, letting one
/// unresolved dir (a `HOME`-dependent legacy path under an env
/// without `HOME`) report in place instead of hiding the rest.
fn dir_value(dir: Result<PathBuf>) -> Value {
    match dir {
        Ok(d) => json!(d.display().to_string()),
        Err(e) => json!(format!("unresolved: {e}")),
    }
}

pub(super) fn check_layout(_scan: &Scan) -> Check {
    let layout = match crate::home::layout() {
        Ok(l) => l,
        Err(e) => {
            return check(
                "layout",
                Level::Fail,
                json!({"error": e.to_string()}),
                Value::Null,
                format!("home resolution refused: {e}"),
                "export CADENCE_HOME as an absolute path or unset it".to_string(),
            );
        }
    };
    let dirs = [
        ("tracker", crate::home::tracker_dir()),
        ("state", crate::home::state_dir()),
        ("vault", crate::home::vault_dir()),
        ("repos", crate::home::repos_dir()),
    ];
    let detail = std::iter::once(format!(
        "{} → {}",
        layout.source.as_str(),
        layout.root.display()
    ))
    .chain(dirs.iter().map(|(name, dir)| match dir {
        Ok(d) => format!("{name} {}", d.display()),
        Err(e) => format!("{name} unresolved ({e})"),
    }))
    .collect::<Vec<_>>()
    .join("; ");
    check(
        "layout",
        Level::Ok,
        json!({
            "source": layout.source.as_str(),
            "home": layout.root.display().to_string(),
            "tracker": dir_value(crate::home::tracker_dir()),
            "state": dir_value(crate::home::state_dir()),
            "vault": dir_value(crate::home::vault_dir()),
            "repos": dir_value(crate::home::repos_dir()),
        }),
        Value::Null,
        detail,
        String::new(),
    )
}
