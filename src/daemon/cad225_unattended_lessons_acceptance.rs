//! Independent CAD-225 acceptance: scoped lesson evidence review and reuse.
//!
//! Written from the ticket outcome — "a scoped lesson receives two
//! independent evidence reviews over unchanged content, becomes eligible
//! through the supported authenticated path, and is reused by another
//! worker on a relevant subsequent task; invalid or unreviewed guidance
//! is withheld" — against the real `memory_propose`/`memory_review`/
//! `memory_finalize` handlers, not a mock of them. Every call derives
//! its `NativeIdentity` through `memory_actor` from a registered agent
//! row stamped with the endpoint identity an adapter `open` would
//! record (`store.set_identity`); no test code constructs a proof.
//!
//! | case | real guard |
//! |---|---|
//! | unproven, operator, unregistered, `reviewer`-role, or endpoint-less caller | `memory_actor` + `valid_identity_proof` in the write |
//! | request params carrying identity fields or finalize edits | `reject_memory_identity_claims`, `rpc_memory_finalize` body/edit refusal |
//! | author reviews its own proposal; same identity reviews twice; a re-registered alias reviews again | `submit_review` author and per-cycle dedup on stable id AND alias |
//! | review or finalize over a stale digest, a wrong operation, or no active cycle | the `digest` pin and `operation_cycle` in `submit_review`/`finalize_native` |
//! | two pass receipts but no PM finalize; PM finalize under quorum | `retrieval_status` (status/finalization/cycle) and `finalize_native` |
//! | two distinct non-author proofs + PM `accept` on the exact digest | retrieval eligible — injected into a subsequent task's lessons file and the briefing's `project_rules` |
//! | an accepted lesson whose scope does not cover the task | `applies` scope union in `match_memories` |
//! | `status: accepted` forged into a reviewed-never file | `finalization_for` finds no matching PM acceptance finalization |
//! | body mutated after acceptance | `semantic_digest` pin voids retrieval; even a valid verify cycle on the new digest cannot restore it |
//!
//! Fixture boundaries (fixture vs genuine process ancestry): the seam
//! `Asserted::Agent` resolves the registered row — production would
//! additionally require the peer's `/proc` ancestry to reach that
//! agent's live pane or enrolled endpoint (`caller_identity`,
//! identity.rs:145). All endpoint rows here record this test process's
//! pid: distinctness comes from alias+registration (the `stable_id`),
//! never from pids. `dispatch::run`'s Unix-socket send is outside the
//! fixture (same boundary the parent module documents); what these
//! cases prove is dispatch PAYLOAD GENERATION — `memory::match_for_issue`
//! on the loaded ticket, `render_lessons`, and the
//! `<state>/dispatch/<mid>-lessons.md` artifact — plus the briefing's
//! `project_rules` read. They do NOT prove a worker received the
//! kickoff, read the file, or applied the lesson: the consumer-side
//! half of "reused by another worker" still needs the native
//! socket/provider smoke, so the whole-chain reuse outcome remains
//! open here. One honest residual: tracker files are unauthenticated,
//! so `retrieval_status` blocks a forged `status:` or a digest drift
//! but cannot detect a fully consistent forged receipt chain — only a
//! PM-dir writer could plant one.

use super::*;

/// `agent:<alias>` — the asserted caller for every memory RPC.
fn agent(alias: &str) -> Asserted {
    Asserted::Agent(alias.to_string())
}

/// A valid memory body: fact lines, then `**Why:**`/`**How to apply:**`.
fn lesson_body(fact: &str) -> String {
    format!(
        "{fact}\n\n**Why:** proven on the bound unattended path\n\n\
         **How to apply:** derive identity from the connection, never the request\n"
    )
}

/// `rpc_memory_propose` params: explicit slug, kind and scope.
fn propose_params(slug: &str, kind: &str, scope: Value, fact: &str) -> Value {
    json!({
        "project": PROJECT,
        "kind": kind,
        "scope": scope,
        "source": "CAD-225 acceptance fixture",
        "confidence": "high",
        "id": slug,
        "text": lesson_body(fact),
    })
}

/// `rpc_memory_review` params: an evidence pass on the exact digest.
fn review_params(slug: &str, operation: &str, verdict: &str, digest: &str) -> Value {
    json!({
        "project": PROJECT,
        "slug": slug,
        "operation": operation,
        "verdict": verdict,
        "evidence": format!("read the file and re-derived the claim on {digest}"),
        "digest": digest,
    })
}

/// `rpc_memory_finalize` params.
fn finalize_params(slug: &str, operation: &str, digest: &str) -> Value {
    json!({
        "project": PROJECT,
        "slug": slug,
        "operation": operation,
        "digest": digest,
    })
}

/// The memory as the tracker stores it now.
fn stored(world: &World, slug: &str) -> crate::memory::Memory {
    crate::memory::load_project(&world.pm(), PROJECT)
        .expect("memory load")
        .into_iter()
        .find(|m| m.front.id == slug)
        .unwrap_or_else(|| panic!("memory {slug} missing"))
}

/// `retrieval_status` — the eligibility decision every reader shares.
fn retrieval(world: &World, slug: &str) -> (bool, String) {
    crate::memory::retrieval_status(&stored(world, slug))
}

/// The semantic digest of the stored record.
fn digest_of(world: &World, slug: &str) -> String {
    crate::memory::semantic_digest(&stored(world, slug))
}

/// Slugs a match result would inject.
fn slugs(matched: &crate::memory::Matched) -> BTreeSet<String> {
    matched.lessons.iter().map(|m| m.front.id.clone()).collect()
}

/// The payload `issue/dispatch.rs:801-863` builds for a worker's kickoff
/// before the socket send: `match_for_issue` on the loaded ticket with
/// the worker row's provider, `render_lessons`, and the
/// `<state>/dispatch/<mid>-lessons.md` artifact (tmp+rename, exactly as
/// dispatch writes it). Returns the match and the file text.
fn dispatch_lessons(
    world: &World,
    ticket: &crate::issue::board::Issue,
    provider: &str,
    mid: &str,
) -> (crate::memory::Matched, String) {
    let pm = crate::issue::Pm::at(&world.pm()).expect("fixture pm");
    let (matched, errors) = crate::memory::match_for_issue(&pm, ticket, Some(provider))
        .expect("match must read the tracker");
    assert!(
        crate::memory::load_errors_line(&errors).is_none(),
        "memory load errors: {errors:?}"
    );
    let (text, _) = crate::memory::render_lessons(&matched);
    if !text.is_empty() {
        let dir = world.root.path().join("dispatch");
        let tmp = dir.join(format!("{mid}-lessons.tmp"));
        let file = dir.join(format!("{mid}-lessons.md"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&tmp, &text).unwrap();
        std::fs::rename(&tmp, &file).unwrap();
    }
    (matched, text)
}

/// A fresh plain ticket — the subsequent task another worker is sent.
fn new_task(world: &World, title: &str, tags: &[&str]) -> crate::issue::board::Issue {
    let pm = crate::issue::Pm::at(&world.pm()).expect("fixture pm");
    let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    let out = crate::issue::write::new_issue(
        &pm,
        &world.checkout(),
        Some(PROJECT),
        title,
        None,
        None,
        &[],
        None,
        None,
        &tags,
        None,
        None,
        "pm",
    )
    .expect("ticket created");
    world.issue(out["id"].as_str().unwrap())
}
/// Snap before a refused call, prove it carried a named refusal and no
/// tracker, queue, or approval effect.
fn refuse(world: &World, call: impl FnOnce(&World) -> Result<Value>) {
    let before = world.snap();
    world.assert_refused(call(world), &before);
}

impl World {
    /// Register `alias` in `role` and stamp the endpoint identity an
    /// adapter `open` would have written (`store.set_identity`):
    /// generation, pid + recorded start. A bare row resolves through
    /// the seam but its proof is incomplete — `valid_identity_proof`
    /// refuses it. The pid is this test process's; see the module doc.
    fn memory_agent(&self, alias: &str, role: &str) {
        self.register(alias, role);
        self.shared
            .store
            .set_identity(
                alias,
                &crate::adapter::Identity {
                    thread_id: format!("{alias}-thread"),
                    session_id: format!("{alias}-session"),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(format!("{alias}-gen-1")),
                    attach: None,
                },
            )
            .unwrap_or_else(|e| panic!("endpoint identity for {alias}: {e}"));
    }

    /// Remove and re-register `alias`, then stamp a fresh generation —
    /// a NEW registration under the same alias (new `stable_id`, same
    /// display name), the laundering a retry would need.
    fn re_register(&self, alias: &str, role: &str) {
        self.shared
            .store
            .remove_agent(alias, false, &json!({"by": "fixture", "by_kind": "test"}))
            .unwrap_or_else(|e| panic!("remove {alias}: {e}"));
        self.register(alias, role);
        self.shared
            .store
            .set_identity(
                alias,
                &crate::adapter::Identity {
                    thread_id: format!("{alias}-thread-2"),
                    session_id: format!("{alias}-session-2"),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(format!("{alias}-gen-2")),
                    attach: None,
                },
            )
            .unwrap_or_else(|e| panic!("endpoint identity for {alias}: {e}"));
    }

    fn memory_propose(&self, who: Asserted, params: Value) -> Result<Value> {
        self.call(who, |shared| {
            shared.rpc_memory_propose(&params, std::process::id())
        })
    }

    fn memory_review(&self, who: Asserted, params: Value) -> Result<Value> {
        self.call(who, |shared| {
            shared.rpc_memory_review(&params, std::process::id())
        })
    }

    fn memory_finalize(&self, who: Asserted, params: Value) -> Result<Value> {
        self.call(who, |shared| {
            shared.rpc_memory_finalize(&params, std::process::id())
        })
    }
}

#[test]
fn cad225_lesson_needs_two_distinct_reviewer_proofs_pm_finalize_then_reuse() {
    let mut world = World::open();
    world.memory_agent("pm", "pm");
    world.memory_agent("worker-author", "worker");
    world.memory_agent("worker-r1", "worker");
    world.memory_agent("worker-r2", "worker");

    // Three proposals: the scoped lesson under test, a never-reviewed
    // file, and a lesson scoped to a provider this team does not run.
    let p1 = world
        .memory_propose(
            agent("worker-author"),
            propose_params(
                "lane-lesson",
                "gotcha",
                json!({"tags": ["lane"]}),
                "PINNED FACT — bound grants admit only the exact render",
            ),
        )
        .expect("native worker proposal");
    assert_eq!(p1["status"].as_str(), Some("proposed"));
    assert_eq!(p1["committed"].as_bool(), Some(true));
    assert_eq!(p1["quorum"]["eligible"].as_bool(), Some(false));
    let d1 = p1["digest"].as_str().expect("proposal digest").to_string();
    assert!(d1.len() == 64 && d1.bytes().all(|b| b.is_ascii_hexdigit()));

    let p3 = world
        .memory_propose(
            agent("worker-r1"),
            propose_params(
                "forged-status",
                "gotcha",
                json!({"tags": ["lane"]}),
                "UNREVIEWED FACT — never evidence-checked",
            ),
        )
        .expect("unreviewed proposal");
    let d3 = p3["digest"].as_str().unwrap().to_string();

    let p4 = world
        .memory_propose(
            agent("worker-r2"),
            propose_params(
                "codex-only",
                "recipe",
                json!({"providers": ["codex"]}),
                "CODEX FACT — provider-scoped lesson",
            ),
        )
        .expect("provider-scoped proposal");
    let d4 = p4["digest"].as_str().unwrap().to_string();

    // ── Refusals: none of these may touch the tracker ─────────────
    let bad64 = "0".repeat(64);
    let mut smuggled = propose_params("claim-smuggle", "gotcha", json!({}), "FACT");
    smuggled["reviewer"] = json!("pm");
    refuse(&world, |w| {
        w.memory_propose(agent("worker-author"), smuggled)
    });
    refuse(&world, |w| {
        w.memory_review(
            agent("worker-r1"),
            review_params("lane-lesson", "accept", "pass", &bad64),
        )
    });
    let mut blank_evidence = review_params("lane-lesson", "accept", "pass", &d1);
    blank_evidence["evidence"] = json!("  ");
    refuse(&world, |w| {
        w.memory_review(agent("worker-r1"), blank_evidence)
    });
    refuse(&world, |w| {
        w.memory_review(
            agent("worker-r1"),
            review_params("lane-lesson", "verify", "pass", &d1),
        )
    });
    refuse(&world, |w| {
        w.memory_review(
            agent("worker-author"),
            review_params("lane-lesson", "accept", "pass", &d1),
        )
    });
    refuse(&world, |w| {
        w.memory_review(
            agent("reviewer"),
            review_params("lane-lesson", "accept", "pass", &d1),
        )
    });
    refuse(&world, |w| {
        w.memory_review(
            Asserted::Unproven,
            review_params("lane-lesson", "accept", "pass", &d1),
        )
    });
    refuse(&world, |w| {
        w.memory_review(
            Asserted::Operator,
            review_params("lane-lesson", "accept", "pass", &d1),
        )
    });
    refuse(&world, |w| {
        w.memory_review(
            agent("ghost"),
            review_params("lane-lesson", "accept", "pass", &d1),
        )
    });
    refuse(&world, |w| {
        w.memory_finalize(
            agent("worker-r1"),
            finalize_params("lane-lesson", "accept", &d1),
        )
    });
    let mut edited = finalize_params("lane-lesson", "accept", &d1);
    edited["edit"] = json!({"text": "reword the claim"});
    refuse(&world, |w| w.memory_finalize(agent("pm"), edited));
    refuse(&world, |w| {
        w.memory_finalize(agent("pm"), finalize_params("lane-lesson", "accept", &d1))
    });
    refuse(&world, |w| {
        w.memory_finalize(agent("pm"), finalize_params("forged-status", "accept", &d3))
    });
    refuse(&world, |w| {
        w.memory_propose(
            agent("worker-author"),
            propose_params("lane-lesson", "gotcha", json!({}), "OVERWRITE FACT"),
        )
    });

    // ── Two independent pass receipts on the unchanged digest ─────
    let r1 = world
        .memory_review(
            agent("worker-r1"),
            review_params("lane-lesson", "accept", "pass", &d1),
        )
        .expect("first distinct reviewer");
    assert_eq!(r1["cycle"].as_u64(), Some(1));
    assert_eq!(r1["quorum"]["eligible"].as_bool(), Some(false));

    // The same caller cannot review its cycle twice — not even under a
    // re-registered alias (new stable_id, same alias): both refuse.
    let dup = world.memory_review(
        agent("worker-r1"),
        review_params("lane-lesson", "accept", "pass", &d1),
    );
    assert!(dup.is_err(), "same caller must not review the cycle twice");
    assert_eq!(
        stored(&world, "lane-lesson").front.reviews.len(),
        1,
        "the refused retry attached no second receipt"
    );
    let registration = world
        .shared
        .store
        .agent("worker-r1")
        .expect("reviewer row")
        .created
        .to_bits();
    world.re_register("worker-r1", "worker");
    let laundered = world.memory_review(
        agent("worker-r1"),
        review_params("lane-lesson", "accept", "pass", &d1),
    );
    assert!(
        laundered.is_err(),
        "a re-registered alias must not review the cycle again"
    );
    let receipts = &stored(&world, "lane-lesson").front.reviews;
    assert_eq!(receipts.len(), 1, "the laundered retry attached no receipt");
    assert_eq!(
        receipts[0].identity,
        format!("worker-r1#{registration}"),
        "the lone receipt is the removed registration's — the retry's new \
         stable_id never landed, so the same-alias bar held"
    );

    let r2 = world
        .memory_review(
            agent("worker-r2"),
            review_params("lane-lesson", "accept", "pass", &d1),
        )
        .expect("second distinct reviewer");
    assert_eq!(r2["quorum"]["eligible"].as_bool(), Some(true));

    // Quorum alone is not eligibility: the record is still proposed and
    // every reader still withholds it until a PM finalizes.
    assert_eq!(stored(&world, "lane-lesson").front.status, "proposed");
    assert!(!retrieval(&world, "lane-lesson").0);

    // The provider-scoped lesson takes the same supported path — two
    // distinct non-author receipts plus the PM acceptance.
    for who in ["worker-author", "worker-r1"] {
        world
            .memory_review(
                agent(who),
                review_params("codex-only", "accept", "pass", &d4),
            )
            .expect("codex-only review");
    }
    world
        .memory_finalize(agent("pm"), finalize_params("codex-only", "accept", &d4))
        .expect("codex-only finalize");

    // Wrong digest cannot finalize; the exact digest can.
    refuse(&world, |w| {
        w.memory_finalize(
            agent("pm"),
            finalize_params("lane-lesson", "accept", &bad64),
        )
    });
    let f1 = world
        .memory_finalize(agent("pm"), finalize_params("lane-lesson", "accept", &d1))
        .expect("PM acceptance finalization");
    assert_eq!(f1["finalized"].as_bool(), Some(true));
    assert_eq!(f1["status"].as_str(), Some("accepted"));
    assert_eq!(f1["cycle"].as_u64(), Some(1));

    // A project-wide rule rides the briefing's `project_rules` — same
    // supported path, proposed by a different worker.
    let p2 = world
        .memory_propose(
            agent("worker-r2"),
            propose_params(
                "project-rule",
                "rule",
                json!({"project": true}),
                "RULE FACT — never propose under a forged grant id",
            ),
        )
        .expect("rule proposal");
    let d2 = p2["digest"].as_str().unwrap().to_string();
    // The re-registered worker-r1 is a valid actor for a memory it has
    // never reviewed — the alias bar is per cycle, not a ban.
    for who in ["worker-author", "worker-r1"] {
        world
            .memory_review(
                agent(who),
                review_params("project-rule", "accept", "pass", &d2),
            )
            .expect("project-rule review");
    }
    world
        .memory_finalize(agent("pm"), finalize_params("project-rule", "accept", &d2))
        .expect("project-rule finalize");

    // ── Eligible state, proven on the stored receipts ─────────────
    let (ok, why) = retrieval(&world, "lane-lesson");
    assert!(ok, "eligible after PM acceptance: {why}");
    let mem = stored(&world, "lane-lesson");
    assert_eq!(mem.front.status, "accepted");
    assert_eq!(mem.front.author.as_deref(), Some("worker-author"));
    assert_eq!(mem.front.reviews.len(), 2);
    let reviewers: BTreeSet<String> = mem
        .front
        .reviews
        .iter()
        .map(|r| r.reviewer.clone())
        .collect();
    assert_eq!(
        reviewers,
        BTreeSet::from(["worker-r1".to_string(), "worker-r2".to_string()])
    );
    for receipt in &mem.front.reviews {
        assert_eq!(receipt.operation, "accept");
        assert_eq!(receipt.cycle, 1);
        assert_eq!(receipt.digest, d1);
        assert_eq!(receipt.role, "worker");
        assert!(!receipt.generation.is_empty());
        assert_ne!(receipt.process_start, 0);
        assert!(!receipt.evidence.is_empty());
        assert_ne!(receipt.reviewer, "worker-author");
    }
    assert_ne!(mem.front.reviews[0].identity, mem.front.reviews[1].identity);
    let fin = mem
        .front
        .finalizations
        .iter()
        .find(|f| f.operation == "accept")
        .expect("PM acceptance receipt");
    assert_eq!(fin.cycle, 1);
    assert_eq!(fin.digest, d1);
    assert_eq!(fin.finalizer.alias, "pm");
    assert_eq!(fin.finalizer.role, "pm");
    assert!(retrieval(&world, "project-rule").0);
    assert!(retrieval(&world, "codex-only").0);
    assert!(
        !retrieval(&world, "forged-status").0,
        "unreviewed proposal must stay ineligible"
    );

    // ── A forged `status:` cannot make unreviewed guidance eligible ─
    // The tracker file is plain text; flip the field the way a PM-dir
    // writer could and prove the recorded-receipt chain still blocks it.
    let l3_path = world
        .pm()
        .join(PROJECT)
        .join("memory")
        .join("forged-status.md");
    let text = std::fs::read_to_string(&l3_path).unwrap();
    let forged = text.replacen("status: proposed", "status: accepted", 1);
    assert_ne!(forged, text, "fixture must actually flip the field");
    std::fs::write(&l3_path, forged).unwrap();
    let (ok, _why) = retrieval(&world, "forged-status");
    assert!(!ok, "forged status must stay ineligible: {_why}");
    let forged_mem = stored(&world, "forged-status");
    assert_eq!(forged_mem.front.status, "accepted");
    assert!(
        forged_mem.front.reviews.is_empty() && forged_mem.front.finalizations.is_empty(),
        "ineligible because the recorded evidence chain is empty: \
         no reviewer receipts and no PM acceptance finalization exist"
    );
    // The forged record can't even be pushed through review now: the
    // status field it forged makes an accept cycle impossible.
    let rescue = world.memory_review(
        agent("worker-author"),
        review_params(
            "forged-status",
            "accept",
            "pass",
            &digest_of(&world, "forged-status"),
        ),
    );
    assert!(rescue.is_err(), "forged status must break the accept path");

    // ── Reuse: another worker's subsequent task gets the lesson ────
    // `dispatch::run` matches on the worker row's provider — the second
    // worker here is `worker-consumer`, a different registered endpoint
    // than the author and both reviewers.
    world.memory_agent("worker-consumer", "worker");
    let tagged = new_task(&world, "Subsequent scoped task", &["lane"]);
    let (matched, text) = dispatch_lessons(&world, &tagged, "inbox", "dispatch-1");
    assert_eq!(
        slugs(&matched),
        BTreeSet::from(["lane-lesson".to_string(), "project-rule".to_string()]),
        "the scoped task must get the accepted lesson and the project rule"
    );
    assert!(matched.withheld.is_empty(), "nothing here is stale-marked");
    for mem in &matched.lessons {
        assert_eq!(
            matched.label(mem),
            "unverified",
            "accepted-but-unverified lessons are labelled, never presented as verified"
        );
        assert!(
            crate::memory::last_verified(mem).is_none(),
            "no verify finalization exists — the label is structural, not wording"
        );
    }
    assert!(text.contains("lane-lesson"), "the injected lesson is named");
    assert!(
        text.contains("PINNED FACT"),
        "the injected lesson's fact rides the file"
    );
    assert!(
        !text.contains("codex-only"),
        "out-of-scope lesson not injected"
    );
    assert!(
        !text.contains("forged-status") && !text.contains("UNREVIEWED FACT"),
        "unreviewed or status-forged guidance never reaches the worker"
    );
    let file = world
        .root
        .path()
        .join("dispatch")
        .join("dispatch-1-lessons.md");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);

    // Scope selectivity: an untagged task gets only the project rule;
    // a codex-provider dispatch additionally gets the codex lesson.
    let untagged = new_task(&world, "Subsequent untagged task", &[]);
    let (untagged_match, _) = dispatch_lessons(&world, &untagged, "inbox", "dispatch-2");
    assert_eq!(
        slugs(&untagged_match),
        BTreeSet::from(["project-rule".to_string()]),
        "tag-scoped lesson must not leak onto an untagged task"
    );
    let (codex_match, _) = dispatch_lessons(&world, &tagged, "codex", "dispatch-3");
    assert!(
        slugs(&codex_match).contains("codex-only"),
        "the provider-scoped lesson matches only its provider"
    );

    // The briefing surface (`cli/briefing.rs` reads `project_rules`):
    // the project-wide rule lists, scoped/unreviewed lessons do not.
    let pm = crate::issue::Pm::at(&world.pm()).expect("fixture pm");
    let (rules, errors) = crate::memory::project_rules(&pm, PROJECT);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(slugs(&rules), BTreeSet::from(["project-rule".to_string()]));
    assert!(rules.withheld.is_empty());

    // ── Restart: eligibility is tracker state, not daemon state ────
    world.shared = World::reopen(world.root.path().to_path_buf());
    assert!(retrieval(&world, "lane-lesson").0);
    let (again, again_text) = dispatch_lessons(&world, &tagged, "inbox", "dispatch-4");
    assert_eq!(slugs(&again), slugs(&matched));
    assert_eq!(again_text, text);
}

#[test]
fn cad225_mutated_lesson_content_voids_its_acceptance_for_retrieval() {
    let world = World::open();
    world.memory_agent("pm", "pm");
    world.memory_agent("worker-a", "worker");
    world.memory_agent("worker-b", "worker");
    world.memory_agent("worker-c", "worker");

    let proposed = world
        .memory_propose(
            agent("worker-a"),
            propose_params(
                "mutable-rule",
                "rule",
                json!({"project": true}),
                "PINNED FACT line v1",
            ),
        )
        .expect("proposal");
    let d = proposed["digest"].as_str().unwrap().to_string();
    for who in ["worker-b", "worker-c"] {
        world
            .memory_review(
                agent(who),
                review_params("mutable-rule", "accept", "pass", &d),
            )
            .expect("accept review");
    }
    world
        .memory_finalize(agent("pm"), finalize_params("mutable-rule", "accept", &d))
        .expect("accept finalize");
    assert!(retrieval(&world, "mutable-rule").0);

    // Mutate the claim out of band — the file, not any caller path.
    let path = world
        .pm()
        .join(PROJECT)
        .join("memory")
        .join("mutable-rule.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replacen(
            "PINNED FACT line v1",
            "PINNED FACT line v1 (post-acceptance edit)",
            1,
        ),
    )
    .unwrap();
    let d_new = digest_of(&world, "mutable-rule");
    assert_ne!(d_new, d, "the edit must change the semantic digest");

    // Retrieval is voided immediately: the acceptance finalization is
    // bound to the OLD digest.
    let (ok, _why) = retrieval(&world, "mutable-rule");
    assert!(!ok, "mutated content must not stay eligible: {_why}");
    let mutated = stored(&world, "mutable-rule");
    assert_eq!(mutated.front.status, "accepted");
    assert!(
        mutated.front.finalizations.iter().all(|f| f.digest == d),
        "every recorded finalization still names the pre-mutation digest \
         — none covers the mutated claim, so retrieval stays void"
    );

    // The pinned digest bars reviews against the old revision.
    let stale = world.memory_review(
        agent("worker-b"),
        review_params("mutable-rule", "verify", "pass", &d),
    );
    assert!(
        stale.is_err(),
        "a stale digest cannot review the mutated record"
    );
    let after_stale = stored(&world, "mutable-rule");
    assert_eq!(after_stale.front.reviews.len(), 2);
    assert_eq!(
        after_stale.front.active_operation, None,
        "the refused verify opened no review cycle"
    );

    // Even the supported re-check — two distinct verify receipts plus a
    // PM verify finalize on the NEW digest — cannot restore the record:
    // its acceptance finalization names the old claim forever. The only
    // supported path for changed content is a fresh proposal.
    for who in ["worker-b", "worker-c"] {
        world
            .memory_review(
                agent(who),
                review_params("mutable-rule", "verify", "pass", &d_new),
            )
            .expect("verify review on the mutated digest");
    }
    world
        .memory_finalize(
            agent("pm"),
            finalize_params("mutable-rule", "verify", &d_new),
        )
        .expect("PM verify finalize");
    let (ok, why) = retrieval(&world, "mutable-rule");
    assert!(
        !ok,
        "a verify cycle on mutated content must not launder eligibility: {why}"
    );
    let overwrite = world.memory_propose(
        agent("worker-a"),
        propose_params(
            "mutable-rule",
            "rule",
            json!({"project": true}),
            "PINNED FACT line v2",
        ),
    );
    assert!(
        overwrite.is_err(),
        "the mutated slug cannot be re-proposed in place"
    );

    // And nothing downstream ever sees it.
    let task = new_task(&world, "Later task", &[]);
    let (matched, _) = dispatch_lessons(&world, &task, "inbox", "dispatch-m");
    assert!(
        !slugs(&matched).contains("mutable-rule"),
        "mutated record must not inject"
    );
    assert!(
        matched
            .withheld
            .iter()
            .all(|(m, _)| m.front.id != "mutable-rule"),
        "mutated record is not stale-marked — it is invalid"
    );
}

#[test]
fn cad225_memory_rpcs_refuse_non_agent_and_identity_incomplete_callers() {
    let world = World::open();
    world.memory_agent("pm", "pm");
    world.memory_agent("worker-a", "worker");
    let proposed = world
        .memory_propose(
            agent("worker-a"),
            propose_params("review-target", "gotcha", json!({}), "TARGET FACT"),
        )
        .expect("baseline proposal");
    let d = proposed["digest"].as_str().unwrap().to_string();

    for who in [
        Asserted::Unproven,
        Asserted::Operator,
        agent("ghost"),
        agent("reviewer"),
        // `worker` was registered by World::open but has no endpoint
        // identity: a row alone is not a memory actor.
        agent("worker"),
    ] {
        refuse(&world, |w| {
            w.memory_propose(
                who.clone(),
                propose_params("should-not-exist", "gotcha", json!({}), "NOPE FACT"),
            )
        });
        refuse(&world, |w| {
            w.memory_review(
                who.clone(),
                review_params("review-target", "accept", "pass", &d),
            )
        });
        refuse(&world, |w| {
            w.memory_finalize(who.clone(), finalize_params("review-target", "accept", &d))
        });
    }
    let (all, errors) = crate::memory::load_project_report(&world.pm(), PROJECT);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        all.iter().map(|m| m.front.id.as_str()).collect::<Vec<_>>(),
        vec!["review-target"],
        "refused callers must leave no memory behind"
    );
    assert!(world.issues().is_empty(), "no tickets were created");

    // A legitimate pass, then the alias-laundering case: remove the
    // reviewer, re-register the alias with a new registration and
    // generation — the stored receipt's alias still bars the retry.
    world.memory_agent("worker-b", "worker");
    world
        .memory_review(
            agent("worker-b"),
            review_params("review-target", "accept", "pass", &d),
        )
        .expect("first pass");
    world.re_register("worker-b", "worker");
    let retry = world.memory_review(
        agent("worker-b"),
        review_params("review-target", "accept", "pass", &d),
    );
    assert!(
        retry.is_err(),
        "a re-registered alias cannot double-review the cycle"
    );
    assert_eq!(
        stored(&world, "review-target").front.reviews.len(),
        1,
        "the laundered retry attached no second receipt"
    );
}
