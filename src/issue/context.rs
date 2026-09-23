//! Bounded, read-only project documentation context.
//!
//! The context reader is deliberately narrower than a file server: it reads
//! one fixed, tracked manifest and the manifest's tracked text blobs from the
//! selected project's declared repository at one immutable HEAD revision.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::issue::{project, Pm};
use crate::memory;
use crate::proc::{self, BoundedError};

pub const DEFAULT_MANIFEST_PATH: &str = "docs/cadence/project-context.yaml";
pub const MAX_MANIFEST_BYTES: usize = 32 * 1024;
pub const MAX_MANIFEST_ENTRIES: usize = 32;
pub const MAX_PATH_BYTES: usize = 256;
pub const MAX_TITLE_BYTES: usize = 160;
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
pub const MAX_EXCERPT_BYTES: usize = 16 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(3);
const ROLES: &[&str] = &["pm", "dev", "qa", "ops"];
const KINDS: &[&str] = &[
    "index",
    "scope",
    "architecture",
    "spec",
    "adr",
    "validation",
    "release",
];
const DOCUMENT_EXTENSIONS: &[&str] = &["md", "markdown"];
const ROOT_DOCUMENTS: &[&str] = &[
    "README.md",
    "ARCHITECTURE.md",
    "DESIGN.md",
    "CONTRIBUTING.md",
    "AGENTS.md",
];
// Exact path segments blocked before any Git lookup. The document allowlist
// below is still the primary boundary; these names keep dependency, build,
// runtime, and credential trees out even when they contain Markdown.
const DENIED_COMPONENTS: &[&str] = &[
    ".git",
    ".cadence",
    "target",
    "node_modules",
    "runtime",
    "credentials",
    "vendor",
    "build",
    "dist",
    "coverage",
    "tmp",
    "secret",
    "secrets",
    "password",
    "passwords",
    "token",
    "tokens",
    "key",
    "keys",
    "private",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextManifest {
    pub schema: u32,
    pub project: String,
    pub documents: Vec<ManifestDocument>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestDocument {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub title: String,
    pub required: bool,
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoSnapshot {
    pub head_revision: Option<String>,
    pub expected_revision: Option<String>,
    pub revision_state: String,
    pub dirty: Option<bool>,
    pub dirty_truncated: bool,
    pub repo_identity: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
struct RepoSource {
    root: PathBuf,
    head: String,
    snapshot: RepoSnapshot,
}

#[derive(Clone, Debug)]
enum BlobError {
    State(&'static str, String),
}

#[derive(Clone, Debug)]
struct SourceError {
    state: &'static str,
    message: String,
}

impl SourceError {
    fn new(state: &'static str, message: impl Into<String>) -> Self {
        Self {
            state,
            message: message.into(),
        }
    }
}

impl BlobError {
    fn state(&self) -> &'static str {
        match self {
            Self::State(state, _) => state,
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::State(_, reason) => reason.clone(),
        }
    }
}

fn bounded_git(root: &Path, args: &[String]) -> Result<Vec<u8>, String> {
    bounded_git_with_limit(root, args, MAX_GIT_OUTPUT_BYTES)
}

fn bounded_git_with_limit(
    root: &Path,
    args: &[String],
    output_limit: usize,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("--no-optional-locks")
        .env("GIT_OPTIONAL_LOCKS", "0");
    for arg in args {
        cmd.arg(arg);
    }
    let (output, bounds) =
        proc::run_bounded_limited(&mut cmd, GIT_TIMEOUT, output_limit).map_err(|e| match e {
            BoundedError::TimedOut { .. } => "git operation timed out".to_string(),
            BoundedError::Spawn(_) => "git could not be started".to_string(),
            BoundedError::Wait(_) => "git operation could not be reaped".to_string(),
        })?;
    if bounds.stdout_exceeded {
        return Err("git stdout exceeded the bounded limit".to_string());
    }
    if bounds.stderr_exceeded {
        return Err("git stderr exceeded the bounded limit".to_string());
    }
    if !output.status.success() {
        return Err("git operation failed".to_string());
    }
    Ok(output.stdout)
}

fn git_text(root: &Path, args: &[String]) -> Result<String, String> {
    let bytes = bounded_git(root, args)?;
    String::from_utf8(bytes).map_err(|_| "git returned non-UTF-8 output".to_string())
}

pub fn valid_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn dirty_probe(root: &Path) -> (Option<bool>, bool, Option<String>) {
    let args = vec![
        "status".to_string(),
        "--porcelain=v1".to_string(),
        "--untracked-files=no".to_string(),
    ];
    match bounded_git(root, &args) {
        Ok(bytes) => (Some(!bytes.is_empty()), false, None),
        Err(error) if error.contains("stdout exceeded") => (
            Some(true),
            true,
            Some("tracked status exceeded the bounded output; dirty=true".to_string()),
        ),
        Err(error) => (
            None,
            false,
            Some(format!("working tree state unavailable: {error}")),
        ),
    }
}

fn resolve_source(
    selected: &project::Project,
    expected: Option<&str>,
) -> Result<RepoSource, SourceError> {
    let local: Vec<&project::Repo> = selected
        .repos
        .iter()
        .filter(|repo| {
            repo.path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty())
        })
        .collect();
    if local.is_empty() {
        return Err(SourceError::new(
            "missing_repository",
            "project declares no local repository",
        ));
    }
    if local.len() > 1 {
        return Err(SourceError::new(
            "ambiguous_repository",
            "project declares multiple local repositories",
        ));
    }
    let repo = local[0];
    let declared = project::expand_home(repo.path.as_deref().unwrap_or_default());
    if !declared.is_absolute() {
        return Err(SourceError::new(
            "unavailable_repository",
            "declared repository path must be absolute",
        ));
    }
    let metadata = declared.symlink_metadata().map_err(|_| {
        SourceError::new(
            "unavailable_repository",
            "declared repository path is missing",
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(SourceError::new(
            "unavailable_repository",
            "declared repository path is a symlink",
        ));
    }
    if !metadata.is_dir() {
        return Err(SourceError::new(
            "unavailable_repository",
            "declared repository path is not a directory",
        ));
    }
    let root = declared.canonicalize().map_err(|_| {
        SourceError::new(
            "unavailable_repository",
            "declared repository path is unreadable",
        )
    })?;
    let top = git_text(
        &root,
        &["rev-parse".to_string(), "--show-toplevel".to_string()],
    )
    .map_err(|error| SourceError::new("unavailable_repository", error))?
    .trim()
    .to_string();
    let top = PathBuf::from(top).canonicalize().map_err(|_| {
        SourceError::new(
            "unavailable_repository",
            "git repository root is unreadable",
        )
    })?;
    if top != root {
        return Err(SourceError::new(
            "unavailable_repository",
            "declared path is not the repository root",
        ));
    }
    let head = git_text(
        &root,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            "HEAD".to_string(),
        ],
    )
    .map_err(|error| SourceError::new("unavailable_repository", error))?
    .trim()
    .to_string();
    if !valid_revision(&head) {
        return Err(SourceError::new(
            "unavailable_repository",
            "repository HEAD is not a full SHA-1 revision",
        ));
    }
    let (dirty, dirty_truncated, dirty_error) = dirty_probe(&root);
    let revision_state = match expected {
        Some(value) if value != head => "stale",
        Some(_) => "current",
        None => "uncompared",
    };
    let identity = repo
        .remote
        .as_deref()
        .filter(|remote| !remote.trim().is_empty())
        .map(project::normalize_remote)
        .filter(|remote| {
            !remote.is_empty()
                && !remote.contains("..")
                && remote.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/')
                })
        })
        .or_else(|| {
            root.file_name()
                .map(|name| format!("declared:{}", shorten(&name.to_string_lossy(), 128).0))
        });
    Ok(RepoSource {
        root,
        head: head.clone(),
        snapshot: RepoSnapshot {
            head_revision: Some(head),
            expected_revision: expected.map(str::to_string),
            revision_state: revision_state.to_string(),
            dirty,
            dirty_truncated,
            repo_identity: identity,
            error: dirty_error,
        },
    })
}

fn path_error(path: &str) -> Option<String> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.starts_with('/') {
        return Some("path must be a bounded relative path".to_string());
    }
    if path
        .bytes()
        .any(|byte| byte.is_ascii_control() || matches!(byte, b'*' | b'?' | b'[' | b']' | b':'))
        || path.contains('\\')
    {
        return Some("path contains a forbidden character".to_string());
    }
    let components: Vec<&str> = path.split('/').collect();
    if components
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Some("path contains an invalid component".to_string());
    }
    if components.iter().any(|part| {
        DENIED_COMPONENTS.contains(part)
            || part.starts_with('.')
            || part.starts_with(".env")
            || part.ends_with(".pem")
            || part.ends_with(".key")
            || part.ends_with(".p12")
            || part.eq_ignore_ascii_case("id_rsa")
    }) {
        return Some("path is outside the documentation allowlist".to_string());
    }
    let is_docs_markdown = components.first() == Some(&"docs")
        && components.len() > 1
        && Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| DOCUMENT_EXTENSIONS.contains(&ext))
            .unwrap_or(false);
    let is_root_document = components.len() == 1 && ROOT_DOCUMENTS.contains(&path);
    if !is_docs_markdown && !is_root_document {
        return Some("path must have a documentation extension".to_string());
    }
    None
}

fn id_error(id: &str) -> Option<String> {
    if id.is_empty()
        || id.len() > 48
        || id.starts_with('-')
        || id.ends_with('-')
        || id.contains("--")
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        Some("id must use lowercase letters, digits, and single hyphens".to_string())
    } else {
        None
    }
}

fn validate_manifest(manifest: &ContextManifest, selected: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if manifest.schema != 1 {
        errors.push("unsupported manifest schema".to_string());
    }
    if manifest.project != selected {
        errors.push("manifest project does not match selected project".to_string());
    }
    if manifest.documents.len() > MAX_MANIFEST_ENTRIES {
        errors.push("manifest has too many documents".to_string());
    }
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for document in manifest.documents.iter().take(MAX_MANIFEST_ENTRIES) {
        if let Some(error) = id_error(&document.id) {
            errors.push(format!("{}: {error}", document.id));
        }
        if !ids.insert(document.id.clone()) {
            errors.push(format!("duplicate document id: {}", document.id));
        }
        if !paths.insert(document.path.clone()) {
            errors.push(format!("duplicate document path: {}", document.path));
        }
        if let Some(error) = path_error(&document.path) {
            errors.push(format!("{}: {error}", document.id));
        }
        if !KINDS.contains(&document.kind.as_str()) {
            errors.push(format!("{}: unsupported document kind", document.id));
        }
        if document.title.is_empty() || document.title.len() > MAX_TITLE_BYTES {
            errors.push(format!("{}: title is too large or empty", document.id));
        }
        if document
            .roles
            .iter()
            .any(|role| !ROLES.contains(&role.as_str()))
        {
            errors.push(format!("{}: unsupported role", document.id));
        }
    }
    errors
}

#[derive(Debug)]
struct TreeEntry {
    mode: String,
    kind: String,
    object: String,
}

fn tree_entry(root: &Path, head: &str, path: &str) -> Result<Option<TreeEntry>, BlobError> {
    let args = vec![
        "ls-tree".to_string(),
        "-z".to_string(),
        head.to_string(),
        "--".to_string(),
        path.to_string(),
    ];
    let output = bounded_git(root, &args).map_err(|error| BlobError::State("unreadable", error))?;
    if output.is_empty() {
        return Ok(None);
    }
    let record = output.split(|byte| *byte == 0).next().unwrap_or_default();
    let record = String::from_utf8(record.to_vec())
        .map_err(|_| BlobError::State("unreadable", "tree entry is not UTF-8".to_string()))?;
    let (metadata, found_path) = record
        .split_once('\t')
        .ok_or_else(|| BlobError::State("unreadable", "malformed tree entry".to_string()))?;
    if found_path != path {
        return Ok(None);
    }
    let mut parts = metadata.split_whitespace();
    let mode = parts.next().unwrap_or_default().to_string();
    let kind = parts.next().unwrap_or_default().to_string();
    let object = parts.next().unwrap_or_default().to_string();
    if mode.is_empty() || kind.is_empty() || object.is_empty() {
        return Err(BlobError::State(
            "unreadable",
            "malformed tree entry".to_string(),
        ));
    }
    Ok(Some(TreeEntry { mode, kind, object }))
}

fn read_blob(root: &Path, head: &str, path: &str) -> Result<Option<String>, BlobError> {
    let parts: Vec<&str> = path.split('/').collect();
    for index in 0..parts.len() {
        let prefix = parts[..=index].join("/");
        let Some(entry) = tree_entry(root, head, &prefix)? else {
            return Ok(None);
        };
        let final_entry = index + 1 == parts.len();
        if !final_entry {
            if entry.mode == "120000" || entry.kind == "commit" {
                return Err(BlobError::State(
                    "unreadable",
                    "symlink or submodule parent is rejected".to_string(),
                ));
            }
            if entry.kind != "tree" {
                return Err(BlobError::State(
                    "unreadable",
                    "document parent is not a tracked tree".to_string(),
                ));
            }
            continue;
        }
        if entry.mode == "120000" {
            return Err(BlobError::State(
                "unreadable",
                "tracked symlink is rejected".to_string(),
            ));
        }
        if entry.kind != "blob" || !matches!(entry.mode.as_str(), "100644" | "100755") {
            return Err(BlobError::State(
                "unreadable",
                "document is not a regular tracked blob".to_string(),
            ));
        }
        let output = bounded_git_with_limit(
            root,
            &["cat-file".to_string(), "blob".to_string(), entry.object],
            MAX_DOCUMENT_BYTES + 1,
        )
        .map_err(|error| {
            if error.contains("stdout exceeded") {
                BlobError::State(
                    "too_large",
                    format!("document exceeds {MAX_DOCUMENT_BYTES} bytes"),
                )
            } else {
                BlobError::State("unreadable", error)
            }
        })?;
        if output.len() > MAX_DOCUMENT_BYTES {
            return Err(BlobError::State(
                "too_large",
                format!("document exceeds {MAX_DOCUMENT_BYTES} bytes"),
            ));
        }
        if output.contains(&0) {
            return Err(BlobError::State(
                "unreadable",
                "document contains binary data".to_string(),
            ));
        }
        let text = String::from_utf8(output)
            .map_err(|_| BlobError::State("unreadable", "document is not UTF-8".to_string()))?;
        return Ok(Some(text));
    }
    Ok(None)
}

fn shorten(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn memory_context(pm: &Pm, selected: &project::Project, paths: &[String]) -> Value {
    let (pool, load_errors) = memory::load_project_report(&pm.dir, &selected.key);
    let context = memory::MatchCtx {
        components: selected.components.clone(),
        paths: paths.to_vec(),
        providers: Vec::new(),
        tags: Vec::new(),
    };
    let fresh = memory::Freshness::for_project(Some(selected));
    let matched = memory::match_memories(&pool, &context, &fresh);
    let (lessons, slugs) = memory::render_lessons(&matched);
    let included_ids: HashSet<&str> = slugs.iter().map(String::as_str).collect();
    let included: Vec<Value> = matched
        .lessons
        .iter()
        .filter(|memory| included_ids.contains(memory.front.id.as_str()))
        .map(|memory| {
            json!({
                "id": memory.front.id,
                "kind": memory.front.kind,
                "confidence": memory.front.confidence,
                "verified_at": memory::last_verified(memory),
                "evidence": matched.label(memory),
            })
        })
        .collect();
    let matched_ids: HashSet<&str> = matched
        .lessons
        .iter()
        .map(|memory| memory.front.id.as_str())
        .collect();
    let stale: std::collections::HashMap<&str, &str> = matched
        .withheld
        .iter()
        .map(|(memory, reason)| (memory.front.id.as_str(), reason.as_str()))
        .collect();
    let withheld_all: Vec<Value> = pool
        .iter()
        .filter(|memory| !included_ids.contains(memory.front.id.as_str()))
        .map(|memory| {
            let reason = if matched_ids.contains(memory.front.id.as_str()) {
                "lesson renderer omitted this eligible entry at its cap".to_string()
            } else if let Some(reason) = stale.get(memory.front.id.as_str()) {
                reason.to_string()
            } else if memory.front.status == "accepted" {
                let (eligible, reason) = memory::retrieval_status(memory);
                if !eligible {
                    reason
                } else {
                    "not applicable to the selected context".to_string()
                }
            } else {
                format!("review blocked: memory status is {}", memory.front.status)
            };
            json!({
                "id": memory.front.id,
                "status": memory.front.status,
                "reason": shorten(&reason, 256).0,
            })
        })
        .collect();
    let withheld_total = withheld_all.len();
    let withheld = withheld_all
        .into_iter()
        .take(MAX_MANIFEST_ENTRIES)
        .collect::<Vec<_>>();
    let withheld_omitted = withheld_total.saturating_sub(withheld.len());
    let load_errors_total = load_errors.len();
    let errors: Vec<String> = load_errors
        .into_iter()
        .take(MAX_MANIFEST_ENTRIES)
        .map(|error| shorten(&error, 256).0)
        .collect();
    let load_errors_omitted = load_errors_total.saturating_sub(errors.len());
    json!({
        "included": included,
        "lessons": lessons,
        "withheld": withheld,
        "withheld_total": withheld_total,
        "withheld_omitted": withheld_omitted,
        "load_errors": errors,
        "load_errors_total": load_errors_total,
        "load_errors_omitted": load_errors_omitted,
        "matched_total": matched.lessons.len(),
    })
}

fn document_value(
    document: &ManifestDocument,
    selected: bool,
    selection_reason: &str,
    result: Result<Option<String>, BlobError>,
) -> (Value, bool, Option<String>) {
    let id = shorten(&document.id, 48).0;
    let kind = shorten(&document.kind, 32).0;
    let path = shorten(&document.path, MAX_PATH_BYTES).0;
    let title = shorten(&document.title, MAX_TITLE_BYTES).0;
    if !selected {
        if let Err(error) = result {
            return (
                json!({
                    "id": id,
                    "kind": kind,
                    "path": path,
                    "title": title,
                    "required": document.required,
                    "selected": false,
                    "selection_reason": selection_reason,
                    "state": error.state(),
                    "reason": error.reason(),
                }),
                false,
                None,
            );
        }
        return (
            json!({
                "id": id,
                "kind": kind,
                "path": path,
                "title": title,
                "required": document.required,
                "selected": false,
                "selection_reason": selection_reason,
                "state": "not_selected",
            }),
            false,
            None,
        );
    }
    match result {
        Ok(Some(text)) => {
            let (excerpt, truncated) = shorten(&text, MAX_EXCERPT_BYTES);
            (
                json!({
                    "id": id,
                    "kind": kind,
                    "path": path,
                    "title": title,
                    "required": document.required,
                    "selected": true,
                    "selection_reason": selection_reason,
                    "state": "ready",
                    "bytes": text.len(),
                    "excerpt": excerpt,
                    "truncated": truncated,
                }),
                true,
                Some(document.path.clone()),
            )
        }
        Ok(None) => (
            json!({
                "id": id,
                "kind": kind,
                "path": path,
                "title": title,
                "required": document.required,
                "selected": true,
                "selection_reason": selection_reason,
                "state": "missing",
                "reason": "tracked blob is absent at HEAD",
            }),
            false,
            None,
        ),
        Err(error) => (
            json!({
                "id": id,
                "kind": kind,
                "path": path,
                "title": title,
                "required": document.required,
                "selected": true,
                "selection_reason": selection_reason,
                "state": error.state(),
                "reason": error.reason(),
            }),
            false,
            None,
        ),
    }
}

fn cap_response(mut payload: Value) -> Value {
    let mut reductions = 0usize;
    loop {
        let size = serde_json::to_vec_pretty(&payload)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        if size <= MAX_RESPONSE_BYTES {
            break;
        }
        let mut changed = false;
        if let Some(documents) = payload.get_mut("documents").and_then(Value::as_array_mut) {
            for document in documents.iter_mut().rev() {
                let Some(excerpt) = document
                    .get_mut("excerpt")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                if excerpt.is_empty() {
                    continue;
                }
                let (shortened, _) = shorten(&excerpt, excerpt.len() / 2);
                document["excerpt"] = Value::String(shortened);
                document["truncated"] = Value::Bool(true);
                changed = true;
                reductions += 1;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    if let Some(limits) = payload.get_mut("limits") {
        limits["response_truncated"] = Value::Bool(reductions > 0);
        limits["response_excerpt_reductions"] = Value::from(reductions);
    }
    payload
}

/// Build a context response. This never returns a filesystem error to callers:
/// project/source/document failures are stateful JSON so the UI can explain
/// missing or blocked context without guessing.
pub fn bundle(
    pm: &Pm,
    selected: &project::Project,
    role: Option<&str>,
    expected_revision: Option<&str>,
) -> Value {
    let source = match resolve_source(selected, expected_revision) {
        Ok(source) => source,
        Err(error) => {
            let reason = error.message;
            return json!({
                "project": selected.key,
                "state": error.state,
                "manifest": {"path": DEFAULT_MANIFEST_PATH, "state": error.state, "errors": [reason.clone()]},
                "snapshot": RepoSnapshot {
                    head_revision: None,
                    expected_revision: expected_revision.map(str::to_string),
                    revision_state: "unknown".to_string(),
                    dirty: None,
                    dirty_truncated: false,
                    repo_identity: None,
                    error: Some(reason),
                },
                "documents": [],
                "memories": empty_memories(),
                "limits": limits_json(false),
            });
        }
    };
    let manifest_result = read_blob(&source.root, &source.head, DEFAULT_MANIFEST_PATH);
    let manifest_text = match manifest_result {
        Ok(Some(text)) => text,
        Ok(None) => {
            return cap_response(json!({
                "project": selected.key,
                "state": "missing",
                "manifest": {"path": DEFAULT_MANIFEST_PATH, "state": "missing", "revision": source.head, "errors": ["tracked manifest is absent at HEAD"]},
                "snapshot": source.snapshot,
                "documents": [],
                "memories": empty_memories(),
                "limits": limits_json(false),
            }));
        }
        Err(error) => {
            let state = error.state();
            let reason = error.reason();
            return cap_response(json!({
                "project": selected.key,
                "state": state,
                "manifest": {"path": DEFAULT_MANIFEST_PATH, "state": state, "revision": source.head, "errors": [reason]},
                "snapshot": source.snapshot,
                "documents": [],
                "memories": empty_memories(),
                "limits": limits_json(false),
            }));
        }
    };
    if manifest_text.len() > MAX_MANIFEST_BYTES {
        return cap_response(json!({
            "project": selected.key,
            "state": "too_large",
            "manifest": {"path": DEFAULT_MANIFEST_PATH, "state": "too_large", "revision": source.head, "errors": [format!("manifest exceeds {MAX_MANIFEST_BYTES} bytes")]},
            "snapshot": source.snapshot,
            "documents": [],
            "memories": empty_memories(),
            "limits": limits_json(false),
        }));
    }
    let manifest: ContextManifest = match serde_yaml::from_str(&manifest_text) {
        Ok(manifest) => manifest,
        Err(error) => {
            return cap_response(json!({
                "project": selected.key,
                "state": "conflict",
                "manifest": {"path": DEFAULT_MANIFEST_PATH, "state": "conflict", "revision": source.head, "errors": [shorten(&format!("manifest YAML is invalid: {error}"), 512).0]},
                "snapshot": source.snapshot,
                "documents": [],
                "memories": empty_memories(),
                "limits": limits_json(false),
            }));
        }
    };
    let manifest_errors = validate_manifest(&manifest, &selected.key);
    let valid_manifest = manifest_errors.is_empty();
    let manifest_entry_count = manifest.documents.len();
    let manifest_entries_omitted = manifest_entry_count.saturating_sub(MAX_MANIFEST_ENTRIES);
    let manifest_errors = manifest_errors
        .into_iter()
        .map(|error| shorten(&error, 512).0)
        .collect::<Vec<_>>();
    let mut documents = Vec::new();
    let mut selected_paths = Vec::new();
    let mut required_failure = false;
    for document in manifest.documents.iter().take(MAX_MANIFEST_ENTRIES) {
        let role_selected = role
            .map(|role| {
                document.roles.is_empty()
                    || document.required
                    || document.roles.iter().any(|r| r == role)
            })
            .unwrap_or(true);
        let is_selected = valid_manifest && role_selected;
        let reason = if document.required {
            "required".to_string()
        } else if let Some(role) = role {
            if document.roles.is_empty() {
                "optional-default".to_string()
            } else if document.roles.iter().any(|candidate| candidate == role) {
                format!("role:{role}")
            } else {
                "excluded:role-mismatch".to_string()
            }
        } else {
            "optional-default".to_string()
        };
        let read = if let Some(reason) = path_error(&document.path) {
            Err(BlobError::State("invalid_path", reason))
        } else if is_selected {
            read_blob(&source.root, &source.head, &document.path)
        } else {
            Ok(None)
        };
        let (value, ready, path) = document_value(document, is_selected, &reason, read);
        if document.required && (!is_selected || !ready) {
            required_failure = true;
        }
        if let Some(path) = path {
            selected_paths.push(path);
        }
        if is_selected
            && path_error(&document.path).is_none()
            && !selected_paths.iter().any(|path| path == &document.path)
        {
            // A manifest path is context even when its tracked blob is
            // missing or unreadable; memory matching must not depend on a
            // document happening to be available at this HEAD.
            selected_paths.push(document.path.clone());
        }
        documents.push(value);
    }
    let state = if !valid_manifest {
        "conflict"
    } else if required_failure {
        let required_state = |wanted: &str| {
            documents.iter().any(|document| {
                document["required"].as_bool() == Some(true) && document["state"] == wanted
            })
        };
        if required_state("missing") {
            "missing"
        } else if required_state("too_large") {
            "too_large"
        } else if required_state("unreadable") {
            "unreadable"
        } else if required_state("invalid_path") {
            "invalid_path"
        } else {
            "blocked"
        }
    } else {
        "ready"
    };
    let memories = if valid_manifest {
        memory_context(pm, selected, &selected_paths)
    } else {
        empty_memories()
    };
    cap_response(json!({
        "project": selected.key,
        "state": state,
        "manifest": {
            "path": DEFAULT_MANIFEST_PATH,
            "state": if valid_manifest {"ready"} else {"conflict"},
            "revision": source.head,
            "entry_count": manifest_entry_count,
            "entries_omitted": manifest_entries_omitted,
            "errors": manifest_errors,
        },
        "snapshot": source.snapshot,
        "documents": documents,
        "memories": memories,
        "limits": limits_json(false),
    }))
}

fn limits_json(response_truncated: bool) -> Value {
    json!({
        "max_manifest_bytes": MAX_MANIFEST_BYTES,
        "max_manifest_entries": MAX_MANIFEST_ENTRIES,
        "max_path_bytes": MAX_PATH_BYTES,
        "max_title_bytes": MAX_TITLE_BYTES,
        "max_document_bytes": MAX_DOCUMENT_BYTES,
        "max_excerpt_bytes": MAX_EXCERPT_BYTES,
        "max_git_output_bytes": MAX_GIT_OUTPUT_BYTES,
        "max_memory_entries": memory::LESSON_MAX_ENTRIES,
        "max_memory_lesson_bytes": memory::LESSON_MAX_BYTES,
        "max_response_bytes": MAX_RESPONSE_BYTES,
        "response_truncated": response_truncated,
        "response_excerpt_reductions": 0,
    })
}

fn empty_memories() -> Value {
    json!({
        "included": [],
        "lessons": "",
        "withheld": [],
        "withheld_total": 0,
        "withheld_omitted": 0,
        "load_errors": [],
        "load_errors_total": 0,
        "load_errors_omitted": 0,
        "matched_total": 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn paths_are_document_only_and_traversal_free() {
        for bad in [
            "../secret.md",
            "/tmp/read.md",
            "docs\\secret.md",
            "docs/.env.md",
            "target/build.md",
            "docs/data.json",
            "docs/../CHARTER.md",
            "docs/guide.adoc",
            "docs/guide.MD",
            "docs/*.md",
        ] {
            assert!(path_error(bad).is_some(), "accepted {bad}");
        }
        assert!(path_error("docs/CHARTER.md").is_none());
    }

    #[test]
    fn ids_and_manifest_duplicates_are_bounded() {
        assert!(id_error("bad--id").is_some());
        assert!(id_error("scope").is_none());
        let manifest = ContextManifest {
            schema: 1,
            project: "cadence".to_string(),
            documents: vec![
                ManifestDocument {
                    id: "scope".to_string(),
                    kind: "scope".to_string(),
                    path: "docs/CHARTER.md".to_string(),
                    title: "Scope".to_string(),
                    required: true,
                    roles: vec![],
                },
                ManifestDocument {
                    id: "scope".to_string(),
                    kind: "scope".to_string(),
                    path: "docs/CHARTER.md".to_string(),
                    title: "Scope".to_string(),
                    required: true,
                    roles: vec![],
                },
            ],
        };
        let errors = validate_manifest(&manifest, "cadence");
        assert!(errors.iter().any(|error| error.contains("duplicate")));
    }

    #[test]
    fn response_bound_shortens_json_without_dropping_document_metadata() {
        let documents = (0..MAX_MANIFEST_ENTRIES)
            .map(|index| {
                json!({
                    "id": format!("doc-{index}"),
                    "kind": "spec",
                    "path": format!("docs/doc-{index}.md"),
                    "title": "bounded document",
                    "required": false,
                    "selected": true,
                    "selection_reason": "optional-default",
                    "state": "ready",
                    "bytes": MAX_DOCUMENT_BYTES,
                    "excerpt": "x".repeat(MAX_EXCERPT_BYTES),
                    "truncated": false,
                })
            })
            .collect::<Vec<_>>();
        let capped = cap_response(json!({
            "documents": documents,
            "limits": limits_json(false),
        }));
        let bytes = serde_json::to_vec_pretty(&capped).unwrap();
        assert!(bytes.len() <= MAX_RESPONSE_BYTES);
        assert_eq!(capped["limits"]["response_truncated"], true);
        assert_eq!(
            capped["documents"].as_array().unwrap().len(),
            MAX_MANIFEST_ENTRIES
        );
        assert!(capped["documents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|document| document.get("id").is_some()));
    }

    #[test]
    fn lesson_renderer_caps_entries_and_total_text() {
        let memories = (0..20)
            .map(|index| memory::Memory {
                project: "cadence".to_string(),
                front: memory::Front {
                    id: format!("lesson-{index}"),
                    kind: "rule".to_string(),
                    status: "accepted".to_string(),
                    scope: memory::Scope::default(),
                    source: None,
                    confidence: "high".to_string(),
                    created: "2026-01-01T00:00:00Z".to_string(),
                    verified_at: Some("2026-01-01T00:00:00Z".to_string()),
                    stale: None,
                    supersedes: None,
                    author: None,
                    author_proof: None,
                    contributors: Vec::new(),
                    review_cycle: 0,
                    active_operation: None,
                    reviews: Vec::new(),
                    finalizations: Vec::new(),
                },
                path: PathBuf::from(format!("lesson-{index}.md")),
                body: format!(
                    "{}\n\n**Why:** fixture\n\n**How to apply:** use it\n",
                    "x".repeat(600)
                ),
            })
            .collect::<Vec<_>>();
        let (lessons, slugs) = memory::render_lessons(&memory::Matched {
            lessons: memories,
            withheld: Vec::new(),
            fresh: memory::Freshness::for_project(None),
        });
        assert!(slugs.len() <= memory::LESSON_MAX_ENTRIES);
        assert!(lessons.len() <= memory::LESSON_MAX_BYTES);
    }

    #[test]
    fn blob_read_stays_on_pinned_head_after_repository_moves() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        assert!(git(&["config", "user.email", "context@test"])
            .status
            .success());
        assert!(git(&["config", "user.name", "context-test"])
            .status
            .success());
        std::fs::create_dir_all(repo.path().join("docs")).unwrap();
        std::fs::write(repo.path().join("docs/pinned.md"), "old head\n").unwrap();
        assert!(git(&["add", "-A"]).status.success());
        assert!(git(&["commit", "-qm", "old"]).status.success());
        let old_head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(repo.path().join("docs/pinned.md"), "new head\n").unwrap();
        assert!(git(&["add", "-A"]).status.success());
        assert!(git(&["commit", "-qm", "new"]).status.success());
        let pinned = read_blob(repo.path(), &old_head, "docs/pinned.md")
            .unwrap()
            .unwrap();
        assert_eq!(pinned, "old head\n");
    }
}
