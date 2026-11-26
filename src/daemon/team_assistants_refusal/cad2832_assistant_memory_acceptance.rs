//! CAD-2832 independent acceptance tests for personal assistant working memory,
//! rich `SOUL.md` updates, permitted skill reads, turn-mode gating, security
//! confinement, and 3-layer context delivery.
//!
//! Authored strictly from the `CAD-2832` ticket requirements and the public
//! `Fx` / `Shared` interfaces by an independent fresh-context session. The
//! implementer must not edit or weaken this file.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

static MSG_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_id(prefix: &str) -> String {
    let n = MSG_SEQ.fetch_add(1, AtomicOrdering::Relaxed);
    format!("{prefix}-{n}")
}

struct TurnStamp {
    msg_id: String,
    token: String,
}

fn set_agent_generation(fx: &Fx, alias: &str, gen_hex: &str) {
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET generation=?, state='idle', params=? WHERE alias=?",
                rusqlite::params![
                    gen_hex,
                    json!({"report_timeout_secs": 3600}).to_string(),
                    alias
                ],
            )?;
            Ok(())
        })
        .unwrap();
}

fn finish_turn(fx: &Fx, msg_id: &str) {
    let msg = fx
        .shared
        .store
        .message(msg_id)
        .unwrap()
        .expect("turn message missing");
    fx.shared
        .store
        .finish(
            &msg,
            "completed",
            &json!({"status": "completed", "text": "done"}),
            None,
        )
        .unwrap();
}

fn finish_any_running_messages(fx: &Fx, alias: &str) {
    let msgs = fx.shared.store.messages(alias).unwrap();
    for m in msgs {
        if m.state == "running" {
            let _ = fx.shared.store.finish(
                &m,
                "completed",
                &json!({"status": "completed", "text": "done"}),
                None,
            );
            if fx
                .shared
                .store
                .message(&m.id)
                .unwrap()
                .is_some_and(|cur| cur.state == "running")
            {
                fx.shared
                    .store
                    .fixture_write(|tx| {
                        tx.execute(
                            "UPDATE messages SET state='done' WHERE id=?",
                            rusqlite::params![m.id],
                        )?;
                        Ok(())
                    })
                    .unwrap();
            }
        }
    }
}

fn start_interactive_turn(fx: &Fx, sub: &str, alias: &str, body: &str) -> TurnStamp {
    finish_any_running_messages(fx, alias);
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, sub)
        .unwrap()
        .expect("assistant binding missing");
    let gen_hex = "0123456789abcdef0123456789abcdef";
    set_agent_generation(fx, alias, gen_hex);
    let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(gen_hex);
    let msg_id = next_id("cad2832-interactive");
    fx.shared
        .store
        .enqueue_sent(
            alias,
            body,
            None,
            &msg_id,
            &format!("member-{sub}"),
            None,
            &crate::store::Sender::VerifiedMember(crate::store::MemberAuthority {
                org: ORG.to_string(),
                sub: sub.to_string(),
                assistant_id: binding.assistant_id,
                credential_id: format!("cred-{sub}"),
            }),
        )
        .unwrap();
    fx.shared.store.mark_running(&msg_id, &token).unwrap();
    TurnStamp { msg_id, token }
}

fn snapshot_assistant_files(cwd: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path)
                    .map(|p| p.to_string_lossy().into_owned().into_bytes())
                    .unwrap_or_default();
                out.insert(format!("symlink:{rel}"), target);
            } else if meta.is_dir() {
                out.insert(format!("dir:{rel}/"), Vec::new());
                walk(base, &path, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(cwd, cwd, &mut out);
    out
}

fn read_text_field(val: &Value) -> String {
    val.get("content")
        .or_else(|| val.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected content/text string in response: {val}"))
        .to_string()
}

fn extract_history_id(val: &Value, context: &str) -> String {
    val.get("history_id")
        .or_else(|| val.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{context}: missing history_id in {val}"))
}

#[test]
fn cad2832_interactive_happy_paths_and_undo() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let sub = "sub-cad2832-happy";
    let rt = fx.member_runtime(sub);
    let cwd = fx.shared.assistant_principal_cwd(ORG, sub);

    let _turn = start_interactive_turn(&fx, sub, &rt.alias, "Set up my preferences and notes");

    // 1. update_soul (multi-line soul + optional tone)
    let multi_soul =
        "## Working Norms\n- Lead with concrete next steps.\n- Never guess unverified metrics.\n";
    let soul_res = fx
        .as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({
                "op": "update_soul",
                "soul": multi_soul,
                "tone": "crisp and pragmatic"
            }),
        )
        .expect("interactive update_soul failed");
    let soul_history_id = extract_history_id(&soul_res, "update_soul");
    assert!(
        !soul_history_id.is_empty(),
        "update_soul returned empty history_id: {soul_res}"
    );
    let soul_on_disk = std::fs::read_to_string(cwd.join("SOUL.md")).expect("SOUL.md missing");
    assert!(
        soul_on_disk.contains("Lead with concrete next steps.")
            && soul_on_disk.contains("Never guess unverified metrics.")
            && soul_on_disk.contains("crisp and pragmatic"),
        "SOUL.md missing updated body/tone: {soul_on_disk}"
    );

    // 2. remember + deduplication of identical normalized facts
    let fact_a = "User prefers UTC timestamps in weekly digests";
    let fact_b = "Primary workspace timezone is Asia/Hong_Kong";
    let rem_a = fx
        .as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({"op": "remember", "fact": fact_a}),
        )
        .expect("interactive remember fact_a failed");
    let _rem_a_id = extract_history_id(&rem_a, "remember fact_a");

    let mem_after_a =
        std::fs::read_to_string(cwd.join("MEMORY.md")).expect("MEMORY.md missing after remember");
    assert!(
        mem_after_a.contains(fact_a),
        "MEMORY.md missing fact_a: {mem_after_a}"
    );
    // Dated bullet check (YYYY-MM-DD prefix on the bullet line).
    assert!(
        mem_after_a
            .lines()
            .any(|l| l.starts_with("- ") && l.contains("20") && l.contains(fact_a)),
        "MEMORY.md fact_a is not a dated bullet: {mem_after_a}"
    );

    // Duplicate normalized fact must deduplicate cleanly.
    let _rem_dup = fx
        .as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({"op": "remember", "fact": "  user prefers UTC timestamps in weekly digests  "}),
        )
        .expect("deduplicating identical normalized fact should succeed idempotently");
    let mem_after_dup = std::fs::read_to_string(cwd.join("MEMORY.md")).unwrap();
    let count_fact_a = mem_after_dup
        .lines()
        .filter(|l| {
            l.to_lowercase()
                .contains("utc timestamps in weekly digests")
        })
        .count();
    assert_eq!(
        count_fact_a, 1,
        "identical normalized fact was duplicated in MEMORY.md: {mem_after_dup}"
    );

    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "remember", "fact": fact_b}),
    )
    .expect("interactive remember fact_b failed");

    // 3. write_topic (memory/<topic>.md)
    let topic_res = fx
        .as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({
                "op": "write_topic",
                "topic": "q4-roadmap",
                "content": "# Q4 Roadmap\n- Ship Atlas billing v2\n- Freeze schema on Dec 1\n"
            }),
        )
        .expect("interactive write_topic failed");
    assert!(
        topic_res.is_object(),
        "write_topic did not return object: {topic_res}"
    );
    let topic_on_disk =
        std::fs::read_to_string(cwd.join("memory/q4-roadmap.md")).expect("topic file missing");
    assert!(
        topic_on_disk.contains("Ship Atlas billing v2"),
        "topic file missing expected content: {topic_on_disk}"
    );

    // 4. assistant_memory_search & assistant_memory_read
    let search_mem = fx
        .as_assistant(
            &rt.alias,
            "assistant_memory_search",
            json!({"query": "UTC timestamps", "limit": 10}),
        )
        .expect("assistant_memory_search on MEMORY.md failed");
    assert!(
        search_mem.to_string().contains("UTC timestamps"),
        "search did not return MEMORY.md hit: {search_mem}"
    );
    let search_topic = fx
        .as_assistant(
            &rt.alias,
            "assistant_memory_search",
            json!({"query": "Atlas billing", "limit": 10}),
        )
        .expect("assistant_memory_search on topic failed");
    assert!(
        search_topic.to_string().contains("Atlas billing")
            && search_topic.to_string().contains("q4-roadmap.md"),
        "search did not return topic hit: {search_topic}"
    );

    let read_mem = fx
        .as_assistant(
            &rt.alias,
            "assistant_memory_read",
            json!({"file": "MEMORY.md"}),
        )
        .expect("assistant_memory_read MEMORY.md failed");
    assert!(read_text_field(&read_mem).contains(fact_a));

    let read_soul = fx
        .as_assistant(
            &rt.alias,
            "assistant_memory_read",
            json!({"file": "SOUL.md"}),
        )
        .expect("assistant_memory_read SOUL.md failed");
    assert!(read_text_field(&read_soul).contains("Lead with concrete next steps."));

    let read_topic = fx
        .as_assistant(
            &rt.alias,
            "assistant_memory_read",
            json!({"file": "memory/q4-roadmap.md"}),
        )
        .expect("assistant_memory_read topic failed");
    assert!(read_text_field(&read_topic).contains("Ship Atlas billing v2"));

    // 5. forget & undo + stale CAS/undo rejection
    let forget_res = fx
        .as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({"op": "forget", "query": "UTC timestamps"}),
        )
        .expect("interactive forget failed");
    let forget_history_id = extract_history_id(&forget_res, "forget");
    let mem_after_forget = std::fs::read_to_string(cwd.join("MEMORY.md")).unwrap();
    assert!(
        !mem_after_forget.contains(fact_a) && mem_after_forget.contains(fact_b),
        "forget did not selectively remove fact_a: {mem_after_forget}"
    );

    fx.as_assistant(
        &rt.alias,
        "assistant_memory_undo",
        json!({"history_id": forget_history_id}),
    )
    .expect("assistant_memory_undo of forget failed");
    let mem_after_undo = std::fs::read_to_string(cwd.join("MEMORY.md")).unwrap();
    assert!(
        mem_after_undo.contains(fact_a) && mem_after_undo.contains(fact_b),
        "undo did not restore forgotten fact_a: {mem_after_undo}"
    );

    // Replaying the already-applied undo (stale undo) must be refused without mutating files.
    let before_stale_undo = snapshot_assistant_files(&cwd);
    let stale_undo = fx.as_assistant(
        &rt.alias,
        "assistant_memory_undo",
        json!({"history_id": forget_history_id}),
    );
    let _ = err_text(stale_undo, "replayed/stale assistant_memory_undo");
    assert_eq!(
        snapshot_assistant_files(&cwd),
        before_stale_undo,
        "stale undo mutated files"
    );

    // Undo of an older entry whose target file was subsequently modified (stale CAS)
    // or unknown history_id must also refuse cleanly without mutating files.
    let stale_soul_undo = fx.as_assistant(
        &rt.alias,
        "assistant_memory_undo",
        json!({"history_id": "mem_nonexistent_0000"}),
    );
    let _ = err_text(stale_soul_undo, "unknown history_id undo");
    assert_eq!(
        snapshot_assistant_files(&cwd),
        before_stale_undo,
        "unknown history_id undo mutated files"
    );

    // 6. assistant_skill_show (assistant-memory / SKILL.md)
    let skill_file = fx
        .as_assistant(
            &rt.alias,
            "assistant_skill_show",
            json!({"name": "assistant-memory", "file": "SKILL.md"}),
        )
        .expect("assistant_skill_show assistant-memory SKILL.md failed");
    let skill_text = read_text_field(&skill_file);
    assert!(
        !skill_text.trim().is_empty(),
        "assistant_skill_show returned empty SKILL.md"
    );
}

#[test]
fn cad2832_turn_mode_gating_job_event_nudge_and_unstamped() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let sub = "sub-cad2832-turnmode";
    let rt = fx.member_runtime(sub);
    let cwd = fx.shared.assistant_principal_cwd(ORG, sub);

    // Seed initial SOUL.md and MEMORY.md during an interactive turn so we have
    // real files + a real history_id to probe refusals against.
    let t_init = start_interactive_turn(&fx, sub, &rt.alias, "seed memory and soul");
    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "update_soul", "soul": "Initial working style.\n"}),
    )
    .expect("seed update_soul failed");
    let seed_rem = fx
        .as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({"op": "remember", "fact": "Existing baseline fact"}),
        )
        .expect("seed remember failed");
    let seed_history_id = extract_history_id(&seed_rem, "seed remember");
    finish_turn(&fx, &t_init.msg_id);

    // 1. Real `assist_job_event` turn (minted via promote_member_fixture_job + route_assist_job_notifications)
    let req = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(sub),
            "title": "cad2832 job event gating",
            "request_key": "req-1",
            "issue": FIXTURE_ISSUE,
            "body": "Run diagnostics"
        }),
    );
    let _job = promote_member_fixture_job(&fx, sub, &req);
    let queued = fx
        .shared
        .route_assist_job_notifications()
        .expect("route_assist_job_notifications failed");
    assert!(queued >= 1, "expected at least one assist_job_event notice");

    let job_event_msg = fx
        .shared
        .store
        .messages(&rt.alias)
        .unwrap()
        .into_iter()
        .find(|m| m.source == "assist_job_event")
        .expect("no assist_job_event message queued for assistant");

    finish_any_running_messages(&fx, &rt.alias);
    let gen_hex = "0123456789abcdef0123456789abcdef";
    set_agent_generation(&fx, &rt.alias, gen_hex);
    let job_token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(gen_hex);
    fx.shared
        .store
        .mark_running(&job_event_msg.id, &job_token)
        .unwrap();

    let soul_before_job = std::fs::read(cwd.join("SOUL.md")).unwrap();
    let mem_before_job = std::fs::read(cwd.join("MEMORY.md")).unwrap();

    // Job event turns MUST refuse update_soul, forget, and undo.
    let _ = err_text(
        fx.as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({"op": "update_soul", "soul": "Hijacked by job event"}),
        ),
        "job_event update_soul",
    );
    let _ = err_text(
        fx.as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({"op": "forget", "query": "Existing baseline"}),
        ),
        "job_event forget",
    );
    let _ = err_text(
        fx.as_assistant(
            &rt.alias,
            "assistant_memory_undo",
            json!({"history_id": seed_history_id}),
        ),
        "job_event undo",
    );
    assert_eq!(
        std::fs::read(cwd.join("SOUL.md")).unwrap(),
        soul_before_job,
        "refused job_event mutations modified SOUL.md"
    );
    assert_eq!(
        std::fs::read(cwd.join("MEMORY.md")).unwrap(),
        mem_before_job,
        "refused job_event mutations modified MEMORY.md"
    );

    // Job event turns MUST allow remember (stamped ` · from: job_event`) and
    // write_topic (`"source": "job_event"` + `> Provenance: job_event`).
    let job_rem = fx
        .as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({"op": "remember", "fact": "Job event observed release v2.4"}),
        )
        .expect("job_event remember should be allowed");
    assert!(job_rem.is_object(), "job_event remember result: {job_rem}");
    let mem_after_job_rem = std::fs::read_to_string(cwd.join("MEMORY.md")).unwrap();
    assert!(
        mem_after_job_rem
            .lines()
            .any(|l| l.contains("Job event observed release v2.4")
                && l.contains(" · from: job_event")),
        "job_event remember did not stamp ` · from: job_event`: {mem_after_job_rem}"
    );

    let job_topic = fx
        .as_assistant(
            &rt.alias,
            "assistant_context_update",
            json!({
                "op": "write_topic",
                "topic": "release-notes",
                "content": "# Release Notes\n- Deployed v2.4\n"
            }),
        )
        .expect("job_event write_topic should be allowed");
    assert_eq!(
        job_topic["source"],
        json!("job_event"),
        "job_event write_topic did not report source=job_event: {job_topic}"
    );
    let topic_on_disk = std::fs::read_to_string(cwd.join("memory/release-notes.md")).unwrap();
    assert!(
        topic_on_disk.contains("> Provenance: job_event")
            && topic_on_disk.contains("Deployed v2.4"),
        "job_event write_topic missing `> Provenance: job_event`: {topic_on_disk}"
    );

    // 2. Synthetic nudge turn (`source: "nudge"`): MUST refuse ALL mutations.
    finish_any_running_messages(&fx, &rt.alias);
    set_agent_generation(&fx, &rt.alias, gen_hex);
    let nudge_token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(gen_hex);
    let nudge_id = next_id("cad2832-nudge");
    fx.shared
        .store
        .enqueue(&rt.alias, "periodic nudge check", None, &nudge_id, "nudge")
        .unwrap();
    fx.shared
        .store
        .mark_running(&nudge_id, &nudge_token)
        .unwrap();

    let before_nudge = snapshot_assistant_files(&cwd);
    for (label, method, params) in [
        (
            "nudge remember",
            "assistant_context_update",
            json!({"op": "remember", "fact": "Nudge injected fact"}),
        ),
        (
            "nudge write_topic",
            "assistant_context_update",
            json!({"op": "write_topic", "topic": "nudge-topic", "content": "nudge"}),
        ),
        (
            "nudge update_soul",
            "assistant_context_update",
            json!({"op": "update_soul", "soul": "Nudge soul"}),
        ),
        (
            "nudge forget",
            "assistant_context_update",
            json!({"op": "forget", "query": "Existing baseline"}),
        ),
        (
            "nudge undo",
            "assistant_memory_undo",
            json!({"history_id": seed_history_id}),
        ),
    ] {
        let _ = err_text(fx.as_assistant(&rt.alias, method, params), label);
    }
    assert_eq!(
        snapshot_assistant_files(&cwd),
        before_nudge,
        "nudge turn mutated files"
    );

    // 3a. Unstamped / no running turn (idle agent with only queued message): MUST refuse ALL mutations.
    finish_any_running_messages(&fx, &rt.alias);
    let queued_only_id = next_id("cad2832-queued");
    fx.shared
        .store
        .enqueue_sent(
            &rt.alias,
            "not running yet",
            None,
            &queued_only_id,
            &format!("member-{sub}"),
            None,
            &crate::store::Sender::VerifiedMember(crate::store::MemberAuthority {
                org: ORG.to_string(),
                sub: sub.to_string(),
                assistant_id: format!("assist_{sub}"),
                credential_id: format!("cred-{sub}"),
            }),
        )
        .unwrap();
    let before_idle = snapshot_assistant_files(&cwd);
    for (label, method, params) in [
        (
            "idle remember",
            "assistant_context_update",
            json!({"op": "remember", "fact": "Idle fact"}),
        ),
        (
            "idle write_topic",
            "assistant_context_update",
            json!({"op": "write_topic", "topic": "idle-topic", "content": "idle"}),
        ),
        (
            "idle update_soul",
            "assistant_context_update",
            json!({"op": "update_soul", "soul": "Idle soul"}),
        ),
        (
            "idle forget",
            "assistant_context_update",
            json!({"op": "forget", "query": "Existing baseline"}),
        ),
        (
            "idle undo",
            "assistant_memory_undo",
            json!({"history_id": seed_history_id}),
        ),
    ] {
        let _ = err_text(fx.as_assistant(&rt.alias, method, params), label);
    }
    assert_eq!(
        snapshot_assistant_files(&cwd),
        before_idle,
        "idle unstamped state mutated files"
    );

    // 3b. Running message without a VerifiedMember stamp (source "cli", Sender::Operator):
    // MUST refuse ALL mutations.
    let unstamped_msg = next_id("cad2832-unstamped");
    fx.shared
        .store
        .enqueue_sent(
            &rt.alias,
            "unstamped cli message",
            None,
            &unstamped_msg,
            "cli",
            None,
            &crate::store::Sender::Operator,
        )
        .unwrap();
    let unstamped_token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(gen_hex);
    fx.shared
        .store
        .mark_running(&unstamped_msg, &unstamped_token)
        .unwrap();
    for (label, method, params) in [
        (
            "unstamped remember",
            "assistant_context_update",
            json!({"op": "remember", "fact": "Unstamped fact"}),
        ),
        (
            "unstamped write_topic",
            "assistant_context_update",
            json!({"op": "write_topic", "topic": "unstamped-topic", "content": "x"}),
        ),
        (
            "unstamped update_soul",
            "assistant_context_update",
            json!({"op": "update_soul", "soul": "Unstamped soul"}),
        ),
        (
            "unstamped forget",
            "assistant_context_update",
            json!({"op": "forget", "query": "Existing baseline"}),
        ),
        (
            "unstamped undo",
            "assistant_memory_undo",
            json!({"history_id": seed_history_id}),
        ),
    ] {
        let _ = err_text(fx.as_assistant(&rt.alias, method, params), label);
    }
    assert_eq!(
        snapshot_assistant_files(&cwd),
        before_idle,
        "forged-token turn mutated files"
    );

    // 3c. Foreign-stamped running turn (VerifiedMember belongs to another principal): MUST refuse ALL mutations.
    finish_any_running_messages(&fx, &rt.alias);
    set_agent_generation(&fx, &rt.alias, gen_hex);
    let foreign_token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(gen_hex);
    let foreign_stamp_msg = next_id("cad2832-foreign-stamp");
    fx.shared
        .store
        .enqueue_sent(
            &rt.alias,
            "foreign stamped",
            None,
            &foreign_stamp_msg,
            "member-sub-foreign",
            None,
            &crate::store::Sender::VerifiedMember(crate::store::MemberAuthority {
                org: ORG.to_string(),
                sub: "sub-foreign-principal".to_string(),
                assistant_id: "assist_sub-foreign-principal".to_string(),
                credential_id: "cred-sub-foreign-principal".to_string(),
            }),
        )
        .unwrap();
    fx.shared
        .store
        .mark_running(&foreign_stamp_msg, &foreign_token)
        .unwrap();
    for (label, method, params) in [
        (
            "foreign-stamped remember",
            "assistant_context_update",
            json!({"op": "remember", "fact": "Foreign stamped fact"}),
        ),
        (
            "foreign-stamped write_topic",
            "assistant_context_update",
            json!({"op": "write_topic", "topic": "foreign-topic", "content": "x"}),
        ),
        (
            "foreign-stamped update_soul",
            "assistant_context_update",
            json!({"op": "update_soul", "soul": "Foreign soul"}),
        ),
        (
            "foreign-stamped forget",
            "assistant_context_update",
            json!({"op": "forget", "query": "Existing baseline"}),
        ),
        (
            "foreign-stamped undo",
            "assistant_memory_undo",
            json!({"history_id": seed_history_id}),
        ),
    ] {
        let _ = err_text(fx.as_assistant(&rt.alias, method, params), label);
    }
    assert_eq!(
        snapshot_assistant_files(&cwd),
        before_idle,
        "foreign-stamped turn mutated files"
    );
}

#[test]
fn cad2832_security_confinement_secrets_and_immutability_refusals() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let victim_sub = "sub-cad2832-victim";
    let thief_sub = "sub-cad2832-thief";
    let victim = fx.member_runtime(victim_sub);
    let thief = fx.member_runtime(thief_sub);
    let cwd = fx.shared.assistant_principal_cwd(ORG, victim_sub);

    // Seed baseline state during a valid interactive turn so SOUL.md, MEMORY.md,
    // a topic, and a history_id exist.
    let _turn = start_interactive_turn(&fx, victim_sub, &victim.alias, "interactive baseline");
    let soul_res = fx
        .as_assistant(
            &victim.alias,
            "assistant_context_update",
            json!({"op": "update_soul", "soul": "Trusted baseline soul\n", "tone": "steady"}),
        )
        .expect("baseline update_soul failed");
    let current_sha = soul_res
        .get("sha")
        .or_else(|| soul_res.get("soul_sha"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let rem_res = fx
        .as_assistant(
            &victim.alias,
            "assistant_context_update",
            json!({"op": "remember", "fact": "Baseline safe memory entry"}),
        )
        .expect("baseline remember failed");
    let history_id = extract_history_id(&rem_res, "baseline remember");
    fx.as_assistant(
        &victim.alias,
        "assistant_context_update",
        json!({"op": "write_topic", "topic": "notes", "content": "# Notes\n- safe line\n"}),
    )
    .expect("baseline write_topic failed");

    let before_all = snapshot_assistant_files(&cwd);
    let agents_before = std::fs::read(cwd.join("AGENTS.md")).expect("AGENTS.md missing");
    let mem_before = std::fs::read(cwd.join("MEMORY.md")).unwrap();
    let soul_before = std::fs::read(cwd.join("SOUL.md")).unwrap();

    // 1. Operator caller (`fx.op_err`), foreign assistant (`thief`), and ordinary
    // worker must be refused on all 5 `assistant_*` RPCs.
    let all_five_calls = [
        (
            "assistant_context_update",
            json!({"op": "remember", "fact": "unauthorized"}),
        ),
        ("assistant_memory_search", json!({"query": "Baseline"})),
        ("assistant_memory_read", json!({"file": "MEMORY.md"})),
        ("assistant_memory_undo", json!({"history_id": history_id})),
        (
            "assistant_skill_show",
            json!({"name": "assistant-memory", "file": "SKILL.md"}),
        ),
    ];
    for (method, params) in &all_five_calls {
        let _ = err_text(
            fx.op_err(method, params.clone()),
            &format!("operator calling {method}"),
        );
        let _ = err_text(
            fx.as_assistant(&victim.worker, method, params.clone()),
            &format!("ordinary worker calling {method}"),
        );
    }

    // Foreign assistant (`thief`) refused on all 5 RPCs targeting victim's
    // surface or files, leaving both assistants' files untouched.
    let thief_cwd = fx.shared.assistant_principal_cwd(ORG, thief_sub);
    let thief_before = snapshot_assistant_files(&thief_cwd);
    for (method, params) in [
        (
            "assistant_context_update",
            json!({"op": "remember", "fact": "thief write", "alias": victim.alias}),
        ),
        (
            "assistant_memory_search",
            json!({"query": "Baseline", "alias": victim.alias}),
        ),
        ("assistant_memory_read", json!({"file": "memory/notes.md"})),
        (
            "assistant_memory_read",
            json!({"file": "MEMORY.md", "alias": victim.alias}),
        ),
        ("assistant_memory_undo", json!({"history_id": history_id})),
        (
            "assistant_skill_show",
            json!({"name": "assistant-memory", "file": "SKILL.md", "alias": victim.alias}),
        ),
    ] {
        let _ = err_text(
            fx.as_assistant(&thief.alias, method, params),
            &format!("thief calling {method}"),
        );
    }
    assert_eq!(snapshot_assistant_files(&cwd), before_all);
    assert_eq!(snapshot_assistant_files(&thief_cwd), thief_before);

    // 2. Caller-supplied identity fields (`alias`, `to`, `agent`, `org`, `sub`,
    // `user`, `assistant_id`) must be refused BEFORE execution.
    for key in ["alias", "to", "agent", "org", "sub", "user", "assistant_id"] {
        let own_val = match key {
            "alias" | "to" | "agent" => victim.alias.clone(),
            "org" => ORG.to_string(),
            "sub" | "user" => victim_sub.to_string(),
            "assistant_id" => format!("assist_{victim_sub}"),
            _ => unreachable!(),
        };
        for (method, mut base_params) in [
            (
                "assistant_context_update",
                json!({"op": "remember", "fact": "identity param must refuse"}),
            ),
            ("assistant_memory_search", json!({"query": "Baseline"})),
            ("assistant_memory_read", json!({"file": "MEMORY.md"})),
            ("assistant_memory_undo", json!({"history_id": history_id})),
            (
                "assistant_skill_show",
                json!({"name": "assistant-memory", "file": "SKILL.md"}),
            ),
        ] {
            base_params[key] = json!(own_val);
            let e = err_text(
                fx.as_assistant(&victim.alias, method, base_params),
                &format!("{method} with caller-supplied identity field {key}"),
            );
            assert!(
                e.contains("identity") && e.contains("connection"),
                "{method} with {key} refused for unexpected reason (must refuse before execution): {e}"
            );
        }
    }
    assert_eq!(
        snapshot_assistant_files(&cwd),
        before_all,
        "identity-field probe mutated files"
    );

    // 3. Unlisted skill (`website-builder`) & skill path traversal refused.
    for params in [
        json!({"name": "website-builder"}),
        json!({"name": "website-builder", "file": "SKILL.md"}),
        json!({"name": "assistant-memory", "file": "../SOUL.md"}),
    ] {
        let _ = err_text(
            fx.as_assistant(&victim.alias, "assistant_skill_show", params.clone()),
            &format!("unlisted/traversal skill show {params}"),
        );
    }

    // 4. Secret payloads refused in BOTH MEMORY.md (`remember`) AND SOUL.md (`update_soul`),
    // constructed dynamically at runtime (never static literals).
    let ghp_secret = format!("ghp_{}", "A1b2C3d4E5".repeat(4).get(..36).unwrap());
    let aws_secret = ["AK", "IA", &"A2B3C4D5".repeat(2)].concat();

    for secret in [&ghp_secret, &aws_secret] {
        let _ = err_text(
            fx.as_assistant(
                &victim.alias,
                "assistant_context_update",
                json!({"op": "remember", "fact": format!("Use token {secret} for deploy")}),
            ),
            "remember with secret payload",
        );
        let _ = err_text(
            fx.as_assistant(
                &victim.alias,
                "assistant_context_update",
                json!({"op": "update_soul", "soul": format!("Auth key: {secret}\n")}),
            ),
            "update_soul soul body with secret payload",
        );
        let _ = err_text(
            fx.as_assistant(
                &victim.alias,
                "assistant_context_update",
                json!({"op": "update_soul", "soul": "Clean body\n", "tone": format!("tone {secret}")}),
            ),
            "update_soul tone with secret payload",
        );
    }
    assert_eq!(
        std::fs::read(cwd.join("MEMORY.md")).unwrap(),
        mem_before,
        "secret payload mutated MEMORY.md"
    );
    assert_eq!(
        std::fs::read(cwd.join("SOUL.md")).unwrap(),
        soul_before,
        "secret payload mutated SOUL.md"
    );

    // 5. `AGENTS.md` fails closed on BOTH reads AND attempted writes/overwrites,
    // including `bootstrap_ack`, leaving `AGENTS.md` byte-identical.
    for file in ["AGENTS.md", "./AGENTS.md", "memory/../AGENTS.md"] {
        let _ = err_text(
            fx.as_assistant(
                &victim.alias,
                "assistant_memory_read",
                json!({"file": file}),
            ),
            &format!("read {file}"),
        );
    }
    for topic in [
        "AGENTS",
        "AGENTS.md",
        "../AGENTS",
        "../AGENTS.md",
        "./AGENTS.md",
    ] {
        let _ = err_text(
            fx.as_assistant(
                &victim.alias,
                "assistant_context_update",
                json!({"op": "write_topic", "topic": topic, "content": "OVERWRITTEN AGENTS"}),
            ),
            &format!("write_topic targeting {topic}"),
        );
    }
    assert_eq!(
        std::fs::read(cwd.join("AGENTS.md")).unwrap(),
        agents_before,
        "AGENTS.md was modified by read/write_topic"
    );

    // 6. Symlink refusal, `../escape` traversal, `.memory-history.jsonl` read refusal,
    // and stale `expected_sha` on `update_soul`.
    let outside = fx.dir.path().join("outside-target.txt");
    std::fs::write(&outside, b"OUTSIDE_UNTOUCHED\n").unwrap();
    std::fs::create_dir_all(cwd.join("memory")).unwrap();
    std::os::unix::fs::symlink(&outside, cwd.join("memory/evil.md")).unwrap();

    let _ = err_text(
        fx.as_assistant(
            &victim.alias,
            "assistant_memory_read",
            json!({"file": "memory/evil.md"}),
        ),
        "symlink read memory/evil.md",
    );
    let _ = err_text(
        fx.as_assistant(
            &victim.alias,
            "assistant_context_update",
            json!({"op": "write_topic", "topic": "evil", "content": "PWNED OUTSIDE"}),
        ),
        "symlink write_topic evil",
    );
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"OUTSIDE_UNTOUCHED\n",
        "symlink write modified outside target file"
    );

    for bad_topic in ["../escape", "../../escape", "nested/topic"] {
        let _ = err_text(
            fx.as_assistant(
                &victim.alias,
                "assistant_context_update",
                json!({"op": "write_topic", "topic": bad_topic, "content": "escape"}),
            ),
            &format!("traversal write_topic {bad_topic}"),
        );
    }
    assert!(
        !cwd.join("escape.md").exists() && !fx.dir.path().join("escape.md").exists(),
        "topic traversal created escape.md"
    );

    let _ = err_text(
        fx.as_assistant(
            &victim.alias,
            "assistant_memory_read",
            json!({"file": ".memory-history.jsonl"}),
        ),
        "read .memory-history.jsonl",
    );

    // Stale expected_sha on update_soul must refuse leaving SOUL.md byte-identical.
    let stale_sha = match current_sha {
        Some(sha) if sha != "0".repeat(64) => "0".repeat(64),
        _ => "f".repeat(64),
    };
    let _ = err_text(
        fx.as_assistant(
            &victim.alias,
            "assistant_context_update",
            json!({
                "op": "update_soul",
                "soul": "Should fail stale CAS\n",
                "expected_sha": stale_sha
            }),
        ),
        "update_soul with stale expected_sha",
    );
    assert_eq!(
        std::fs::read(cwd.join("SOUL.md")).unwrap(),
        soul_before,
        "stale expected_sha mutated SOUL.md"
    );

    // `bootstrap_ack` (`message_report` with `bootstrap`) cannot modify `AGENTS.md`.
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, victim_sub)
        .unwrap()
        .unwrap();
    let _ = fx.as_assistant(
        &victim.alias,
        "message_report",
        json!({
            "message": _turn.msg_id,
            "token": _turn.token,
            "kind": "ack",
            "bootstrap": {
                "action": "complete",
                "expected_revision": binding.bootstrap_revision,
                "profile": {
                    "name": "Ada",
                    "tone": "concise",
                    "preferences": "bullets",
                    "agents": "OVERWRITE ATTEMPT",
                    "AGENTS.md": "OVERWRITE ATTEMPT"
                }
            }
        }),
    );
    assert_eq!(
        std::fs::read(cwd.join("AGENTS.md")).unwrap(),
        agents_before,
        "AGENTS.md was modified by bootstrap_ack"
    );

    // 7. Revoked binding (`fx.revoke(sub)`) closes all 5 RPCs immediately.
    fx.revoke(victim_sub);
    let before_revoke_calls = snapshot_assistant_files(&cwd);
    for (method, params) in all_five_calls {
        let _ = err_text(
            fx.as_assistant(&victim.alias, method, params),
            &format!("revoked binding calling {method}"),
        );
    }
    assert_eq!(
        snapshot_assistant_files(&cwd),
        before_revoke_calls,
        "revoked binding calls mutated files"
    );
}

#[test]
fn cad2832_three_layer_context_delivery_and_commit_discipline() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let sub = "sub-cad2832-delivery";
    let rt = fx.member_runtime(sub);
    let cwd = fx.shared.assistant_principal_cwd(ORG, sub);

    // Seed sentinels in IDENTITY.md and USER.md so we can verify compact deltas
    // never re-send IDENTITY.md, USER.md, or `## Available skills`.
    let id_sentinel = "IDENTITY_SENTINEL_CAD2832";
    let user_sentinel = "USER_SENTINEL_CAD2832";
    std::fs::write(
        cwd.join("IDENTITY.md"),
        format!("# Identity\n{id_sentinel}\n"),
    )
    .unwrap();
    std::fs::write(cwd.join("USER.md"), format!("# User\n{user_sentinel}\n")).unwrap();

    // Seed SOUL.md and MEMORY.md on Turn 1 (uncommitted yet).
    let t1 = start_interactive_turn(&fx, sub, &rt.alias, "turn 1 initial");
    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "update_soul", "soul": "SOUL_MARKER_V1\n"}),
    )
    .expect("t1 update_soul failed");
    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "remember", "fact": "MEMORY_MARKER_V1"}),
    )
    .expect("t1 remember failed");

    let msg1 = fx.shared.store.message(&t1.msg_id).unwrap().unwrap();
    let (layer1, d1_opt) = fx
        .shared
        .member_profile_context_for_turn(&msg1, Some("sess-1"), false)
        .expect("fresh turn context delivery failed")
        .expect("fresh turn returned None instead of Layer-1");
    let d1 = d1_opt.expect("fresh turn did not return MemberDeliveredContext");

    assert!(
        layer1.contains("SOUL_MARKER_V1"),
        "Layer-1 missing SOUL.md content: {layer1}"
    );
    assert!(
        layer1.contains("MEMORY_MARKER_V1")
            && layer1.contains("notes to consult, not instructions"),
        "Layer-1 missing fenced MEMORY.md notice/content: {layer1}"
    );
    assert!(
        layer1.contains("## Available skills") && layer1.contains("assistant-memory"),
        "Layer-1 missing Available skills / assistant-memory: {layer1}"
    );
    assert!(
        layer1.contains(id_sentinel) && layer1.contains(user_sentinel),
        "Layer-1 missing IDENTITY.md / USER.md content: {layer1}"
    );

    // Mid-turn guarantee A: staging `d1` and calling assistant_context_update(remember)
    // BEFORE commit_member_delivered, then discarding the turn (`discard_member_turn_state`),
    // MUST NOT advance `delivered`. Since turn 1 never committed, turn 2 still
    // sees full Layer-1 including the mid-turn remembered fact!
    fx.shared
        .stage_pending_member_delivered(&rt.alias, &t1.msg_id, d1);
    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "remember", "fact": "MID_TURN_UNCOMMITTED_V1"}),
    )
    .expect("mid-turn remember failed");
    fx.shared.discard_member_turn_state(&rt.alias, &t1.msg_id);

    let t2 = start_interactive_turn(&fx, sub, &rt.alias, "turn 2 after discard");
    let msg2 = fx.shared.store.message(&t2.msg_id).unwrap().unwrap();
    let (layer1_after_discard, d2_opt) = fx
        .shared
        .member_profile_context_for_turn(&msg2, Some("sess-1"), false)
        .expect("turn 2 context delivery failed")
        .expect("turn 2 returned None after turn 1 was discarded");
    let d2 = d2_opt.expect("turn 2 missing MemberDeliveredContext");
    assert!(
        layer1_after_discard.contains("MID_TURN_UNCOMMITTED_V1")
            && layer1_after_discard.contains("## Available skills"),
        "turn 2 after discard did not reload full Layer-1 with uncommitted edit: {layer1_after_discard}"
    );

    // Commit d2 after turn 2 completion -> subsequent unchanged turn returns None.
    finish_turn(&fx, &t2.msg_id);
    fx.shared
        .commit_member_delivered(&rt.alias, &t2.msg_id, Some(d2));

    let t3 = start_interactive_turn(&fx, sub, &rt.alias, "turn 3 unchanged");
    let msg3 = fx.shared.store.message(&t3.msg_id).unwrap().unwrap();
    let unchanged = fx
        .shared
        .member_profile_context_for_turn(&msg3, Some("sess-1"), false)
        .expect("turn 3 context delivery failed");
    assert!(
        unchanged.is_none(),
        "unchanged turn after commit should return None, got: {unchanged:?}"
    );

    // Mid-turn guarantee B (after an earlier commit!): edit memory mid-turn,
    // then discard instead of committing -> next turn MUST see the edit as a delta
    // (because `delivered` stayed at pre-turn state).
    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "remember", "fact": "MID_TURN_DISCARDED_DELTA"}),
    )
    .expect("t3 mid-turn remember failed");
    fx.shared.discard_member_turn_state(&rt.alias, &t3.msg_id);

    let t4 = start_interactive_turn(&fx, sub, &rt.alias, "turn 4 sees discarded edit");
    let msg4 = fx.shared.store.message(&t4.msg_id).unwrap().unwrap();
    let (delta_from_discard, d4_opt) = fx
        .shared
        .member_profile_context_for_turn(&msg4, Some("sess-1"), false)
        .expect("turn 4 context delivery failed")
        .expect("turn 4 missed uncommitted edit from discarded turn 3");
    let d4 = d4_opt.expect("turn 4 missing delivered context");
    assert!(
        delta_from_discard.contains("MID_TURN_DISCARDED_DELTA"),
        "turn 4 delta missing discarded turn 3 edit: {delta_from_discard}"
    );
    fx.shared
        .stage_pending_member_delivered(&rt.alias, &t4.msg_id, d4);

    // In-turn self-edit + stage_pending_member_delivered + store.finish completion +
    // commit_member_delivered(alias, turn_id, None) -> next turn returns None (no echo).
    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "remember", "fact": "IN_TURN_SELF_EDIT_COMMITTED"}),
    )
    .expect("t4 self-edit remember failed");
    finish_turn(&fx, &t4.msg_id);
    fx.shared
        .commit_member_delivered(&rt.alias, &t4.msg_id, None);

    let t5 = start_interactive_turn(&fx, sub, &rt.alias, "turn 5 no echo after self-edit");
    let msg5 = fx.shared.store.message(&t5.msg_id).unwrap().unwrap();
    let after_self_edit = fx
        .shared
        .member_profile_context_for_turn(&msg5, Some("sess-1"), false)
        .expect("turn 5 context delivery failed");
    assert!(
        after_self_edit.is_none(),
        "committed in-turn self-edit echoed redundantly on next turn: {after_self_edit:?}"
    );
    fx.shared
        .commit_member_delivered(&rt.alias, &t5.msg_id, None);

    // External edit to MEMORY.md & SOUL.md emits a compact delta WITHOUT
    // re-sending IDENTITY.md, USER.md, or `## Available skills`.
    let mem_cur = std::fs::read_to_string(cwd.join("MEMORY.md")).unwrap();
    std::fs::write(
        cwd.join("MEMORY.md"),
        format!("{mem_cur}\n- [2026-11-26] EXTERNAL_MEMORY_EDIT_V2\n"),
    )
    .unwrap();
    std::fs::write(cwd.join("SOUL.md"), "EXTERNAL_SOUL_EDIT_V2\n").unwrap();

    let t6 = start_interactive_turn(&fx, sub, &rt.alias, "turn 6 external edit delta");
    let msg6 = fx.shared.store.message(&t6.msg_id).unwrap().unwrap();
    let (delta_ext, d6_opt) = fx
        .shared
        .member_profile_context_for_turn(&msg6, Some("sess-1"), false)
        .expect("turn 6 external edit delivery failed")
        .expect("external edit to MEMORY.md/SOUL.md returned None");
    let d6 = d6_opt.expect("turn 6 missing delivered context");
    assert!(
        delta_ext.contains("EXTERNAL_MEMORY_EDIT_V2")
            && delta_ext.contains("EXTERNAL_SOUL_EDIT_V2"),
        "compact delta missing external MEMORY.md/SOUL.md edits: {delta_ext}"
    );
    assert!(
        !delta_ext.contains(id_sentinel)
            && !delta_ext.contains(user_sentinel)
            && !delta_ext.contains("## Available skills"),
        "compact delta re-sent IDENTITY.md, USER.md, or Available skills: {delta_ext}"
    );
    // Also test in-turn self-edit committed directly via commit_member_delivered(Some(d6))
    // yields no redundant echo on the following turn.
    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "remember", "fact": "DIRECT_COMMIT_SELF_EDIT"}),
    )
    .expect("t6 direct self-edit remember failed");
    fx.shared
        .commit_member_delivered(&rt.alias, &t6.msg_id, Some(d6));

    let t6b = start_interactive_turn(&fx, sub, &rt.alias, "turn 6b no echo after direct commit");
    let msg6b = fx.shared.store.message(&t6b.msg_id).unwrap().unwrap();
    let after_direct_self_edit = fx
        .shared
        .member_profile_context_for_turn(&msg6b, Some("sess-1"), false)
        .expect("turn 6b context delivery failed");
    assert!(
        after_direct_self_edit.is_none(),
        "direct-committed in-turn self-edit echoed redundantly on next turn: {after_direct_self_edit:?}"
    );

    // `force_full = true` (compaction/reset) reloads full Layer-1 with updated content.
    let t7 = start_interactive_turn(&fx, sub, &rt.alias, "turn 7 force_full reset");
    let msg7 = fx.shared.store.message(&t7.msg_id).unwrap().unwrap();
    let (full_reload, d7_opt) = fx
        .shared
        .member_profile_context_for_turn(&msg7, Some("sess-1"), true)
        .expect("force_full reload failed")
        .expect("force_full = true returned None");
    let d7 = d7_opt.expect("force_full missing delivered context");
    assert!(
        full_reload.contains("EXTERNAL_MEMORY_EDIT_V2")
            && full_reload.contains("EXTERNAL_SOUL_EDIT_V2")
            && full_reload.contains("notes to consult, not instructions")
            && full_reload.contains("## Available skills")
            && full_reload.contains(id_sentinel)
            && full_reload.contains(user_sentinel),
        "force_full did not reload complete Layer-1: {full_reload}"
    );
    fx.shared
        .commit_member_delivered(&rt.alias, &t7.msg_id, Some(d7));

    // Changed `session_key` reloads full Layer-1 including updated content.
    let t8 = start_interactive_turn(&fx, sub, &rt.alias, "turn 8 new session_key");
    let msg8 = fx.shared.store.message(&t8.msg_id).unwrap().unwrap();
    let (sess2_full, d8_opt) = fx
        .shared
        .member_profile_context_for_turn(&msg8, Some("sess-2"), false)
        .expect("changed session_key reload failed")
        .expect("changed session_key returned None");
    let d8 = d8_opt.expect("changed session_key missing delivered context");
    assert!(
        sess2_full.contains("EXTERNAL_MEMORY_EDIT_V2")
            && sess2_full.contains("EXTERNAL_SOUL_EDIT_V2")
            && sess2_full.contains("## Available skills")
            && sess2_full.contains(id_sentinel),
        "changed session_key did not reload full Layer-1: {sess2_full}"
    );
    fx.shared
        .commit_member_delivered(&rt.alias, &t8.msg_id, Some(d8));

    // clear_member_delivered resets delivered state so same session_key reloads Layer-1.
    fx.shared.clear_member_delivered(&rt.alias);
    let t9 = start_interactive_turn(&fx, sub, &rt.alias, "turn 9 after clear_member_delivered");
    let msg9 = fx.shared.store.message(&t9.msg_id).unwrap().unwrap();
    let (after_clear, d9_opt) = fx
        .shared
        .member_profile_context_for_turn(&msg9, Some("sess-2"), false)
        .expect("post-clear reload failed")
        .expect("clear_member_delivered did not reset delivered state");
    let d9 = d9_opt.expect("post-clear reload missing delivered context");
    assert!(
        after_clear.contains("## Available skills")
            && after_clear.contains("EXTERNAL_SOUL_EDIT_V2"),
        "post-clear reload missing full Layer-1: {after_clear}"
    );
    finish_turn(&fx, &t9.msg_id);
    fx.shared
        .commit_member_delivered(&rt.alias, &t9.msg_id, Some(d9));

    // Scoped mid-turn refresh isolation: an external edit to SOUL.md during a live
    // turn where the assistant only calls `remember` on MEMORY.md must still be
    // delivered on the next turn (not swallowed), while MEMORY.md is suppressed.
    let t10 = start_interactive_turn(&fx, sub, &rt.alias, "turn 10 scoped refresh isolation");
    let msg10 = fx.shared.store.message(&t10.msg_id).unwrap().unwrap();
    assert!(
        fx.shared
            .member_profile_context_for_turn(&msg10, Some("sess-2"), false)
            .expect("turn 10 context delivery failed")
            .is_none(),
        "turn 10 expected unchanged baseline"
    );
    std::fs::write(cwd.join("SOUL.md"), "EXTERNAL_SOUL_SCOPED_V3\n").unwrap();
    fx.as_assistant(
        &rt.alias,
        "assistant_context_update",
        json!({"op": "remember", "fact": "SCOPED_MEMORY_SELF_EDIT_V3"}),
    )
    .expect("t10 scoped remember failed");
    finish_turn(&fx, &t10.msg_id);
    fx.shared
        .commit_member_delivered(&rt.alias, &t10.msg_id, None);

    let t11 = start_interactive_turn(&fx, sub, &rt.alias, "turn 11 sees external SOUL only");
    let msg11 = fx.shared.store.message(&t11.msg_id).unwrap().unwrap();
    let (scoped_delta, _) = fx
        .shared
        .member_profile_context_for_turn(&msg11, Some("sess-2"), false)
        .expect("turn 11 context delivery failed")
        .expect("external SOUL.md edit was swallowed by MEMORY.md self-refresh");
    assert!(
        scoped_delta.contains("SOUL.md") && scoped_delta.contains("EXTERNAL_SOUL_SCOPED_V3"),
        "turn 11 delta missing external SOUL.md update: {scoped_delta}"
    );
    assert!(
        !scoped_delta.contains("MEMORY.md") && !scoped_delta.contains("SCOPED_MEMORY_SELF_EDIT_V3"),
        "turn 11 delta redundantly echoed MEMORY.md self-edit: {scoped_delta}"
    );
}
