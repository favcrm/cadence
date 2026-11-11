//! `/api/stream` — server-sent events. One open connection per tab
//! carries the read model's invalidation frames: `stream_events`
//! subscribes and writes each frame straight onto the socket until the
//! client, watcher, or live-session validation closes it.
//!
//! The watcher is shared and its frames are the operator's: an issue entity
//! carries the operator's card, an `issues` frame is taken from a tracker-wide
//! stamp, and the `jobs`, `agents`, `monitoring` and `threads` frames from
//! operator-wide fingerprints. A signed member therefore never gets a shared
//! frame as written, and never gets one merely because the entity it names is
//! readable: frames are decided by comparing the reader's projected
//! representation before and after. The projections use the corresponding
//! member HTTP reads and close the stream when their scope cannot be resolved.
//! This is not yet a complete no-signal guarantee: delivery provenance and
//! globally allocated sequence values still need scoped contracts.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tiny_http::Request;

use super::read_model;
use crate::issue::{project, Pm};
use crate::project_policy;

/// Newest mtime among regular files under `dir` — the tracker-change
/// fingerprint. Small tree; a full walk every poll is still cheap.
pub(crate) fn dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let m = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if newest.is_none_or(|n| m > n) {
                    newest = Some(m);
                }
            }
        }
    }
    newest
}

/// A cheap content fingerprint for a JSON value — the serialized bytes
/// through a stable hasher (no new dependency for one hash).
pub(crate) fn value_fp(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(value).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// The exact collection representation, shared with SSE aggregate change
/// detection so config/count changes invalidate and title-only edits do not.
/// Counts come from the tracker's already-loaded issues — a title edit must
/// not re-parse every folder to learn that the counts did not move.
pub(crate) fn projects_payload(pm: &Pm, counts: &HashMap<String, usize>) -> Value {
    let projects = project::list(&pm.dir).unwrap_or_default();
    let payload: Vec<Value> = projects.iter().map(|p| json!({
        "key": p.key, "prefix": p.prefix, "components": p.components,
        "tags": p.tags, "default_owner": p.default_owner,
        "repos": p.repos.iter().map(|r| json!({"path": r.path, "remote": r.remote})).collect::<Vec<_>>(),
        "issues": counts.get(&p.key).copied().unwrap_or(0),
    })).collect();
    json!({"projects": payload})
}

/// The board resources one stream event invalidates, sent as the frame's
/// data (`{"resources":[...]}`) so the client refetches only those. The
/// event name stays the change source, which older clients key on.
/// - tracker files → the cards, the per-project list, the open issue
///   and the overview's tracker rows;
/// - jobs → card status (job outcomes) and agent task bindings;
/// - agents → agent rows and their `by_issue` strip (cards read their
///   agent chips from it), the open issue's agents, overview needs;
/// - monitors → the overview's monitoring block only.
pub(crate) fn event_resources(name: &str) -> &'static [&'static str] {
    match name {
        "issues" => &[
            "issues",
            "projects",
            "issue",
            "overview",
            "workflows",
            "apps",
            "app",
            "app_runs",
            "outbox",
            "app_outputs",
            // CAD-1950: a project's recorded policy is a separate read, not a
            // collection the entity frame patches, so it survives the filter.
            "projectPolicy",
        ],
        "jobs" => &[
            "issues",
            "agents",
            "issue",
            "overview",
            "app_runs",
            "outbox",
            "app_outputs",
        ],
        // A released publish lands an outbox item — the same event the
        // effect row's state change produces. Agent rows change what an
        // app's workflow checks resolve to, so apps refetch too.
        "agents" => &["agents", "issue", "overview", "outbox", "apps", "app"],
        "monitoring" => &["overview"],
        // CAD-818: the Threads view's own invalidation. It names only the
        // `threads` resource, so no existing stream refetches more.
        "threads" => &["threads"],
        // CAD-1700: the master view's own invalidation. It names only the
        // `master` resource, so no existing stream refetches more.
        "master" => &["master"],
        _ => &[
            "issues",
            "projects",
            "agents",
            "issue",
            "overview",
            "workflows",
            "apps",
            "app",
            "app_runs",
            "outbox",
            "app_outputs",
        ],
    }
}

/// `GET /api/stream` — server-sent events written straight onto the
/// socket. tiny_http's chunked path buffers small writes inside
/// `chunked_transfer::Encoder` (it flushes only on `flush()` or a full
/// chunk), so a reader-based `Response` would never emit a small SSE
/// frame. `into_writer` hands over the socket: the head is written by
/// hand, each frame flushes immediately, and dropping the writer on
/// exit closes the stream — which is also how a dead client surfaces.
pub(crate) fn stream_hello(entities: bool) -> String {
    let mut data = json!({"build": crate::overview::BUILD_ID});
    if entities {
        data["entities"] = json!(true);
    }
    format!("event: hello\ndata: {data}\n\n")
}

/// True when this reader's live projection can be established at all: a
/// known project-policy caller on a tracker the board can check. A reader
/// that fails this is refused the stream before its head, exactly as a
/// refused read is — never handed an apparently live stream that can only
/// stay silent.
pub(super) fn member_stream_available(pm_dir: &Path, actor: &str) -> bool {
    project_policy::caller_of(actor).is_some()
        && Pm::at(pm_dir).is_ok_and(|pm| project::list_checked(&pm.dir).is_ok())
}

/// A legacy invalidation frame, the shape every client keys on.
fn legacy_frame(name: &str) -> String {
    format!(
        "event: {name}\ndata: {}\n\n",
        json!({"resources": event_resources(name)})
    )
}

/// An entity frame, plus the `entities=1` client's resource/rev patch.
fn entity_text(kind: &str, data: &Value, entities: bool) -> String {
    let text = format!("event: {kind}\ndata: {data}\n\n");
    if entities {
        read_model::entity_frame(&text)
    } else {
        text
    }
}

/// The shared watcher's frames, as a restricted reader must decide them.
/// Anything this does not name is refused: an unproven shape never rides a
/// signed member's socket.
enum SharedFrame {
    /// The tick's boundary marker; never written to a socket.
    Heartbeat,
    /// A legacy invalidation (`{"resources":[...]}`), by event name.
    Legacy(String),
    /// An entity diff: the kind and its parsed data.
    Entity(&'static str, Value),
    /// The daemon's aggregate: `by_issue`/`totals`/`profiles` on a normal
    /// tick, freshness alone while the daemon is unreachable.
    AgentMeta(Value),
    /// The aggregate frame only the operator's entity client receives.
    Aggregates,
}

fn shared_frame(text: &str) -> Option<SharedFrame> {
    if text.is_empty() {
        return Some(SharedFrame::Heartbeat);
    }
    let (head, tail) = text.split_once("\ndata: ")?;
    let name = head.strip_prefix("event: ")?;
    let data: Value = serde_json::from_str(tail.trim()).ok()?;
    Some(match name {
        "issues" | "jobs" | "agents" | "monitoring" | "threads" | "master" => {
            SharedFrame::Legacy(name.to_owned())
        }
        "issue" => SharedFrame::Entity("issue", data),
        "plan" => SharedFrame::Entity("plan", data),
        "agent" => SharedFrame::Entity("agent", data),
        "agent_meta" => SharedFrame::AgentMeta(data),
        "aggregates" => SharedFrame::Aggregates,
        _ => return None,
    })
}

/// The reader's own card/plan fingerprint: the entity it would be served, its
/// display clock removed, plus the issue folder stamp the shared diff uses, so
/// a body, comment or artifact edit still invalidates while another reader's
/// job outcome (which the member card does not carry) does not.
fn card_fp(
    read: &read_model::BoardRead,
    ctx: &crate::issue::work::Ctx,
    view: &crate::issue::board::View,
) -> (u64, Value) {
    let card = read.card(ctx, view);
    let mut stable = card.clone();
    if stable["claim"].is_object() {
        stable["claim"]["age_secs"] = Value::Null;
    }
    (
        value_fp(&json!([stable, read_model::folder_stamp(&view.issue.dir)])),
        card,
    )
}

/// One shared agent row through the member HTTP projection: the row the
/// reader's own `/api/agents` would serve, or `None` when the row is not
/// the reader's to see. A tombstone is an invalidation, not an alias
/// disclosure.
fn scoped_agent_row(
    row: &Value,
    bindings: &HashMap<String, super::serve::TaskBinding>,
    scope: &super::serve::MemberAgentScope,
) -> Option<Value> {
    let alias = row["alias"].as_str()?;
    let mut payload = json!({"agents": [row]});
    super::serve::restrict_member_agent_rows(&mut payload, scope);
    super::serve::scope_member_agents(&mut payload, bindings, scope);
    let row = payload["agents"].as_array_mut()?.pop()?;
    Some(json!({"op": "upsert", "id": alias, "agent": row}))
}

/// The fingerprint a member's agent row is judged by. The row is already the
/// member projection — the view's allowlist with `scope_member_agents`'
/// clock-derived fields removed — so this is stable across ticks and moves
/// only when something this reader may see changed.
fn agent_row_fp(row: &Value) -> u64 {
    value_fp(row)
}

/// The reader's own project listing: config and issue counts for the
/// projects it may read, and nothing at all for the others — not their row,
/// their count or their existence.
fn visible_projects_fp(model: &read_model::Model, pm: &Pm, readable: &BTreeSet<String>) -> u64 {
    let mut payload = model.projects(pm);
    if let Some(rows) = payload["projects"].as_array_mut() {
        rows.retain(|row| {
            row["key"]
                .as_str()
                .is_some_and(|key| readable.contains(key))
        });
    }
    value_fp(&payload)
}

/// The delivery records this reader may see. `delivery.json` is written for
/// the whole board, so its file stamp is not this reader's change: a record
/// belongs to a project, and only a readable project's records can move the
/// reader's own overview rows. `None` means the file cannot be scoped at all
/// (unreadable or malformed), so the stream must close.
///
/// CAD-2215: only a record standing on an exact native dispatch receipt is
/// shared. A legacy candidate is operator recovery evidence and a private
/// job delivery lives in its own ledger, so neither can move this
/// fingerprint — hidden activity stays silent, while a verified ordinary
/// change still reaches every reader entitled to it.
fn visible_delivery_fp(state_dir: &Path, readable: &BTreeSet<String>) -> Option<u64> {
    let records = crate::delivery::load(state_dir).ok()?;
    let mut visible = serde_json::Map::new();
    for (issue, record) in records {
        if record.origin.is_verified_ordinary() && readable.contains(&record.project) {
            visible.insert(issue, serde_json::to_value(record).ok()?);
        }
    }
    Some(value_fp(&Value::Object(visible)))
}

/// An alert's `age_secs` is the display clock, not a change.
fn strip_alert_ages(alerts: &mut Value) {
    if let Some(rows) = alerts.as_array_mut() {
        for row in rows {
            if let Some(fields) = row.as_object_mut() {
                fields.remove("age_secs");
                // Opaque ack tokens are randomized; they are not activity.
                fields.remove("ack");
            }
        }
    }
}

/// The reader's own monitoring block — the projection `/api/overview` gives
/// it, over the tasks its job bindings prove it may read — with the alert
/// ages left out of the fingerprint.
fn monitoring_fingerprint(
    state_dir: &Path,
    bindings: &HashMap<String, super::serve::TaskBinding>,
    proof: &super::operator::ReadRelayProof,
) -> u64 {
    let allowed: HashSet<String> = bindings.keys().cloned().collect();
    let mut relay = json!({});
    proof.attach(&mut relay);
    let mut value = crate::overview::monitoring_for_tasks(state_dir, &allowed, &relay);
    strip_alert_ages(&mut value["alerts"]);
    if let Some(monitors) = value["monitors"].as_array_mut() {
        for monitor in monitors {
            strip_alert_ages(&mut monitor["alerts"]);
        }
    }
    value_fp(&value)
}

/// The thread pages this reader may read, by daemon-verified alias: the same
/// scoped tail page the agent chat fetches, and nothing else. A shared thread
/// stamp leaves this fingerprint alone when the movement was another reader's
/// (or another reader's private job's), so only a change the chat would show
/// invalidates it. `None` means the page cannot be read, so the stream must
/// close rather than silently stop reporting this reader's updates.
fn visible_threads_fp(
    state_dir: &Path,
    proof: &super::operator::ReadRelayProof,
    scope: &super::serve::MemberAgentScope,
) -> Option<u64> {
    let mut aliases: Vec<String> = scope.aliases().cloned().collect();
    aliases.sort();
    let budget = crate::client::MetaBudget::fresh(crate::client::MetaBudget::LIMIT);
    let mut pages = Vec::with_capacity(aliases.len());
    for alias in aliases {
        // The chat's own first page; a hidden row the projection drops is
        // absent here exactly as it is absent there.
        let mut params = json!({"alias": &alias, "tail": true, "limit": 100});
        proof.attach(&mut params);
        let mut page = budget.read(state_dir, "thread_read", params).ok()?;
        if !super::threads::thread_scope_confirmed(&mut page, proof, false) {
            return None;
        }
        super::member_view::member_view(&mut page);
        pages.push(json!({"alias": &alias, "entries": page["entries"]}));
    }
    Some(value_fp(&json!(pages)))
}

/// One signed member's projection of the shared watcher.
///
/// Every frame is decided by comparing the reader's own representation with
/// the one it was last told about, so a shared frame caused only by another
/// reader's activity changes nothing and produces nothing.
struct MemberReader {
    pm: Pm,
    actor: String,
    /// Fingerprint of the reader's own project listing.
    projects_fp: u64,
    /// Fingerprint of the reader's own scoped delivery records.
    delivery_fp: u64,
    /// The daemon-verified member job list (bindings, task states, outcomes).
    jobs_fp: u64,
    /// The daemon-verified agent and running-message scope.
    scope: super::serve::MemberAgentScope,
    /// The reader's own monitoring projection, without its display clocks.
    monitoring_fp: u64,
    /// Fingerprint of the reader's own scoped thread pages.
    threads_fp: u64,
    /// id → the card/plan this reader was last told. Presence is also the
    /// tombstone entitlement: only a disclosed id may be deleted from view.
    card_fps: HashMap<String, u64>,
    plan_fps: HashMap<String, u64>,
    /// alias → fingerprint of the member row this reader was last told.
    agent_fps: HashMap<String, u64>,
}

impl MemberReader {
    /// Establish the reader's baseline. `None` means the projection could not
    /// be resolved — an unreadable tracker, an unknown caller spelling or a
    /// daemon that will not confirm the reader — and the stream must close.
    fn open(
        state_dir: &Path,
        pm_dir: &Path,
        actor: &str,
        proof: &super::operator::ReadRelayProof,
    ) -> Option<Self> {
        if !member_stream_available(pm_dir, actor) {
            return None;
        }
        // A relay with no reader in it is the board's own operator
        // connection. It cannot be projected for a member: every projection
        // below would answer with the operator's rows.
        if matches!(proof, super::operator::ReadRelayProof::Operator) {
            return None;
        }
        let pm = Pm::at(pm_dir).ok()?;
        let model = read_model::get(state_dir, pm_dir);
        let jobs = super::serve::readable_job_state(state_dir, &pm, actor, proof).ok()?;
        let scope = super::serve::member_agent_scope(state_dir, proof).ok()?;
        let read =
            super::serve::member_board_over(state_dir, &pm, actor, proof, None, &jobs, &scope)
                .ok()?;
        let readable = project_policy::readable(&pm, actor);
        // What the reader's own initial HTTP reads already told it: the
        // representation it holds, and the ids it may be told a tombstone for.
        let by_id = read.by_id();
        let ctx = read.ctx(&by_id);
        let mut card_fps = HashMap::with_capacity(read.views.len());
        let mut plan_fps = HashMap::new();
        for view in read.views.iter() {
            let id = view.issue.front.id.clone();
            card_fps.insert(id.clone(), card_fp(&read, &ctx, view).0);
            let plan = crate::issue::plan::plan_json(view, &by_id);
            if !plan.is_null() {
                plan_fps.insert(id, value_fp(&plan));
            }
        }
        // The rows this reader's own `/api/agents` already told it.
        let mut agents_payload = model.agents();
        super::serve::restrict_member_agent_rows(&mut agents_payload, &scope);
        super::serve::scope_member_agents(&mut agents_payload, &jobs.bindings, &scope);
        let agent_fps: HashMap<String, u64> = agents_payload["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| Some((row["alias"].as_str()?.to_owned(), agent_row_fp(row))))
            .collect();
        Some(Self {
            projects_fp: visible_projects_fp(&model, &pm, &readable),
            delivery_fp: visible_delivery_fp(state_dir, &readable)?,
            jobs_fp: jobs.fingerprint,
            monitoring_fp: monitoring_fingerprint(state_dir, &jobs.bindings, proof),
            threads_fp: visible_threads_fp(state_dir, proof, &scope)?,
            card_fps,
            plan_fps,
            agent_fps,
            scope,
            pm,
            actor: actor.to_owned(),
        })
    }

    /// The frames this reader may receive for one complete shared batch.
    /// `None` closes the stream: a projection or a frame shape that cannot be
    /// proven safe is refused, never guessed at.
    fn filter(
        &mut self,
        batch: &[Arc<str>],
        state_dir: &Path,
        proof: &super::operator::ReadRelayProof,
        entities: bool,
    ) -> Option<Vec<String>> {
        let model = read_model::get(state_dir, &self.pm.dir);
        let mut classified = Vec::with_capacity(batch.len());
        let mut want_board = false;
        let mut want_scope = false;
        let mut want_jobs = false;
        let mut want_monitoring = false;
        // The shared tracker stamp says only "the tracker changed". What it
        // may tell this reader is its own project listing and its own
        // delivery records.
        let mut tracker_signal = false;
        // The thread stamp says "some message or topic moved"; the reader's
        // own scoped thread pages decide whether any of it is theirs.
        let mut threads_signal = false;
        // Only a shared frame that says "a job or an agent moved" may make a
        // `jobs`/`agents` frame. A tracker frame fetches these projections to
        // render cards, but must not turn an older, unrelated difference into
        // an extra frame of its own.
        let mut jobs_signal = false;
        let mut scope_signal = false;
        let mut freshness = false;
        for text in batch {
            let frame = shared_frame(text)?;
            match &frame {
                SharedFrame::Heartbeat | SharedFrame::Aggregates => {}
                SharedFrame::Legacy(name) => match name.as_str() {
                    "issues" => tracker_signal = true,
                    "jobs" => {
                        want_jobs = true;
                        jobs_signal = true;
                    }
                    "agents" => {
                        want_jobs = true;
                        want_scope = true;
                        jobs_signal = true;
                        scope_signal = true;
                    }
                    "monitoring" => {
                        want_jobs = true;
                        want_monitoring = true;
                    }
                    "threads" => {
                        want_scope = true;
                        threads_signal = true;
                    }
                    // The operator's master state is not projected for a
                    // member (`/api/master/state` and `/api/threads/master`
                    // keep global queue counts and turn identifiers), so a
                    // `master` frame cannot be proven to be this reader's
                    // activity. Fail closed until that projection is scoped.
                    "master" => {}
                    _ => return None,
                },
                SharedFrame::Entity("issue" | "plan", _) => want_board = true,
                SharedFrame::Entity("agent", _) => {
                    want_jobs = true;
                    want_scope = true;
                    jobs_signal = true;
                    scope_signal = true;
                }
                SharedFrame::Entity(..) => return None,
                SharedFrame::AgentMeta(data) => {
                    // `by_issue`/`totals`/`profiles` are the shared board's
                    // and never ride a member socket. A frame that is
                    // freshness alone is the daemon's availability, which the
                    // member's own `/api/agents` carries too.
                    if data.get("by_issue").is_some() && data.get("totals").is_some() {
                        want_jobs = true;
                        want_scope = true;
                        jobs_signal = true;
                        scope_signal = true;
                    } else {
                        freshness = true;
                    }
                }
            }
            classified.push(frame);
        }

        // The reader's own tracker aggregate: its project listing (config and
        // counts of readable projects only) and its own delivery records.
        let mut projects_moved = false;
        let mut delivery_moved = false;
        if tracker_signal {
            let readable = project_policy::readable(&self.pm, &self.actor);
            let projects = visible_projects_fp(&model, &self.pm, &readable);
            projects_moved = projects != self.projects_fp;
            self.projects_fp = projects;
            let delivery = visible_delivery_fp(state_dir, &readable)?;
            delivery_moved = delivery != self.delivery_fp;
            self.delivery_fp = delivery;
        }
        // A project set that moved may have taken ids out of this reader's
        // board; the board below rebases what it may still be told about.
        if projects_moved {
            want_board = true;
        }

        let jobs = if want_jobs || want_board || want_monitoring {
            Some(super::serve::readable_job_state(state_dir, &self.pm, &self.actor, proof).ok()?)
        } else {
            None
        };
        let scope = if want_scope || want_board {
            Some(super::serve::member_agent_scope(state_dir, proof).ok()?)
        } else {
            None
        };
        let board = if want_board {
            Some(
                super::serve::member_board_over(
                    state_dir,
                    &self.pm,
                    &self.actor,
                    proof,
                    None,
                    jobs.as_ref()?,
                    scope.as_ref()?,
                )
                .ok()?,
            )
        } else {
            None
        };

        // One shared diff is only this reader's frame when the entity it
        // rebuilt differs from the one this reader was last told. A hidden
        // job that moves the shared card leaves the member card identical and
        // so produces nothing.
        let mut work: Vec<String> = Vec::new();
        let mut agents_out: Vec<String> = Vec::new();
        for frame in &classified {
            match frame {
                SharedFrame::Entity(kind, data) if matches!(*kind, "issue" | "plan") => {
                    let op = data["op"].as_str()?;
                    let id = data["id"].as_str()?;
                    match op {
                        "upsert" => {
                            let read = board.as_ref()?;
                            let by_id = read.by_id();
                            let Some(view) = by_id.get(id) else {
                                continue;
                            };
                            let ctx = read.ctx(&by_id);
                            let (fp, value) = if *kind == "issue" {
                                card_fp(read, &ctx, view)
                            } else {
                                let plan = crate::issue::plan::plan_json(view, &by_id);
                                (value_fp(&plan), plan)
                            };
                            if value.is_null() {
                                continue;
                            }
                            let known = if *kind == "issue" {
                                self.card_fps.get(id)
                            } else {
                                self.plan_fps.get(id)
                            };
                            if known == Some(&fp) {
                                continue;
                            }
                            if *kind == "issue" {
                                self.card_fps.insert(id.to_owned(), fp);
                            } else {
                                self.plan_fps.insert(id.to_owned(), fp);
                            }
                            let mut scoped = data.clone();
                            scoped[kind] = value;
                            work.push(entity_text(kind, &scoped, entities));
                        }
                        // Only an id this reader was already told about may
                        // be deleted from its view.
                        "delete" => {
                            let known = if *kind == "issue" {
                                self.card_fps.remove(id)
                            } else {
                                self.plan_fps.remove(id)
                            };
                            if *kind == "issue" {
                                self.plan_fps.remove(id);
                            }
                            if known.is_some() {
                                work.push(entity_text(
                                    kind,
                                    &json!({"op": "delete", "id": id}),
                                    entities,
                                ));
                            }
                        }
                        _ => return None,
                    }
                }
                SharedFrame::Entity(kind, data) if *kind == "agent" => {
                    let scope = scope.as_ref()?;
                    let jobs = jobs.as_ref()?;
                    let op = data["op"].as_str()?;
                    let alias = data["id"].as_str()?;
                    match op {
                        "upsert" => {
                            if !data["agent"].is_object() {
                                return None;
                            }
                            let Some(frame) =
                                scoped_agent_row(&data["agent"], &jobs.bindings, scope)
                            else {
                                // An alias outside the reader's scope is not
                                // its to name, now or in a later tombstone.
                                if !scope.has_alias(alias) {
                                    self.agent_fps.remove(alias);
                                }
                                continue;
                            };
                            let fp = agent_row_fp(&frame["agent"]);
                            if self.agent_fps.get(alias) == Some(&fp) {
                                continue;
                            }
                            self.agent_fps.insert(alias.to_owned(), fp);
                            agents_out.push(entity_text("agent", &frame, entities));
                        }
                        // Only an alias this reader was already told about may
                        // be removed from its view.
                        "delete" => {
                            if self.agent_fps.remove(alias).is_some() {
                                agents_out.push(entity_text(
                                    "agent",
                                    &json!({"op": "delete", "id": alias}),
                                    entities,
                                ));
                            }
                        }
                        _ => return None,
                    }
                }
                _ => {}
            }
        }

        // A scope change can remove or add visible cards independently of the
        // shared entity diff. Reconcile the reader's current board explicitly:
        // marking an unseen card as delivered can swallow a later visible
        // update if it landed while this batch was being processed.
        if projects_moved {
            let read = board.as_ref()?;
            let by_id = read.by_id();
            let ctx = read.ctx(&by_id);
            self.card_fps.retain(|id, _| by_id.contains_key(id));
            self.plan_fps.retain(|id, _| by_id.contains_key(id));
            for view in read.views.iter() {
                let id = &view.issue.front.id;
                let (fp, card) = card_fp(read, &ctx, view);
                if self.card_fps.get(id) != Some(&fp) {
                    self.card_fps.insert(id.clone(), fp);
                    work.push(entity_text(
                        "issue",
                        &json!({"op": "upsert", "id": id, "issue": card}),
                        entities,
                    ));
                }
                let plan = crate::issue::plan::plan_json(view, &by_id);
                if plan.is_null() {
                    if self.plan_fps.remove(id).is_some() {
                        work.push(entity_text(
                            "plan",
                            &json!({"op": "delete", "id": id}),
                            entities,
                        ));
                    }
                } else {
                    let fp = value_fp(&plan);
                    if self.plan_fps.get(id) != Some(&fp) {
                        self.plan_fps.insert(id.clone(), fp);
                        work.push(entity_text(
                            "plan",
                            &json!({"op": "upsert", "id": id, "plan": plan}),
                            entities,
                        ));
                    }
                }
            }
        }
        let issues_frame = projects_moved || delivery_moved || !work.is_empty();
        // A job frame means a job the daemon reports for this reader moved.
        let mut jobs_frame = false;
        if jobs_signal {
            if let Some(jobs) = jobs.as_ref() {
                if jobs.fingerprint != self.jobs_fp {
                    jobs_frame = true;
                }
                self.jobs_fp = jobs.fingerprint;
            }
        }
        // The thread invalidation is the reader's own scoped pages changing.
        let mut threads_frame = false;
        if threads_signal {
            let fp = visible_threads_fp(state_dir, proof, scope.as_ref()?)?;
            threads_frame = self.threads_fp != fp;
            self.threads_fp = fp;
        }
        // An agent frame means a row the reader may see moved, a binding it
        // may see moved, its own scope did, or the daemon became unreachable.
        let mut agents_frame = freshness || !agents_out.is_empty() || jobs_frame;
        if scope_signal {
            if let Some(next) = scope {
                if next != self.scope {
                    agents_frame = true;
                    // The client's refetch drops rows it may no longer see, so
                    // an alias that left the scope must not keep a tombstone
                    // entitlement either.
                    self.agent_fps.retain(|alias, _| next.has_alias(alias));
                    self.scope = next;
                }
            }
        }
        let mut monitoring_frame = false;
        if want_monitoring {
            let jobs = jobs.as_ref()?;
            let fp = monitoring_fingerprint(state_dir, &jobs.bindings, proof);
            if fp != self.monitoring_fp {
                monitoring_frame = true;
                self.monitoring_fp = fp;
            }
        }

        let mut out = Vec::with_capacity(5 + agents_out.len() + work.len());
        if issues_frame {
            out.push(legacy_frame("issues"));
        }
        if jobs_frame {
            out.push(legacy_frame("jobs"));
        }
        if agents_frame {
            out.push(legacy_frame("agents"));
        }
        if monitoring_frame {
            out.push(legacy_frame("monitoring"));
        }
        if threads_frame {
            out.push(legacy_frame("threads"));
        }
        out.extend(agents_out);
        out.extend(work);
        Some(out)
    }
}

pub(super) fn stream_events(
    request: Request,
    state_dir: &Path,
    pm_dir: &Path,
    mut admission: super::operator::ReadAdmission,
    operator: bool,
    actor: &str,
    #[cfg(test)] before_member_batch: Option<&super::MemberStreamBatchHook>,
) {
    let entities = request
        .url()
        .split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|part| part == "entities=1"));
    if !admission.still_live(state_dir) {
        return;
    }
    // Join the board's shared watcher before the reader's baseline is taken:
    // the first subscriber's baseline is taken inside `subscribe`, so a change
    // that lands while the baseline below is read is either already in that
    // baseline (and in the client's post-head reads, and in the client's own
    // resync on open) or arrives as a frame. Taking the baseline first would
    // let such a change vanish between it and the subscription.
    let rx = read_model::get(state_dir, pm_dir).subscribe();
    // Resolve the signed member's baseline before sending SSE headers.
    // An unavailable projection is a truthful HTTP error, not an apparently
    // live stream that can only stay empty.
    let mut reader = if operator {
        None
    } else {
        match MemberReader::open(state_dir, pm_dir, actor, &admission.relay_proof()) {
            Some(reader) => Some(reader),
            None => {
                let _ = request.respond(super::serve::err_response(
                    503,
                    "member live updates are unavailable",
                ));
                return;
            }
        }
    };
    let mut w = request.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Cache-Control: no-store\r\nConnection: close\r\n\r\n";
    if w.write_all(head.as_bytes())
        .and_then(|_| w.flush())
        .is_err()
    {
        return;
    }
    let frame = |w: &mut dyn Write, bytes: &[u8]| -> bool {
        w.write_all(bytes).and_then(|_| w.flush()).is_ok()
    };
    // First frame immediately — `hello` names the serving build (a tab
    // running an older bundle compares and prompts a reload, CAD-573)
    // and the `: ping` right behind it proves the stream is live and
    // gives proxies something to flush before the first event exists.
    let hello = stream_hello(entities);
    if !frame(&mut w, hello.as_bytes()) || !frame(&mut w, b": ping\n\n") {
        return;
    }
    // `: ping` every 15 s of wire silence — the watcher's per-second
    // heartbeat would otherwise starve the keepalive, and an idle dead
    // client would never surface without a write.
    let mut ping_at = Instant::now() + Duration::from_secs(15);
    loop {
        let next = rx.recv_timeout(admission.wait_timeout());
        if !admission.still_live(state_dir) {
            return;
        }
        match next {
            Ok(f) if &*f == read_model::HEARTBEAT || &*f == read_model::BATCH_END => {}
            Ok(first) => {
                if !admission.still_live_before_frame(state_dir) {
                    return;
                }
                if let Some(reader) = reader.as_mut() {
                    // A shared batch is decided as a whole — an `issues`
                    // invalidation is only owed once its entity diffs are
                    // known — so wait for the watcher's end-of-batch marker
                    // rather than assuming nothing more is queued.
                    let mut batch = Vec::new();
                    let mut complete = false;
                    let mut next = Some(first);
                    while let Some(f) = next {
                        if &*f == read_model::BATCH_END {
                            complete = true;
                            break;
                        }
                        batch.push(f);
                        // The marker is part of the same lock-held send, so it
                        // is already on its way; the deadline only guards a
                        // watcher that died mid-batch. It must not ride
                        // `wait_timeout`, which may be zero right after a
                        // revalidation.
                        next = rx.recv_timeout(Duration::from_secs(5)).ok();
                    }
                    // An incomplete batch cannot be proven, so it is not
                    // forwarded: a frame decided on half a tick's information
                    // could leak on one side or hide an update on the other.
                    if !complete || !admission.still_live_before_frame(state_dir) {
                        return;
                    }
                    #[cfg(test)]
                    if let Some(hook) = before_member_batch {
                        hook(actor, &batch);
                    }
                    let Some(frames) =
                        reader.filter(&batch, state_dir, &admission.relay_proof(), entities)
                    else {
                        return;
                    };
                    for text in frames {
                        if !frame(&mut w, text.as_bytes()) {
                            return;
                        }
                    }
                } else {
                    // The operator's stream is the watcher's own, one frame at
                    // a time, exactly as written.
                    if (!entities || !operator) && first.starts_with("event: aggregates\n") {
                        continue;
                    }
                    let optimized = (entities
                        && (operator
                            || first.starts_with("event: issue\n")
                            || first.starts_with("event: agent\n")
                            || first.starts_with("event: plan\n")))
                    .then(|| read_model::entity_frame(first.as_ref()));
                    let bytes = optimized.as_deref().unwrap_or_else(|| first.as_ref());
                    if !frame(&mut w, bytes.as_bytes()) {
                        return;
                    }
                }
                ping_at = Instant::now() + Duration::from_secs(15);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() >= ping_at {
            let ping: &[u8] = if entities {
                b"event: heartbeat\ndata: {}\n\n"
            } else {
                b": ping\n\n"
            };
            if !frame(&mut w, ping) {
                return;
            }
            ping_at = Instant::now() + Duration::from_secs(15);
        }
    }
}
