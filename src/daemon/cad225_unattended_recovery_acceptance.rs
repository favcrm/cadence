//! Independent CAD-225 recovery acceptance — child of
//! `cad225_unattended_acceptance`, written from the ticket's recovery
//! outcomes, not the implementer's tests.
//!
//! The sibling file proves what the grant refuses. This file proves the
//! loop survives its own failures:
//!
//! | case | real machinery crossed |
//! |---|---|
//! | admission wake lost (no master), daemon restarted | `route_wakes` reconstruction from `decided_by`/`unattended_authorization_for_issue` receipts — one `plan_approved` wake, deterministic `sys-` id |
//! | duplicate wake production | `wake_on_plan_approved` + `route_wakes` dedupe on `proto::daemon_message_id`, before and after a `Shared` reopen |
//! | coordinator restart with work in flight | `Store::open_adopting` without a marker — queued rows stay queued, in-flight rows become `unknown` evidence, never replayed |
//! | in-flight PTY report-owing turn at restart | the swept `unknown` row keeps id/lane/turn correlation for an operator reconcile |
//! | provider death mid-turn (`DISCONNECT` on the `fake` endpoint) | the real `OutcomeUnknown` producer: actor `finish(unknown)` + `set_state_detached(attention)`, fenced relaunch blocked, then `unattended_worker_complete` -> `unattended_completion_unresolved` -> `Escalated` + `unattended_hold` — ONE operator row across ticket+lane, no review kickoff, no replay |
//! | provider quota denial mid-turn (`CAD225_QUOTA_ERROR`, socketed) | `Error::Provider` through the actor's fatal arm: `failed` evidence + `attention` fence, ONE coalesced operator row for lane+ticket, no retry — recovery is `try_resume` (operator unfence) then a new adapter-run owned turn proving the head, not reconcile |
//! | review's recipient missing | `pick_reviewer` finds nobody -> `Unstaffed` retried per pass with one `review_unstaffed` row/event, staffed again -> exactly one kickoff |
//! | changed input to a present grant | `propose_workflow` errs with zero tracker mutation — not an approval gap |
//! | approval requirement (ungranted manual plan) | the `plan` needs-me row stands while the plan is `proposed`; `plan_approve` (operator-only) is the resolution, acks and agents are not |
//! | acknowledgment | `needs_dismiss` hides a row but clears no cause; `agent_inbox_ack` consumes queued mail, never `unknown` evidence |
//!
//! Same fixture limits as the parent: no daemon socket is bound and
//! `Shared::new` without a shutdown marker is the crash-equivalent cold
//! open — `recover()` fences every in-flight row it sweeps, which is
//! the restart contract under test. `master_dispatch` is not called;
//! dispatch turns are seeded as the store rows a real kickoff leaves,
//! except the provider-failure case, which launches a real actor on the
//! `fake` endpoint kind (in-process, no live provider). Needs-me rows
//! are the tracker/delivery.json-driven subset — agent-row kinds
//! (`fenced`, `stopped`, `approval`) need the `agent_list` probes a
//! socketless fixture cannot serve — the held case therefore registers
//! a live master lane (its admission wake lands) as the reachable form
//! of "team ownership is available", and `failure_rows` counts every
//! operator row on a failure's subjects rather than assuming which
//! kinds may appear. Escalation quality is asserted by state, actor and
//! a driven resolution verb, never by row wording.

use super::*;

/// `master_woken` daemon events — one per wake the daemon queued.
fn master_woken(world: &World) -> Vec<Value> {
    world
        .shared
        .store
        .events_tail(crate::store::Store::DAEMON_STREAM, 200)
        .unwrap_or_default()
        .into_iter()
        .filter(|event| event.kind == "master_woken")
        .map(|event| event.payload)
        .collect()
}

/// The master's queued wake rows (source `wake` only).
fn wakes(world: &World) -> Vec<crate::store::Message> {
    world
        .shared
        .store
        .messages(crate::master::ALIAS)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.source == crate::master::WAKE_SOURCE)
        .collect()
}

/// `review_unstaffed` daemon events naming `ticket` — one per cause.
fn unstaffed_events(world: &World, ticket: &str) -> usize {
    world
        .shared
        .store
        .events_tail(crate::store::Store::DAEMON_STREAM, 100)
        .unwrap_or_default()
        .iter()
        .filter(|event| {
            event.kind == "review_unstaffed" && event.payload["issue"].as_str() == Some(ticket)
        })
        .count()
}

/// Needs-me rows of `kind` about `id`, through the same operator-owned
/// read the board serves. `id` is the subject's issue (a ticket or its
/// epic for the `plan` row).
fn needs_rows_in(view: &Value, kind: &str, id: &str) -> Vec<Value> {
    view["needs_me"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            row["kind"].as_str() == Some(kind) && row["subject"]["id"].as_str() == Some(id)
        })
        .cloned()
        .collect()
}

/// The socketless overview read — the same shape the board serves.
fn needs_rows(world: &World, kind: &str, id: &str) -> Vec<Value> {
    needs_rows_in(
        &crate::overview::overview_cached(world.root.path(), &world.pm()),
        kind,
        id,
    )
}

/// ONE actionable unresolved escalation: a single operator-audience row
/// on the subject. The supported action is asserted by driving the real
/// verb afterward, never by matching row wording.
fn assert_one_escalation(world: &World, kind: &str, id: &str) -> Value {
    let rows = needs_rows(world, kind, id);
    assert_eq!(
        rows.len(),
        1,
        "exactly one {kind} needs-me row for {id}: {rows:?}"
    );
    let row = &rows[0];
    assert_eq!(
        row["audience"].as_str(),
        Some("operator"),
        "the {kind} row must name the operator as its owner: {row}"
    );
    row.clone()
}

/// Every operator-audience row attributable to one failure — any row
/// whose subject is one of `subjects` (the ticket, the epic, the fenced
/// alias). The contract is ONE unresolved escalation per cause: a
/// second row stacking on the same failure is a duplicate-cause bug.
fn failure_rows_in(view: &Value, subjects: &[&str]) -> Vec<Value> {
    view["needs_me"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            row["audience"].as_str() == Some("operator")
                && row["subject"]["id"]
                    .as_str()
                    .is_some_and(|id| subjects.contains(&id))
        })
        .cloned()
        .collect()
}

fn failure_rows(world: &World, subjects: &[&str]) -> Vec<Value> {
    failure_rows_in(
        &crate::overview::overview_cached(world.root.path(), &world.pm()),
        subjects,
    )
}

/// The operator's acknowledgment of a needs-me subject — the board's
/// dismiss. It hides the row; it resolves nothing. `kind`/`id` are the
/// row's subject pair (`issue`, `agent`, …).
fn dismiss(world: &World, kind: &str, id: &str) {
    crate::needs_dismiss::record(
        world.root.path(),
        json!({"kind": kind, "id": id, "mode": "dismiss", "at": now_secs() as i64}),
        now_secs() as i64,
    )
    .expect("the dismiss write answers");
}

/// A caller — agent or unproven — must not settle an outcome.
fn reconcile_as(world: &World, who: Asserted, params: Value) -> Result<Value> {
    let shared = world.shared.clone();
    scoped(who, || shared.rpc_reconcile(&params, std::process::id()))
}

/// Register a pty-endpoint agent: a lane whose turns are pasted, so a
/// `running` row owes the daemon a report its provider may have consumed.
fn register_pty(world: &World, alias: &str, role: &str) {
    world
        .shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "claude",
            endpoint_kind: "pty",
            role,
            cwd: world.root.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap_or_else(|e| panic!("register {alias}: {e}"));
}

/// Register a `fake`-endpoint agent: the in-process provider double,
/// so a launched actor runs real adapter turns with no live provider.
fn register_fake(world: &World, alias: &str, role: &str, cwd: &str) {
    world
        .shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role,
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap_or_else(|e| panic!("register {alias}: {e}"));
}

/// Wait for a launched actor to fully exit: it removes its lifecycle
/// entry only after every state write (the message result, the fence,
/// the `attention` event) is durable — no sleeps in the assertions.
fn actor_exit(world: &World, alias: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let owned = world
            .shared
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .contains_key(alias);
        if !owned {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the fake {alias} actor never exited"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until the message reaches `state` — the resumed actor's own
/// adapter completes its turn asynchronously; the deadline bounds it.
fn wait_message_state(world: &World, id: &str, state: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let current = world
            .shared
            .store
            .message(id)
            .unwrap()
            .map(|m| m.state)
            .unwrap_or_default();
        if current == state {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "message {id} is {current}, never reached {state}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The reviewer the fixture registers on the `fake` endpoint is an
/// actor-kind lane, so `register_agent` leaves it `starting` until its
/// actor would report ready. `pick_reviewer` routes only to `idle`
/// reviewers, so a registered-but-never-ready reviewer cannot be
/// staffed. `set_agent_state` is the live-state write the actor's own
/// ready transition uses.
fn reviewer_ready(world: &World, alias: &str) {
    world
        .shared
        .store
        .set_agent_state(alias, "idle", None)
        .expect("the reviewer reports ready");
}

/// `World::dispatch_turn` for a lane it cannot name: the same kickoff
/// row `dispatch_send` leaves, on `alias` instead of `worker`.
fn dispatch_turn_on(
    world: &World,
    alias: &str,
    ticket: Option<&str>,
    seq: usize,
    running: bool,
    result: Option<Option<&str>>,
    body: &str,
) -> String {
    let id = format!("cad225-dispatch-{alias}-{seq}");
    let (duplicate, _state) = world
        .shared
        .store
        .enqueue_steered(
            alias,
            body,
            None,
            &id,
            "dispatch",
            None,
            ticket,
            Some("/tmp/cad225-wt"),
            &Sender::Unattributed,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
        )
        .expect("the kickoff row is a store write, not a caller path");
    assert!(!duplicate, "kickoff seed {id} must not collide");
    if running || result.is_some() {
        world
            .shared
            .store
            .mark_running(&id, &format!("turn-{alias}-{seq}"))
            .expect("provider ack");
    }
    if let Some(inner) = result {
        let message = world
            .shared
            .store
            .message(&id)
            .expect("turn reads")
            .expect("the seeded kickoff exists");
        world
            .shared
            .store
            .finish(
                &message,
                "completed",
                &match inner {
                    Some(sha) => json!({"sha": sha}),
                    None => json!({"text": "done without a recorded head"}),
                },
                None,
            )
            .expect("turn completes");
    }
    id
}

/// Reboot the coordinator on the same state — `Shared::new` on a dir
/// with no shutdown marker is the crash-equivalent cold open.
fn restart(mut world: World) -> World {
    let root = world.root.path().to_path_buf();
    drop(world.shared);
    world.shared = World::reopen(root);
    world
}

/// Delivery-record read used across the recovery cases.
fn record(world: &World, ticket: &str) -> Record {
    delivery::load(world.root.path())
        .unwrap()
        .get(ticket)
        .cloned()
        .unwrap_or_else(|| panic!("{ticket} keeps a delivery record"))
}

/// Seed the record a bound run's dispatch opens.
fn record_working(world: &World, ticket: &str) {
    let mut all = delivery::load(world.root.path()).unwrap();
    let mut rec = Record::new(ticket, PROJECT, "worker", 1);
    rec.state = State::Working;
    all.insert(ticket.to_string(), rec);
    delivery::save(world.root.path(), &all).unwrap();
}

/// Seed the record a bound run's dispatch to `worker_alias` opens.
fn record_working_for(world: &World, ticket: &str, worker_alias: &str) {
    let mut all = delivery::load(world.root.path()).unwrap();
    let mut rec = Record::new(ticket, PROJECT, worker_alias, 1);
    rec.state = State::Working;
    all.insert(ticket.to_string(), rec);
    delivery::save(world.root.path(), &all).unwrap();
}

/// `World::done_report` for a lane it cannot name: the identical report
/// `task_report::list` sees, filed as `agent` on the head `sha`.
fn file_done_report(world: &World, ticket: &str, agent: &str, at: i64, sha: &str) {
    let iso = crate::issue::time::iso(at);
    let compact: String = iso.chars().filter(|c| *c != '-' && *c != ':').collect();
    let name = format!("{compact}-{agent}-{at}.md");
    let dir = world.pm().join(PROJECT).join(ticket).join("reports");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(&name),
        format!(
            "---\nschema: cadence.report/2\nkind: done\ntask: {ticket}\nagent: {agent}\n\
             sha: {sha}\npr: https://github.com/{REPO}/pull/{PR}\n---\n\n\
             ## Expected\n\nx\n\n## Evidence\n\nx\n\n## Cause\n\nx\n\n\
             ## Correction\n\nx\n\n## Lesson\n\nx\n\n## Next\n\nx\n"
        ),
    )
    .unwrap();
}

#[test]
fn cad225_lost_admission_wake_is_reconstructed_once_across_restart() {
    let mut world = World::open();
    // Admission with no master registered: the accept-at-create wake has
    // no mailbox to land in — the exact loss `route_wakes` exists to heal.
    let admitted = world.grant_and_admit(None);
    assert!(
        master_woken(&world).is_empty(),
        "no master means no wake was queued at admission"
    );
    world.register(crate::master::ALIAS, "worker");
    assert!(
        wakes(&world).is_empty(),
        "a late master starts with an empty mailbox"
    );
    // The coordinator restarts before any router pass: reconstruction is
    // from durable receipts, not in-memory state.
    world = restart(world);
    assert_eq!(
        world.issue(&admitted.ticket).front.status,
        "ready",
        "the restart must not lose the admitted ticket"
    );
    let run = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .unwrap()
        .expect("the bound run survives the restart");
    assert_eq!(run.grant_id, admitted.grant_id);
    assert_eq!(run.epic, admitted.epic);

    let woken = world.shared.route_wakes().expect("the router pass answers");
    assert_eq!(
        woken, 1,
        "the lost admission wake is reconstructed exactly once"
    );
    let woken_events = master_woken(&world);
    assert_eq!(woken_events.len(), 1);
    assert_eq!(woken_events[0]["event"].as_str(), Some("plan_approved"));
    assert_eq!(
        woken_events[0]["issue"].as_str(),
        Some(admitted.epic.as_str())
    );
    let queued_wakes = wakes(&world);
    assert_eq!(queued_wakes.len(), 1, "one wake row, {queued_wakes:?}");
    let wake = &queued_wakes[0];
    assert_eq!(wake.alias, crate::master::ALIAS);
    assert_eq!(wake.state, "queued");
    // The deterministic id binds epic + grant + expiry — the key every
    // replay must dedupe against.
    let key = format!("{}/{}/{}", admitted.epic, admitted.grant_id, run.expires_at);
    assert_eq!(
        wake.id,
        crate::master::wake_id("plan_approved", &key),
        "the reconstruction names the same dedupe key the admission path uses"
    );
    assert!(wake.body.contains(&admitted.grant_id), "{}", wake.body);
    assert!(wake.body.contains(&admitted.epic), "{}", wake.body);

    // A duplicate event replay and a duplicate router pass admit nothing
    // twice — before and after a second coordinator restart.
    let issues = world.issues();
    let commits = world.commits();
    world.shared.wake_on_plan_approved(&json!({
        "epic": admitted.epic,
        "project": PROJECT,
        "grant_id": admitted.grant_id,
        "grant_expires_at": run.expires_at,
    }));
    assert_eq!(
        world.shared.route_wakes().expect("replayed pass answers"),
        0,
        "a replayed pass queued a second wake"
    );
    world = restart(world);
    assert_eq!(
        world
            .shared
            .route_wakes()
            .expect("post-restart pass answers"),
        0,
        "a restart forgot the wake was already queued"
    );
    assert_eq!(wakes(&world).len(), 1, "one wake, never a duplicate");
    assert_eq!(master_woken(&world).len(), 1);
    assert_eq!(world.issues(), issues, "no second admitted plan");
    assert_eq!(world.commits(), commits, "no second admission commit");
}

#[test]
fn cad225_restart_preserves_reporting_correlation_and_never_replays() {
    let mut world = World::open();
    register_pty(&world, "pty-worker", "worker");
    let admitted = world.grant_and_admit(None);
    let ticket = &admitted.ticket;

    // A queued kickoff: mail the restart must not lose or duplicate.
    let queued_id = dispatch_turn_on(
        &world,
        "pty-worker",
        Some(ticket.as_str()),
        1,
        false,
        None,
        "kickoff",
    );
    // An in-flight report-owing PTY turn: pasted and acknowledged, its
    // report never filed — the ownership-proven correlation to preserve.
    let running_id = dispatch_turn_on(
        &world,
        "pty-worker",
        Some(ticket.as_str()),
        2,
        true,
        None,
        "kickoff",
    );
    let msg = world.shared.store.message(&running_id).unwrap().unwrap();
    world
        .shared
        .store
        .mark_submitted(&msg)
        .expect("the paste ack stands the turn at submitted");
    assert!(
        world
            .shared
            .store
            .message(&running_id)
            .unwrap()
            .unwrap()
            .awaiting_report(),
        "the turn is delivered and owes its report"
    );

    // Cold restart: no shutdown marker, so recovery cannot prove the
    // paste's fate — it must fence, never adopt and never replay.
    world = restart(world);
    let queued = world.shared.store.message(&queued_id).unwrap().unwrap();
    assert_eq!(queued.state, "queued", "queued mail survives a restart");
    assert_eq!(queued.issue.as_deref(), Some(ticket.as_str()));
    let in_flight = world.shared.store.message(&running_id).unwrap().unwrap();
    assert_eq!(
        in_flight.state, "unknown",
        "an unproven in-flight turn is held, not adopted: {in_flight:?}"
    );
    // The ownership-proven correlation is retained as evidence: same row,
    // same lane tags, same turn token — nothing deleted, nothing replayed.
    assert_eq!(in_flight.issue.as_deref(), Some(ticket.as_str()));
    assert_eq!(in_flight.worktree.as_deref(), Some("/tmp/cad225-wt"));
    assert_eq!(
        in_flight.turn_id.as_deref(),
        Some("turn-pty-worker-2"),
        "the submission token is the correlation an operator reconciles"
    );
    assert!(
        world.shared.store.has_unknown("pty-worker").unwrap(),
        "the unresolved outcome fences the lane"
    );
    assert_eq!(
        world.shared.store.agent("pty-worker").unwrap().state,
        "offline",
        "a fenced pty lane is not relaunched on its own"
    );
    // No replay: the lane holds exactly its two rows, both durable.
    let rows = world.shared.store.messages("pty-worker").unwrap();
    assert_eq!(
        rows.iter()
            .filter(|m| m.issue.as_deref() == Some(ticket.as_str()))
            .count(),
        2,
        "restart created or dropped kickoff rows: {rows:?}"
    );
    // A second cold restart neither re-fences nor erases the evidence:
    // the same row is still `unknown` with its correlation intact.
    world = restart(world);
    let still = world.shared.store.message(&running_id).unwrap().unwrap();
    assert_eq!(still.state, "unknown");
    assert_eq!(still.turn_id.as_deref(), Some("turn-pty-worker-2"));
    assert_eq!(still.issue.as_deref(), Some(ticket.as_str()));
}

#[test]
fn cad225_missing_review_recipient_escalates_once_and_staffing_recovers() {
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    let ticket = admitted.ticket;
    // A send to an unregistered alias refuses before any write — the
    // missing-recipient guard is synchronous, not a queued retry.
    let before = world.snap();
    world.assert_refused(
        world
            .shared
            .clone()
            .rpc_send(&json!({"alias": "ghost-worker", "text": "go"})),
        &before,
    );
    // The review's recipient is missing: nobody staffed with the reviewer
    // role for this lane. The fixture's reviewer is an actor-kind lane
    // admitted `starting`; `remove_agent` only serves an operator who
    // stopped the lane first — `set_state_detached` is the same
    // terminal write `agent stop` lands.
    world
        .shared
        .store
        .set_state_detached("reviewer", "stopped", None)
        .expect("the reviewer lane stops");
    world
        .shared
        .store
        .remove_agent(
            "reviewer",
            true,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .expect("remove the only reviewer");
    record_working(&world, &ticket);
    world.dispatch_turn(&ticket, 1, false, Some(Some(HEAD)));
    world.done_report(&ticket, now_secs() as i64, HEAD);
    world
        .shared
        .route_delivery()
        .expect("a router pass with no reviewer answers");
    let rec = record(&world, &ticket);
    assert_eq!(
        rec.state,
        State::Unstaffed,
        "a missing recipient holds the review, it is not lost"
    );
    let row = assert_one_escalation(&world, "review_unstaffed", &ticket);
    assert_eq!(
        failure_rows(&world, &[ticket.as_str(), "worker"]).len(),
        1,
        "the missing recipient stacks exactly one operator row: {row}"
    );
    // Bounded retry: every pass re-judges — same one row, same one event,
    // and still no kickoff to anyone.
    world.shared.route_delivery().unwrap();
    world.shared.route_delivery().unwrap();
    assert_eq!(record(&world, &ticket).state, State::Unstaffed);
    assert_eq!(needs_rows(&world, "review_unstaffed", &ticket).len(), 1);
    assert_eq!(unstaffed_events(&world, &ticket), 1);
    assert!(world.routed(&ticket).is_empty());
    // Acknowledgment is not resolution: dismissing the row changes the
    // view, never the cause.
    dismiss(&world, "issue", &ticket);
    assert!(needs_rows(&world, "review_unstaffed", &ticket).is_empty());
    assert_eq!(
        record(&world, &ticket).state,
        State::Unstaffed,
        "a dismissed row is not a staffed review"
    );
    world.shared.route_delivery().unwrap();
    assert_eq!(record(&world, &ticket).state, State::Unstaffed);
    // Recovery is the missing recipient arriving — then exactly one
    // kickoff, to it, and no row left to show.
    world.register("reviewer", "reviewer");
    reviewer_ready(&world, "reviewer");
    world
        .shared
        .route_delivery()
        .expect("the staffed pass answers");
    assert_eq!(record(&world, &ticket).state, State::Reviewing);
    let routed = world.routed(&ticket);
    assert_eq!(routed.len(), 1, "exactly one review kickoff: {routed:?}");
    assert_eq!(routed[0]["reviewer"].as_str(), Some("reviewer"));
    assert_eq!(routed[0]["sha"].as_str(), Some(HEAD));
    let kickoffs = world.shared.store.messages("reviewer").unwrap();
    assert_eq!(
        kickoffs.len(),
        1,
        "the recovery routes one turn, not a replay backlog"
    );
    assert_eq!(kickoffs[0].source.as_str(), "review");
    assert!(needs_rows(&world, "review_unstaffed", &ticket).is_empty());
}

#[test]
fn cad225_provider_failure_fences_lane_and_holds_until_operator_reconcile() {
    let world = World::open();
    // A healthy master stands registered before admission — team
    // ownership is available, so the held row's operator audience must
    // be the row's own class, never an owner-cannot-act fallback. Its
    // admission wake lands: the coordinator is live and usable.
    world.register(crate::master::ALIAS, "worker");
    let admitted = world.grant_and_admit(None);
    // The seeded reviewer must be routable when the hold resolves.
    reviewer_ready(&world, "reviewer");
    assert!(
        !wakes(&world).is_empty(),
        "the registered master is a live lane — its admission wake landed"
    );
    assert_eq!(
        world
            .shared
            .store
            .agent(crate::master::ALIAS)
            .unwrap()
            .state
            .as_str(),
        "idle",
        "the master is healthy, not fenced or stopped"
    );
    let ticket = admitted.ticket;
    record_working_for(&world, &ticket, "fworker");
    file_done_report(&world, &ticket, "fworker", now_secs() as i64, HEAD);
    register_fake(
        &world,
        "fworker",
        "worker",
        world.root.path().to_str().unwrap(),
    );
    // The kickoff the bound dispatch left: the fake endpoint's
    // `DISCONNECT` directive severs the transport mid-turn — the real
    // `OutcomeUnknown` producer, not a seeded aftermath row.
    let turn_id = dispatch_turn_on(
        &world,
        "fworker",
        Some(ticket.as_str()),
        1,
        false,
        None,
        "DISCONNECT",
    );
    world
        .shared
        .launch_actor("fworker")
        .expect("the fake lane launches");
    actor_exit(&world, "fworker");
    // The provider failure produced the fence, not an assertion seed:
    // the turn is `unknown` under its real issued token, the lane is
    // `attention`, and the lane cannot relaunch itself while fenced.
    let msg = world.shared.store.message(&turn_id).unwrap().unwrap();
    assert_eq!(msg.state, "unknown", "the failed turn is held: {msg:?}");
    assert!(
        msg.turn_id
            .as_deref()
            .is_some_and(|t| t.starts_with("fake-turn-")),
        "the adapter's issued token is the correlation: {msg:?}"
    );
    assert!(world.shared.store.has_unknown("fworker").unwrap());
    assert_eq!(
        world.shared.store.agent("fworker").unwrap().state,
        "attention",
        "a provider death fences the lane, never silently retried"
    );
    world
        .shared
        .launch_actor("fworker")
        .expect("a fenced launch answers");
    assert!(
        !world
            .shared
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .contains_key("fworker"),
        "the fence blocks self-relaunch until reconcile"
    );
    // The operator-facing contract: the done report plus the unknown
    // owned turn yield ONE actionable unresolved escalation for the
    // whole failure — no second row stacks on ticket or lane.
    world
        .shared
        .route_delivery()
        .expect("a router pass on an unknown turn answers");
    let rec = record(&world, &ticket);
    assert_eq!(
        rec.state,
        State::Escalated,
        "an unknown owned outcome is held, never believed"
    );
    assert!(
        rec.unattended_hold,
        "the hold flags the record for the operator"
    );
    assert_eq!(
        rec.unattended_turn.as_deref(),
        Some(turn_id.as_str()),
        "the hold names the owned unknown turn it is gated on"
    );
    let row = assert_one_escalation(&world, "unattended_held", &ticket);
    assert_eq!(
        failure_rows(&world, &[ticket.as_str(), "fworker"]).len(),
        1,
        "one escalation serves the whole failure — ticket and lane share it: {row}"
    );
    assert!(
        world.routed(&ticket).is_empty(),
        "an unresolved outcome must not route a review"
    );
    // Bounded: a repeat pass re-judges to the same hold — no replay of
    // the unknown turn, no second escalation.
    world.shared.route_delivery().unwrap();
    let again = record(&world, &ticket);
    assert_eq!(again.state, State::Escalated);
    assert_eq!(again.note, rec.note, "the held reason is stable");
    assert!(world.routed(&ticket).is_empty());
    // Acknowledgment is not resolution: a dismiss hides the row, the
    // unknown row and the fence still stand.
    dismiss(&world, "issue", &ticket);
    assert!(needs_rows(&world, "unattended_held", &ticket).is_empty());
    assert_eq!(
        world
            .shared
            .store
            .unknown_messages("fworker")
            .unwrap()
            .len(),
        1,
        "the unproven outcome survives an acknowledgment"
    );
    assert_eq!(record(&world, &ticket).state, State::Escalated);
    assert_eq!(
        world.shared.store.agent("fworker").unwrap().state,
        "attention",
        "acknowledgment never lifts the fence"
    );
    // Reconcile is the operator's verb — the fenced worker itself and an
    // unproven caller are refused.
    assert!(
        reconcile_as(
            &world,
            Asserted::Agent("fworker".into()),
            json!({"message": &turn_id, "status": "completed", "sha": HEAD}),
        )
        .is_err(),
        "the fenced worker cannot settle its own unknown outcome"
    );
    assert!(
        reconcile_as(
            &world,
            Asserted::Unproven,
            json!({"message": &turn_id, "status": "completed", "sha": HEAD}),
        )
        .is_err(),
        "an unattributable caller cannot settle an unknown outcome"
    );
    assert_eq!(
        world.shared.store.message(&turn_id).unwrap().unwrap().state,
        "unknown"
    );
    // Resolution is the operator naming the real outcome — the head the
    // turn actually produced — which completes the owned correlation and
    // lifts the fence.
    reconcile_as(
        &world,
        Asserted::Operator,
        json!({"message": &turn_id, "status": "completed", "sha": HEAD}),
    )
    .expect("operator reconcile answers");
    let settled = world.shared.store.message(&turn_id).unwrap().unwrap();
    assert_eq!(settled.state, "completed");
    assert_eq!(settled.result.as_ref().unwrap()["sha"].as_str(), Some(HEAD));
    assert!(world
        .shared
        .store
        .unknown_messages("fworker")
        .unwrap()
        .is_empty());
    assert_ne!(
        world.shared.store.agent("fworker").unwrap().state,
        "attention",
        "the last unknown reconciling lifts the fence"
    );
    // The next pass uses the now-proven correlation: one review kickoff,
    // no replay of the turn, no held row left.
    world
        .shared
        .route_delivery()
        .expect("the post-reconcile pass answers");
    assert_eq!(record(&world, &ticket).state, State::Reviewing);
    let routed = world.routed(&ticket);
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0]["reviewer"].as_str(), Some("reviewer"));
    assert_eq!(routed[0]["sha"].as_str(), Some(HEAD));
    assert!(needs_rows(&world, "unattended_held", &ticket).is_empty());
    let dispatch_rows: Vec<_> = world
        .shared
        .store
        .messages("fworker")
        .unwrap()
        .into_iter()
        .filter(|m| m.source == "dispatch" && m.issue.as_deref() == Some(ticket.as_str()))
        .collect();
    assert_eq!(
        dispatch_rows.len(),
        1,
        "the recovery reconciled the owned turn; it never replayed it"
    );
}

#[test]
fn cad225_approval_requirement_escalates_until_the_operator_decides() {
    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), None, None),
        )
        .unwrap();
    // A changed input to the present bounded grant is not an approval
    // gap: it is refused outright with zero tracker mutation (the
    // sibling file's contract — re-asserted so this case cannot inherit
    // a narrowed meaning).
    let before = world.snap();
    world.assert_refused(
        world.propose_workflow(Asserted::Operator, json!({"topic": "go"}), None),
        &before,
    );
    // The genuine approval requirement is the ungranted manual plan: a
    // free-text proposal the operator must decide before its work runs.
    let proposed = world
        .propose_value(json!({"project": PROJECT, "text": FILE_PLAN}))
        .expect("the manual plan is an ordinary proposal");
    assert_eq!(proposed["state"].as_str(), Some("proposed"));
    let epic = proposed["epic"].as_str().unwrap().to_string();
    let ticket = world.issue(&epic).front.plan.unwrap().tickets[0].clone();
    assert_eq!(world.issue(&ticket).front.status, "backlog");
    world.bind_job(&ticket);
    let row = assert_one_escalation(&world, "plan", &epic);
    assert_eq!(
        failure_rows(&world, &[epic.as_str(), ticket.as_str()]).len(),
        1,
        "the undecided plan stacks exactly one operator row: {row}"
    );
    // Acknowledgment is not resolution: dismissed, the row hides — the
    // gate still refuses the plan's work and no ticket is ready.
    dismiss(&world, "issue", &epic);
    assert!(needs_rows(&world, "plan", &epic).is_empty());
    assert!(
        world.shared.plan_gate_task("cad225-job-t1").is_err(),
        "a dismissed row is not an approval"
    );
    assert_eq!(world.issue(&ticket).front.status, "backlog");
    // An agent cannot decide: approval is operator-only by connection.
    let shared = world.shared.clone();
    assert!(
        scoped(Asserted::Agent("worker".into()), || shared.rpc_plan_decide(
            &json!({"epic": epic.clone()}),
            std::process::id(),
            true
        ))
        .is_err(),
        "an agent approved the plan it needs approved"
    );
    assert!(
        world.shared.plan_gate_task("cad225-job-t1").is_err(),
        "a refused decision opened the gate"
    );
    // The operator's decision is the only clear: tickets go ready, the
    // bound gate passes, and the row is gone.
    let shared = world.shared.clone();
    scoped(Asserted::Operator, || {
        shared.rpc_plan_decide(&json!({"epic": epic.clone()}), std::process::id(), true)
    })
    .expect("the operator's plan_approve answers");
    assert_eq!(
        world.issue(&epic).front.plan.unwrap().state,
        "approved",
        "the operator decision lands on the tracker"
    );
    assert_eq!(world.issue(&ticket).front.status, "ready");
    assert!(
        world.shared.plan_gate_task("cad225-job-t1").is_ok(),
        "the resolved cause opens the plan gate"
    );
    assert!(
        needs_rows(&world, "plan", &epic).is_empty(),
        "a decided plan has no escalation left"
    );
}

/// The same provider failure served over the real socket: `agent_list`
/// answers, so the fenced lane row exists and the ticket hold must
/// merge into it — ONE operator row carrying both causes, while an
/// unrelated fenced lane stays its own row.
#[test]
fn cad225_socketed_overview_coalesces_the_same_failure_once() {
    use super::cad225_unattended_chain_acceptance::Served;
    let world = World::open();
    world.register(crate::master::ALIAS, "worker");
    let admitted = world.grant_and_admit(None);
    let ticket = admitted.ticket;
    // The seeded reviewer must be routable when the hold resolves.
    reviewer_ready(&world, "reviewer");
    record_working_for(&world, &ticket, "fworker");
    file_done_report(&world, &ticket, "fworker", now_secs() as i64, HEAD);
    // The lane's registration cwd sits under a DIFFERENT project: the
    // grant pinned the dispatch to `widgets`, so a fenced row's
    // cwd-derived project must never veto merging the same owned
    // unknown turn — only the held record's own evidence decides.
    let other_dir = world.root.path().join("other-repo");
    std::fs::create_dir_all(&other_dir).unwrap();
    crate::issue::write::project_add(
        &crate::issue::Pm::at(&world.pm()).expect("PM opens"),
        "other",
        "OTH",
        &[other_dir.to_str().unwrap().to_string()],
        &[],
        &[],
        None,
    )
    .expect("a second project registers");
    register_fake(&world, "fworker", "worker", other_dir.to_str().unwrap());
    let turn_id = dispatch_turn_on(
        &world,
        "fworker",
        Some(ticket.as_str()),
        1,
        false,
        None,
        "DISCONNECT",
    );
    world
        .shared
        .launch_actor("fworker")
        .expect("the fake lane launches");
    actor_exit(&world, "fworker");
    // An unrelated failure on another lane: its row must remain visible
    // and must never absorb this ticket's cause.
    register_fake(
        &world,
        "bystander",
        "worker",
        world.root.path().to_str().unwrap(),
    );
    let stray = dispatch_turn_on(&world, "bystander", None, 1, false, None, "DISCONNECT");
    world
        .shared
        .launch_actor("bystander")
        .expect("the unrelated lane launches");
    actor_exit(&world, "bystander");
    world
        .shared
        .route_delivery()
        .expect("the pass escalates the unknown owned turn");
    let rec = record(&world, &ticket);
    assert_eq!(rec.state, State::Escalated);
    assert!(rec.unattended_hold);
    assert_eq!(rec.unattended_turn.as_deref(), Some(turn_id.as_str()));

    // The real daemon surface: the overview's own `agent_list` probes
    // answer, which is what the socketless fixture hid.
    let _daemon = Served::start(world.root.path());
    let view = crate::overview::overview(world.root.path(), &world.pm());

    let held = failure_rows_in(&view, &[ticket.as_str(), "fworker"]);
    assert_eq!(
        held.len(),
        1,
        "one unresolved operator row serves ticket + fenced lane: {held:?}"
    );
    assert_eq!(
        held[0]["subject"]["id"].as_str(),
        Some("fworker"),
        "the merged row keeps the fence's recovery control: {held:?}"
    );
    let causes: Vec<&str> = held[0]["causes"]
        .as_array()
        .map(|c| c.iter().filter_map(|x| x["cause"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        causes.contains(&"fenced") && causes.contains(&"unattended_held"),
        "both causes are retained on the one row: {causes:?}"
    );
    // The unrelated lane's failure is a second, separate operator row —
    // coalescing must not swallow it.
    let bystander = failure_rows_in(&view, &["bystander"]);
    assert_eq!(bystander.len(), 1, "the unrelated fence keeps its row");
    let stray_causes: Vec<&str> = bystander[0]["causes"]
        .as_array()
        .map(|c| c.iter().filter_map(|x| x["cause"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        !stray_causes.contains(&"unattended_held"),
        "an unrelated lane must not inherit the ticket's hold: {stray_causes:?}"
    );
    assert!(
        world.shared.store.message(&stray).unwrap().unwrap().state == "unknown",
        "the bystander's failed turn is fenced evidence too"
    );

    // Acknowledgment is not resolution — now at the agent subject the
    // merged row actually shows under.
    dismiss(&world, "agent", "fworker");
    let after_ack = crate::overview::overview(world.root.path(), &world.pm());
    assert!(
        failure_rows_in(&after_ack, &[ticket.as_str(), "fworker"]).is_empty(),
        "the acknowledged row hides"
    );
    assert_eq!(
        world
            .shared
            .store
            .unknown_messages("fworker")
            .unwrap()
            .len(),
        1,
        "the unknown outcome still stands after acknowledgment"
    );
    assert_eq!(record(&world, &ticket).state, State::Escalated);

    // Resolution is the operator's reconcile: the fence lifts, the
    // ticket's review routes once, and no failure row remains.
    reconcile_as(
        &world,
        Asserted::Operator,
        json!({"message": &turn_id, "status": "completed", "sha": HEAD}),
    )
    .expect("operator reconcile answers");
    world
        .shared
        .route_delivery()
        .expect("the post-reconcile pass answers");
    assert_eq!(record(&world, &ticket).state, State::Reviewing);
    let routed = world.routed(&ticket);
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0]["sha"].as_str(), Some(HEAD));
    let settled_view = crate::overview::overview(world.root.path(), &world.pm());
    assert!(
        failure_rows_in(&settled_view, &[ticket.as_str(), "fworker"]).is_empty(),
        "resolution clears the escalation, not the evidence path"
    );
    assert_eq!(
        failure_rows_in(&settled_view, &["bystander"]).len(),
        1,
        "the unrelated lane's unresolved failure still stands"
    );
}

/// The provider's denied outcome — quota exhausted mid-turn — is a real
/// `Error::Provider` through the actor's fatal arm: `failed` evidence,
/// `attention` fence, ONE operator row for lane + held ticket. Fixture
/// limit: `CAD225_QUOTA_ERROR` is the provider-error variant a quota
/// denial surfaces as; it is not quota-account proof.
#[test]
fn cad225_quota_fatal_fences_lane_and_holds_once_socketed() {
    use super::cad225_unattended_chain_acceptance::Served;
    let world = World::open();
    world.register(crate::master::ALIAS, "worker");
    let admitted = world.grant_and_admit(None);
    let ticket = admitted.ticket;
    // The seeded reviewer must be routable when the hold resolves.
    reviewer_ready(&world, "reviewer");
    record_working_for(&world, &ticket, "fworker");
    file_done_report(&world, &ticket, "fworker", now_secs() as i64, HEAD);
    register_fake(
        &world,
        "fworker",
        "worker",
        world.root.path().to_str().unwrap(),
    );
    let turn_id = dispatch_turn_on(
        &world,
        "fworker",
        Some(ticket.as_str()),
        1,
        false,
        None,
        "CAD225_QUOTA_ERROR",
    );
    world
        .shared
        .launch_actor("fworker")
        .expect("the fake lane launches");
    actor_exit(&world, "fworker");
    // The fatal arm's durable outcome: `failed` evidence, `attention`
    // fence — a denied outcome is never retried and never `unknown`.
    let msg = world.shared.store.message(&turn_id).unwrap().unwrap();
    assert_eq!(msg.state, "failed", "the denied turn is recorded failed");
    assert!(
        msg.error
            .as_deref()
            .is_some_and(|e| e.contains("quota exhausted")),
        "the provider's own account is the preserved evidence: {msg:?}"
    );
    assert_eq!(
        world.shared.store.agent("fworker").unwrap().state,
        "attention",
        "a provider death fences the lane"
    );
    assert!(
        !world.shared.store.has_unknown("fworker").unwrap(),
        "a denied outcome is evidence, never an unresolved unknown"
    );
    world
        .shared
        .route_delivery()
        .expect("a router pass on the failed turn answers");
    let rec = record(&world, &ticket);
    assert_eq!(rec.state, State::Escalated);
    assert!(rec.unattended_hold);
    assert_eq!(
        rec.unattended_turn.as_deref(),
        Some(turn_id.as_str()),
        "the hold names the owned failed turn it is gated on"
    );
    // Bounded: repeat passes re-judge to the same hold — the daemon
    // never retries a denied turn on its own.
    world.shared.route_delivery().unwrap();
    let again = record(&world, &ticket);
    assert_eq!(again.state, State::Escalated);
    assert!(world.routed(&ticket).is_empty());
    let dispatch_rows = world
        .shared
        .store
        .messages("fworker")
        .unwrap()
        .into_iter()
        .filter(|m| m.source == "dispatch" && m.issue.as_deref() == Some(ticket.as_str()))
        .count();
    assert_eq!(dispatch_rows, 1, "no silent retry of the denied turn");

    // The real socket: the fenced lane row and the ticket hold are ONE
    // actionable unresolved escalation — the same owned failure.
    let _daemon = Served::start(world.root.path());
    let view = crate::overview::overview(world.root.path(), &world.pm());
    let rows = failure_rows_in(&view, &[ticket.as_str(), "fworker"]);
    assert_eq!(
        rows.len(),
        1,
        "the denied outcome stacks exactly one operator row — fenced lane and held ticket are the same failure: {rows:?}"
    );
    let causes: Vec<&str> = rows[0]["causes"]
        .as_array()
        .map(|c| c.iter().filter_map(|x| x["cause"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        causes.contains(&"fenced") && causes.contains(&"unattended_held"),
        "both causes are retained on the one row: {causes:?}"
    );

    // Acknowledgment is not resolution: the row hides, the denied
    // evidence and the hold both stand.
    dismiss(&world, "agent", "fworker");
    let after_ack = crate::overview::overview(world.root.path(), &world.pm());
    assert!(
        failure_rows_in(&after_ack, &[ticket.as_str(), "fworker"]).is_empty(),
        "the acknowledged row hides"
    );
    assert_eq!(
        world.shared.store.message(&turn_id).unwrap().unwrap().state,
        "failed",
        "acknowledgment never rewrites the denied outcome"
    );
    assert_eq!(record(&world, &ticket).state, State::Escalated);

    // Supported recovery: a denied turn cannot be reconciled (it is not
    // `unknown`) — the operator restores the lane's provider and
    // UNFENCES it explicitly through `try_resume` (what the
    // `agent_resume` RPC calls — `unfence`'s own refusal points at it),
    // then a NEW owned turn proves the real head. The failed row is
    // never relabeled.
    assert!(
        reconcile_as(
            &world,
            Asserted::Operator,
            json!({"message": &turn_id, "status": "completed", "sha": HEAD}),
        )
        .is_err(),
        "a failed turn is not reconcilable — only unknown outcomes are"
    );
    // The proof turn is a store-queued `dispatch` row — the real
    // resumed adapter completes it (turn id + `SHA:` trailer), not an
    // authenticated `master_dispatch` send, which this fixture does not
    // exercise.
    // The next owned turn is queued BEFORE the resume wakes the lane —
    // the live adapter proves the head through the real
    // `REPORT_SHA:` turn-result convention, not a seeded result.
    let proof_id = dispatch_turn_on(
        &world,
        "fworker",
        Some(ticket.as_str()),
        2,
        false,
        None,
        &format!("REPORT_SHA:{HEAD}"),
    );
    {
        let shared = world.shared.clone();
        scoped(Asserted::Operator, || shared.try_resume("fworker"))
            .expect("the operator's supported unfence answers");
    }
    // The resumed lane's adapter completes the proof turn; then disable
    // the lane so its actor exits cleanly (leaving it idle would leak
    // the thread into the shared fixture daemon).
    wait_message_state(&world, &proof_id, "completed");
    // The proof came through the lane's own adapter, not a seeded
    // result: the turn id is the provider's `fake-turn-*` handle and
    // its recorded outcome carries the head trailer the worker
    // protocol parses into the turn's sha.
    let proof = world
        .shared
        .store
        .message(&proof_id)
        .unwrap()
        .expect("the proof turn row");
    assert!(
        proof
            .turn_id
            .as_deref()
            .is_some_and(|t| t.starts_with("fake-turn-")),
        "the live adapter produced the proof turn: {proof:?}"
    );
    assert!(
        proof
            .result
            .as_ref()
            .and_then(|r| r["text"].as_str())
            .is_some_and(|t| t.contains(&format!("SHA: {HEAD}"))),
        "the adapter's own report proved {HEAD}: {proof:?}"
    );
    world
        .shared
        .store
        .set_enabled("fworker", false)
        .expect("the lane disables");
    world.shared.notify_agent("fworker");
    actor_exit(&world, "fworker");
    assert_eq!(
        world.shared.store.agent("fworker").unwrap().state,
        "stopped",
        "the unfenced lane settles, not back into attention"
    );
    world
        .shared
        .route_delivery()
        .expect("the recovery pass answers");
    assert_eq!(record(&world, &ticket).state, State::Reviewing);
    let routed = world.routed(&ticket);
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0]["sha"].as_str(), Some(HEAD));
    assert_eq!(
        world.shared.store.message(&turn_id).unwrap().unwrap().state,
        "failed",
        "the denied turn's outcome is preserved, never relabeled"
    );
    let settled_view = crate::overview::overview(world.root.path(), &world.pm());
    assert!(
        failure_rows_in(&settled_view, &[ticket.as_str(), "fworker"]).is_empty(),
        "a proven head clears the escalation"
    );
}
