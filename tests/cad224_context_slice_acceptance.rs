//! CAD-224 first-slice acceptance: the bounded, read-only project context.
//!
//! This is independent acceptance evidence for the *source delivery* slice —
//! a selected project's registered scope/design/architecture/QA documents read
//! from one pinned tracked revision, plus the eligible project memory the
//! existing quorum-aware renderer selects. It is not evidence for the full
//! goal-intake outcome (persisted goals, outcome-to-source mapping, plan
//! revision, ready-work scheduling), which stays open on the ticket.
//!
//! Every case drives the real path: a real PM registry, real Git repositories,
//! real memory records parsed by the real loader, and the real
//! `context::bundle` entry point the HTTP route calls. Nothing here stubs the
//! reader, the Git plumbing or the memory renderer.

use cadence_agent::issue::{context, project, Pm};
use cadence_agent::memory;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST: &str = "docs/cadence/project-context.yaml";

// ---------------------------------------------------------------- fixtures

fn git(dir: &Path, args: &[&str]) -> String {
    // The subreaper must see every child (CAD-308); the retained tests use
    // the same wrapper instead of `Command::output`.
    let out = cadence_agent::reaper::output(Command::new("git").arg("-C").arg(dir).args(args))
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end()
        .to_string()
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "cad224@test"]);
    git(dir.path(), &["config", "user.name", "cad224"]);
    dir
}

fn write(root: &Path, rel: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn commit(root: &Path, message: &str) -> String {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", message]);
    git(root, &["rev-parse", "HEAD"])
}

fn pm_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pm.yaml"), "schema: 1\n").unwrap();
    dir
}

fn add_project(pm: &Path, key: &str, repos: &[&Path], components: &[&str]) {
    let mut yaml = format!("key: {key}\nprefix: {}\n", key.to_uppercase());
    if !repos.is_empty() {
        yaml.push_str("repos:\n");
        for repo in repos {
            yaml.push_str(&format!("  - path: \"{}\"\n", repo.display()));
        }
    }
    if !components.is_empty() {
        yaml.push_str("components:\n");
        for component in components {
            yaml.push_str(&format!("  - {component}\n"));
        }
    }
    write(pm, &format!("{key}/project.yaml"), yaml);
}

struct Doc<'a> {
    id: &'a str,
    kind: &'a str,
    path: &'a str,
    title: &'a str,
    required: bool,
    roles: &'a [&'a str],
}

fn doc<'a>(id: &'a str, kind: &'a str, path: &'a str, required: bool) -> Doc<'a> {
    Doc {
        id,
        kind,
        path,
        title: id,
        required,
        roles: &[],
    }
}

fn manifest(project: &str, docs: &[Doc<'_>]) -> String {
    let mut yaml = format!("schema: 1\nproject: {project}\ndocuments:\n");
    for doc in docs {
        yaml.push_str(&format!(
            "  - id: {}\n    kind: {}\n    path: \"{}\"\n    title: {}\n    required: {}\n",
            doc.id, doc.kind, doc.path, doc.title, doc.required
        ));
        if !doc.roles.is_empty() {
            yaml.push_str(&format!("    roles: [{}]\n", doc.roles.join(", ")));
        }
    }
    yaml
}

fn bundle(pm: &Path, key: &str, role: Option<&str>, expected: Option<&str>) -> Value {
    let pm = Pm::at(pm).unwrap();
    let projects = project::list(&pm.dir).unwrap();
    let selected = projects
        .iter()
        .find(|candidate| candidate.key == key)
        .unwrap_or_else(|| panic!("project {key} is registered"));
    context::bundle(&pm, selected, role, expected)
}

fn document<'a>(bundle: &'a Value, id: &str) -> &'a Value {
    bundle["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|document| document["id"] == id)
        .unwrap_or_else(|| panic!("document {id} is in the bundle: {bundle}"))
}

fn documents(bundle: &Value) -> Vec<String> {
    bundle["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|document| document["id"].as_str().unwrap().to_string())
        .collect()
}

fn included(bundle: &Value) -> Vec<String> {
    bundle["memories"]["included"]
        .as_array()
        .unwrap()
        .iter()
        .map(|memory| memory["id"].as_str().unwrap().to_string())
        .collect()
}

fn withheld(bundle: &Value) -> Vec<(String, String)> {
    bundle["memories"]["withheld"]
        .as_array()
        .unwrap()
        .iter()
        .map(|memory| {
            (
                memory["id"].as_str().unwrap().to_string(),
                memory["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn reason_for(bundle: &Value, id: &str) -> String {
    withheld(bundle)
        .into_iter()
        .find(|(candidate, _)| candidate == id)
        .unwrap_or_else(|| panic!("{id} is explained as withheld: {bundle}"))
        .1
}

fn serialized(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap()
}

/// The bundle must never carry bytes from outside the selected manifest.
fn assert_absent(bundle: &Value, marker: &str) {
    let text = serialized(bundle);
    assert!(!text.contains(marker), "the bundle leaked {marker}: {text}");
}

fn memory_file(pm: &Path, key: &str, slug: &str, front: &str, body: &str) -> PathBuf {
    let rel = format!("{key}/memory/{slug}.md");
    write(pm, &rel, format!("---\n{front}---\n{body}"));
    pm.join(rel)
}

const LESSON_BODY: &str = "Read the manifest before planning.\n\n**Why:** plans drift from the sources.\n\n**How to apply:** open the pinned document first.\n";

/// A complete accepted record: authenticated proposer, two distinct
/// authenticated passing reviews and the matching PM acceptance
/// finalization — the exact shape CAD-191 retrieval requires. The digest is
/// computed by the real `semantic_digest`, so an edit to any claim field
/// invalidates these receipts exactly as production would.
fn eligible_memory(pm: &Path, key: &str, slug: &str, paths: &[&str]) -> PathBuf {
    let scope = if paths.is_empty() {
        "scope:\n  project: true\n".to_string()
    } else {
        let mut scope = "scope:\n  paths:\n".to_string();
        for path in paths {
            scope.push_str(&format!("    - \"{path}\"\n"));
        }
        scope
    };
    let claim = format!(
        "id: {slug}\ntype: gotcha\nstatus: accepted\n{scope}source: CAD-224\nconfidence: high\ncreated: 2026-09-01T00:00:00Z\nauthor: cad224-author\nauthor_proof:\n  alias: cad224-author\n  registration: 11\n  generation: gen-author\n  process_start: 1001\n  role: worker\nreview_cycle: 1\n"
    );
    // Phase one: the claim alone, so the real loader can hash exactly what
    // the receipts will attest to.
    memory_file(pm, key, slug, &claim, LESSON_BODY);
    let (pool, errors) = memory::load_project_report(pm, key);
    assert!(errors.is_empty(), "{slug} draft load errors: {errors:?}");
    let memory = pool
        .iter()
        .find(|memory| memory.front.id == slug)
        .unwrap_or_else(|| panic!("{slug} draft loads"));
    let digest = memory::semantic_digest(memory);
    let receipts = format!(
        "reviews:\n  - reviewer: cad224-review-one\n    identity: cad224-review-one#21\n    generation: gen-r1\n    process_start: 2001\n    role: worker\n    operation: accept\n    cycle: 1\n    digest: {digest}\n    verdict: pass\n    evidence: \"checked against the tracked documents\"\n    recorded_at: 2026-09-02T00:00:00Z\n  - reviewer: cad224-review-two\n    identity: cad224-review-two#22\n    generation: gen-r2\n    process_start: 2002\n    role: worker\n    operation: accept\n    cycle: 1\n    digest: {digest}\n    verdict: pass\n    evidence: \"re-read the cited source\"\n    recorded_at: 2026-09-02T01:00:00Z\nfinalizations:\n  - operation: accept\n    cycle: 1\n    digest: {digest}\n    finalizer:\n      alias: cad224-pm\n      registration: 31\n      generation: gen-pm\n      process_start: 3001\n      role: pm\n    finalized_at: 2026-09-03T00:00:00Z\n"
    );
    memory_file(pm, key, slug, &format!("{claim}{receipts}"), LESSON_BODY)
}

fn proposed_memory(pm: &Path, key: &str, slug: &str) {
    memory_file(
        pm,
        key,
        slug,
        &format!(
            "id: {slug}\ntype: rule\nstatus: proposed\nscope:\n  paths:\n    - \"docs/**\"\nconfidence: medium\ncreated: 2026-09-01T00:00:00Z\nauthor: cad224-author\n"
        ),
        LESSON_BODY,
    );
}

/// An accepted record whose receipts attest to a different claim digest.
fn edited_digest_memory(pm: &Path, key: &str, slug: &str) {
    let stale_digest = "0".repeat(64);
    let front = format!(
        "id: {slug}\ntype: rule\nstatus: accepted\nscope:\n  paths:\n    - \"docs/**\"\nconfidence: high\ncreated: 2026-09-01T00:00:00Z\nauthor: cad224-author\nauthor_proof:\n  alias: cad224-author\n  registration: 11\n  generation: gen-author\n  process_start: 1001\n  role: worker\nreview_cycle: 1\nreviews:\n  - reviewer: cad224-review-one\n    identity: cad224-review-one#21\n    generation: gen-r1\n    process_start: 2001\n    role: worker\n    operation: accept\n    cycle: 1\n    digest: {stale_digest}\n    verdict: pass\n    evidence: \"checked\"\n    recorded_at: 2026-09-02T00:00:00Z\n  - reviewer: cad224-review-two\n    identity: cad224-review-two#22\n    generation: gen-r2\n    process_start: 2002\n    role: worker\n    operation: accept\n    cycle: 1\n    digest: {stale_digest}\n    verdict: pass\n    evidence: \"checked\"\n    recorded_at: 2026-09-02T01:00:00Z\nfinalizations:\n  - operation: accept\n    cycle: 1\n    digest: {stale_digest}\n    finalizer:\n      alias: cad224-pm\n      registration: 31\n      generation: gen-pm\n      process_start: 3001\n      role: pm\n    finalized_at: 2026-09-03T00:00:00Z\n"
    );
    memory_file(pm, key, slug, &front, LESSON_BODY);
}

fn memory_snapshot(pm: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for project in std::fs::read_dir(pm).unwrap().flatten() {
        let dir = project.path().join("memory");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            files.push((entry.path(), std::fs::read(entry.path()).unwrap()));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

// ------------------------------------------------------------------- tests

/// Two registered projects with differently named documents: each bundle is
/// built from its own declared repository and pinned revision, names its own
/// sources, and carries only its own eligible memory. Role is a relevance
/// hint, never an authorization claim.
#[test]
fn two_projects_never_leak_documents_revisions_or_memory() {
    let alpha_repo = repo();
    write(
        alpha_repo.path(),
        MANIFEST,
        manifest(
            "alpha",
            &[
                doc("alpha-scope", "scope", "docs/alpha/scope.md", true),
                doc(
                    "alpha-arch",
                    "architecture",
                    "docs/alpha/architecture.md",
                    true,
                ),
                Doc {
                    id: "alpha-qa",
                    kind: "validation",
                    path: "docs/alpha/qa.md",
                    title: "alpha-qa",
                    required: false,
                    roles: &["qa"],
                },
                Doc {
                    id: "alpha-release",
                    kind: "release",
                    path: "docs/alpha/release.md",
                    title: "alpha-release",
                    required: false,
                    roles: &["devops"],
                },
            ],
        ),
    );
    write(
        alpha_repo.path(),
        "docs/alpha/scope.md",
        "ALPHA-SCOPE-BODY\n",
    );
    write(
        alpha_repo.path(),
        "docs/alpha/architecture.md",
        "ALPHA-ARCH-BODY\n",
    );
    write(alpha_repo.path(), "docs/alpha/qa.md", "ALPHA-QA-BODY\n");
    write(
        alpha_repo.path(),
        "docs/alpha/release.md",
        "ALPHA-RELEASE-BODY\n",
    );
    let alpha_head = commit(alpha_repo.path(), "alpha context");

    let beta_repo = repo();
    write(
        beta_repo.path(),
        MANIFEST,
        manifest(
            "beta",
            &[
                doc("beta-design", "spec", "docs/beta/design.md", true),
                doc("beta-board", "validation", "docs/beta/board.md", true),
            ],
        ),
    );
    write(
        beta_repo.path(),
        "docs/beta/design.md",
        "BETA-DESIGN-BODY\n",
    );
    write(beta_repo.path(), "docs/beta/board.md", "BETA-BOARD-BODY\n");
    let beta_head = commit(beta_repo.path(), "beta context");

    let pm = pm_dir();
    add_project(pm.path(), "alpha", &[alpha_repo.path()], &["api"]);
    add_project(pm.path(), "beta", &[beta_repo.path()], &["web"]);
    eligible_memory(pm.path(), "alpha", "alpha-lesson", &["docs/alpha/**"]);
    eligible_memory(pm.path(), "beta", "beta-lesson", &["docs/beta/**"]);

    let alpha = bundle(pm.path(), "alpha", None, None);
    assert_eq!(alpha["project"], "alpha");
    assert_eq!(alpha["snapshot"]["head_revision"], alpha_head);
    assert_eq!(alpha["manifest"]["revision"], alpha_head);
    assert_eq!(alpha["snapshot"]["revision_state"], "uncompared");
    assert_eq!(
        documents(&alpha),
        ["alpha-scope", "alpha-arch", "alpha-qa", "alpha-release"]
    );
    assert_eq!(included(&alpha), ["alpha-lesson"]);
    assert_absent(&alpha, "BETA-DESIGN-BODY");
    assert_absent(&alpha, "beta-lesson");
    assert_absent(&alpha, "docs/beta/design.md");

    let beta = bundle(pm.path(), "beta", None, None);
    assert_eq!(beta["project"], "beta");
    assert_eq!(beta["snapshot"]["head_revision"], beta_head);
    assert_ne!(alpha_head, beta_head);
    assert_eq!(documents(&beta), ["beta-design", "beta-board"]);
    assert_eq!(included(&beta), ["beta-lesson"]);
    assert_absent(&beta, "ALPHA-SCOPE-BODY");
    assert_absent(&beta, "alpha-lesson");
    assert_absent(&beta, "docs/alpha/scope.md");

    // The role hint narrows optional documents only; required ones stay.
    let devops = bundle(pm.path(), "alpha", Some("devops"), None);
    assert_eq!(document(&devops, "alpha-release")["selected"], true);
    assert_eq!(document(&devops, "alpha-qa")["selected"], false);
    assert_eq!(
        document(&devops, "alpha-qa")["selection_reason"],
        "excluded:role-mismatch"
    );
    assert_eq!(document(&devops, "alpha-scope")["selected"], true);

    let qa = bundle(pm.path(), "alpha", Some("qa"), None);
    assert_eq!(document(&qa, "alpha-qa")["selected"], true);
    assert_eq!(document(&qa, "alpha-release")["selected"], false);
    assert_absent(&qa, "ALPHA-RELEASE-BODY");
}

/// Absent manifests, absent required documents and malformed manifests are
/// named states — never a silent "checked" claim, and never a partial
/// selection that pretends the manifest is usable.
#[test]
fn missing_and_conflicting_context_are_explicit() {
    let empty_repo = repo();
    write(empty_repo.path(), "README.md", "# no manifest here\n");
    let empty_head = commit(empty_repo.path(), "no manifest");

    let partial_repo = repo();
    write(
        partial_repo.path(),
        MANIFEST,
        manifest(
            "partial",
            &[
                doc("scope", "scope", "docs/scope.md", true),
                doc("arch", "architecture", "docs/architecture.md", true),
                doc("optional-design", "spec", "docs/design.md", false),
            ],
        ),
    );
    write(partial_repo.path(), "docs/scope.md", "PARTIAL-SCOPE\n");
    commit(partial_repo.path(), "partial context");

    let broken_repo = repo();
    write(
        broken_repo.path(),
        MANIFEST,
        "schema: 1\nproject: [unclosed\n",
    );
    write(broken_repo.path(), "docs/scope.md", "BROKEN-SCOPE\n");
    commit(broken_repo.path(), "broken manifest");

    let duplicate_repo = repo();
    write(
        duplicate_repo.path(),
        MANIFEST,
        manifest(
            "duplicate",
            &[
                doc("scope", "scope", "docs/scope.md", true),
                doc("scope-copy", "scope", "docs/scope.md", true),
            ],
        ),
    );
    write(duplicate_repo.path(), "docs/scope.md", "DUPLICATE-SCOPE\n");
    commit(duplicate_repo.path(), "duplicate path");

    let mismatch_repo = repo();
    write(
        mismatch_repo.path(),
        MANIFEST,
        manifest(
            "somebody-else",
            &[doc("scope", "scope", "docs/scope.md", true)],
        ),
    );
    write(mismatch_repo.path(), "docs/scope.md", "MISMATCH-SCOPE\n");
    commit(mismatch_repo.path(), "mismatched manifest");

    let oversized_repo = repo();
    let mut oversized = format!("# {}\n", "p".repeat(33 * 1024));
    oversized.push_str("schema: 1\nproject: oversized\ndocuments: []\n");
    write(oversized_repo.path(), MANIFEST, oversized);
    write(oversized_repo.path(), "docs/scope.md", "OVERSIZED-SCOPE\n");
    commit(oversized_repo.path(), "oversized manifest");

    let pm = pm_dir();
    for (key, repo) in [
        ("empty", &empty_repo),
        ("partial", &partial_repo),
        ("broken", &broken_repo),
        ("duplicate", &duplicate_repo),
        ("mismatch", &mismatch_repo),
        ("oversized", &oversized_repo),
    ] {
        add_project(pm.path(), key, &[repo.path()], &[]);
    }
    // An eligible lesson exists; a conflicted manifest must still select none.
    eligible_memory(pm.path(), "duplicate", "duplicate-lesson", &[]);

    let empty = bundle(pm.path(), "empty", None, None);
    assert_eq!(empty["state"], "missing");
    assert_eq!(empty["manifest"]["state"], "missing");
    assert_eq!(empty["manifest"]["path"], MANIFEST);
    assert_eq!(empty["snapshot"]["head_revision"], empty_head);
    assert!(empty["documents"].as_array().unwrap().is_empty());
    assert!(included(&empty).is_empty());

    let partial = bundle(pm.path(), "partial", None, None);
    assert_eq!(partial["state"], "missing");
    assert_eq!(partial["manifest"]["state"], "ready");
    let arch = document(&partial, "arch");
    assert_eq!(arch["state"], "missing");
    assert_eq!(arch["required"], true);
    assert_eq!(arch["selected"], true);
    assert!(arch.get("excerpt").is_none());
    assert!(arch.get("bytes").is_none());
    let optional = document(&partial, "optional-design");
    assert_eq!(optional["state"], "missing");
    assert_eq!(optional["required"], false);
    assert_eq!(document(&partial, "scope")["state"], "ready");

    let broken = bundle(pm.path(), "broken", None, None);
    assert_eq!(broken["state"], "conflict");
    assert_eq!(broken["manifest"]["state"], "conflict");
    assert!(!broken["manifest"]["errors"].as_array().unwrap().is_empty());
    assert!(broken["documents"].as_array().unwrap().is_empty());
    assert!(included(&broken).is_empty());

    let duplicate = bundle(pm.path(), "duplicate", None, None);
    assert_eq!(duplicate["state"], "conflict");
    let errors = serialized(&duplicate["manifest"]["errors"]);
    assert!(errors.contains("duplicate document path"), "{errors}");
    for id in documents(&duplicate) {
        let entry = document(&duplicate, &id);
        assert_eq!(entry["selected"], false, "{id}");
        assert_eq!(entry["state"], "not_selected", "{id}");
    }
    assert!(included(&duplicate).is_empty(), "{duplicate}");

    let mismatch = bundle(pm.path(), "mismatch", None, None);
    assert_eq!(mismatch["state"], "conflict");
    assert!(serialized(&mismatch["manifest"]["errors"]).contains("does not match"));

    let oversized = bundle(pm.path(), "oversized", None, None);
    assert_eq!(oversized["state"], "too_large");
    assert!(oversized["documents"].as_array().unwrap().is_empty());
    assert_absent(&oversized, "OVERSIZED-SCOPE");
}

/// A project whose local repository cannot be identified unambiguously is
/// named as such. Nothing is guessed from the first repo, the cwd, a name
/// prefix, or another project's checkout.
#[test]
fn ambiguous_and_unavailable_repositories_are_named_not_guessed() {
    let first = repo();
    write(first.path(), MANIFEST, manifest("two-repos", &[]));
    write(first.path(), "docs/scope.md", "FIRST-REPO-BODY\n");
    commit(first.path(), "first");
    let second = repo();
    write(second.path(), "docs/scope.md", "SECOND-REPO-BODY\n");
    commit(second.path(), "second");

    let nested_parent = repo();
    write(nested_parent.path(), "docs/scope.md", "NESTED-BODY\n");
    commit(nested_parent.path(), "parent");
    let nested = nested_parent.path().join("sub");
    std::fs::create_dir_all(&nested).unwrap();

    let linked_target = repo();
    write(linked_target.path(), MANIFEST, manifest("linked", &[]));
    write(linked_target.path(), "docs/scope.md", "LINKED-BODY\n");
    commit(linked_target.path(), "linked");
    let link_parent = tempfile::tempdir().unwrap();
    let link = link_parent.path().join("checkout");
    std::os::unix::fs::symlink(linked_target.path(), &link).unwrap();

    let pm = pm_dir();
    add_project(pm.path(), "two-repos", &[first.path(), second.path()], &[]);
    add_project(pm.path(), "no-repo", &[], &[]);
    add_project(pm.path(), "nested", &[nested.as_path()], &[]);
    add_project(pm.path(), "linked", &[link.as_path()], &[]);
    write(
        pm.path(),
        "relative/project.yaml",
        "key: relative\nprefix: RELATIVE\nrepos:\n  - path: \"relative/checkout\"\n",
    );

    let two = bundle(pm.path(), "two-repos", None, None);
    assert_eq!(two["state"], "ambiguous_repository");
    assert_eq!(two["snapshot"]["head_revision"], Value::Null);
    assert_eq!(two["snapshot"]["repo_identity"], Value::Null);
    assert!(two["documents"].as_array().unwrap().is_empty());
    assert_absent(&two, "FIRST-REPO-BODY");
    assert_absent(&two, "SECOND-REPO-BODY");

    let none = bundle(pm.path(), "no-repo", None, None);
    assert_eq!(none["state"], "missing_repository");
    assert!(none["documents"].as_array().unwrap().is_empty());

    let nested_bundle = bundle(pm.path(), "nested", None, None);
    assert_eq!(nested_bundle["state"], "unavailable_repository");
    assert!(serialized(&nested_bundle["snapshot"]["error"]).contains("not the repository root"));
    assert_absent(&nested_bundle, "NESTED-BODY");

    let linked_bundle = bundle(pm.path(), "linked", None, None);
    assert_eq!(linked_bundle["state"], "unavailable_repository");
    assert!(serialized(&linked_bundle["snapshot"]["error"]).contains("symlink"));
    assert_absent(&linked_bundle, "LINKED-BODY");

    let relative_bundle = bundle(pm.path(), "relative", None, None);
    assert_eq!(relative_bundle["state"], "unavailable_repository");
    assert!(serialized(&relative_bundle["snapshot"]["error"]).contains("absolute"));
}

/// A source that moved after a request is a recheck signal: both revisions
/// are named and the bundle never claims to be latest-main. A dirty working
/// tree is reported but never served — the pinned commit's bytes are.
#[test]
fn stale_revision_and_dirty_tree_never_serve_worktree_bytes() {
    let repo = repo();
    write(
        repo.path(),
        MANIFEST,
        manifest("race", &[doc("scope", "scope", "docs/scope.md", true)]),
    );
    write(repo.path(), "docs/scope.md", "COMMITTED-REVISION-ONE\n");
    let first = commit(repo.path(), "rev one");

    let pm = pm_dir();
    add_project(pm.path(), "race", &[repo.path()], &[]);

    let initial = bundle(pm.path(), "race", None, None);
    assert_eq!(initial["snapshot"]["revision_state"], "uncompared");
    assert_eq!(
        document(&initial, "scope")["excerpt"],
        "COMMITTED-REVISION-ONE\n"
    );

    write(repo.path(), "docs/scope.md", "COMMITTED-REVISION-TWO\n");
    let second = commit(repo.path(), "rev two");
    assert_ne!(first, second);

    let stale = bundle(pm.path(), "race", None, Some(&first));
    assert_eq!(stale["snapshot"]["revision_state"], "stale");
    assert_eq!(stale["snapshot"]["expected_revision"], first);
    assert_eq!(stale["snapshot"]["head_revision"], second);
    assert_eq!(stale["manifest"]["revision"], second);
    assert_eq!(
        document(&stale, "scope")["excerpt"],
        "COMMITTED-REVISION-TWO\n"
    );

    let current = bundle(pm.path(), "race", None, Some(&second));
    assert_eq!(current["snapshot"]["revision_state"], "current");
    assert_eq!(current["snapshot"]["expected_revision"], second);

    // An uncommitted edit is a dirty worktree, not a new source revision.
    write(repo.path(), "docs/scope.md", "WORKTREE-ONLY-BODY\n");
    let dirty = bundle(pm.path(), "race", None, Some(&second));
    assert_eq!(dirty["snapshot"]["dirty"], true);
    assert_eq!(dirty["snapshot"]["head_revision"], second);
    assert_eq!(dirty["snapshot"]["revision_state"], "current");
    assert_eq!(
        document(&dirty, "scope")["excerpt"],
        "COMMITTED-REVISION-TWO\n"
    );
    assert_absent(&dirty, "WORKTREE-ONLY-BODY");
}

/// The documented allowlist, tracked-blob mode checks and bounded reads are
/// the read boundary. Escapes, symlinks, directories, sensitive trees and
/// oversized blobs are refused with a named state and no target bytes.
#[test]
fn path_isolation_refuses_escapes_symlinks_directories_and_oversized_blobs() {
    let isolated = repo();
    write(isolated.path(), "secret.md", "TOP-SECRET-MARKER\n");
    write(isolated.path(), ".cadence/private.md", "PRIVATE-MARKER\n");
    write(isolated.path(), "docs/token/keys.md", "TOKEN-MARKER\n");
    write(
        isolated.path(),
        "docs/real-target.md",
        "REAL-TARGET-MARKER\n",
    );
    std::os::unix::fs::symlink("../secret.md", isolated.path().join("docs/link.md")).unwrap();
    write(isolated.path(), "docs/dir.md/inner.md", "DIR-MARKER\n");
    write(isolated.path(), "docs/big.md", "b".repeat(64 * 1024 + 1));
    write(
        isolated.path(),
        MANIFEST,
        manifest(
            "isolation",
            &[
                doc("scope", "scope", "docs/scope.md", true),
                doc("link", "spec", "docs/link.md", false),
                doc("dir", "spec", "docs/dir.md", false),
                doc("big", "spec", "docs/big.md", false),
            ],
        ),
    );
    write(isolated.path(), "docs/scope.md", "ISOLATION-SCOPE\n");
    commit(isolated.path(), "isolation fixture");

    let pm = pm_dir();
    add_project(pm.path(), "isolation", &[isolated.path()], &[]);
    let isolation = bundle(pm.path(), "isolation", None, None);
    assert_eq!(document(&isolation, "scope")["state"], "ready");
    assert_eq!(document(&isolation, "link")["state"], "unreadable");
    assert!(serialized(&document(&isolation, "link")["reason"]).contains("symlink"));
    assert_eq!(document(&isolation, "dir")["state"], "unreadable");
    assert_eq!(document(&isolation, "big")["state"], "too_large");
    for id in ["link", "dir", "big"] {
        assert!(document(&isolation, id).get("excerpt").is_none(), "{id}");
    }
    // Optional failures do not fail the whole bundle, but they are never
    // silently dropped or served.
    assert_eq!(isolation["state"], "ready");
    assert_absent(&isolation, "TOP-SECRET-MARKER");
    assert_absent(&isolation, "REAL-TARGET-MARKER");

    let escape_repo = repo();
    write(escape_repo.path(), "secret.md", "ESCAPE-SECRET-MARKER\n");
    write(
        escape_repo.path(),
        ".cadence/private.md",
        "ESCAPE-PRIVATE-MARKER\n",
    );
    write(
        escape_repo.path(),
        "docs/token/keys.md",
        "ESCAPE-TOKEN-MARKER\n",
    );
    write(
        escape_repo.path(),
        MANIFEST,
        manifest(
            "escape",
            &[
                doc("up", "scope", "../secret.md", true),
                doc("absolute", "scope", "/etc/passwd", true),
                doc("traverse", "scope", "docs/../../secret.md", false),
                doc("cadence", "spec", ".cadence/private.md", false),
                doc("token", "spec", "docs/token/keys.md", false),
            ],
        ),
    );
    commit(escape_repo.path(), "escape fixture");
    add_project(pm.path(), "escape", &[escape_repo.path()], &[]);

    let escape = bundle(pm.path(), "escape", None, None);
    assert_eq!(escape["state"], "conflict");
    for id in ["up", "absolute", "traverse", "cadence", "token"] {
        assert_eq!(document(&escape, id)["state"], "invalid_path", "{id}");
        assert_eq!(document(&escape, id)["selected"], false, "{id}");
        assert!(document(&escape, id).get("excerpt").is_none(), "{id}");
    }
    assert_absent(&escape, "ESCAPE-SECRET-MARKER");
    assert_absent(&escape, "ESCAPE-PRIVATE-MARKER");
    assert_absent(&escape, "ESCAPE-TOKEN-MARKER");
    assert_absent(&escape, "root:x:");
}

/// The bundle is a pure read: the same request repeated (serially or
/// concurrently) is byte-identical, and a restart re-derives the same context
/// from the pinned revision and the on-disk memory store. A later commit is a
/// new revision, not a silent overwrite of the previous one.
#[test]
fn duplicate_requests_and_restart_recover_the_same_context() {
    let repo = repo();
    write(
        repo.path(),
        MANIFEST,
        manifest("steady", &[doc("scope", "scope", "docs/scope.md", true)]),
    );
    write(repo.path(), "docs/scope.md", "STEADY-REVISION-ONE\n");
    let first = commit(repo.path(), "steady one");

    let pm = pm_dir();
    add_project(pm.path(), "steady", &[repo.path()], &[]);
    eligible_memory(pm.path(), "steady", "steady-lesson", &["docs/scope.md"]);
    proposed_memory(pm.path(), "steady", "steady-proposal");
    write(
        pm.path(),
        "steady/memory/broken.md",
        "this is not a frontmatter fence\n",
    );

    let before = memory_snapshot(pm.path());
    let one = serialized(&bundle(pm.path(), "steady", None, None));
    let two = serialized(&bundle(pm.path(), "steady", None, None));
    assert_eq!(one, two, "a repeated request must not change the bundle");

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let pm_path = pm.path().to_path_buf();
            std::thread::spawn(move || serialized(&bundle(&pm_path, "steady", None, None)))
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap(), one, "concurrent requests differ");
    }

    let bundle_value: Value = serde_json::from_str(&one).unwrap();
    assert_eq!(bundle_value["snapshot"]["head_revision"], first);
    assert_eq!(included(&bundle_value), ["steady-lesson"]);
    assert_eq!(
        reason_for(&bundle_value, "steady-proposal"),
        "review blocked: memory status is proposed"
    );
    let load_errors = bundle_value["memories"]["load_errors"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(load_errors.len(), 1, "{load_errors:?}");
    assert!(load_errors[0].as_str().unwrap().contains("broken.md"));
    assert_eq!(bundle_value["memories"]["load_errors_total"], 1);

    // Restart: a fresh registry read, memory load and bundle. No in-process
    // state is carried, so the recovery must reproduce the same answer.
    let restarted = serialized(&bundle(pm.path(), "steady", None, None));
    assert_eq!(restarted, one, "restart changed the recovered context");
    assert_eq!(
        memory_snapshot(pm.path()),
        before,
        "reading context must not promote or rewrite memory"
    );

    write(repo.path(), "docs/scope.md", "STEADY-REVISION-TWO\n");
    let second = commit(repo.path(), "steady two");
    let moved = bundle(pm.path(), "steady", None, Some(&first));
    assert_eq!(moved["snapshot"]["revision_state"], "stale");
    assert_eq!(moved["snapshot"]["head_revision"], second);
    assert_eq!(
        document(&moved, "scope")["excerpt"],
        "STEADY-REVISION-TWO\n"
    );
    // Memory is re-derived from disk, so the eligible lesson survives the
    // restart and the revision move; the withholding reasons do too.
    assert_eq!(included(&moved), ["steady-lesson"]);
    assert!(reason_for(&moved, "steady-proposal").contains("proposed"));
}

/// Only the quorum-aware renderer's eligible, applicable records are
/// included; everything else is explained. The renderer's caps hold, and no
/// lesson gains authority from being displayed.
#[test]
fn memory_selection_is_bounded_eligible_and_never_grants_authority() {
    let repo = repo();
    write(
        repo.path(),
        MANIFEST,
        manifest("mem", &[doc("scope", "scope", "docs/scope.md", true)]),
    );
    write(repo.path(), "docs/scope.md", "MEM-SCOPE\n");
    commit(repo.path(), "mem fixture");

    let pm = pm_dir();
    add_project(pm.path(), "mem", &[repo.path()], &[]);
    for index in 0..15 {
        eligible_memory(
            pm.path(),
            "mem",
            &format!("eligible-{index:02}"),
            &["docs/**"],
        );
    }
    proposed_memory(pm.path(), "mem", "proposal-only");
    edited_digest_memory(pm.path(), "mem", "edited-claim");
    let stale_path = eligible_memory(pm.path(), "mem", "decayed-lesson", &["docs/**"]);
    let stale_text = std::fs::read_to_string(&stale_path).unwrap();
    std::fs::write(
        &stale_path,
        stale_text.replace(
            "status: accepted",
            "status: accepted\nstale: \"the cited source was rewritten\"",
        ),
    )
    .unwrap();
    // A record outside the selected manifest's paths is not applicable.
    eligible_memory(pm.path(), "mem", "unrelated-lesson", &["src/**"]);

    let before = memory_snapshot(pm.path());
    let bundle = bundle(pm.path(), "mem", None, None);

    let selected = included(&bundle);
    assert_eq!(
        selected.len(),
        memory::LESSON_MAX_ENTRIES,
        "the renderer cap bounds inclusion: {bundle}"
    );
    assert!(selected.iter().all(|id| id.starts_with("eligible-")));
    let lessons = bundle["memories"]["lessons"].as_str().unwrap();
    assert!(lessons.len() <= memory::LESSON_MAX_BYTES);
    assert!(lessons.contains("eligible-00"));

    assert_eq!(
        reason_for(&bundle, "proposal-only"),
        "review blocked: memory status is proposed"
    );
    assert!(reason_for(&bundle, "edited-claim").contains("finalization"));
    assert!(reason_for(&bundle, "decayed-lesson").contains("stale"));
    assert!(reason_for(&bundle, "unrelated-lesson").contains("not applicable"));
    // The over-cap eligible records are named as omitted, not dropped.
    assert!(withheld(&bundle)
        .iter()
        .any(|(id, reason)| id.starts_with("eligible-") && reason.contains("cap")));
    assert_eq!(bundle["memories"]["matched_total"], 15);

    // Displayed evidence carries no authority: no permission, rule or
    // dispatch fields, and the store on disk is untouched.
    for memory in bundle["memories"]["included"].as_array().unwrap() {
        let mut keys: Vec<&str> = memory
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["confidence", "evidence", "id", "kind", "verified_at"],
            "unexpected memory authority field: {memory}"
        );
    }
    let text = serialized(&bundle);
    for forbidden in [
        "\"authority\"",
        "\"permissions\"",
        "\"dispatch\"",
        "\"rules\"",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
    assert_eq!(
        memory_snapshot(pm.path()),
        before,
        "context delivery must not promote, edit or re-write lessons"
    );
}

/// The response cap is a hard contract: an oversized bundle shortens excerpts
/// and says so, but never drops document identity, path, state or size.
#[test]
fn response_bound_shortens_excerpts_without_dropping_documents() {
    let repo = repo();
    let mut docs = String::new();
    for index in 0..20 {
        let path = format!("docs/doc-{index:02}.md");
        write(repo.path(), &path, "x".repeat(16 * 1024));
        docs.push_str(&format!(
            "  - id: doc-{index:02}\n    kind: spec\n    path: \"{path}\"\n    title: large document\n    required: true\n"
        ));
    }
    write(
        repo.path(),
        MANIFEST,
        format!("schema: 1\nproject: bounded\ndocuments:\n{docs}"),
    );
    commit(repo.path(), "bounded fixture");

    let pm = pm_dir();
    add_project(pm.path(), "bounded", &[repo.path()], &[]);
    let bounded = bundle(pm.path(), "bounded", None, None);

    assert_eq!(bounded["state"], "ready");
    assert_eq!(bounded["documents"].as_array().unwrap().len(), 20);
    assert_eq!(bounded["limits"]["response_truncated"], true);
    assert!(
        bounded["limits"]["response_excerpt_reductions"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(bounded["limits"]["max_response_bytes"], 256 * 1024);
    assert!(serialized(&bounded).len() <= 256 * 1024);
    // Only the excerpts needed to fit the cap are shortened; every document
    // keeps its identity, path, state and recorded size.
    let mut shortened = 0;
    for entry in bounded["documents"].as_array().unwrap() {
        assert_eq!(entry["state"], "ready");
        assert_eq!(entry["required"], true);
        assert_eq!(entry["bytes"], 16 * 1024);
        assert!(
            entry["excerpt"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "{entry}"
        );
        if entry["truncated"] == true {
            shortened += 1;
        }
        assert!(entry["path"].as_str().unwrap().starts_with("docs/doc-"));
    }
    assert!(shortened > 0, "the cap must shorten something: {bounded}");
}

/// A memory file's frontmatter is untrusted: the on-disk loader parses it
/// without a slug or length check, so a proposed record can carry an id larger
/// than the whole response budget. Delivery bounds it, and the response stays
/// inside the contract even when the manifest has no excerpts to shorten.
#[test]
fn oversized_memory_metadata_is_bounded_at_delivery() {
    let normal_repo = repo();
    write(
        normal_repo.path(),
        MANIFEST,
        manifest(
            "oversized-memory",
            &[doc("scope", "scope", "docs/scope.md", true)],
        ),
    );
    write(
        normal_repo.path(),
        "docs/scope.md",
        "OVERSIZED-SCOPE-BODY\n",
    );
    commit(normal_repo.path(), "oversized memory fixture");

    let blank_repo = repo();
    write(
        blank_repo.path(),
        MANIFEST,
        "schema: 1\nproject: blank\ndocuments: []\n",
    );
    commit(blank_repo.path(), "empty document manifest");

    let pm = pm_dir();
    add_project(pm.path(), "oversized-memory", &[normal_repo.path()], &[]);
    add_project(pm.path(), "blank", &[blank_repo.path()], &[]);

    let oversized_id = "z".repeat(300 * 1024);
    for key in ["oversized-memory", "blank"] {
        memory_file(
            pm.path(),
            key,
            "oversized-proposal",
            &format!(
                "id: {oversized_id}\ntype: rule\nstatus: proposed\nscope:\n  paths:\n    - \"docs/**\"\nconfidence: medium\ncreated: 2026-09-01T00:00:00Z\nauthor: cad224-author\n"
            ),
            LESSON_BODY,
        );
    }

    // The 300 KiB id never reaches the response: it is shortened in place and
    // the entry keeps its status and its explanation.
    let normal = bundle(pm.path(), "oversized-memory", None, None);
    assert!(serialized(&normal).len() <= 256 * 1024);
    assert!(serialized(&normal).len() < 64 * 1024);
    assert_eq!(normal["state"], "ready");
    assert_eq!(normal["memories"]["withheld_total"], 1);
    assert_eq!(normal["memories"]["withheld_omitted"], 0);
    let entry = &normal["memories"]["withheld"][0];
    assert_eq!(entry["id"].as_str().unwrap().len(), 128);
    assert_eq!(entry["status"], "proposed");
    assert!(entry["reason"]
        .as_str()
        .unwrap()
        .contains("memory status is proposed"));
    assert_eq!(
        document(&normal, "scope")["excerpt"],
        "OVERSIZED-SCOPE-BODY\n"
    );
    assert_absent(&normal, &"z".repeat(129));

    // A valid empty-document manifest has no excerpt to shorten at all; the
    // same bound must hold there too.
    let blank = bundle(pm.path(), "blank", None, None);
    assert!(serialized(&blank).len() <= 256 * 1024);
    assert!(serialized(&blank).len() < 64 * 1024);
    assert_eq!(blank["state"], "ready");
    assert!(blank["documents"].as_array().unwrap().is_empty());
    assert_eq!(blank["memories"]["withheld_total"], 1);
    assert_eq!(
        blank["memories"]["withheld"][0]["id"]
            .as_str()
            .unwrap()
            .len(),
        128
    );
    assert_absent(&blank, &"z".repeat(129));
}
