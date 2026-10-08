//! Board endpoints for installed apps (CAD-557) — the Apps page's
//! reads and the operator's one write:
//!
//! - `GET /api/apps[?project=<key>]` — every installed app, one row
//!   per `<project>/<app>` ([`app::ls`], the same rows `cadence app
//!   ls` prints): title, version, the declared connection slots with
//!   their effective bindings (an explicit unbind reads `bound:
//!   null`), the bundle's workflow names, the recorded source (a git
//!   install pins its commit SHA), the digest and the three-state
//!   `approval` — `approved` / `changed` / `unapproved` / `unknown`.
//! - `GET /api/apps/<project>/<name>` — one app's detail
//!   ([`app::show`]): the agent guide, each workflow's checked
//!   summary, the rubrics' bodies, the install record, plus this
//!   app's row of [`app::doctor`]'s slot findings.
//! - `GET /api/apps/<project>/<name>/runs` — the plans/epics proposed
//!   from this app's workflows (CAD-563), by the recorded
//!   `plan.workflow` provenance: the epic, its derived status and the
//!   plan's state, tickets and size-weighted progress ([`plan::plan_json`]),
//!   so the app page lists its runs without leaving the app.
//! - `GET /api/apps/<project>/<name>/outputs` — the `local` outbox
//!   items this app's runs produced (CAD-563), operator-only like
//!   `/api/outbox` (the same proof, the same relay): an item is
//!   attributed by its effect's recorded `task` — a ticket of one of
//!   the app's runs — and a task-less send belongs to no run, so the
//!   page counts only what its runs did (CAD-571 N7).
//! - `POST /api/apps/<project>/<name>/approve` — relays the daemon's
//!   `app_approve` verbatim — the same call `cadence app approve`
//!   makes; the board keeps no second approval path.
//!
//! Approve is `OperatorOnly` in `operator::WRITE_ROUTES` (an unlisted
//! shape would fail the same way), because the board relays over its
//! own daemon connection: the daemon attributes the approval to
//! whoever that connection proves — the proven operator or nobody —
//! so an agent's or a member's HTTP request must never reach it.
//! `admit` runs the operator proof on the HTTP peer before the
//! handler runs; the daemon's own `operator_connection` gate still
//! refuses what the board must not do.

use std::collections::{HashMap, HashSet};

use serde::de::{MapAccess, Visitor};
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::Request;

use super::{err_response, home, json_response, read_body, HttpResp, ServeOpts};
use crate::client;
use crate::issue::{app, board, model, plan, Pm};

/// The approve body is `{}` — anything else is refused by shape.
const BODY_CAP: u64 = 4 * 1024;

/// CAD-996: the upload route's own wire cap — much larger than the
/// `{source}`-install `BODY_CAP` because the bundle arrives inline as a JSON
/// file-map. Scoped to `/api/app-installations/upload`; the install route's
/// 4 KiB cap is unchanged.
const UPLOAD_WIRE_CAP: u64 = 8 * 1024 * 1024;
/// Decoded ceiling for the whole bundle (matches `app::MAX_APP_BYTES`).
const UPLOAD_TOTAL_BYTES: u64 = crate::issue::app::MAX_APP_BYTES;
/// CAD-1194: the install-check body — exactly `{"source": …}`. The derive
/// refuses unknown keys and a repeated `source`, which a `Value` parse
/// would silently collapse to the last one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallCheckBody {
    source: String,
}

/// At most this many files — the same bound `snapshot` enforces on disk.
const UPLOAD_MAX_FILES: usize = 128;

/// The upload body: exactly `{"files": {path: utf8}}`. A custom
/// `Deserialize` is REQUIRED — `serde_json::Value` silently keeps the last
/// duplicate key, so `{"files":…,"files":…}` would collapse to one map and
/// defeat the strict-shape gate. This visitor rejects any repeated or unknown
/// top-level key at parse time.
#[derive(Debug)]
struct UploadBody {
    files: Vec<(String, String)>,
    /// CAD-1186: optional pin on the staged bundle's digest.
    expected_digest: Option<String>,
}

impl<'de> Deserialize<'de> for UploadBody {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct UploadVisitor;
        impl<'de> Visitor<'de> for UploadVisitor {
            type Value = UploadBody;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object with exactly one `files` map")
            }
            fn visit_map<A>(self, mut map: A) -> std::result::Result<UploadBody, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut files: Option<Vec<(String, String)>> = None;
                let mut expected_digest: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "expected_digest" {
                        if expected_digest.is_some() {
                            return Err(serde::de::Error::duplicate_field("expected_digest"));
                        }
                        let digest = map.next_value::<String>()?;
                        if digest.is_empty() {
                            return Err(serde::de::Error::custom("expected_digest is empty"));
                        }
                        expected_digest = Some(digest);
                        continue;
                    }
                    if key != "files" {
                        return Err(serde::de::Error::unknown_field(
                            &key,
                            &["files", "expected_digest"],
                        ));
                    }
                    if files.is_some() {
                        return Err(serde::de::Error::duplicate_field("files"));
                    }
                    // Deserialize the VALUE as the inner {path: text} map —
                    // `next_value` consumes only this field's value; a nested
                    // visitor reads its entries with duplicate-path rejection.
                    files = Some(map.next_value::<FilesMap>()?.0);
                }
                let files = files.ok_or_else(|| serde::de::Error::missing_field("files"))?;
                if files.is_empty() {
                    return Err(serde::de::Error::custom("files map is empty"));
                }
                Ok(UploadBody {
                    files,
                    expected_digest,
                })
            }
        }
        deserializer.deserialize_map(UploadVisitor)
    }
}

/// The inner `{path: text}` map as a value — its own `Deserialize` so the
/// outer visitor can `next_value::<FilesMap>()`. Duplicate inner path keys are
/// rejected here (a `Value`/derived map would silently keep the last one and
/// still stage a bundle), and per-file/count bounds apply during the read.
struct FilesMap(Vec<(String, String)>);

impl<'de> Deserialize<'de> for FilesMap {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct FilesVisitor;
        impl<'de> Visitor<'de> for FilesVisitor {
            type Value = FilesMap;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map of bundle path to UTF-8 text")
            }
            fn visit_map<A>(self, mut map: A) -> std::result::Result<FilesMap, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut seen: HashSet<String> = HashSet::new();
                let mut files: Vec<(String, String)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(serde::de::Error::custom(format!("duplicate path '{key}'")));
                    }
                    let cap = crate::issue::app::file_cap(&key);
                    if value.len() as u64 > cap {
                        return Err(serde::de::Error::custom(format!(
                            "file '{key}' is over the {cap}-byte per-file cap"
                        )));
                    }
                    files.push((key, value));
                    if files.len() > UPLOAD_MAX_FILES {
                        return Err(serde::de::Error::custom(format!(
                            "bundle exceeds the {UPLOAD_MAX_FILES}-file cap"
                        )));
                    }
                }
                Ok(FilesMap(files))
            }
        }
        deserializer.deserialize_map(FilesVisitor)
    }
}

/// The upgrade request body decoded STRICTLY — a `Value` parse would
/// collapse a duplicate `files`/`source`/`expected_*` key to the last
/// write before any gate ran. This visitor rejects a repeated or unknown
/// top-level key outright, decodes `files` via the duplicate-rejecting
/// [`FilesMap`], and keeps `source`/the `expected_*`/`request_id` pins as
/// raw `Value`s for the field-count/schema gate in
/// [`workspace_upgrade_transport`]. The wire shape is closed: only
/// `files`, `source`, `expected_digest`, `expected_generation`,
/// `expected_new_digest`, `request_id`.
struct UpgradeWire {
    files: Option<Vec<(String, String)>>,
    source: Option<Value>,
    fields: serde_json::Map<String, Value>,
}

impl<'de> Deserialize<'de> for UpgradeWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const ALLOWED: &[&str] = &[
            "files",
            "source",
            "expected_digest",
            "expected_generation",
            "expected_new_digest",
            "request_id",
        ];
        struct WireVisitor;
        impl<'de> Visitor<'de> for WireVisitor {
            type Value = UpgradeWire;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a closed upgrade object")
            }
            fn visit_map<A>(self, mut map: A) -> std::result::Result<UpgradeWire, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut files: Option<Vec<(String, String)>> = None;
                let mut source: Option<Value> = None;
                let mut fields = serde_json::Map::new();
                let mut seen: HashSet<String> = HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !ALLOWED.contains(&key.as_str()) {
                        return Err(serde::de::Error::unknown_field(&key, ALLOWED));
                    }
                    if !seen.insert(key.clone()) {
                        return Err(serde::de::Error::custom(format!("duplicate field '{key}'")));
                    }
                    match key.as_str() {
                        "files" => {
                            files = Some(map.next_value::<FilesMap>()?.0);
                        }
                        "source" => {
                            source = Some(map.next_value::<Value>()?);
                        }
                        _ => {
                            fields.insert(key, map.next_value::<Value>()?);
                        }
                    }
                }
                Ok(UpgradeWire {
                    files,
                    source,
                    fields,
                })
            }
        }
        deserializer.deserialize_map(WireVisitor)
    }
}

/// A leaf file name is a visible flat name — non-empty, ASCII, no `.`-lead
/// (dotfile), no `..`, no embedded `\`/`\0`. `seg` is already a single raw
/// segment (the caller split on literal `/`). `workflows/` leaves additionally
/// need a `valid_tag` `.md` stem — the same rule the installer's `snapshot`
/// enforces, so an upload cannot stage a workflow the validator would reject.
fn leaf_ok(seg: &str, need_md_tag: bool) -> bool {
    if seg.is_empty()
        || !seg.is_ascii()
        || seg.starts_with('.')
        || seg == ".."
        || seg.contains('\\')
        || seg.contains('\0')
    {
        return false;
    }
    if need_md_tag {
        seg.ends_with(".md") && crate::issue::model::valid_tag(seg.trim_end_matches(".md"))
    } else {
        true
    }
}

/// One uploaded path is admitted only by an EXACT lexical grammar — we split
/// on literal `/` and never normalize. `Path::components` folds `a//b`,
/// `a/./b` and `a/../b` together, which would let two distinct raw JSON keys
/// alias the same staged file and defeat duplicate detection; this grammar
/// refuses them instead. Admitted shapes (the only flat entries the bundle
/// allows):
///   `app.md`, `app-chat.json`         (exactly; CAD-1110)
///   `workflows/<tag>.md`              (valid_tag stem — the installer's rule)
///   `rubrics/<leaf>` `templates/<leaf>` (any visible flat leaf)
///   `views/app-views-v1.json`         (exact descriptor filename)
/// Empty segments (`//`, leading `/`, trailing `/`), `.`/`..`, `\`, `\0` and
/// any other top-level name are refused. `snapshot`/`validate_texts` re-check
/// daemon-side; this schema refuses bad keys before a single byte is staged.
fn upload_path_ok(path: &str) -> bool {
    if path.is_empty() || !path.is_ascii() || path.contains('\\') || path.contains('\0') {
        return false;
    }
    let segs: Vec<&str> = path.split('/').collect();
    // Any empty segment means a doubled, leading or trailing slash.
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return false;
    }
    match segs.as_slice() {
        ["app.md"] | ["app-chat.json"] => true,
        ["workflows", leaf] => leaf_ok(leaf, true),
        ["views", leaf] => *leaf == crate::issue::app_view::FILE,
        ["rubrics", leaf] | ["templates", leaf] => leaf_ok(leaf, false),
        // CAD-1006: screens/<tag>/<leaf> — the one depth-2 entry: a
        // tag-named package dir holding `screens.json` or a
        // `<stem>.<js|css|svg|json>` leaf per the validator grammar.
        ["screens", tag, leaf] => {
            crate::issue::model::valid_tag(tag) && crate::issue::app_screen_pkg::leaf_ok(leaf)
        }
        _ => false,
    }
}

/// Stage a validated `{files}` map to a server-derived temp dir OUTSIDE
/// the PM tracker and return its absolute path as the `source` string.
/// The `TempDir` is removed on every drop path; it is provenance
/// (`Source::Path`) only — a one-shot, not a reusable update URL. Shared
/// by `workspace_upload` (install) and the CAD-1006 upgrade transport.
fn stage_files(files: &[(String, String)]) -> Result<(tempfile::TempDir, String), HttpResp> {
    let staging = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return Err(err_response(500, &format!("staging dir failed: {e}"))),
    };
    for (path, text) in files {
        let dest = staging.path().join(path);
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(err_response(500, &format!("staging mkdir failed: {e}")));
            }
        }
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.custom_flags(libc::O_NOFOLLOW);
        }
        let wrote = open
            .open(&dest)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
        if let Err(e) = wrote {
            return Err(err_response(500, &format!("staging write failed: {e}")));
        }
    }
    let source = staging.path().to_string_lossy().to_string();
    Ok((staging, source))
}

/// Stage the validated file-map to a server-derived temp dir OUTSIDE the PM
/// tracker (`workspace.rs` refuses a source inside `pm.dir` — the tracker is
/// never its own source), then hand its absolute path to the unchanged
/// `app_workspace_install` path-source installer. The temp dir is removed on
/// every return path; it is provenance (`Source::Path`) only — a one-shot,
/// not a reusable update URL.
pub(super) fn workspace_upload(request: &mut Request, state: &std::path::Path) -> HttpResp {
    let bytes = match read_body(request, UPLOAD_WIRE_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: UploadBody = match serde_json::from_slice(&bytes) {
        Ok(b) => b,
        Err(e) => {
            return err_response(
                400,
                &format!("upload body must be {{\"files\":{{path:text}}}} — {e}"),
            )
        }
    };
    // Validate every path key against the flat allowlist + check the decoded
    // aggregate before touching the filesystem.
    let mut total: u64 = 0;
    for (path, text) in &body.files {
        if !upload_path_ok(path) {
            return err_response(
                400,
                &format!("bundle path '{path}' is not an allowed flat entry"),
            );
        }
        total += text.len() as u64;
        if total > UPLOAD_TOTAL_BYTES {
            return err_response(400, "bundle exceeds its aggregate byte cap");
        }
    }
    // Stage into a server-derived external temp dir (mode-0700, under the
    // process temp root — never under `pm.dir`), then hand its absolute path
    // to the unchanged path-source installer. `staging` is removed on drop.
    let (_staging, source) = match stage_files(&body.files) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    // The daemon re-validates and journals the staged dir as a Source::Path;
    // `installed_by`/`approved` come from the proven operator connection, never
    // the body. `staging` (a TempDir) is removed on drop, success or failure.
    let mut params = json!({"source": source});
    if let Some(digest) = &body.expected_digest {
        params["expected_digest"] = json!(digest);
    }
    match client::rpc(state, "app_workspace_install", params) {
        Ok(value) => json_response(value),
        Err(error) => home::rpc_err(&error, "app_workspace_install"),
    }
}

/// CAD-1006 upgrade transport. The new bundle's content arrives EITHER
/// as a `{files}` map (server-staged to a tempdir whose path becomes the
/// derived `source`) OR as the legacy `source` path/URL the caller names
/// — never both, and never neither. `files` mode keeps the board from
/// ever trusting a caller-controlled path; `source` mode preserves the
/// existing CLI/git transport unchanged. Returns `(params, staging)` —
/// `staging` is `Some` only for `files` mode and must outlive the RPC.
fn workspace_upgrade_transport(
    method: &str,
    id: &str,
    wire: UpgradeWire,
) -> std::result::Result<(Value, Option<tempfile::TempDir>), HttpResp> {
    let UpgradeWire {
        files,
        source,
        fields,
    } = wire;
    if files.is_some() && source.is_some() {
        return Err(err_response(
            400,
            "upgrade body must carry `files` or `source`, never both",
        ));
    }
    if files.is_none() && source.is_none() {
        return Err(err_response(
            400,
            "upgrade body requires a `files` map or a `source`",
        ));
    }
    let expected: &[&str] = if method == "app_workspace_upgrade" {
        &[
            "expected_digest",
            "expected_generation",
            "expected_new_digest",
            "request_id",
        ]
    } else {
        &["expected_digest", "expected_generation"]
    };
    if fields.len() != expected.len()
        || fields.keys().any(|key| !expected.contains(&key.as_str()))
        || expected.iter().any(|key| {
            fields
                .get(*key)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        })
    {
        return Err(err_response(
            400,
            "upgrade body has missing or unsupported fields",
        ));
    }
    if let Some(files) = files {
        // `{files}` mode — server-derived `source` via secure staging.
        // `files` is already the strict duplicate-free Vec<(path,text)>.
        if files.is_empty() {
            return Err(err_response(400, "upgrade files map is empty"));
        }
        let mut total: u64 = 0;
        for (path, text) in &files {
            if !upload_path_ok(path) {
                return Err(err_response(
                    400,
                    &format!("bundle path '{path}' is not an allowed flat entry"),
                ));
            }
            if text.len() as u64 > crate::issue::app::file_cap(path) {
                return Err(err_response(400, "a bundle file exceeds its byte cap"));
            }
            total += text.len() as u64;
        }
        if total > UPLOAD_TOTAL_BYTES {
            return Err(err_response(400, "bundle exceeds its aggregate byte cap"));
        }
        let (staging, source) = stage_files(&files)?;
        let mut params = Value::Object(fields);
        params["source"] = json!(source);
        params["install_id"] = json!(id);
        Ok((params, Some(staging)))
    } else {
        // Legacy `source` mode — the validated path/URL passes through
        // unchanged; no staging, the daemon re-checks it natively.
        let source = source.unwrap();
        if source.as_str().map(str::is_empty).unwrap_or(true) {
            return Err(err_response(
                400,
                "upgrade source must be a non-empty string",
            ));
        }
        let mut params = Value::Object(fields);
        params["source"] = source;
        params["install_id"] = json!(id);
        Ok((params, None))
    }
}

/// `true` when the approve body is exactly an empty JSON object. An
/// approve request carries nothing — like the daemon, attribution is
/// the board's proven connection, so a body field is a forgery attempt.
/// A non-object body (`[]`, `null`, `1`) fails too: serde would read a
/// fieldless struct from `[]`, which is not the shape the board admits.
fn approve_body_ok(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .is_some_and(|v| v.as_object().is_some_and(|m| m.is_empty()))
}

/// What `/api/apps…` names — the list, one app's detail, its runs or
/// its outputs.
#[derive(Debug)]
pub(super) enum ReadRoute<'a> {
    List,
    Detail(&'a str, &'a str),
    Runs(&'a str, &'a str),
    Outputs(&'a str, &'a str),
}

/// The read route for a path under `/api/apps` — exactly `/api/apps`
/// (or `/api/apps/`), `/api/apps/<project>/<name>` (app names are
/// tag-shaped — a single segment), or one app's `/runs` or `/outputs`.
pub(super) fn read_route(path: &str) -> Option<ReadRoute<'_>> {
    let rest = path.strip_prefix("/api/apps")?;
    match rest {
        "" | "/" => Some(ReadRoute::List),
        tail => {
            let tail = tail.strip_prefix('/')?;
            let (project, rest) = tail.split_once('/')?;
            if project.is_empty() {
                return None;
            }
            let Some((name, sub)) = rest.split_once('/') else {
                return (!rest.is_empty() && !rest.contains('/'))
                    .then_some(ReadRoute::Detail(project, rest));
            };
            if name.is_empty() {
                return None;
            }
            match sub {
                "runs" => Some(ReadRoute::Runs(project, name)),
                "outputs" => Some(ReadRoute::Outputs(project, name)),
                _ => None,
            }
        }
    }
}

/// `(project, name)` for `POST /api/apps/<project>/<name>/approve` —
/// `None` when the path is not that route.
pub(super) fn approve_route(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/apps/")?;
    let (project, rest) = tail.split_once('/')?;
    let (name, verb) = rest.split_once('/')?;
    (verb == "approve" && !project.is_empty() && !name.is_empty()).then_some((project, name))
}

/// `(project, name)` for `POST /api/apps/<project>/<name>/revoke` —
/// `None` when the path is not that route (CAD-577). The operator's
/// counterpart to `approve`: it withdraws the approval and revokes
/// every grant the approval derived.
pub(super) fn revoke_route(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/apps/")?;
    let (project, rest) = tail.split_once('/')?;
    let (name, verb) = rest.split_once('/')?;
    (verb == "revoke" && !project.is_empty() && !name.is_empty()).then_some((project, name))
}

/// `(project, name)` for `POST /api/apps/<project>/<name>/team` —
/// `None` when the path is not that route (CAD-577).
pub(super) fn team_route(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/apps/")?;
    let (project, rest) = tail.split_once('/')?;
    let (name, verb) = rest.split_once('/')?;
    (verb == "team" && !project.is_empty() && !name.is_empty()).then_some((project, name))
}

/// `(project, name)` for `POST /api/apps/<project>/<name>/worker` —
/// `None` when the path is not that route (CAD-577).
pub(super) fn worker_route(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/apps/")?;
    let (project, rest) = tail.split_once('/')?;
    let (name, verb) = rest.split_once('/')?;
    (verb == "worker" && !project.is_empty() && !name.is_empty()).then_some((project, name))
}

/// The worker body: `{"role": "<input>"}` — the role the operator
/// picked. Anything else is refused by shape.
fn worker_body_ok(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|v| v.get("role").and_then(Value::as_str).map(str::to_string))
        .is_some_and(|role| !role.is_empty())
}

/// The team body: `{"team": ["<input>=<agent>", …]}` — the roles the
/// operator picked. Anything else is refused by shape.
fn team_body_ok(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|v| v.get("team").and_then(Value::as_array).cloned())
        .is_some_and(|list| list.iter().all(Value::is_string))
}

/// `GET` dispatch for the app reads.
pub(super) fn read(
    request: &Request,
    pm: &Pm,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    query: &dyn Fn(&str) -> Option<String>,
    route: ReadRoute<'_>,
) -> HttpResp {
    match route {
        ReadRoute::List => list(pm, state_dir, query),
        ReadRoute::Detail(project, name) => detail(pm, state_dir, project, name),
        ReadRoute::Runs(project, name) => runs(pm, state_dir, project, name),
        ReadRoute::Outputs(project, name) => outputs(request, pm, state_dir, opts, project, name),
    }
}

/// `GET /api/apps[?project=<key>]` — the `app ls` payload.
fn list(pm: &Pm, state_dir: &std::path::Path, query: &dyn Fn(&str) -> Option<String>) -> HttpResp {
    let project = query("project");
    if let Some(p) = &project {
        if !model::valid_key(p) {
            return err_response(400, "bad project key");
        }
    }
    match app::ls(pm, project.as_deref(), state_dir) {
        Ok(listed) => json_response(listed),
        Err(e) if e.to_string().starts_with("No project") => err_response(404, &e.to_string()),
        Err(e) => err_response(503, &e.to_string()),
    }
}

/// `GET /api/apps/<project>/<name>` — `app show` plus the doctor row.
/// An app that cannot be described (not installed, a broken record or
/// manifest, a refused walk) is a 404 — there is no detail to show.
fn detail(pm: &Pm, state_dir: &std::path::Path, key: &str, name: &str) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    match app::show(pm, key, name, state_dir) {
        Ok(mut out) => {
            if let Some(why) = out["error"].as_str() {
                let why = why.to_string();
                return err_response(404, &why);
            }
            out["doctor"] = doctor_row(&pm.dir, state_dir, key, name);
            json_response(out)
        }
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.starts_with("No project") {
                404
            } else {
                503
            };
            err_response(code, &msg)
        }
    }
}

/// `GET /api/apps/<project>/<name>/runs` — every plan/epic proposed
/// from this app's workflows, by the recorded `plan.workflow`
/// provenance (`<app>/<wf>`, CAD-547). Each row carries the epic's
/// derived status and the plan block `plan show` renders: state,
/// tickets and size-weighted progress. An app that is not installed
/// (or cannot be described) is a 404, like its detail.
fn runs(pm: &Pm, state_dir: &std::path::Path, key: &str, name: &str) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    if let Err(e) = app::digest(&pm.dir, key, name) {
        return err_response(404, &e.to_string());
    }
    json_response(json!({
        "project": key,
        "name": name,
        "runs": app_runs(pm, state_dir, key, name),
    }))
}

/// The app's runs from the board's read model — the same indexed
/// views `/api/issues` derives from, so a tracker write shows on the
/// next read.
fn app_runs(pm: &Pm, state_dir: &std::path::Path, key: &str, name: &str) -> Vec<Value> {
    let read = super::read_model::get(state_dir, &pm.dir).board(pm, Some(key));
    let by_id = read.by_id();
    read.views
        .iter()
        .filter(|v| run_app(v).is_some_and(|(a, _)| a == name))
        .map(|v| {
            json!({
                "epic": v.issue.front.id,
                "title": v.issue.front.title,
                "status": v.status,
                "workflow": v.issue.front.plan.as_ref().and_then(|p| p.workflow.clone()),
                "plan": plan::plan_json(v, &by_id),
            })
        })
        .collect()
}

/// The app a run records in `plan.workflow` — `<app>/<wf>` only; a
/// stored workflow's bare name is not an app's (the same split
/// `app remove` refuses on).
fn run_app(v: &board::View) -> Option<(&str, &str)> {
    app::split_ref(v.issue.front.plan.as_ref()?.workflow.as_deref()?)
}

/// `GET /api/apps/<project>/<name>/outputs` — the `local` outbox items
/// this app's runs produced, plus the sends those runs staged and the
/// operator has not released yet (CAD-563): the ledger relayed exactly
/// as `/api/outbox` relays it, narrowed to what the app's runs account
/// for — the run whose plan lists the effect's `task`; a task-less send
/// is no run's (CAD-571 N7). Operator-only — the same proof
/// `/api/outbox` runs, because the ledger's previews and paths are the
/// operator's — so the gate comes before any tracker or ledger read.
fn outputs(
    request: &Request,
    pm: &Pm,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    key: &str,
    name: &str,
) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    if let Err(resp) = home::outbox_gate(
        request,
        state_dir,
        opts,
        &format!("GET /api/apps/{key}/{name}/outputs"),
    ) {
        return resp;
    }
    if let Err(e) = app::digest(&pm.dir, key, name) {
        return err_response(404, &e.to_string());
    }
    // What an effect is attributed by: the run whose plan lists the
    // effect's `task` — a ticket of one of the app's runs. A send
    // staged without a task is attributed to no run: the page counts
    // only what its runs did (CAD-571 N7), never a task-less send an
    // agent owning one of the tickets happened to stage.
    let read = super::read_model::get(state_dir, &pm.dir).board(pm, Some(key));
    let runs: Vec<(String, HashSet<String>)> = read
        .views
        .iter()
        .filter(|v| run_app(v).is_some_and(|(a, _)| a == name))
        .map(|v| {
            let tickets: HashSet<String> = v
                .issue
                .front
                .plan
                .iter()
                .flat_map(|p| p.tickets.iter().cloned())
                .collect();
            (v.issue.front.id.clone(), tickets)
        })
        .collect();
    let runs_for = |task: Option<&str>| -> Vec<String> {
        let Some(task) = task else {
            return Vec::new();
        };
        runs.iter()
            .filter(|(_, tickets)| tickets.contains(task))
            .map(|(epic, _)| epic.clone())
            .collect()
    };
    let facts = effect_facts(state_dir);
    let out = match client::rpc(state_dir, "platform_outbox", json!({})) {
        Ok(out) => out,
        Err(e) => return home::rpc_err(&e, "platform_outbox"),
    };
    let items: Vec<Value> = out["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let fact = item["effect_id"].as_str().and_then(|id| facts.get(id))?;
            let runs = runs_for(fact.task.as_deref());
            if runs.is_empty() {
                return None;
            }
            let mut item = item.clone();
            item["runs"] = json!(runs);
            Some(item)
        })
        .collect();
    // A staged send the operator has not released (or that is in
    // flight) is the app's next output — the release row the board
    // links to Needs you.
    let mut pending: Vec<Value> = Vec::new();
    for (effect_id, fact) in &facts {
        if !matches!(fact.state.as_str(), "waiting" | "decided") {
            continue;
        }
        let runs = runs_for(fact.task.as_deref());
        if runs.is_empty() {
            continue;
        }
        pending.push(json!({
            "effect_id": effect_id,
            "state": fact.state,
            "title": fact.title,
            "runs": runs,
        }));
    }
    pending.sort_by(|a, b| a["effect_id"].as_str().cmp(&b["effect_id"].as_str()));
    json_response(json!({"project": key, "name": name, "items": items, "pending": pending}))
}

/// What the durable effect ledger says about one effect: the task it
/// named (if any), its state and the human title of its input. A
/// read-only open, like the `plan_proposed` fallback
/// `open_plan_epics` reads; the caller has already proven the operator,
/// so nothing here widens a gate. Unreadable ledger: no effect is
/// attributed, never an error.
struct EffectFacts {
    task: Option<String>,
    state: String,
    title: Option<String>,
}

fn effect_facts(state_dir: &std::path::Path) -> HashMap<String, EffectFacts> {
    let Ok(conn) = crate::store::open_read_only(&state_dir.join("cadence.sqlite3")) else {
        return HashMap::new();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT effect_id, task, state, input, input_summary FROM platform_effects WHERE authorization_kind='agent_grant'")
    else {
        return HashMap::new();
    };
    let rows = stmt.query_map([], |r| {
        let input: String = r.get(3)?;
        let title = serde_json::from_str::<Value>(&input)
            .ok()
            .and_then(|v| v["title"].as_str().map(str::to_string));
        Ok((
            r.get::<_, String>(0)?,
            EffectFacts {
                task: r.get(1)?,
                state: r.get(2)?,
                title: title.or_else(|| r.get::<_, Option<String>>(4).ok().flatten()),
            },
        ))
    });
    match rows {
        Ok(rows) => rows.flatten().collect(),
        Err(_) => HashMap::new(),
    }
}

/// This app's row of `cadence app doctor` — its slot findings —
/// `null` when the app is absent from the scan. The connections the
/// daemon registers (plus the built-in `local`) decide what a binding
/// resolves to; unreachable, the check reports itself unavailable,
/// never "unknown connection" — the same rule `doctor` applies.
fn doctor_row(
    pm_dir: &std::path::Path,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
) -> Value {
    let known = client::rpc(state_dir, "daemon_info", json!({}))
        .ok()
        .and_then(|v| v["connections"].as_array().cloned())
        .map(|a| {
            let mut set: std::collections::HashSet<String> = a
                .iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect();
            set.insert(app::LOCAL_CONNECTION.to_string());
            set
        });
    app::doctor(pm_dir, known.as_ref())["apps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["project"].as_str() == Some(key) && r["app"].as_str() == Some(name))
        .cloned()
        .unwrap_or(Value::Null)
}

/// `POST /api/apps/<project>/<name>/team` — relays the daemon's
/// `app_set_team` (CAD-577). Operator-only on the board (the route is
/// listed in `operator::WRITE_ROUTES`), so the daemon attributes the
/// write to the board's own proven connection. The body carries the
/// `team` list; every other field is refused by shape.
pub(super) fn set_team(
    request: &mut Request,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    if !team_body_ok(&bytes) {
        return err_response(400, "app team takes {\"team\": [\"<input>=<agent>\"]}");
    }
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    match client::rpc(
        state_dir,
        "app_set_team",
        json!({"project": key, "name": name, "team": body["team"]}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => home::rpc_err(&e, "app_set_team"),
    }
}

/// `POST /api/apps/<project>/<name>/worker` — relays the daemon's
/// `app_add_worker` (CAD-577): join a new Devin worker for one team
/// role. Operator-only on the board (the route is listed in
/// `operator::WRITE_ROUTES`). The body carries `{"role": "<input>"}`;
/// every other field is refused by shape.
pub(super) fn add_worker(
    request: &mut Request,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    if !worker_body_ok(&bytes) {
        return err_response(400, "app add worker takes {\"role\": \"<input>\"}");
    }
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    match client::rpc(
        state_dir,
        "app_add_worker",
        json!({"project": key, "name": name, "role": body["role"]}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => home::rpc_err(&e, "app_add_worker"),
    }
}

/// `POST /api/apps/<project>/<name>/approve` — the same `app_approve`
/// call `cadence app approve` makes. The board was already admitted
/// as the operator (`operator::admit`); the daemon's
/// `operator_connection` attributes the approval to the board's own
/// connection and re-checks the installed bundle before recording.
pub(super) fn approve(
    request: &mut Request,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    if !approve_body_ok(&bytes) {
        return err_response(400, "app approve takes no fields — send {}");
    }
    match client::rpc(
        state_dir,
        "app_approve",
        json!({"project": key, "name": name}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => home::rpc_err(&e, "app_approve"),
    }
}

/// `POST /api/apps/<project>/<name>/revoke` — the same `app_revoke`
/// call `cadence app revoke` makes (CAD-577). Operator-only on the
/// board (the route is listed in `operator::WRITE_ROUTES`), so the
/// daemon attributes the revocation to the board's own proven
/// connection. The body is `{}`; attribution is never a field.
pub(super) fn revoke(
    request: &mut Request,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
) -> HttpResp {
    if !model::valid_key(key) || !model::valid_tag(name) {
        return err_response(400, "bad project or app name");
    }
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    if !approve_body_ok(&bytes) {
        return err_response(400, "app revoke takes no fields — send {}");
    }
    match client::rpc(
        state_dir,
        "app_revoke",
        json!({"project": key, "name": name}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => home::rpc_err(&e, "app_revoke"),
    }
}

/// CAD-667 separate stable-ID transport, leaving legacy Apps UI untouched.
pub(super) fn workspace(
    request: &mut Request,
    state: &std::path::Path,
    method: &str,
    id: Option<&str>,
) -> HttpResp {
    // CAD-1006: when the upgrade body carries a `{files}` map, the
    // staging TempDir must outlive the `client::rpc` below — bind it
    // here so it drops only AFTER the complete response. `None` when the
    // caller used the legacy `source` transport (no staging) or another
    // method entirely.
    let mut _upgrade_staging: Option<tempfile::TempDir> = None;
    let params = if matches!(
        method,
        "app_workspace_upgrade" | "app_workspace_upgrade_check"
    ) {
        // The new bundle arrives EITHER as a server-staged `{files}` map
        // (CAD-1006) OR as the legacy `source` path/URL — never both.
        // `files` carries the whole bundle inline, so this branch reads
        // the bounded 8 MiB upload wire cap — the 4 KiB `BODY_CAP` would
        // refuse a real files map before the RPC ever ran.
        let bytes = match read_body(request, UPLOAD_WIRE_CAP) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        // Decode STRICTLY — a `Value` parse would silently collapse a
        // duplicate `files`/`source`/expected key before the gate ran;
        // `UpgradeWire` rejects repeated or unknown top-level keys and
        // duplicate inner path keys at parse time.
        let wire: UpgradeWire = match serde_json::from_slice(&bytes) {
            Ok(w) => w,
            Err(_) => return err_response(400, "upgrade body must be a JSON object"),
        };
        let id = match id {
            Some(id) => id,
            None => return err_response(400, "upgrade needs an installation id"),
        };
        let (params, staging) = match workspace_upgrade_transport(method, id, wire) {
            Ok(pair) => pair,
            Err(resp) => return resp,
        };
        _upgrade_staging = staging;
        params
    } else if method == "app_workspace_upgrade_recover" {
        let bytes = match read_body(request, BODY_CAP) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return err_response(400, "upgrade body must be a JSON object"),
        };
        let Some(fields) = value.as_object() else {
            return err_response(400, "upgrade body must be a JSON object");
        };
        if fields.len() != 1
            || fields
                .get("request_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return err_response(400, "upgrade body has missing or unsupported fields");
        }
        let mut params = value;
        params["install_id"] = json!(id);
        params
    } else if method == "app_workspace_migration_recover" {
        let bytes = match read_body(request, BODY_CAP) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return err_response(400, "recovery body must be an object"),
        };
        let Some(fields) = value.as_object() else {
            return err_response(400, "recovery body must be an object");
        };
        if fields.keys().any(|key| key != "rollback")
            || fields
                .get("rollback")
                .is_some_and(|value| !value.is_boolean())
        {
            return err_response(400, "recovery body admits only boolean rollback");
        };
        json!({"journal_id":id,"rollback":value.get("rollback").and_then(Value::as_bool).unwrap_or(false)})
    } else if method == "app_workspace_migrate" || method == "app_workspace_recover" {
        let bytes = match read_body(request, BODY_CAP) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        if !approve_body_ok(&bytes) {
            return err_response(400, "catalog operation requires an empty object");
        };
        if let Some(id) = id {
            json!({"install_id":id})
        } else {
            json!({})
        }
    } else if method == "app_workspace_install_check" {
        // CAD-1194: the read-only digest proposal an install pins. The
        // body is exactly `{source}` — decoded STRICTLY, so a repeated or
        // unknown key is refused instead of collapsing to the last one;
        // identity, approval and digest are never caller fields here.
        let bytes = match read_body(request, BODY_CAP) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        match serde_json::from_slice::<InstallCheckBody>(&bytes) {
            Ok(body) if !body.source.is_empty() => json!({"source":body.source}),
            _ => {
                return err_response(
                    400,
                    "install check body admits only a non-empty source; identity and approval are never caller fields",
                )
            }
        }
    } else if method == "app_workspace_install" {
        let bytes = match read_body(request, BODY_CAP) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return err_response(400, "install body must be an object containing source"),
        };
        let valid = value.as_object().is_some_and(|fields| {
            fields
                .get("source")
                .and_then(Value::as_str)
                .is_some_and(|source| !source.is_empty())
                && fields
                    .keys()
                    .all(|k| k == "source" || k == "expected_digest")
                && fields
                    .get("expected_digest")
                    .is_none_or(|d| d.as_str().is_some_and(|d| !d.is_empty()))
        });
        if !valid {
            return err_response(
                400,
                "install body admits only source and an optional expected_digest; identity and approval are never caller fields",
            );
        }
        value
    } else if let Some(id) = id {
        json!({"install_id":id})
    } else {
        json!({})
    };
    match client::rpc(state, method, params) {
        Ok(value) => json_response(value),
        Err(error) => home::rpc_err(&error, method),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The read route's shape: exactly the list, `<project>/<name>`, or
    /// one app's `runs` / `outputs` — no partial segment, no deeper
    /// path, no sibling prefix, and no other verb.
    #[test]
    fn read_route_is_exact() {
        assert!(matches!(read_route("/api/apps"), Some(ReadRoute::List)));
        assert!(matches!(read_route("/api/apps/"), Some(ReadRoute::List)));
        match read_route("/api/apps/demo/studio") {
            Some(ReadRoute::Detail(p, n)) => assert_eq!((p, n), ("demo", "studio")),
            other => panic!("detail: {other:?}"),
        }
        match read_route("/api/apps/demo/studio/runs") {
            Some(ReadRoute::Runs(p, n)) => assert_eq!((p, n), ("demo", "studio")),
            other => panic!("runs: {other:?}"),
        }
        match read_route("/api/apps/demo/studio/outputs") {
            Some(ReadRoute::Outputs(p, n)) => assert_eq!((p, n), ("demo", "studio")),
            other => panic!("outputs: {other:?}"),
        }
        for dead in [
            "/api/apps/demo",
            "/api/apps/demo/",
            "/api/apps/demo/studio/x",
            "/api/apps/demo/studio/runs/x",
            "/api/apps/demo/studio/approve",
            "/api/apps/demo//runs",
            "/api/apps//studio/runs",
            "/api/apps//studio",
            "/api/apps/demo//",
            "/api/appsx",
            "/api/app",
        ] {
            assert!(read_route(dead).is_none(), "{dead}");
        }
    }

    /// The approve route's shape: `POST /api/apps/<project>/<name>/approve`
    /// — the verb is the last segment, never a wildcard position.
    #[test]
    fn approve_route_is_exact() {
        assert_eq!(
            approve_route("/api/apps/demo/studio/approve"),
            Some(("demo", "studio"))
        );
        for dead in [
            "/api/apps",
            "/api/apps/demo/studio",
            "/api/apps/demo/studio/reject",
            "/api/apps/demo/approve",
            "/api/apps//studio/approve",
            "/api/apps/demo//approve",
            "/api/apps/demo/studio/approve/extra",
        ] {
            assert_eq!(approve_route(dead), None, "{dead}");
        }
    }

    /// The revoke route's shape: `POST /api/apps/<project>/<name>/revoke`.
    #[test]
    fn revoke_route_is_exact() {
        assert_eq!(
            revoke_route("/api/apps/demo/studio/revoke"),
            Some(("demo", "studio"))
        );
        for dead in [
            "/api/apps",
            "/api/apps/demo/studio",
            "/api/apps/demo/studio/approve",
            "/api/apps/demo/revoke",
            "/api/apps//studio/revoke",
            "/api/apps/demo/studio/revoke/extra",
        ] {
            assert_eq!(revoke_route(dead), None, "{dead}");
        }
    }

    /// The team route and body shape (CAD-577).
    #[test]
    fn team_route_and_body_are_exact() {
        assert_eq!(
            team_route("/api/apps/demo/studio/team"),
            Some(("demo", "studio"))
        );
        for dead in [
            "/api/apps",
            "/api/apps/demo/studio",
            "/api/apps/demo/studio/approve",
            "/api/apps/demo/team",
            "/api/apps//studio/team",
            "/api/apps/demo/studio/team/extra",
        ] {
            assert_eq!(team_route(dead), None, "{dead}");
        }
        for ok in [r#"{"team":[]}"#, r#"{"team":["writer=a"]}"#] {
            assert!(team_body_ok(ok.as_bytes()), "{ok}");
        }
        for bad in [
            r#"{"team":"a"}"#,
            r#"{"team":[1]}"#,
            r#"{"writer":"a"}"#,
            "[]",
            "null",
        ] {
            assert!(!team_body_ok(bad.as_bytes()), "{bad}");
        }
    }

    /// The "Add worker" route and body shape (CAD-577).
    #[test]
    fn worker_route_and_body_are_exact() {
        assert_eq!(
            worker_route("/api/apps/demo/studio/worker"),
            Some(("demo", "studio"))
        );
        for dead in [
            "/api/apps",
            "/api/apps/demo/studio",
            "/api/apps/demo/studio/team",
            "/api/apps/demo/worker",
            "/api/apps//studio/worker",
            "/api/apps/demo/studio/worker/extra",
        ] {
            assert_eq!(worker_route(dead), None, "{dead}");
        }
        for ok in [r#"{"role":"publisher"}"#, r#"{"role":"writer"}"#] {
            assert!(worker_body_ok(ok.as_bytes()), "{ok}");
        }
        for bad in [
            r#"{"role":""}"#,
            r#"{"role":1}"#,
            r#"{"writer":"a"}"#,
            "[]",
            "null",
        ] {
            assert!(!worker_body_ok(bad.as_bytes()), "{bad}");
        }
    }

    /// The body admits exactly `{}` — a field, an empty array or a
    /// scalar fails the shape before the daemon is asked.
    #[test]
    fn approve_body_is_empty_object_only() {
        for ok in ["{}", " { } ", "{\n}"] {
            assert!(approve_body_ok(ok.as_bytes()), "{ok:?}");
        }
        for forged in [
            r#"{"actor":"operator"}"#,
            r#"{"operator":true}"#,
            r#"{"project":"x"}"#,
            r#"{"name":"y"}"#,
            "[]",
            "null",
            "1",
            "",
            "{",
        ] {
            assert!(!approve_body_ok(forged.as_bytes()), "{forged}");
        }
    }
}
