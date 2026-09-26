//! board_context: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

fn context_repo(repo: &Path, project: &str, document_path: &str, document: &str) -> String {
    assert!(git(repo, &["init", "-q"]).0);
    assert!(git(repo, &["config", "user.email", "context@test"]).0);
    assert!(git(repo, &["config", "user.name", "context-test"]).0);
    let manifest = format!(
        "schema: 1\nproject: {project}\ndocuments:\n  - id: guide\n    kind: index\n    path: {document_path}\n    title: Guide\n    required: true\n    roles: [pm, dev, qa, devops]\n"
    );
    let manifest_path = repo.join("docs/cadence/project-context.yaml");
    std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
    std::fs::write(manifest_path, manifest).unwrap();
    let document_path = repo.join(document_path);
    if let Some(parent) = document_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(document_path, document).unwrap();
    assert!(git(repo, &["add", "-A"]).0);
    assert!(git(repo, &["commit", "-qm", "context fixture"]).0);
    head(repo)
}

fn add_context_project(pm: &Path, state: &Path, key: &str, prefix: &str, repos: &[&Path]) {
    let mut args = vec!["issue", "project", "add", key, "--prefix", prefix];
    let paths: Vec<String> = repos
        .iter()
        .map(|repo| repo.to_str().unwrap().to_string())
        .collect();
    for path in &paths {
        args.extend(["--repo", path.as_str()]);
    }
    assert!(
        cli(pm, state, &args).0,
        "could not add context project {key}"
    );
}

#[test]
fn project_context_api_scopes_projects_and_pins_revision() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let alpha = TempDir::new().unwrap();
    let beta = TempDir::new().unwrap();
    let alpha_old = context_repo(alpha.path(), "alpha", "docs/alpha.md", "alpha HEAD text\n");
    let beta_head = context_repo(beta.path(), "beta", "docs/beta.md", "beta only text\n");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "alpha", "A", &[alpha.path()]);
    add_context_project(pm.path(), state.path(), "beta", "B", &[beta.path()]);
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    let (code, body) = http(port, "GET", "/api/projects/alpha/context?role=pm", &host);
    assert_eq!(code, 200, "{body}");
    let first: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(first["project"], "alpha");
    assert_eq!(first["state"], "ready");
    assert_eq!(first["snapshot"]["head_revision"], alpha_old);
    assert_eq!(first["snapshot"]["revision_state"], "uncompared");
    assert!(!first["snapshot"]["dirty"].as_bool().unwrap());
    assert!(first["documents"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("alpha HEAD"));

    // Untracked bytes do not make dirty=true and a tracked edit is still
    // excluded from the pinned HEAD blob.
    std::fs::write(
        alpha.path().join("docs/alpha.md"),
        "alpha working tree only\n",
    )
    .unwrap();
    std::fs::write(alpha.path().join("secret.txt"), "never served\n").unwrap();
    let query = format!("/api/projects/alpha/context?role=pm&expected_revision={alpha_old}");
    let (code, body) = http(port, "GET", &query, &host);
    assert_eq!(code, 200, "{body}");
    let dirty: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(dirty["snapshot"]["head_revision"], alpha_old);
    assert_eq!(dirty["snapshot"]["revision_state"], "current");
    assert!(dirty["snapshot"]["dirty"].as_bool().unwrap());
    assert!(dirty["documents"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("alpha HEAD"));
    assert!(!body.contains("secret.txt"));

    assert!(git(alpha.path(), &["add", "docs/alpha.md"]).0);
    assert!(git(alpha.path(), &["commit", "-qm", "advance context"]).0);
    let alpha_new = head(alpha.path());
    assert_ne!(alpha_new, alpha_old);
    let query = format!("/api/projects/alpha/context?role=pm&expected_revision={alpha_old}");
    let (code, body) = http(port, "GET", &query, &host);
    assert_eq!(code, 200, "{body}");
    let stale: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(stale["snapshot"]["revision_state"], "stale");
    assert_eq!(stale["snapshot"]["expected_revision"], alpha_old);
    assert_eq!(stale["snapshot"]["head_revision"], alpha_new);
    assert!(stale["documents"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("alpha working"));

    let (code, body) = http(port, "GET", "/api/projects/beta/context?role=pm", &host);
    assert_eq!(code, 200, "{body}");
    let second: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(second["project"], "beta");
    assert_eq!(second["snapshot"]["head_revision"], beta_head);
    assert!(second.to_string().contains("beta only"));
    assert!(!second.to_string().contains("alpha HEAD"));
    assert!(!second.to_string().contains(&alpha_new));

    let (code, _) = http(port, "GET", "/api/projects/unknown/context?role=pm", &host);
    assert_eq!(code, 404);
    let (code, _) = http(
        port,
        "GET",
        "/api/projects/alpha/context?role=pm&unexpected=1",
        &host,
    );
    assert_eq!(code, 400);
    let (code, _) = http(
        port,
        "GET",
        "/api/projects/alpha/context?expected_revision=bad",
        &host,
    );
    assert_eq!(code, 400);
}

/// `devops` is the stored context role. A manifest or caller written
/// before the rename still says `ops`; both spellings select the same
/// documents and the response only ever names `devops`.
#[test]
fn project_context_accepts_ops_and_stores_devops() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let _ = context_repo(repo.path(), "roles", "docs/guide.md", "guide text\n");
    let manifest = "schema: 1\nproject: roles\ndocuments:\n  - id: guide\n    kind: index\n    path: docs/guide.md\n    title: Guide\n    required: true\n  - id: legacy\n    kind: release\n    path: docs/legacy.md\n    title: Legacy ops document\n    required: false\n    roles: [ops]\n  - id: current\n    kind: validation\n    path: docs/current.md\n    title: Current devops document\n    required: false\n    roles: [devops]\n";
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        manifest,
    )
    .unwrap();
    std::fs::write(repo.path().join("docs/legacy.md"), "legacy text\n").unwrap();
    std::fs::write(repo.path().join("docs/current.md"), "current text\n").unwrap();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "role fixture"]).0);
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "roles", "R", &[repo.path()]);
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    for role in ["devops", "ops"] {
        let query = format!("/api/projects/roles/context?role={role}");
        let (code, body) = http(port, "GET", &query, &host);
        assert_eq!(code, 200, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["state"], "ready", "role={role}: {body}");
        for index in [1, 2] {
            let document = &value["documents"][index];
            assert_eq!(document["selected"], true, "role={role}: {document}");
            assert_eq!(document["selection_reason"], "role:devops", "role={role}");
        }
        assert!(!body.contains("role:ops"), "role={role}: {body}");
    }

    let (code, body) = http(port, "GET", "/api/projects/roles/context?role=dev", &host);
    assert_eq!(code, 200, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    for index in [1, 2] {
        assert_eq!(value["documents"][index]["selected"], false);
        assert_eq!(
            value["documents"][index]["selection_reason"],
            "excluded:role-mismatch"
        );
    }
    let (code, _) = http(
        port,
        "GET",
        "/api/projects/roles/context?role=operations",
        &host,
    );
    assert_eq!(code, 400);
}

#[test]
fn project_context_dirty_probe_failure_keeps_pinned_documents() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let head = context_repo(
        repo.path(),
        "dirty-probe",
        "docs/probe.md",
        "probe HEAD remains readable\n",
    );

    // status needs the index; immutable HEAD/tree/blob readers do not.
    std::fs::write(repo.path().join(".git/index"), b"not a git index\n").unwrap();
    let (status_ok, status_output) = git(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=no"],
    );
    assert!(
        !status_ok,
        "corrupted index unexpectedly passed status: {status_output}"
    );
    let (head_ok, observed_head) = git(repo.path(), &["rev-parse", "--verify", "HEAD"]);
    assert!(head_ok);
    assert_eq!(observed_head.trim(), head);
    let (tree_ok, tree) = git(repo.path(), &["ls-tree", &head, "--", "docs/probe.md"]);
    assert!(tree_ok);
    assert!(tree.contains("docs/probe.md"));
    let (blob_ok, blob) = git(repo.path(), &["cat-file", "blob", "HEAD:docs/probe.md"]);
    assert!(blob_ok);
    assert!(blob.contains("probe HEAD remains readable"));

    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "dirty-probe", "D", &[repo.path()]);
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/projects/dirty-probe/context", &host);
    assert_eq!(code, 200, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["snapshot"]["dirty"], Value::Null);
    assert!(!value["snapshot"]["error"]
        .as_str()
        .unwrap_or_default()
        .is_empty());
    assert_eq!(value["snapshot"]["head_revision"], head);
    assert_ne!(value["state"], "unavailable_repository");
    assert!(value["documents"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("probe HEAD remains readable"));
}

#[test]
fn project_context_rejects_invalid_paths_and_reports_repository_states() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let no_manifest = TempDir::new().unwrap();
    let _ = context_repo(repo.path(), "bad", "docs/guide.md", "safe text\n");
    assert!(git(no_manifest.path(), &["init", "-q"]).0);
    assert!(
        git(
            no_manifest.path(),
            &["config", "user.email", "context@test"]
        )
        .0
    );
    assert!(git(no_manifest.path(), &["config", "user.name", "context-test"]).0);
    std::fs::write(no_manifest.path().join("README.md"), "no manifest\n").unwrap();
    assert!(git(no_manifest.path(), &["add", "-A"]).0);
    assert!(git(no_manifest.path(), &["commit", "-qm", "without manifest"]).0);
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "bad", "BAD", &[repo.path()]);
    add_context_project(
        pm.path(),
        state.path(),
        "no-manifest",
        "N",
        &[no_manifest.path()],
    );
    add_context_project(pm.path(), state.path(), "remote", "REM", &[]);
    add_context_project(
        pm.path(),
        state.path(),
        "multi",
        "M",
        &[repo.path(), outside.path()],
    );
    add_context_project(
        pm.path(),
        state.path(),
        "unavailable",
        "U",
        &[Path::new("/definitely/missing/cadence-context")],
    );
    // `outside` is deliberately not a git repository; the multi-repo state
    // is decided from declarations before either path is opened.
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    for (key, state_name) in [
        ("no-manifest", "missing"),
        ("remote", "missing_repository"),
        ("multi", "ambiguous_repository"),
        ("unavailable", "unavailable_repository"),
    ] {
        let (code, body) = http(port, "GET", &format!("/api/projects/{key}/context"), &host);
        assert_eq!(code, 200, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["state"], state_name, "{value}");
    }

    // A manifest path escape is retained as metadata and blocked before any
    // Git blob lookup.
    let manifest = "schema: 1\nproject: bad\ndocuments:\n  - id: escape\n    kind: index\n    path: ../outside.md\n    title: Escape\n    required: true\n";
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        manifest,
    )
    .unwrap();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "escape path"]).0);
    let (code, body) = http(port, "GET", "/api/projects/bad/context", &host);
    assert_eq!(code, 200, "{body}");
    let escaped: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(escaped["state"], "conflict");
    assert_eq!(escaped["documents"][0]["state"], "invalid_path");
    assert!(escaped["documents"][0].get("excerpt").is_none());

    // A valid manifest still reports each missing required document exactly.
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: bad\ndocuments:\n  - id: architecture\n    kind: architecture\n    path: docs/ARCHITECTURE.md\n    title: Architecture\n    required: true\n",
    )
    .unwrap();
    std::fs::remove_file(repo.path().join("docs/link.md")).ok();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "missing architecture"]).0);
    let (code, body) = http(port, "GET", "/api/projects/bad/context", &host);
    assert_eq!(code, 200, "{body}");
    let missing: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(missing["state"], "missing");
    assert_eq!(missing["documents"][0]["state"], "missing");

    // A tracked document over the blob bound is reported without serving a
    // prefix of its bytes.
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: bad\ndocuments:\n  - id: huge\n    kind: spec\n    path: docs/huge.md\n    title: Huge\n    required: true\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("docs/huge.md"), vec![b'x'; 65 * 1024]).unwrap();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "oversized document"]).0);
    let (code, body) = http(port, "GET", "/api/projects/bad/context", &host);
    assert_eq!(code, 200, "{body}");
    let huge: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(huge["state"], "too_large");
    assert_eq!(huge["documents"][0]["state"], "too_large");
    assert!(huge["documents"][0].get("excerpt").is_none());

    // A tracked symlink is rejected even though its target is a document.
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: bad\ndocuments:\n  - id: link\n    kind: index\n    path: docs/link.md\n    title: Link\n    required: true\n",
    )
    .unwrap();
    std::fs::write(
        repo.path().join("docs/real.md"),
        "target bytes must not appear\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("real.md", repo.path().join("docs/link.md")).unwrap();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "symlink path"]).0);
    let (code, body) = http(port, "GET", "/api/projects/bad/context", &host);
    assert_eq!(code, 200, "{body}");
    let linked: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(linked["state"], "unreadable");
    assert_eq!(linked["documents"][0]["state"], "unreadable");
    assert!(!body.contains("target bytes must not appear"));
}

#[test]
fn project_context_manifest_conflicts_are_bounded_and_select_nothing() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let _ = context_repo(repo.path(), "conflicts", "docs/guide.md", "guide\n");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "conflicts", "C", &[repo.path()]);
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let replace_manifest = |manifest: &str, message: &str| -> Value {
        std::fs::write(
            repo.path().join("docs/cadence/project-context.yaml"),
            manifest,
        )
        .unwrap();
        assert!(git(repo.path(), &["add", "-A"]).0);
        assert!(git(repo.path(), &["commit", "-qm", message]).0);
        let (code, body) = http(port, "GET", "/api/projects/conflicts/context", &host);
        assert_eq!(code, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    };

    let malformed = replace_manifest("schema: [", "malformed context manifest");
    assert_eq!(malformed["state"], "conflict");
    assert_eq!(malformed["documents"].as_array().unwrap().len(), 0);

    let mismatched = replace_manifest(
        "schema: 1\nproject: another\ndocuments:\n  - id: guide\n    kind: index\n    path: docs/guide.md\n    title: Guide\n    required: true\n",
        "mismatched context manifest",
    );
    assert_eq!(mismatched["state"], "conflict");
    assert_eq!(mismatched["documents"][0]["selected"], false);
    assert!(mismatched["documents"][0].get("excerpt").is_none());

    let duplicate = replace_manifest(
        "schema: 1\nproject: conflicts\ndocuments:\n  - id: guide\n    kind: index\n    path: docs/guide.md\n    title: Guide\n    required: true\n  - id: guide\n    kind: index\n    path: docs/guide.md\n    title: Duplicate\n    required: false\n",
        "duplicate context manifest",
    );
    assert_eq!(duplicate["state"], "conflict");
    assert!(duplicate["manifest"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error.as_str().unwrap().contains("duplicate")));
    assert!(duplicate["documents"]
        .as_array()
        .unwrap()
        .iter()
        .all(|document| document["selected"] == false));

    let mut oversized = String::from("schema: 1\nproject: conflicts\ndocuments:\n");
    for index in 0..33 {
        oversized.push_str(&format!(
            "  - id: doc-{index}\n    kind: spec\n    path: docs/doc-{index}.md\n    title: Document {index}\n    required: false\n"
        ));
    }
    let oversized = replace_manifest(&oversized, "oversized context manifest");
    assert_eq!(oversized["state"], "conflict");
    assert_eq!(oversized["manifest"]["entry_count"], 33);
    assert_eq!(oversized["manifest"]["entries_omitted"], 1);
    assert_eq!(oversized["documents"].as_array().unwrap().len(), 32);
    assert!(oversized["documents"]
        .as_array()
        .unwrap()
        .iter()
        .all(|document| document["selected"] == false));
    assert!(!oversized.to_string().contains("doc-32.md"));
}

#[test]
fn project_context_memories_include_only_verified_lessons_and_bound_withheld() {
    let pm_dir = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let _ = context_repo(repo.path(), "memory", "docs/guide.md", "memory context\n");
    assert!(cli(pm_dir.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm_dir.path(), state.path(), "memory", "M", &[repo.path()]);
    let pm = cadence_agent::issue::Pm::at(pm_dir.path()).unwrap();
    let identity =
        |alias: &str, registration: u64, role: &str| cadence_agent::memory::NativeIdentity {
            proof: cadence_agent::memory::IdentityProof {
                alias: alias.to_string(),
                registration,
                generation: format!("{alias}-generation"),
                process_start: registration,
                role: role.to_string(),
            },
        };
    let author = identity("author", 1, "worker");
    let reviewer_a = identity("reviewer-a", 2, "worker");
    let reviewer_b = identity("reviewer-b", 3, "worker");
    let finalizer = identity("pm", 4, "pm");
    let scope = cadence_agent::memory::Scope {
        paths: vec!["docs/**".to_string()],
        ..Default::default()
    };
    let proposed = cadence_agent::memory::propose_native(
        &pm,
        "memory",
        "rule",
        &scope,
        Some("CAD-224"),
        Some("high"),
        None,
        Some("verified memory fact\n\n**Why:** fixture\n\n**How to apply:** use it\n"),
        Some("verified"),
        &author,
    )
    .unwrap();
    let digest = proposed["digest"].as_str().unwrap().to_string();
    for reviewer in [&reviewer_a, &reviewer_b] {
        cadence_agent::memory::submit_review(
            &pm,
            Some("memory"),
            "verified",
            &cadence_agent::memory::ReviewRequest {
                operation: "accept",
                verdict: "pass",
                evidence: "reviewed fixture",
                expected_digest: &digest,
            },
            reviewer,
        )
        .unwrap();
    }
    cadence_agent::memory::finalize_native(
        &pm,
        Some("memory"),
        "verified",
        "accept",
        &digest,
        &finalizer,
    )
    .unwrap();
    cadence_agent::memory::propose_native(
        &pm,
        "memory",
        "rule",
        &scope,
        Some("CAD-224"),
        Some("high"),
        None,
        Some("proposed claim must be withheld\n\n**Why:** fixture\n\n**How to apply:** never serve\n"),
        Some("proposed"),
        &author,
    )
    .unwrap();
    std::fs::create_dir_all(pm_dir.path().join("memory/memory")).unwrap();
    std::fs::write(
        pm_dir.path().join("memory/memory/broken.md"),
        "this is not a memory document\n",
    )
    .unwrap();

    let (port, _board) = start_ui(pm_dir.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/projects/memory/context", &host);
    assert_eq!(code, 200, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    let ids: Vec<&str> = value["memories"]["included"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(ids.contains(&"verified"), "{value}");
    assert!(!ids.contains(&"proposed"));
    assert!(value["memories"]["withheld"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == "proposed"));
    assert!(value["memories"]["load_errors_total"].as_u64().unwrap() >= 1);
    assert!(value["memories"]["load_errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error.as_str().unwrap().contains("broken.md")));
    assert!(value["memories"]["lessons"].as_str().unwrap().len() <= 4 * 1024);
}
