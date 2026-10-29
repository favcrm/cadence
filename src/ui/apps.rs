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

use std::collections::{BTreeMap, HashMap, HashSet};

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
/// Optional catalogue locator. Every field is required when the object is
/// present; unknown keys are refused. It names a release, it does not grant
/// permission.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueWire {
    id: String,
    version: String,
    digest: String,
}

fn catalogue_wire_ok(wire: &CatalogueWire) -> bool {
    !wire.id.is_empty() && !wire.version.is_empty() && !wire.digest.is_empty()
}

/// CAD-1194: the install-check body. `source` is required. `catalogue` and
/// `catalogue_resolution` are optional and mutually exclusive.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallCheckBody {
    source: String,
    #[serde(default)]
    catalogue: Option<CatalogueWire>,
    #[serde(default)]
    catalogue_resolution: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallBody {
    source: String,
    #[serde(default)]
    expected_digest: Option<String>,
    #[serde(default)]
    catalogue: Option<CatalogueWire>,
    #[serde(default)]
    catalogue_resolution: Option<String>,
}

/// At most this many files — the same bound `snapshot` enforces on disk.
const UPLOAD_MAX_FILES: usize = 128;

/// The upload body: `{"files": {path: utf8}}`, or — CAD-1942 — a catalogue
/// locator with no `files`, whose release the board fetches itself. A custom
/// `Deserialize` is REQUIRED — `serde_json::Value` silently keeps the last
/// duplicate key, so `{"files":…,"files":…}` would collapse to one map and
/// defeat the strict-shape gate. This visitor rejects any repeated or unknown
/// top-level key at parse time.
#[derive(Debug)]
struct UploadBody {
    /// Absent only beside a catalogue locator (CAD-1942).
    files: Option<Vec<(String, String)>>,
    /// CAD-1186: optional pin on the staged bundle's digest.
    expected_digest: Option<String>,
    catalogue: Option<CatalogueWire>,
    catalogue_resolution: Option<String>,
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
                f.write_str("an object with one `files` map or a catalogue locator")
            }
            fn visit_map<A>(self, mut map: A) -> std::result::Result<UploadBody, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut files: Option<Vec<(String, String)>> = None;
                let mut expected_digest: Option<String> = None;
                let mut catalogue: Option<CatalogueWire> = None;
                let mut catalogue_resolution: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "catalogue" {
                        if catalogue.is_some() {
                            return Err(serde::de::Error::duplicate_field("catalogue"));
                        }
                        catalogue = Some(map.next_value::<CatalogueWire>()?);
                        continue;
                    }
                    if key == "catalogue_resolution" {
                        if catalogue_resolution.is_some() {
                            return Err(serde::de::Error::duplicate_field("catalogue_resolution"));
                        }
                        catalogue_resolution = Some(map.next_value::<String>()?);
                        continue;
                    }
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
                            &[
                                "files",
                                "expected_digest",
                                "catalogue",
                                "catalogue_resolution",
                            ],
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
                if files.is_none() && catalogue.is_none() {
                    return Err(serde::de::Error::missing_field("files"));
                }
                if files.as_ref().is_some_and(Vec::is_empty) {
                    return Err(serde::de::Error::custom("files map is empty"));
                }
                if catalogue.is_some() && catalogue_resolution.is_some() {
                    return Err(serde::de::Error::custom(
                        "catalogue locator and manual resolution are mutually exclusive",
                    ));
                }
                Ok(UploadBody {
                    files,
                    expected_digest,
                    catalogue,
                    catalogue_resolution,
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
/// `expected_new_digest`, `request_id`, and — on an apply, not a check —
/// the optional automatic-consent pair `automatic` + `observed_approval`.
struct UpgradeWire {
    files: Option<Vec<(String, String)>>,
    source: Option<Value>,
    catalogue: Option<CatalogueWire>,
    catalogue_resolution: Option<String>,
    automatic: Option<Value>,
    observed_approval: Option<Value>,
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
            "catalogue",
            "catalogue_resolution",
            "automatic",
            "observed_approval",
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
                let mut catalogue: Option<CatalogueWire> = None;
                let mut catalogue_resolution: Option<String> = None;
                let mut automatic: Option<Value> = None;
                let mut observed_approval: Option<Value> = None;
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
                        "catalogue" => {
                            catalogue = Some(map.next_value::<CatalogueWire>()?);
                        }
                        "catalogue_resolution" => {
                            catalogue_resolution = Some(map.next_value::<String>()?);
                        }
                        "automatic" => {
                            automatic = Some(map.next_value::<Value>()?);
                        }
                        "observed_approval" => {
                            observed_approval = Some(map.next_value::<Value>()?);
                        }
                        _ => {
                            fields.insert(key, map.next_value::<Value>()?);
                        }
                    }
                }
                if catalogue.is_some() && catalogue_resolution.is_some() {
                    return Err(serde::de::Error::custom(
                        "catalogue locator and manual resolution are mutually exclusive",
                    ));
                }
                Ok(UpgradeWire {
                    files,
                    source,
                    catalogue,
                    catalogue_resolution,
                    automatic,
                    observed_approval,
                    fields,
                })
            }
        }
        deserializer.deserialize_map(WireVisitor)
    }
}

/// One uploaded path is admitted only by the host's bundle grammar. The root
/// set is the same one the source loader and catalogue importer use
/// (`app::root_member`); this transport applies the full predicate because it
/// receives a flat path map rather than a directory to walk.
/// Keeping a private copy here is exactly how CAD-1415's `app-actions.json`
/// reached the daemon's grammar but not this transport.
fn upload_path_ok(path: &str) -> bool {
    crate::issue::app::bundle_path_ok(path)
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

/// Attach a catalogue locator or an explicit manual resolution. Neither is
/// authority: the daemon re-checks the package and the closed directory.
fn catalogue_params(
    catalogue: Option<CatalogueWire>,
    resolution: Option<String>,
) -> std::result::Result<Value, &'static str> {
    match (catalogue, resolution.as_deref()) {
        (Some(wire), None) if catalogue_wire_ok(&wire) => Ok(json!({
            "catalogue": {"id": wire.id, "version": wire.version, "digest": wire.digest}
        })),
        (None, Some("manual")) => Ok(json!({"catalogue_resolution": "manual"})),
        (None, None) => Ok(json!({})),
        _ => Err(
            "catalogue requires id, version and digest together, or catalogue_resolution \"manual\", never both",
        ),
    }
}

fn merge_catalogue(params: &mut Value, extra: Value) {
    if let (Some(dest), Some(src)) = (params.as_object_mut(), extra.as_object()) {
        for (key, value) in src {
            dest.insert(key.clone(), value.clone());
        }
    }
}

/// CAD-1942: the release document the catalog serves for one exact release.
/// `files` goes through the same strict [`FilesMap`] a posted bundle does —
/// duplicate paths, per-file caps and the file-count cap apply alike.
#[derive(Deserialize)]
struct ReleaseDocument {
    files: FilesMap,
    expected_digest: String,
}

/// CAD-1942: a catalogue install names its release; the board fetches the
/// bytes from the configured catalog through the same closed release route
/// `/api/public-apps` relays, and admits them only when they hash to the
/// locator's digest under the bundle digest the daemon computes. A posted
/// `files` map (older boards, during rollout) must equal the fetched release.
/// A forged locator is refused before any network read.
fn catalogue_release_files(
    origin: &str,
    locator: &CatalogueWire,
    posted: Option<Vec<(String, String)>>,
) -> Result<Vec<(String, String)>, HttpResp> {
    let Some(url) = super::public_directory::release_url(
        origin,
        &locator.id,
        &locator.version,
        &locator.digest,
    ) else {
        return Err(err_response(
            400,
            "catalogue locator must name a directory id, an exact version and a sha256 digest",
        ));
    };
    let Some((status, bytes)) = super::public_directory::fetch_catalog(&url) else {
        return Err(err_response(
            502,
            "The catalogue release could not be read.",
        ));
    };
    if status != 200 {
        return Err(err_response(status, "catalogue release is not installable"));
    }
    let Ok(document) = serde_json::from_slice::<ReleaseDocument>(&bytes) else {
        return Err(err_response(
            502,
            "The catalogue release could not be read.",
        ));
    };
    let fetched: BTreeMap<String, String> = document.files.0.into_iter().collect();
    if fetched.is_empty()
        || document.expected_digest != locator.digest
        || crate::issue::app_catalog::workspace::bundle_digest(&fetched) != locator.digest
    {
        return Err(err_response(
            409,
            "catalogue release bytes do not match the locator digest",
        ));
    }
    if let Some(posted) = posted {
        if posted.into_iter().collect::<BTreeMap<_, _>>() != fetched {
            return Err(err_response(
                409,
                "posted files differ from the catalogue release",
            ));
        }
    }
    Ok(fetched.into_iter().collect())
}

/// How the upgrade transport obtains a catalogue release (CAD-1942): the
/// configured catalog in production, a loopback catalog in tests.
type ReleaseFetch<'a> = dyn Fn(&CatalogueWire, Option<Vec<(String, String)>>) -> Result<Vec<(String, String)>, HttpResp>
    + 'a;

/// The configured catalog's release for `locator` (see
/// [`catalogue_release_files`]).
fn configured_release_files(
    locator: &CatalogueWire,
    posted: Option<Vec<(String, String)>>,
) -> Result<Vec<(String, String)>, HttpResp> {
    let Some(origin) = crate::issue::app_catalog::client::catalog_origin() else {
        return Err(err_response(503, "public directory is not configured"));
    };
    catalogue_release_files(&origin, locator, posted)
}

pub(super) fn hosted_operation_id(request: &Request) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("X-Cadence-Hosted-Operation-Id"))
        .map(|header| header.value.as_str().to_owned())
}

/// Stage the validated file-map to a server-derived temp dir OUTSIDE the PM
/// tracker (`workspace.rs` refuses a source inside `pm.dir` — the tracker is
/// never its own source), then hand its absolute path to the unchanged
/// `app_workspace_install` path-source installer. The temp dir is removed on
/// every return path; it is provenance (`Source::Path`) only — a one-shot,
/// not a reusable update URL.
pub(super) fn workspace_upload(request: &mut Request, state: &std::path::Path) -> HttpResp {
    workspace_upload_with_mode(request, state, false)
}

pub(super) fn workspace_upload_check(request: &mut Request, state: &std::path::Path) -> HttpResp {
    workspace_upload_with_mode(request, state, true)
}

fn workspace_upload_with_mode(
    request: &mut Request,
    state: &std::path::Path,
    check_only: bool,
) -> HttpResp {
    let bytes = match read_body(request, UPLOAD_WIRE_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: UploadBody = match serde_json::from_slice(&bytes) {
        Ok(b) => b,
        Err(e) => {
            return err_response(
                400,
                &format!(
                    "upload body must be {{\"files\":{{path:text}}}} or a catalogue locator — {e}"
                ),
            )
        }
    };
    let locator = body.catalogue.clone();
    let catalogue = match catalogue_params(body.catalogue, body.catalogue_resolution) {
        Ok(extra) => extra,
        Err(message) => return err_response(400, message),
    };
    // CAD-1942: a catalogue install names its release; the board fetches it.
    let files = match (locator, body.files) {
        (Some(locator), posted) => {
            if body
                .expected_digest
                .as_deref()
                .is_some_and(|digest| digest != locator.digest)
            {
                return err_response(409, "expected_digest does not match the catalogue locator");
            }
            match configured_release_files(&locator, posted) {
                Ok(files) => files,
                Err(resp) => return resp,
            }
        }
        (None, Some(files)) => files,
        (None, None) => return err_response(400, "upload body requires a `files` map"),
    };
    // Validate every path key against the flat allowlist + check the decoded
    // aggregate before touching the filesystem.
    let mut total: u64 = 0;
    for (path, text) in &files {
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
    let (_staging, source) = match stage_files(&files) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    // The daemon re-validates and journals the staged dir as a Source::Path;
    // `installed_by`/`approved` come from the proven operator connection, never
    // the body. `staging` (a TempDir) is removed on drop, success or failure.
    let mut params = json!({"source": source});
    if let Some(digest) = body.expected_digest.as_ref().filter(|_| !check_only) {
        params["expected_digest"] = json!(digest);
    }
    merge_catalogue(&mut params, catalogue);
    let method = if check_only {
        "app_workspace_install_check"
    } else {
        "app_workspace_install"
    };
    if !check_only {
        if let Some(id) = hosted_operation_id(request) {
            params["hosted_operation_id"] = json!(id);
        }
    }
    match client::rpc(state, method, params) {
        Ok(value) => {
            if check_only
                && body.expected_digest.as_deref().is_some_and(|expected| {
                    value.get("digest").and_then(Value::as_str) != Some(expected)
                })
            {
                return err_response(
                    409,
                    "checked package digest does not match the selected release",
                );
            }
            json_response(value)
        }
        Err(error) => home::rpc_err(&error, method),
    }
}

/// CAD-1006 upgrade transport. The new bundle's content arrives EITHER
/// as a `{files}` map (server-staged to a tempdir whose path becomes the
/// derived `source`) OR as the legacy `source` path/URL the caller names
/// — never both, and never neither. `files` mode keeps the board from
/// ever trusting a caller-controlled path; `source` mode preserves the
/// existing CLI/git transport unchanged. CAD-1942: a catalogue locator with
/// no `source` needs no `files` — `release` fetches the named release (and
/// compares a posted `files` map with it) only after every shape and pin
/// check passed. Returns `(params, staging)` — `staging` is `Some` only for
/// `files` mode and must outlive the RPC.
fn workspace_upgrade_transport(
    method: &str,
    id: &str,
    wire: UpgradeWire,
    release: &ReleaseFetch<'_>,
) -> std::result::Result<(Value, Option<tempfile::TempDir>), HttpResp> {
    let UpgradeWire {
        files,
        source,
        catalogue,
        catalogue_resolution,
        automatic,
        observed_approval,
        fields,
    } = wire;
    let locator = catalogue.clone();
    let catalogue = match catalogue_params(catalogue, catalogue_resolution) {
        Ok(extra) => extra,
        Err(message) => return Err(err_response(400, message)),
    };
    if files.is_some() && source.is_some() {
        return Err(err_response(
            400,
            "upgrade body must carry `files` or `source`, never both",
        ));
    }
    if files.is_none() && source.is_none() && locator.is_none() {
        return Err(err_response(
            400,
            "upgrade body requires a `files` map, a `source` or a catalogue locator",
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
    let claim = match automatic_upgrade_claim(method, automatic, observed_approval) {
        Ok(claim) => claim,
        Err(message) => return Err(err_response(400, message)),
    };
    let files = match locator {
        Some(locator) if source.is_none() => {
            if fields
                .get("expected_new_digest")
                .is_some_and(|digest| digest.as_str() != Some(locator.digest.as_str()))
            {
                return Err(err_response(
                    409,
                    "expected_new_digest does not match the catalogue locator",
                ));
            }
            Some(release(&locator, files)?)
        }
        _ => files,
    };
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
        merge_catalogue(&mut params, catalogue);
        merge_automatic(&mut params, &claim);
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
        merge_catalogue(&mut params, catalogue);
        merge_automatic(&mut params, &claim);
        Ok((params, None))
    }
}

/// The automatic-consent pair is optional, and only on an apply. A check
/// never carries it. A partial or contradictory pair is refused rather
/// than treated as a manual upgrade, which would record fresh consent.
fn automatic_upgrade_claim(
    method: &str,
    automatic: Option<Value>,
    observed_approval: Option<Value>,
) -> std::result::Result<Option<Value>, &'static str> {
    match (method, automatic, observed_approval) {
        ("app_workspace_upgrade", None, None) => Ok(None),
        ("app_workspace_upgrade", Some(Value::Bool(true)), Some(Value::String(state)))
            if state == "approved" =>
        {
            Ok(Some(
                json!({"automatic": true, "observed_approval": "approved"}),
            ))
        }
        ("app_workspace_upgrade", _, _) => {
            Err("an automatic update must set automatic true and observed_approval \"approved\"")
        }
        (_, None, None) => Ok(None),
        _ => Err("upgrade check does not accept automatic consent fields"),
    }
}

fn merge_automatic(params: &mut Value, claim: &Option<Value>) {
    let Some(extra) = claim else {
        return;
    };
    let (Some(dest), Some(src)) = (params.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (key, value) in src {
        dest.insert(key.clone(), value.clone());
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
    let mut params = if matches!(
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
        let (params, staging) =
            match workspace_upgrade_transport(method, id, wire, &configured_release_files) {
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
        // CAD-1194: the read-only digest proposal an install pins.
        // `source` is required. An optional catalogue locator or
        // `catalogue_resolution` may name provenance; a repeated or
        // unknown key is refused. Identity and approval are never caller
        // fields here.
        let bytes = match read_body(request, BODY_CAP) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        match serde_json::from_slice::<InstallCheckBody>(&bytes) {
            Ok(body) if !body.source.is_empty() => {
                match catalogue_params(body.catalogue, body.catalogue_resolution) {
                    Ok(extra) => {
                        let mut params = json!({"source": body.source});
                        merge_catalogue(&mut params, extra);
                        params
                    }
                    Err(message) => return err_response(400, message),
                }
            }
            _ => {
                return err_response(
                    400,
                    "install check body admits a non-empty source and an optional catalogue locator; identity and approval are never caller fields",
                )
            }
        }
    } else if method == "app_workspace_install" {
        let bytes = match read_body(request, BODY_CAP) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        let body: InstallBody = match serde_json::from_slice::<InstallBody>(&bytes) {
            Ok(body) if !body.source.is_empty() => body,
            _ => {
                return err_response(
                    400,
                    "install body admits source, an optional expected_digest, and an optional catalogue locator; identity and approval are never caller fields",
                )
            }
        };
        if body
            .expected_digest
            .as_ref()
            .is_some_and(|digest| digest.is_empty())
        {
            return err_response(400, "expected_digest is empty");
        }
        let mut params = json!({"source": body.source});
        if let Some(digest) = body.expected_digest {
            params["expected_digest"] = json!(digest);
        }
        match catalogue_params(body.catalogue, body.catalogue_resolution) {
            Ok(extra) => merge_catalogue(&mut params, extra),
            Err(message) => return err_response(400, message),
        }
        params
    } else if let Some(id) = id {
        json!({"install_id":id})
    } else {
        json!({})
    };
    if matches!(method, "app_workspace_install" | "app_workspace_upgrade") {
        if let Some(id) = hosted_operation_id(request) {
            params["hosted_operation_id"] = json!(id);
        }
    }
    match client::rpc(state, method, params) {
        Ok(value) => json_response(value),
        Err(error) => home::rpc_err(&error, method),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const LOCATOR: &str = r#"{"id":"social-content","version":"1.2.3","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;

    /// CAD-1942: a catalogue install names the release; it does not carry it.
    /// `files` may be absent only beside a locator, and is never empty.
    #[test]
    fn cad1942_reference_only_upload_body_parses() {
        let reference = format!(r#"{{"catalogue":{LOCATOR},"expected_digest":"sha256:aa"}}"#);
        assert!(serde_json::from_str::<UploadBody>(&reference).is_ok());
        for bad in [
            r#"{"expected_digest":"sha256:aa"}"#.to_string(),
            format!(r#"{{"catalogue":{LOCATOR},"files":{{}}}}"#),
            format!(r#"{{"catalogue":{LOCATOR},"catalogue":{LOCATOR}}}"#),
            format!(r#"{{"catalogue":{LOCATOR},"catalogue_resolution":"manual"}}"#),
        ] {
            assert!(serde_json::from_str::<UploadBody>(&bad).is_err(), "{bad}");
        }
    }

    fn no_release(
        _: &CatalogueWire,
        _: Option<Vec<(String, String)>>,
    ) -> Result<Vec<(String, String)>, HttpResp> {
        panic!("this body must not reach a catalogue fetch")
    }

    /// A loopback catalog serving `release` as version 1.2.3 under its own
    /// digest, a tampered copy as 6.6.6 under the same digest, and 404 for
    /// anything else. Returns the origin and its request count.
    fn fake_catalog(
        release: &BTreeMap<String, String>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let digest = crate::issue::app_catalog::workspace::bundle_digest(release);
        let release = release.clone();
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                seen.fetch_add(1, SeqCst);
                let url = request.url().to_string();
                let pinned = url.contains(&format!("digest={}", digest.replace(':', "%3A")));
                let body = if pinned && url.contains("version=1.2.3") {
                    json!({"files": release, "expected_digest": digest})
                } else if pinned && url.contains("version=6.6.6") {
                    json!({"files": {"app.md": "tampered"}, "expected_digest": digest})
                } else {
                    let _ = request.respond(tiny_http::Response::empty(404));
                    continue;
                };
                let _ = request.respond(tiny_http::Response::from_data(body.to_string()));
            }
        });
        (format!("http://127.0.0.1:{port}"), hits)
    }

    fn locator(version: &str, digest: &str) -> CatalogueWire {
        CatalogueWire {
            id: "social-content".into(),
            version: version.into(),
            digest: digest.into(),
        }
    }

    /// CAD-1942: a reference-only catalogue upgrade of a release over 64 KiB
    /// is fetched by the board, verified against the locator digest and
    /// staged for the unchanged daemon path; a lying catalog, an unknown
    /// release, a forged locator, differing posted files and a new-digest pin
    /// that disagrees with the locator are refused.
    #[test]
    fn cad1942_catalogue_release_is_fetched_and_verified() {
        use std::sync::atomic::Ordering::SeqCst;
        let release: BTreeMap<String, String> = [
            (
                "app.md".to_string(),
                "---\napp: social-content\nversion: '1.2.3'\n---\n".to_string(),
            ),
            ("screens/main/app.js".to_string(), "x".repeat(80 * 1024)),
        ]
        .into();
        assert!(release.keys().all(|path| upload_path_ok(path)));
        let digest = crate::issue::app_catalog::workspace::bundle_digest(&release);
        let (origin, hits) = fake_catalog(&release);
        let fetch = |locator: &CatalogueWire, posted: Option<Vec<(String, String)>>| {
            catalogue_release_files(&origin, locator, posted)
        };
        let status = |result: Result<Vec<(String, String)>, HttpResp>| match result {
            Ok(_) => panic!("refusal expected"),
            Err(resp) => resp.status_code().0,
        };

        let body = json!({
            "catalogue": {"id": "social-content", "version": "1.2.3", "digest": digest},
            "expected_digest": "sha256:old",
            "expected_generation": "gen",
            "expected_new_digest": digest,
            "request_id": "req-1",
        });
        let wire: UpgradeWire = serde_json::from_value(body.clone()).unwrap();
        let Ok((params, Some(staging))) =
            workspace_upgrade_transport("app_workspace_upgrade", "inst", wire, &fetch)
        else {
            panic!("a reference-only catalogue upgrade is admitted and staged");
        };
        let staged = std::fs::read_to_string(staging.path().join("screens/main/app.js")).unwrap();
        assert!(staged.len() > 64 * 1024);
        assert_eq!(params["catalogue"]["digest"], json!(digest));
        assert_eq!(params["expected_new_digest"], json!(digest));
        assert_eq!(hits.load(SeqCst), 1);

        let Ok(fetched) = fetch(&locator("1.2.3", &digest), None) else {
            panic!("the pinned release is fetched");
        };
        assert_eq!(fetched.into_iter().collect::<BTreeMap<_, _>>(), release);
        let posted: Vec<(String, String)> = release.clone().into_iter().collect();
        assert!(fetch(&locator("1.2.3", &digest), Some(posted)).is_ok());
        let differing = vec![("app.md".to_string(), "other".to_string())];
        assert_eq!(
            status(fetch(&locator("1.2.3", &digest), Some(differing))),
            409
        );
        assert_eq!(status(fetch(&locator("6.6.6", &digest), None)), 409);
        let unknown = format!("sha256:{}", "b".repeat(64));
        assert_eq!(status(fetch(&locator("1.2.3", &unknown), None)), 404);

        let before = hits.load(SeqCst);
        let forged = CatalogueWire {
            id: "../admin".into(),
            ..locator("1.2.3", &digest)
        };
        assert_eq!(status(fetch(&forged, None)), 400);
        assert_eq!(status(fetch(&locator("1.2.3", "sha256:short"), None)), 400);
        assert_eq!(
            hits.load(SeqCst),
            before,
            "a forged locator is never fetched"
        );

        let mut stale = body;
        stale["expected_new_digest"] = json!(unknown);
        let wire: UpgradeWire = serde_json::from_value(stale).unwrap();
        let Err(resp) =
            workspace_upgrade_transport("app_workspace_upgrade", "inst", wire, &no_release)
        else {
            panic!("an expected_new_digest other than the locator is refused");
        };
        assert_eq!(resp.status_code().0, 409);
    }

    /// An automatic apply forwards the consent pair. A manual apply omits
    /// it. A partial pair, or the pair on a check, is a 400 and is not
    /// forwarded as a manual upgrade.
    #[test]
    fn automatic_upgrade_claim_is_forwarded_and_closed() {
        let admit = |body: &str| {
            let wire: UpgradeWire = serde_json::from_str(body).unwrap();
            match workspace_upgrade_transport("app_workspace_upgrade", "inst", wire, &no_release) {
                Ok(admitted) => admitted,
                Err(_) => panic!("upgrade transport refused a closed body"),
            }
        };
        let manual = r#"{"source":"/tmp/app","expected_digest":"sha256:aa","expected_generation":"gen","expected_new_digest":"sha256:bb","request_id":"req-1"}"#;
        let (params, staging) = admit(manual);
        assert!(staging.is_none());
        assert!(params.get("automatic").is_none());
        assert!(params.get("observed_approval").is_none());

        let claimed = r#"{"source":"/tmp/app","expected_digest":"sha256:aa","expected_generation":"gen","expected_new_digest":"sha256:bb","request_id":"req-1","automatic":true,"observed_approval":"approved"}"#;
        let (params, _) = admit(claimed);
        assert_eq!(params["automatic"], json!(true));
        assert_eq!(params["observed_approval"], json!("approved"));
        assert_eq!(params["expected_digest"], json!("sha256:aa"));

        for bad in [
            r#"{"source":"/tmp/app","expected_digest":"sha256:aa","expected_generation":"gen","expected_new_digest":"sha256:bb","request_id":"req-1","automatic":true}"#,
            r#"{"source":"/tmp/app","expected_digest":"sha256:aa","expected_generation":"gen","expected_new_digest":"sha256:bb","request_id":"req-1","automatic":false,"observed_approval":"approved"}"#,
            r#"{"source":"/tmp/app","expected_digest":"sha256:aa","expected_generation":"gen","expected_new_digest":"sha256:bb","request_id":"req-1","observed_approval":"approved"}"#,
            r#"{"source":"/tmp/app","expected_digest":"sha256:aa","expected_generation":"gen","expected_new_digest":"sha256:bb","request_id":"req-1","automatic":true,"observed_approval":"revoked"}"#,
            r#"{"source":"/tmp/app","expected_digest":"sha256:aa","expected_generation":"gen","automatic":true,"observed_approval":"approved"}"#,
        ] {
            let wire: UpgradeWire = serde_json::from_str(bad).unwrap();
            assert!(
                workspace_upgrade_transport("app_workspace_upgrade", "inst", wire, &no_release)
                    .is_err(),
                "{bad}"
            );
        }

        let check = r#"{"source":"/tmp/app","expected_digest":"sha256:aa","expected_generation":"gen","automatic":true,"observed_approval":"approved"}"#;
        let wire: UpgradeWire = serde_json::from_str(check).unwrap();
        assert!(workspace_upgrade_transport(
            "app_workspace_upgrade_check",
            "inst",
            wire,
            &no_release
        )
        .is_err());
    }

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

#[cfg(test)]
#[path = "cad1942_by_reference_refusal.rs"]
mod cad1942_by_reference_refusal;
