//! Independently authored CAD-1814 acceptance for identity-bound OMP restart.
//! Exercise the real launch-argument decision without starting a provider or
//! claiming authenticated inference. Synthetic serialization-contract fixtures
//! below follow pinned OMP 18.12.6 types, not captured provider evidence.
//! No credentials or model-success evidence is fabricated.

use super::pi_launch::worker_session_args;
use super::*;

fn omp_worker(state: &Path, previously_owned: bool) -> Agent {
    Agent {
        alias: "omp-restart".into(),
        uid: "agt-omp-restart".into(),
        provider: "omp".into(),
        endpoint_kind: "managed".into(),
        role: "worker".into(),
        team_role: None,
        cwd: state.to_string_lossy().into_owned(),
        sandbox: "read-only".into(),
        instructions: None,
        thread_id: previously_owned.then(|| "owned-omp-session".into()),
        session_id: previously_owned.then(|| "owned-omp-session".into()),
        model: None,
        effort: None,
        pid: None,
        pid_start: None,
        endpoint: None,
        params: Some(json!({"model": "openai-codex/gpt-5.2"})),
        model_selection: None,
        quota: None,
        generation: None,
        state: "offline".into(),
        enabled: true,
        gc_keep: false,
        archived_at: None,
        archived_by: None,
        archive_reason: None,
        lifecycle_rev: 0,
        error: None,
        created: 0.0,
        updated: 0.0,
    }
}

#[test]
fn fresh_omp_without_stored_identity_can_mint_its_first_session() {
    let root = tempfile::tempdir().unwrap();
    let agent = omp_worker(root.path(), false);
    let expected_dir = root.path().join("agents").join(&agent.alias);

    assert_eq!(
        worker_session_args(root.path(), &agent).unwrap(),
        vec![
            "--session-dir".to_string(),
            expected_dir.to_string_lossy().into_owned(),
        ]
    );
}

#[test]
fn owned_omp_with_missing_pointer_and_file_must_not_start_fresh() {
    let root = tempfile::tempdir().unwrap();
    let agent = omp_worker(root.path(), true);

    assert!(
        worker_session_args(root.path(), &agent).is_err(),
        "a stored OMP identity without its persistence must refuse, not mint a replacement"
    );
}

#[test]
fn owned_omp_with_dangling_pointer_must_not_start_fresh() {
    let root = tempfile::tempdir().unwrap();
    let agent = omp_worker(root.path(), true);
    let worker = root.path().join("agents").join(&agent.alias);
    std::fs::create_dir_all(&worker).unwrap();
    let missing = worker.join("missing-owned-session.jsonl");
    std::fs::write(
        worker.join("session.current"),
        missing.to_string_lossy().as_bytes(),
    )
    .unwrap();

    assert!(
        worker_session_args(root.path(), &agent).is_err(),
        "a dangling owned-session pointer must refuse, not silently select fresh argv"
    );
}

#[test]
fn owned_omp_with_missing_pointer_must_not_adopt_an_unrelated_fallback() {
    let root = tempfile::tempdir().unwrap();
    let agent = omp_worker(root.path(), true);
    let worker = root.path().join("agents").join(&agent.alias);
    std::fs::create_dir_all(&worker).unwrap();
    // Existence is not identity evidence. This is an opaque decoy, not a
    // fabricated native session or a claim that OMP could resume its bytes.
    std::fs::write(worker.join("session.jsonl"), b"unrelated file").unwrap();

    assert!(
        worker_session_args(root.path(), &agent).is_err(),
        "losing the owned pointer must not authorize adoption of session.jsonl"
    );
}

#[test]
fn owned_omp_with_outside_pointer_must_not_adopt_any_fallback() {
    let root = tempfile::tempdir().unwrap();
    let agent = omp_worker(root.path(), true);
    let worker = root.path().join("agents").join(&agent.alias);
    std::fs::create_dir_all(&worker).unwrap();
    let outside = root.path().join("outside-worker.jsonl");
    std::fs::write(&outside, b"outside worker file").unwrap();
    std::fs::write(worker.join("session.jsonl"), b"unrelated fallback").unwrap();
    std::fs::write(
        worker.join("session.current"),
        outside.to_string_lossy().as_bytes(),
    )
    .unwrap();

    assert!(
        worker_session_args(root.path(), &agent).is_err(),
        "an outside-worker pointer must refuse, not adopt it or a different fallback session"
    );
}

/// Synthetic serialization-contract fixture, NOT real-provider evidence.
/// OMP v18.12.6: session-entries.ts:16-46 defines version 3, the 256-byte
/// leading title slot and SessionHeader fields; session-title-slot.ts:113-125
/// pads that slot; session-manager.ts:1568-1572 writes title then header.
/// https://github.com/can1357/oh-my-pi/tree/v18.12.6/packages/coding-agent/src/session
fn write_serialization_contract_session(
    state: &Path,
    agent: &Agent,
    native_id: &str,
    saved_cwd: &Path,
) -> PathBuf {
    let worker = state.join("agents").join(&agent.alias);
    std::fs::create_dir_all(&worker).unwrap();
    let file = worker.join("synthetic-owned.jsonl");
    let mut title = json!({
        "type": "title",
        "v": 1,
        "title": "",
        "updatedAt": "2026-10-25T00:00:00.000Z",
        "pad": "",
    });
    let unpadded_bytes = serde_json::to_string(&title).unwrap().len() + 1;
    title["pad"] = json!(" ".repeat(256 - unpadded_bytes));
    let title_line = format!("{title}\n");
    assert_eq!(title_line.len(), 256);
    let header = json!({
        "type": "session",
        "version": 3,
        "id": native_id,
        "timestamp": "2026-10-25T00:00:00.000Z",
        "cwd": saved_cwd.to_string_lossy(),
    });
    std::fs::write(&file, format!("{title_line}{header}\n")).unwrap();
    std::fs::write(
        worker.join("session.current"),
        file.to_string_lossy().as_bytes(),
    )
    .unwrap();
    file
}

#[test]
fn owned_omp_with_matching_native_header_and_cwd_resumes_its_exact_file() {
    let root = tempfile::tempdir().unwrap();
    let mut agent = omp_worker(root.path(), true);
    agent.params.as_mut().unwrap()["catalogue"] = json!("developer");
    let file = write_serialization_contract_session(
        root.path(),
        &agent,
        agent.session_id.as_deref().unwrap(),
        root.path(),
    );

    assert_eq!(
        worker_session_args(root.path(), &agent).unwrap(),
        vec!["--resume".to_string(), file.to_string_lossy().into_owned()]
    );
}

#[test]
fn owned_omp_with_different_requested_cwd_refuses_unsupported_relocation() {
    let root = tempfile::tempdir().unwrap();
    let mut agent = omp_worker(root.path(), true);
    let lane = root.path().join("repo/.cadence/wt/task");
    std::fs::create_dir_all(&lane).unwrap();
    let params = agent.params.as_mut().unwrap();
    params["catalogue"] = json!("developer");
    params["launch_workdir"] = json!(lane.to_string_lossy());
    write_serialization_contract_session(
        root.path(),
        &agent,
        agent.session_id.as_deref().unwrap(),
        root.path(),
    );

    assert!(
        worker_session_args(root.path(), &agent).is_err(),
        "a saved home cwd must not silently resume for a lane or use unsupported Pi fork flags"
    );
}

#[test]
fn owned_omp_with_different_native_header_identity_refuses_retargeting() {
    let root = tempfile::tempdir().unwrap();
    let agent = omp_worker(root.path(), true);
    write_serialization_contract_session(
        root.path(),
        &agent,
        "different-native-session",
        root.path(),
    );

    assert!(
        worker_session_args(root.path(), &agent).is_err(),
        "an in-worker pointer is not authority to replace the stored native session identity"
    );
}
