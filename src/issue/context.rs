//! Bounded, read-only project documentation context.
//!
//! The context reader is deliberately narrower than a file server: it reads
//! one fixed, tracked manifest and the manifest's tracked text blobs from the
//! selected project's declared repository at one immutable HEAD revision. The
//! manifest is the project's own `docs/cadence/projects/<key>/project-context.yaml`
//! when that is tracked, else the shared `docs/cadence/project-context.yaml`
//! (which must name the same project) — one checkout can serve several
//! projects (CAD-2001).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;

use crate::issue::{areas, project};
#[cfg(test)]
use crate::memory;
use crate::proc::{self, BoundedError};

mod bundle;

pub use bundle::bundle;
use bundle::shorten;
#[cfg(test)]
use bundle::{bounded_len, cap_response, empty_memories, limits_json};

pub const DEFAULT_MANIFEST_PATH: &str = "docs/cadence/project-context.yaml";
/// The per-project manifest, preferred when tracked: one checkout can
/// serve several projects (CAD-2001), and each keeps its own opt-in.
const PROJECT_MANIFEST_DIR: &str = "docs/cadence/projects";
pub const MAX_MANIFEST_BYTES: usize = 32 * 1024;
pub const MAX_MANIFEST_ENTRIES: usize = 32;
pub const MAX_PATH_BYTES: usize = 256;
pub const MAX_TITLE_BYTES: usize = 160;
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
pub const MAX_EXCERPT_BYTES: usize = 16 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// Untrusted memory frontmatter copied into the bundle as display metadata:
/// the on-disk loader parses it without a slug/length check, so every copied
/// field is bounded here, at the delivery boundary.
pub(super) const MAX_MEMORY_ID_BYTES: usize = 128;
pub(super) const MAX_MEMORY_FIELD_BYTES: usize = 64;
/// Git diagnostics and memory reasons are untrusted text kept for diagnosis.
pub(super) const MAX_REASON_BYTES: usize = 512;
pub(super) const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(3);
/// Canonical context roles. `ops` is an input alias of `devops` (manifests
/// and callers written before the rename) and is never itself stored.
const ROLES: &[&str] = &["pm", "dev", "qa", "devops"];
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
    /// CAD-1822: explicit opt-in for task-context delivery to issue-bound
    /// workers. Absent or false keeps every existing dispatch path unchanged;
    /// the board's Project Context reader ignores it.
    #[serde(default)]
    pub agent_delivery: bool,
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
    #[serde(default, deserialize_with = "deserialize_roles")]
    pub roles: Vec<String>,
}

/// Map a context role input to its stored key: `ops` becomes `devops`.
/// Anything else passes through unchanged, so validation still names an
/// unknown role instead of a silently rewritten one.
pub fn canonical_role(role: &str) -> &str {
    if role == "ops" {
        "devops"
    } else {
        role
    }
}

pub fn valid_role(role: &str) -> bool {
    ROLES.contains(&role)
}

fn deserialize_roles<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let roles = Vec::<String>::deserialize(deserializer)?;
    Ok(roles
        .iter()
        .map(|role| canonical_role(role).to_string())
        .collect())
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
pub(super) struct RepoSource {
    pub(super) root: PathBuf,
    pub(super) head: String,
    pub(super) snapshot: RepoSnapshot,
}

#[derive(Clone, Debug)]
pub(super) enum BlobError {
    State(&'static str, String),
}

#[derive(Clone, Debug)]
pub(super) struct SourceError {
    pub(super) state: &'static str,
    pub(super) message: String,
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
    pub(super) fn state(&self) -> &'static str {
        match self {
            Self::State(state, _) => state,
        }
    }

    pub(super) fn reason(&self) -> String {
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
            Some(
                shorten(
                    &format!("working tree state unavailable: {error}"),
                    MAX_REASON_BYTES,
                )
                .0,
            ),
        ),
    }
}

fn resolve_root(selected: &project::Project) -> Result<(PathBuf, &project::Repo), SourceError> {
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
    // Two rows naming one checkout are one repository (CAD-2001); only
    // genuinely different checkouts make the source ambiguous, and that
    // is named rather than resolved to the first row.
    let mut distinct: Vec<&project::Repo> = Vec::new();
    for repo in &local {
        let declared = project::expand_home(repo.path.as_deref().unwrap_or_default());
        let identity = declared.canonicalize().unwrap_or(declared);
        let seen = distinct.iter().any(|row| {
            let path = project::expand_home(row.path.as_deref().unwrap_or_default());
            path.canonicalize().unwrap_or(path) == identity
        });
        if !seen {
            distinct.push(repo);
        }
    }
    if distinct.len() > 1 {
        return Err(SourceError::new(
            "ambiguous_repository",
            format!(
                "project declares {} local repositories ({}); context source is ambiguous",
                distinct.len(),
                distinct
                    .iter()
                    .filter_map(|row| row.path.as_deref())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    let repo = distinct[0];
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
    Ok((root, repo))
}

fn head_of(root: &Path) -> Result<String, SourceError> {
    let head = git_text(
        root,
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
    Ok(head)
}

pub(super) fn resolve_source(
    selected: &project::Project,
    expected: Option<&str>,
) -> Result<RepoSource, SourceError> {
    let (root, repo) = resolve_root(selected)?;
    let head = head_of(&root)?;
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

/// A project document read at the repository's pinned HEAD for task delivery.
/// The body is not stored here: [`read_pinned_blob`] re-reads the exact blob
/// and checks its digest, so a pinned reference is not a source cache that can
/// outlive the repository or pass for authority.
#[derive(Clone, Debug)]
pub struct PinnedDocument {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub title: String,
    pub required: bool,
    pub roles: Vec<String>,
    pub selected: bool,
    pub selection_reason: String,
    pub state: &'static str,
    pub reason: Option<String>,
    pub digest: Option<String>,
    pub bytes: Option<usize>,
}

/// One project's manifest and documents at HEAD, or the reason it is not
/// usable. `unconfigured` means delivery is not enabled: no declared local
/// repository, no readable and parseable tracked manifest, or
/// `agent_delivery` absent/false. `conflict` is an opted-in manifest that
/// fails validation.
#[derive(Clone, Debug)]
pub struct PinnedContext {
    pub state: &'static str,
    pub project: String,
    pub revision: Option<String>,
    pub documents: Vec<PinnedDocument>,
    pub errors: Vec<String>,
}

fn unconfigured(project: &str, revision: Option<String>) -> PinnedContext {
    PinnedContext {
        state: "unconfigured",
        project: project.to_string(),
        revision,
        documents: Vec::new(),
        errors: Vec::new(),
    }
}

/// Pin the selected project's tracked context manifest and documents at its
/// current HEAD, but only when that committed manifest sets
/// `agent_delivery: true`. The manifest is the project's own
/// `docs/cadence/projects/<key>/project-context.yaml` when that is tracked,
/// else the shared `docs/cadence/project-context.yaml` — whose `project:`
/// must still name this project (`validate_manifest`). An unprovable opt-in
/// (no repo, no HEAD, no manifest, unreadable or unparseable YAML) returns
/// `unconfigured`, so existing dispatch behavior is untouched. Reuses the
/// bounded readers and allowlist the board's context query uses; never
/// accepts a caller-supplied path or revision.
pub fn pin(selected: &project::Project, role: Option<&str>) -> PinnedContext {
    pin_context(selected, role, None)
}

/// Pin required and cited sources for planning, independently of delivery opt-in.
pub(crate) fn pin_for_plan(
    selected: &project::Project,
    cited: impl Fn(&str) -> bool,
) -> PinnedContext {
    pin_context(selected, None, Some(&cited))
}

fn pin_context(
    selected: &project::Project,
    role: Option<&str>,
    cited: Option<&dyn Fn(&str) -> bool>,
) -> PinnedContext {
    let unavailable = |revision: Option<String>, message: String| {
        let mut context = unconfigured(&selected.key, revision);
        if cited.is_some() {
            context.errors.push(shorten(&message, MAX_REASON_BYTES).0);
        }
        context
    };
    // No declared local repository, several distinct ones, or one that is
    // not a readable absolute checkout: no committed opt-in can be
    // proven, so every existing dispatch path stays unchanged. (Two rows
    // naming one checkout are one repository — CAD-2001.)
    let (root, _) = match resolve_root(selected) {
        Ok(source) => source,
        Err(error) => {
            return unavailable(
                None,
                format!("register an accessible repository root: {}", error.message),
            )
        }
    };
    let head = match head_of(&root) {
        Ok(head) => head,
        Err(error) => {
            return unavailable(
                None,
                format!("resolve a committed repository HEAD: {}", error.message),
            )
        }
    };
    let (manifest_path, result) = select_manifest(&root, &head, &selected.key);
    let text = match result {
        Ok(Some(text)) => text,
        Ok(None) => {
            return unavailable(
                Some(head),
                format!("track a project context manifest at {manifest_path}"),
            )
        }
        Err(error) => {
            return unavailable(
                Some(head),
                format!("read tracked manifest {manifest_path}: {}", error.reason()),
            )
        }
    };
    if text.len() > MAX_MANIFEST_BYTES {
        return unavailable(
            Some(head),
            format!("manifest {manifest_path} exceeds {MAX_MANIFEST_BYTES} bytes"),
        );
    }
    let manifest = match serde_yaml::from_str::<ContextManifest>(&text) {
        Ok(manifest) => manifest,
        Err(error) => {
            return unavailable(
                Some(head),
                format!("fix malformed manifest {manifest_path}: {error}"),
            )
        }
    };
    if cited.is_none() && !manifest.agent_delivery {
        // Explicit opt-in is required. An existing Overview/Project Context
        // manifest without it must not change dispatch behavior, and the
        // board reader keeps using the manifest as before.
        return unconfigured(&selected.key, Some(head));
    }
    let errors = validate_manifest(&manifest, &selected.key);
    if !errors.is_empty() {
        return PinnedContext {
            state: "conflict",
            project: selected.key.clone(),
            revision: Some(head),
            documents: Vec::new(),
            errors: errors
                .into_iter()
                .map(|error| shorten(&error, 512).0)
                .collect(),
        };
    }
    let canonical = role.map(canonical_role);
    let mut documents = Vec::new();
    for document in manifest.documents.iter().take(MAX_MANIFEST_ENTRIES) {
        let selected_here = if let Some(cited) = cited {
            document.required || cited(&document.id)
        } else {
            canonical.is_none()
                || document.required
                || document.roles.is_empty()
                || canonical.is_some_and(|role| {
                    document
                        .roles
                        .iter()
                        .any(|candidate| candidate.as_str() == role)
                })
        };
        let selection_reason = if document.required {
            "required".to_string()
        } else if cited.is_some() {
            if selected_here {
                "cited".to_string()
            } else {
                "excluded:not-cited".to_string()
            }
        } else if let Some(role) = canonical {
            if document.roles.is_empty() {
                "optional-default".to_string()
            } else if document
                .roles
                .iter()
                .any(|candidate| candidate.as_str() == role)
            {
                format!("role:{role}")
            } else {
                "excluded:role-mismatch".to_string()
            }
        } else {
            "optional-default".to_string()
        };
        let (state, reason, digest, bytes) = if !selected_here {
            ("not_selected", None, None, None)
        } else if let Some(reason) = path_error(&document.path) {
            ("invalid_path", Some(reason), None, None)
        } else {
            match read_blob(&root, &head, &document.path) {
                Ok(Some(blob)) => (
                    "ready",
                    None,
                    Some(crate::delegation::sha256_hex(blob.as_bytes())),
                    Some(blob.len()),
                ),
                Ok(None) => (
                    "missing",
                    Some("tracked blob is absent at HEAD".to_string()),
                    None,
                    None,
                ),
                Err(error) => (error.state(), Some(error.reason()), None, None),
            }
        };
        documents.push(PinnedDocument {
            id: shorten(&document.id, 48).0,
            kind: shorten(&document.kind, 32).0,
            path: shorten(&document.path, MAX_PATH_BYTES).0,
            title: shorten(&document.title, MAX_TITLE_BYTES).0,
            required: document.required,
            roles: document.roles.clone(),
            selected: selected_here,
            selection_reason,
            state,
            reason,
            digest,
            bytes,
        });
    }
    PinnedContext {
        state: "ready",
        project: selected.key.clone(),
        revision: Some(head),
        documents,
        errors: Vec::new(),
    }
}

/// The declared repository root for a project, for re-reading pinned blobs
/// without re-resolving HEAD.
pub fn repository_root(selected: &project::Project) -> Result<PathBuf, (String, String)> {
    resolve_root(selected)
        .map(|(root, _)| root)
        .map_err(|error| (error.state.to_string(), error.message))
}

/// Check literal source paths against the registered repository at a pinned commit.
pub(crate) fn check_plan_paths<'a>(
    selected: &project::Project,
    revision: &str,
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<(), (String, String)> {
    if !valid_revision(revision) {
        return Err((
            "invalid_revision".to_string(),
            "pinned revision is not a full SHA-1".to_string(),
        ));
    }
    let (root, _) = resolve_root(selected).map_err(|error| {
        (
            error.state.to_string(),
            shorten(&error.message, MAX_REASON_BYTES).0,
        )
    })?;
    for path in paths {
        let display = shorten(&areas::scrub(path), MAX_PATH_BYTES).0;
        let refusal = |state: &str, reason: &str| {
            (
                state.to_string(),
                shorten(&format!("path '{display}': {reason}"), MAX_REASON_BYTES).0,
            )
        };
        if let Err(error) = areas::check_path(path) {
            return Err(refusal("invalid_path", &error.to_string()));
        }
        if path
            .chars()
            .any(|c| matches!(c, '*' | '?' | '[' | ']' | ':'))
        {
            return Err(refusal(
                "invalid_path",
                "literal paths only; globs are not allowed",
            ));
        }
        let literal = path.strip_suffix('/').unwrap_or(path);
        let entry = tree_entry(&root, revision, literal)
            .map_err(|error| refusal(error.state(), &error.reason()))?
            .ok_or_else(|| refusal("missing", "not tracked at the pinned revision"))?;
        let is_tree = entry.kind == "tree" && entry.mode == "040000";
        let is_file = entry.kind == "blob" && matches!(entry.mode.as_str(), "100644" | "100755");
        if !is_tree && (!is_file || path.ends_with('/')) {
            return Err(refusal(
                "invalid_path",
                "not a tracked regular file or directory",
            ));
        }
    }
    Ok(())
}

/// Re-read `path` at the exact pinned `revision` and verify the recorded
/// digest. Missing, unreadable, oversized, symlinked, non-UTF-8 and
/// digest-mismatched blobs all refuse — a pinned reference never falls back to
/// another revision or to the working tree.
pub fn read_pinned_blob(
    root: &Path,
    revision: &str,
    path: &str,
    digest: &str,
) -> Result<String, (String, String)> {
    if !valid_revision(revision) {
        return Err((
            "invalid_revision".to_string(),
            "pinned revision is not a full SHA-1".to_string(),
        ));
    }
    if let Some(reason) = path_error(path) {
        return Err(("invalid_path".to_string(), reason));
    }
    match read_blob(root, revision, path) {
        Ok(Some(text)) => {
            if crate::delegation::sha256_hex(text.as_bytes()) != digest {
                return Err((
                    "revision_changed".to_string(),
                    "the pinned blob's digest does not match its recorded digest".to_string(),
                ));
            }
            Ok(text)
        }
        Ok(None) => Err((
            "missing".to_string(),
            "the pinned blob is absent at its revision".to_string(),
        )),
        Err(error) => Err((error.state().to_string(), error.reason())),
    }
}

pub(super) fn path_error(path: &str) -> Option<String> {
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

pub(super) fn validate_manifest(manifest: &ContextManifest, selected: &str) -> Vec<String> {
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
        if document.roles.iter().any(|role| !valid_role(role)) {
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

pub(super) fn read_blob(root: &Path, head: &str, path: &str) -> Result<Option<String>, BlobError> {
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

/// The per-project manifest path for `key`, or `None` when the key is not
/// one safe path segment. A `project.yaml` is hand-editable, so its key is
/// never trusted as a path component.
fn project_manifest_path(key: &str) -> Option<String> {
    cadence_core::issue::valid_key(key)
        .then(|| format!("{PROJECT_MANIFEST_DIR}/{key}/project-context.yaml"))
}

/// The path this project prefers: its own per-project manifest, else the
/// shared default.
pub(super) fn preferred_manifest_path(key: &str) -> String {
    project_manifest_path(key).unwrap_or_else(|| DEFAULT_MANIFEST_PATH.to_string())
}

/// The manifest a project reads at `head` on a possibly shared checkout:
/// `docs/cadence/projects/<key>/project-context.yaml` when it is tracked
/// at HEAD, else the shared default. Returns the path to report and its
/// text (`None` when neither is tracked).
///
/// A per-project path that exists but cannot be read is an error, never a
/// silent fall back: the default may describe another project, and the
/// `manifest.project == selected.key` guard is what keeps that honest.
pub(super) fn select_manifest(
    root: &Path,
    head: &str,
    key: &str,
) -> (String, Result<Option<String>, BlobError>) {
    if let Some(path) = project_manifest_path(key) {
        match read_blob(root, head, &path) {
            Ok(Some(text)) => return (path, Ok(Some(text))),
            Ok(None) => {}
            Err(error) => return (path, Err(error)),
        }
    }
    (
        DEFAULT_MANIFEST_PATH.to_string(),
        read_blob(root, head, DEFAULT_MANIFEST_PATH),
    )
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
            agent_delivery: false,
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
    fn manifest_roles_store_devops_and_accept_legacy_ops() {
        let yaml = "schema: 1\nproject: cadence\ndocuments:\n  - id: release\n    kind: release\n    path: docs/release.md\n    title: Release\n    required: false\n    roles: [pm, ops, devops]\n";
        let manifest: ContextManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.documents[0].roles, ["pm", "devops", "devops"]);
        assert!(validate_manifest(&manifest, "cadence").is_empty());

        // The stored form round-trips as `devops`; `ops` never comes back.
        let written = serde_yaml::to_string(&manifest).unwrap();
        assert!(!written.contains("- ops"), "{written}");
        let reread: ContextManifest = serde_yaml::from_str(&written).unwrap();
        assert_eq!(reread.documents[0].roles, ["pm", "devops", "devops"]);
        assert!(validate_manifest(&reread, "cadence").is_empty());
    }

    #[test]
    fn manifest_rejects_unknown_roles_without_rewriting_them() {
        let yaml = "schema: 1\nproject: cadence\ndocuments:\n  - id: release\n    kind: release\n    path: docs/release.md\n    title: Release\n    required: false\n    roles: [operations]\n";
        let manifest: ContextManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.documents[0].roles, ["operations"]);
        let errors = validate_manifest(&manifest, "cadence");
        assert_eq!(errors, ["release: unsupported role"]);
    }

    #[test]
    fn tracked_manifest_uses_devops_and_validates() {
        let text = include_str!("../../docs/cadence/project-context.yaml");
        assert!(!text.contains(" ops"), "manifest still writes ops");
        let manifest: ContextManifest = serde_yaml::from_str(text).unwrap();
        assert_eq!(
            validate_manifest(&manifest, "cadence"),
            Vec::<String>::new()
        );
        assert!(manifest
            .documents
            .iter()
            .any(|document| document.roles.iter().any(|role| role == "devops")));
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
        // The same budget keeps a bounded excerpt of every document: no
        // document is drained while another keeps its full text.
        let excerpts: Vec<usize> = capped["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|document| document["excerpt"].as_str().unwrap().len())
            .collect();
        let widest = *excerpts.iter().max().unwrap();
        let narrowest = *excerpts.iter().min().unwrap();
        assert!(narrowest > 0, "{excerpts:?}");
        assert!(widest <= narrowest * 2, "{excerpts:?}");
    }

    /// A single untrusted memory id can exceed the whole response budget. It
    /// is dropped with its count kept, and the document excerpt survives.
    #[test]
    fn response_bound_drops_oversized_withheld_memory_before_excerpts() {
        let capped = cap_response(json!({
            "project": "bounded",
            "state": "ready",
            "manifest": {"path": DEFAULT_MANIFEST_PATH, "state": "ready", "revision": "a".repeat(40), "errors": []},
            "snapshot": {"head_revision": "a".repeat(40)},
            "documents": [{
                "id": "scope", "kind": "scope", "path": "docs/scope.md", "title": "scope",
                "required": true, "selected": true, "selection_reason": "required",
                "state": "ready", "bytes": 11, "excerpt": "SCOPE-BODY\n", "truncated": false,
            }],
            "memories": {
                "included": [], "lessons": "",
                "withheld": [{"id": "z".repeat(MAX_RESPONSE_BYTES + 1), "status": "proposed", "reason": "review blocked"}],
                "withheld_total": 1, "withheld_omitted": 0,
                "load_errors": [], "load_errors_total": 0, "load_errors_omitted": 0,
                "matched_total": 0,
            },
            "limits": limits_json(false),
        }));
        assert!(bounded_len(&capped) <= MAX_RESPONSE_BYTES);
        assert_eq!(capped["state"], "ready");
        assert_eq!(capped["limits"]["response_truncated"], true);
        assert_eq!(capped["documents"][0]["excerpt"], "SCOPE-BODY\n");
        assert!(capped["memories"]["withheld"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(capped["memories"]["withheld_total"], 1);
        assert_eq!(capped["memories"]["withheld_omitted"], 1);
    }

    /// When the payload cannot fit even with every optional section dropped,
    /// the response fails closed: state and counts, never oversized bytes.
    #[test]
    fn response_bound_fails_closed_when_metadata_cannot_fit() {
        let capped = cap_response(json!({
            "project": "bounded",
            "state": "ready",
            "manifest": {
                "path": DEFAULT_MANIFEST_PATH, "state": "ready", "revision": "a".repeat(40),
                "entry_count": 1, "errors": [],
            },
            "snapshot": {"head_revision": "a".repeat(40)},
            "documents": [{
                "id": "scope", "kind": "scope", "path": "docs/scope.md",
                "title": "t".repeat(MAX_RESPONSE_BYTES), "required": true, "selected": true,
                "selection_reason": "required", "state": "ready", "bytes": 11,
                "excerpt": "", "truncated": true,
            }],
            "memories": empty_memories(),
            "limits": limits_json(false),
        }));
        assert!(bounded_len(&capped) <= MAX_RESPONSE_BYTES);
        assert_eq!(capped["state"], "too_large");
        assert_eq!(capped["limits"]["response_truncated"], true);
        assert!(capped["documents"].as_array().unwrap().is_empty());
        assert_eq!(capped["manifest"]["documents_omitted"], 1);
        assert_eq!(capped["manifest"]["entry_count"], 1);
        assert_eq!(capped["manifest"]["revision"], "a".repeat(40));
        assert_eq!(capped["snapshot"]["head_revision"], "a".repeat(40));
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
                    policy: None,
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

    /// CAD-2001: one checkout, two projects. Each reads its own tracked
    /// manifest; the shared default serves only the project it names, and
    /// a project the default does not name is refused, never given
    /// another project's documents.
    #[test]
    fn a_shared_checkout_selects_each_projects_own_manifest() {
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
        let manifest = |project: &str, document: &str| {
            format!(
                "schema: 1\nproject: {project}\nagent_delivery: true\ndocuments:\n  \
                 - id: scope\n    kind: scope\n    path: docs/{document}.md\n    \
                 title: Scope\n    required: true\n"
            )
        };
        for (path, body) in [
            (
                DEFAULT_MANIFEST_PATH.to_string(),
                manifest("alpha", "alpha"),
            ),
            (
                "docs/cadence/projects/beta/project-context.yaml".to_string(),
                manifest("beta", "beta"),
            ),
            ("docs/alpha.md".to_string(), "ALPHA\n".to_string()),
            ("docs/beta.md".to_string(), "BETA\n".to_string()),
        ] {
            let full = repo.path().join(&path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        assert!(git(&["add", "-A"]).status.success());
        assert!(git(&["commit", "-qm", "contexts"]).status.success());
        let head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        let fixture = |key: &str| project::Project {
            key: key.to_string(),
            prefix: key[..3].to_uppercase(),
            repos: vec![project::Repo {
                path: Some(repo.path().to_string_lossy().to_string()),
                remote: None,
                base: None,
            }],
            components: Vec::new(),
            tags: Vec::new(),
            default_owner: None,
            build: None,
            memory: None,
            intake: None,
            id: None,
            policy: None,
        };
        let read = |key: &str| {
            let (path, text) = select_manifest(repo.path(), &head, key);
            let text = text.unwrap().expect("a manifest is tracked");
            let manifest: ContextManifest = serde_yaml::from_str(&text).unwrap();
            (path, manifest)
        };
        // beta's own manifest wins over the shared default.
        let (path, manifest) = read("beta");
        assert_eq!(path, "docs/cadence/projects/beta/project-context.yaml");
        assert_eq!(manifest.project, "beta");
        assert!(validate_manifest(&manifest, "beta").is_empty());
        // alpha has no per-project manifest: the default, which names it.
        let (path, manifest) = read("alpha");
        assert_eq!(path, DEFAULT_MANIFEST_PATH);
        assert!(validate_manifest(&manifest, "alpha").is_empty());
        // gamma is named by neither: the default is read but refused.
        let (path, manifest) = read("gamma");
        assert_eq!(path, DEFAULT_MANIFEST_PATH);
        assert!(!validate_manifest(&manifest, "gamma").is_empty());
        // pin follows the same selection, and refuses the mismatch.
        let pinned = pin(&fixture("beta"), None);
        assert_eq!(pinned.state, "ready");
        assert_eq!(pinned.documents[0].path, "docs/beta.md");
        let pinned = pin(&fixture("alpha"), None);
        assert_eq!(pinned.state, "ready");
        assert_eq!(pinned.documents[0].path, "docs/alpha.md");
        let pinned = pin(&fixture("gamma"), None);
        assert_eq!(pinned.state, "conflict");
        assert!(pinned.documents.is_empty());
        // A key is never trusted as a path component.
        assert!(project_manifest_path("../../etc").is_none());
        assert!(project_manifest_path("agents").is_some());
        assert_eq!(preferred_manifest_path("../x"), DEFAULT_MANIFEST_PATH);
    }
}
