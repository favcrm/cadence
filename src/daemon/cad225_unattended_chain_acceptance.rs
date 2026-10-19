//! CAD-225 chain acceptance — the accepted plan reaches the merged head
//! through the daemon's real public surface, not planted store rows.
//!
//! `cad225_unattended_acceptance.rs` holds the shared fixture (`World`,
//! `options`, seams) and the negative refusal cases. This module's job is
//! the accepted end-to-end chain, so every hop that exists as an RPC goes
//! over the fixture daemon's real unix socket with a real asserted caller
//! (`X-Cadence-Test-As` -> the server-side `Seam::admits` check):
//!
//!   operator:  workflow_approve(unattended)  -> grant
//!   operator:  plan_propose(--workflow)      -> approved epic + ticket
//!   master:    master_dispatch               -> issue start + kickoff send
//!   (store)    kickoff turn finishes with HEAD
//!   (file)     worker `done` report          -> route_delivery -> Reviewing
//!   (store)    revision turn finishes with the reviewed HEAD
//!   (file)     second `done` report          -> route_delivery -> Reviewing
//!   reviewer:  report_verdict PASS risk=auto -> Passed + verdict receipt
//!   operator:  delivery_observe OPEN         -> intent + scripted `gh pr
//!              merge --match-head-commit HEAD`
//!   operator:  delivery_observe MERGED       -> Merged + ticket done
//!
//! Two injections stand between the real verdicts and must write nothing:
//! a verdict on a moved head (stale) and the worker judging its own work
//! (self-review). Both hit the same `rpc_report_verdict` guards the real
//! verdicts cross.
//!
//! What is NOT exercised by RPC: `route_delivery` is daemon-internal (the
//! report router — `Some(0)` here — or `done_report`'s handoff calls it),
//! so this test calls it on `world.shared` exactly where production calls
//! it. The kickoff/revision turn lifecycle (`mark_running`, `finish`) is a
//! store effect the provider adapter performs, not a caller RPC.
//!
//! The checkout's `origin` is seeded so `issue start` resolves a real
//! base offline: a `url.<file>.insteadOf` rewrite keeps
//! `remote.origin.url` reading as `https://github.com/acme/widgets`
//! (what `project.yaml` recorded and what `unattended_dispatch_authority`
//! matches the grant against) while `fetch`/`ls-remote` reach the
//! on-disk seed. `gh` never runs on this path — `unattended_dispatch_authority`
//! only matches the registered project repo — and `dispatch::run`'s
//! launcher skips `dispatch_send` when the issue already holds an open
//! worktree ref.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use super::*;

/// The fixture daemon: `serve_with` on the world's state dir in a thread.
/// `Shared::new` does not take the singleton flock (it is `new_leased`),
/// so the served instance shares the world's SQLite store and delivery
/// files the way two daemon processes would — every effect below is
/// visible to both handles. Dropping signals `opts.stop` and joins.
pub(super) struct Served {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Served {
    /// Blocks until the socket exists and the daemon token is readable —
    /// `client::rpc` needs both. 30s mirrors the in-flight-attempt budget
    /// a daemon startup gets, far past fixture startup.
    pub(super) fn start(root: &Path) -> Self {
        let mut opts = options(root);
        let stop = Arc::new(AtomicBool::new(false));
        opts.stop = Some(stop.clone());
        let state = root.to_path_buf();
        let join = std::thread::spawn(move || {
            crate::daemon::serve_with(&state, opts).expect("fixture daemon serves");
        });
        let socket = root.join("cadence.sock");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !(socket.exists() && crate::test_seam::Seam::token_at(root).is_some()) {
            assert!(
                Instant::now() < deadline,
                "fixture daemon did not arm a socket+token under {root:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Served {
            stop,
            join: Some(join),
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// One verb over the fixture daemon's socket. The caller is asserted in
/// the test seam and re-checked server-side; `peer_pid` is this process.
pub(super) fn rpc(state: &Path, who: Asserted, method: &str, params: Value) -> Result<Value> {
    scoped(who, || crate::client::rpc(state, method, params))
}

/// A verdict report body for `report_verdict`: the same frontmatter shape
/// `cadence report --kind verdict` files. `risk` is the reviewer's
/// declaration — `Some("auto")` only on the PASS, since an unattended PASS
/// without one is refused and any PASS here must confer authority.
fn verdict_text(ticket: &str, verdict: &str, sha: &str, risk: Option<&str>) -> String {
    let risk_line = risk.map(|r| format!("risk: {r}\n")).unwrap_or_default();
    format!(
        "---\nkind: verdict\ntask: {ticket}\nverdict: {verdict}\nsha: {sha}\n{risk_line}---\n\n\
         Findings for {ticket} at {sha}.\n"
    )
}

/// Give `checkout` a reachable `origin/main` without touching
/// `remote.origin.url`: a seed repo holds one real commit on `main`, and a
/// repo-local `url.<file>.insteadOf` rewrite maps the recorded
/// `https://github.com/acme/widgets` fetch URL onto it. `repo_identity`
/// still reads the configured URL (identity stays `acme/widgets`);
/// `resolve_base`'s `ls-remote`/`fetch` machinery follows the rewrite, so
/// the run proceeds entirely offline. Objects fetched into `checkout`
/// make `git worktree add <base_sha>` a local operation.
fn seed_checkout_history(world: &World) {
    let seed = world.root.path().join("seed");
    let checkout = world.checkout();
    let git = |dir: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?}: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("seed.txt"), "seed\n").unwrap();
    git(&seed, &["add", "seed.txt"]);
    git(
        &seed,
        &[
            "-c",
            "user.email=cad225@fixture",
            "-c",
            "user.name=cad225",
            "commit",
            "-m",
            "seed",
        ],
    );
    let rewrite = format!("url.file://{}.insteadOf", seed.display());
    let mapped = format!("https://github.com/{REPO}.git");
    git(&checkout, &["config", rewrite.as_str(), mapped.as_str()]);
}

/// The kickoff-turn id `master_dispatch` leaves queued for `worker` on
/// `ticket` — the row `unattended_worker_complete` will later match
/// against the done report.
fn kickoff_turn(world: &World, ticket: &str) -> String {
    let turn = world
        .shared
        .store
        .worker_issue_turn(ticket, "worker")
        .expect("turn reads")
        .unwrap_or_else(|| panic!("{ticket}'s dispatch left no daemon kickoff turn for worker"));
    assert_eq!(turn.state, "queued", "kickoff stands queued for the worker");
    turn.id
}

/// Stand a store turn at `running`, then complete it with `result` — the
/// provider-adapter effects, in the order the adapter performs them.
fn finish_turn(world: &World, id: &str, result: Value) {
    world.shared.store.mark_running(id, "cad225").expect("ack");
    let message = world
        .shared
        .store
        .message(id)
        .expect("turn reads")
        .expect("turn exists");
    world
        .shared
        .store
        .finish(&message, "completed", &result, None)
        .expect("turn completes");
}

/// Delivery record `state` for `ticket`, fresh from delivery.json.
fn record(world: &World, ticket: &str) -> Record {
    let mut all = delivery::load(world.root.path()).expect("delivery reads");
    all.remove(ticket)
        .unwrap_or_else(|| panic!("{ticket} has a delivery record"))
}

/// Report filenames under the ticket's reports dir — refused verdicts
/// must add none.
fn report_files(world: &World, ticket: &str) -> BTreeSet<String> {
    let dir = world.pm().join(PROJECT).join(ticket).join("reports");
    std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn cad225_unattended_chain_goal_to_merged() {
    let world = World::open();
    let state = world.root.path();
    world.register("master", "pm");
    seed_checkout_history(&world);
    world.set_case("in");
    world.set_head(HEAD);
    let _daemon = Served::start(state);
    // `master_policy` narrows every master call to `MASTER_APP_ALLOWED`
    // when the master's running turn is unresolvable (fail closed) or
    // sits in an app conversation; only a home-thread turn reaches
    // `master_dispatch`. Stand the master's running turn the home
    // thread's kickoff leaves AFTER `Served::start` — daemon recovery
    // sweeps in-flight rows to `unknown` on boot, so a turn planted
    // before serve would be fenced evidence, never `running`. No thread
    // entry means `message_conversation` -> `Ok(None)` ->
    // `MasterScope::Home`. The turn stays `running` for the test's
    // span, which is the fixture context, not the guard.
    let (dup, _state) = world
        .shared
        .store
        .enqueue_steered(
            "master",
            "home thread turn",
            None,
            "cad225-home-turn",
            "chat",
            None,
            None,
            None,
            &Sender::Unattributed,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
        )
        .expect("the master's home turn enqueues");
    assert!(!dup, "home turn id must not collide");
    world
        .shared
        .store
        .mark_running("cad225-home-turn", "cad225-home")
        .expect("the home turn runs");

    // 1. The operator grants one exact render over the live socket.
    let granted = rpc(
        state,
        Asserted::Operator,
        "workflow_approve",
        world.unattended(&[PATH], bound_inputs(), None, None),
    )
    .expect("operator unattended grant");
    let grant_id = world.require_native_grant(&granted);

    // 2. The operator proposes the grant's exact render — admitted
    // approved-at-create through the same RPC gate the refusal cases hit.
    let proposed = rpc(
        state,
        Asserted::Operator,
        "plan_propose",
        json!({
            "project": PROJECT,
            "workflow": WORKFLOW,
            "inputs": bound_inputs(),
        }),
    )
    .expect("exact render admits");
    assert_eq!(
        proposed["state"].as_str(),
        Some("approved"),
        "grant admits at create: {proposed}"
    );
    let epic = proposed["epic"].as_str().unwrap().to_string();
    let plan = world.issue(&epic).front.plan.expect("epic plan");
    assert_eq!(plan.state, "approved");
    assert_eq!(plan.tickets.len(), 1);
    let ticket = plan.tickets[0].clone();
    // The scripted `gh` binds the PR title to the real ticket id —
    // `classify`'s exact-title check must see it, never a default.
    std::fs::write(world.root.path().join("issue-id"), &ticket).unwrap();
    assert_eq!(world.issue(&ticket).front.status, "ready");
    let run = world
        .shared
        .unattended_authorization_for_issue(&ticket)
        .expect("bound run reads")
        .expect("admitted ticket is bound");
    assert_eq!(run.grant_id, grant_id);
    assert_eq!(run.repo, REPO);

    // 3. The master dispatches the ticket over the socket: plan gate,
    // grant repo check, issue start (base resolve against the seeded
    // origin, real worktree lane), dispatch_send kickoff, Working record.
    let dispatched = rpc(
        state,
        Asserted::Agent("master".into()),
        "master_dispatch",
        json!({"issue": ticket, "to": "worker"}),
    )
    .expect("the master dispatches a granted ready ticket");
    assert_ne!(
        dispatched["dispatched"].as_bool(),
        Some(false),
        "dispatch did not run: {dispatched}"
    );
    assert_eq!(world.issue(&ticket).front.status, "doing");
    let kickoff = kickoff_turn(&world, &ticket);
    // A second dispatch is refused by the state guard having written
    // nothing new — the dispatch lock makes the check-and-run atomic.
    assert!(
        rpc(
            state,
            Asserted::Agent("master".into()),
            "master_dispatch",
            json!({"issue": ticket}),
        )
        .is_err(),
        "a 'doing' ticket cannot be dispatched again"
    );

    // 4. The worker's turn completes with HEAD; its done report routes to
    // review intake. `route_delivery` is daemon-internal (router-disabled
    // fixture) — called exactly where the router would call it.
    finish_turn(&world, &kickoff, json!({"sha": HEAD}));
    world.done_report(&ticket, now_secs() as i64 + 60, HEAD);
    world
        .shared
        .route_delivery()
        .expect("intake routes the report");
    let rec = record(&world, &ticket);
    assert_eq!(rec.state, State::Reviewing, "done report routes to review");
    assert_eq!(rec.head.as_deref(), Some(HEAD));
    assert_eq!(rec.reviewer.as_deref(), Some("reviewer"));
    assert_eq!(world.routed(&ticket).len(), 1, "one review kickoff routed");

    // 5a. A verdict on a moved head is refused by the stale-sha guard and
    // writes nothing — no receipt, no report file, no state change.
    let commits_before = world.commits();
    let reports_before = report_files(&world, &ticket);
    let stale = rpc(
        state,
        Asserted::Agent("reviewer".into()),
        "report_verdict",
        json!({"issue": ticket, "text": verdict_text(&ticket, "revise", MOVED, None)}),
    )
    .expect_err("stale verdict refuses");
    assert!(
        stale.to_string().contains("stale verdict"),
        "moved head names the stale-sha guard, got {stale}"
    );

    // 5b. The worker cannot judge its own work: the worker-check fires
    // before state/assignment, so even the assigned head is refused.
    let self_verdict = rpc(
        state,
        Asserted::Agent("worker".into()),
        "report_verdict",
        json!({"issue": ticket, "text": verdict_text(&ticket, "pass", HEAD, Some("auto"))}),
    )
    .expect_err("a worker never judges its own work");
    assert!(
        self_verdict
            .to_string()
            .contains("never judges its own work"),
        "self-verdict names the worker guard, got {self_verdict}"
    );
    let rec = record(&world, &ticket);
    assert_eq!(
        rec.state,
        State::Reviewing,
        "refused verdicts hold the state"
    );
    assert!(
        rec.verdict.is_none(),
        "no verdict record from refused verdicts"
    );
    assert_eq!(
        report_files(&world, &ticket),
        reports_before,
        "refused verdicts filed no report"
    );
    assert_eq!(world.routed(&ticket).len(), 1, "no extra review routing");
    assert_eq!(
        world.commits(),
        commits_before,
        "refused verdicts committed nothing to the tracker"
    );

    // 6. The assigned reviewer's REVISE verdict on the under-review head
    // files a report, increments revisions and queues the revision turn —
    // the row `unattended_worker_complete` matches by `revise-<hash>` and
    // requires a NULL sender_uid (daemon-sent, never caller-attributed).
    let revised = rpc(
        state,
        Asserted::Agent("reviewer".into()),
        "report_verdict",
        json!({"issue": ticket, "text": verdict_text(&ticket, "revise", HEAD, None)}),
    )
    .expect("the assigned reviewer's REVISE is taken");
    assert_eq!(
        revised["delivery"]["state"].as_str(),
        Some("working"),
        "the REVISE verdict returns the delivery record it reopened: {revised}"
    );
    let rec = record(&world, &ticket);
    assert_eq!(rec.state, State::Working);
    assert_eq!(rec.revisions, 1);
    let verdict_report = rec
        .verdict
        .as_ref()
        .expect("revise verdict recorded")
        .report
        .clone();
    let revise_id = format!("revise-{}", &sha256_hex(verdict_report.as_bytes())[..16]);
    let revision = world
        .shared
        .store
        .worker_issue_turn(&ticket, "worker")
        .expect("the lane's latest owned turn reads")
        .expect("the REVISE hand-back queued a native scoped turn");
    assert_eq!(
        revision.id, revise_id,
        "the lane's latest owned turn is the daemon's revision hand-back: {revision:?}"
    );
    assert_eq!(revision.state, "queued");

    // 7. The revision completes at the same head — the worker's fix lands
    // on the reviewed sha (a moved head here would strand the round).
    finish_turn(&world, &revise_id, json!({"sha": HEAD}));
    world.done_report(&ticket, now_secs() as i64 + 120, HEAD);
    world.shared.route_delivery().expect("second report routes");
    let rec = record(&world, &ticket);
    assert_eq!(
        rec.state,
        State::Reviewing,
        "revision report routes back to review"
    );
    assert_eq!(
        world.routed(&ticket).len(),
        2,
        "second review kickoff routed"
    );

    // 8. The assigned reviewer passes the under-review head with the
    // explicit `auto` risk declaration an unattended PASS requires. The
    // daemon-attested receipt must satisfy both export scopes.
    rpc(
        state,
        Asserted::Agent("reviewer".into()),
        "report_verdict",
        json!({"issue": ticket, "text": verdict_text(&ticket, "pass", HEAD, Some("auto"))}),
    )
    .expect("the assigned reviewer's PASS is taken");
    let rec = record(&world, &ticket);
    assert_eq!(rec.state, State::Passed);
    assert_eq!(
        rec.verdict.as_ref().map(|v| v.verdict.as_str()),
        Some("pass")
    );
    assert_eq!(rec.verdict.as_ref().map(|v| v.sha.as_str()), Some(HEAD));
    assert!(
        world
            .shared
            .store
            .review_export_recorded(&rec)
            .expect("receipt reads"),
        "the daemon-attested verdict receipt matches the export scope"
    );
    assert!(
        world
            .shared
            .store
            .unattended_review_recorded(&rec)
            .expect("receipt reads"),
        "the receipt carries the reviewer's auto declaration"
    );

    // 9. The operator's OPEN observation enqueues: the durable intent
    // lands before the scripted `gh pr merge --match-head-commit HEAD`
    // (the mutation line) — while compare/pr-view evidence stays reads.
    let compare_before = world.gh_calls("compare");
    let enqueue = rpc(
        state,
        Asserted::Operator,
        "delivery_observe",
        json!({
            "issue": ticket, "head": HEAD, "pr_state": "OPEN",
            "ci_green": true, "additions": 1, "deletions": 0, "files": 1,
        }),
    )
    .expect("granted, passed, in-scope head enqueues");
    assert!(
        enqueue["unattended_refusal"].is_null(),
        "in-scope enqueue records no refusal: {enqueue}"
    );
    let intent = world
        .shared
        .store
        .unattended_merge_intent(&ticket, HEAD)
        .expect("intent reads")
        .expect("the durable intent precedes the merge");
    assert_eq!(
        intent["head"].as_str(),
        Some(HEAD),
        "intent binds the passed head"
    );
    assert_eq!(
        intent["grant_id"].as_str(),
        Some(grant_id.as_str()),
        "intent binds the granting authority"
    );
    let merges = world.snap().merges;
    assert_eq!(
        merges.lines().count(),
        1,
        "exactly one merge mutation ran: {merges}"
    );
    assert!(
        merges.contains("pr merge") && merges.contains("--match-head-commit"),
        "merge mutation is head-pinned: {merges}"
    );
    assert!(
        merges.contains(HEAD),
        "the mutation names the passed head: {merges}"
    );
    assert!(
        world.gh_calls("compare") > compare_before,
        "the diff classification evidence was actually read"
    );
    assert_eq!(
        world.recorded_merge_approvals(),
        0,
        "landing needed no merge approval — the grant authorized it"
    );

    // The mutation succeeded but the queue service has not admitted the
    // head yet: the record stays `passed`, marked pending — a known
    // queue state, no hold, no receipt.
    let rec = record(&world, &ticket);
    assert_eq!(rec.state, State::Passed, "pending admission stays passed");
    assert!(rec.unattended_pending, "pending queue admission is marked");
    assert!(!rec.merge_ready(), "pending never presents a merge card");
    assert!(
        world
            .shared
            .store
            .unattended_merge_enqueued(&ticket, HEAD)
            .expect("enqueued reads")
            .is_none(),
        "a pending request wrote an enqueue receipt"
    );

    // 9b. The queue service admits the head — the native entry is the
    //    only proof; the reconcile never replays the mutation.
    world.queue_admitted();
    let admitted = rpc(
        state,
        Asserted::Operator,
        "delivery_observe",
        json!({
            "issue": ticket, "head": HEAD, "pr_state": "OPEN",
            "ci_green": true, "auto_merge": true,
            "additions": 1, "deletions": 0, "files": 1,
        }),
    )
    .expect("the observed queue entry admits");
    assert!(
        admitted["unattended_refusal"].is_null(),
        "a native queue entry records no refusal: {admitted}"
    );
    let rec = record(&world, &ticket);
    assert_eq!(rec.state, State::Enqueued, "the entry lands Enqueued");
    assert!(!rec.unattended_pending, "admission clears pending");
    let enqueued = world
        .shared
        .store
        .unattended_merge_enqueued(&ticket, HEAD)
        .expect("enqueued reads")
        .expect("the observed queue entry is recorded");
    assert!(
        enqueued["queue_entry"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "the receipt carries the queue's own entry id"
    );
    assert_eq!(
        world.snap().merges.lines().count(),
        1,
        "reconcile never replays the mutation"
    );

    // 10. MERGED settles: ticket done, record merged, no second merge.
    rpc(
        state,
        Asserted::Operator,
        "delivery_observe",
        json!({
            "issue": ticket, "head": HEAD, "pr_state": "MERGED",
            "ci_green": true, "additions": 1, "deletions": 0, "files": 1,
        }),
    )
    .expect("merged head settles");
    let rec = record(&world, &ticket);
    assert_eq!(rec.state, State::Merged);
    assert_eq!(
        world.issue(&ticket).front.status,
        "done",
        "settle completes the ticket"
    );
    assert_eq!(
        world.snap().merges.lines().count(),
        1,
        "settle merges nothing itself"
    );
}

/// Release the parked `upload-pack` on drop — a test panic must never
/// strand the daemon thread inside the fixture's git wait. `hold()`
/// returns the armed guard so the release reads as one deliberate step
/// at the call site.
struct RemoteRelease {
    state: PathBuf,
}

impl RemoteRelease {
    fn hold(state: &Path) -> Self {
        Self {
            state: state.to_path_buf(),
        }
    }
    fn release(&self) {
        let _ = std::fs::write(self.state.join("up-go"), "1\n");
    }
}

impl Drop for RemoteRelease {
    fn drop(&mut self) {
        self.release();
    }
}

/// CAD-225: automatic dispatch must carry live authority to the send,
/// not only past `master_dispatch`'s preflight. `dispatch::run` does its
/// blocking base resolution BEFORE the kickoff send; here the fixture
/// checkout's `remote.origin.uploadpack` parks that remote read long
/// enough for the granted TTL to lapse, then lets git proceed. On the
/// flawed path the operator-classified self-RPC send still enqueues.
/// The `up-call` marker proves the ordering — nothing before
/// `unattended_dispatch_authority`'s bound-repo selection touches `origin`,
/// so the parked call can only be `resolve_base` running AFTER the
/// live-grant preflight, inside `issue::dispatch::run`.
#[test]
fn cad225_expired_authority_must_not_dispatch_after_blocking_setup() {
    let world = World::open();
    let state = world.root.path();
    world.register("master", "pm");
    seed_checkout_history(&world);
    let _daemon = Served::start(state);
    // Same fixture context the chain test stands AFTER `Served::start`:
    // a running home turn so `master_policy` scopes this as a home-thread
    // master call rather than fencing it unresolved.
    let (dup, _state) = world
        .shared
        .store
        .enqueue_steered(
            "master",
            "home thread turn",
            None,
            "cad225-home-turn",
            "chat",
            None,
            None,
            None,
            &Sender::Unattributed,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
        )
        .expect("the master's home turn enqueues");
    assert!(!dup, "home turn id must not collide");
    world
        .shared
        .store
        .mark_running("cad225-home-turn", "cad225-home")
        .expect("the home turn runs");

    // Grant (short TTL) and admit the exact bound render, both over the
    // real socket — the same automatic authority the chain test proves.
    let granted = rpc(
        state,
        Asserted::Operator,
        "workflow_approve",
        world.unattended(&[PATH], bound_inputs(), Some(10), None),
    )
    .expect("operator unattended grant");
    let grant_id = world.require_native_grant(&granted);
    let proposed = rpc(
        state,
        Asserted::Operator,
        "plan_propose",
        json!({
            "project": PROJECT,
            "workflow": WORKFLOW,
            "inputs": bound_inputs(),
        }),
    )
    .expect("exact render admits");
    let epic = proposed["epic"].as_str().unwrap().to_string();
    let plan = world.issue(&epic).front.plan.expect("epic plan");
    assert_eq!(plan.state, "approved");
    assert_eq!(plan.tickets.len(), 1);
    let ticket = plan.tickets[0].clone();
    let run = world
        .shared
        .unattended_authorization_for_issue(&ticket)
        .expect("bound run reads")
        .expect("admitted ticket is bound");
    assert_eq!(run.grant_id, grant_id);
    assert!(
        run.expires_at > now_secs(),
        "fixture grant must still be live when the dispatch starts"
    );

    // Park the checkout's remote transport: `ls-remote`/`fetch` against
    // the seeded `file://` rewrite still forks `git-upload-pack`, and
    // `remote.origin.uploadpack` names the program git runs for it.
    // The wrapper signals the daemon thread arrived at remote base
    // resolution, parks until `up-go`, then execs the real
    // `git-upload-pack` — a repo-local config hook on fixture state,
    // never a production seam. Its own deadline fails the remote read
    // rather than parking forever if the test never releases it.
    let wrapper = state.join("up-pack");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\n\
         : > \"$(dirname \"$0\")/up-call\"\n\
         wait=0\n\
         while [ ! -f \"$(dirname \"$0\")/up-go\" ]; do\n\
         \tsleep 0.2\n\
         \twait=$((wait + 1))\n\
         \tif [ $wait -gt 300 ]; then exit 1; fi\n\
         done\n\
         exec git-upload-pack \"$@\"\n",
    )
    .expect("upload-pack wrapper writes");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("wrapper is executable");
    }
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(world.checkout())
        .args(["config", "remote.origin.uploadpack"])
        .arg(&wrapper)
        .status()
        .expect("fixture git");
    assert!(git.success(), "remote.origin.uploadpack configures");

    let release = RemoteRelease::hold(state);
    let state_dir = state.to_path_buf();
    let dispatched_ticket = ticket.clone();
    let dispatch = std::thread::spawn(move || {
        rpc(
            &state_dir,
            Asserted::Agent("master".into()),
            "master_dispatch",
            json!({"issue": dispatched_ticket, "to": "worker"}),
        )
    });
    assert!(
        world.wait_for("up-call", 30),
        "the automatic dispatch never reached its blocking remote setup \
         — preflight must have refused early or remote resolution never ran"
    );
    // The grant was proved live before this point; let its TTL fully
    // lapse while `resolve_base` is still parked.
    let wait = (run.expires_at - now_secs() + 1.0).clamp(0.0, 30.0);
    std::thread::sleep(Duration::from_secs_f64(wait));
    world.assert_invalid_run(&ticket);
    release.release();
    let outcome = dispatch
        .join()
        .expect("the dispatch thread answered, never deadlocked");
    // Only a standing server-side refusal counts: `rejected`/`gate` are
    // typed wire kinds the daemon answers for invalid authority, while a
    // transport/timeout/local failure arrives as `internal`/`unknown`/
    // `busy` and must not masquerade as the guard.
    assert!(
        match &outcome {
            Err(e) => matches!(e.kind(), "rejected" | "gate"),
            Ok(body) =>
                body["dispatched"].as_bool() == Some(false)
                    || body["unattended_refusal"]
                        .as_str()
                        .is_some_and(|w| !w.is_empty()),
        },
        "automatic dispatch whose authority expired inside its blocking \
         setup must be refused by the daemon, not enqueued: {outcome:?}"
    );
    let rows = world
        .shared
        .store
        .messages("worker")
        .expect("worker lane reads")
        .iter()
        .filter(|m| m.issue.as_deref() == Some(ticket.as_str()))
        .count();
    assert_eq!(
        rows, 0,
        "a refused automatic dispatch wrote a kickoff turn for {ticket}"
    );
}

// ---- CAD-225 revision blockers ---------------------------------------------
//
// Two review findings this file independently encodes:
//
// 1. The bound deadline is proved BEFORE the queue write transaction —
//    `dispatch_send`/`send_issue_followup` check `unattended_dispatch_authority`
//    while `enqueue_steered`'s `BEGIN IMMEDIATE` can still be parked behind
//    a writer the daemon does not control. A second, isolated SQLite
//    connection holding `BEGIN IMMEDIATE` past the grant expiry must make
//    the admission refuse at the writer boundary — never write the row.
// 2. `granted_semantics` appends the epic body after a bare `body=`
//    delimiter, so moving the body's head into `plan.reason` (a field that
//    legitimately parses multiline values) leaves the canonical string —
//    and therefore the fingerprint — byte-identical. The admission's
//    semantic guard must refuse the moved body anyway.

/// A real second write connection to the world's SQLite store: the same
/// file, a fresh `BEGIN IMMEDIATE`, held open until `release_at`, then
/// rolled back. This is a foreign writer competing for the daemon's one
/// database writer slot — the boundary `enqueue_steered`'s `write_tx`
/// waits on — never a stubbed or test-only seam inside the daemon.
struct StoreWriterHold {
    armed: mpsc::Receiver<Result<()>>,
    released: mpsc::Receiver<Instant>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl StoreWriterHold {
    /// Opens `<state>/cadence.sqlite3` and takes its write lock. `start`
    /// returns only once `BEGIN IMMEDIATE` holds (or failed), so the
    /// caller knows the lock is owned before the operation under test
    /// begins — it can never "race ahead" of this fixture.
    fn start(db: PathBuf, release_at: Instant) -> Self {
        let (armed_tx, armed) = mpsc::channel();
        let (released_tx, released) = mpsc::channel();
        let join = std::thread::spawn(move || {
            let held = (|| -> rusqlite::Result<rusqlite::Connection> {
                let conn = rusqlite::Connection::open(&db)?;
                conn.busy_timeout(Duration::from_millis(250))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                Ok(conn)
            })();
            match held {
                Ok(conn) => {
                    let _ = armed_tx.send(Ok(()));
                    let now = Instant::now();
                    if release_at > now {
                        std::thread::sleep(release_at - now);
                    }
                    // The reported instant is the PRE-unlock lower
                    // bound, taken before ROLLBACK is issued — an answer
                    // stamped before it provably never crossed the
                    // writer boundary; one after it waited for or
                    // arrived at a lock this fixture had already begun
                    // releasing.
                    let pre_release = Instant::now();
                    let _ = conn.execute_batch("ROLLBACK");
                    let _ = released_tx.send(pre_release);
                    return;
                }
                Err(e) => {
                    let _ = armed_tx.send(Err(e.into()));
                }
            }
            let _ = released_tx.send(Instant::now());
        });
        let me = Self {
            armed,
            released,
            join: Some(join),
        };
        me.armed
            .recv_timeout(Duration::from_secs(30))
            .expect("the fixture writer reported back")
            .expect("the fixture's BEGIN IMMEDIATE acquired the write lock");
        me
    }

    /// The instant the write lock actually went away — proof an answer
    /// received after this point could only have been computed on the
    /// far side of the writer boundary.
    fn release_proof(&self) -> Instant {
        self.released
            .recv_timeout(Duration::from_secs(30))
            .expect("the fixture writer released its transaction")
    }
}

impl Drop for StoreWriterHold {
    fn drop(&mut self) {
        let _ = self.released.recv_timeout(Duration::from_secs(30));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Arm the foreign writer at `expires_at - arm_before` and release it at
/// `expires_at + release_after`: the store's queue write then waits on a
/// live boundary whose release lands AFTER the grant's deadline, inside
/// the 5s busy_timeout, so only a deadline checked inside the write can
/// refuse it.
fn hold_writer_past_grant(
    world: &World,
    expires_at: f64,
    arm_before: f64,
    release_after: f64,
) -> StoreWriterHold {
    let arm_at = Instant::now()
        + Duration::from_secs_f64((expires_at - now_secs() - arm_before).clamp(0.0, 20.0));
    let release_at = Instant::now()
        + Duration::from_secs_f64((expires_at - now_secs() + release_after).max(0.0));
    let now = Instant::now();
    if arm_at > now {
        std::thread::sleep(arm_at - now);
    }
    StoreWriterHold::start(world.root.path().join("cadence.sqlite3"), release_at)
}

/// The typed server refusal the deadline guard must answer with —
/// `rejected`/`gate`/`conflict` are authority answers; `busy`,
/// `internal` and `unknown` are contention, local failure and lost
/// transport and can never count as the queue refusing the turn.
/// `answered` is the instant the call returned — it must sit on the far
/// side of the writer's pre-release mark: an answer the daemon produced
/// before the foreign `BEGIN IMMEDIATE` began releasing never crossed
/// the queue's write boundary.
fn assert_deadline_refused<T: std::fmt::Debug>(
    outcome: &Result<T>,
    answered: Instant,
    released: Instant,
) {
    assert!(
        answered >= released,
        "the admission answered before the queue writer began releasing — \
         it never waited at the store's write boundary: {outcome:?}"
    );
    assert!(
        matches!(outcome, Err(e) if matches!(e.kind(), "rejected" | "gate" | "conflict")),
        "admission after an expired grant must be a typed refusal, not a \
         queued turn and not a busy/internal/unknown transport failure: {outcome:?}"
    );
}

/// Review blocker 1, initial admission: the master's automatic dispatch
/// (real `master_dispatch` -> `issue::dispatch::run` -> socketed
/// `dispatch_send`, unattended kickoff) reaches `enqueue_steered` while a
/// foreign SQLite writer owns the store's write lock. The lock is
/// released only after the grant's deadline, inside the writer's busy
/// window, so the enqueue genuinely ran `BEGIN IMMEDIATE` during a live
/// grant and acquired it over a dead one. Only an inside-the-transaction
/// bound-deadline check can refuse it: no kickoff row, no
/// `master_dispatched` event, and a closed `message` ref that counts no
/// live binding.
#[test]
fn cad225_grant_deadline_binds_the_kickoff_queue_write() {
    let world = World::open();
    let state = world.root.path();
    world.register("master", "pm");
    seed_checkout_history(&world);
    let _daemon = Served::start(state);
    // The home-thread turn `master_policy` requires before a master's
    // `master_dispatch` is in scope — planted after `Served::start` so the
    // boot sweep does not fence it `unknown` (chain-test pattern).
    let (dup, _state) = world
        .shared
        .store
        .enqueue_steered(
            "master",
            "home thread turn",
            None,
            "cad225-home-turn",
            "chat",
            None,
            None,
            None,
            &Sender::Unattributed,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
        )
        .expect("the master's home turn enqueues");
    assert!(!dup, "home turn id must not collide");
    world
        .shared
        .store
        .mark_running("cad225-home-turn", "cad225-home")
        .expect("the home turn runs");

    // Grant (short TTL) and admit the exact bound render over the real
    // socket — authority identical to the chain test.
    let granted = rpc(
        state,
        Asserted::Operator,
        "workflow_approve",
        world.unattended(&[PATH], bound_inputs(), Some(12), None),
    )
    .expect("operator unattended grant");
    let grant_id = world.require_native_grant(&granted);
    let proposed = rpc(
        state,
        Asserted::Operator,
        "plan_propose",
        json!({
            "project": PROJECT,
            "workflow": WORKFLOW,
            "inputs": bound_inputs(),
        }),
    )
    .expect("exact render admits");
    let epic = proposed["epic"].as_str().unwrap().to_string();
    let plan = world.issue(&epic).front.plan.expect("epic plan");
    assert_eq!(plan.tickets.len(), 1);
    let ticket = plan.tickets[0].clone();
    let run = world
        .shared
        .unattended_authorization_for_issue(&ticket)
        .expect("bound run reads")
        .expect("admitted ticket is bound");
    assert_eq!(run.grant_id, grant_id);
    assert!(
        run.expires_at > now_secs() + 2.0,
        "the fixture grant must still be comfortably live at dispatch"
    );

    // The store's writer boundary is held from just before expiry to just
    // past it. `dispatch_send`'s enqueue is the only store write inside
    // `master_dispatch` before the record/event tail, so whatever still
    // waits once the foreign lock falls IS the queue admission — the
    // preflight expiry checks happen on read connections this writer
    // never blocks.
    let block = hold_writer_past_grant(&world, run.expires_at, 2.5, 0.7);
    let (done_tx, done) = mpsc::channel();
    let state_dir = state.to_path_buf();
    let dispatch_ticket = ticket.clone();
    let call = std::thread::spawn(move || {
        let outcome = rpc(
            &state_dir,
            Asserted::Agent("master".into()),
            "master_dispatch",
            json!({"issue": dispatch_ticket, "to": "worker"}),
        );
        let _ = done_tx.send((outcome, Instant::now()));
    });
    // While the foreign writer still owns the lock AND the grant is
    // already dead, the admission must still be parked at the boundary —
    // an answer inside the hold proves an earlier refusal path, never
    // the queue's write wait.
    let probe_at =
        Instant::now() + Duration::from_secs_f64((run.expires_at - now_secs() + 0.15).max(0.0));
    if probe_at > Instant::now() {
        std::thread::sleep(probe_at - Instant::now());
    }
    assert!(
        done.try_recv().is_err(),
        "the dispatch answered while the store writer was still held — \
         it refused at the preflight gate, never reaching the queue write"
    );
    let released = block.release_proof();
    let (outcome, answered) = done
        .recv_timeout(Duration::from_secs(30))
        .expect("the dispatch answered once the write lock fell");
    call.join().expect("dispatch thread");
    assert_deadline_refused(&outcome, answered, released);

    let rows = world
        .shared
        .store
        .messages("worker")
        .expect("worker lane reads")
        .iter()
        .filter(|m| m.issue.as_deref() == Some(ticket.as_str()))
        .count();
    assert_eq!(
        rows, 0,
        "the expired grant's automatic kickoff still enqueued for {ticket}"
    );
    assert!(
        world
            .shared
            .store
            .events_tail(crate::store::Store::DAEMON_STREAM, 100)
            .unwrap()
            .iter()
            .all(|e| !(e.kind == "master_dispatched"
                && e.payload["issue"].as_str() == Some(ticket.as_str()))),
        "a refused automatic dispatch recorded a master_dispatched event"
    );
    assert!(
        world
            .issue(&ticket)
            .front
            .refs
            .iter()
            .all(|r| r.kind != "message" || r.closed == Some(true)),
        "the refused kickoff's tracker ref stayed closed history, never a live binding"
    );
    // The grant is spent, not consumed by this attempt — revalidation
    // reads it as dead authority, the same `Err` a revoked grant gives.
    world.assert_invalid_run(&ticket);
}

/// Review blocker 1, native follow-up: `send_issue_followup` is the
/// follow-up queue admission a bound run performs — a genuinely
/// different consumer of the same enqueue than the kickoff
/// (daemon-derived `source='review'` row, owned issue/worktree tags).
/// The test drives the producer itself so the refusal lands as the
/// typed `Err` `send_issue_followup` returns — never prose `fix_turn`
/// folds it into; `checkup_idle_lane` then supplies the real operator
/// surface (checkup event, parked delivery). With the store's write
/// lock held across the grant's deadline, the follow-up must refuse at
/// the writer boundary too: the row never exists.
#[test]
fn cad225_grant_deadline_binds_the_followup_queue_write() {
    let world = World::open();
    let admitted = world.grant_and_admit(Some(12));
    // The lane's original dispatch kickoff, run and completed — the
    // native queue record `send_issue_followup` derives its lane tags
    // from.
    world.dispatch_turn(&admitted.ticket, 1, true, Some(Some(HEAD)));
    // Red CI on the recorded head is the checkup's one fix trigger.
    world.save_red(&admitted.ticket);
    let run = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .expect("bound run reads")
        .expect("admitted ticket is bound");
    assert!(
        run.expires_at > now_secs() + 2.0,
        "the fixture grant must still be live when the follow-up arrives"
    );

    // The real mid `fix_turn` would use — `checkup-fix-<ticket>-<head>` —
    // so `send_issue_followup` derives the same lane and writes (or, on
    // a correct daemon, refuses) the same row the checkup would.
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
    let rec = record(&world, &admitted.ticket);
    let block = hold_writer_past_grant(&world, run.expires_at, 2.5, 0.7);
    let (done_tx, done) = mpsc::channel();
    let shared = world.shared.clone();
    let send_mid = mid.clone();
    let call = std::thread::spawn(move || {
        // The same production boundary `checkup_idle_lane`'s fix path
        // reaches — the typed `Err` it returns is the producer's own
        // refusal signal, before any prose wrapping.
        let outcome = shared.send_issue_followup(
            &rec,
            &send_mid,
            "fix the red ci check on the recorded head",
        );
        let _ = done_tx.send((outcome, Instant::now()));
    });
    // While the foreign writer still owns the lock AND the grant is
    // already dead, the producer must still be parked at the boundary —
    // an answer inside the hold proves an earlier refusal path, never
    // the queue's write wait.
    let probe_at =
        Instant::now() + Duration::from_secs_f64((run.expires_at - now_secs() + 0.15).max(0.0));
    if probe_at > Instant::now() {
        std::thread::sleep(probe_at - Instant::now());
    }
    assert!(
        done.try_recv().is_err(),
        "the follow-up answered while the store writer was still held — \
         it refused before the queue write wait"
    );
    let released = block.release_proof();
    let (outcome, answered) = done
        .recv_timeout(Duration::from_secs(30))
        .expect("the follow-up answered once the write lock fell");
    call.join().expect("follow-up thread");
    assert_deadline_refused(&outcome, answered, released);

    // The record as the checkup receives it — the refusal path must
    // leave every field it does not own exactly here (see below).
    let pre = record(&world, &admitted.ticket);

    // The real operator surface: with no fix row queued, the checkup
    // visit on the same report escalates the lane and parks its
    // delivery record — `fix_turn` hits the same refusal and records it.
    world
        .shared
        .checkup_idle_lane(&world.worker_agent(), &[report])
        .expect("the checkup visit answers");
    assert!(
        world
            .shared
            .store
            .message(&mid)
            .expect("turn reads")
            .is_none(),
        "the expired grant's bound follow-up still enqueued as {mid}"
    );
    let checkups: Vec<Value> = world
        .shared
        .store
        .events_tail("worker", 20)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "checkup")
        .map(|e| e.payload)
        .collect();
    assert_eq!(
        checkups.len(),
        1,
        "the checkup records exactly one outcome for the held lane"
    );
    assert_eq!(
        checkups[0]["outcome"].as_str(),
        Some("escalate"),
        "the deadline refusal escalates to the operator, never dispatches a fix: {checkups:?}"
    );
    let rec = record(&world, &admitted.ticket);
    assert!(
        rec.unattended_hold,
        "the bound lane is held for the operator after the refused follow-up"
    );
    // The refusal path marks the hold and leaves the record otherwise
    // untouched — the fix arm's success bookkeeping (`enter(Working)`
    // plus clearing verdict/readiness/pending) never ran. `save_red`
    // already plants `Working`, so the honest no-restart marker is the
    // record's entry stamp and readiness fields sitting exactly where
    // the fixture put them, not a state change.
    assert_eq!(
        rec.state, pre.state,
        "the refused follow-up moved the lane's state"
    );
    assert_eq!(
        rec.since, pre.since,
        "the refused follow-up re-stamped a state entry"
    );
    assert!(
        rec.verdict == pre.verdict
            && rec.ready_evidence == pre.ready_evidence
            && rec.ready_binding == pre.ready_binding
            && rec.unattended_pending == pre.unattended_pending
            && rec.rounds == pre.rounds,
        "the refused follow-up ran the fix arm's restart bookkeeping anyway: {rec:?}"
    );
    // The grant is spent, not consumed — revalidation reads it as dead
    // authority.
    world.assert_invalid_run(&admitted.ticket);
}

/// Review blocker 2: the canonical-semantics collision. The bound
/// workflow's `goal` embeds a literal `\nbody=\n` (legitimate multiline
/// frontmatter text), so the admitted epic's real body is
/// `P\nbody=\nQ`. The mutation moves the body's head `P` into
/// `plan.reason` — a field that parses multiline values — leaving
/// `reason="-\nbody=\n"+P` and `body=Q`: every other field identical,
/// the file written through the tracker's own `save_front` + scoped
/// commit and still parsed by the real loader. `granted_semantics`'s
/// `plan.reason=-\nbody=\nP\nbody=\nQ\n/` encodes the mutated issue
/// byte-identically to the admitted one, so a native authorization that
/// still accepts the run is the encoder bug's proof, not a pass. The
/// stored marks and fingerprints are never touched — this is tracker
/// content changed inside a bound run, the class the semantic check
/// exists to refuse.
#[test]
fn cad225_moved_epic_body_tail_refuses_native_authorization() {
    let delimiter = "\nbody=\n";
    let world = World::open();
    // A legitimate multiline workflow goal, stored BEFORE issuance —
    // changing the template afterwards would void the grant for an
    // unrelated reason and prove the wrong guard.
    world.write_workflow(
        "---\ntitle: \"Ship {{topic}}\"\ngoal: |\n  Land {{topic}} through the granted path\n  body=\n  this tail is the epic body text\ninputs:\n  topic:\n    ask: \"What ships?\"\n---\n\nContext for {{topic}}.\n\n## Land {{topic}}\nagent: worker\nsize: S\n\nDo the landing work.\n\n### Acceptance\n- [ ] diff stays on src/ship.rs\n",
        false,
    );
    let admitted = world.grant_and_admit(None);

    let epic_issue = world.issue(&admitted.epic);
    let (original_front, original_body) =
        crate::issue::write::load_front(&epic_issue.dir).expect("epic reads");
    assert_eq!(
        original_front
            .plan
            .as_ref()
            .and_then(|p| p.reason.as_deref()),
        None,
        "the admitted epic carries no plan reason — the collision needs None"
    );
    let (head, tail) = original_body.split_once(delimiter).unwrap_or_else(|| {
        panic!("the bound render's epic body must embed `body=`: {original_body}")
    });
    let moved_reason = format!("-{delimiter}{head}");
    let moved_body = tail.to_string();

    // The mutation is the tracker's own write path: load the file, move
    // the head into the parsed `plan.reason`, save and commit it under
    // the scoped PM lock — no store row, mark or fingerprint is touched.
    let pm = crate::issue::Pm::at(&world.pm()).expect("open PM");
    let file = epic_issue.dir.join("issue.md");
    let lock = pm
        .lock_for_paths(std::slice::from_ref(&file))
        .expect("tracker lock");
    let mut moved = original_front.clone();
    moved.plan.as_mut().expect("epic plan").reason = Some(moved_reason.clone());
    crate::issue::write::save_front(&epic_issue.dir, &moved, &moved_body)
        .expect("the tracker's own writer saves the moved file");
    crate::issue::write::commit_scoped(
        &pm,
        &lock,
        std::slice::from_ref(&file),
        &format!("{}: move body head into plan reason", admitted.epic),
        &[admitted.epic.as_str()],
        "operator",
    )
    .expect("the moved file commits");
    drop(lock);

    // The attack stays genuinely parsed and valid: the real loader reads
    // the mutated file back, the moved fields land exactly, and every
    // other semantic field is byte-identical to the admitted epic.
    let (mutated_front, mutated_body) =
        crate::issue::write::load_front(&epic_issue.dir).expect("mutated epic parses");
    assert_eq!(mutated_body, moved_body, "the moved body reads back");
    let moved_plan = mutated_front.plan.clone().expect("plan still parses");
    assert_eq!(
        moved_plan.reason.as_deref(),
        Some(moved_reason.as_str()),
        "the moved reason parses as a multiline plan field"
    );
    let mut reason_restored = moved_plan.clone();
    reason_restored.reason = None;
    assert_eq!(
        reason_restored,
        original_front.plan.clone().unwrap(),
        "every other plan field is unchanged — the reason moved the body, not metadata"
    );
    for field in [
        (
            mutated_front.title.as_str(),
            original_front.title.as_str(),
            "title",
        ),
        (
            mutated_front.owner.as_deref().unwrap_or("-"),
            original_front.owner.as_deref().unwrap_or("-"),
            "owner",
        ),
        (
            mutated_front.priority.as_str(),
            original_front.priority.as_str(),
            "priority",
        ),
        (
            mutated_front.status.as_str(),
            original_front.status.as_str(),
            "status",
        ),
    ] {
        assert_eq!(
            field.0, field.1,
            "the mutation must leave {} unchanged",
            field.2
        );
    }
    assert_eq!(mutated_front.blocked_by, original_front.blocked_by);
    assert_eq!(mutated_front.parent, original_front.parent);
    assert_eq!(mutated_front.plan_epic, original_front.plan_epic);
    assert_eq!(mutated_front.item_type, original_front.item_type);

    // The native authorization the bound run consumes — the same gate
    // `dispatch_send`/`send_issue_followup`/`try_unattended_enqueue` all
    // sit behind — must refuse the changed body through the semantics
    // check, not through a fingerprint mismatch we forged (none) or an
    // unrelated guard.
    let refusal = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .err()
        .expect("a moved epic body is changed work — the bound grant must not authorize it");
    assert!(
        matches!(refusal.kind(), "rejected" | "gate"),
        "the refusal is a typed authority answer, not a fault: {refusal:?}"
    );
    assert_eq!(
        refusal.code(),
        Some("unattended_mismatch"),
        "the refusal is the semantic-mismatch guard's own coded answer, \
         not prose and not an unrelated check: {refusal:?}"
    );

    // The real consumer proves the refusal end-to-end: the checkup's
    // native follow-up admission must not queue a fix turn over an
    // authorization the moved body voided.
    world.dispatch_turn(&admitted.ticket, 1, true, Some(Some(HEAD)));
    world.save_red(&admitted.ticket);
    let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
    world
        .shared
        .checkup_idle_lane(&world.worker_agent(), &[report])
        .expect("the checkup visit answers");
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    assert!(
        world
            .shared
            .store
            .message(&mid)
            .expect("turn reads")
            .is_none(),
        "a run whose authorized body moved must not queue its follow-up as {mid}"
    );
    let rec = record(&world, &admitted.ticket);
    assert!(
        rec.unattended_hold,
        "the lane is held for the operator — the run's authority cannot be read as live"
    );
}

/// CAD-1561 compatibility on the bound automatic lane: an ordinary
/// delivery whose `delivery:` policy is operator-approved to land on a
/// non-default integration branch is legitimately delivered there —
/// `classify` binds `base_ref` to the approved target and the explicit
/// (`require_queue=false`) floor greens it. The bound workflow's
/// automatic landing keeps the old default-branch-only rule —
/// `full_ci_green` runs `require_queue` authority on the repository
/// default — so `delivery_observe`'s automatic enqueue refuses the
/// same otherwise-authorized request with no queue request and no
/// durable intent. The controls prove the refusal is the retained
/// target gate itself: the same fixtures carry the ordinary path on
/// `integration` and the full automatic floor on `main`, so a missing
/// policy, a malformed fixture or a dropped queue check cannot pass
/// here by accident — were the target gate silently removed, the
/// integration branch's branch-scoped rules/protection/workflow
/// fixtures (which the stub serves verbatim, identical to the
/// enforced default set) would carry the landing instead.
#[test]
fn cad225_approved_integration_target_cannot_consume_automatic_authority() {
    const INTEGRATION: &str = "integration";
    let world = World::open();

    // A legitimate `delivery:` policy approving `integration` as THE
    // non-default target, recorded through the real operator RPC — the
    // daemon itself reads the file and records the policy plus digest,
    // so `approved_integration_target`'s provenance check binds this
    // exact section rather than test-injected halves.
    std::fs::write(
        world.pm().join(PROJECT).join("PROJECT.md"),
        format!(
            "---\ndelivery:\n  integration_branch: {INTEGRATION}\n  reviews:\n    review:\n      kind: agent\n      focus: general\n  risk:\n    - require: [review]\n---\n# Widgets\n"
        ),
    )
    .expect("PROJECT.md writes");
    world
        .call(Asserted::Operator, |shared| {
            shared.rpc_project_work_approve(&json!({"project": PROJECT}), std::process::id())
        })
        .expect("the operator approves the delivery policy");

    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.set_base(INTEGRATION);
    world.save_pass(&admitted.ticket, "reviewer");
    let pr = format!("https://github.com/{REPO}/pull/{PR}");
    let request = json!({"issue": admitted.ticket, "pr": pr, "head": HEAD});

    // The approved-target ordinary path: `classify` admits the
    // non-default base and the explicit delivery floor
    // (`require_queue=false`) greens it — queue authority is
    // intentionally not required there.
    let classified = super::super::delivery_requirements_rpc::classify(&world.shared, &request)
        .expect("the approved integration target classifies");
    assert_eq!(
        classified["base_ref"].as_str(),
        Some(INTEGRATION),
        "classification must bind the actual target, never the default"
    );
    assert!(
        super::super::delivery_requirements_rpc::full_ci_green(
            &world.shared,
            REPO,
            PR,
            HEAD,
            BASE,
            INTEGRATION,
            false
        )
        .expect("the ordinary approved-target floor answers"),
        "ordinary delivery to the approved target must remain allowed"
    );

    // The target gate at the guard level: the SAME otherwise-green
    // request under the automatic floor (`require_queue=true`) is a
    // typed refusal, not a wait — the default-branch rule fires before
    // the integration branch's branch-scoped enforcement fixtures are
    // even consulted, so it cannot hide behind observe's earlier
    // eligibility checks.
    let refusal = super::super::delivery_requirements_rpc::full_ci_green(
        &world.shared,
        REPO,
        PR,
        HEAD,
        BASE,
        INTEGRATION,
        true,
    )
    .expect_err("automatic authority must refuse the approved non-default target");
    assert!(
        matches!(refusal.kind(), "rejected" | "gate"),
        "the target refusal is a typed authority answer, not a fault: {refusal:?}"
    );

    // The default-target control: identical fixtures pass the FULL
    // automatic floor (`require_queue=true`) on `main`. Together with
    // the `integration` ordinary green and the refused landing below,
    // the triplet isolates the default-branch rule — enforcement on
    // either branch alone cannot produce it.
    world.set_base("main");
    assert!(
        super::super::delivery_requirements_rpc::full_ci_green(
            &world.shared,
            REPO,
            PR,
            HEAD,
            BASE,
            "main",
            true
        )
        .expect("the enforced-queue floor on the default branch answers"),
        "the automatic floor still greens the default target"
    );
    world.set_base(INTEGRATION);

    // The bound automatic path answers the same otherwise-authorized
    // request: `delivery_observe` -> `try_unattended_enqueue` ->
    // `full_ci_green(require_queue)` refuses the non-default target —
    // the observe holds for the operator with no queue-entry probe, no
    // `pr merge`, no durable intent and a live grant.
    let landed = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
    world.assert_not_enqueued(&admitted.ticket, &landed);
    assert_eq!(
        world.gh_calls("graphql"),
        0,
        "the queue-entry probe ran before automatic authority proved"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        0,
        "the refused landing ran the queue mutation"
    );
}

// ---- Independent v4 race acceptance ---------------------------------------

struct CanonicalWriterWait {
    entered: mpsc::Sender<()>,
    seen: AtomicBool,
    release: AtomicBool,
}

thread_local! {
    static CANONICAL_WRITER_WAIT: std::cell::RefCell<Option<Arc<CanonicalWriterWait>>> =
        const { std::cell::RefCell::new(None) };
}

/// SQLite itself calls this only when the real connection cannot acquire
/// its writer. TLS routes the witness to this test's producer, so concurrent
/// tests cannot consume one another's barrier. No daemon guard is replaced.
fn canonical_writer_busy(_attempt: i32) -> bool {
    let wait = CANONICAL_WRITER_WAIT.with(|slot| slot.borrow().clone());
    let Some(wait) = wait else {
        return false;
    };
    if !wait.seen.swap(true, Ordering::SeqCst) {
        let _ = wait.entered.send(());
    }
    let deadline = Instant::now() + crate::store::BUSY_TIMEOUT;
    while !wait.release.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    wait.release.load(Ordering::SeqCst)
}

/// The foreign connection, SQLite contention witness and producer are one
/// cleanup unit. Even an assertion panic releases the writer/callback,
/// joins the producer and restores the normal connection busy timeout.
struct CanonicalSqlRace<T: Send + 'static> {
    shared: Arc<Shared>,
    writer: Option<rusqlite::Connection>,
    wait: Arc<CanonicalWriterWait>,
    entered: mpsc::Receiver<()>,
    producer: Option<std::thread::JoinHandle<Result<T>>>,
}

impl<T: Send + 'static> CanonicalSqlRace<T> {
    fn start(world: &World, run: impl FnOnce(Arc<Shared>) -> Result<T> + Send + 'static) -> Self {
        let writer = rusqlite::Connection::open(world.root.path().join("cadence.sqlite3"))
            .expect("foreign SQLite connection");
        writer
            .busy_timeout(Duration::from_millis(250))
            .expect("foreign timeout");
        writer
            .execute_batch("BEGIN IMMEDIATE")
            .expect("foreign writer owns the database");
        let (entered_tx, entered) = mpsc::channel();
        let wait = Arc::new(CanonicalWriterWait {
            entered: entered_tx,
            seen: AtomicBool::new(false),
            release: AtomicBool::new(false),
        });
        world
            .shared
            .store
            .try_conn()
            .expect("idle fixture store connection")
            .busy_handler(Some(canonical_writer_busy))
            .expect("install SQLite contention witness");
        let shared = world.shared.clone();
        let producer_wait = wait.clone();
        let producer = std::thread::spawn(move || {
            CANONICAL_WRITER_WAIT.with(|slot| *slot.borrow_mut() = Some(producer_wait));
            let result = run(shared);
            CANONICAL_WRITER_WAIT.with(|slot| *slot.borrow_mut() = None);
            result
        });
        Self {
            shared: world.shared.clone(),
            writer: Some(writer),
            wait,
            entered,
            producer: Some(producer),
        }
    }

    fn at_writer(&self) {
        self.entered
            .recv_timeout(Duration::from_secs(30))
            .expect("the real producer must reach SQLite writer contention");
        assert!(self.wait.seen.load(Ordering::SeqCst));
        assert!(!self.wait.release.load(Ordering::SeqCst));
        assert!(
            !self.producer.as_ref().expect("producer").is_finished(),
            "an unrelated early refusal cannot count as the writer race"
        );
    }

    fn finish(&mut self) -> Result<T> {
        let released = self
            .writer
            .take()
            .expect("held writer")
            .execute_batch("ROLLBACK");
        self.wait.release.store(true, Ordering::SeqCst);
        let result = self.producer.take().expect("producer").join();
        self.shared
            .store
            .try_conn()
            .expect("producer released its store connection")
            .busy_timeout(crate::store::BUSY_TIMEOUT)
            .expect("restore normal busy timeout");
        released.expect("release the foreign SQLite writer");
        result.expect("producer thread must not panic")
    }
}

impl<T: Send + 'static> Drop for CanonicalSqlRace<T> {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            let _ = writer.execute_batch("ROLLBACK");
        }
        self.wait.release.store(true, Ordering::SeqCst);
        if let Some(producer) = self.producer.take() {
            let _ = producer.join();
        }
        if let Ok(conn) = self.shared.store.try_conn() {
            let _ = conn.busy_timeout(crate::store::BUSY_TIMEOUT);
        }
    }
}

/// This is the supported tracker transaction: the same scoped PM lock,
/// parsed frontmatter writer and Git commit used by issue writes. A busy
/// PM lock means the action owns the snapshot; no file bypass is attempted.
/// Returning true proves the semantic edit COMMITTED while SQL was held.
fn try_canonical_title_edit(world: &World, ticket: &str, title: &str) -> bool {
    let pm = crate::issue::Pm::at(&world.pm()).expect("open tracker");
    let dir = world.pm().join(PROJECT).join(ticket);
    let file = dir.join("issue.md");
    let Some(lock) = pm
        .try_lock_for_paths(std::slice::from_ref(&file))
        .expect("probe the real tracker writer")
    else {
        return false;
    };
    let (mut front, body) = crate::issue::write::load_front(&dir).expect("read current issue");
    assert_ne!(
        front.title, title,
        "the attempted edit must change semantics"
    );
    front.title = title.to_string();
    crate::issue::write::save_front(&dir, &front, &body).expect("supported issue writer");
    crate::issue::write::commit_scoped(
        &pm,
        &lock,
        std::slice::from_ref(&file),
        &format!("{ticket}: change bound title at writer barrier"),
        &[ticket],
        "operator",
    )
    .expect("supported tracker commit");
    drop(lock);
    assert_eq!(
        world.issue(ticket).front.title,
        title,
        "committed edit reads back"
    );
    true
}

/// If the snapshot lock won, perform the supported edit only AFTER the
/// action returned. This is a valid serialization, not a demanded refusal.
fn edit_title_after_action(world: &World, ticket: &str, title: &str) {
    let pm = crate::issue::Pm::at(&world.pm()).expect("open tracker");
    crate::issue::write::set_fields_if_rev(
        &pm,
        &[ticket.to_string()],
        &[format!("title={title}")],
        "operator",
        None,
        None,
    )
    .expect("the deferred supported edit commits after the action");
    assert_eq!(world.issue(ticket).front.title, title);
}

fn bound_followup(shared: Arc<Shared>, rec: Record, mid: String) -> Result<bool> {
    let _grant = shared
        .unattended_lock
        .read()
        .unwrap_or_else(|e| e.into_inner());
    let _delivery = shared
        .delivery_lock
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    shared.send_issue_followup(&rec, &mid, "fix the red CI on this bound issue")
}

fn observe_canonical_race(shared: Arc<Shared>, ticket: String) -> Result<Value> {
    scoped(Asserted::Operator, || {
        shared.rpc_delivery_observe(
            &json!({
                "issue": ticket, "head": HEAD, "pr_state": "OPEN",
                "ci_green": true, "additions": 1, "deletions": 0, "files": 1,
            }),
            std::process::id(),
        )
    })
}

fn assert_canonical_landing_requested(world: &World, ticket: &str, outcome: &Result<Value>) {
    let body = outcome.as_ref().expect("valid automatic landing answers");
    assert!(body["unattended_refusal"].is_null(), "{body}");
    assert!(
        world
            .shared
            .store
            .unattended_merge_intent(ticket, HEAD)
            .unwrap()
            .is_some(),
        "a valid default-target action must record its native intent"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "exactly one remote enqueue request"
    );
    assert_eq!(
        world.recorded_merge_approvals(),
        0,
        "no fabricated operator approval"
    );
    assert!(!record(world, ticket).unattended_hold);
}

fn assert_canonical_landing_refused(world: &World, ticket: &str, outcome: &Result<Value>) {
    match outcome {
        Err(error) => assert!(
            matches!(error.kind(), "rejected" | "gate" | "conflict"),
            "contention/transport faults are not authority refusal: {error:?}"
        ),
        Ok(body) => assert!(
            body["unattended_refusal"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "the live consumer must report an authority refusal: {body}"
        ),
    }
    let rec = record(world, ticket);
    assert!(
        rec.unattended_hold,
        "refused landing must be held for the operator"
    );
    assert_ne!(rec.state, State::Enqueued);
    assert_ne!(rec.state, State::Merged);
    assert!(
        world
            .shared
            .store
            .unattended_merge_intent(ticket, HEAD)
            .unwrap()
            .is_none(),
        "a refused action must not leave a fresh native intent"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        0,
        "no unauthorized remote mutation"
    );
    assert_eq!(world.recorded_merge_approvals(), 0, "no operator approval");
}

fn canonical_default_landing_control() {
    let world = World::open();
    let admitted = world.grant_and_admit(Some(120));
    world.save_pass(&admitted.ticket, "reviewer");
    let outcome = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
    assert_canonical_landing_requested(&world, &admitted.ticket, &outcome);
}

#[test]
fn cad225_followup_canonical_snapshot_survives_sql_writer_wait() {
    let world = World::open();
    let admitted = world.grant_and_admit(Some(120));
    world.dispatch_turn(&admitted.ticket, 1, true, Some(Some(HEAD)));
    world.save_red(&admitted.ticket);

    // Same run, producer and lane, without mutation: admission really works.
    let control = "canonical-followup-control";
    assert!(bound_followup(
        world.shared.clone(),
        record(&world, &admitted.ticket),
        control.into()
    )
    .expect("valid bound follow-up control"));
    let message = world
        .shared
        .store
        .message(control)
        .unwrap()
        .expect("control row");
    assert_eq!(message.source, "review");
    assert_eq!(message.issue.as_deref(), Some(admitted.ticket.as_str()));
    assert_eq!(message.worktree.as_deref(), world.checkout().to_str());
    finish_turn(&world, control, json!({"sha": HEAD}));

    let expiry = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .expect("live authority")
        .expect("bound run")
        .expires_at;
    assert!(expiry > now_secs() + 60.0, "this is not an expiry test");
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    let rec = record(&world, &admitted.ticket);
    let send_mid = mid.clone();
    let mut race =
        CanonicalSqlRace::start(&world, move |shared| bound_followup(shared, rec, send_mid));
    race.at_writer();
    let title = "Changed work while follow-up writer is parked";
    let edited_first = try_canonical_title_edit(&world, &admitted.ticket, title);
    assert!(
        expiry > now_secs() + 30.0,
        "authority must remain comfortably live"
    );
    let outcome = race.finish();
    if edited_first {
        let error = outcome.expect_err("changed canonical work must refuse a fresh follow-up");
        assert!(
            matches!(error.kind(), "rejected" | "gate" | "conflict"),
            "an unrelated busy/transport fault cannot satisfy the guard: {error:?}"
        );
        assert!(
            world.shared.store.message(&mid).unwrap().is_none(),
            "no stale follow-up row"
        );
        let before = record(&world, &admitted.ticket);
        let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
        world
            .shared
            .checkup_idle_lane(&world.worker_agent(), &[report])
            .expect("operator-facing checkup answers");
        let after = record(&world, &admitted.ticket);
        assert!(after.unattended_hold, "the invalid run must be held");
        assert_eq!(after.state, before.state);
        assert_eq!(after.since, before.since);
        assert_eq!(after.verdict, before.verdict);
        assert_eq!(after.ready_evidence, before.ready_evidence);
        assert_eq!(after.ready_binding, before.ready_binding);
        assert_eq!(after.unattended_pending, before.unattended_pending);
        assert!(world.shared.store.message(&mid).unwrap().is_none());
    } else {
        assert!(outcome.expect("snapshot owner may authorize before the edit"));
        let message = world
            .shared
            .store
            .message(&mid)
            .unwrap()
            .expect("authorized follow-up");
        assert_eq!(message.source, "review");
        assert_eq!(message.issue.as_deref(), Some(admitted.ticket.as_str()));
        assert_eq!(message.worktree.as_deref(), world.checkout().to_str());
        edit_title_after_action(&world, &admitted.ticket, title);
    }
    world.assert_invalid_run(&admitted.ticket);
}

#[test]
fn cad225_landing_canonical_snapshot_survives_sql_writer_wait() {
    canonical_default_landing_control();
    let world = World::open();
    let admitted = world.grant_and_admit(Some(120));
    world.save_pass(&admitted.ticket, "reviewer");
    let expiry = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .expect("live authority")
        .expect("bound run")
        .expires_at;
    assert!(expiry > now_secs() + 60.0, "this is not an expiry test");
    let ticket = admitted.ticket.clone();
    let mut race =
        CanonicalSqlRace::start(&world, move |shared| observe_canonical_race(shared, ticket));
    race.at_writer();
    // The valid control and actual SQLite callback exclude an earlier
    // profile/CI/refusal branch from masquerading as the intent boundary.
    assert!(world.gh_calls("workflows/ci.yml/runs") > 0);
    assert!(world.gh_calls("graphql") > 0);
    let title = "Changed work while landing intent writer is parked";
    let edited_first = try_canonical_title_edit(&world, &admitted.ticket, title);
    assert!(
        expiry > now_secs() + 30.0,
        "authority must remain comfortably live"
    );
    let outcome = race.finish();
    if edited_first {
        assert_canonical_landing_refused(&world, &admitted.ticket, &outcome);
    } else {
        assert_canonical_landing_requested(&world, &admitted.ticket, &outcome);
        edit_title_after_action(&world, &admitted.ticket, title);
    }
    world.assert_invalid_run(&admitted.ticket);
}

/// One-shot final queue-observation rendezvous; release/join on panic too.
struct CanonicalQueueRead {
    root: PathBuf,
    producer: Option<std::thread::JoinHandle<Result<Value>>>,
}

impl CanonicalQueueRead {
    fn start(world: &World, ticket: String) -> Self {
        std::fs::write(world.root.path().join("qr-arm"), "1\n").expect("arm queue read");
        let shared = world.shared.clone();
        Self {
            root: world.root.path().to_path_buf(),
            producer: Some(std::thread::spawn(move || {
                observe_canonical_race(shared, ticket)
            })),
        }
    }

    fn finish(&mut self) -> Result<Value> {
        std::fs::write(self.root.join("qr-go"), "1\n").expect("release queue read");
        self.producer
            .take()
            .expect("producer")
            .join()
            .expect("observe thread")
    }
}

impl Drop for CanonicalQueueRead {
    fn drop(&mut self) {
        let _ = std::fs::write(self.root.join("qr-go"), "1\n");
        if let Some(producer) = self.producer.take() {
            let _ = producer.join();
        }
    }
}

fn canonical_ordinary_integration_control() {
    let world = World::open();
    std::fs::write(
        world.pm().join(PROJECT).join("PROJECT.md"),
        "---\ndelivery:\n  integration_branch: integration\n  reviews:\n    review:\n      kind: agent\n      focus: general\n  risk:\n    - require: [review]\n---\n# Widgets\n",
    )
    .expect("ordinary integration policy");
    world
        .call(Asserted::Operator, |shared| {
            shared.rpc_project_work_approve(&json!({"project": PROJECT}), std::process::id())
        })
        .expect("native operator policy approval");
    let admitted = world.grant_and_admit(Some(120));
    world.set_base("integration");
    let classified = super::super::delivery_requirements_rpc::classify(
        &world.shared,
        &json!({
            "issue": admitted.ticket,
            "pr": format!("https://github.com/{REPO}/pull/{PR}"),
            "head": HEAD,
        }),
    )
    .expect("ordinary approved integration target classifies");
    assert_eq!(classified["base_ref"].as_str(), Some("integration"));
    assert!(
        super::super::delivery_requirements_rpc::full_ci_green(
            &world.shared,
            REPO,
            PR,
            HEAD,
            BASE,
            "integration",
            false,
        )
        .expect("ordinary approved-target CI answers"),
        "ordinary approved integration delivery remains allowed"
    );
}

// The policy edit uses the same PM writer lock and scoped Git transaction
// as supported tracker writes; it never bypasses an action-owned snapshot.
const CANONICAL_OPERATOR_POLICY: &str = "---\ndelivery:\n  reviews:\n    review:\n      kind: agent\n      focus: general\n    human:\n      kind: operator\n  risk:\n    - require: [review, human]\n---\n# Widgets\n";

fn try_operator_policy_edit(world: &World) -> bool {
    let pm = crate::issue::Pm::at(&world.pm()).expect("open tracker");
    let file = world.pm().join(PROJECT).join("PROJECT.md");
    let Some(lock) = pm
        .try_lock_for_paths(std::slice::from_ref(&file))
        .expect("supported policy writer lock")
    else {
        return false;
    };
    let parsed = crate::issue::delivery_policy::parse(CANONICAL_OPERATOR_POLICY)
        .expect("the injected policy is valid")
        .expect("delivery section");
    parsed.validate().expect("valid required operator review");
    std::fs::write(&file, CANONICAL_OPERATOR_POLICY).expect("save policy under PM lock");
    crate::issue::write::commit_scoped(
        &pm,
        &lock,
        std::slice::from_ref(&file),
        "Require operator review during the landing observation",
        &[],
        "operator",
    )
    .expect("scoped policy commit");
    drop(lock);
    assert_eq!(
        std::fs::read_to_string(file).expect("committed policy reads"),
        CANONICAL_OPERATOR_POLICY
    );
    true
}

// This guard is inside the scoped-thread body: on panic it releases the
// remote read BEFORE thread::scope joins an approval waiting on the action.
struct PolicyObservationRelease(PathBuf);

impl Drop for PolicyObservationRelease {
    fn drop(&mut self) {
        let _ = std::fs::write(self.0.join("qr-go"), "1\n");
    }
}

#[test]
fn cad225_landing_revalidates_policy_after_final_observation() {
    canonical_default_landing_control();
    let expected_policy = crate::issue::delivery_policy::parse(CANONICAL_OPERATOR_POLICY)
        .expect("valid policy")
        .expect("delivery section");
    let expected_digest = crate::issue::delivery_policy::digest(&expected_policy);
    let world = World::open();
    let admitted = world.grant_and_admit(Some(120));
    world.save_pass(&admitted.ticket, "reviewer");
    let mut read = CanonicalQueueRead::start(&world, admitted.ticket.clone());
    assert!(
        world.wait_for("qr-call", 30),
        "valid default classification must reach the final queue read"
    );
    assert!(world.gh_calls("workflows/ci.yml/runs") > 0);
    assert!(world.gh_calls(&format!("compare/{BASE}...{HEAD}")) >= 2);
    assert_eq!(world.gh_calls("pr merge"), 0);

    let edited_first = try_operator_policy_edit(&world);
    if edited_first {
        let (entered_tx, entered) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let shared = world.shared.clone();
        let outcome = std::thread::scope(|scope| {
            let _release = PolicyObservationRelease(world.root.path().to_path_buf());
            let approve = scope.spawn(move || {
                let _ = entered_tx.send(());
                let result = scoped(Asserted::Operator, || {
                    shared
                        .rpc_project_work_approve(&json!({"project": PROJECT}), std::process::id())
                });
                let _ = done_tx.send(());
                result
            });
            entered
                .recv_timeout(Duration::from_secs(5))
                .expect("native approval attempt starts during the observation");
            // Give a nonserialized approval an opportunity to complete.
            // Timeout is NOT an assertion or authority signal: a correct
            // writer may serialize approval behind the still-running action.
            let _ = done.recv_timeout(Duration::from_millis(250));
            let outcome = read.finish();
            let approval = approve
                .join()
                .expect("approval thread joins")
                .expect("the native operator approves the valid policy");
            assert_eq!(approval["by"].as_str(), Some("operator"));
            assert_eq!(
                approval["delivery_digest"].as_str(),
                Some(expected_digest.as_str())
            );
            outcome
        });
        assert_canonical_landing_refused(&world, &admitted.ticket, &outcome);
    } else {
        // The action already owns the policy snapshot. It may authorize
        // first; only after it returns may the supported edit commit.
        let outcome = read.finish();
        assert_canonical_landing_requested(&world, &admitted.ticket, &outcome);
        assert!(try_operator_policy_edit(&world));
        world
            .call(Asserted::Operator, |shared| {
                shared.rpc_project_work_approve(&json!({"project": PROJECT}), std::process::id())
            })
            .expect("native policy approval after the authorized action");
    }

    let approvals = world
        .shared
        .store
        .work_approvals()
        .expect("native approvals read");
    let approval = approvals.get(PROJECT).expect("native project approval");
    assert_eq!(approval["by"].as_str(), Some("operator"));
    assert_eq!(
        approval["delivery_digest"].as_str(),
        Some(expected_digest.as_str())
    );
    let classified = super::super::delivery_requirements_rpc::classify(
        &world.shared,
        &json!({
            "issue": admitted.ticket,
            "pr": format!("https://github.com/{REPO}/pull/{PR}"),
            "head": HEAD,
        }),
    )
    .expect("same valid head/base/branch with the new policy classifies");
    assert_eq!(classified["head"].as_str(), Some(HEAD));
    assert_eq!(classified["base"].as_str(), Some(BASE));
    assert_eq!(classified["base_ref"].as_str(), Some("main"));
    assert_ne!(
        classified["requirements"]["activation"].as_str(),
        Some("active")
    );
    assert!(world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .expect("the workflow and issue semantics were unchanged")
        .is_some());
    assert_eq!(world.recorded_merge_approvals(), 0);
}

#[test]
fn cad225_fresh_admission_expiry_binds_the_intent_writer() {
    // A real live admission is the control, not just a parsable grant.
    {
        let control = World::open();
        let admitted = control.grant_and_admit(Some(120));
        assert_eq!(control.issue(&admitted.ticket).front.status, "ready");
    }
    let world = World::open();
    let granted = world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), Some(8), None),
        )
        .expect("native short-lived grant");
    world.require_native_grant(&granted);
    let expiry = world.workflow_approval()["unattended"]["expires_at"]
        .as_f64()
        .expect("native server expiry");
    let issues_before = world.issues();
    let commits_before = world.commits();
    let until_arm = expiry - now_secs() - 2.0;
    if until_arm > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(until_arm));
    }
    let mut race = CanonicalSqlRace::start(&world, move |shared| {
        scoped(Asserted::Operator, || {
            shared.rpc_plan_propose(
                &json!({
                    "project": PROJECT,
                    "workflow": WORKFLOW,
                    "inputs": bound_inputs(),
                }),
                std::process::id(),
            )
        })
    });
    race.at_writer();
    assert!(
        now_secs() < expiry,
        "intent reached its real writer while live"
    );
    let until_expired = expiry - now_secs() + 0.15;
    if until_expired > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(until_expired));
    }
    assert!(now_secs() >= expiry);
    assert!(!race.wait.release.load(Ordering::SeqCst));
    assert!(!race.producer.as_ref().expect("producer").is_finished());
    let outcome = race.finish();
    let error = outcome.expect_err("a fresh intent must not admit after its grant expires");
    assert!(
        matches!(error.kind(), "rejected" | "gate" | "conflict"),
        "SQLite busy/transport/setup errors do not prove refusal: {error:?}"
    );
    assert_eq!(
        world.issues(),
        issues_before,
        "no logical epic or ticket files"
    );
    assert_eq!(
        world.commits(),
        commits_before,
        "no admission tracker commit"
    );
    assert!(
        world.audit_events().iter().all(|(kind, payload)| {
            kind != crate::store::UNATTENDED_RUN_EVENT || payload["status"] != "admitted"
        }),
        "no admitted run receipt may be recorded after the expired intent wait"
    );
}

#[test]
fn cad225_final_queue_observation_binds_target_and_base() {
    canonical_default_landing_control();
    canonical_ordinary_integration_control();
    for missing in [None, Some("name"), Some("oid")] {
        let world = World::open();
        let admitted = world.grant_and_admit(Some(120));
        world.save_pass(&admitted.ticket, "reviewer");
        let mut read = CanonicalQueueRead::start(&world, admitted.ticket.clone());
        assert!(
            world.wait_for("qr-call", 30),
            "otherwise-valid default landing must reach its final queue observation"
        );
        assert!(
            world.gh_calls("workflows/ci.yml/runs") > 0,
            "canonical CI was consulted"
        );
        assert!(
            world.gh_calls(&format!("compare/{BASE}...{HEAD}")) >= 2,
            "both authorized classifications completed before the injection"
        );
        assert_eq!(
            world.gh_calls("pr merge"),
            0,
            "barrier precedes remote mutation"
        );
        if let Some(field) = missing {
            std::fs::write(world.root.path().join("qr-missing"), field)
                .expect("omit target evidence from the final queue response");
        } else {
            // Same head AND base OID: only destination identity changes.
            world.set_base("integration");
        }
        let outcome = read.finish();
        let observed = std::fs::read_to_string(world.root.path().join("qr-observed"))
            .expect("the fixture recorded its actual final remote observation");
        let target = if missing.is_some() {
            "main"
        } else {
            "integration"
        };
        assert_eq!(observed.trim(), format!("{HEAD}|{target}|{BASE}"));
        assert_canonical_landing_refused(&world, &admitted.ticket, &outcome);
        assert!(
            world
                .shared
                .unattended_authorization_for_issue(&admitted.ticket)
                .expect("tracker/grant were unchanged")
                .is_some(),
            "this must be a target-evidence refusal, not lost workflow authority"
        );
    }
}

// ---- Independent postmerge settlement acceptance -------------------------

/// Produce the authority through native grant/admission, owned completion,
/// router intake, assigned-reviewer verdict and observer queue reconciliation.
/// No settlement record, grant fingerprint or intent is planted here.
fn settlement_ready(world: &World) -> Admitted {
    let admitted = world.grant_and_admit(Some(120));
    world.dispatch_turn(&admitted.ticket, 1, true, Some(Some(HEAD)));
    world
        .shared
        .delivery_start(&admitted.ticket, PROJECT, "worker")
        .expect("native delivery starts");
    world.done_report(&admitted.ticket, now_secs() as i64 + 60, HEAD);
    world
        .shared
        .route_delivery()
        .expect("owned done report routes");
    assert_eq!(record(world, &admitted.ticket).state, State::Reviewing);
    let shared = world.shared.clone();
    scoped(Asserted::Agent("reviewer".into()), || {
        shared.rpc_report_verdict(
            &json!({
                "issue": admitted.ticket,
                "text": verdict_text(&admitted.ticket, "pass", HEAD, Some("auto")),
            }),
            std::process::id(),
        )
    })
    .expect("assigned reviewer records the native exact-head PASS");
    assert!(world
        .shared
        .store
        .unattended_review_recorded(&record(world, &admitted.ticket))
        .expect("native review receipt reads"));
    world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("native observer requests the protected queue");
    world.queue_admitted();
    world
        .observe_auto(Asserted::Operator, &admitted.ticket, HEAD, "OPEN")
        .expect("native observer records actual queue admission");
    assert_eq!(record(world, &admitted.ticket).state, State::Enqueued);
    assert!(world
        .shared
        .store
        .unattended_merge_intent(&admitted.ticket, HEAD)
        .unwrap()
        .is_some());
    assert!(world
        .shared
        .store
        .unattended_merge_enqueued(&admitted.ticket, HEAD)
        .unwrap()
        .is_some());
    assert_eq!(world.gh_calls("pr merge"), 1);
    assert_eq!(world.recorded_merge_approvals(), 0);
    admitted
}

/// A real Git commit failure after the writer has captured status creates
/// Pending. Restore the hook even if the observation or assertion panics.
struct SettlementCommitFailure(PathBuf);

impl SettlementCommitFailure {
    fn install(world: &World) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let hook = world.pm().join(".git/hooks/pre-commit");
        assert!(!hook.exists(), "only own a new fixture hook");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").expect("write failing hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("make hook executable");
        Self(hook)
    }
}

impl Drop for SettlementCommitFailure {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn settlement_pending(world: &World, ticket: &str) {
    let before = world.issue(ticket).front.status;
    let failure = SettlementCommitFailure::install(world);
    world
        .observe_state(Asserted::Operator, ticket, HEAD, "MERGED")
        .expect("merge observation answers despite failed tracker commit");
    drop(failure);
    let rec = record(world, ticket);
    assert_eq!(rec.state, State::Merged);
    assert!(
        matches!(
            &rec.ticket_done,
            Some(delivery::TicketDone::Pending {
                from: Some(from),
                attempts: 1,
                ..
            }) if from == &before
        ),
        "a real failed commit must create the first pending write: {:?}",
        rec.ticket_done
    );
    assert_eq!(world.issue(ticket).front.status, before);
}

/// Existing scripted PR view rendezvous: in settlement the next view is
/// after the early canonical check. A missing marker is an early-path failure,
/// never an accepted refusal. Release and join on every exit, including panic.
struct SettlementRead {
    root: PathBuf,
    finished: mpsc::Receiver<()>,
    producer: Option<std::thread::JoinHandle<Result<()>>>,
}

impl SettlementRead {
    fn start(world: &World, ticket: String, retry: bool) -> Self {
        world.arm_rendezvous();
        let root = world.root.path().to_path_buf();
        let shared = world.shared.clone();
        let (finished_tx, finished) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            let result = if retry {
                shared.settle_ticket_done().map(|_| ())
            } else {
                scoped(Asserted::Operator, || {
                    shared.rpc_delivery_observe(
                        &json!({
                            "issue": ticket, "head": HEAD, "pr_state": "MERGED",
                            "ci_green": true, "auto_merge": true,
                            "additions": 1, "deletions": 0, "files": 1,
                        }),
                        std::process::id(),
                    )
                })
                .map(|_| ())
            };
            let _ = finished_tx.send(());
            result
        });
        Self {
            root,
            finished,
            producer: Some(producer),
        }
    }

    fn at_read(&self, world: &World) {
        assert!(
            world.wait_for("rv-call", 30),
            "otherwise-valid settlement must reach its blocking remote read"
        );
        assert!(
            !self.producer.as_ref().expect("producer").is_finished(),
            "an unrelated early refusal cannot satisfy the race"
        );
        assert!(!world.root.path().join("rv-go").exists());
    }

    fn finish(&mut self) {
        self.finish_releasing(|| {});
    }

    fn finish_releasing(&mut self, release: impl FnOnce()) {
        std::fs::write(self.root.join("rv-go"), "1\n").expect("release remote read");
        let finished = self.finished.recv_timeout(Duration::from_secs(5));
        // Release a test-owned tracker writer before any timeout panic
        // joins the producer, so a broken blocking implementation cleans up.
        release();
        finished.expect("settlement must finish without waiting for a tracker writer");
        self.producer
            .take()
            .expect("producer")
            .join()
            .expect("settlement thread")
            .expect("settlement path answers; refusal is its recorded outcome");
    }
}

impl Drop for SettlementRead {
    fn drop(&mut self) {
        let _ = std::fs::write(self.root.join("rv-go"), "1\n");
        if let Some(producer) = self.producer.take() {
            let _ = producer.join();
        }
    }
}

fn assert_settlement_marked(world: &World, ticket: &str) {
    let rec = record(world, ticket);
    assert_eq!(rec.state, State::Merged);
    assert!(
        matches!(rec.ticket_done, Some(delivery::TicketDone::Marked { .. })),
        "valid settlement must actually mark done: {:?}",
        rec.ticket_done
    );
    assert_eq!(world.issue(ticket).front.status, "done");
    assert_eq!(world.gh_calls("pr merge"), 1, "no merge replay");
    assert_eq!(world.recorded_merge_approvals(), 0);
}

fn assert_settlement_refused(world: &World, ticket: &str, status: &str) {
    let rec = record(world, ticket);
    assert_eq!(
        rec.state,
        State::Merged,
        "remote merge remains an observation"
    );
    assert!(
        matches!(
            &rec.ticket_done,
            Some(delivery::TicketDone::Refused { why }) if !why.is_empty()
        ),
        "changed/expired authority must refuse, not claim success or busy: {:?}",
        rec.ticket_done
    );
    assert_eq!(world.issue(ticket).front.status, status);
    assert_ne!(status, "done");
    assert!(world
        .shared
        .store
        .events_tail(crate::store::Store::DAEMON_STREAM, 100)
        .expect("settlement notices read")
        .iter()
        .all(|event| { event.kind != "ticket_done_on_merge" || event.payload["issue"] != ticket }));
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "settlement cannot replay merge"
    );
    assert_eq!(world.recorded_merge_approvals(), 0);
}

#[test]
fn cad225_settlement_remote_read_valid_initial_and_retry_controls() {
    for retry in [false, true] {
        let world = World::open();
        let admitted = settlement_ready(&world);
        if retry {
            settlement_pending(&world, &admitted.ticket);
        }
        let mut read = SettlementRead::start(&world, admitted.ticket.clone(), retry);
        read.at_read(&world);
        read.finish();
        assert_settlement_marked(&world, &admitted.ticket);
    }
}

fn settlement_semantic_race(retry: bool) {
    let world = World::open();
    let admitted = settlement_ready(&world);
    if retry {
        settlement_pending(&world, &admitted.ticket);
    }
    let before = world.issue(&admitted.ticket).front.status;
    let expiry = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .unwrap()
        .expect("live bound authority before collection")
        .expires_at;
    let mut read = SettlementRead::start(&world, admitted.ticket.clone(), retry);
    read.at_read(&world);
    assert!(expiry > now_secs() + 30.0, "this is not an expiry case");
    let title = "Changed canonical work during postmerge remote collection";
    let edited_first = try_canonical_title_edit(&world, &admitted.ticket, title);
    assert!(
        expiry > now_secs() + 20.0,
        "grant stays live through the edit"
    );
    read.finish();
    if edited_first {
        assert_settlement_refused(&world, &admitted.ticket, &before);
    } else {
        // A snapshot lock may serialize the action before the edit.
        // Never bypass it or demand refusal of unchanged work.
        assert_settlement_marked(&world, &admitted.ticket);
        edit_title_after_action(&world, &admitted.ticket, title);
    }
    assert_eq!(world.issue(&admitted.ticket).front.title, title);
    world.assert_invalid_run(&admitted.ticket);
}

#[test]
fn cad225_settlement_remote_read_semantic_edit_initial() {
    settlement_semantic_race(false);
}

#[test]
fn cad225_settlement_remote_read_semantic_edit_retry() {
    settlement_semantic_race(true);
}

fn settlement_expiry_race(retry: bool) {
    let world = World::open();
    let admitted = settlement_ready(&world);
    if retry {
        settlement_pending(&world, &admitted.ticket);
    }
    world
        .approve(
            Asserted::Operator,
            json!({
                "project": PROJECT, "name": WORKFLOW,
                "renew": admitted.grant_id, "ttl_secs": 8,
            }),
        )
        .expect("native renewal sets a short real deadline");
    let expiry = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .unwrap()
        .expect("renewed authority is live before remote collection")
        .expires_at;
    let before = world.issue(&admitted.ticket).front.status;
    let mut read = SettlementRead::start(&world, admitted.ticket.clone(), retry);
    read.at_read(&world);
    assert!(expiry > now_secs(), "remote collection began before expiry");
    let remaining = expiry - now_secs() + 1.0;
    assert!((0.0..=10.0).contains(&remaining));
    std::thread::sleep(Duration::from_secs_f64(remaining));
    assert!(now_secs() > expiry, "the real server deadline elapsed");
    read.finish();
    assert_settlement_refused(&world, &admitted.ticket, &before);
    world.assert_invalid_run(&admitted.ticket);
}

#[test]
fn cad225_settlement_remote_read_expiry_initial() {
    settlement_expiry_race(false);
}

#[test]
fn cad225_settlement_remote_read_expiry_retry() {
    settlement_expiry_race(true);
}

#[test]
fn cad225_settlement_retry_busy_is_nonwaiting_and_preserves_status_override() {
    let world = World::open();
    let admitted = settlement_ready(&world);
    settlement_pending(&world, &admitted.ticket);
    let pm = crate::issue::Pm::at(&world.pm()).expect("open tracker");
    let writer = pm
        .try_lock_for_paths(&[world.pm().join(PROJECT).join(&admitted.ticket)])
        .expect("probe supported tracker writer")
        .expect("own the tracker before retry begins");
    let mut read = SettlementRead::start(&world, admitted.ticket.clone(), true);
    // This control permits an early busy outcome: only the attack cases
    // require the remote barrier. A guarded implementation may refuse busy
    // before collecting evidence, while another validates after collection.
    // finish must return while this genuine writer remains held. A wait or
    // successful done write cannot pass as the legitimate pending retry.
    read.finish_releasing(|| drop(writer));
    let rec = record(&world, &admitted.ticket);
    assert!(
        matches!(rec.ticket_done, Some(delivery::TicketDone::Pending { .. })),
        "busy retry must retain its captured status: {:?}",
        rec.ticket_done
    );
    assert_ne!(world.issue(&admitted.ticket).front.status, "done");
    crate::issue::write::set_fields_if_rev(
        &pm,
        std::slice::from_ref(&admitted.ticket),
        &["status=backlog".to_string()],
        "operator",
        None,
        None,
    )
    .expect("supported operator status override");
    world
        .shared
        .settle_ticket_done()
        .expect("retry respects its original status CAS");
    assert!(
        matches!(
            &record(&world, &admitted.ticket).ticket_done,
            Some(delivery::TicketDone::Kept { status }) if status == "backlog"
        ),
        "an operator status override must be kept"
    );
    assert_eq!(world.issue(&admitted.ticket).front.status, "backlog");
    assert_eq!(world.gh_calls("pr merge"), 1);
    assert_eq!(world.recorded_merge_approvals(), 0);
}
