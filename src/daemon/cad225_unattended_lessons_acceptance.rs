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
    let (matched, errors) = crate::memory::match_for_issue(&pm, ticket, Some(provider), None)
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

fn cad225_historical_v1_candidate(
    world: &World,
    author: &str,
    slug: &str,
    kind: &str,
    scope: Value,
    fact: &str,
) -> crate::memory::Memory {
    let row = world
        .shared
        .store
        .agent(author)
        .expect("historical author registration");
    assert!(matches!(row.role.as_str(), "pm" | "worker"));
    let generation = row
        .generation
        .as_deref()
        .expect("historical native endpoint generation");
    assert!(!generation.is_empty());
    let process_start = u64::try_from(row.pid_start.expect("historical native process start"))
        .expect("historical process start is positive");
    assert_ne!(process_start, 0);
    let front: crate::memory::Front = serde_json::from_value(json!({
        "id": slug,
        "type": kind,
        "status": "proposed",
        "scope": scope,
        "source": "CAD-225 acceptance fixture",
        "confidence": "high",
        "created": "2026-01-01T00:00:00Z",
        "author": author,
        "author_proof": {
            "alias": author,
            "registration": row.created.to_bits(),
            "generation": generation,
            "process_start": process_start,
            "role": row.role,
        },
        "contributors": [],
        "review_cycle": 0,
        "reviews": [],
        "finalizations": [],
    }))
    .expect("explicit existing-v1 frontmatter fixture");
    assert!(front.policy.is_none());
    assert!(front.author_proof.as_ref().unwrap().uid.is_none());
    assert!(front.active_operation.is_none());
    assert!(front.reviews.is_empty());
    assert!(front.finalizations.is_empty());
    let pm = crate::issue::Pm::at(&world.pm()).expect("historical fixture PM");
    let path = crate::memory::memory_dir(&pm, PROJECT).join(format!("{slug}.md"));
    let body = lesson_body(fact);
    let memory = crate::memory::Memory {
        project: PROJECT.to_string(),
        front,
        body,
        path,
    };
    let digest = crate::memory::semantic_digest(&memory);
    let before_commits = world.snap().commits;
    {
        let lock = pm
            .lock_for_paths(std::slice::from_ref(&memory.path))
            .expect("historical fixture scoped lock");
        assert!(
            !memory.path.exists(),
            "historical fixture cannot overwrite a record"
        );
        std::fs::create_dir_all(crate::memory::memory_dir(&pm, PROJECT)).unwrap();
        std::fs::write(
            &memory.path,
            format!(
                "---\n{}---\n\n{}",
                serde_yaml::to_string(&memory.front).unwrap(),
                memory.body
            ),
        )
        .expect("write only historical fixture frontmatter/body");
        pm.commit_scoped(
            &lock,
            std::slice::from_ref(&memory.path),
            "Historical v1 candidate fixture; not a new proposal\n\nActor: test\n",
        )
        .expect("commit only historical candidate fixture");
    }
    assert_eq!(world.snap().commits, before_commits + 1);
    let loaded = stored(world, slug);
    assert!(loaded.front.policy.is_none());
    assert!(loaded.front.author_proof.as_ref().unwrap().uid.is_none());
    assert_eq!(loaded.front.status, "proposed");
    assert!(loaded.front.reviews.is_empty() && loaded.front.finalizations.is_empty());
    assert_eq!(loaded.body, memory.body);
    assert_eq!(crate::memory::semantic_digest(&loaded), digest);
    assert!(!crate::memory::retrieval_status(&loaded).0);
    loaded
}

// CAD-347 vendor/authority oracles use the actual public JSON dispatcher.
// In particular, memory_classify must be wired through BOTH caller_gate
// and dispatch_method; these tests do not call an anticipated Rust API.
fn cad347_vendor_rpc(world: &World, who: Asserted, method: &str, params: Value) -> Result<Value> {
    world.call(who, |_| {
        world.shared.dispatch(method, &params, std::process::id())
    })
}

fn cad347_vendor_identity(world: &World, alias: &str, model: Option<&str>, generation: &str) {
    world
        .shared
        .store
        .set_identity(
            alias,
            &crate::adapter::Identity {
                thread_id: format!("{alias}-thread"),
                session_id: format!("{alias}-session"),
                model: model.map(str::to_string),
                effort: None,
                pid: std::process::id(),
                endpoint: None,
                generation: Some(generation.to_string()),
                attach: None,
            },
        )
        .unwrap_or_else(|e| panic!("native ready snapshot for {alias}: {e}"));
}

fn cad347_vendor_agent(world: &World, alias: &str, transport: &str, model: Option<&str>) {
    let endpoint_kind = match transport {
        "inbox" => "inbox",
        "pi" => "managed",
        _ => panic!("unsupported acceptance transport"),
    };
    world
        .shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: transport,
            endpoint_kind,
            role: "worker",
            cwd: world.root.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap_or_else(|e| panic!("register {alias}: {e}"));
    cad347_vendor_identity(world, alias, model, "cad347-generation-1");
}

fn cad347_vendor_candidate(world: &World, slug: &str, audience: &str) -> String {
    let mut params = propose_params(
        slug,
        "decision",
        json!({}),
        "Prefer independently accountable evidence review for this judgement",
    );
    params["source"] = json!("CAD-347 independent vendor acceptance");
    params["audience"] = json!(audience);
    let proposal = cad347_vendor_rpc(world, agent("cad347-author"), "memory_propose", params)
        .expect("new native judgement proposal");
    let digest = proposal["digest"]
        .as_str()
        .expect("proposal digest")
        .to_string();
    assert_eq!(digest_of(world, slug), digest);
    assert!(!retrieval(world, slug).0);
    digest
}

fn cad347_vendor_attest(world: &World, slug: &str, digest: &str) -> String {
    cad347_vendor_rpc(
        world,
        Asserted::Operator,
        "memory_classify",
        json!({
            "project": PROJECT,
            "slug": slug,
            "digest": digest,
            "provenance": "agent-verified",
            "evidence": "Operator attests the agent-authored source provenance; no mechanical check is claimed",
        }),
    )
    .expect("operator source-provenance classification through native dispatch");
    let classified_digest = digest_of(world, slug);
    assert_ne!(
        classified_digest, digest,
        "source classification changes the semantic claim"
    );
    assert!(
        !retrieval(world, slug).0,
        "classification alone is not acceptance"
    );
    classified_digest
}

fn cad347_vendor_refuse(world: &World, slug: &str, who: Asserted, method: &str, params: Value) {
    let path = stored(world, slug).path;
    let before_bytes = std::fs::read(&path).expect("candidate before refused call");
    refuse(world, |w| cad347_vendor_rpc(w, who, method, params));
    assert_eq!(
        std::fs::read(&path).expect("candidate after refused call"),
        before_bytes,
        "refused native call must preserve all candidate bytes"
    );
}

// A nonqualifying review may be rejected at submission or retained as a
// nonqualifying receipt. The authority assertion is at finalization,
// not a prescription of which review-admission stage must reject it.
fn cad347_vendor_try_review(world: &World, slug: &str, reviewer: &str, digest: &str) {
    let before = world.snap();
    let path = stored(world, slug).path;
    let before_bytes = std::fs::read(&path).expect("candidate before review");
    let result = cad347_vendor_rpc(
        world,
        agent(reviewer),
        "memory_review",
        review_params(slug, "accept", "pass", digest),
    );
    if result.is_err() {
        world.assert_refused(result, &before);
        assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
    }
    assert_eq!(digest_of(world, slug), digest);
    assert!(!retrieval(world, slug).0, "a review is not finalization");
}

// Snapshot the entire canonical memory subtree, including temporary
// files and empty directories, not just parseable memory records.
fn cad347_memory_tree(world: &World) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let root = world.pm().join(PROJECT).join("memory");
    let mut found = BTreeMap::new();
    let mut pending = vec![root.clone()];
    while let Some(path) = pending.pop() {
        if !path.exists() {
            continue;
        }
        let relative = path.strip_prefix(&root).unwrap().to_path_buf();
        if path.is_dir() {
            found.insert(relative, None);
            for entry in std::fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
        } else {
            found.insert(relative, Some(std::fs::read(&path).unwrap()));
        }
    }
    found
}

fn cad347_closed_refuse(world: &World, who: Asserted, method: &str, params: Value) {
    let before_files = cad347_memory_tree(world);
    refuse(world, |w| cad347_vendor_rpc(w, who, method, params));
    assert_eq!(
        cad347_memory_tree(world),
        before_files,
        "rejected input must not create, replace, or partially write memory files"
    );
}

// These are deliberately UNTRUSTED request values. No entry is a native
// check receipt, approval, active-turn proof, or execution result.
fn cad347_forbidden_authority_fields() -> Vec<(&'static str, Value)> {
    vec![
        ("policy", json!({"revision": 1})),
        ("policy_revision", json!(1)),
        ("schema_revision", json!(1)),
        ("author", json!("operator")),
        ("author_proof", json!({"untrusted_fixture": true})),
        ("contributors", json!(["operator"])),
        ("provenance", json!("operator")),
        ("model", json!("anthropic/claude-sonnet-4-5")),
        ("provider", json!("anthropic")),
        ("vendor", json!("anthropic")),
        ("generation", json!("caller-selected-generation")),
        ("check_receipt", json!({"untrusted_fixture": true})),
        ("checks", json!([{"untrusted_fixture": true}])),
        ("finalizations", json!([{"untrusted_fixture": true}])),
        ("operator_approval", json!({"untrusted_fixture": true})),
        ("usage_receipt", json!({"untrusted_fixture": true})),
        ("feedback", json!({"used_reports": 999})),
        ("used_reports", json!(999)),
        ("wrong_reports", json!(999)),
        ("reread_reports", json!(999)),
        ("last_used_at", json!("2099-01-01T00:00:00Z")),
        ("unexpected_cad347_field", json!(true)),
    ]
}

#[test]
fn cad347_new_proposal_rejects_trusted_and_unknown_request_fields_without_effects() {
    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    for (index, (field, value)) in cad347_forbidden_authority_fields().into_iter().enumerate() {
        let slug = format!("cad347-forbidden-{index}");
        let mut params = propose_params(
            &slug,
            "decision",
            json!({}),
            "Only daemon-owned evidence can authorize this judgement",
        );
        params[field] = value;
        cad347_closed_refuse(&world, agent("cad347-author"), "memory_propose", params);
    }
}

#[test]
fn cad347_imported_frontmatter_cannot_select_legacy_or_supply_trusted_authority() {
    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    let seed_slug = "cad347-import-seed";
    cad347_vendor_candidate(&world, seed_slug, "project");
    let seed = stored(&world, seed_slug);
    let mut template = serde_json::to_value(&seed.front).unwrap();
    // A legacy-shaped imported document is not an EXISTING legacy
    // canonical record. A successful NEW import must still be v2.
    template.as_object_mut().unwrap().remove("policy");
    for (index, (field, value)) in [
        ("policy", json!({"revision": 1})),
        ("schema_revision", json!(1)),
        ("author", json!("operator")),
        ("provenance", json!("operator")),
        ("model", json!("anthropic/claude-sonnet-4-5")),
        ("check_receipt", json!({"untrusted_fixture": true})),
        ("feedback", json!({"used_reports": 999})),
    ]
    .into_iter()
    .enumerate()
    {
        let slug = format!("cad347-import-{index}");
        let mut front = template.clone();
        front[field] = value;
        let raw = format!(
            "---\n{}---\n\n{}",
            serde_yaml::to_string(&front).unwrap(),
            seed.body
        );
        let mut params = propose_params(&slug, "decision", json!({}), "unused");
        params.as_object_mut().unwrap().remove("text");
        params["from"] = json!(raw);
        let before = world.snap();
        let before_files = cad347_memory_tree(&world);
        let result = cad347_vendor_rpc(&world, agent("cad347-author"), "memory_propose", params);
        match result {
            Err(error) => {
                world.assert_refused(Err(error), &before);
                assert_eq!(cad347_memory_tree(&world), before_files);
            }
            Ok(proposal) => {
                assert_eq!(proposal["policy_revision"].as_u64(), Some(2));
                let imported = stored(&world, &slug);
                assert_eq!(imported.front.status, "proposed");
                assert_eq!(
                    imported.front.author_proof.as_ref().unwrap().alias,
                    "cad347-author"
                );
                assert!(imported.front.reviews.is_empty());
                assert!(imported.front.finalizations.is_empty());
                let front = serde_json::to_value(&imported.front).unwrap();
                let policy = &front["policy"];
                assert_eq!(policy["revision"].as_u64(), Some(2));
                assert_eq!(policy["nature"].as_str(), Some("judgement"));
                assert_eq!(policy["provenance"].as_str(), Some("agent-claimed"));
                assert_eq!(policy["reviews"].as_array().unwrap().len(), 0);
                assert_eq!(policy["finalizations"].as_array().unwrap().len(), 0);
                assert_eq!(policy["feedback"]["used_reports"].as_u64(), Some(0));
                assert!(!retrieval(&world, &slug).0);
            }
        }
    }
}

#[test]
fn cad347_classify_review_and_finalize_inputs_are_closed_even_for_valid_callers() {
    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    cad347_vendor_agent(
        &world,
        "cad347-curator",
        "inbox",
        Some("anthropic/claude-sonnet-4-5"),
    );
    let slug = "cad347-closed-transitions";
    let proposed = cad347_vendor_candidate(&world, slug, "project");
    for (field, value) in cad347_forbidden_authority_fields() {
        let mut params = json!({
            "project": PROJECT,
            "slug": slug,
            "digest": proposed,
            "provenance": "agent-verified",
            "evidence": "Operator source attestation, not a check receipt",
        });
        params[field] = value;
        cad347_closed_refuse(&world, Asserted::Operator, "memory_classify", params);
    }
    // Positive setup prevents missing endpoint or blanket caller refusal
    // from satisfying the closed-input assertions as a whole.
    let digest = cad347_vendor_attest(&world, slug, &proposed);
    for (field, value) in cad347_forbidden_authority_fields() {
        let mut params = review_params(slug, "accept", "pass", &digest);
        params[field] = value;
        cad347_closed_refuse(&world, agent("cad347-curator"), "memory_review", params);
    }
    cad347_vendor_rpc(
        &world,
        agent("cad347-curator"),
        "memory_review",
        review_params(slug, "accept", "pass", &digest),
    )
    .expect("the same native curator can submit an unmodified valid review");
    for (field, value) in cad347_forbidden_authority_fields() {
        let mut params = finalize_params(slug, "accept", &digest);
        params[field] = value;
        cad347_closed_refuse(&world, Asserted::Operator, "memory_finalize", params);
    }
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    )
    .expect("valid operator finalization after the mandatory cross-vendor curator");
    assert!(retrieval(&world, slug).0);
}

#[test]
fn cad347_mechanical_promotion_stays_blocked_without_genuine_executor_evidence() {
    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    cad347_vendor_agent(
        &world,
        "cad347-curator",
        "inbox",
        Some("anthropic/claude-sonnet-4-5"),
    );
    cad347_vendor_agent(
        &world,
        "cad347-other-vendor",
        "inbox",
        Some("google/gemini-2.5-pro"),
    );
    world.memory_agent("cad347-pm", "pm");
    cad347_vendor_identity(
        &world,
        "cad347-pm",
        Some("openai/gpt-5"),
        "cad347-pm-confirmed-generation",
    );
    // Prove this asserted operator is supported by the native matrix:
    // judgement approval succeeds without inventing a human agent alias.
    let control_slug = "cad347-operator-judgement-control";
    let mut control_params = propose_params(
        control_slug,
        "decision",
        json!({}),
        "An operator judgement approval is not a mechanical check",
    );
    control_params["nature"] = json!("judgement");
    let control = cad347_vendor_rpc(&world, Asserted::Operator, "memory_propose", control_params)
        .expect("native operator judgement proposal");
    let control_digest = control["digest"].as_str().unwrap();
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(control_slug, "accept", control_digest),
    )
    .expect("positive operator judgement authorization control");
    assert!(retrieval(&world, control_slug).0);

    for route in ["operator", "agent-claimed", "agent-verified"] {
        let slug = format!("cad347-mechanical-{route}");
        let mut params = propose_params(
            &slug,
            "rule",
            json!({}),
            "Mechanical validity requires a genuine qualifying executor receipt",
        );
        params["nature"] = json!("mechanical");
        params["verification_condition"] = json!({"check_id": "cad347-evidence-unavailable"});
        let who = if route == "operator" {
            Asserted::Operator
        } else {
            agent("cad347-author")
        };
        let proposal = cad347_vendor_rpc(&world, who, "memory_propose", params)
            .expect("mechanical candidate with declarative condition metadata");
        let mut digest = proposal["digest"].as_str().unwrap().to_string();
        if route == "agent-verified" {
            digest = cad347_vendor_attest(&world, &slug, &digest);
        }
        // Review receipts and operator provenance attestations are not
        // CAD-348 executor evidence. Admission may reject nonqualifying
        // reviews or retain them without permitting promotion.
        for reviewer in ["cad347-curator", "cad347-other-vendor"] {
            cad347_vendor_try_review(&world, &slug, reviewer, &digest);
        }
        for finalizer in [agent("cad347-pm"), Asserted::Operator] {
            cad347_closed_refuse(
                &world,
                finalizer,
                "memory_finalize",
                finalize_params(&slug, "accept", &digest),
            );
        }
        assert!(!retrieval(&world, &slug).0);
    }
}

#[test]
fn cad347_verification_metadata_never_accepts_command_fields_or_executes_them() {
    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    let marker = world.root.path().join("cad347-command-must-not-run");
    let command = format!("touch {}", marker.display());
    for (index, (field, value)) in [
        ("command", json!(command)),
        ("shell", json!(command)),
        ("argv", json!(["/usr/bin/touch", marker.to_str().unwrap()])),
    ]
    .into_iter()
    .enumerate()
    {
        for nested in [false, true] {
            let slug = format!("cad347-no-exec-{index}-{nested}");
            let mut params = propose_params(
                &slug,
                "rule",
                json!({}),
                "A declared verification condition never authorizes command execution",
            );
            params["nature"] = json!("mechanical");
            params["verification_condition"] = json!({"check_id": "cad347-evidence-unavailable"});
            if nested {
                params["verification_condition"][field] = value.clone();
            } else {
                params[field] = value.clone();
            }
            cad347_closed_refuse(&world, agent("cad347-author"), "memory_propose", params);
            assert!(
                !marker.exists(),
                "untrusted verification metadata executed a command"
            );
        }
    }
}

#[test]
fn cad347_classification_digest_change_refuses_old_review_classify_and_finalize() {
    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    cad347_vendor_agent(
        &world,
        "cad347-curator",
        "inbox",
        Some("anthropic/claude-sonnet-4-5"),
    );
    let slug = "cad347-stale-classification-digest";
    let old_digest = cad347_vendor_candidate(&world, slug, "project");
    let digest = cad347_vendor_attest(&world, slug, &old_digest);
    cad347_closed_refuse(
        &world,
        agent("cad347-curator"),
        "memory_review",
        review_params(slug, "accept", "pass", &old_digest),
    );
    cad347_closed_refuse(
        &world,
        Asserted::Operator,
        "memory_classify",
        json!({
            "project": PROJECT,
            "slug": slug,
            "digest": old_digest,
            "provenance": "agent-verified",
            "evidence": "An old revision cannot authorize a new classification",
        }),
    );
    cad347_vendor_rpc(
        &world,
        agent("cad347-curator"),
        "memory_review",
        review_params(slug, "accept", "pass", &digest),
    )
    .expect("current-digest independent curator review");
    cad347_closed_refuse(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(slug, "accept", &old_digest),
    );
    assert!(!retrieval(&world, slug).0);
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    )
    .expect("current digest can finalize after the qualifying curator");
    assert!(retrieval(&world, slug).0);
}

#[test]
fn cad347_native_review_reloads_after_overlapping_operator_classification() {
    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    cad347_vendor_agent(
        &world,
        "cad347-curator",
        "inbox",
        Some("anthropic/claude-sonnet-4-5"),
    );
    let slug = "cad347-overlapping-classification";
    let old_digest = cad347_vendor_candidate(&world, slug, "project");
    let path = stored(&world, slug).path;
    let initial_front = serde_json::to_value(&stored(&world, slug).front).unwrap();
    let initial_attestations = initial_front["policy"]["attestations"]
        .as_array()
        .expect("v2 native provenance history")
        .len();
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    let current_digest = std::thread::scope(|scope| {
        let writer_world = &world;
        let writer_digest = old_digest.clone();
        let writer = scope.spawn(move || {
            reached_rx
                .recv_timeout(Duration::from_secs(15))
                .expect("native review never reached the actual pre-lock handoff");
            // This is the real operator-only native transition, not a
            // direct file edit or a manually manufactured policy receipt.
            let classified_digest = cad347_vendor_attest(writer_world, slug, &writer_digest);
            // Classification has completed its own lock/commit discipline.
            // Preserve the writer's resulting bytes/history/commit as the
            // baseline: only the stale review must have no further effect.
            let before_review = writer_world.snap();
            let before_files = cad347_memory_tree(writer_world);
            proceed_tx.send(()).unwrap();
            (before_review, before_files, classified_digest)
        });
        crate::memory::set_review_after_initial_find_hook_for_test(Some((
            path.clone(),
            reached_tx,
            proceed_rx,
        )));
        let result = cad347_vendor_rpc(
            &world,
            agent("cad347-curator"),
            "memory_review",
            review_params(slug, "accept", "pass", &old_digest),
        );
        crate::memory::set_review_after_initial_find_hook_for_test(None);
        let (before_review, before_files, classified_digest) = writer
            .join()
            .expect("owned native classification writer failed");
        world.assert_refused(result, &before_review);
        assert_eq!(cad347_memory_tree(&world), before_files);
        classified_digest
    });
    assert_ne!(current_digest, old_digest);
    assert_eq!(digest_of(&world, slug), current_digest);
    let classified = stored(&world, slug);
    let front = serde_json::to_value(&classified.front).unwrap();
    let attestations = front["policy"]["attestations"].as_array().unwrap();
    assert_eq!(attestations.len(), initial_attestations + 1);
    assert_eq!(
        attestations.last().unwrap()["digest_before"].as_str(),
        Some(old_digest.as_str()),
        "the operator's actual provenance transition must remain in history"
    );
    assert!(!retrieval(&world, slug).0);
    assert!(crate::memory::last_verified(&classified).is_none());
    cad347_vendor_rpc(
        &world,
        agent("cad347-curator"),
        "memory_review",
        review_params(slug, "accept", "pass", &current_digest),
    )
    .expect("the same confirmed native curator can review the post-classification digest");
}

#[test]
fn cad347_classify_rejects_secret_in_immutable_reference_without_echo_or_effects() {
    use sha2::{Digest, Sha256};

    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    let slug = "cad347-classify-secret-reference";
    let digest = cad347_vendor_candidate(&world, slug, "project");
    let revision = crate::issue::git(&world.pm(), &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    // Identical deterministic credential-shaped construction to the
    // existing memory unit secret test; not an actual credential.
    let fixture_digest: String = Sha256::digest(b"memory-fixture")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let token = ["figd", "_", &fixture_digest[..40]].concat();
    let before = world.snap();
    let before_files = cad347_memory_tree(&world);
    let before_front = serde_json::to_value(&stored(&world, slug).front).unwrap();
    let before_history = before_front["policy"]["attestations"].clone();
    let result = cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_classify",
        json!({
            "project": PROJECT,
            "slug": slug,
            "digest": digest,
            "provenance": "agent-verified",
            "evidence": "Operator source-provenance attestation with an immutable reference",
            "evidence_refs": [{
                "kind": "git",
                "source": format!("fixture/{token}"),
                "revision": revision,
            }],
        }),
    );
    // Never format the response/input into a failure: a regressing
    // handler might otherwise echo the very secret this test protects.
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("classification accepted a credential-shaped evidence reference"),
    };
    assert_eq!(error.code(), Some("secret_detected"));
    let message = error.to_string();
    assert!(
        !message.contains(&token),
        "secret refusal echoed the synthetic credential"
    );
    assert!(
        !message.contains(&fixture_digest[..40]),
        "secret refusal echoed the synthetic credential material"
    );
    // Boolean comparisons deliberately avoid printing potentially
    // sensitive file bytes or audit payloads if the guard regresses.
    assert!(
        world.snap() == before,
        "secret rejection changed tracker or commit effects"
    );
    assert!(
        cad347_memory_tree(&world) == before_files,
        "secret rejection changed canonical memory files"
    );
    let after_front = serde_json::to_value(&stored(&world, slug).front).unwrap();
    assert!(
        after_front["policy"]["attestations"] == before_history,
        "secret rejection changed provenance history"
    );
    assert_eq!(digest_of(&world, slug), digest);
    assert!(!retrieval(&world, slug).0);

    // Same actual operator, current candidate, reason and immutable
    // revision; only the unsafe reference source is replaced. This
    // reference names the candidate file at its genuine tracker commit.
    let safe_source = format!("{PROJECT}/memory/{slug}.md");
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_classify",
        json!({
            "project": PROJECT,
            "slug": slug,
            "digest": digest,
            "provenance": "agent-verified",
            "evidence": "Operator source-provenance attestation with an immutable reference",
            "evidence_refs": [{
                "kind": "git",
                "source": safe_source,
                "revision": revision,
            }],
        }),
    )
    .expect("safe immutable reference remains admissible for operator classification");
    assert_ne!(digest_of(&world, slug), digest);
    let classified = stored(&world, slug);
    let front = serde_json::to_value(&classified.front).unwrap();
    let history = front["policy"]["attestations"].as_array().unwrap();
    assert_eq!(history.len(), before_history.as_array().unwrap().len() + 1);
    let attestation = history.last().unwrap();
    assert_eq!(attestation["digest_before"].as_str(), Some(digest.as_str()));
    assert_eq!(
        attestation["evidence_refs"][0]["source"].as_str(),
        Some(safe_source.as_str())
    );
    assert_eq!(
        attestation["evidence_refs"][0]["revision"].as_str(),
        Some(revision.as_str())
    );
    assert!(!retrieval(&world, slug).0);
    assert!(crate::memory::last_verified(&classified).is_none());
}

#[test]
fn cad347_unknown_native_model_cannot_supply_cross_vendor_authority() {
    for (author_model, curator_model) in [
        (None, Some("anthropic/claude-sonnet-4-5")),
        (Some("openai/gpt-5"), None),
        (
            Some("unidentified/model"),
            Some("anthropic/claude-sonnet-4-5"),
        ),
        (Some("openai/gpt-5"), Some("unidentified/model")),
    ] {
        let world = World::open();
        cad347_vendor_agent(&world, "cad347-author", "inbox", author_model);
        cad347_vendor_agent(&world, "cad347-curator", "inbox", curator_model);
        let slug = "cad347-unknown-vendor";
        let proposed = cad347_vendor_candidate(&world, slug, "project");
        let digest = cad347_vendor_attest(&world, slug, &proposed);
        cad347_vendor_try_review(&world, slug, "cad347-curator", &digest);
        // An operator cannot substitute for the first proven curator
        // on an agent-verified judgement, including unknown-model cases.
        cad347_vendor_refuse(
            &world,
            slug,
            Asserted::Operator,
            "memory_finalize",
            finalize_params(slug, "accept", &digest),
        );
        assert!(!retrieval(&world, slug).0);
    }
}

#[test]
fn cad347_current_row_model_requires_matching_native_ready_evidence() {
    for target in ["cad347-author", "cad347-curator"] {
        for generation_drift in [false, true] {
            let world = World::open();
            for (alias, confirmed_model) in [
                ("cad347-author", "openai/gpt-5"),
                ("cad347-curator", "anthropic/claude-sonnet-4-5"),
            ] {
                let ready_model = if alias == target && !generation_drift {
                    None
                } else {
                    Some(confirmed_model)
                };
                cad347_vendor_agent(&world, alias, "inbox", ready_model);
            }
            let model = if target == "cad347-author" {
                "openai/gpt-5"
            } else {
                "anthropic/claude-sonnet-4-5"
            };
            let generation = if generation_drift {
                "cad347-generation-2"
            } else {
                "cad347-generation-1"
            };
            // Deliberate adversarial persisted-state fixture, NOT a
            // trusted identity event: make the current row look useful
            // without publishing a corresponding native ready snapshot.
            // The real native proposal/review/finalize guards run below.
            world
                .shared
                .store
                .write_tx(|tx| {
                    let changed = tx.execute(
                        "UPDATE agents SET model=?1,generation=?2 WHERE alias=?3",
                        rusqlite::params![model, generation, target],
                    )?;
                    assert_eq!(changed, 1);
                    Ok(())
                })
                .expect("seed row-only model evidence mismatch");
            let slug = "cad347-row-only-model";
            let proposed = cad347_vendor_candidate(&world, slug, "project");
            let digest = cad347_vendor_attest(&world, slug, &proposed);
            cad347_vendor_try_review(&world, slug, "cad347-curator", &digest);
            cad347_vendor_refuse(
                &world,
                slug,
                Asserted::Operator,
                "memory_finalize",
                finalize_params(slug, "accept", &digest),
            );
            assert!(!retrieval(&world, slug).0);
        }
    }
}

#[test]
fn cad347_openai_provider_aliases_are_not_independent_vendors() {
    for transport in ["inbox", "pi"] {
        for (author_model, curator_model) in [
            ("openai/gpt-5", "openai-codex/gpt-5"),
            ("openai-codex/gpt-5", "openai/gpt-5"),
        ] {
            let world = World::open();
            cad347_vendor_agent(&world, "cad347-author", transport, Some(author_model));
            cad347_vendor_agent(&world, "cad347-curator", transport, Some(curator_model));
            let slug = "cad347-same-openai-vendor";
            let proposed = cad347_vendor_candidate(&world, slug, "project");
            let digest = cad347_vendor_attest(&world, slug, &proposed);
            cad347_vendor_try_review(&world, slug, "cad347-curator", &digest);
            cad347_vendor_refuse(
                &world,
                slug,
                Asserted::Operator,
                "memory_finalize",
                finalize_params(slug, "accept", &digest),
            );
            assert!(!retrieval(&world, slug).0);
        }
    }
}

#[test]
fn cad347_shared_transport_preserves_real_cross_vendor_curator_authority() {
    for transport in ["inbox", "pi"] {
        let world = World::open();
        cad347_vendor_agent(&world, "cad347-author", transport, Some("openai/gpt-5"));
        cad347_vendor_agent(
            &world,
            "cad347-curator",
            transport,
            Some("anthropic/claude-sonnet-4-5"),
        );
        let slug = "cad347-shared-transport";
        let proposed = cad347_vendor_candidate(&world, slug, "project");
        let digest = cad347_vendor_attest(&world, slug, &proposed);
        cad347_vendor_rpc(
            &world,
            agent("cad347-curator"),
            "memory_review",
            review_params(slug, "accept", "pass", &digest),
        )
        .expect("confirmed cross-vendor curator behind the same transport");
        cad347_vendor_rpc(
            &world,
            agent("cad347-curator"),
            "memory_finalize",
            finalize_params(slug, "accept", &digest),
        )
        .expect("one qualifying curator can finalize project agent-verified judgement");
        assert!(retrieval(&world, slug).0);
    }
}

#[test]
fn cad347_operator_classification_and_company_extra_approval_are_distinct() {
    let world = World::open();
    for (alias, model) in [
        ("cad347-author", "openai/gpt-5"),
        ("cad347-curator", "anthropic/claude-sonnet-4-5"),
        ("cad347-same-vendor", "anthropic/claude-sonnet-4-5"),
        ("cad347-other-vendor", "google/gemini-2.5-pro"),
    ] {
        cad347_vendor_agent(&world, alias, "inbox", Some(model));
    }
    let slug = "cad347-company-curators";
    let proposed = cad347_vendor_candidate(&world, slug, "company");
    let classification = json!({
        "project": PROJECT,
        "slug": slug,
        "digest": proposed,
        "provenance": "agent-verified",
        "evidence": "Source-provenance attestation, not a mechanical check",
    });
    for who in [
        agent("cad347-author"),
        agent("cad347-curator"),
        Asserted::Unproven,
    ] {
        cad347_vendor_refuse(&world, slug, who, "memory_classify", classification.clone());
    }
    let digest = cad347_vendor_attest(&world, slug, &proposed);
    // Operator classification and even operator finalization cannot
    // replace the first mandatory independent cross-vendor curator.
    cad347_vendor_refuse(
        &world,
        slug,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    );
    cad347_vendor_rpc(
        &world,
        agent("cad347-curator"),
        "memory_review",
        review_params(slug, "accept", "pass", &digest),
    )
    .expect("first company curator");
    cad347_vendor_refuse(
        &world,
        slug,
        agent("cad347-curator"),
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    );
    cad347_vendor_try_review(&world, slug, "cad347-same-vendor", &digest);
    cad347_vendor_refuse(
        &world,
        slug,
        agent("cad347-curator"),
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    );
    cad347_vendor_rpc(
        &world,
        agent("cad347-other-vendor"),
        "memory_review",
        review_params(slug, "accept", "pass", &digest),
    )
    .expect("second distinct-vendor company curator");
    cad347_vendor_rpc(
        &world,
        agent("cad347-curator"),
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    )
    .expect("company judgement with two independent distinct-vendor curators");
    assert!(retrieval(&world, slug).0);

    // Alternative company route: a real operator can provide the extra
    // approval AFTER the first mandatory curator, not instead of it.
    let operator_slug = "cad347-company-operator";
    let proposed = cad347_vendor_candidate(&world, operator_slug, "company");
    let digest = cad347_vendor_attest(&world, operator_slug, &proposed);
    cad347_vendor_rpc(
        &world,
        agent("cad347-curator"),
        "memory_review",
        review_params(operator_slug, "accept", "pass", &digest),
    )
    .expect("mandatory first curator on operator-extra-approval route");
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(operator_slug, "accept", &digest),
    )
    .expect("operator supplies company extra approval after independent curator");
    assert!(retrieval(&world, operator_slug).0);
}

#[test]
fn cad347_new_agent_claimed_judgement_two_agent_reviews_cannot_replace_operator() {
    let world = World::open();
    for (alias, role, model) in [
        ("cad347-author", "worker", "gpt-5"),
        ("cad347-r1", "worker", "claude-sonnet-4-5"),
        ("cad347-r2", "worker", "gemini-2.5-pro"),
        ("cad347-pm", "pm", "gpt-5"),
    ] {
        world.register(alias, role);
        // Match the native adapter-open persistence seam: set_identity
        // atomically records the confirmed model and generation both on
        // the agent row and in its ready event. Request fields and
        // configured transport labels provide no model authority here.
        world
            .shared
            .store
            .set_identity(
                alias,
                &crate::adapter::Identity {
                    thread_id: format!("{alias}-thread"),
                    session_id: format!("{alias}-session"),
                    model: Some(model.to_string()),
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(format!("{alias}-gen-1")),
                    attach: None,
                },
            )
            .unwrap_or_else(|e| panic!("confirmed native identity for {alias}: {e}"));
    }

    let slug = "cad347-operator-required";
    let mut params = propose_params(
        slug,
        "decision",
        json!({}),
        "Prefer an explicit operator decision for judgement approval",
    );
    params["source"] = json!("CAD-347 independent authority acceptance");
    // Deliberately omit policy, nature, provenance, and approval fields:
    // a NEW native agent proposal must default to agent-claimed judgement.
    let proposal = world
        .memory_propose(agent("cad347-author"), params)
        .expect("new native agent judgement proposal");
    let digest = proposal["digest"]
        .as_str()
        .expect("proposal digest")
        .to_string();
    assert_eq!(stored(&world, slug).front.status, "proposed");
    assert!(!retrieval(&world, slug).0);

    // Both reviewers are distinct non-authors. Their native ready
    // snapshots identify Anthropic and Google, independently of the
    // author's OpenAI model; lack of model evidence must not be the
    // reason this authority oracle refuses finalization.
    for reviewer in ["cad347-r1", "cad347-r2"] {
        world
            .memory_review(
                agent(reviewer),
                review_params(slug, "accept", "pass", &digest),
            )
            .unwrap_or_else(|e| panic!("independent native review by {reviewer}: {e}"));
    }
    assert_eq!(digest_of(&world, slug), digest);
    assert!(!retrieval(&world, slug).0);

    let path = stored(&world, slug).path;
    let before_bytes = std::fs::read(&path).expect("candidate bytes before refusal");
    // A proven PM agent is still not a proven operator. Under v1 this
    // exact call accepts; v2 must refuse without committing or changing
    // the candidate, even after two independent native pass reviews.
    refuse(&world, |w| {
        w.memory_finalize(agent("cad347-pm"), finalize_params(slug, "accept", &digest))
    });
    assert_eq!(
        std::fs::read(&path).expect("candidate bytes after refusal"),
        before_bytes,
        "refused agent finalization must preserve the entire candidate"
    );
    assert_eq!(stored(&world, slug).front.status, "proposed");
    assert_eq!(digest_of(&world, slug), digest);
    assert!(!retrieval(&world, slug).0);
}

#[test]
fn cad225_lesson_needs_two_distinct_reviewer_proofs_pm_finalize_then_reuse() {
    let mut world = World::open();
    world.memory_agent("pm", "pm");
    world.memory_agent("worker-author", "worker");
    world.memory_agent("worker-r1", "worker");
    world.memory_agent("worker-r2", "worker");

    // Existing-v1 candidates: the scoped lesson, a never-reviewed file,
    // and a provider-scoped lesson. Native reviews/finalization below
    // exercise their preserved historical receipt semantics.
    let p1 = cad225_historical_v1_candidate(
        &world,
        "worker-author",
        "lane-lesson",
        "gotcha",
        json!({"tags": ["lane"]}),
        "PINNED FACT — bound grants admit only the exact render",
    );
    assert_eq!(p1.front.status, "proposed");
    assert!(p1.front.policy.is_none());
    assert!(!crate::memory::retrieval_status(&p1).0);
    let d1 = crate::memory::semantic_digest(&p1);
    assert!(d1.len() == 64 && d1.bytes().all(|b| b.is_ascii_hexdigit()));

    let p3 = cad225_historical_v1_candidate(
        &world,
        "worker-r1",
        "forged-status",
        "gotcha",
        json!({"tags": ["lane"]}),
        "UNREVIEWED FACT — never evidence-checked",
    );
    let d3 = crate::memory::semantic_digest(&p3);

    let p4 = cad225_historical_v1_candidate(
        &world,
        "worker-r2",
        "codex-only",
        "recipe",
        json!({"providers": ["codex"]}),
        "CODEX FACT — provider-scoped lesson",
    );
    let d4 = crate::memory::semantic_digest(&p4);

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

    // An existing-v1 project-wide rule follows the same preserved
    // native review/finalization path.
    let p2 = cad225_historical_v1_candidate(
        &world,
        "worker-r2",
        "project-rule",
        "rule",
        json!({"project": true}),
        "RULE FACT — never propose under a forged grant id",
    );
    let d2 = crate::memory::semantic_digest(&p2);
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
    let (rules, errors) = crate::memory::project_rules(&pm, PROJECT, None);
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

    let proposed = cad225_historical_v1_candidate(
        &world,
        "worker-a",
        "mutable-rule",
        "rule",
        json!({"project": true}),
        "PINNED FACT line v1",
    );
    let d = crate::memory::semantic_digest(&proposed);
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
    let proposed = cad225_historical_v1_candidate(
        &world,
        "worker-a",
        "review-target",
        "gotcha",
        json!({}),
        "TARGET FACT",
    );
    let d = crate::memory::semantic_digest(&proposed);
    let baseline_bytes = std::fs::read(&proposed.path).unwrap();

    // These callers remain forbidden on genuine NEW proposals as well
    // as native review/finalization of the historical candidate.
    for who in [
        Asserted::Unproven,
        agent("ghost"),
        agent("reviewer"),
        // Registered by World::open but without an endpoint identity:
        // a row alone is not an authenticated memory actor.
        agent("worker"),
    ] {
        refuse(&world, |w| {
            w.memory_propose(
                who.clone(),
                propose_params("should-not-exist", "gotcha", json!({}), "NOPE FACT"),
            )
        });
        assert_eq!(std::fs::read(&proposed.path).unwrap(), baseline_bytes);
        refuse(&world, |w| {
            w.memory_review(
                who.clone(),
                review_params("review-target", "accept", "pass", &d),
            )
        });
        assert_eq!(std::fs::read(&proposed.path).unwrap(), baseline_bytes);
        refuse(&world, |w| {
            w.memory_finalize(who.clone(), finalize_params("review-target", "accept", &d))
        });
        assert_eq!(std::fs::read(&proposed.path).unwrap(), baseline_bytes);
    }

    // Approved cutover: positive operator proof can originate NEW v2
    // judgements, but cannot replace historical v1 curator/PM rules.
    refuse(&world, |w| {
        w.memory_review(
            Asserted::Operator,
            review_params("review-target", "accept", "pass", &d),
        )
    });
    assert_eq!(std::fs::read(&proposed.path).unwrap(), baseline_bytes);
    refuse(&world, |w| {
        w.memory_finalize(
            Asserted::Operator,
            finalize_params("review-target", "accept", &d),
        )
    });
    assert_eq!(std::fs::read(&proposed.path).unwrap(), baseline_bytes);

    let (all, errors) = crate::memory::load_project_report(&world.pm(), PROJECT);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        all.iter().map(|m| m.front.id.as_str()).collect::<Vec<_>>(),
        vec!["review-target"],
        "refused callers must leave no memory behind"
    );
    assert!(world.issues().is_empty(), "no tickets were created");

    // Actual native legacy review, then recycled-alias refusal.
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

#[test]
fn cad347_unused_judgement_expires_at_28_days_without_destructive_expiry() {
    use crate::memory::{self, Freshness, MatchCtx};

    let world = World::open();
    world.memory_agent("cad347-lifecycle-author", "worker");
    let slug = "cad347-unused-judgement";
    let proposed = world
        .memory_propose(
            agent("cad347-lifecycle-author"),
            propose_params(
                slug,
                "gotcha",
                json!({"project": true}),
                "LIFECYCLE FACT — supplied guidance is not reported use",
            ),
        )
        .expect("native agent proposal");
    let digest = proposed["digest"].as_str().expect("proposal digest");
    world
        .memory_finalize(Asserted::Operator, finalize_params(slug, "accept", digest))
        .expect("positive operator approval of agent-claimed judgement");

    let accepted = stored(&world, slug);
    let front = serde_json::to_value(&accepted.front).expect("serialize native front");
    let policy = &front["policy"];
    assert_eq!(accepted.front.status, "accepted");
    assert_eq!(policy["revision"].as_u64(), Some(2));
    assert_eq!(policy["nature"].as_str(), Some("judgement"));
    assert_eq!(policy["provenance"].as_str(), Some("agent-claimed"));
    assert_eq!(policy["author"]["actor"]["kind"].as_str(), Some("agent"));
    assert_eq!(memory::semantic_digest(&accepted), digest);
    assert!(memory::last_verified(&accepted).is_none());
    assert!(accepted.front.verified_at.is_none());
    for counter in ["used_reports", "wrong_reports", "reread_reports"] {
        assert_eq!(policy["feedback"][counter].as_u64(), Some(0));
    }
    assert!(policy["feedback"]["last_use"].is_null());

    let acceptances: Vec<_> = policy["finalizations"]
        .as_array()
        .expect("native policy finalization history")
        .iter()
        .filter(|receipt| {
            receipt["operation"].as_str() == Some("accept")
                && receipt["digest"].as_str() == Some(digest)
        })
        .collect();
    assert_eq!(acceptances.len(), 1);
    assert_eq!(
        acceptances[0]["finalizer"]["kind"].as_str(),
        Some("operator")
    );
    let accepted_at = crate::issue::time::parse_iso(
        acceptances[0]["recorded_at"]
            .as_str()
            .expect("native acceptance timestamp"),
    )
    .expect("parse native acceptance timestamp");
    let expiry = accepted_at + 28 * 86_400;
    let original_bytes = std::fs::read(&accepted.path).expect("canonical bytes");
    let ctx = MatchCtx::default();

    let assert_supply = |memory: &memory::Memory, now: i64, window_days: u64, supplied: bool| {
        let fresh = Freshness { window_days, now };
        let matched = memory::match_memories(std::slice::from_ref(memory), &ctx, &fresh);
        assert_eq!(
            matched.lessons.iter().any(|lesson| lesson.front.id == slug),
            supplied,
            "unexpected matching eligibility at {now}, freshness window {window_days}"
        );
        let (_, rendered_slugs) = memory::render_lessons(&matched);
        assert_eq!(
            rendered_slugs.iter().any(|rendered| rendered == slug),
            supplied,
            "rendered supply disagrees with matching eligibility"
        );
        assert_eq!(memory::semantic_digest(memory), digest);
    };

    // A short evidence-age window is not a short inactivity lifetime.
    // Judgement acceptance is not an at-SHA mechanical verification.
    for window_days in [0, 7, 365] {
        let fresh = Freshness {
            window_days,
            now: accepted_at + 27 * 86_400,
        };
        assert_eq!(
            memory::evidence_json(&accepted, &fresh)["state"].as_str(),
            Some("unverified")
        );
        assert_supply(&accepted, fresh.now, window_days, true);
        assert_supply(&accepted, expiry - 1, window_days, true);
        assert_supply(&accepted, expiry, window_days, false);
        assert_supply(&accepted, expiry + 1, window_days, false);
    }

    // Perturb metadata on a native-qualified record, not its authority:
    // create no receipt, proof, reported-use counter or different claim.
    // A raw verification timestamp must not renew inactivity.
    let mut recently_stamped = accepted.clone();
    recently_stamped.front.verified_at = Some(crate::issue::time::iso(expiry - 1));
    assert_eq!(memory::semantic_digest(&recently_stamped), digest);
    assert!(memory::last_verified(&recently_stamped).is_none());
    assert_supply(&recently_stamped, expiry, 365, false);
    assert_supply(&recently_stamped, expiry + 1, 365, false);

    // Reading, matching and rendering supply no authenticated-use report;
    // expiry withholds but preserves the canonical claim and history.
    assert_eq!(
        std::fs::read(&accepted.path).expect("retained bytes"),
        original_bytes
    );
    let retained = stored(&world, slug);
    assert_eq!(serde_json::to_value(&retained.front).unwrap(), front);
    assert_eq!(retained.body, accepted.body);
    assert_eq!(memory::semantic_digest(&retained), digest);
}

fn cad347_feedback_world() -> (World, String, i64) {
    let world = World::open();
    world.memory_agent("cad347-feedback-pm", "pm");
    world.memory_agent("cad347-feedback-author", "worker");
    for alias in ["cad347-feedback-worker", "cad347-feedback-other"] {
        cad347_vendor_agent(&world, alias, "pi", None);
        cad347_vendor_identity(&world, alias, None, "0123456789abcdef0123456789abcdef");
        world
            .shared
            .store
            .set_params(alias, &json!({"upstream": "cad347-feedback-pm"}))
            .expect("native job-group membership");
    }
    let proposal = cad347_vendor_rpc(
        &world,
        agent("cad347-feedback-author"),
        "memory_propose",
        propose_params(
            "cad347-feedback-lesson",
            "gotcha",
            json!({"project": true}),
            "FEEDBACK FACT — count authenticated reports, not inferred benefit",
        ),
    )
    .expect("native agent judgement proposal");
    let digest = proposal["digest"]
        .as_str()
        .expect("proposal digest")
        .to_string();
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params("cad347-feedback-lesson", "accept", &digest),
    )
    .expect("positive operator judgement approval");

    // Historical-clock fixture ONLY: move timestamps on the ACTUAL
    // native approval. Create no approval, review, usage or check receipt.
    let mut memory = stored(&world, "cad347-feedback-lesson");
    let mut front = serde_json::to_value(&memory.front).unwrap();
    assert_eq!(front["policy"]["revision"].as_u64(), Some(2));
    assert_eq!(front["policy"]["nature"].as_str(), Some("judgement"));
    assert_eq!(
        front["policy"]["provenance"].as_str(),
        Some("agent-claimed")
    );
    assert!(front["policy"]["feedback"]["last_use"].is_null());
    assert_eq!(
        front["policy"]["feedback"]["used_reports"].as_u64(),
        Some(0)
    );
    let acceptances: Vec<usize> = front["policy"]["finalizations"]
        .as_array()
        .expect("native finalization history")
        .iter()
        .enumerate()
        .filter_map(|(i, receipt)| {
            (receipt["operation"].as_str() == Some("accept")
                && receipt["digest"].as_str() == Some(digest.as_str()))
            .then_some(i)
        })
        .collect();
    assert_eq!(acceptances.len(), 1);
    let index = acceptances[0];
    assert_eq!(
        front["policy"]["finalizations"][index]["finalizer"]["kind"].as_str(),
        Some("operator"),
    );
    let old_acceptance = crate::issue::time::now_epoch() - 27 * 86_400;
    front["created"] = json!(crate::issue::time::iso(old_acceptance - 86_400));
    front["policy"]["finalizations"][index]["recorded_at"] =
        json!(crate::issue::time::iso(old_acceptance));
    memory.front = serde_json::from_value(front).expect("historical native front");
    assert_eq!(crate::memory::semantic_digest(&memory), digest);
    let pm = crate::issue::Pm::at(&world.pm()).unwrap();
    {
        // Global lock + existing global commit: no scoped/unscoped mismatch.
        let _lock = pm.lock().expect("historical fixture PM lock");
        let yaml = serde_yaml::to_string(&memory.front).unwrap();
        std::fs::write(&memory.path, format!("---\n{yaml}---\n\n{}", memory.body))
            .expect("write historical timestamp fixture");
        pm.commit(
            std::slice::from_ref(&memory.path),
            "Historical clock fixture for authenticated memory feedback\n\nActor: test\n",
        )
        .expect("commit only historical fixture metadata");
    }
    let loaded = stored(&world, "cad347-feedback-lesson");
    assert_eq!(crate::memory::semantic_digest(&loaded), digest);
    assert!(crate::memory::last_verified(&loaded).is_none());
    (world, digest, old_acceptance + 28 * 86_400)
}

fn cad347_feedback_issue(world: &World, project: &str) -> String {
    let pm = crate::issue::Pm::at(&world.pm()).unwrap();
    let out = crate::issue::write::new_issue(
        &pm,
        &world.checkout(),
        Some(project),
        "Authenticated memory-feedback scope fixture",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "pm",
    )
    .expect("canonical native issue");
    out["id"].as_str().unwrap().to_string()
}

fn cad347_feedback_foreign_project(world: &World) -> String {
    let pm = crate::issue::Pm::at(&world.pm()).unwrap();
    crate::issue::write::project_add(&pm, "cad347-foreign", "FB", &[], &[], &[], None)
        .expect("foreign canonical project");
    cad347_feedback_issue(world, "cad347-foreign")
}

fn cad347_feedback_start(world: &World, alias: &str, message: &str) -> String {
    let row = world.shared.store.agent(alias).unwrap();
    let generation = row
        .generation
        .as_deref()
        .expect("native endpoint generation");
    let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(generation);
    assert!(crate::adapter::registry::turn_token_current(
        &row.provider,
        &row.endpoint_kind,
        Some(generation),
        &token,
    ));
    world
        .shared
        .store
        .mark_running(message, &token)
        .expect("native provider turn-start effect");
    let stored = world.shared.store.message(message).unwrap().unwrap();
    assert_eq!(stored.alias, alias);
    assert_eq!(stored.recipient_uid.as_deref(), Some(row.uid.as_str()));
    assert_eq!(stored.state, "running");
    assert_eq!(stored.turn_id.as_deref(), Some(token.as_str()));
    token
}

fn cad347_feedback_turn(
    world: &World,
    alias: &str,
    message: &str,
    task: Option<&str>,
    issue: Option<&str>,
) -> String {
    let source = if task.is_some() { "send" } else { "dispatch" };
    let (duplicate, _) = world
        .shared
        .store
        .enqueue_steered(
            alias,
            "Native feedback-turn fixture",
            None,
            message,
            source,
            task,
            issue,
            Some(world.checkout().to_str().unwrap()),
            &Sender::Unattributed,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
        )
        .expect("native durable message effect");
    assert!(!duplicate);
    cad347_feedback_start(world, alias, message)
}

fn cad347_feedback_job_turn(world: &World, issue: Option<&str>) -> (String, String, String) {
    let spec = "Exercise authenticated memory feedback on the assigned native task.\n";
    let path = world.root.path().join("cad347-feedback-spec.md");
    std::fs::write(&path, spec).unwrap();
    let spec_digest = format!("{:x}", Sha256::digest(spec.as_bytes()));
    let (duplicate, job) = world
        .shared
        .store
        .create_job(
            "cad347-feedback-job",
            Some("Feedback scope fixture"),
            path.to_str().unwrap(),
            &spec_digest,
            "cad347-feedback-pm",
            issue,
            None,
            None,
            3,
            None,
            None,
            None,
            None,
            None,
            Some("cad347-feedback-worker"),
            Some("Report only authenticated current-scope use"),
        )
        .expect("native open job and initial task");
    assert!(!duplicate);
    assert_eq!(job.state, "open");
    assert_eq!(job.issue_id.as_deref(), issue);
    let task_id = "cad347-feedback-job-t1";
    let (_, message, duplicate, _) = world
        .shared
        .store
        .dispatch_task(task_id, None, None, "cad347-feedback-pm")
        .expect("native current-revision task dispatch");
    assert!(!duplicate);
    let token = cad347_feedback_start(world, "cad347-feedback-worker", &message);
    let task = world.shared.store.task(task_id).unwrap();
    assert_eq!(task.state, "running");
    assert_eq!(task.revision, 1);
    assert_eq!(task.assignee.as_deref(), Some("cad347-feedback-worker"));
    assert_eq!(task.dispatch_message.as_deref(), Some(message.as_str()));
    (task.id, message, token)
}

fn cad347_feedback_params(digest: &str, message: &str, token: &str, kind: &str) -> Value {
    json!({
        "project": PROJECT,
        "slug": "cad347-feedback-lesson",
        "digest": digest,
        "message": message,
        "token": token,
        "kind": kind,
        "evidence": "Explicit feedback report; not a claim of unique benefit",
    })
}

fn cad347_feedback_metadata(world: &World) -> Value {
    let memory = stored(world, "cad347-feedback-lesson");
    let front = serde_json::to_value(&memory.front).unwrap();
    front["policy"]["feedback"].clone()
}

fn cad347_feedback_supplied(world: &World, now: i64) -> bool {
    let memories = crate::memory::load_project(&world.pm(), PROJECT).unwrap();
    let matched = crate::memory::match_memories(
        &memories,
        &crate::memory::MatchCtx::default(),
        &crate::memory::Freshness {
            window_days: 365,
            now,
        },
    );
    let (_, supplied) = crate::memory::render_lessons(&matched);
    supplied.iter().any(|slug| slug == "cad347-feedback-lesson")
}

#[test]
fn cad347_feedback_issue_turn_requires_connection_ownership_currency_and_scope() {
    let (world, digest, old_expiry) = cad347_feedback_world();
    let issue = cad347_feedback_issue(&world, PROJECT);
    let foreign_issue = cad347_feedback_foreign_project(&world);
    let message = "cad347-feedback-owned";
    let token = cad347_feedback_turn(
        &world,
        "cad347-feedback-worker",
        message,
        None,
        Some(&issue),
    );
    let valid = cad347_feedback_params(&digest, message, &token, "used");
    let refuse_feedback = |who: Asserted, params: Value| {
        cad347_vendor_refuse(
            &world,
            "cad347-feedback-lesson",
            who,
            "memory_feedback",
            params,
        );
    };

    // A token with the right generation/scheme is not authentication.
    let row = world.shared.store.agent("cad347-feedback-worker").unwrap();
    let forged =
        crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(row.generation.as_deref().unwrap());
    assert_ne!(forged, token);
    assert!(crate::adapter::registry::turn_token_current(
        &row.provider,
        &row.endpoint_kind,
        row.generation.as_deref(),
        &forged,
    ));
    let mut bad = valid.clone();
    bad["token"] = json!(forged);
    refuse_feedback(agent("cad347-feedback-worker"), bad);
    refuse_feedback(agent("cad347-feedback-other"), valid.clone());
    refuse_feedback(Asserted::Unproven, valid.clone());
    refuse_feedback(Asserted::Operator, valid.clone());
    for (field, value) in [
        ("message", json!("cad347-nonexistent-message")),
        ("digest", json!("0".repeat(64))),
        ("project", json!("cad347-foreign")),
    ] {
        let mut bad = valid.clone();
        bad[field] = value;
        refuse_feedback(agent("cad347-feedback-worker"), bad);
    }
    for (field, value) in [
        ("actor", json!("operator")),
        ("provider", json!("openai")),
        ("receipt", json!({"approved": true})),
        ("timestamp", json!("2099-01-01T00:00:00Z")),
        ("used_reports", json!(999)),
    ] {
        let mut bad = valid.clone();
        bad[field] = value;
        refuse_feedback(agent("cad347-feedback-worker"), bad);
    }

    // Same cwd and request project cannot replace native issue scope.
    for (mid, bound_issue) in [
        (
            "cad347-feedback-foreign-scope",
            Some(foreign_issue.as_str()),
        ),
        ("cad347-feedback-unbound", None),
    ] {
        let scoped_token =
            cad347_feedback_turn(&world, "cad347-feedback-other", mid, None, bound_issue);
        refuse_feedback(
            agent("cad347-feedback-other"),
            cad347_feedback_params(&digest, mid, &scoped_token, "used"),
        );
        let stored = world.shared.store.message(mid).unwrap().unwrap();
        world
            .shared
            .store
            .finish(&stored, "completed", &json!({"text": "fixture done"}), None)
            .expect("finish native negative-scope turn");
    }

    for kind in ["wrong", "reread"] {
        cad347_vendor_rpc(
            &world,
            agent("cad347-feedback-worker"),
            "memory_feedback",
            cad347_feedback_params(&digest, message, &token, kind),
        )
        .expect("authenticated non-use feedback report");
    }
    let feedback = cad347_feedback_metadata(&world);
    assert_eq!(feedback["used_reports"].as_u64(), Some(0));
    assert_eq!(feedback["wrong_reports"].as_u64(), Some(1));
    assert_eq!(feedback["reread_reports"].as_u64(), Some(1));
    assert!(feedback["last_use"].is_null());
    assert!(cad347_feedback_supplied(&world, old_expiry - 1));
    assert!(!cad347_feedback_supplied(&world, old_expiry));

    // Repeated valid reports are reports, not deduplicated benefits.
    for _ in 0..2 {
        cad347_vendor_rpc(
            &world,
            agent("cad347-feedback-worker"),
            "memory_feedback",
            valid.clone(),
        )
        .expect("owned active same-project use report");
    }
    let feedback = cad347_feedback_metadata(&world);
    assert_eq!(feedback["used_reports"].as_u64(), Some(2));
    let use_receipt = feedback["last_use"].clone();
    assert_eq!(use_receipt["digest"].as_str(), Some(digest.as_str()));
    assert_eq!(
        use_receipt["actor"]["alias"].as_str(),
        Some("cad347-feedback-worker")
    );
    assert_eq!(use_receipt["actor"]["uid"].as_str(), Some(row.uid.as_str()));
    assert_eq!(use_receipt["message"].as_str(), Some(message));
    assert!(use_receipt["task"].is_null());
    let used_at = crate::issue::time::parse_iso(
        use_receipt["recorded_at"]
            .as_str()
            .expect("native use timestamp"),
    )
    .unwrap();
    assert!(cad347_feedback_supplied(&world, old_expiry + 1));
    assert!(cad347_feedback_supplied(&world, used_at + 28 * 86_400 - 1));
    assert!(!cad347_feedback_supplied(&world, used_at + 28 * 86_400));
    for kind in ["wrong", "reread"] {
        cad347_vendor_rpc(
            &world,
            agent("cad347-feedback-worker"),
            "memory_feedback",
            cad347_feedback_params(&digest, message, &token, kind),
        )
        .expect("later non-use report");
        assert_eq!(cad347_feedback_metadata(&world)["last_use"], use_receipt);
    }
    let retained = stored(&world, "cad347-feedback-lesson");
    assert_eq!(crate::memory::semantic_digest(&retained), digest);
    assert!(crate::memory::last_verified(&retained).is_none());
    assert!(!std::fs::read_to_string(&retained.path)
        .unwrap()
        .contains(&token));
    assert_eq!(
        cad347_feedback_metadata(&world)["wrong_reports"].as_u64(),
        Some(2)
    );
    assert_eq!(
        cad347_feedback_metadata(&world)["reread_reports"].as_u64(),
        Some(2)
    );

    let stored_turn = world.shared.store.message(message).unwrap().unwrap();
    world
        .shared
        .store
        .finish(
            &stored_turn,
            "completed",
            &json!({"text": "fixture done"}),
            None,
        )
        .expect("native turn completes");
    // Its token still has the current generation, but the turn is over.
    assert!(crate::adapter::registry::turn_token_current(
        &row.provider,
        &row.endpoint_kind,
        row.generation.as_deref(),
        &token,
    ));
    refuse_feedback(agent("cad347-feedback-worker"), valid);

    let stale_mid = "cad347-feedback-stale-generation";
    let stale_token = cad347_feedback_turn(
        &world,
        "cad347-feedback-other",
        stale_mid,
        None,
        Some(&issue),
    );
    cad347_vendor_identity(
        &world,
        "cad347-feedback-other",
        None,
        "fedcba9876543210fedcba9876543210",
    );
    // A genuine unadopted restart fences this turn to unknown. Do not
    // fabricate a still-running old-generation turn or reanimate it.
    let retired = world.shared.store.message(stale_mid).unwrap().unwrap();
    let current = world.shared.store.agent("cad347-feedback-other").unwrap();
    assert_eq!(retired.state, "unknown");
    assert_eq!(retired.turn_id.as_deref(), Some(stale_token.as_str()));
    assert_eq!(retired.recipient_uid.as_deref(), Some(current.uid.as_str()));
    assert_eq!(
        current.generation.as_deref(),
        Some("fedcba9876543210fedcba9876543210")
    );
    assert!(!crate::adapter::registry::turn_token_current(
        &current.provider,
        &current.endpoint_kind,
        current.generation.as_deref(),
        &stale_token,
    ));
    refuse_feedback(
        agent("cad347-feedback-other"),
        cad347_feedback_params(&digest, stale_mid, &stale_token, "used"),
    );
}

#[test]
fn cad347_feedback_job_task_requires_current_dispatch_assignee_and_active_state() {
    for binding in ["same-project", "foreign-project", "unbound"] {
        let (world, digest, old_expiry) = cad347_feedback_world();
        let issue = match binding {
            "same-project" => Some(cad347_feedback_issue(&world, PROJECT)),
            "foreign-project" => Some(cad347_feedback_foreign_project(&world)),
            "unbound" => None,
            _ => unreachable!(),
        };
        let (task_id, message, token) = cad347_feedback_job_turn(&world, issue.as_deref());
        let valid = cad347_feedback_params(&digest, &message, &token, "used");
        if binding != "same-project" {
            cad347_vendor_refuse(
                &world,
                "cad347-feedback-lesson",
                agent("cad347-feedback-worker"),
                "memory_feedback",
                valid,
            );
            assert_eq!(
                cad347_feedback_metadata(&world)["used_reports"].as_u64(),
                Some(0)
            );
            assert!(!cad347_feedback_supplied(&world, old_expiry));
            continue;
        }
        cad347_vendor_rpc(
            &world,
            agent("cad347-feedback-worker"),
            "memory_feedback",
            valid.clone(),
        )
        .expect("native current task and canonical same-project job binding");
        assert_eq!(
            cad347_feedback_metadata(&world)["used_reports"].as_u64(),
            Some(1)
        );
        assert_eq!(
            cad347_feedback_metadata(&world)["last_use"]["task"].as_str(),
            Some(task_id.as_str()),
        );
        assert!(cad347_feedback_supplied(&world, old_expiry + 1));

        // A task-indexed follow-up is not the current task kickoff.
        // The first case IS assigned to the caller; only dispatch
        // correlation distinguishes it from the qualifying kickoff.
        for (alias, mid) in [
            ("cad347-feedback-worker", "cad347-feedback-own-followup"),
            ("cad347-feedback-other", "cad347-feedback-foreign-assignee"),
        ] {
            let followup_token =
                cad347_feedback_turn(&world, alias, mid, Some(&task_id), issue.as_deref());
            assert_eq!(
                world
                    .shared
                    .store
                    .task(&task_id)
                    .unwrap()
                    .dispatch_message
                    .as_deref(),
                Some(message.as_str()),
            );
            cad347_vendor_refuse(
                &world,
                "cad347-feedback-lesson",
                agent(alias),
                "memory_feedback",
                cad347_feedback_params(&digest, mid, &followup_token, "used"),
            );
            let followup = world.shared.store.message(mid).unwrap().unwrap();
            world
                .shared
                .store
                .finish(
                    &followup,
                    "completed",
                    &json!({"text": "fixture followup done"}),
                    None,
                )
                .expect("native follow-up completes without advancing the task");
        }

        // A failed kickoff is retired; genuine redispatch changes the
        // native assignee, revision and dispatch_message together.
        let first_turn = world.shared.store.message(&message).unwrap().unwrap();
        world
            .shared
            .store
            .finish(
                &first_turn,
                "failed",
                &json!({"reason": "fixture retry"}),
                None,
            )
            .expect("native failed turn");
        let (_, next_message, duplicate, _) = world
            .shared
            .store
            .dispatch_task(
                &task_id,
                Some("cad347-feedback-other"),
                None,
                "cad347-feedback-pm",
            )
            .expect("native second-revision reassignment");
        assert!(!duplicate);
        assert_ne!(next_message, message);
        let next_token = cad347_feedback_start(&world, "cad347-feedback-other", &next_message);
        let task = world.shared.store.task(&task_id).unwrap();
        assert_eq!(task.revision, 2);
        assert_eq!(task.assignee.as_deref(), Some("cad347-feedback-other"));
        assert_eq!(
            task.dispatch_message.as_deref(),
            Some(next_message.as_str())
        );
        assert_eq!(task.state, "running");
        cad347_vendor_refuse(
            &world,
            "cad347-feedback-lesson",
            agent("cad347-feedback-worker"),
            "memory_feedback",
            valid,
        );
        let next = cad347_feedback_params(&digest, &next_message, &next_token, "used");
        cad347_vendor_rpc(
            &world,
            agent("cad347-feedback-other"),
            "memory_feedback",
            next.clone(),
        )
        .expect("current assigned revision can report use");
        assert_eq!(
            cad347_feedback_metadata(&world)["used_reports"].as_u64(),
            Some(2)
        );
        assert_eq!(
            cad347_feedback_metadata(&world)["last_use"]["message"].as_str(),
            Some(next_message.as_str()),
        );
        let current_assignee = world.shared.store.agent("cad347-feedback-other").unwrap();
        assert_eq!(
            cad347_feedback_metadata(&world)["last_use"]["actor"]["uid"].as_str(),
            Some(current_assignee.uid.as_str()),
        );

        // Native cancellation deliberately cannot unpaste a running
        // turn: the message stays running, but task authority is gone.
        let cancelled = world
            .shared
            .store
            .cancel_task(&task_id, "cad347-feedback-pm")
            .expect("native task cancellation");
        assert_eq!(cancelled.state, "cancelled");
        assert_eq!(
            world
                .shared
                .store
                .message(&next_message)
                .unwrap()
                .unwrap()
                .state,
            "running"
        );
        assert_eq!(
            world.shared.store.job("cad347-feedback-job").unwrap().state,
            "open"
        );
        cad347_vendor_refuse(
            &world,
            "cad347-feedback-lesson",
            agent("cad347-feedback-other"),
            "memory_feedback",
            next,
        );
        assert_eq!(
            crate::memory::semantic_digest(&stored(&world, "cad347-feedback-lesson")),
            digest
        );
        let bytes = std::fs::read_to_string(stored(&world, "cad347-feedback-lesson").path).unwrap();
        assert!(!bytes.contains(&token));
        assert!(!bytes.contains(&next_token));
    }
}

#[test]
fn cad347_external_source_judgement_requires_operator_release_without_fake_freshness() {
    let world = World::open();
    cad347_vendor_agent(&world, "cad347-author", "inbox", Some("openai/gpt-5"));
    cad347_vendor_agent(
        &world,
        "cad347-curator",
        "inbox",
        Some("anthropic/claude-sonnet-4-5"),
    );
    let slug = "cad347-external-judgement";
    let mut params = propose_params(
        slug,
        "decision",
        json!({}),
        "An external judgement remains unavailable until explicitly released by the operator",
    );
    params["source"] = json!("https://example.invalid/cad347/external-judgement");
    params["nature"] = json!("judgement");
    let proposal = cad347_vendor_rpc(&world, agent("cad347-author"), "memory_propose", params)
        .expect("native external-source proposal without asserted provenance authority");
    let digest = proposal["digest"].as_str().unwrap().to_string();
    let withheld = stored(&world, slug);
    assert_ne!(withheld.front.status, "accepted");
    let front = serde_json::to_value(&withheld.front).unwrap();
    assert_eq!(
        front["policy"]["provenance"].as_str(),
        Some("external-derived")
    );
    assert!(
        !retrieval(&world, slug).0,
        "external judgement must be withheld before operator release"
    );
    assert!(crate::memory::last_verified(&withheld).is_none());
    for who in [
        agent("cad347-author"),
        agent("cad347-curator"),
        Asserted::Unproven,
    ] {
        cad347_vendor_refuse(
            &world,
            slug,
            who,
            "memory_finalize",
            finalize_params(slug, "accept", &digest),
        );
        assert_eq!(digest_of(&world, slug), digest);
        assert!(
            !retrieval(&world, slug).0,
            "refused caller must not release the external judgement"
        );
    }
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    )
    .expect("positively asserted operator releases the withheld external judgement");
    let released = stored(&world, slug);
    assert_eq!(released.front.status, "accepted");
    assert_eq!(digest_of(&world, slug), digest);
    let front = serde_json::to_value(&released.front).unwrap();
    assert_eq!(
        front["policy"]["provenance"].as_str(),
        Some("external-derived")
    );
    assert!(
        retrieval(&world, slug).0,
        "operator release makes the external judgement eligible"
    );
    assert!(
        crate::memory::last_verified(&released).is_none(),
        "operator release of external judgement must not invent mechanical freshness"
    );
}

#[test]
fn cad347_operator_origin_judgement_needs_actual_operator_decision_before_supply() {
    let world = World::open();
    world.memory_agent("cad347-op-worker", "worker");
    world.memory_agent("cad347-op-pm", "pm");
    let slug = "cad347-operator-origin";
    let proposal = cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_propose",
        propose_params(
            slug,
            "rule",
            json!({"project": true}),
            "OPERATOR FACT — origin is not an implicit approval or a worker's authority",
        ),
    )
    .expect("positively proven operator originates a native judgement");
    let digest = proposal["digest"].as_str().unwrap().to_string();
    let candidate = stored(&world, slug);
    let candidate_front = serde_json::to_value(&candidate.front).unwrap();
    assert_eq!(
        candidate_front["policy"]["provenance"].as_str(),
        Some("operator")
    );
    assert_eq!(
        candidate_front["policy"]["nature"].as_str(),
        Some("judgement")
    );
    assert_eq!(
        candidate_front["policy"]["author"]["actor"]["kind"].as_str(),
        Some("operator")
    );
    let operator_uid = candidate_front["policy"]["author"]["actor"]["uid"]
        .as_u64()
        .expect("native operator origin uid");
    assert!(candidate_front["policy"]["finalizations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!crate::memory::retrieval_status(&candidate).0);
    let pm = crate::issue::Pm::at(&world.pm()).unwrap();
    let (before, errors) = crate::memory::project_rules(&pm, PROJECT, None);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!slugs(&before).contains(slug));
    let (_, before_supply) = crate::memory::render_lessons(&before);
    assert!(!before_supply.iter().any(|supplied| supplied == slug));

    for who in [
        agent("cad347-op-worker"),
        agent("cad347-op-pm"),
        Asserted::Unproven,
    ] {
        cad347_vendor_refuse(
            &world,
            slug,
            who,
            "memory_finalize",
            finalize_params(slug, "accept", &digest),
        );
    }
    assert!(!retrieval(&world, slug).0);
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    )
    .expect("positively proven operator records the judgement decision");
    let accepted = stored(&world, slug);
    assert_eq!(crate::memory::semantic_digest(&accepted), digest);
    assert!(crate::memory::retrieval_status(&accepted).0);
    let accepted_front = serde_json::to_value(&accepted.front).unwrap();
    assert!(accepted_front["policy"]["finalizations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|receipt| {
            receipt["operation"].as_str() == Some("accept")
                && receipt["digest"].as_str() == Some(digest.as_str())
                && receipt["status"].as_str() == Some("accepted")
                && receipt["finalizer"]["kind"].as_str() == Some("operator")
                && receipt["finalizer"]["uid"].as_u64() == Some(operator_uid)
        }));
    assert!(accepted.front.verified_at.is_none());
    assert!(crate::memory::last_verified(&accepted).is_none());
    let (after, errors) = crate::memory::project_rules(&pm, PROJECT, None);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(slugs(&after), BTreeSet::from([slug.to_string()]));
    let (_, after_supply) = crate::memory::render_lessons(&after);
    assert_eq!(after_supply, vec![slug.to_string()]);
}

#[test]
fn cad347_native_role_applicability_withholds_unknown_wrong_and_foreign_project() {
    let world = World::open();
    world.memory_agent("cad347-role-author", "worker");
    for (alias, team_role) in [("cad347-role-target", "dev"), ("cad347-role-other", "qa")] {
        world
            .shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "inbox",
                endpoint_kind: "inbox",
                role: "worker",
                cwd: world.root.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: Some(team_role),
                model_policy: None,
            })
            .expect("explicit native target team-role registration");
    }
    let target = world.shared.store.agent("cad347-role-target").unwrap();
    let other = world.shared.store.agent("cad347-role-other").unwrap();
    let target_role = target
        .team_role
        .as_deref()
        .expect("native target has an explicitly assigned team role")
        .to_owned();
    let other_role = other
        .team_role
        .as_deref()
        .expect("native other target has an explicitly assigned team role")
        .to_owned();
    assert_eq!(
        target.role, other.role,
        "authorization roles must not explain applicability"
    );
    assert_ne!(
        target_role, other_role,
        "fixture needs distinct assigned team roles"
    );
    let pm = crate::issue::Pm::at(&world.pm()).unwrap();
    let foreign_key = "cad347-role-foreign";
    crate::issue::write::project_add(&pm, foreign_key, "RF", &[], &[], &[], None)
        .expect("canonical foreign project");
    let role_slug = "cad347-role-restricted";
    let company_slug = "cad347-company-local";
    let foreign_slug = "cad347-foreign-control";
    for (project, slug, audience, roles) in [
        (PROJECT, role_slug, "role", vec![target_role.clone()]),
        (PROJECT, company_slug, "company", Vec::<String>::new()),
        (foreign_key, foreign_slug, "project", Vec::<String>::new()),
    ] {
        let mut params = propose_params(
            slug,
            "rule",
            json!({"project": true}),
            "ROLE FACT — applicability follows native team role and canonical project",
        );
        params["project"] = json!(project);
        params["audience"] = json!(audience);
        params["roles"] = json!(roles);
        let proposal = cad347_vendor_rpc(
            &world,
            agent("cad347-role-author"),
            "memory_propose",
            params,
        )
        .expect("native audience-scoped judgement proposal");
        let digest = proposal["digest"].as_str().unwrap();
        let mut approval = finalize_params(slug, "accept", digest);
        approval["project"] = json!(project);
        cad347_vendor_rpc(&world, Asserted::Operator, "memory_finalize", approval)
            .expect("actual operator judgement approval");
        let memory = crate::memory::load_project(&world.pm(), project)
            .unwrap()
            .into_iter()
            .find(|memory| memory.front.id == slug)
            .unwrap();
        assert_eq!(memory.front.status, "accepted");
        assert_eq!(crate::memory::semantic_digest(&memory), digest);
        assert!(crate::memory::retrieval_status(&memory).0);
        let front = serde_json::to_value(&memory.front).unwrap();
        assert!(front["policy"]["finalizations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|receipt| {
                receipt["operation"].as_str() == Some("accept")
                    && receipt["digest"].as_str() == Some(digest)
                    && receipt["status"].as_str() == Some("accepted")
                    && receipt["finalizer"]["kind"].as_str() == Some("operator")
            }));
    }
    let issue = new_task(&world, "Native team-role applicability task", &[]);
    let assert_projection = |role: Option<&str>, expected: BTreeSet<String>| {
        let (matched, errors) =
            crate::memory::match_for_issue(&pm, &issue, Some(target.provider.as_str()), role)
                .expect("real native-team-role-aware issue match");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(slugs(&matched), expected);
        let (_, rendered) = crate::memory::render_lessons(&matched);
        assert_eq!(rendered.into_iter().collect::<BTreeSet<_>>(), expected);
        let (rules, errors) = crate::memory::project_rules(&pm, PROJECT, role);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(slugs(&rules), expected);
        if role.is_none() {
            assert!(matched
                .withheld
                .iter()
                .any(|(memory, _)| memory.front.id == role_slug));
            assert!(rules
                .withheld
                .iter()
                .any(|(memory, _)| memory.front.id == role_slug));
        }
    };
    let company_only = BTreeSet::from([company_slug.to_string()]);
    assert_projection(None, company_only.clone());
    assert_projection(Some(other_role.as_str()), company_only);
    assert_projection(
        Some(target_role.as_str()),
        BTreeSet::from([role_slug.to_string(), company_slug.to_string()]),
    );

    // Foreign project gets its genuine eligible control, never the
    // originating project's company audience or role-scoped record.
    let out = crate::issue::write::new_issue(
        &pm,
        &world.checkout(),
        Some(foreign_key),
        "Foreign native team-role-consumer task",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "pm",
    )
    .expect("canonical foreign task");
    let foreign_issue = crate::issue::board::find_issue(&world.pm(), out["id"].as_str().unwrap())
        .expect("load canonical foreign task");
    let (foreign, errors) = crate::memory::match_for_issue(
        &pm,
        &foreign_issue,
        Some(target.provider.as_str()),
        Some(target_role.as_str()),
    )
    .expect("foreign project native match");
    assert!(errors.is_empty(), "{errors:?}");
    let expected_foreign = BTreeSet::from([foreign_slug.to_string()]);
    assert_eq!(slugs(&foreign), expected_foreign);
    let (_, rendered) = crate::memory::render_lessons(&foreign);
    assert_eq!(
        rendered.into_iter().collect::<BTreeSet<_>>(),
        expected_foreign
    );
    let (foreign_rules, errors) =
        crate::memory::project_rules(&pm, foreign_key, Some(target_role.as_str()));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(slugs(&foreign_rules), expected_foreign);
}

#[test]
fn cad347_operator_judgement_revalidation_and_invalidation_preserve_history_and_withhold() {
    use crate::memory::{self, policy::ActorSnapshot};

    let world = World::open();
    world.memory_agent("cad347-transition-worker", "worker");
    world.memory_agent("cad347-transition-pm", "pm");
    let slug = "cad347-operator-lifecycle";
    let proposal = cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_propose",
        propose_params(
            slug,
            "rule",
            json!({"project": true}),
            "LIFECYCLE FACT — validity decisions retain the original claim and prior decisions",
        ),
    )
    .expect("native operator judgement candidate");
    let digest = proposal["digest"].as_str().unwrap().to_string();
    let original = stored(&world, slug);
    let original_body = original.body.clone();
    let original_author = original.front.policy.as_ref().unwrap().author.clone();
    let task = new_task(&world, "Consume native lifecycle guidance", &[]);
    let pm = crate::issue::Pm::at(&world.pm()).unwrap();
    let mut mismatched_digest = digest.clone();
    let replacement = if digest.starts_with('0') { "1" } else { "0" };
    mismatched_digest.replace_range(..1, replacement);
    assert_ne!(mismatched_digest, digest);

    let params = |operation: &str, expected: &str, evidence: &str| {
        let mut params = finalize_params(slug, operation, expected);
        params["evidence"] = json!(evidence);
        params
    };
    let assert_state = |status: &str, eligible: bool| -> memory::Memory {
        let memory = stored(&world, slug);
        assert_eq!(memory.front.status, status);
        assert_eq!(memory.body, original_body);
        assert_eq!(memory::semantic_digest(&memory), digest);
        let policy = memory.front.policy.as_ref().expect("retained v2 policy");
        assert_eq!(policy.author, original_author);
        assert_eq!(policy.feedback.used_reports, 0);
        assert!(policy.feedback.last_use.is_none());
        assert_eq!(memory::retrieval_status(&memory).0, eligible);
        assert!(memory.front.verified_at.is_none());
        assert!(memory::last_verified(&memory).is_none());
        let fresh = memory::Freshness::for_key(&world.pm(), PROJECT);
        let detail = memory::detail_json(&memory, &fresh);
        assert_eq!(detail["status"].as_str(), Some(status));
        assert_eq!(detail["body"].as_str(), Some(original_body.as_str()));
        assert_eq!(detail["quorum"]["eligible"].as_bool(), Some(eligible));
        assert_ne!(detail["evidence"]["state"].as_str(), Some("verified"));
        let (matched, errors) = memory::match_for_issue(&pm, &task, Some("inbox"), None)
            .expect("real lifecycle issue match");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            matched.lessons.iter().any(|lesson| lesson.front.id == slug),
            eligible
        );
        let (_, supplied) = memory::render_lessons(&matched);
        assert_eq!(supplied.iter().any(|supplied| supplied == slug), eligible);
        let (rules, errors) = memory::project_rules(&pm, PROJECT, None);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            rules.lessons.iter().any(|lesson| lesson.front.id == slug),
            eligible
        );
        memory
    };

    assert_state("proposed", false);
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    )
    .expect("actual initial operator judgement decision");
    let accepted = assert_state("accepted", true);
    let accepted_history = accepted
        .front
        .policy
        .as_ref()
        .unwrap()
        .finalizations
        .clone();
    assert_eq!(accepted_history.len(), 1);
    assert_eq!(accepted_history[0].operation, "accept");
    assert_eq!(accepted_history[0].digest, digest);
    assert!(matches!(
        &accepted_history[0].finalizer,
        ActorSnapshot::Operator { .. }
    ));

    // These are real endpoint-qualified native agents, not missing
    // identities. Neither PM role nor absent identity is operator proof.
    for operation in ["revalidate", "invalidate"] {
        for who in [
            agent("cad347-transition-worker"),
            agent("cad347-transition-pm"),
            Asserted::Unproven,
        ] {
            cad347_vendor_refuse(
                &world,
                slug,
                who,
                "memory_finalize",
                params(
                    operation,
                    &digest,
                    "Non-operator lifecycle attempt must change nothing",
                ),
            );
        }
        cad347_vendor_refuse(
            &world,
            slug,
            Asserted::Operator,
            "memory_finalize",
            params(
                operation,
                &mismatched_digest,
                "Mismatching expected revision must change nothing",
            ),
        );
        assert_state("accepted", true);
    }

    let revalidation_reason =
        "Operator requests a current policy review after task assumptions changed";
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        params("revalidate", &digest, revalidation_reason),
    )
    .expect("native operator starts revalidation");
    let pending = assert_state("pending-revalidation", false);
    let pending_history = pending.front.policy.as_ref().unwrap().finalizations.clone();
    assert_eq!(pending_history.len(), accepted_history.len() + 1);
    assert_eq!(
        &pending_history[..accepted_history.len()],
        accepted_history.as_slice()
    );
    assert!(pending.front.review_cycle > accepted.front.review_cycle);
    let pending_decision = pending_history.last().unwrap();
    assert_eq!(pending_decision.operation, "revalidate");
    assert_eq!(pending_decision.status, "pending-revalidation");
    assert_eq!(pending_decision.digest, digest);
    assert_eq!(
        pending_decision.evidence.as_deref(),
        Some(revalidation_reason)
    );
    assert!(matches!(
        &pending_decision.finalizer,
        ActorSnapshot::Operator { .. }
    ));

    // The old acceptance receipt still exists but cannot supply the
    // pending record or substitute for the current verify decision.
    cad347_vendor_refuse(
        &world,
        slug,
        Asserted::Operator,
        "memory_finalize",
        finalize_params(slug, "accept", &digest),
    );
    for who in [
        agent("cad347-transition-worker"),
        agent("cad347-transition-pm"),
        Asserted::Unproven,
    ] {
        cad347_vendor_refuse(
            &world,
            slug,
            who,
            "memory_finalize",
            params(
                "verify",
                &digest,
                "Only the actual operator can decide this judgement",
            ),
        );
    }
    cad347_vendor_refuse(
        &world,
        slug,
        Asserted::Operator,
        "memory_finalize",
        params(
            "verify",
            &mismatched_digest,
            "Stale expected revision cannot complete revalidation",
        ),
    );
    assert_state("pending-revalidation", false);

    let review_reason =
        "Operator re-examined the unchanged judgement against the current task assumptions";
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        params("verify", &digest, review_reason),
    )
    .expect("actual current-cycle operator policy decision");
    let restored = assert_state("accepted", true);
    let restored_history = restored
        .front
        .policy
        .as_ref()
        .unwrap()
        .finalizations
        .clone();
    assert_eq!(restored_history.len(), pending_history.len() + 1);
    assert_eq!(
        &restored_history[..pending_history.len()],
        pending_history.as_slice()
    );
    assert_eq!(restored.front.review_cycle, pending.front.review_cycle);
    let review_decision = restored_history.last().unwrap();
    assert_eq!(review_decision.operation, "verify");
    assert_eq!(review_decision.status, "accepted");
    assert_eq!(review_decision.cycle, restored.front.review_cycle);
    assert_eq!(review_decision.digest, digest);
    assert_eq!(review_decision.evidence.as_deref(), Some(review_reason));
    assert!(matches!(
        &review_decision.finalizer,
        ActorSnapshot::Operator { .. }
    ));

    let invalidation_reason = "Operator invalidates the guidance after contrary evidence";
    cad347_vendor_refuse(
        &world,
        slug,
        Asserted::Operator,
        "memory_finalize",
        params("invalidate", &mismatched_digest, invalidation_reason),
    );
    assert_state("accepted", true);
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        params("invalidate", &digest, invalidation_reason),
    )
    .expect("actual current-digest operator invalidation");
    let invalidated = assert_state("invalidated", false);
    let invalidated_history = &invalidated.front.policy.as_ref().unwrap().finalizations;
    assert_eq!(invalidated_history.len(), restored_history.len() + 1);
    assert_eq!(
        &invalidated_history[..restored_history.len()],
        restored_history.as_slice()
    );
    assert!(invalidated.front.review_cycle > restored.front.review_cycle);
    let invalidation = invalidated_history.last().unwrap();
    assert_eq!(invalidation.operation, "invalidate");
    assert_eq!(invalidation.status, "invalidated");
    assert_eq!(invalidation.digest, digest);
    assert_eq!(invalidation.evidence.as_deref(), Some(invalidation_reason));
    assert!(matches!(
        &invalidation.finalizer,
        ActorSnapshot::Operator { .. }
    ));

    for operation in ["accept", "verify", "revalidate"] {
        cad347_vendor_refuse(
            &world,
            slug,
            Asserted::Operator,
            "memory_finalize",
            params(
                operation,
                &digest,
                "Terminal invalidation must not reopen in place",
            ),
        );
    }
    assert_state("invalidated", false);
}

#[test]
fn cad347_operator_rejected_candidate_is_retained_and_cannot_be_resurrected() {
    let world = World::open();
    world.memory_agent("cad347-reject-worker", "worker");
    world.memory_agent("cad347-reject-pm", "pm");
    let slug = "cad347-rejected-candidate";
    let proposal = cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_propose",
        propose_params(
            slug,
            "rule",
            json!({"project": true}),
            "REJECTED FACT — retain the candidate without ever supplying it",
        ),
    )
    .expect("actual native judgement candidate");
    let digest = proposal["digest"].as_str().unwrap().to_string();
    let candidate = stored(&world, slug);
    assert_eq!(candidate.front.status, "proposed");
    assert!(!retrieval(&world, slug).0);
    assert!(candidate
        .front
        .policy
        .as_ref()
        .unwrap()
        .finalizations
        .is_empty());
    let reason = "Operator rejects the candidate because its evidence does not support the claim";
    let rejection_params = |expected: &str| {
        let mut params = finalize_params(slug, "reject", expected);
        params["evidence"] = json!(reason);
        params
    };
    for who in [
        agent("cad347-reject-worker"),
        agent("cad347-reject-pm"),
        Asserted::Unproven,
    ] {
        cad347_vendor_refuse(
            &world,
            slug,
            who,
            "memory_finalize",
            rejection_params(&digest),
        );
    }
    let mut mismatched_digest = digest.clone();
    let replacement = if digest.starts_with('0') { "1" } else { "0" };
    mismatched_digest.replace_range(..1, replacement);
    cad347_vendor_refuse(
        &world,
        slug,
        Asserted::Operator,
        "memory_finalize",
        rejection_params(&mismatched_digest),
    );
    cad347_vendor_rpc(
        &world,
        Asserted::Operator,
        "memory_finalize",
        rejection_params(&digest),
    )
    .expect("actual current-digest operator rejection");
    let rejected = stored(&world, slug);
    assert_eq!(rejected.front.status, "rejected");
    assert_eq!(rejected.body, candidate.body);
    assert_eq!(crate::memory::semantic_digest(&rejected), digest);
    assert!(!retrieval(&world, slug).0);
    let history = &rejected.front.policy.as_ref().unwrap().finalizations;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].operation, "reject");
    assert_eq!(history[0].status, "rejected");
    assert_eq!(history[0].digest, digest);
    assert_eq!(history[0].evidence.as_deref(), Some(reason));
    assert!(matches!(
        &history[0].finalizer,
        crate::memory::policy::ActorSnapshot::Operator { .. },
    ));
    for operation in ["accept", "verify", "revalidate"] {
        let mut params = finalize_params(slug, operation, &digest);
        params["evidence"] = json!("Rejected candidate must not be resurrected in place");
        cad347_vendor_refuse(&world, slug, Asserted::Operator, "memory_finalize", params);
    }
    let pm = crate::issue::Pm::at(&world.pm()).unwrap();
    let task = new_task(&world, "Rejected guidance remains unavailable", &[]);
    let (matched, errors) = crate::memory::match_for_issue(&pm, &task, Some("inbox"), None)
        .expect("real rejected-record match");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!slugs(&matched).contains(slug));
    let (_, supplied) = crate::memory::render_lessons(&matched);
    assert!(!supplied.iter().any(|supplied| supplied == slug));
    let (rules, errors) = crate::memory::project_rules(&pm, PROJECT, None);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!slugs(&rules).contains(slug));
    let retained = stored(&world, slug);
    assert_eq!(retained.body, candidate.body);
    assert_eq!(
        retained.front.policy.as_ref().unwrap().finalizations,
        *history
    );
}
