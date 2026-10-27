//! Stage moves and type/milestone checks.

use super::*;

/// CAD-405: an issue with a plan or children is an epic — an explicit
/// other type would silently drop it from every epic view.
pub(super) fn check_type_change(pm: &Pm, front: &Front) -> Result<()> {
    let Some(t) = front.item_type.as_deref().filter(|t| *t != "epic") else {
        return Ok(());
    };
    let id = front.id.as_str();
    let why = if front.plan.is_some() {
        Some("it carries a plan")
    } else if board::load_all(&pm.dir, None)?
        .iter()
        .any(|i| i.front.parent.as_deref() == Some(id))
    {
        Some("it has children")
    } else {
        None
    };
    match why {
        Some(why) => Err(Error::rejected(format!(
            "{id} cannot be type '{t}' — {why}, so it is an epic"
        ))),
        None => Ok(()),
    }
}

/// CAD-405: a milestone the project's `PROJECT.md` declares — any
/// well-formed id when it declares none.
pub(super) fn check_milestone(
    pm: &Pm,
    project: &project::Project,
    milestone: Option<&str>,
) -> Result<()> {
    let Some(m) = milestone else { return Ok(()) };
    let cfg = crate::issue::work::load_config(&pm.dir, &project.key)?;
    if !cfg.milestones.is_empty() && !cfg.milestones.iter().any(|d| d.id == m) {
        return Err(Error::rejected(format!(
            "Unknown milestone '{m}' — {} declares: {}",
            project.key,
            cfg.milestones
                .iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(())
}

/// CAD-405 `issue epic stage` — move an epic to another stage: one
/// frontmatter write (`stage`, `stage_at`) and one tracker commit whose
/// `Actor:` trailer is the mover. The move is checked under the PM lock
/// ([`crate::issue::work::check_move`]: one stage forward, any stage
/// back), then `authorize` decides who may make it and returns the
/// actor — the daemon demands the proven operator for a forward move
/// into one of the project's `operator_stages`. A plan epic's first
/// forward move is `cadence plan approve`, an approved plan never moves
/// back before build (the plan owns shape), and a rejected plan never
/// moves. The gate keys apply only as `approvals` allow
/// ([`crate::issue::work::effective`]); a malformed `PROJECT.md`
/// refuses: a gate never guesses.
#[track_caller]
pub fn move_stage(
    pm: &Pm,
    epic: &str,
    to: &str,
    note: Option<&str>,
    approvals: &crate::issue::work::Approvals,
    authorize: impl FnOnce(&crate::issue::work::Move) -> Result<String>,
) -> Result<Value> {
    use crate::issue::work;
    let note = match note.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => {
            crate::secret::guard(&format!("{epic}: stage note"), n)?;
            Some(crate::issue::claim::clean_text("--note", n)?)
        }
        None => None,
    };
    let (project, dir) = issue_dir(pm, epic)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    let (mut front, body) = load_front(&dir)?;
    let (cfg, unapproved) = work::effective(
        &project.key,
        work::load_config(&pm.dir, &project.key)?,
        approvals.get(&project.key).map(String::as_str),
    );
    let views = board::views(&pm.config.notes_dir(), board::load_all(&pm.dir, None)?);
    let Some(view) = views.iter().find(|v| v.issue.front.id == epic) else {
        return Err(Error::internal(format!("{epic} missing from reload")));
    };
    let kind = model::item_type(&front, view.container);
    if kind != "epic" {
        return Err(Error::rejected(format!(
            "{epic} is a {kind} — only epics have stages \
             (`cadence issue set {epic} type=epic` makes it one)"
        )));
    }
    work::plan_allows_moves(&front)?;
    let cur = work::stage_of(&front, &cfg, view.status == "done");
    let mv = work::check_move(&cfg, &cur, to, work::floor(&front, &cfg))?;
    let by = authorize(&mv)?;
    let at = time::iso(time::now_epoch());
    front.stage = Some(mv.to.clone());
    front.stage_at = Some(at.clone());
    let file = dir.join("issue.md");
    let original = std::fs::read_to_string(&file)?;
    let mut subject = format!("{epic}: stage {} → {}", mv.from, mv.to);
    if let Some(n) = &note {
        subject.push_str(&format!(" — {n}"));
    }
    let written = save_front(&dir, &front, &body).and_then(|_| {
        commit_who_scoped(
            pm,
            &lock,
            std::slice::from_ref(&file),
            &subject,
            &[epic],
            "",
            Some(&by),
        )
    });
    let foreign = match written {
        Ok(f) => f,
        Err(e) => {
            // Nothing half-done: the file goes back to what was committed.
            let _ = atomic_write(&file, &original);
            return Err(e);
        }
    };
    let mut out = json!({
        "epic": epic,
        "project": project.key,
        "from": mv.from,
        "to": mv.to,
        "forward": mv.forward,
        "needs_operator": mv.needs_operator,
        "exit_met": mv.forward.then_some(mv.exit),
        "note": note,
        "by": by,
        "at": at,
        "config_unapproved": unapproved,
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
}
