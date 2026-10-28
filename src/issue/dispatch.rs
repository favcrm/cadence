//! `cadence dispatch <ISSUE>` — the one-step dispatch: `issue start`
//! (idempotent) then exactly one templated kickoff message to the
//! worker, a tracker comment and a `message` ref on the issue. With
//! `--job` the kickoff goes through `job dispatch` instead, so the
//! task and the message are bound. Everything is validated before
//! anything is created — a refused dispatch leaves no worktree,
//! branch, commit or queued message behind.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::adapter::pty;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::model::Front;
use crate::issue::parse::AcceptanceItem;
use crate::issue::{board, claim, parse, start, write, Pm};
use crate::memory;
use crate::store;

/// Message states that mean a dispatch is still in flight — a second
/// dispatch must not queue a duplicate while one is live.
const LIVE_MESSAGE_STATES: &[&str] = &["queued", "submitting", "running"];

/// Automatic provenance is authenticated at the daemon's final send.
/// Explicit dispatch and repair retain the connection-bound caller's authority.
pub enum DispatchOrigin {
    Automatic,
    Explicit,
}

pub struct DispatchArgs {
    pub to: String,
    /// The kickoff note; `None` points the worker at the ticket's own
    /// `issue.md` (CAD-339 — a plan ticket already carries its brief).
    pub note: Option<PathBuf>,
    pub name: Option<String>,
    pub base: Option<String>,
    pub repo: Option<PathBuf>,
    pub reply_to: Option<String>,
    pub summary: Option<String>,
    /// `--job` — the spec file `job_new` hashes; the kickoff then goes
    /// through `task_dispatch` rather than `agent_send`.
    pub job_spec: Option<PathBuf>,
    /// `--no-lessons` — skip project-memory injection for this send.
    pub no_lessons: bool,
    /// `--force` — dispatch even when the worker's pty pane cwd is
    /// outside the project's repos (CAD-202); recorded on the issue.
    pub force: bool,
    /// CAD-383 `--take-over <reason>` — dispatch an issue another PM or
    /// lane holds in doing/review; recorded on the issue.
    pub take_over: Option<String>,
}

/// The fixed single-line kickoff body — note path, issue id, summary
/// or title, the worktree/branch/base, the trailer and the return
/// address. Built verbatim: a title or `--summary` carrying control
/// chars produces a violating body and `check_body` refuses rather
/// than laundering it into something the operator didn't write.
///
/// CAD-468: an explicitly-reported endpoint's kickoff also carries the
/// exact `cadence message result` command — the worker that missed or
/// never read its briefing still has the contract in the message that
/// opened its turn. Turn-result endpoints finish by their result text
/// and never run it, so theirs is omitted.
#[allow(clippy::too_many_arguments)]
fn kickoff_body(
    issue: &str,
    title: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
    provider: &str,
    endpoint_kind: &str,
) -> String {
    let sha7: String = base_sha.chars().take(7).collect();
    let report = if crate::adapter::registry::report_hint(provider, endpoint_kind)
        == crate::adapter::registry::Reporting::Explicit
    {
        " Report: `cadence message result <id> --token <turn_id> --text '<summary>'` \
         (`cadence self` prints both)."
    } else {
        ""
    };
    format!(
        "read {} — {issue}: {title}. Your worktree exists: {} (branch \
         {branch}, base {sha7}). Commit trailer: Issue: {issue}. PR to \
         main; reply to {reply_to}. Company questions: read the \
         \"Business knowledge\" section of \
         $HOME/.agents/skills/cadence/SKILL.md.{report} For implementation completion, \
         file `cadence report file --task {issue} --kind done --file <f>` with frontmatter \
         `sha: <full committed head>` and `pr: <PR URL>`. End your returned result \
         (or explicit message result text) with a final `SHA: <same full head>` line.",
        note.display(),
        wt_dir.display()
    )
}

/// CAD-159: an issue's acceptance items on one line; `None` when there
/// are none. Always the whole list — CAD-160 removed the "N items, too
/// long to inline" pointer, since criteria are never dropped.
///
/// CAD-300: each item is numbered and its text is a JSON string
/// literal — `1) [ ] "a"; 2) [x] "b"` — so text that looks like the
/// listing's own syntax (`; `, `[x]`, `2)`) stays inside its quotes
/// and cannot read as another item or a checked box.
/// [`parse_acceptance_listing`] reads it back exactly. Quoting escapes
/// every control character and U+2028/U+2029, so the result can ride a
/// single-line pty kickoff without losing them.
pub(crate) fn acceptance_listing(items: &[AcceptanceItem]) -> Option<String> {
    (!items.is_empty()).then(|| inline_listing(items))
}

/// The CAD-300 listing of `items` at any length —
/// `1) [ ] "a"; 2) [x] "b"`, numbered from 1.
pub(crate) fn inline_listing(items: &[AcceptanceItem]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mark = if item.checked { 'x' } else { ' ' };
            format!("{}) [{mark}] {}", i + 1, quote_item(&item.text))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// CAD-160: the still-unchecked criteria in a task's stored
/// acceptance. A whole CAD-300 listing yields its `[ ]` items; any
/// other non-blank text (a free-form `--accept`) is one unchecked item.
pub(crate) fn outstanding_items(acceptance: Option<&str>) -> Vec<AcceptanceItem> {
    let Some(text) = acceptance.map(str::trim).filter(|t| !t.is_empty()) else {
        return vec![];
    };
    match parse_acceptance_listing(text) {
        Some((items, "")) => items.into_iter().filter(|i| !i.checked).collect(),
        _ => vec![AcceptanceItem {
            text: text.to_string(),
            checked: false,
        }],
    }
}

/// `text` as a JSON string literal that is also safe on one pty line:
/// `"` and `\` are escaped as JSON requires, tab as `\t`, and every
/// other control character (C0, DEL, C1) and the line/paragraph
/// separators U+2028/U+2029 as `\uXXXX`. All of those are in the BMP,
/// so four hex digits always suffice.
fn quote_item(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// CAD-300: read an [`acceptance_listing`] back — the numbered,
/// JSON-quoted items at the start of `s` — returning them and the rest
/// of `s` (the kickoff's `.` onwards). `None` when `s` does not start
/// with item `1)`, or an item is malformed: a numbering gap, a checkbox
/// other than `[ ]`/`[x]`, or text that is not a JSON string literal.
pub fn parse_acceptance_listing(s: &str) -> Option<(Vec<AcceptanceItem>, &str)> {
    let mut items = vec![];
    let mut rest = s;
    loop {
        rest = rest.strip_prefix(&format!("{}) ", items.len() + 1))?;
        let checked = match rest.get(..4)? {
            "[x] " => true,
            "[ ] " => false,
            _ => return None,
        };
        rest = &rest[4..];
        if !rest.starts_with('"') {
            return None;
        }
        let mut quoted = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let text = quoted.next()?.ok()?;
        rest = &rest[quoted.byte_offset()..];
        items.push(AcceptanceItem { text, checked });
        match rest.strip_prefix("; ") {
            Some(next) if next.starts_with(&format!("{}) ", items.len() + 1)) => rest = next,
            _ => return Some((items, rest)),
        }
    }
}

/// CAD-159: the warning for an issue with no acceptance items. The PM
/// decided (2026-09-23, ADR-0002 §8.3) to warn and still dispatch
/// until live issues are backfilled; refusal is a later ticket.
fn acceptance_warning(issue: &str, dispatched: bool) -> String {
    let outcome = if dispatched {
        "Dispatched anyway."
    } else {
        "Not re-sent: this issue's kickoff is already in flight."
    };
    format!(
        "{issue} has no acceptance criteria (its ## Acceptance section has no \
         checklist items; a bare `- [ ]` does not count) — add them with \
         `cadence issue acceptance {issue} --from <file>`. {outcome}"
    )
}

/// The plain-path kickoff with every one of the issue's acceptance
/// items appended as ` Acceptance: <listing>.` — never a pointer, never
/// cut (CAD-160). [`plain_kickoff`] fits the result to the pty ceiling.
fn with_acceptance(body: String, items: &[AcceptanceItem]) -> String {
    match acceptance_listing(items) {
        Some(listing) => format!("{body} Acceptance: {listing}."),
        None => body,
    }
}

/// CAD-160: the plain kickoff fitted to the pty ceiling. Prose gives
/// way — the title (or `--summary`) is shortened and ends in `…` — and
/// the acceptance items never do. When they cannot fit even with the
/// title cut to nothing, the dispatch refuses naming the ceiling and
/// the note; it runs before `issue start`, so nothing is created.
#[allow(clippy::too_many_arguments)]
fn plain_kickoff(
    issue: &str,
    title: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
    items: &[AcceptanceItem],
    provider: &str,
    endpoint_kind: &str,
) -> Result<String> {
    const CUT: &str = "…";
    let build = |title: &str| {
        with_acceptance(
            kickoff_body(
                issue,
                title,
                note,
                wt_dir,
                branch,
                base_sha,
                reply_to,
                provider,
                endpoint_kind,
            ),
            items,
        )
    };
    let full = build(title);
    let over = full.len().saturating_sub(pty::MAX_BODY);
    if over == 0 {
        return Ok(full);
    }
    if let Some(keep) = title.len().checked_sub(over + CUT.len()) {
        let mut end = keep;
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        return Ok(build(&format!("{}{CUT}", &title[..end])));
    }
    let criteria = acceptance_listing(items).unwrap_or_default();
    Err(Error::rejected(format!(
        "{issue}'s acceptance criteria ({} bytes) do not fit the {}-char pty kickoff \
         ceiling even with the title cut — criteria are never truncated or dropped \
         (CAD-160). Shorten them (`cadence issue acceptance {issue} --from <file>`) \
         and keep the detail in the note {}. Nothing was created or queued.",
        criteria.len(),
        pty::MAX_BODY,
        note.display()
    )))
}

/// The plain kickoff with every acceptance item appended, checked against
/// the pty rules. Built by the caller for the legacy opt-out path, or by
/// the context guard's lazy factory for an opted-in one — never both.
#[allow(clippy::too_many_arguments)]
fn plain_body(
    front: &Front,
    summary: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
    items: &[AcceptanceItem],
    provider: &str,
    endpoint_kind: &str,
) -> Result<String> {
    let body = plain_kickoff(
        &front.id,
        summary,
        note,
        wt_dir,
        branch,
        base_sha,
        reply_to,
        items,
        provider,
        endpoint_kind,
    )?;
    check_body(&body, provider)?;
    Ok(body)
}

/// The placeholder job/task `issue start --job` is about to create, with
/// the exact spec path, scope, criteria and id lengths the daemon mints
/// (`job-` + 8 hex, `-t1`, a 32-hex message id). Both the legacy kickoff
/// size check and the pre-admission context guard use it.
fn job_prototype(
    issue: &str,
    items: &[AcceptanceItem],
    spec: &Path,
    wt_name: &str,
    branch: &str,
    base_sha: &str,
) -> (store::Job, store::Task) {
    // `issue start --job` records the canonical spec path on the job.
    let spec = spec.canonicalize().unwrap_or_else(|_| spec.to_path_buf());
    let job_id = "job-00000000";
    let job = store::Job {
        id: job_id.to_string(),
        title: None,
        spec_path: spec.to_string_lossy().into_owned(),
        spec_sha256: None,
        pm_alias: String::new(),
        issue_id: Some(issue.to_string()),
        repo: None,
        base_ref: None,
        state: "open".to_string(),
        max_revisions: 0,
        stall_secs: None,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    let task = store::Task {
        id: format!("{job_id}-t1"),
        job_id: job_id.to_string(),
        title: None,
        role: "worker".to_string(),
        assignee: None,
        spec_path: None,
        acceptance: acceptance_listing(items),
        worktree: Some(wt_name.to_string()),
        branch: Some(branch.to_string()),
        base_sha: Some(base_sha.to_string()),
        head_sha: None,
        state: "draft".to_string(),
        revision: 0,
        dispatch_message: None,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    (job, task)
}

/// CAD-160: a `--job` dispatch whose kickoff cannot carry its
/// acceptance items whole is refused before `issue start` creates a
/// worktree, branch or job. The check builds the legacy kickoff `job
/// dispatch` will send — same spec path, scope, criteria and report
/// contract, for the assignee's own endpoint and ceiling (48000 for a
/// Devin cloud session) — with placeholders the length of what the daemon
/// mints. `agent` is the assignee's `agent_show` row. An opted-in
/// kickoff's exact body and combined cap are checked by [`job_context`]
/// instead.
fn check_job_kickoff(
    issue: &str,
    items: &[AcceptanceItem],
    spec: &Path,
    wt_name: &str,
    branch: &str,
    base_sha: &str,
    agent: &Value,
) -> Result<()> {
    let (job, task) = job_prototype(issue, items, spec, wt_name, branch, base_sha);
    let provider = agent["provider"].as_str().unwrap_or_default();
    let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
    // Legacy preview: no pinned context, so the acceptance rides the body.
    let Err(_) = store::job_kickoff(&job, &task, 1, &"0".repeat(32), provider, kind, false) else {
        return Ok(());
    };
    Err(Error::rejected(format!(
        "{issue}'s acceptance criteria ({} bytes) do not fit the {}-char kickoff ceiling \
         beside its fixed fields — criteria are never truncated or dropped (CAD-160). \
         Shorten them (`cadence issue acceptance {issue} --from <file>`) and keep the \
         detail in the spec file {}. Nothing was created or queued.",
        acceptance_listing(items).unwrap_or_default().len(),
        store::kickoff_ceiling(provider, kind),
        job.spec_path,
    )))
}

/// The same rules the pty endpoint enforces pre-write: 1–4000 chars,
/// no control characters (single line), and no leading character the
/// provider's TUI treats as a command. Checked here so a bad body
/// refuses before `issue start` creates anything.
fn check_body(body: &str, provider: &str) -> Result<()> {
    if body.is_empty() || body.len() > pty::MAX_BODY {
        return Err(Error::rejected("Dispatch body must be 1–4000 characters"));
    }
    if pty::has_control_chars(body) {
        return Err(Error::rejected(
            "Dispatch body must be a single line without control characters",
        ));
    }
    if let Some(prefix) = body.trim_start().chars().next() {
        if pty::forbidden_prefixes(provider).contains(&prefix) {
            return Err(Error::rejected(format!(
                "Dispatch body starts with '{prefix}', which {provider} \
                 treats as a command or mode switch — refusing to send it"
            )));
        }
    }
    Ok(())
}

/// CAD-202: a pty worker's live pane cwd must be a directory that
/// still exists and lies inside one of the issue project's repos (a
/// linked worktree under `<repo>/.cadence/wt/` counts) — otherwise the
/// issue's work would run nowhere, or in another project. Reads the
/// `pane_cwd` fact `agent show` reports; a worker with no live pane
/// (or a non-pty endpoint) is not checked here. A deleted cwd always
/// refuses (`cwd_deleted` — delivery would refuse anyway); a foreign
/// one refuses (`cwd_outside_project`) unless `force`, which returns
/// the override note the caller records.
pub(crate) fn check_lane_cwd(
    project: &crate::issue::project::Project,
    worker: &str,
    agent: &Value,
    force: bool,
) -> Result<Option<String>> {
    if agent["endpoint_kind"].as_str() != Some("pty") {
        return Ok(None);
    }
    let Some(path) = agent["pane_cwd"]["path"].as_str() else {
        return Ok(None);
    };
    if agent["pane_cwd"]["deleted"].as_bool() == Some(true) {
        return Err(Error::invalid(
            "cwd_deleted",
            format!(
                "cwd_deleted: worker '{worker}' has a deleted working directory ({path}) — its \
                 pane cannot run this work; re-home the lane (`cadence agent stop \
                 {worker}`, fix its cwd, resume) before dispatching"
            ),
        ));
    }
    let repos = start::declared_repos(project);
    if repos.is_empty() {
        return Ok(None);
    }
    let cwd = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path));
    if repos.iter().any(|repo| cwd.starts_with(repo)) {
        return Ok(None);
    }
    let listed = repos
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if !force {
        return Err(Error::invalid(
            "cwd_outside_project",
            format!(
                "cwd_outside_project: worker '{worker}' pane cwd {} is outside every repo of project \
                 {} ({listed}) — its work would run in the wrong repo; re-home the \
                 lane, or pass --force to dispatch anyway (recorded on the issue)",
                cwd.display(),
                project.key
            ),
        ));
    }
    Ok(Some(format!(
        "Dispatch override (--force, cwd_outside_project): {worker}'s pane cwd {} \
         is outside every repo of project {} ({listed})",
        cwd.display(),
        project.key
    )))
}

/// The lane worktree's current HEAD — best-effort evidence carried on
/// a reported-duplicate record; `None` when git cannot answer (a lane
/// deleted mid-dispatch reads as unknown, never as a match).
fn lane_head(worktree: &Path) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(worktree).args(["rev-parse", "HEAD"]);
    let out = crate::proc::run_bounded(&mut cmd, Duration::from_secs(30)).ok()?;
    (out.status.success())
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Lesson facts the pinned context carries, read back from the manifest it
/// produced. The legacy `<message>-lessons.md` sidecar is not written when
/// context is active, so the dispatch comment and its JSON report the same
/// selection from the pinned record instead of a second memory match.
struct ContextFacts {
    lessons: Vec<String>,
    lessons_withheld: Vec<(String, String)>,
    lessons_omitted: Vec<(String, String)>,
    lessons_error: Option<String>,
}

impl ContextFacts {
    fn from_manifest(manifest: &crate::context_pack::Manifest) -> Self {
        let mut lessons = Vec::new();
        for document in &manifest.documents {
            if document.kind == LESSON_KIND {
                lessons.extend(document.lesson_ids.iter().cloned());
            }
        }
        let mut lessons_withheld = Vec::new();
        let mut lessons_omitted = Vec::new();
        let mut lessons_error = None;
        for gap in &manifest.withheld {
            if gap.path != LESSON_PATH {
                continue;
            }
            // The aggregate gap reason keeps the complete count; bounded
            // detail names the exact lessons with their own reasons. A pin
            // written before detail existed still reports its aggregate fact.
            let detail = |notes: &[crate::context_pack::LessonNote]| {
                notes
                    .iter()
                    .map(|note| (note.id.clone(), note.reason.clone()))
                    .collect::<Vec<_>>()
            };
            match gap.state.as_str() {
                "withheld" => {
                    // The aggregate fact keeps the complete count and the
                    // truncation disclosure; bounded detail names the
                    // individual lessons after it. A pre-detail pin has an
                    // empty list, so the extend is a no-op there.
                    lessons_withheld.push((gap.id.clone(), gap.reason.clone()));
                    lessons_withheld.extend(detail(&gap.lessons));
                }
                "overflow" if gap.id == LESSON_OMITTED_ID => {
                    lessons_omitted.push((gap.id.clone(), gap.reason.clone()));
                    lessons_omitted.extend(detail(&gap.lessons));
                }
                "unreadable" | "unavailable" => {
                    // A pin written before the redaction boundary can still
                    // carry a raw load-error line; redact before reporting.
                    lessons_error = Some(redacted_fact(&gap.reason));
                }
                _ => {}
            }
        }
        ContextFacts {
            lessons,
            lessons_withheld,
            lessons_omitted,
            lessons_error,
        }
    }
}

/// `dispatch <ISSUE> --to <worker> --note <path> [--job --spec f]`.
///
/// `dispatch_pm`: `Some(alias)` when the daemon itself dispatches
/// (master_dispatch) — it writes the dispatch record directly, pm =
/// its own alias; `None` for the CLI, whose `dispatch_record` RPC
/// derives the pm from the caller's connection.
///
/// Native automatic callers preserve their origin through the self-RPC so
/// live authority is checked after blocking setup, at the final enqueue gate.
struct Preflight {
    project: crate::issue::project::Project,
    dir: PathBuf,
    front: Front,
    items: Vec<AcceptanceItem>,
    acceptance: Value,
    acceptance_warning: Option<String>,
    reply_to: String,
    note: PathBuf,
    show: Value,
    provider: String,
    endpoint_kind: String,
    cwd_override: Option<String>,
    body: Option<String>,
    /// CAD-1822: this plain dispatch's assigned project is opted in to task
    /// context delivery. Only then may a changed kickoff body supersede a
    /// reported duplicate; non-opted projects keep the legacy duplicate
    /// behavior.
    context_active: bool,
    /// Stable actionable plain contract, excluding tracker bookkeeping.
    context_contract_digest: Option<String>,
    /// CAD-1846: the lesson facts the pinned context carries for the plain
    /// kickoff. Present exactly when this dispatch's own plain context was
    /// prepared, so the legacy lesson sidecar is skipped instead of injected
    /// twice; a `--job` kickoff carries its selection in the task pin.
    context_lessons: Option<ContextFacts>,
    /// CAD-1846: the issue's open native job whose default task was created
    /// but never accepted, verified against every material scope/spec input.
    /// `start` reuses this binding instead of minting another job; the store's
    /// own idempotency and one-open-job guard still decide.
    reuse_job: Option<String>,
}

/// The one pre-dispatch gate every caller shares: resolve the issue, read
/// it, require acceptance when the caller does, and apply the plan gate.
/// Authority, claim and target checks stay with the caller — this only
/// removes the duplicated load/acceptance/plan sequence.
#[derive(Clone)]
pub(crate) struct GatedIssue {
    pub project: crate::issue::project::Project,
    pub dir: PathBuf,
    pub front: Front,
    pub items: Vec<AcceptanceItem>,
    /// Exact source from which `front` and `items` were parsed.
    pub source: String,
    /// Complete canonical prose from that same parse, without criteria copies.
    pub request: String,
}

/// One issue read: the project and folder that hold it, its frontmatter, its
/// complete body and the exact `issue.md` source. Callers apply their own
/// policy to this one snapshot, so a gate and a context capture never
/// disagree about what the issue said.
struct IssueSnapshot {
    project: crate::issue::project::Project,
    dir: PathBuf,
    front: Front,
    body: String,
    source: String,
}

impl IssueSnapshot {
    /// The same lookup rule as `write::issue_dir`: `None` only when no
    /// project holds a real folder with a real `issue.md` for `id`. A
    /// tracker that cannot be listed, or a held issue that cannot be read or
    /// parsed, is an error — never "not ours".
    fn read(pm: &Pm, id: &str) -> Result<Option<Self>> {
        crate::issue::model::check_id(id)?;
        for project in crate::issue::project::list(&pm.dir)? {
            let dir = pm.dir.join(&project.key).join(id);
            if !board::is_real_dir(&dir) || !board::is_real_file(&dir.join("issue.md")) {
                continue;
            }
            let source = std::fs::read_to_string(dir.join("issue.md"))?;
            let (front, body) = parse::parse_issue(&source)?;
            return Ok(Some(IssueSnapshot {
                project,
                dir,
                front,
                body,
                source,
            }));
        }
        Ok(None)
    }

    /// The dispatch snapshot from this one read. `gate` adds the plain
    /// dispatch plan policy; `require_acceptance` adds the plain kickoff's
    /// acceptance requirement. A context capture opts out of both.
    fn into_gated(
        self,
        pm_dir: &Path,
        id: &str,
        gate: bool,
        require_acceptance: bool,
    ) -> Result<GatedIssue> {
        let IssueSnapshot {
            project,
            dir,
            front,
            body,
            source,
        } = self;
        if gate {
            crate::issue::plan::gate(pm_dir, &front, &body)?;
        }
        let (request, items) = parse::task_request(body);
        if require_acceptance && items.is_empty() {
            return Err(Error::rejected(format!(
                "{id} has no acceptance criteria — add them with \
                 `cadence issue acceptance {id} --from <file>` before kickoff"
            )));
        }
        Ok(GatedIssue {
            project,
            dir,
            front,
            items,
            source,
            request,
        })
    }
}

/// CAD-1846: the one-read parent capture a native job's context needs, with
/// no plain-dispatch policy of its own. `None` means no project holds an
/// issue of that id — a job may legitimately name one (`plan_gate_task`
/// allows it: jobs need not name a tracker issue), and such a job keeps its
/// legacy kickoff instead of gaining a new refusal merely because context
/// capture was attempted. A held issue that cannot be read, and a tracker
/// that cannot be listed, still fail closed.
pub(crate) fn capture_issue(pm: &Pm, id: &str) -> Result<Option<GatedIssue>> {
    let Some(snapshot) = IssueSnapshot::read(pm, id)? else {
        return Ok(None);
    };
    snapshot.into_gated(&pm.dir, id, false, false).map(Some)
}

/// The one plain pre-dispatch gate every caller shares: resolve the issue,
/// read it, apply the plan gate and require acceptance when the caller does.
/// Authority, claim and target checks stay with the caller — this only
/// removes the duplicated load/acceptance/plan sequence.
pub(crate) fn gate_issue(pm: &Pm, id: &str, require_acceptance: bool) -> Result<GatedIssue> {
    let Some(snapshot) = IssueSnapshot::read(pm, id)? else {
        return Err(Error::rejected(format!(
            "Unknown issue '{id}' — `cadence issue ls` lists what exists"
        )));
    };
    snapshot.into_gated(&pm.dir, id, true, require_acceptance)
}

/// The worker-only checks every dispatch runs before it creates a lane:
/// the agent exists and is not fenced, its pane is inside this project,
/// and a confined pi worker is not on a Devin model. Split out so a
/// caller that runs `issue start` itself (the board kickoff) can refuse
/// before the first mutation.
pub(crate) struct SeatCheck {
    pub show: Value,
    pub agent: Value,
    pub cwd_override: Option<String>,
    pub provider: String,
    pub endpoint_kind: String,
}

pub(crate) fn check_seat(
    project: &crate::issue::project::Project,
    alias: &str,
    force: bool,
    state_dir: &Path,
) -> Result<SeatCheck> {
    let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
    let agent = show["agent"].clone();
    if agent["state"].as_str() == Some("attention") {
        return Err(Error::rejected(format!(
            "Worker '{alias}' is fenced ({}) — reconcile it before dispatching",
            agent["error"].as_str().unwrap_or("attention")
        )));
    }
    if agent["enabled"].as_bool() == Some(false) {
        return Err(Error::rejected(format!(
            "Worker '{alias}' is disabled — resume it before dispatching"
        )));
    }
    // CAD-202: the lane's pane must still be inside this project.
    let cwd_override = check_lane_cwd(project, alias, &agent, force)?;
    let provider = agent["provider"].as_str().unwrap_or_default().to_string();
    let endpoint_kind = agent["endpoint_kind"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if crate::adapter::registry::pi_family_managed(&provider, &endpoint_kind) {
        crate::adapter::pi::refuse_confined_devin(
            alias,
            agent["params"]["confine"].as_bool() == Some(true),
            agent["params"]["model"].as_str().unwrap_or_default(),
        )?;
    }
    Ok(SeatCheck {
        show,
        agent,
        cwd_override,
        provider,
        endpoint_kind,
    })
}

/// The runtime facts that pick a task's context role and delivery
/// envelope. They come from the actual assigned worker row, never from a
/// request field, a project default or a guessed model.
pub(crate) struct ContextTarget<'a> {
    pub team_role: Option<&'a str>,
    pub provider: &'a str,
    pub endpoint_kind: &'a str,
}

/// A validated opted-in task context: the manifest the admission
/// transaction must pin, and the exact body that turn delivers.
pub(crate) struct PreparedContext<'a> {
    pub manifest: crate::context_pack::Manifest,
    pub body: std::borrow::Cow<'a, str>,
}

/// One job task's dispatch snapshot: the store-captured rows, the
/// proposed dispatch revision and message id, the actual assignee's
/// runtime facts and — for a native admission — the caller's proof of the
/// issue lane the job actually holds.
pub(crate) struct JobContext<'a> {
    pub job: &'a store::Job,
    pub task: &'a store::Task,
    pub revision: i64,
    pub message: &'a str,
    pub target: ContextTarget<'a>,
    /// The native caller's scope proof for `(project, front)`: the exact
    /// issue lane, the job's declared repo and the task's lane must agree
    /// before any spec byte enters the contract. `None` is a preview (the
    /// preflight of a dispatch whose lane does not exist yet) and is never
    /// an admission; no fake proof stands in for it.
    pub admission_scope: Option<AdmissionScope<'a>>,
}

/// The native caller's issue-lane proof, run once inside the guard's lazy
/// factory — after opt-in and endpoint confirmation, before any spec read.
pub(crate) type AdmissionScope<'a> =
    &'a dyn Fn(&crate::issue::project::Project, &Front) -> crate::error::Result<()>;

/// Synthetic identity of the optional lesson source and its aggregate gap
/// facts. Nothing is read from this path: it names the source family in a
/// manifest/receipt and keeps lesson gaps distinguishable from repository
/// documents. Provenance is deliberately one fact per category — a count and
/// a representative reason — so a large memory pool can never turn optional
/// metadata into a mandatory-overflow refusal.
const LESSON_SOURCE_ID: &str = "native:lessons";
const LESSON_KIND: &str = "lessons";
const LESSON_PATH: &str = "memory://accepted-lessons";
const LESSON_WITHHELD_ID: &str = "native:lessons-withheld";
const LESSON_LOAD_ID: &str = "native:lessons-load";
const LESSON_OMITTED_ID: &str = "native:lessons-omitted";

/// One aggregate lesson fact stays this small. A memory record's own text
/// (for example a `stale:` note) is file-controlled, so it can never be
/// copied whole into mandatory metadata: the optional lesson source carries
/// the renderer-capped detail, and this fact names the count and a bounded
/// reason.
const LESSON_FACT_MAX: usize = 480;

fn lesson_fact(text: &str) -> String {
    if text.len() <= LESSON_FACT_MAX {
        return text.to_string();
    }
    let mut end = LESSON_FACT_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn content_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Redacted, bounded file-controlled lesson text: a withholding reason, a
/// memory load-error line (an unreadable file name can be token-shaped) or a
/// match failure. Redaction runs before the cap, and a redaction failure shows
/// the marker — never the raw value.
fn redacted_fact(text: &str) -> String {
    let clean = match crate::secret::redact_text(text) {
        Ok(text) => text,
        Err(_) => crate::context_pack::LESSON_REDACTION_UNAVAILABLE.to_string(),
    };
    lesson_fact(&clean)
}

/// Select the project-memory lessons current policy applies to this issue
/// and the actual assignee's provider, exactly once. The rendered Markdown
/// becomes one optional content-addressed source whose included IDs are the
/// entries the renderer actually emitted; withheld, load-error and omitted
/// counts stay visible as bounded aggregate facts. Eligibility, ranking,
/// withholding and the render caps are the existing memory policy; nothing
/// here re-verifies or re-renders.
fn lesson_selection(
    pm: &Pm,
    project: &crate::issue::project::Project,
    front: &Front,
    provider: &str,
    role: Option<&str>,
) -> crate::context_pack::LessonSelection {
    let issue = board::Issue {
        project: project.key.clone(),
        dir: pm.dir.join(&project.key).join(&front.id),
        front: front.clone(),
        body: String::new(),
        comments: vec![],
        artifacts: vec![],
    };
    let gap = |id: &str, state: &str, reason: String| crate::context_pack::Gap {
        id: id.to_string(),
        path: LESSON_PATH.to_string(),
        state: state.to_string(),
        reason,
        required: false,
        lessons: Vec::new(),
    };
    let mut withheld = Vec::new();
    let source = match memory::match_for_issue(pm, &issue, Some(provider), role) {
        Err(error) => {
            withheld.push(gap(
                LESSON_SOURCE_ID,
                "unavailable",
                redacted_fact(&format!("memory match failed: {error}")),
            ));
            None
        }
        Ok((matched, errors)) => {
            if !matched.withheld.is_empty() {
                // The aggregate fact names the complete count; bounded detail
                // names each lesson with its recorded reason. Neither enters
                // the mandatory payload — the optional lesson source carries
                // the renderer-capped text, and both facts ride the pin and
                // supply receipts.
                let detail = crate::context_pack::lesson_notes(
                    matched
                        .withheld
                        .iter()
                        .map(|(memory, reason)| (memory.front.id.clone(), reason.clone())),
                );
                let shown = detail.len();
                withheld.push(crate::context_pack::Gap {
                    id: LESSON_WITHHELD_ID.to_string(),
                    path: LESSON_PATH.to_string(),
                    state: "withheld".to_string(),
                    reason: format!(
                        "{} in-scope lesson(s) withheld by validity or role policy; \
                         `cadence memory match --issue {} --json` lists reasons{}",
                        matched.withheld.len(),
                        front.id,
                        if shown < matched.withheld.len() {
                            format!("; first {shown} named here")
                        } else {
                            String::new()
                        },
                    ),
                    required: false,
                    lessons: detail,
                });
            }
            if let Some(line) = memory::load_errors_line(&errors) {
                // The first failing file name is file-controlled and can be
                // token-shaped; redact before bounding the gap reason.
                withheld.push(gap(LESSON_LOAD_ID, "unreadable", redacted_fact(&line)));
            }
            let (text, slugs) = memory::render_lessons(&matched);
            // Omitted reporting does not depend on the renderer's header: a
            // matched lesson the renderer did not emit is named (bounded) even
            // when no lesson text shipped at all.
            let omitted = matched.lessons.len().saturating_sub(slugs.len());
            if omitted > 0 {
                // The emitted slugs are the renderer's truth; everything
                // else was dropped by its entry or byte cap.
                let emitted: std::collections::HashSet<&str> =
                    slugs.iter().map(String::as_str).collect();
                let detail = crate::context_pack::lesson_notes(
                    matched
                        .lessons
                        .iter()
                        .filter(|memory| !emitted.contains(memory.front.id.as_str()))
                        .map(|memory| {
                            (
                                memory.front.id.clone(),
                                "matched lesson omitted by the bounded lesson injection cap"
                                    .to_string(),
                            )
                        }),
                );
                let shown = detail.len();
                withheld.push(crate::context_pack::Gap {
                    id: LESSON_OMITTED_ID.to_string(),
                    path: LESSON_PATH.to_string(),
                    state: "overflow".to_string(),
                    reason: format!(
                        "{omitted} matched lesson(s) omitted by the lesson injection cap{}",
                        if shown < omitted {
                            format!("; first {shown} named here")
                        } else {
                            String::new()
                        },
                    ),
                    required: false,
                    lessons: detail,
                });
            }
            if text.is_empty() {
                None
            } else {
                let digest = content_digest(text.as_bytes());
                Some(crate::context_pack::Source {
                    id: LESSON_SOURCE_ID.to_string(),
                    kind: LESSON_KIND.to_string(),
                    path: LESSON_PATH.to_string(),
                    title: "Project lessons".to_string(),
                    required: false,
                    revision: digest.clone(),
                    digest,
                    bytes: text.len(),
                    inline: Some(text),
                    lesson_ids: slugs,
                })
            }
        }
    };
    crate::context_pack::LessonSelection { source, withheld }
}

/// Build, validate and return the manifest a new kickoff's admission
/// transaction will pin. `body` is the exact delivered text; the required
/// sources and that body plus known adapter material must fit the combined
/// cap before anything is admitted.
#[allow(clippy::too_many_arguments)]
fn admit_context<'a>(
    project: &crate::issue::project::Project,
    pinned: crate::issue::context::PinnedContext,
    role: &str,
    assigned: Option<&str>,
    lessons: Option<crate::context_pack::LessonSelection>,
    contract: crate::context_pack::TaskContract,
    body: std::borrow::Cow<'a, str>,
    provider: &str,
) -> Result<Option<PreparedContext<'a>>> {
    let manifest =
        crate::context_pack::build(contract, &project.key, &pinned, role, assigned, lessons)
            .map_err(|refusal| refusal.error())?;
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let root = crate::issue::context::repository_root(project).map_err(|(state, detail)| {
        crate::context_pack::Refusal::UnavailableProject { state, detail }.error()
    })?;
    let mut reader = |document: &crate::context_pack::Source| {
        crate::issue::context::read_pinned_blob(
            &root,
            &document.revision,
            &document.path,
            &document.digest,
        )
    };
    crate::context_pack::plan(
        &manifest,
        None,
        false,
        body.as_ref(),
        crate::context_pack::adapter_reserved_bytes(provider),
        &mut reader,
    )
    .map_err(|refusal| refusal.error())?;
    Ok(Some(PreparedContext { manifest, body }))
}

/// CAD-1822/CAD-1846: pin and validate a task's context for `target`
/// before a kickoff exists. Shared by [`preflight`] (CLI and
/// `issue_kickoff` HTTP/RPC, before any lane is created) and the daemon's
/// `dispatch_send` (the last gate before enqueue).
///
/// The pin comes first: a project that does not participate returns
/// `None` without calling `make`, so an opt-out dispatch builds no new
/// contract and reads no new source. Selection is daemon-side — the
/// issue's own project, its declared local repository and its tracked
/// manifest at HEAD; no request field can name a source, revision or
/// path. `make` supplies the complete contract and the exact body this
/// turn delivers.
///
/// A required source that cannot be read, an unusable manifest, a
/// combined mandatory payload over the local cap and a non-managed
/// endpoint that cannot receive required prompt context all refuse with
/// [`crate::context_pack::Refusal`] before anything is created.
pub(crate) fn context_guard<'a>(
    pm: &Pm,
    project: &crate::issue::project::Project,
    front: &Front,
    target: ContextTarget<'_>,
    no_lessons: bool,
    make: impl FnOnce() -> Result<(crate::context_pack::TaskContract, std::borrow::Cow<'a, str>)>,
) -> Result<Option<PreparedContext<'a>>> {
    let (role, assigned) = crate::context_pack::selection_role(target.team_role);
    let pinned = crate::issue::context::pin(project, Some(role));
    if pinned.state == "unconfigured" {
        return Ok(None);
    }
    if !crate::continuity::endpoint_takes_packs(target.endpoint_kind) {
        return Err(crate::context_pack::Refusal::UnsupportedEndpoint {
            endpoint: target.endpoint_kind.to_string(),
        }
        .error());
    }
    let lessons = (!no_lessons)
        .then(|| lesson_selection(pm, project, front, target.provider, target.team_role));
    let (contract, body) = make()?;
    admit_context(
        project,
        pinned,
        role,
        assigned.as_deref(),
        lessons,
        contract,
        body,
        target.provider,
    )
}

/// Does `lane`'s directory name belong to issue `id` under the layout
/// (`<id-lower>-<slug>`)? A layout lane named for another issue is a foreign
/// source even when it lives under the same declared repository.
fn lane_of_issue(lane: &Path, issue_id: &str) -> bool {
    lane.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.starts_with(&crate::worktree::layout::issue_stem(issue_id)))
}

/// The canonical task lane a recorded `worktree` names: an exact layout lane
/// (`<declared repo>/.cadence/wt/<name>`) of this issue, below a repository
/// the project declares. A bare name is resolved under the declared repos; an
/// ambiguous match is refused rather than guessed, and a path of any other
/// shape is never an approved lane.
fn task_spec_lane(declared: &[PathBuf], worktree: Option<&str>, issue_id: &str) -> Option<PathBuf> {
    let worktree = worktree?;
    let raw = Path::new(worktree);
    if raw.is_absolute() {
        let lane = raw.canonicalize().ok()?;
        let root = crate::worktree::layout::root_of(&lane)?;
        return (declared.contains(&root)
            && lane.starts_with(crate::worktree::layout::worktrees_dir(&root))
            && lane_of_issue(&lane, issue_id))
        .then_some(lane);
    }
    let mut parts = raw.components();
    match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(_)), None) => {}
        _ => return None,
    }
    let mut found = declared.iter().filter_map(|root| {
        let lane = crate::worktree::layout::worktree_dir(root, worktree)
            .canonicalize()
            .ok()?;
        (crate::worktree::layout::root_of(&lane).as_ref() == Some(root)
            && lane_of_issue(&lane, issue_id))
        .then_some(lane)
    });
    let lane = found.next()?;
    if found.next().is_some() {
        return None;
    }
    Some(lane)
}

/// The base a relative *task override* resolves against: the task's own lane,
/// or the declared repo root a cloud task records as its worktree. Never the
/// daemon's working directory.
fn task_spec_base(declared: &[PathBuf], worktree: Option<&str>, issue_id: &str) -> Option<PathBuf> {
    if let Some(lane) = task_spec_lane(declared, worktree, issue_id) {
        return Some(lane);
    }
    let raw = Path::new(worktree?);
    if !raw.is_absolute() {
        return None;
    }
    let base = raw.canonicalize().ok()?;
    (base.is_dir() && declared.contains(&base)).then_some(base)
}

/// The base a relative *job* spec resolves against: the repository the job
/// declares, else the project's single declared repository. Zero or several
/// candidates is ambiguity, refused rather than guessed.
fn default_spec_base(declared: &[PathBuf], job_repo: Option<&str>) -> Option<PathBuf> {
    match job_repo {
        Some(repo) => {
            let repo = crate::issue::project::expand_home(repo);
            let repo = repo.canonicalize().ok()?;
            declared.iter().find(|root| **root == repo).cloned()
        }
        None => match declared {
            [only] => Some(only.to_path_buf()),
            _ => None,
        },
    }
}

/// Is `rel` a tracked regular blob at `root`'s HEAD? The bounded `ls-tree`
/// proof the project-context reader uses, without its document allowlist: a
/// task spec may be any tracked text file. This tests the server-derived
/// repository, never the caller's path as a capability, and makes no claim
/// that the working bytes match HEAD.
fn tracked_at_head(root: &Path, rel: &Path) -> Result<bool> {
    let path = rel.to_str().ok_or_else(|| {
        Error::rejected("context refused: spec-scope-unverifiable: the task spec path is not UTF-8")
    })?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["--literal-pathspecs", "ls-tree", "-z", "HEAD", "--", path]);
    let out = crate::proc::run_bounded(&mut cmd, Duration::from_secs(30)).map_err(|error| {
        Error::rejected(format!(
            "context refused: spec-scope-unverifiable: {} could not prove tracked task \
             specs ({error}) — nothing was read",
            root.display()
        ))
    })?;
    if !out.status.success() {
        return Ok(false);
    }
    Ok(out
        .stdout
        .split(|byte| *byte == 0)
        .next()
        .is_some_and(|record| {
            record.starts_with(b"100644 blob ") || record.starts_with(b"100755 blob ")
        }))
}

fn spec_scope_error(path: &str, why: &str) -> Error {
    crate::context_pack::Refusal::ScopeConflict {
        detail: format!("the task spec {path} is outside the approved source scope: {why}"),
    }
    .error()
}

/// Approve and read one job task spec. The canonical target must be the
/// task's own lane, the captured issue directory, or a tracked file of a
/// declared project repository; anything else is a scope conflict before a
/// byte is read. The read itself is descriptor-relative beneath the approved
/// root, nofollow, bounded to the existing local combined cap and UTF-8 only;
/// a failure states that bounded reader's actual error rather than pretending
/// the file was unavailable or verified at a commit.
fn read_job_spec(
    project: &crate::issue::project::Project,
    issue_dir: &Path,
    task: &store::Task,
    job: &store::Job,
    spec_path: &str,
) -> Result<(String, PathBuf, &'static str)> {
    let declared = start::declared_repos(project);
    let issue_id = job.issue_id.as_deref().unwrap_or_default();
    let raw = Path::new(spec_path);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        let base = if task.spec_path.is_some() {
            task_spec_base(&declared, task.worktree.as_deref(), issue_id)
        } else {
            default_spec_base(&declared, job.repo.as_deref())
        };
        let Some(base) = base else {
            return Err(spec_scope_error(
                spec_path,
                "a relative spec has no approved task lane or declared repository",
            ));
        };
        base.join(raw)
    };
    let file = candidate.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "context refused: spec-unavailable: the task spec {spec_path} could not be \
             resolved ({error}) — nothing was read or queued"
        ))
    })?;
    if !file.is_file() {
        return Err(spec_scope_error(spec_path, "it is not a regular file"));
    }
    let issue_dir = issue_dir.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "context refused: spec-scope-unverifiable: the captured issue directory {} \
             could not be resolved ({error}) — nothing was read",
            issue_dir.display()
        ))
    })?;
    let lane = task_spec_lane(&declared, task.worktree.as_deref(), issue_id);
    let (root, label) = if file.starts_with(&issue_dir) {
        (issue_dir, "issue")
    } else if let Some(lane) = lane.filter(|lane| file.starts_with(lane)) {
        (lane, "lane")
    } else {
        let mut approved = None;
        for root in &declared {
            if !file.starts_with(root) {
                continue;
            }
            let Ok(rel) = file.strip_prefix(root) else {
                continue;
            };
            if tracked_at_head(root, rel)? {
                approved = Some((root.to_path_buf(), "tracked-repo"));
                break;
            }
        }
        approved.ok_or_else(|| {
            spec_scope_error(
                spec_path,
                "it is outside the task lane and captured issue directory and is not a \
                 tracked file of a declared project repository",
            )
        })?
    };
    let relative = file
        .strip_prefix(&root)
        .map_err(|_| spec_scope_error(spec_path, "it is not beneath the approved root"))?;
    let reader = super::app_catalog::fs::Root::open(&root).map_err(|error| {
        Error::rejected(format!(
            "context refused: spec-unavailable: the approved root {} could not be opened \
             by the bounded source reader ({error}) — nothing was read or queued",
            root.display()
        ))
    })?;
    let text = reader
        .read(relative, crate::context_pack::MAX_COMBINED_BYTES as u64)
        .map_err(|error| {
            Error::rejected(format!(
                "context refused: spec-unavailable: the approved task spec {} could not \
                 be read by the bounded source reader ({error}) — nothing was truncated \
                 or queued",
                file.display()
            ))
        })?;
    let Some(text) = text else {
        return Err(Error::rejected(format!(
            "context refused: spec-unavailable: the approved task spec {} does not exist",
            file.display()
        )));
    };
    Ok((text, file, label))
}

/// Canonical issue specs contain mutable tracker metadata that is not part
/// of their effective spec. Pin every body byte, including all criteria;
/// retain the separate raw source digest in the delivery manifest.
pub(crate) fn canonical_issue_spec_digest(
    pm: &Pm,
    issue: &str,
    spec: &Path,
    only_opted_in: bool,
) -> Result<Option<String>> {
    let Ok((project, dir)) = write::issue_dir(pm, issue) else {
        // Jobs may name issues not held by this tracker.
        return Ok(None);
    };
    let Ok(canonical) = dir.join("issue.md").canonicalize() else {
        return Ok(None);
    };
    if spec.canonicalize().ok().as_ref() != Some(&canonical)
        || (only_opted_in && super::context::pin(&project, None).state == "unconfigured")
    {
        return Ok(None);
    }
    let reader = super::app_catalog::fs::Root::open(&dir)?;
    let source = reader
        .read(
            Path::new("issue.md"),
            crate::context_pack::MAX_COMBINED_BYTES as u64,
        )?
        .ok_or_else(|| Error::rejected("canonical issue spec is unavailable"))?;
    Ok(Some(content_digest(
        parse::split_front(&source)?.1.as_bytes(),
    )))
}

/// CAD-1846: the same mandatory pipeline for an issue-linked job task.
/// The caller proves the job's issue binding and real lane from its own
/// captured rows; this captures the complete effective spec once, the
/// parent issue's complete prose and the task's own acceptance, and
/// builds the contract at the job task's identity. `None` means the
/// project has not opted in — the caller renders the legacy kickoff and
/// never labels it a complete contract.
pub(crate) fn job_context(
    pm: &Pm,
    gated: GatedIssue,
    job: JobContext<'_>,
    no_lessons: bool,
) -> Result<Option<PreparedContext<'static>>> {
    let GatedIssue {
        project,
        dir,
        front,
        items: parent_items,
        source,
        request: parent_request,
    } = gated;
    let JobContext {
        job: job_row,
        task,
        revision,
        message,
        target,
        admission_scope,
    } = job;
    if task.job_id != job_row.id || job_row.issue_id.as_deref() != Some(front.id.as_str()) {
        return Err(Error::rejected(
            "context refused: the job task is not bound to this issue — nothing was queued",
        ));
    }
    let provider = target.provider;
    let endpoint_kind = target.endpoint_kind;
    context_guard(pm, &project, &front, target, no_lessons, || {
        // The caller's own scope proof runs only after this project is
        // confirmed opted in and its endpoint can receive context, and
        // before any spec byte is read: an opt-out dispatch never pays for
        // a lane proof, and its legacy kickoff needs none.
        if let Some(check) = admission_scope {
            check(&project, &front)?;
        }
        // The complete effective spec, approved and captured once. The
        // canonical locator must be the task's own lane, the captured issue
        // directory, or a tracked file of a declared project repository; the
        // bytes come through the descriptor-relative bounded reader, never a
        // daemon-cwd path or an unbounded fs::read.
        let spec_path = task
            .spec_path
            .as_deref()
            .unwrap_or(job_row.spec_path.as_str());
        let (captured_spec, spec_file, spec_scope) =
            read_job_spec(&project, &dir, task, job_row, spec_path)?;
        let issue_file = std::fs::canonicalize(dir.join("issue.md")).ok();
        let canonical_issue_spec = issue_file.as_deref() == Some(spec_file.as_path());
        // Use the selected issue snapshot for both provenance and its request,
        // not a second source revision observed during the scope/read proof.
        let spec = if canonical_issue_spec {
            source.as_str()
        } else {
            captured_spec.as_str()
        };
        let spec_digest = content_digest(spec.as_bytes());
        // The job's recorded digest still gates its own spec. The working
        // bytes may be dirty; this digest is the captured data revision, not
        // a claim that the file matches HEAD.
        if task.spec_path.is_none() {
            if let Some(expected) = job_row.spec_sha256.as_deref() {
                let canonical_digest = if canonical_issue_spec {
                    Some(content_digest(parse::split_front(spec)?.1.as_bytes()))
                } else {
                    None
                };
                if canonical_digest.as_deref().unwrap_or(&spec_digest) != expected {
                    return Err(Error::rejected(format!(
                        "context refused: spec-changed: the task spec {} no longer \
                         matches the revision the job was opened with — reopen the task or \
                         restore the pinned spec; nothing was queued",
                        spec_file.display()
                    )));
                }
            }
        }
        // Do not re-embed tracker frontmatter or criteria already represented
        // by the native task. A task-specific checklist may differ from the
        // canonical spec's checklist, whose other requirements remain mandatory.
        let spec_is_issue = canonical_issue_spec || spec.as_bytes() == source.as_bytes();
        let acceptance = task_acceptance(task);
        let title = task
            .title
            .as_deref()
            .or(job_row.title.as_deref())
            .unwrap_or(front.title.as_str());
        // The identity contract first, so its parent source digest is the
        // captured issue SHA that both provenance and the composite use
        // without hashing the issue source a second time.
        let mut contract = crate::context_pack::TaskContract::new(
            &task.id,
            title,
            source.as_bytes(),
            &acceptance,
            String::new(),
        );
        contract.issue = front.id.clone();
        let parent_revision = contract.revision.clone();
        let mut request = String::with_capacity(
            spec.len() + parent_request.len() + spec_file.as_os_str().len() + 192,
        );
        // Provenance first: the approved canonical locator, the exact
        // captured content digest and the approved scope. This is the data
        // revision of the working bytes — never a fabricated Git-verified SHA.
        request.push_str(&format!(
            "Task spec source: {} (sha256 {}; scope {}; captured working bytes, not a \
             Git-verified revision)\n\n",
            spec_file.display(),
            spec_digest,
            spec_scope,
        ));
        if spec_is_issue {
            request.push_str(&parent_request);
            let mut additional = parent_items
                .iter()
                .filter(|item| !acceptance.iter().any(|task| task.text == item.text))
                .peekable();
            if additional.peek().is_some() {
                if !request.ends_with('\n') {
                    request.push('\n');
                }
                request.push_str("\n## Additional effective spec criteria\n\n");
                for item in additional {
                    request.push_str(if item.checked { "- [x] " } else { "- [ ] " });
                    request.push_str(&item.text);
                    request.push('\n');
                }
            }
        } else {
            request.push_str("## Task spec\n\n");
            request.push_str(spec);
            if !request.ends_with('\n') {
                request.push('\n');
            }
            if !parent_request.trim().is_empty() {
                request.push_str(&format!(
                    "\n## Parent issue {}: {}\n\n",
                    front.id, front.title
                ));
                request.push_str(&parent_request);
                if !request.ends_with('\n') {
                    request.push('\n');
                }
            }
        }
        contract.request = request;
        // The metadata/report envelope, rendered once. Its exact bytes are
        // persisted as the contract's execution so a lost session restores
        // the scope and report/SHA obligation, not only the spec and
        // criteria; the clone is the small necessary copy.
        let body = store::job_kickoff(
            job_row,
            task,
            revision,
            message,
            provider,
            endpoint_kind,
            true,
        )?;
        contract.execution = Some(body.clone());
        // The issue digest alone cannot tell a changed spec, acceptance,
        // title, execution or scope/report envelope from an unchanged one,
        // and a delta keyed on it would leave the previous contract resident.
        // This composite is job-only; the plain path keeps the source SHA.
        let composite = job_contract_digest(&parent_revision, &contract, revision, job_row);
        contract.revision = composite;
        Ok((contract, std::borrow::Cow::Owned(body)))
    })
}

/// The complete acceptance a job task carries, from its own recorded text.
/// A task with no acceptance carries none: the parent issue's criteria are a
/// different contract and are never substituted. A generated CAD-300 listing
/// round-trips exactly; anything else is one free-form criterion whose
/// original bytes are kept (trimming only decides emptiness).
fn task_acceptance(task: &store::Task) -> Vec<AcceptanceItem> {
    let Some(text) = task
        .acceptance
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    else {
        return Vec::new();
    };
    match parse_acceptance_listing(text) {
        Some((items, "")) => items,
        _ => vec![AcceptanceItem {
            text: text.to_string(),
            checked: false,
        }],
    }
}

/// Stream one length-prefixed field into a contract digest. The prefix keeps
/// concatenation unambiguous without serializing or copying the (possibly
/// large) spec/request text merely to hash it.
fn digest_field(hasher: &mut sha2::Sha256, bytes: &[u8]) {
    use sha2::Digest;
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// The job-only contract revision: the captured parent source SHA, the
/// job/task identity and selected native revision, the exact rendered title,
/// request and acceptance, and the exact execution envelope persisted for
/// recovery. The envelope subsumes the scope, provider/report and issue
/// fields, so it is hashed once whole — including a real explicit report
/// message id when the body carries one: restoring that exact obligation
/// after session loss is worth more than excluding delivery ids. A delta
/// keyed on the parent digest alone would leave the previous contract
/// resident whenever only these changed.
fn job_contract_digest(
    parent_revision: &str,
    contract: &crate::context_pack::TaskContract,
    revision: i64,
    job: &store::Job,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    digest_field(&mut hasher, b"cadence-job-contract-v2");
    digest_field(&mut hasher, parent_revision.as_bytes());
    digest_field(&mut hasher, contract.issue.as_bytes());
    digest_field(&mut hasher, contract.task.as_bytes());
    digest_field(&mut hasher, job.id.as_bytes());
    digest_field(&mut hasher, revision.to_string().as_bytes());
    digest_field(&mut hasher, contract.title.as_bytes());
    digest_field(&mut hasher, contract.request.as_bytes());
    digest_field(
        &mut hasher,
        &(contract.acceptance.len() as u64).to_le_bytes(),
    );
    for item in &contract.acceptance {
        digest_field(&mut hasher, item.as_bytes());
    }
    digest_field(
        &mut hasher,
        contract.execution.as_deref().unwrap_or_default().as_bytes(),
    );
    format!("{:x}", hasher.finalize())
}

// ── Early accepted-kickoff gate (CAD-1846) ───────────────────────
//
// An accepted kickoff is answered from the daemon's own rows before a second
// source capture, contract render, lane setup or job creation. A tracker ref
// only locates a candidate; the row itself proves identity and kind.

/// The two kickoff kinds. They have different duplicate identities: a plain
/// kickoff carries no task, a job kickoff is its task's `job_dispatch` row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DispatchMode {
    Plain,
    Job,
}

impl DispatchMode {
    fn label(self) -> &'static str {
        match self {
            DispatchMode::Plain => "plain",
            DispatchMode::Job => "job",
        }
    }
}

/// The kickoff kind a stored message row actually is. `None` for anything
/// else, so an arbitrary user-authored row can never count as a kickoff.
fn row_mode(row: &Value) -> Option<DispatchMode> {
    match (row["source"].as_str(), row["task_id"].as_str()) {
        (Some("dispatch"), None) => Some(DispatchMode::Plain),
        (Some("job_dispatch"), Some(_)) => Some(DispatchMode::Job),
        _ => None,
    }
}

/// The issue's open `message` refs as `(id, recorded owner)` — a locator
/// only; every caller proves the daemon's own row.
fn message_refs(front: &Front, worker: &str) -> Vec<(String, String)> {
    front
        .refs
        .iter()
        .filter(|r| r.kind == "message" && r.closed != Some(true))
        .filter_map(|r| {
            let mid = r.path.clone()?;
            let owner = r
                .agent
                .clone()
                .or_else(|| {
                    r.label
                        .as_deref()
                        .and_then(|label| label.strip_prefix("dispatch → "))
                        .map(str::to_string)
                })
                .unwrap_or_else(|| worker.to_string());
            Some((mid, owner))
        })
        .collect()
}

fn find_row(show: &Value, mid: &str) -> Option<Value> {
    show["messages"]
        .as_array()?
        .iter()
        .find(|row| row["id"].as_str() == Some(mid))
        .cloned()
}

/// The daemon's own row for `mid` on `owner`'s history.
fn message_row(
    state_dir: &Path,
    owner: &str,
    worker: &str,
    show: &Value,
    mid: &str,
) -> Option<Value> {
    if owner == worker {
        return find_row(show, mid);
    }
    client::rpc(state_dir, "agent_show", json!({"alias": owner}))
        .ok()
        .and_then(|show| find_row(&show, mid))
}

fn lexical(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Canonical when both sides resolve, else the absolute lexical path — a
/// recorded string and a live path compare without inventing a root.
fn same_path(recorded: &Path, expected: &Path) -> bool {
    let left = recorded
        .canonicalize()
        .unwrap_or_else(|_| lexical(recorded));
    let right = expected
        .canonicalize()
        .unwrap_or_else(|_| lexical(expected));
    left == right
}

/// A task's recorded worktree against the lane this dispatch resolved: a bare
/// name names the lane, a path must resolve to it.
fn same_lane(recorded: &str, lane: &Path) -> bool {
    let recorded = Path::new(recorded);
    if recorded.is_absolute() || recorded.components().count() != 1 {
        return same_path(recorded, lane);
    }
    match (recorded.file_name(), lane.file_name()) {
        (Some(recorded), Some(lane)) => recorded == lane,
        _ => false,
    }
}

/// The approved spec locator as given: canonical when it resolves, else the
/// absolute lexical path. Never read, never hashed.
fn spec_locator(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| lexical(path))
}

fn same_locator(recorded: &str, locator: &Path) -> bool {
    spec_locator(Path::new(recorded)) == spec_locator(locator)
}

/// The issue's single OPEN native job and its canonical default task. A
/// terminal job is never a retry candidate — the store's one-open-job guard
/// then governs a fresh job.
struct OpenJob {
    job: Value,
    task: Value,
}

fn open_job(state_dir: &Path, front: &Front) -> Result<Option<OpenJob>> {
    let list = client::rpc(state_dir, "job_list", json!({"states": ["open"]}))?;
    let jobs: Vec<Value> = list["jobs"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|job| {
                    job["state"].as_str() == Some("open")
                        && job["issue"].as_str() == Some(front.id.as_str())
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let job = match jobs.as_slice() {
        [] => return Ok(None),
        [job] => job.clone(),
        many => {
            return Err(Error::rejected(format!(
                "{} maps to {} open jobs ({}) — refusing to guess which native \
                 binding a dispatch retries; close the stale ones first",
                front.id,
                many.len(),
                many.iter()
                    .filter_map(|job| job["id"].as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )))
        }
    };
    let id = job["id"].as_str().unwrap_or_default().to_string();
    let task_id = format!("{id}-t1");
    // `job_show` computes file drift and opens the spec. A retry must read
    // only stored identity, even if its source is now huge or unavailable.
    let show = client::rpc(state_dir, "task_show", json!({"task": task_id}))?;
    let task = show["task"].clone();
    if task["id"].as_str() != Some(task_id.as_str()) || task["job"].as_str() != Some(id.as_str()) {
        return Err(Error::rejected(format!(
            "open job {id} does not carry its canonical task {task_id} — refusing to \
             infer a native binding from it"
        )));
    }
    Ok(Some(OpenJob { job, task }))
}

/// The read-only facts an early duplicate gate needs about this dispatch.
struct EarlyDispatch<'a> {
    state_dir: &'a Path,
    front: &'a Front,
    mode: DispatchMode,
    worker: &'a str,
    reply_to: &'a str,
    show: &'a Value,
    repo_root: &'a Path,
    lane: &'a Path,
    branch: &'a str,
    job_locator: Option<&'a Path>,
    acceptance: &'a [AcceptanceItem],
}

/// What the daemon's own rows already say about this dispatch.
enum EarlyKickoff {
    /// Nothing accepted yet: the normal flow runs.
    None,
    /// This request's own accepted kickoff — return it, create nothing.
    Duplicate {
        row: Value,
        job: Option<Value>,
        task: Option<Value>,
        kind: &'static str,
    },
    /// The issue's open job exists but was never accepted: reuse its native
    /// binding instead of minting another job.
    ReuseJob { job: Value },
}

fn busy_error(issue: &str, lane: &Path, mode: DispatchMode, row: &Value) -> Error {
    Error::rejected(format!(
        "{issue}'s lane {} already has a live {} kickoff {} ({}) — wait for it to finish \
         or reconcile it; nothing was created or queued",
        lane.display(),
        mode.label(),
        row["id"].as_str().unwrap_or("?"),
        row["state"].as_str().unwrap_or("?"),
    ))
}

/// Which of this request's material inputs the open job's own scope disagrees
/// with. Job identity, PM, repository, assignee, lane and spec locator are
/// the binding a retry may never re-point.
fn binding_mismatches(binding: &OpenJob, d: &EarlyDispatch<'_>) -> Vec<&'static str> {
    let job = &binding.job;
    let task = &binding.task;
    let mut wrong = Vec::new();
    if job["issue"].as_str() != Some(d.front.id.as_str()) {
        wrong.push("issue");
    }
    if job["pm"].as_str() != Some(d.reply_to) {
        wrong.push("pm");
    }
    if !job["repo"]
        .as_str()
        .is_some_and(|repo| same_path(Path::new(repo), d.repo_root))
    {
        wrong.push("repo");
    }
    if task["job"].as_str() != job["id"].as_str() {
        wrong.push("task job");
    }
    if task["assignee"].as_str() != Some(d.worker) {
        wrong.push("assignee");
    }
    if !task["worktree"]
        .as_str()
        .is_some_and(|worktree| same_lane(worktree, d.lane))
    {
        wrong.push("worktree");
    }
    if task["branch"].as_str() != Some(d.branch) {
        wrong.push("branch");
    }
    let locator_matches = d.job_locator.is_some_and(|locator| {
        task["spec_path"]
            .as_str()
            .or_else(|| job["spec_path"].as_str())
            .is_some_and(|recorded| same_locator(recorded, locator))
    });
    if !locator_matches {
        wrong.push("spec");
    }
    wrong
}

/// The material inputs a never-accepted draft must still agree on before its
/// native binding is reused. The lane's own recorded base is the authority:
/// the default branch moves after the lane was cut, and a legitimate frozen
/// lane must not be refused because `main` advanced.
fn draft_mismatches(binding: &OpenJob, d: &EarlyDispatch<'_>) -> Result<Vec<&'static str>> {
    let job = &binding.job;
    let task = &binding.task;
    let mut wrong = binding_mismatches(binding, d);
    if task["state"].as_str() != Some("draft") {
        wrong.push("task state");
    }
    let recorded = crate::worktree::lifecycle::recoverable_record(
        d.repo_root,
        d.lane,
        "development",
        "cadence issue start",
        Some(d.branch),
        Some(d.front.id.as_str()),
    )?;
    if let Some(base) = recorded.and_then(|record| record.base_sha) {
        if job["base_ref"].as_str() != Some(base.as_str()) {
            wrong.push("job base");
        }
        if task["base_sha"].as_str() != Some(base.as_str()) {
            wrong.push("task base");
        }
    } else if job["base_ref"].as_str() != task["base_sha"].as_str() {
        wrong.push("base");
    }
    let acceptance = acceptance_listing(d.acceptance);
    if task["acceptance"].as_str() != acceptance.as_deref() {
        wrong.push("acceptance");
    }
    Ok(wrong)
}

/// The task's own kickoff row, from the native task view: an id-targeted read,
/// so a bounded agent history can never hide an accepted row.
fn task_message_row(state_dir: &Path, task_id: &str, mid: &str) -> Result<Option<Value>> {
    let show = client::rpc(state_dir, "task_show", json!({"task": task_id}))?;
    Ok(show["task"]["messages"].as_array().and_then(|rows| {
        rows.iter()
            .find(|row| row["id"].as_str() == Some(mid))
            .cloned()
    }))
}

fn accepted_row_ok(row: &Value, d: &EarlyDispatch<'_>, task_id: &str) -> bool {
    row["alias"].as_str() == Some(d.worker)
        && row["issue"].as_str() == Some(d.front.id.as_str())
        && row["worktree"]
            .as_str()
            .is_some_and(|worktree| same_lane(worktree, d.lane))
        && row["task_id"].as_str() == Some(task_id)
        && row_mode(row) == Some(DispatchMode::Job)
}

/// Decide the early outcome from the daemon's rows.
fn early_kickoff(d: &EarlyDispatch<'_>) -> Result<EarlyKickoff> {
    // 1. Live rows the issue's open `message` refs name.
    let mut live_plain = None;
    let mut live_job = None;
    for (mid, owner) in message_refs(d.front, d.worker) {
        let Some(row) = message_row(d.state_dir, &owner, d.worker, d.show, &mid) else {
            continue;
        };
        if !LIVE_MESSAGE_STATES.contains(&row["state"].as_str().unwrap_or_default()) {
            continue;
        }
        if row["alias"].as_str() != Some(d.worker)
            || row["issue"].as_str() != Some(d.front.id.as_str())
            || !row["worktree"]
                .as_str()
                .is_some_and(|worktree| same_path(Path::new(worktree), d.lane))
        {
            continue;
        }
        match row_mode(&row) {
            Some(DispatchMode::Plain) => live_plain = Some(row),
            Some(DispatchMode::Job) => live_job = Some(row),
            None => {}
        }
    }
    if let Some(row) = live_plain {
        if d.mode == DispatchMode::Plain {
            return Ok(EarlyKickoff::Duplicate {
                row,
                job: None,
                task: None,
                kind: "live",
            });
        }
        return Err(busy_error(&d.front.id, d.lane, DispatchMode::Plain, &row));
    }

    // 2. The issue's one open native job and its canonical default task.
    let Some(binding) = open_job(d.state_dir, d.front)? else {
        return match live_job {
            Some(row) => Err(busy_error(&d.front.id, d.lane, DispatchMode::Job, &row)),
            None => Ok(EarlyKickoff::None),
        };
    };
    let job_id = binding.job["id"].as_str().unwrap_or_default().to_string();
    let task_id = binding.task["id"].as_str().unwrap_or_default().to_string();
    let accepted = binding.task["dispatch_message"]
        .as_str()
        .map(str::to_string);
    let on_lane = binding.task["worktree"]
        .as_str()
        .is_some_and(|worktree| same_lane(worktree, d.lane))
        && binding.task["branch"].as_str() == Some(d.branch);

    if d.mode == DispatchMode::Plain {
        // A live job kickoff on this lane is a busy lane, never a duplicate.
        // A recorded kickoff that cannot be proven as this issue's job row is
        // reconciled, never treated as absent.
        if let Some(mid) = &accepted {
            if on_lane {
                let stored = task_message_row(d.state_dir, &task_id, mid)?;
                match stored {
                    Some(row) if accepted_row_ok(&row, d, &task_id) => {
                        if LIVE_MESSAGE_STATES.contains(&row["state"].as_str().unwrap_or_default())
                        {
                            return Err(busy_error(&d.front.id, d.lane, DispatchMode::Job, &row));
                        }
                    }
                    _ => {
                        return Err(Error::rejected(format!(
                            "{} maps to open job {job_id} whose task {task_id} records \
                             kickoff {mid} that the daemon cannot prove as this issue's \
                             job row — reconcile the task before dispatching again; \
                             nothing was created",
                            d.front.id
                        )))
                    }
                }
            }
        }
        // A ref may still name a live job row of another (closed) job on this
        // lane; the lane is busy either way.
        if let Some(row) = live_job {
            return Err(busy_error(&d.front.id, d.lane, DispatchMode::Job, &row));
        }
        return Ok(EarlyKickoff::None);
    }

    // Job mode: the binding must be this request's own scope.
    let wrong = binding_mismatches(&binding, d);
    if !wrong.is_empty() {
        return Err(Error::rejected(format!(
            "{} already maps to open job {job_id} but its native binding differs from this \
             request ({}); use native reopen/revise or cancel the job — nothing was created",
            d.front.id,
            wrong.join(", "),
        )));
    }
    if let Some(mid) = &accepted {
        let stored = task_message_row(d.state_dir, &task_id, mid)?;
        let row = stored.filter(|row| accepted_row_ok(row, d, &task_id));
        let Some(row) = row else {
            return Err(Error::rejected(format!(
                "{} maps to open job {job_id} whose task {task_id} records kickoff {mid} \
                 that the daemon cannot prove as this issue's accepted job row — reconcile \
                 the task before dispatching again; nothing was created",
                d.front.id
            )));
        };
        let state = row["state"].as_str().unwrap_or_default().to_string();
        return match state.as_str() {
            "completed" => Ok(EarlyKickoff::Duplicate {
                row,
                job: Some(binding.job),
                task: Some(binding.task),
                kind: "reported",
            }),
            live if LIVE_MESSAGE_STATES.contains(&live) => Ok(EarlyKickoff::Duplicate {
                row,
                job: Some(binding.job),
                task: Some(binding.task),
                kind: "live",
            }),
            _ => Err(Error::rejected(format!(
                "{} maps to open job {job_id} whose task {task_id} kickoff {mid} ended \
                 '{}' without being accepted — reconcile the task before dispatching \
                 again; nothing was created",
                d.front.id,
                row["state"].as_str().unwrap_or("unknown"),
            ))),
        };
    }
    if let Some(row) = live_job {
        return Err(busy_error(&d.front.id, d.lane, DispatchMode::Job, &row));
    }
    let wrong = draft_mismatches(&binding, d)?;
    if !wrong.is_empty() {
        return Err(Error::rejected(format!(
            "{} already maps to open job {job_id} whose draft binding differs from this \
             request ({}); use native reopen/revise or cancel the job — nothing was created",
            d.front.id,
            wrong.join(", "),
        )));
    }
    Ok(EarlyKickoff::ReuseJob { job: binding.job })
}

fn preflight(
    pm: &Pm,
    id: &str,
    args: &DispatchArgs,
    state_dir: &Path,
) -> Result<std::ops::ControlFlow<Value, Preflight>> {
    let GatedIssue {
        project,
        dir,
        front,
        items,
        source,
        request,
    } = gate_issue(pm, id, false)?;
    // CAD-159: the CAD-238 section-scoped readback. No items warns;
    // it never refuses (PM decision, ADR-0002 §8.3).
    let acceptance_warning = items
        .is_empty()
        .then(|| acceptance_warning(&front.id, true));
    let acceptance = json!({
        "items": items.len(),
        "criteria": items.iter().map(AcceptanceItem::to_json).collect::<Vec<_>>(),
        "warning": acceptance_warning,
    });
    let reply_to = args
        .reply_to
        .clone()
        .or_else(|| std::env::var("CADENCE_ALIAS").ok())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            Error::rejected("Dispatch needs a return address — pass --reply-to <alias>")
        })?;
    let note_arg = args.note.clone().unwrap_or_else(|| dir.join("issue.md"));
    let note = note_arg
        .canonicalize()
        .map_err(|_| Error::rejected(format!("Note {} is unreadable", note_arg.display())))?;
    std::fs::metadata(&note)
        .map_err(|_| Error::rejected(format!("Note {} is unreadable", note_arg.display())))?;
    // CAD-383: an issue someone else holds in doing/review refuses here,
    // before the daemon is asked anything. The requester is the PM
    // (`--reply-to`) and the worker; `issue start` re-checks under the
    // tracker lock and records a take-over.
    claim::check(
        &front,
        &[reply_to.as_str(), args.to.as_str()],
        args.take_over.as_deref(),
        "dispatch",
        || claim::since(&pm.dir, &project.key, &front, Duration::from_secs(2)),
    )?;

    // Pre-flight, before ANYTHING is created: the worker exists and is
    // not fenced. A caller that starts a lane itself (the board) runs
    // this same check before `issue start`, so a refusal leaves nothing.
    let SeatCheck {
        show,
        agent,
        cwd_override,
        provider,
        endpoint_kind,
    } = check_seat(&project, &args.to, args.force, state_dir)?;
    if args.job_spec.is_none() && endpoint_kind == "sandbox" {
        return Err(Error::rejected(format!(
            "Worker '{}' is a cloud sandbox agent — cloud execution is only available \
             through `dispatch --job` with the job's spec; nothing was created",
            args.to
        )));
    }
    if args.job_spec.is_some() {
        let member = agent["alias"].as_str() == Some(reply_to.as_str())
            || agent["params"]["upstream"].as_str() == Some(reply_to.as_str());
        if !member {
            return Err(Error::rejected(format!(
                "'{}' is not in '{reply_to}'s group — the job's tasks can \
                 only bind the PM or a member of its group",
                args.to
            )));
        }
    }

    // The body is fully determined before `issue start`:
    // `resolve_repo`, `resolve_base` and `resolve_lane` are the calls
    // start makes, so the lane checked here — the issue's open lane
    // when it has one (CAD-274), else a fresh one named from
    // `--name`/the title — is the lane start binds (CAD-388 R2-1). A
    // lane start would refuse (a `--name` for another slug, several
    // open lanes, another repo) refuses here, before anything is
    // created. Plain dispatch checks the template body; `--job` checks
    // the daemon's own kickoff (`check_job_kickoff`).
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = start::resolve_repo(&project, args.repo.as_deref(), &cwd)?;
    let base = start::resolve_base(
        &root,
        args.base.as_deref(),
        start::configured_integration(&project, &root),
    )?;
    let base_sha = base.sha.clone();
    let (wt_dir, branch) = start::resolve_lane(&front, args.name.as_deref(), &root)?;
    // CAD-1846: an accepted kickoff is answered from the daemon's own rows
    // before any source capture, contract render, lane setup or job creation.
    // Only a fresh dispatch pays for sources, and only the native guards
    // decide whether it is admitted.
    let mode = if args.job_spec.is_some() {
        DispatchMode::Job
    } else {
        DispatchMode::Plain
    };
    let job_locator = args.job_spec.as_deref().map(spec_locator);
    let mut reuse_job: Option<String> = None;
    match early_kickoff(&EarlyDispatch {
        state_dir,
        front: &front,
        mode,
        worker: &args.to,
        reply_to: &reply_to,
        show: &show,
        repo_root: &root,
        lane: &wt_dir,
        branch: &branch,
        job_locator: job_locator.as_deref(),
        acceptance: &items,
    })? {
        EarlyKickoff::None => {}
        EarlyKickoff::Duplicate {
            row,
            job,
            task,
            kind,
        } => {
            let mut out = json!({
                "issue": front.id,
                "worker": args.to,
                "reply_to": reply_to,
                "note": note,
                "worktree": wt_dir,
                "branch": branch,
                "base": {"ref": base.name, "sha": base.sha},
                "acceptance": acceptance,
                "claim": claim::json(&front, crate::issue::time::now_epoch()),
                "dispatched": false,
                "duplicate": true,
                "duplicate_kind": kind,
                "message": row["id"],
                "message_state": row["state"],
            });
            if let Some(job) = job {
                out["job"] = job["id"].clone();
            }
            if let Some(task) = task {
                out["task"] = task["id"].clone();
                out["task_revision"] = task["revision"].clone();
                out["base"]["sha"] = task["base_sha"].clone();
            }
            if items.is_empty() {
                out["acceptance"]["warning"] = json!(self::acceptance_warning(&front.id, false));
            }
            return Ok(std::ops::ControlFlow::Break(out));
        }
        EarlyKickoff::ReuseJob { job } => {
            reuse_job = job["id"].as_str().map(str::to_string);
        }
    }
    // CAD-1822/CAD-1846: both entry points run the same mandatory pipeline.
    // The task contract and the project's required sources are validated
    // before `issue start`, so a refusal leaves no lane, branch, job or
    // queued message behind. The guard pins first, so its factory — the only
    // place a contract, spec or body is captured — never runs on opt-out.
    let mut context_active = false;
    let mut context_contract_digest = None;
    let mut context_lessons: Option<ContextFacts> = None;
    let body = if args.job_spec.is_none() {
        let summary = args.summary.as_deref().unwrap_or(&front.title);
        let prepared = context_guard(
            pm,
            &project,
            &front,
            ContextTarget {
                team_role: agent["team_role"].as_str(),
                provider: &provider,
                endpoint_kind: &endpoint_kind,
            },
            args.no_lessons,
            || {
                let body = plain_body(
                    &front,
                    summary,
                    &note,
                    &wt_dir,
                    &branch,
                    &base_sha,
                    &reply_to,
                    &items,
                    &provider,
                    &endpoint_kind,
                )?;
                let contract = crate::context_pack::TaskContract::new(
                    &front.id,
                    &front.title,
                    source.as_bytes(),
                    &items,
                    request,
                );
                Ok((contract, std::borrow::Cow::Owned(body)))
            },
        )?;
        match prepared {
            Some(prepared) => {
                context_active = true;
                context_contract_digest = Some(prepared.manifest.task.semantic_digest());
                context_lessons = Some(ContextFacts::from_manifest(&prepared.manifest));
                Some(prepared.body.into_owned())
            }
            None => Some(plain_body(
                &front,
                summary,
                &note,
                &wt_dir,
                &branch,
                &base_sha,
                &reply_to,
                &items,
                &provider,
                &endpoint_kind,
            )?),
        }
    } else {
        if let Some(spec) = &args.job_spec {
            // `issue start --job` scopes the task to the lane's dir name.
            let wt_name = wt_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (prototype_job, prototype_task) =
                job_prototype(&front.id, &items, spec, &wt_name, &branch, &base_sha);
            let placeholder = "0".repeat(32);
            let prepared = job_context(
                pm,
                GatedIssue {
                    project: project.clone(),
                    dir: dir.clone(),
                    front: front.clone(),
                    items: items.clone(),
                    source: source.clone(),
                    request: request.clone(),
                },
                JobContext {
                    job: &prototype_job,
                    task: &prototype_task,
                    revision: 1,
                    message: &placeholder,
                    target: ContextTarget {
                        team_role: agent["team_role"].as_str(),
                        provider: &provider,
                        endpoint_kind: &endpoint_kind,
                    },
                    // Preview only: `issue start` has not created this lane
                    // yet, so there is no admission proof to make.
                    admission_scope: None,
                },
                args.no_lessons,
            )?;
            if prepared.is_some() {
                // The daemon re-selects and pins the same mandatory contract
                // for the real task and message; the combined cap was checked
                // here against exact-length placeholders.
                context_active = true;
            } else {
                check_job_kickoff(
                    &front.id, &items, spec, &wt_name, &branch, &base_sha, &agent,
                )?;
            }
        }
        None
    };

    // `issue start` — idempotent; --owner <worker>, and for --job the
    // PM is the return address and the worker the assignee.
    Ok(std::ops::ControlFlow::Continue(Preflight {
        project,
        dir,
        front,
        items,
        acceptance,
        acceptance_warning,
        reply_to,
        note,
        show,
        provider,
        endpoint_kind,
        cwd_override,
        body,
        context_active,
        context_contract_digest,
        context_lessons,
        reuse_job,
    }))
}

struct Lessons {
    lessons: Vec<String>,
    lessons_file: Option<PathBuf>,
    lessons_error: Option<String>,
    lessons_withheld: Vec<(String, String)>,
    lessons_omitted: Vec<(String, String)>,
    body: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn lessons(
    mid: &str,
    pm: &Pm,
    project: &crate::issue::project::Project,
    dir: &Path,
    front: &Front,
    args: &DispatchArgs,
    state_dir: &Path,
    provider: &str,
    role: Option<&str>,
    body: Option<String>,
) -> Lessons {
    let mut lessons: Vec<String> = vec![];
    let mut lessons_file: Option<PathBuf> = None;
    let mut lessons_error: Option<String> = None;
    // In-scope guidance withheld by validity or role policy carries its
    // reason in the dispatch comment and output so absence is explainable.
    let mut lessons_withheld: Vec<(String, String)> = vec![];
    let mut body = body;
    if !args.no_lessons {
        if let Some(b) = body.as_mut() {
            let issue_obj = board::Issue {
                project: project.key.clone(),
                dir: dir.to_path_buf(),
                front: front.clone(),
                body: String::new(),
                comments: vec![],
                artifacts: vec![],
            };
            match memory::match_for_issue(pm, &issue_obj, Some(provider), role) {
                Err(e) => lessons_error = Some(format!("memory match failed: {e}")),
                Ok((matched, mem_errors)) => {
                    // Valid records still reach the worker; every file
                    // that failed to load is named on the issue.
                    if let Some(line) = memory::load_errors_line(&mem_errors) {
                        lessons_error = Some(line);
                    }
                    lessons_withheld = matched
                        .withheld
                        .iter()
                        .map(|(m, reason)| (m.front.id.clone(), reason.clone()))
                        .collect();
                    // Written whenever it has content: a dispatch whose
                    // every match was withheld still gets the file with
                    // only its Withheld section.
                    let (text, slugs) = memory::render_lessons(&matched);
                    if !text.is_empty() {
                        let ddir = state_dir.join("dispatch");
                        let file = ddir.join(format!("{mid}-lessons.md"));
                        // tmp + rename: a failed write never leaves a
                        // partial or empty lessons artifact behind.
                        let tmp = ddir.join(format!("{mid}-lessons.tmp"));
                        let prior = b.clone();
                        *b = format!("{prior} Lessons: {}.", file.display());
                        if let Err(e) = check_body(b, provider) {
                            *b = prior;
                            let why =
                                format!("lessons path pushed the kickoff over the body limit: {e}");
                            lessons_error = Some(match lessons_error {
                                Some(prev) => format!("{prev}; {why}"),
                                None => why,
                            });
                        } else if let Err(e) = std::fs::create_dir_all(&ddir)
                            .and_then(|_| std::fs::write(&tmp, &text))
                            .and_then(|_| std::fs::rename(&tmp, &file))
                        {
                            let _ = std::fs::remove_file(&tmp);
                            *b = prior;
                            let why = format!("lessons file unwritable: {e}");
                            lessons_error = Some(match lessons_error {
                                Some(prev) => format!("{prev}; {why}"),
                                None => why,
                            });
                        } else {
                            lessons = slugs;
                            lessons_file = Some(file);
                        }
                    }
                }
            }
        }
    }
    Lessons {
        lessons,
        lessons_file,
        // Every legacy error here can carry a file-controlled memory name
        // (load-error line, match failure, lessons path); redact once at the
        // construction boundary, before any output sink bounds it.
        lessons_error: lessons_error.as_deref().map(redacted_fact),
        lessons_withheld,
        // The legacy sidecar path has no pinned selection to compare against,
        // so it reports no omitted detail.
        lessons_omitted: vec![],
        body,
    }
}

struct RecordCtx<'a> {
    pm: &'a Pm,
    id: &'a str,
    args: &'a DispatchArgs,
    actor: &'a str,
    state_dir: &'a Path,
    front: &'a Front,
    started: &'a Value,
    message: &'a str,
    bootstrap_id: &'a str,
    bootstrap_state: Option<String>,
    fold: bool,
    lessons: Vec<String>,
    lessons_file: Option<PathBuf>,
    lessons_error: Option<String>,
    lessons_withheld: Vec<(String, String)>,
    lessons_omitted: Vec<(String, String)>,
    acceptance_warning: Option<String>,
    cwd_override: Option<String>,
    agent: &'a Value,
    note: &'a Path,
    dispatch_pm: Option<&'a str>,
    sent_state: (&'static str, Value),
    out: Value,
}

fn record(ctx: RecordCtx<'_>) -> Result<Value> {
    let RecordCtx {
        pm,
        id,
        args,
        actor,
        state_dir,
        front,
        started,
        message,
        bootstrap_id,
        bootstrap_state,
        fold,
        lessons,
        lessons_file,
        lessons_error,
        lessons_withheld,
        lessons_omitted,
        acceptance_warning,
        cwd_override,
        agent,
        note,
        dispatch_pm,
        sent_state,
        out,
    } = ctx;
    let mut out = out;
    let dispatch_record = match dispatch_pm {
        // An in-daemon dispatch writes the record itself — its pm is
        // the master's alias, not whatever a round-trip connection
        // would derive — and it may replace any prior record (the
        // daemon's own dispatch is the operator-grade writer).
        Some(pm_alias) => {
            let record = json!({
                "issue": front.id,
                "pm": pm_alias,
                "pm_kind": "daemon",
                "worker": args.to,
                "worktree": started["worktree"],
                "branch": started["branch"],
                "message": message,
                "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
            });
            crate::issue::areas::record_dispatch(state_dir, &front.id, record.clone(), true)
                .map(|_| record)
        }
        None => client::rpc(
            state_dir,
            "dispatch_record",
            json!({
                "issue": front.id,
                "message": message,
            }),
        ),
    };

    // The comment rides its own commit through the existing helper. A
    // second line records which lessons were injected.
    let mut comment_text = format!("Dispatched to {}: {}", args.to, note.display());
    if !lessons.is_empty() {
        comment_text.push_str(&format!("\nLessons injected: {}", lessons.join(", ")));
    }
    if !lessons_withheld.is_empty() {
        let items: Vec<String> = lessons_withheld
            .iter()
            .map(|(slug, reason)| format!("{slug} ({})", redacted_fact(reason)))
            .collect();
        comment_text.push_str(&format!("\nLessons withheld: {}", items.join("; ")));
    }
    if !lessons_omitted.is_empty() {
        let items: Vec<String> = lessons_omitted
            .iter()
            .map(|(slug, reason)| format!("{slug} ({})", redacted_fact(reason)))
            .collect();
        comment_text.push_str(&format!("\nLessons omitted: {}", items.join("; ")));
    }
    // A `--job` dispatch's override is already recorded by `issue
    // start`, which re-checks the assignee; the plain path records it
    // here, with the dispatch it allowed.
    if let Some(note) = cwd_override.as_ref().filter(|_| args.job_spec.is_none()) {
        comment_text.push_str(&format!("\n{note}"));
    }
    // CAD-159: the empty-acceptance warning is recorded once, on this
    // dispatch's own comment — a duplicate run returns before here.
    if let Some(warning) = &acceptance_warning {
        comment_text.push_str(&format!("\nAcceptance warning: {warning}"));
    }
    // CAD-467: the fold's outcome is recorded on the dispatch's own
    // comment — both ways. A cancelled row's history names this
    // kickoff; a refused cancel (the bootstrap was claimed first)
    // means this kickoff still carries the onboarding text while the
    // bootstrap turn runs too — at worst the worker reads it twice.
    match bootstrap_state.as_deref() {
        Some("folded") => comment_text.push_str(&format!(
            "\nBootstrap {bootstrap_id} folded into this kickoff — the queued \
             onboarding turn was cancelled; its instructions ride this message."
        )),
        Some(state) if fold => comment_text.push_str(&format!(
            "\nBootstrap {bootstrap_id} was meant to fold into this kickoff but \
             the cancel was refused (row is now {state}) — this kickoff carries \
             the onboarding text; the bootstrap turn delivers it again."
        )),
        _ => {}
    }
    // CAD-378: the advisory lease warnings, recorded with the dispatch
    // they were raised on — never a refusal.
    for line in crate::issue::areas::warning_lines(&started["leases"]) {
        comment_text.push_str(&format!("\nLease warning: {line}"));
    }
    let comment = write::add_comment(pm, id, &comment_text, None, Some("dispatch"), None, actor)?;

    // The worker's current probe verdict — pty only; the operator
    // learns whether delivery happens now or when the pane idles.
    let probe = if agent["endpoint_kind"].as_str() == Some("pty")
        && agent["dead"].as_bool() != Some(true)
    {
        client::rpc(state_dir, "agent_probe", json!({"alias": args.to})).ok()
    } else {
        None
    };

    out["dispatched"] = json!(true);
    out["dispatch_record"] = dispatch_record
        .as_ref()
        .map(|r| r.clone())
        .unwrap_or(Value::Null);
    out["dispatch_record_error"] = dispatch_record
        .as_ref()
        .err()
        .map(|e| json!(e.to_string()))
        .unwrap_or(Value::Null);
    out["message"] = json!(message);
    out[sent_state.0] = sent_state.1;
    out["lessons"] = json!(lessons);
    out["lessons_file"] = lessons_file
        .as_ref()
        .map(|p| json!(p))
        .unwrap_or(Value::Null);
    out["lessons_withheld"] = json!(lessons_withheld
        .iter()
        .map(|(slug, reason)| json!({"slug": slug, "reason": redacted_fact(reason)}))
        .collect::<Vec<_>>());
    out["lessons_omitted"] = json!(lessons_omitted
        .iter()
        .map(|(slug, reason)| json!({"slug": slug, "reason": redacted_fact(reason)}))
        .collect::<Vec<_>>());
    out["lessons_error"] = lessons_error
        .as_ref()
        // The common construction already redacts; this covers a pin written
        // before that boundary and any legacy variant. Redaction failure
        // yields the marker, never the raw value.
        .map(|e| json!(redacted_fact(e)))
        .unwrap_or(Value::Null);
    out["comment"] = comment["comment"].clone();
    // CAD-467: `folded` when the queued bootstrap was cancelled into
    // this kickoff; a live-but-unfolded bootstrap (`queued`/`running`)
    // means the kickoff delivers after it reports — surfaced so the PM
    // sees why this kickoff is not the lane's first turn.
    out["bootstrap"] = bootstrap_state.map(|s| json!(s)).unwrap_or(Value::Null);
    out["job"] = started["job"].clone();
    out["task"] = started["task"].clone();
    out["probe"] = probe.unwrap_or(Value::Null);
    out["cwd_override"] = cwd_override.map(Value::from).unwrap_or(Value::Null);
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn duplicate(
    pm: &Pm,
    id: &str,
    args: &DispatchArgs,
    actor: &str,
    state_dir: &Path,
    front: &Front,
    started: &Value,
    show_now: &Value,
    items: &[AcceptanceItem],
    next_body: Option<&str>,
    context_active: bool,
    next_contract_digest: Option<&str>,
    out: &mut Value,
) -> Result<bool> {
    let mode = if args.job_spec.is_some() {
        DispatchMode::Job
    } else {
        DispatchMode::Plain
    };
    let started_task = started["task"].as_str();
    let mut live: Option<Value> = None;
    let mut reported: Option<Value> = None;
    for (mid, owner) in message_refs(front, &args.to) {
        let Some(found) = message_row(state_dir, &owner, &args.to, show_now, &mid) else {
            continue;
        };
        // An arbitrary user-authored row is never a kickoff of either mode.
        let Some(found_mode) = row_mode(&found) else {
            continue;
        };
        let state = found["state"].as_str().unwrap_or_default();
        if LIVE_MESSAGE_STATES.contains(&state) {
            if found_mode != mode {
                // Another kind of kickoff already holds this lane; a second
                // one would run two tasks against it. Nothing more is queued.
                return Err(Error::rejected(format!(
                    "{}'s lane {} already has a live {} kickoff {} ({}) — wait for it \
                     to finish or reconcile it; no kickoff was queued",
                    front.id,
                    Path::new(started["worktree"].as_str().unwrap_or_default()).display(),
                    found_mode.label(),
                    found["id"].as_str().unwrap_or("?"),
                    state,
                )));
            }
            live = Some(found);
            break;
        }
        // The proof is the daemon-recorded row: this issue's kickoff,
        // delivered to THIS worker (the row's `alias` is the send's
        // target — a kickoff reported by another worker is their
        // history, not this dispatch's), on THIS lane's worktree, of THIS
        // dispatch's kind — and for a job, this exact native task, so a
        // sibling task can never suppress another.
        let reported_here = state == "completed"
            && found["alias"].as_str() == Some(args.to.as_str())
            && found["issue"].as_str() == Some(front.id.as_str())
            && match (found["worktree"].as_str(), started["worktree"].as_str()) {
                (Some(recorded), Some(lane)) => same_path(Path::new(recorded), Path::new(lane)),
                _ => false,
            }
            && found_mode == mode
            && (mode == DispatchMode::Plain || found["task_id"].as_str() == started_task);
        if reported_here {
            reported = Some(found);
        }
    }
    if let Some(msg) = live {
        out["dispatched"] = json!(false);
        out["duplicate"] = json!(true);
        out["duplicate_kind"] = json!("live");
        // CAD-300 (QA R4): a duplicate sends nothing, so its warning
        // must not say it dispatched.
        if items.is_empty() {
            out["acceptance"]["warning"] = json!(self::acceptance_warning(&front.id, false));
        }
        out["message"] = msg["id"].clone();
        out["message_state"] = msg["state"].clone();
        return Ok(true);
    }
    if let Some(msg) = reported {
        // An opted-in task's changed actionable contract/body is a revision,
        // not a duplicate. History is preserved: the old message and its ref stay,
        // and the new kickoff is added beside them. A live duplicate above
        // still suppresses, so an in-flight task is never silently retargeted,
        // and a non-opted project keeps the legacy reported-duplicate outcome
        // even when `--summary` changes the body.
        if let Some(next) = next_body {
            let same = msg["body"].as_str().is_some_and(|previous| {
                previous == next
                    || previous
                        .strip_prefix(next)
                        .is_some_and(|suffix| suffix.starts_with(" Lessons:"))
            });
            let same_contract = match next_contract_digest {
                Some(next) => {
                    let previous = client::rpc(
                        state_dir,
                        "agent_show",
                        json!({
                            "alias": args.to,
                            "limit": 0,
                            "context_message": msg["id"],
                        }),
                    )?;
                    previous["context_contract_digest"].as_str() == Some(next)
                }
                None => true,
            };
            if context_active && (!same || !same_contract) {
                out["revised"] = json!(true);
                out["previous_message"] = msg["id"].clone();
                let _ = write::add_comment(
                    pm,
                    id,
                    &format!(
                        "Revised dispatch to {}: kickoff {} already reported a \
                         different task revision; a new kickoff is dispatched and \
                         the earlier message/ref stay as history.",
                        args.to,
                        msg["id"].as_str().unwrap_or_default(),
                    ),
                    None,
                    Some("dispatch"),
                    None,
                    actor,
                );
                return Ok(false);
            }
        }
        // The kickoff's own report sha against the lane's head now —
        // equal is the cleanest late duplicate; either way the issue
        // was reported by this worker in this lane, so nothing
        // re-sends. Both sides are recorded so the PM can judge.
        let mid = msg["id"].as_str().unwrap_or_default();
        let reported_sha = msg["result"]["sha"].as_str();
        let via = msg["result"]["via"].as_str().unwrap_or("report");
        let lane_head = started["worktree"]
            .as_str()
            .and_then(|wt| lane_head(Path::new(wt)));
        let same_head = match (reported_sha, lane_head.as_deref()) {
            (Some(s), Some(h)) => Some(s == h),
            _ => None,
        };
        out["dispatched"] = json!(false);
        out["duplicate"] = json!(true);
        out["duplicate_kind"] = json!("reported");
        if items.is_empty() {
            out["acceptance"]["warning"] = json!(self::acceptance_warning(&front.id, false));
        }
        out["message"] = msg["id"].clone();
        out["message_state"] = msg["state"].clone();
        out["reported"] = json!({
            "sha": reported_sha,
            "via": via,
            "lane_head": lane_head,
            "same_head": same_head,
        });
        let _ = write::add_comment(
            pm,
            id,
            &format!(
                "Late duplicate dispatch suppressed: {} already reported \
                 kickoff {} (via {}, sha {}, lane head {}) — nothing re-sent.",
                args.to,
                mid,
                via,
                reported_sha.unwrap_or("none recorded"),
                lane_head.as_deref().unwrap_or("unreadable"),
            ),
            None,
            Some("dispatch"),
            None,
            actor,
        );
        return Ok(true);
    }
    Ok(false)
}

pub fn run(
    pm: &Pm,
    id: &str,
    args: &DispatchArgs,
    actor: &str,
    state_dir: &Path,
    dispatch_pm: Option<&str>,
    origin: DispatchOrigin,
) -> Result<Value> {
    let preflight = match preflight(pm, id, args, state_dir)? {
        std::ops::ControlFlow::Break(accepted) => return Ok(accepted),
        std::ops::ControlFlow::Continue(preflight) => preflight,
    };
    let Preflight {
        project,
        dir,
        front,
        items,
        acceptance,
        acceptance_warning,
        reply_to,
        note,
        show,
        provider,
        endpoint_kind,
        cwd_override,
        body,
        context_active,
        context_contract_digest,
        context_lessons,
        reuse_job,
    } = preflight;
    let agent = &show["agent"];
    let start_args = start::StartArgs {
        repo: args.repo.clone(),
        name: args.name.clone(),
        base: args.base.clone(),
        owner: Some(args.to.clone()),
        job: args.job_spec.as_ref().map(|spec| start::JobArgs {
            id: reuse_job,
            pm: reply_to.clone(),
            spec: spec.clone(),
            assignee: Some(args.to.clone()),
            force: args.force,
        }),
        by: Some(reply_to.clone()),
        take_over: args.take_over.clone(),
    };
    let started = start::run(pm, id, &start_args, actor, state_dir)?;

    // The body actually sent is rebuilt from start's returned refs and
    // re-checked: identical inputs make this a no-op, but nothing
    // unchecked ever reaches the pane.
    let body = body
        .map(|_| {
            let summary = args.summary.as_deref().unwrap_or(&front.title);
            let body = plain_kickoff(
                &front.id,
                summary,
                &note,
                Path::new(started["worktree"].as_str().unwrap_or_default()),
                started["branch"].as_str().unwrap_or_default(),
                started["base"]["sha"].as_str().unwrap_or_default(),
                &reply_to,
                &items,
                &provider,
                &endpoint_kind,
            )?;
            check_body(&body, &provider).map(|_| body)
        })
        .transpose()?;

    // Duplicate dispatch: a previously recorded `message` ref still
    // live means a kickoff is in flight — reuse the worktree, queue
    // nothing, say so. The ref names the worker it went to, so a
    // re-dispatch to a DIFFERENT worker is still caught.
    // CAD-467: a ref whose kickoff was already REPORTED is the late
    // duplicate: the work it kicks off is done, so nothing re-sends and
    // the detection is recorded on the issue.
    //
    // Review round: the tracker is forgeable — a planted
    // `refs: [{kind: message, path: <id>}]` must never suppress a real
    // kickoff. The ref only LOCATES the candidate message; the proof is
    // on the message row itself, recorded by the daemon at send:
    // `issue` must be this issue and `worktree` must be this lane's —
    // both present, both equal. A row missing either (an old kickoff,
    // or any unrelated completed message a ref was pointed at) never
    // matches. A kickoff whose recorded worktree is a different lane is
    // that lane's history, not this dispatch's.
    let mut out = json!({
        "issue": front.id,
        "worker": args.to,
        "reply_to": reply_to,
        "note": note,
        "worktree": started["worktree"],
        "branch": started["branch"],
        "base": started["base"],
        "created": started["created"],
        "target_dir": started["target_dir"],
        "slot_env": started["slot_env"],
        "acceptance": acceptance,
        "claim": started["claim"],
        "leases": started["leases"],
    });
    // A fresh read on the target — `show` predates `issue start`, and
    // a launch finishing mid-dispatch can have enqueued a bootstrap.
    let show_now = client::rpc(state_dir, "agent_show", json!({"alias": args.to}))
        .unwrap_or_else(|_| show.clone());
    if duplicate(
        pm,
        id,
        args,
        actor,
        state_dir,
        &front,
        &started,
        &show_now,
        &items,
        body.as_deref(),
        context_active,
        context_contract_digest.as_deref(),
        &mut out,
    )? {
        return Ok(out);
    }

    // The message id is minted up front so the `message` ref commits BEFORE
    // the send: a finish racing the dispatch sees the binding the moment the
    // ref lands (CAD-107).
    let mid = Uuid::new_v4().simple().to_string();
    // Project-memory lessons: accepted memories matching the issue's
    // component/tags/recorded-commit paths and the worker's provider render
    // into `<state>/dispatch/<message>-lessons.md`, written before the send
    // so the kickoff can name it. An opted-in dispatch already carries that
    // same selection inside its pinned context, so the legacy sidecar and
    // its `Lessons:` suffix are skipped there — never injected twice — and
    // `--job`'s kickoff likewise carries it in the pinned task context.
    // Memory failures never sink the dispatch — the worktree already exists
    // by now. A malformed file, an unwritable lessons file or a suffix that
    // pushes the body over the cap degrades to no lessons with
    // `lessons_error` naming the reason.
    let Lessons {
        lessons,
        lessons_file,
        lessons_error,
        lessons_withheld,
        lessons_omitted,
        body,
    } = match context_lessons {
        Some(facts) => Lessons {
            lessons: facts.lessons,
            lessons_file: None,
            lessons_error: facts.lessons_error,
            lessons_withheld: facts.lessons_withheld,
            lessons_omitted: facts.lessons_omitted,
            body,
        },
        None => lessons(
            &mid,
            pm,
            &project,
            &dir,
            &front,
            args,
            state_dir,
            &provider,
            agent["team_role"].as_str(),
            body,
        ),
    };

    // CAD-467: a freshly joined worker's `bootstrap-<alias>` still
    // `queued` would deliver as a bare onboarding turn AHEAD of this
    // kickoff — and a worker that treated it as the task would then
    // meet the kickoff as a second statement of the same work. Fold it
    // instead: carry its body verbatim ahead of the kickoff, then —
    // once the kickoff is safely queued — cancel the bootstrap row, so
    // the lane's first turn is the task and still teaches identity,
    // briefing path and the report command. A bootstrap already
    // `running` keeps its turn — this kickoff queues behind it — and a
    // folded body that outgrows the pty ceiling leaves the bootstrap
    // to deliver on its own. `--job` kickoffs are daemon-templated and
    // cannot carry it, so the fold is plain-path only.
    //
    // Ordering (review): the cancel is the LAST step. The fallible
    // writes — the ref and the send — run while the bootstrap is still
    // queued, so a failure on either leaves onboarding untouched
    // rather than stranding a cancelled row (a cancelled
    // `bootstrap-<alias>` cannot be re-issued — the deterministic id
    // dedupes the restore). The cancel itself is state-guarded: a
    // bootstrap claimed in the meantime refuses it, the folded kickoff
    // still carries the onboarding, and the kickoff queues behind —
    // at worst the worker sees the same text twice, never zero times.
    let bootstrap_id = format!("bootstrap-{}", args.to);
    let mut send_body = body;
    let mut bootstrap_state: Option<String> = None;
    let mut fold = false;
    if let Some(boot) = send_body.as_ref().and_then(|_| {
        show_now["messages"].as_array().and_then(|ms| {
            ms.iter()
                .find(|m| m["id"].as_str() == Some(bootstrap_id.as_str()))
        })
    }) {
        bootstrap_state = boot["state"].as_str().map(str::to_string);
        if bootstrap_state.as_deref() == Some("queued") {
            if let (Some(boot_body), Some(b)) = (boot["body"].as_str(), send_body.as_deref()) {
                let folded = format!("{boot_body} {b}");
                if check_body(&folded, &provider).is_ok() {
                    send_body = Some(folded);
                    fold = true;
                }
            }
        }
    }

    // The ref lands before the send, naming the worktree the kickoff
    // runs against — a re-start under `--name` must not inherit it.
    // If the send then fails the ref stays as the attempt's history
    // (a ref id no live message ever matches blocks nothing) and the
    // failure comment below records it.
    let add_ref_failed = |e: Error| -> Error {
        if fold {
            let _ = write::add_comment(
                pm,
                id,
                &format!(
                    "Dispatch to {} failed recording the kickoff ref: {e} — \
                     bootstrap {bootstrap_id} was never touched; it still \
                     delivers as onboarding.",
                    args.to
                ),
                None,
                Some("dispatch"),
                None,
                actor,
            );
        }
        e
    };
    write::add_ref(
        pm,
        id,
        "message",
        &mid,
        Some(&format!("dispatch → {}", args.to)),
        started["worktree"].as_str(),
        Some(&args.to),
        None,
        actor,
    )
    .map_err(add_ref_failed)?;
    let send_failed = |e: Error| -> Error {
        // The ref recorded pre-send is an orphan — no live message
        // will ever carry `mid`. Close it so it stays history without
        // counting as a binding (or a mid-finish "dispatch recorded"
        // stale reason) for a concurrent finish; best-effort — the
        // send error is the one that matters.
        let _ = write::close_ref(pm, id, "message", &mid, actor);
        // CAD-467: with the cancel moved behind the send, a failed
        // send leaves a fold-intended bootstrap still queued — say so,
        // so nobody goes looking for a restore that is not needed.
        let folded = if fold {
            format!(
                " (bootstrap {bootstrap_id} was left queued — it still \
                 delivers as onboarding)"
            )
        } else {
            String::new()
        };
        let _ = write::add_comment(
            pm,
            id,
            &format!("Dispatch send to {} failed: {e}{folded}", args.to),
            None,
            Some("dispatch"),
            None,
            actor,
        );
        e
    };

    // Exactly one send — plain text kickoff, or `job dispatch`'s
    // spec-bound kickoff for --job (its state lives on the task). Both
    // record the lane on the message row — `issue`/`worktree` via
    // `dispatch_send`, which resolves the lane itself and only checks
    // the worktree this dispatch started (CAD-378 R6: no caller field
    // can set them), the job's own fields in `task_dispatch` — so a
    // later re-dispatch can prove this kickoff's report belongs to
    // THIS issue and lane (CAD-467).
    let (message, sent_state) = if let Some(body) = send_body {
        // `no_lessons` is the caller's existing opt-out, carried to the
        // daemon so its own context selection honors it too.
        let mut send = json!({"alias": args.to, "text": body, "reply_to": reply_to,
                              "message": mid, "no_lessons": args.no_lessons,
                              "issue": front.id, "worktree": started["worktree"]});
        if matches!(origin, DispatchOrigin::Automatic) {
            send["unattended"] = json!(true);
        }
        let sent = client::rpc(state_dir, "dispatch_send", send).map_err(send_failed)?;
        (
            sent["message"].as_str().unwrap_or_default().to_string(),
            ("message_state", sent["state"].clone()),
        )
    } else {
        let task = started["task"].as_str().unwrap_or_default().to_string();
        let sent = client::rpc(
            state_dir,
            "task_dispatch",
            json!({"task": task, "by": reply_to, "message": mid,
                   "no_lessons": args.no_lessons}),
        )
        .map_err(send_failed)?;
        (
            sent["message"].as_str().unwrap_or_default().to_string(),
            ("task_state", sent["task"]["state"].clone()),
        )
    };
    // A same-revision `task_dispatch` retry ignores the minted id and
    // returns the still-live kickoff's id — bind THAT message too so
    // the live one is never unbound. The pre-send ref stays as the
    // attempt's history. An absent `message` in the reply is `""` —
    // never a ref target.
    if !message.is_empty() && message != mid {
        write::add_ref(
            pm,
            id,
            "message",
            &message,
            Some(&format!("dispatch → {}", args.to)),
            started["worktree"].as_str(),
            Some(&args.to),
            None,
            actor,
        )?;
    }

    // The kickoff is safely queued — now the fold's cleanup: cancel
    // the still-queued bootstrap so onboarding does not deliver twice.
    // The guard refuses a bootstrap claimed in the meantime; report the
    // row's actual state (best-effort re-read — a claimed bootstrap
    // reads `running`, the common case) so `bootstrap` never claims a
    // fold that did not happen.
    if fold {
        match client::rpc_relay(
            state_dir,
            "message_cancel",
            json!({"message": bootstrap_id, "by": reply_to,
                   "reason": format!("folded into kickoff {mid} for {}", front.id)}),
        ) {
            Ok(_) => bootstrap_state = Some("folded".to_string()),
            Err(_) => {
                bootstrap_state = client::rpc(state_dir, "agent_show", json!({"alias": args.to}))
                    .ok()
                    .and_then(|s| {
                        s["messages"].as_array()?.iter().find_map(|m| {
                            if m["id"].as_str() == Some(bootstrap_id.as_str()) {
                                m["state"].as_str().map(str::to_string)
                            } else {
                                None
                            }
                        })
                    })
                    .or(Some("running".to_string()));
            }
        }
    }

    // CAD-378: the daemon records what this dispatch bound — worktree,
    // branch, worker and pm — in dispatches.json, derived from the
    // kickoff message the daemon just delivered (never request fields).
    // That record, not anything the tracker carries, binds the lane's
    // area-owner rows. A failure is reported, never silent: the kickoff
    // already went out, and an unbound lane's rows stay up (fail loud).
    record(RecordCtx {
        pm,
        id,
        args,
        actor,
        state_dir,
        front: &front,
        started: &started,
        message: &message,
        bootstrap_id: &bootstrap_id,
        bootstrap_state,
        fold,
        lessons,
        lessons_file,
        lessons_error,
        lessons_withheld,
        lessons_omitted,
        acceptance_warning,
        cwd_override,
        agent,
        note: &note,
        dispatch_pm,
        sent_state,
        out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kickoff() -> String {
        kickoff_body(
            "D-1",
            "Title",
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-1-title"),
            "cadence/d-1-title",
            "0123456789abcdef",
            "pm",
            "devin",
            "pty",
        )
    }

    /// CAD-159: an empty section and a stub-only section both yield no
    /// items — no listing, the kickoff is unchanged, and the warning
    /// names the issue and the authoring command.
    #[test]
    fn empty_and_stub_only_acceptance_has_no_listing() {
        for body in [
            "Title\n\n## Acceptance\n\n",
            "Title\n\n## Acceptance\n\n- [ ]\n- [x]   \n",
        ] {
            let items = parse::acceptance_items(body);
            assert!(items.is_empty(), "{body:?}");
            assert_eq!(acceptance_listing(&items), None);
            assert_eq!(with_acceptance(kickoff(), &items), kickoff());
        }
        let warning = acceptance_warning("D-1", true);
        assert!(
            warning.contains("D-1")
                && warning.contains("cadence issue acceptance D-1 --from <file>"),
            "{warning}"
        );
    }

    /// Populated acceptance is listed inline, one line, numbered and
    /// quoted, checked state kept, control characters escaped.
    #[test]
    fn populated_acceptance_is_listed_inline() {
        let body = "T\n\n## Acceptance\n\n- [ ] first\tpart\n- [x] second\n\n## Notes\n- [ ] not acceptance\n";
        let items = parse::acceptance_items(body);
        assert_eq!(
            acceptance_listing(&items).as_deref(),
            Some(r#"1) [ ] "first\tpart"; 2) [x] "second""#)
        );
        let body = with_acceptance(kickoff(), &items);
        assert!(
            body.ends_with(r#" Acceptance: 1) [ ] "first\tpart"; 2) [x] "second"."#),
            "{body}"
        );
        check_body(&body, "fake").unwrap();
    }

    /// CAD-300: item text a reader could mistake for the listing's own
    /// syntax — `; `, `[x]`, a `2)` number, quotes, backslashes — and
    /// characters a single-line pty body cannot carry (tab, ESC, DEL,
    /// NEL, U+2028/U+2029) round-trip exactly through the kickoff, which
    /// stays one line and control-character free.
    #[test]
    fn tricky_items_round_trip_through_the_kickoff() {
        let items = vec![
            AcceptanceItem {
                text: "alpha; [x] beta".into(),
                checked: false,
            },
            AcceptanceItem {
                text: r#"done; 3) [ ] "fake" \ C:\path"#.into(),
                checked: true,
            },
            AcceptanceItem {
                text: "tab\there esc\u{1b}[31m del\u{7f} nel\u{85} ls\u{2028}ps\u{2029}end".into(),
                checked: false,
            },
            AcceptanceItem {
                text: "[x] looks checked but is not".into(),
                checked: false,
            },
        ];
        let listing = acceptance_listing(&items).unwrap();
        let (back, rest) = parse_acceptance_listing(&listing)
            .unwrap_or_else(|| panic!("listing does not parse: {listing}"));
        assert_eq!(back, items, "{listing}");
        assert_eq!(rest, "", "{listing}");
        let body = with_acceptance(kickoff(), &items);
        check_body(&body, "fake").unwrap();
        assert!(
            !body
                .chars()
                .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')),
            "{body:?}"
        );
        let tail = body.split_once(" Acceptance: ").unwrap().1;
        let (back, rest) = parse_acceptance_listing(tail).unwrap();
        assert_eq!(back, items, "{body}");
        assert_eq!(rest, ".", "{body}");
    }

    /// CAD-160 (replaces CAD-300 QA R3's "sent without the clause"):
    /// a near-limit `--summary` is prose, so it gives way — shortened,
    /// ending in `…` — and every acceptance item goes out whole.
    #[test]
    fn long_summary_gives_way_to_whole_criteria() {
        let summary = "s".repeat(3800);
        let items = vec![AcceptanceItem {
            text: "y".repeat(200),
            checked: false,
        }];
        let body = plain_kickoff(
            "D-1",
            &summary,
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-1-title"),
            "cadence/d-1-title",
            "0123456789abcdef",
            "pm",
            &items,
            "devin",
            "pty",
        )
        .unwrap();
        check_body(&body, "fake").unwrap();
        assert!(body.len() <= pty::MAX_BODY, "{} bytes", body.len());
        assert!(body.contains("D-1: sss"), "{body}");
        assert!(body.contains("s…. Your worktree exists"), "{body}");
        assert!(
            body.ends_with(&format!(" Acceptance: 1) [ ] \"{}\".", "y".repeat(200))),
            "{body}"
        );
    }

    /// CAD-1373: the business-knowledge pointer rides the plain kickoff.
    /// It is short, the ceiling fit still cuts prose first, and the
    /// acceptance tail survives a near-limit title.
    #[test]
    fn plain_kickoff_points_company_questions_at_the_skill_and_keeps_acceptance() {
        let items = vec![AcceptanceItem {
            text: "z".repeat(150),
            checked: false,
        }];
        let body = plain_kickoff(
            "D-2",
            &"t".repeat(3800),
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-2-title"),
            "cadence/d-2-title",
            "0123456789abcdef",
            "pm",
            &items,
            "devin",
            "pty",
        )
        .unwrap();
        check_body(&body, "fake").unwrap();
        assert!(body.len() <= pty::MAX_BODY, "{} bytes", body.len());
        assert!(body.contains("Business knowledge"), "{body}");
        assert!(
            body.ends_with(&format!(" Acceptance: 1) [ ] \"{}\".", "z".repeat(150))),
            "{body}"
        );
    }

    /// CAD-300 (QA R4): a duplicate run dispatched nothing, so its
    /// warning must not say "Dispatched anyway".
    #[test]
    fn duplicate_warning_does_not_claim_a_dispatch() {
        let sent = acceptance_warning("D-1", true);
        let dup = acceptance_warning("D-1", false);
        assert!(sent.ends_with("Dispatched anyway."), "{sent}");
        assert!(!dup.contains("Dispatched anyway"), "{dup}");
        for w in [&sent, &dup] {
            assert!(
                w.contains("cadence issue acceptance D-1 --from <file>"),
                "{w}"
            );
        }
    }

    /// CAD-160 (replaces the CAD-159 readback pointer): a list that
    /// cannot fit whole refuses — plain and `--job` alike — naming the
    /// ceiling and the note or spec file; it is never replaced by a
    /// pointer. Both checks run before `issue start`.
    #[test]
    fn criteria_that_cannot_fit_refuse_naming_ceiling_and_file() {
        let items: Vec<AcceptanceItem> = (0..60)
            .map(|i| AcceptanceItem {
                text: format!("criterion {i} {}", "x".repeat(80)),
                checked: false,
            })
            .collect();
        let err = plain_kickoff(
            "D-1",
            "Title",
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-1-title"),
            "cadence/d-1-title",
            "0123456789abcdef",
            "pm",
            &items,
            "devin",
            "pty",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("note /tmp/note.md"), "{err}");
        assert!(err.contains("Nothing was created or queued"), "{err}");
        let err = job_check(&items, "pty").unwrap_err().to_string();
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("spec file /tmp/spec.md"), "{err}");
        job_check(&items[..10], "pty").unwrap();
    }

    fn items(n: usize) -> Vec<AcceptanceItem> {
        (0..n)
            .map(|i| AcceptanceItem {
                text: format!("criterion {i} {}", "x".repeat(80)),
                checked: false,
            })
            .collect()
    }

    /// The `--job` pre-check against a `devin/<kind>` assignee.
    fn job_check(items: &[AcceptanceItem], kind: &str) -> Result<()> {
        check_job_kickoff(
            "D-2",
            items,
            Path::new("/tmp/spec.md"),
            "d-2-job",
            "cadence/d-2-job",
            &"0".repeat(40),
            &json!({"provider": "devin", "endpoint_kind": kind}),
        )
    }

    /// CAD-160 (QA N1): the `--job` pre-check builds the kickoff the
    /// daemon will send, so a listing that fits alone but not beside
    /// the fixed fields (QA's 37-item probe) refuses before
    /// `issue start` — naming the criteria, not a sender's text. (QA
    /// N6) a Devin cloud assignee is held to its own 48000 ceiling.
    #[test]
    fn job_pre_check_reserves_the_kickoffs_fixed_fields() {
        // QA's probe: 37 items — a 3864-byte listing, 3878 bytes as the
        // kickoff's ` Acceptance: ….` clause — under 4000 alone.
        let gap = items(37);
        assert_eq!(acceptance_listing(&gap).unwrap().len(), 3864);
        let err = job_check(&gap, "pty").unwrap_err().to_string();
        assert!(
            err.contains("D-2's acceptance criteria (3864 bytes)"),
            "{err}"
        );
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("spec file /tmp/spec.md"), "{err}");
        assert!(err.contains("Nothing was created or queued"), "{err}");
        assert!(!err.contains("text"), "{err}");
        // A cloud kickoff carries 48000: the same list, and one past
        // 4000, go out; one past 48000 does not.
        job_check(&gap, "cloud").unwrap();
        job_check(&items(60), "cloud").unwrap();
        let err = job_check(&items(600), "cloud").unwrap_err().to_string();
        assert!(err.contains("48000-char"), "{err}");
    }

    /// CAD-160: the outstanding criteria of a stored task acceptance.
    #[test]
    fn outstanding_items_keep_only_unchecked_criteria() {
        let items = vec![
            AcceptanceItem {
                text: "a; 2) [ ] \"b\"".into(),
                checked: false,
            },
            AcceptanceItem {
                text: "done".into(),
                checked: true,
            },
        ];
        let listing = inline_listing(&items);
        assert_eq!(outstanding_items(Some(&listing)), items[..1].to_vec());
        // Free-form text, or a listing with trailing prose, is one item.
        for text in ["green tests", &format!("{listing} and more")] {
            assert_eq!(
                outstanding_items(Some(text)),
                vec![AcceptanceItem {
                    text: text.to_string(),
                    checked: false
                }]
            );
        }
        assert!(outstanding_items(None).is_empty());
        assert!(outstanding_items(Some("  ")).is_empty());
    }
}
