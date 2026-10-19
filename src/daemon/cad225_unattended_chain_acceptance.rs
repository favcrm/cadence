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
//! (what `project.yaml` recorded and what `unattended_dispatch_repo`
//! matches the grant against) while `fetch`/`ls-remote` reach the
//! on-disk seed. `gh` never runs on this path — `unattended_dispatch_repo`
//! only matches the registered project repo — and `dispatch::run`'s
//! launcher skips `dispatch_send` when the issue already holds an open
//! worktree ref.

use std::sync::atomic::{AtomicBool, Ordering};
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
/// `unattended_dispatch_repo`'s bound-repo selection touches `origin`,
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
