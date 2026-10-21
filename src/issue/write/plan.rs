//! Plan creation, decision and structure checks.

use super::*;

/// CAD-359 `plan propose` — the epic (the plan, `plan.state:
/// proposed`) and one child per ticket in `backlog`, with acceptance,
/// sizes, suggested agents (`owner`) and `blocked_by` links, all in ONE
/// tracker commit. Ids are allocated under the lock; any structural
/// refusal removes every folder this call created.
pub fn create_plan(
    pm: &Pm,
    project_key: &str,
    doc: &crate::issue::plan::PlanDoc,
    workflow: Option<&str>,
    actor: &str,
    // CAD-225: ids a durable unattended intent reserved but never wrote
    // (a crashed admission). Read under the lock so ordinary creation
    // can never land on a reserved id — the intent stays reconcilable.
    avoid: &dyn Fn() -> Result<HashSet<String>>,
) -> Result<Value> {
    use crate::issue::plan::Dep;
    let project = project::list(&pm.dir)?
        .into_iter()
        .find(|p| p.key == project_key)
        .ok_or_else(|| {
            Error::rejected(format!(
                "Unknown project '{project_key}' — `cadence issue project ls` lists them"
            ))
        })?;
    let proposer = actor_who(actor, None);
    let root = pm.dir.join(&project.key);
    let lock = pm.lock_for_paths(std::slice::from_ref(&root))?;
    let avoid = avoid()?;
    let first = next_id(&root, &project.prefix)?;
    let mut ids: Vec<String> = (0..=doc.tickets.len() as u64)
        .map(|n| format!("{}-{}", project.prefix, first + n))
        .collect();
    // A reserved id skips the whole contiguous block, never packs it.
    for offset in 1..=(avoid.len() + ids.len() + 2) as u64 {
        if !ids.iter().any(|id| avoid.contains(id)) {
            break;
        }
        ids = (0..=doc.tickets.len() as u64)
            .map(|n| format!("{}-{}", project.prefix, first + offset + n))
            .collect();
    }
    if ids.iter().any(|id| avoid.contains(id)) {
        return Err(Error::internal("could not allocate unreserved ticket ids"));
    }
    let epic = ids[0].clone();
    let now = time::iso(time::now_epoch());

    let mut epic_front = Front::new(&epic, &doc.title, &now);
    epic_front.plan = Some(model::Plan {
        state: "proposed".to_string(),
        proposed_by: proposer.clone(),
        proposed_at: now.clone(),
        decided_by: None,
        decided_at: None,
        reason: None,
        workflow: workflow.map(str::to_string),
        tickets: ids[1..].to_vec(),
    });
    epic_front.item_type = Some("epic".to_string());
    let mut epic_body = format!("{}\n\n## Goal\n\n{}\n", doc.title, doc.goal);
    if !doc.non_goals.is_empty() {
        epic_body.push_str("\n## Non-goals\n\n");
        for g in &doc.non_goals {
            epic_body.push_str(&format!("- {g}\n"));
        }
    }
    if !doc.intro.is_empty() {
        epic_body.push_str(&format!("\n## Plan\n\n{}\n", doc.intro));
    }
    let mut files: Vec<(PathBuf, Front, String)> = vec![(root.join(&epic), epic_front, epic_body)];
    for (n, ticket) in doc.tickets.iter().enumerate() {
        let id = &ids[n + 1];
        let mut front = Front::new(id, &ticket.title, &now);
        front.parent = Some(epic.clone());
        front.plan_epic = Some(epic.clone());
        front.size = ticket.size.clone();
        front.owner = ticket.agent.clone();
        front.blocked_by = ticket
            .depends_on
            .iter()
            .map(|d| match d {
                Dep::Ticket(k) => ids[k + 1].clone(),
                Dep::Issue(i) => i.clone(),
            })
            .collect();
        let mut body = format!("{}\n", ticket.title);
        if !ticket.description.is_empty() {
            body.push_str(&format!("\n{}\n", ticket.description));
        }
        let body = parse::replace_acceptance(&body, &ticket.acceptance)?;
        files.push((root.join(id), front, body));
    }
    if let Some((dir, _, _)) = files.iter().find(|(dir, _, _)| dir.exists()) {
        return Err(Error::rejected(format!(
            "{} already exists — refusing to overwrite it",
            dir.display()
        )));
    }
    let created = || files.iter().map(|(dir, _, _)| dir);
    let build = || -> Result<Vec<String>> {
        for (dir, front, body) in &files {
            std::fs::create_dir_all(dir.join("comments"))?;
            std::fs::create_dir_all(dir.join("artifacts"))?;
            save_front(dir, front, body)?;
        }
        let issues = board::load_all(&pm.dir, None)?;
        for id in &ids {
            check_structure(&issues, id)?;
        }
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let paths: Vec<PathBuf> = files
            .iter()
            .map(|(dir, _, _)| dir.join("issue.md"))
            .collect();
        commit_scoped(
            pm,
            &lock,
            &paths,
            &format!(
                "{epic}: plan proposed — {} ({} tickets)",
                doc.title,
                doc.tickets.len()
            ),
            &refs,
            actor,
        )
    };
    let foreign = match build() {
        Ok(foreign) => foreign,
        Err(e) => {
            for dir in created() {
                let _ = std::fs::remove_dir_all(dir);
            }
            return Err(e);
        }
    };
    let mut out = json!({
        "epic": epic,
        "project": project.key,
        "title": doc.title,
        "state": "proposed",
        "proposed_by": proposer,
        "tickets": ids[1..],
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// CAD-225: the canonical original semantics an unattended grant binds
/// — everything the authorized work IS, nothing about how it is going.
/// Runtime state (`status`, `claim`, `stage`, timestamps, `created`,
/// `refs`, tags, advisory `paths`) is deliberately absent: a ticket
/// moving `ready` → `doing` is still the same work; a changed title,
/// body, owner, size, dependency or plan membership is not.
pub(crate) fn granted_semantics(front: &Front, body: &str) -> String {
    #[derive(serde::Serialize)]
    struct CanonicalPlan<'a> {
        state: &'a str,
        proposed_by: &'a str,
        decided_by: Option<&'a str>,
        workflow: Option<&'a str>,
        tickets: &'a [String],
        reason: Option<&'a str>,
    }

    #[derive(serde::Serialize)]
    struct CanonicalIssue<'a> {
        id: &'a str,
        title: &'a str,
        priority: &'a str,
        owner: Option<&'a str>,
        size: Option<&'a str>,
        parent: Option<&'a str>,
        plan_epic: Option<&'a str>,
        item_type: Option<&'a str>,
        blocked_by: Vec<&'a str>,
        plan: Option<CanonicalPlan<'a>>,
        body: &'a str,
    }

    let mut blocked_by: Vec<&str> = front.blocked_by.iter().map(String::as_str).collect();
    blocked_by.sort_unstable();
    let canonical = CanonicalIssue {
        id: &front.id,
        title: &front.title,
        priority: &front.priority,
        owner: front.owner.as_deref(),
        size: front.size.as_deref(),
        parent: front.parent.as_deref(),
        plan_epic: front.plan_epic.as_deref(),
        item_type: front.item_type.as_deref(),
        blocked_by,
        plan: front.plan.as_ref().map(|plan| CanonicalPlan {
            state: &plan.state,
            proposed_by: &plan.proposed_by,
            decided_by: plan.decided_by.as_deref(),
            workflow: plan.workflow.as_deref(),
            tickets: &plan.tickets,
            reason: plan.reason.as_deref(),
        }),
        body,
    };
    serde_json::to_string(&canonical)
        .expect("canonical semantic strings, slices and options serialize without failure")
}

/// The reserved ids and canonical semantics of a grant admission,
/// computed under the PM lock before any tracker file exists — the
/// daemon records them as the durable intent inside the same lock.
pub(crate) struct GrantedPreview {
    pub epic: String,
    pub tickets: Vec<String>,
    /// Canonical semantics per issue id — [`granted_semantics`].
    pub semantics: std::collections::BTreeMap<String, String>,
}

/// CAD-225 unattended admission: the grant-bound plan materializes
/// `approved` with `ready` tickets in ONE tracker commit, decided by the
/// workflow grant (`decided_by`), never a fabricated operator decision.
///
/// `fixed` re-creates the exact `Some((epic, tickets))` ids a recorded
/// intent reserved — the recovery path. `expected_semantics` is the
/// intent's canonical map; a rebuild that no longer matches refuses
/// before anything is written.
///
/// `on_intent` runs under the PM lock, after the reserved ids and
/// canonical semantics are computed and the disk is proven free of the
/// binding, but before any folder is created — the durable intent is
/// on record before any tracker mutation. It is skipped on the pure
/// readback path (existing files already identical).
///
/// Disk classification with `fixed` ids: all absent → write (or, with
/// `allow_write` false, `unattended_missing`); all present and
/// semantically identical → return the existing run (`reused`), a
/// second plan is never allocated; anything else → `unattended_diverged`,
/// the caller marks the intent held.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_granted_plan(
    pm: &Pm,
    project_key: &str,
    doc: &crate::issue::plan::PlanDoc,
    workflow: Option<&str>,
    actor: &str,
    decided_by: &str,
    fixed: Option<(&str, &[String])>,
    expected_semantics: Option<&std::collections::BTreeMap<String, String>>,
    allow_write: bool,
    avoid: &dyn Fn() -> Result<HashSet<String>>,
    on_intent: &mut dyn FnMut(&GrantedPreview) -> Result<()>,
) -> Result<Value> {
    use crate::issue::plan::Dep;
    let project = project::list(&pm.dir)?
        .into_iter()
        .find(|p| p.key == project_key)
        .ok_or_else(|| {
            Error::rejected(format!(
                "Unknown project '{project_key}' — `cadence issue project ls` lists them"
            ))
        })?;
    let proposer = actor_who(actor, None);
    let root = pm.dir.join(&project.key);
    let lock = pm.lock_for_paths(std::slice::from_ref(&root))?;
    let ids: Vec<String> = match fixed {
        Some((epic, tickets)) => {
            if tickets.len() != doc.tickets.len() {
                return Err(Error::invalid(
                    "unattended_diverged",
                    format!(
                        "the recorded intent binds {} tickets but the render has {}",
                        tickets.len(),
                        doc.tickets.len()
                    ),
                ));
            }
            let mut seen = HashSet::new();
            for id in std::iter::once(epic).chain(tickets.iter().map(String::as_str)) {
                model::check_id(id)?;
                if !id.starts_with(&format!("{}-", project.prefix)) {
                    return Err(Error::invalid(
                        "unattended_diverged",
                        format!("reserved id '{id}' does not belong to {project_key}"),
                    ));
                }
                if !seen.insert(id.to_string()) {
                    return Err(Error::invalid(
                        "unattended_diverged",
                        format!("reserved id '{id}' is bound twice"),
                    ));
                }
            }
            std::iter::once(epic.to_string())
                .chain(tickets.iter().cloned())
                .collect()
        }
        None => {
            let avoid = avoid()?;
            let first = next_id(&root, &project.prefix)?;
            let mut ids: Vec<String> = (0..=doc.tickets.len() as u64)
                .map(|n| format!("{}-{}", project.prefix, first + n))
                .collect();
            for offset in 1..=(avoid.len() + ids.len() + 2) as u64 {
                if !ids.iter().any(|id| avoid.contains(id)) {
                    break;
                }
                ids = (0..=doc.tickets.len() as u64)
                    .map(|n| format!("{}-{}", project.prefix, first + offset + n))
                    .collect();
            }
            if ids.iter().any(|id| avoid.contains(id)) {
                return Err(Error::internal("could not allocate unreserved ticket ids"));
            }
            ids
        }
    };
    let epic = ids[0].clone();
    let now = time::iso(time::now_epoch());

    let mut epic_front = Front::new(&epic, &doc.title, &now);
    epic_front.plan = Some(model::Plan {
        state: "approved".to_string(),
        proposed_by: proposer.clone(),
        proposed_at: now.clone(),
        decided_by: Some(decided_by.to_string()),
        decided_at: Some(now.clone()),
        reason: None,
        workflow: workflow.map(str::to_string),
        tickets: ids[1..].to_vec(),
    });
    epic_front.item_type = Some("epic".to_string());
    let mut epic_body = format!("{}\n\n## Goal\n\n{}\n", doc.title, doc.goal);
    if !doc.non_goals.is_empty() {
        epic_body.push_str("\n## Non-goals\n\n");
        for g in &doc.non_goals {
            epic_body.push_str(&format!("- {g}\n"));
        }
    }
    if !doc.intro.is_empty() {
        epic_body.push_str(&format!("\n## Plan\n\n{}\n", doc.intro));
    }
    let mut files: Vec<(PathBuf, Front, String)> = vec![(root.join(&epic), epic_front, epic_body)];
    for (n, ticket) in doc.tickets.iter().enumerate() {
        let id = &ids[n + 1];
        let mut front = Front::new(id, &ticket.title, &now);
        // Grant-admitted tickets are `ready` at create — the plan is
        // already approved, so `check_status_write` admits it.
        front.status = "ready".to_string();
        front.parent = Some(epic.clone());
        front.plan_epic = Some(epic.clone());
        front.size = ticket.size.clone();
        front.owner = ticket.agent.clone();
        front.blocked_by = ticket
            .depends_on
            .iter()
            .map(|d| match d {
                Dep::Ticket(k) => ids[k + 1].clone(),
                Dep::Issue(i) => i.clone(),
            })
            .collect();
        let mut body = format!("{}\n", ticket.title);
        if !ticket.description.is_empty() {
            body.push_str(&format!("\n{}\n", ticket.description));
        }
        let body = parse::replace_acceptance(&body, &ticket.acceptance)?;
        files.push((root.join(id), front, body));
    }
    let semantics: std::collections::BTreeMap<String, String> = files
        .iter()
        .map(|(_, front, body)| (front.id.clone(), granted_semantics(front, body)))
        .collect();
    if let Some(expected) = expected_semantics {
        if *expected != semantics {
            return Err(Error::invalid(
                "unattended_diverged",
                "the recorded intent's semantics no longer match what the bound \
                 render produces — the admission is held for reconciliation",
            ));
        }
    }
    // Disk classification under the same lock the write will use.
    let mut present = 0usize;
    let mut identical = true;
    for (dir, front, _) in &files {
        if !dir.join("issue.md").is_file() {
            continue;
        }
        present += 1;
        let loaded = board::load_issue(&pm.dir, &project.key, &front.id).map_err(|e| {
            Error::invalid(
                "unattended_diverged",
                format!("existing {} is unreadable ({e})", front.id),
            )
        })?;
        let loaded_semantics = granted_semantics(&loaded.front, &loaded.body);
        if semantics.get(&front.id) != Some(&loaded_semantics) {
            identical = false;
        }
    }
    // Identical files on disk are NOT commit proof: a torn admission can
    // leave the exact intended content uncommitted (hook refused,
    // process died) or structurally invalid. The reused path therefore
    // still revalidates structure and runs the scoped commit — a clean
    // tree makes it a no-op, an uncommitted or unhooked state commits
    // or fails honestly (recovery then marks the intent held).
    let reused = present == files.len() && identical;
    if present > 0 && !reused {
        return Err(Error::invalid(
            "unattended_diverged",
            format!(
                "{present} of {} reserved ids exist with different content — the \
                 partial admission is held, never overwritten or re-allocated",
                files.len()
            ),
        ));
    }
    if !allow_write && !reused {
        return Err(Error::invalid(
            "unattended_missing",
            "the admitted run's files are gone — held for reconciliation",
        ));
    }
    // Reserve the allocated ids' directories FIRST: `next_id` treats an
    // existing `<prefix>-N` dir as allocated and `new_issue` refuses an
    // existing dir, so a torn admission can never have its ids silently
    // consumed by an unrelated issue or plan — every allocator honors
    // directory existence under this same lock. Directories are resource
    // reservations only; the durable intent stays the authority. Once
    // any dir exists it is NEVER deleted here: a reservation error is
    // still an uncertain outcome (on recovery, a native intent already
    // binds these ids), and only explicit reconciliation may free them.
    for (dir, _, _) in &files {
        std::fs::create_dir_all(dir)?;
    }
    // Then the pre-intent callback — daemon-side: revalidate live grant
    // bounds under this same lock and record the durable intent BEFORE
    // the first issue.md exists. On an error the reservations STAY: the
    // callback may already have persisted the intent (or, on recovery,
    // a native mark already binds these ids), and `new_issue` does not
    // read store reservations — only the directories stop it stealing
    // these ids. An un-bindable dir is a numbering hole, never a stolen
    // id; no logical tracker content is written.
    on_intent(&GrantedPreview {
        epic: epic.clone(),
        tickets: ids[1..].to_vec(),
        semantics,
    })?;
    let build = || -> Result<Vec<String>> {
        // Reused files are byte-identical — leave them untouched (the
        // commit below still proves admission or fails); only a real
        // write creates the folders' contents.
        if !reused {
            for (dir, front, body) in &files {
                std::fs::create_dir_all(dir.join("comments"))?;
                std::fs::create_dir_all(dir.join("artifacts"))?;
                save_front(dir, front, body)?;
            }
        }
        let issues = board::load_all(&pm.dir, None)?;
        for id in &ids {
            check_structure(&issues, id)?;
        }
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let paths: Vec<PathBuf> = files
            .iter()
            .map(|(dir, _, _)| dir.join("issue.md"))
            .collect();
        commit_who_scoped(
            pm,
            &lock,
            &paths,
            &format!(
                "{epic}: plan admitted by {decided_by} ({} tickets ready)",
                doc.tickets.len()
            ),
            &refs,
            actor,
            Some(decided_by),
        )
    };
    let foreign = match build() {
        Ok(foreign) => foreign,
        Err(e) => {
            // The durable intent may already bind these ids — the
            // reservation directories (and any partial issue.md
            // evidence) stay: removing them would let `new_issue` steal
            // the ids, which reads only directory existence. Recovery
            // classifies what is on disk — absent, identical, diverged —
            // and holds on anything unknown.
            return Err(e);
        }
    };
    // `reused`: report the epic's recorded fields — the plan on disk
    // admitted this run, not this call's scratch values.
    let (proposed_by, decided_at) = if reused {
        let epic_issue = board::load_issue(&pm.dir, &project.key, &epic)?;
        let plan = epic_issue.front.plan.clone();
        (
            plan.as_ref().map(|p| p.proposed_by.clone()),
            plan.and_then(|p| p.decided_at),
        )
    } else {
        (Some(proposer.clone()), Some(now.clone()))
    };
    let mut out = json!({
        "epic": epic,
        "project": project.key,
        "title": doc.title,
        "state": "approved",
        "proposed_by": proposed_by,
        "decided_by": decided_by,
        "decided_at": decided_at,
        "tickets": ids[1..],
        "ready": ids[1..],
        "committed": true,
        "reused": reused,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// CAD-360 `plan approve` / `plan reject` — record the operator's
/// decision on a `proposed` plan (state, who, when, why) and, on
/// approval, move its `backlog` tickets to `ready` — one commit. The
/// caller has already proven operator authority (the daemon's
/// connection-bound check); this only writes.
pub fn decide_plan(
    pm: &Pm,
    epic: &str,
    approve: bool,
    by: &str,
    reason: Option<&str>,
) -> Result<Value> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if !approve && reason.is_none() {
        return Err(Error::rejected("plan reject needs --reason"));
    }
    if let Some(reason) = reason {
        crate::secret::guard(&format!("{epic}: plan reason"), reason)?;
    }
    let (project, dir) = issue_dir(pm, epic)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&pm.dir.join(&project.key)))?;
    let (mut front, body) = load_front(&dir)?;
    let Some(plan) = front.plan.as_mut() else {
        return Err(Error::rejected(format!(
            "{epic} is not a plan — `cadence plan show` needs a proposed plan"
        )));
    };
    if plan.state != "proposed" {
        return Err(Error::rejected(format!(
            "plan {epic} is already {} — only a proposed plan is decided",
            plan.state
        )));
    }
    let state = if approve { "approved" } else { "rejected" };
    plan.state = state.to_string();
    plan.decided_by = Some(by.to_string());
    plan.decided_at = Some(time::iso(time::now_epoch()));
    plan.reason = reason.map(str::to_string);
    let decided = plan.clone();
    let mut ready = vec![];
    let mut writes = vec![(dir.clone(), front.clone(), body)];
    if approve {
        for kid in board::load_all(&pm.dir, Some(&project.key))? {
            if decided.tickets.contains(&kid.front.id) && kid.front.status == "backlog" {
                let mut f = kid.front.clone();
                f.status = "ready".to_string();
                ready.push(f.id.clone());
                writes.push((kid.dir.clone(), f, kid.body.clone()));
            }
        }
    }
    // All or nothing: a failed write or commit restores every file.
    let originals: Vec<(PathBuf, String)> = writes
        .iter()
        .map(|(dir, _, _)| {
            let file = dir.join("issue.md");
            std::fs::read_to_string(&file).map(|text| (file, text))
        })
        .collect::<std::io::Result<_>>()?;
    let restore = || {
        for (file, text) in &originals {
            let _ = atomic_write(file, text);
        }
    };
    let mut ids: Vec<&str> = vec![epic];
    ids.extend(ready.iter().map(String::as_str));
    let subject = if approve {
        format!(
            "{epic}: plan approved by {by} ({} tickets ready)",
            ready.len()
        )
    } else {
        format!("{epic}: plan rejected by {by}")
    };
    let paths: Vec<PathBuf> = writes
        .iter()
        .map(|(dir, _, _)| dir.join("issue.md"))
        .collect();
    let written = writes
        .iter()
        .try_for_each(|(dir, front, body)| save_front(dir, front, body))
        .and_then(|_| commit_who_scoped(pm, &lock, &paths, &subject, &ids, "", Some(by)));
    let foreign = match written {
        Ok(f) => f,
        Err(e) => {
            restore();
            return Err(e);
        }
    };
    let mut out = json!({
        "epic": epic,
        "project": project.key,
        "state": state,
        "decided_by": decided.decided_by,
        "decided_at": decided.decided_at,
        "reason": decided.reason,
        "ready": ready,
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// Reject shapes lint would flag, at write time: self-links, missing
/// targets, parent cycles and depth > 2, blocked_by cycles.
pub(crate) fn check_structure(issues: &[board::Issue], id: &str) -> Result<()> {
    let by_id: HashMap<&str, &board::Issue> =
        issues.iter().map(|i| (i.front.id.as_str(), i)).collect();
    let this = by_id
        .get(id)
        .ok_or_else(|| Error::internal("new issue missing from reload"))?;
    for dep in &this.front.blocked_by {
        if dep == &this.front.id {
            return Err(Error::rejected(format!("{id} cannot block itself")));
        }
        if !by_id.contains_key(dep.as_str()) {
            return Err(Error::rejected(format!(
                "blocked_by target '{dep}' does not exist — \
                 `cadence issue new \"title\"` creates it first"
            )));
        }
    }
    // blocked_by cycle: walk targets; a return to id is a cycle.
    let mut seen = HashSet::new();
    let mut stack: Vec<&str> = this.front.blocked_by.iter().map(String::as_str).collect();
    while let Some(cur) = stack.pop() {
        if cur == id {
            return Err(Error::rejected(format!(
                "blocked_by would create a cycle through {id} — \
                 reorder the dependency instead"
            )));
        }
        if seen.insert(cur) {
            if let Some(node) = by_id.get(cur) {
                stack.extend(node.front.blocked_by.iter().map(String::as_str));
            }
        }
    }
    check_parent(&by_id, this)
}

/// Parent rules: target exists, not self, no cycle, depth stays ≤ 2.
pub(super) fn check_parent(
    by_id: &HashMap<&str, &board::Issue>,
    this: &board::Issue,
) -> Result<()> {
    let Some(parent) = &this.front.parent else {
        return Ok(());
    };
    let id = this.front.id.as_str();
    if parent == id {
        return Err(Error::rejected(format!("{id} cannot be its own parent")));
    }
    let Some(parent_issue) = by_id.get(parent.as_str()) else {
        return Err(Error::rejected(format!(
            "parent '{parent}' does not exist — create it with \
             `cadence issue new` first"
        )));
    };
    if parent_issue.front.parent.is_some() {
        return Err(Error::rejected(format!(
            "{parent} is itself a sub-issue — depth is two levels; \
             parent {id} under a root issue instead"
        )));
    }
    if by_id
        .values()
        .any(|i| i.front.parent.as_deref() == Some(id))
    {
        return Err(Error::rejected(format!(
            "{id} has children — making it a sub-issue would exceed depth 2"
        )));
    }
    // Cycle guard for hand-edited files (writes can never make one).
    let mut cur = parent_issue;
    while let Some(up) = &cur.front.parent {
        if up == id {
            return Err(Error::rejected(format!(
                "parent link would cycle through {id}"
            )));
        }
        match by_id.get(up.as_str()) {
            Some(next) => cur = next,
            None => break,
        }
    }
    Ok(())
}
