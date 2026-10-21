//! CAD-1642: Publish Web behind the daemon's platform gate.
//!
//! `cadence web` used to call `/v1/web-sites` from the CLI process. This
//! adapter is the one transport now: the CLI freezes and stages a bounded
//! bundle under the daemon's state directory, and every upload or action
//! request is a reviewed tool call with a caller, an effect row and the
//! daemon's single credential renewal.
//!
//! The protocol and its durable per-org intents live in
//! [`crate::web_sites`]; this file only maps reviewed tools to it. No tool
//! here publishes: `publish_site`, `rollback_site` and `unpublish_site` are
//! `draft` calls that create the AgenticOS owner action.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::adapter::PlatformAdapter;
use crate::contract_fixture::ToolTable;
use crate::error::{Error, Result};
use crate::remote_cli::{self, RemoteTarget};

pub const PLATFORM: &str = "agenticos-web";
const STAGING_DIR: &str = "web-staging";
const PLAN_CAP: u64 = 8 * 1024 * 1024;
const WAKE_DEFAULT: u64 = 120;
const WAKE_MAX: u64 = 600;

const TABLE: &str = r#"{"platform":"agenticos-web","manifest_version":"cadence-web-sites@1","tools":[
 {"tool":"prepare_site","effect":"draft","scopes":["web.prepare"],"label":"Upload a staged static site and finalize a ready release (no build, no publish)"},
 {"tool":"publish_site","effect":"draft","scopes":["web.prepare"],"label":"Create the owner action that would publish a prepared release"},
 {"tool":"rollback_site","effect":"draft","scopes":["web.prepare"],"label":"Create the owner action that would restore an earlier release"},
 {"tool":"unpublish_site","effect":"draft","scopes":["web.prepare"],"label":"Create the owner action that would take a site offline"},
 {"tool":"site_status","effect":"read","scopes":["web.read"],"label":"Show a site's live state and this machine's in-flight actions"},
 {"tool":"site_releases","effect":"read","scopes":["web.read"],"label":"List a site's releases, newest first"},
 {"tool":"site_submissions","effect":"read","scopes":["web.read"],"label":"Read a site's form submissions, newest first"}
]}"#;

pub struct AgenticosWebAdapter {
    table: ToolTable,
    state_dir: PathBuf,
}

impl AgenticosWebAdapter {
    fn new(state_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            table: ToolTable::from_json(
                &serde_json::from_str(TABLE).expect("web tool table parses"),
            )?,
            state_dir,
        })
    }

    fn run(&self, account: &str, tool: &str, input: &Value) -> Result<Value> {
        if input
            .get("org")
            .and_then(Value::as_str)
            .is_some_and(|org| org != account)
        {
            return Err(Error::rejected(
                "agenticos-web: 'org' must equal the authorized account",
            ));
        }
        let target = target_for(account)?;
        let wake = input["wake_timeout"]
            .as_u64()
            .unwrap_or(WAKE_DEFAULT)
            .min(WAKE_MAX);
        match tool {
            "prepare_site" => {
                let (site, slug, spa) = self.read_staging(required_str(input, "staging")?)?;
                crate::web_sites::prepare_frozen(&target, site, &slug, spa, wake)
            }
            "publish_site" => crate::web_sites::request_action(
                &target,
                "publish",
                None,
                Some(required_str(input, "release")?),
                wake,
            ),
            "rollback_site" => crate::web_sites::request_action(
                &target,
                "rollback",
                Some(required_str(input, "slug")?),
                Some(required_str(input, "release")?),
                wake,
            ),
            "unpublish_site" => crate::web_sites::request_action(
                &target,
                "unpublish",
                Some(required_str(input, "slug")?),
                None,
                wake,
            ),
            "site_status" => crate::web_sites::status(&target, required_str(input, "slug")?, wake),
            "site_releases" => {
                crate::web_sites::releases(&target, required_str(input, "slug")?, wake)
            }
            "site_submissions" => crate::web_sites::submissions(
                &target,
                required_str(input, "slug")?,
                input.get("form").and_then(Value::as_str),
                input["limit"].as_u64().unwrap_or(50) as u32,
                input.get("cursor").and_then(Value::as_str),
                wake,
            ),
            _ => Err(Error::rejected("unknown agenticos-web tool")),
        }
    }

    /// Read one staging plan and its frozen bytes, verifying the digest and
    /// every file hash before any request. A path outside the daemon's own
    /// staging root, an unsafe manifest path or changed bytes is refused.
    fn read_staging(&self, token: &str) -> Result<(crate::web_publish::FrozenSite, String, bool)> {
        let valid = (8..=64).contains(&token.len())
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !valid {
            return Err(Error::rejected(
                "prepare_site: 'staging' must be one staging token",
            ));
        }
        use crate::web_publish::anchored_fs::{open_child, open_root, Child};
        let root = open_root(&self.state_dir.join(STAGING_DIR)).map_err(|error| {
            Error::rejected(format!(
                "prepare_site: cannot open the staging root: {error}"
            ))
        })?;
        let dir = match open_child(&root, token).map_err(|error| {
            Error::rejected(format!(
                "prepare_site: cannot open the staging token: {error}"
            ))
        })? {
            Child::Directory(dir) => dir,
            _ => {
                return Err(Error::rejected(
                    "prepare_site: the staging token is not a plain directory",
                ))
            }
        };
        let mut plan_bytes = Vec::new();
        match open_child(&dir, "plan.json").map_err(|error| {
            Error::rejected(format!("prepare_site: cannot open the plan: {error}"))
        })? {
            Child::Regular(file, size) if size <= PLAN_CAP => {
                file.take(PLAN_CAP + 1)
                    .read_to_end(&mut plan_bytes)
                    .map_err(|error| {
                        Error::rejected(format!("prepare_site: cannot read the plan: {error}"))
                    })?;
            }
            Child::Regular(_, _) => {
                return Err(Error::rejected("prepare_site: the plan exceeds 8 MiB"))
            }
            _ => {
                return Err(Error::rejected(
                    "prepare_site: the plan is not a plain file",
                ))
            }
        }
        let plan: Value = serde_json::from_slice(&plan_bytes)
            .map_err(|_| Error::rejected("prepare_site: the plan is not valid JSON"))?;
        let slug = plan["slug"]
            .as_str()
            .ok_or_else(|| Error::rejected("prepare_site: the plan has no slug"))?
            .to_string();
        crate::web_publish::validate_slug(&slug)?;
        let spa = plan["spa"].as_bool().unwrap_or(false);
        let digest = plan["digest"]
            .as_str()
            .ok_or_else(|| Error::rejected("prepare_site: the plan has no digest"))?
            .to_string();
        let canonical = plan["canonical"]
            .as_str()
            .ok_or_else(|| Error::rejected("prepare_site: the plan has no canonical manifest"))?
            .to_string();
        if hex(&Sha256::digest(canonical.as_bytes())) != digest {
            return Err(Error::rejected(
                "prepare_site: the plan digest does not match its canonical manifest",
            ));
        }
        let manifest: Value = serde_json::from_str(&canonical)
            .map_err(|_| Error::rejected("prepare_site: the canonical manifest is not JSON"))?;
        let rows = plan["files"]
            .as_array()
            .ok_or_else(|| Error::rejected("prepare_site: the plan has no files"))?;
        if rows.len() > crate::web_publish::MAX_FILES {
            return Err(Error::rejected("prepare_site: too many files"));
        }
        let manifest_rows = manifest["files"]
            .as_array()
            .ok_or_else(|| Error::rejected("prepare_site: the canonical manifest has no files"))?;
        if manifest_rows.len() != rows.len() {
            return Err(Error::rejected(
                "prepare_site: the plan file rows do not match the manifest",
            ));
        }
        let manifest_by_path: std::collections::BTreeMap<&str, &Value> = manifest_rows
            .iter()
            .filter_map(|row| row["path"].as_str().map(|path| (path, row)))
            .collect();
        if manifest_by_path.len() != manifest_rows.len() {
            return Err(Error::rejected(
                "prepare_site: the canonical manifest repeats a path",
            ));
        }
        let files_dir = match open_child(&dir, "files").map_err(|error| {
            Error::rejected(format!(
                "prepare_site: cannot open the files directory: {error}"
            ))
        })? {
            Child::Directory(dir) => dir,
            _ => {
                return Err(Error::rejected(
                    "prepare_site: the files entry is not a plain directory",
                ))
            }
        };
        let mut files = Vec::with_capacity(rows.len());
        let mut seen_paths = std::collections::BTreeSet::new();
        let mut total = 0u64;
        for row in rows {
            let path = row["path"]
                .as_str()
                .ok_or_else(|| Error::rejected("prepare_site: a file row has no path"))?
                .to_string();
            if path.is_empty()
                || path.starts_with('/')
                || path.contains('\\')
                || path.split('/').any(|part| part == ".." || part.is_empty())
            {
                return Err(Error::rejected("prepare_site: the plan has an unsafe path"));
            }
            let sha256 = row["sha256"]
                .as_str()
                .ok_or_else(|| Error::rejected("prepare_site: a file row has no hash"))?
                .to_string();
            let row_bytes = row["bytes"]
                .as_u64()
                .ok_or_else(|| Error::rejected("prepare_site: a file row has no size"))?;
            if !seen_paths.insert(path.clone()) {
                return Err(Error::rejected("prepare_site: the plan repeats a path"));
            }
            let manifest_row = manifest_by_path.get(path.as_str()).ok_or_else(|| {
                Error::rejected("prepare_site: the plan names a file outside the manifest")
            })?;
            if manifest_row["sha256"].as_str() != Some(sha256.as_str())
                || manifest_row["bytes"].as_u64() != Some(row_bytes)
            {
                return Err(Error::rejected(
                    "prepare_site: a plan file row does not match the manifest",
                ));
            }
            let manifest_mime = manifest_row["mime"]
                .as_str()
                .ok_or_else(|| Error::rejected("prepare_site: the manifest has no mime"))?;
            if manifest_mime != crate::web_publish::mime_for(&path)? {
                return Err(Error::rejected(
                    "prepare_site: a plan file mime does not match the manifest",
                ));
            }
            let mut bytes = Vec::new();
            match open_child(&files_dir, &crate::web_publish::encode_artifact_path(&path)).map_err(
                |error| Error::rejected(format!("prepare_site: cannot open '{path}': {error}")),
            )? {
                Child::Regular(file, _) => {
                    file.take(crate::web_publish::MAX_FILE_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|error| {
                            Error::rejected(format!("prepare_site: cannot read '{path}': {error}"))
                        })?;
                }
                _ => {
                    return Err(Error::rejected(format!(
                        "prepare_site: '{path}' is not a plain file"
                    )))
                }
            }
            if bytes.len() as u64 > crate::web_publish::MAX_FILE_BYTES {
                return Err(Error::rejected(format!(
                    "prepare_site: '{path}' exceeds the file bound"
                )));
            }
            if bytes.len() as u64 != row_bytes {
                return Err(Error::rejected(format!(
                    "prepare_site: '{path}' does not match its recorded size"
                )));
            }
            if hex(&Sha256::digest(&bytes)) != sha256 {
                return Err(Error::rejected(format!(
                    "prepare_site: '{path}' changed after it was frozen"
                )));
            }
            total += bytes.len() as u64;
            if total > crate::web_publish::MAX_TOTAL_BYTES {
                return Err(Error::rejected(
                    "prepare_site: the site exceeds the size bound",
                ));
            }
            files.push(crate::web_publish::FrozenFile {
                path: path.clone(),
                sha256,
                mime: crate::web_publish::mime_for(&path)?,
                bytes,
            });
        }
        Ok((
            crate::web_publish::FrozenSite {
                files,
                manifest,
                digest,
                total_bytes: total,
                canonical,
            },
            slug,
            spa,
        ))
    }
}

impl PlatformAdapter for AgenticosWebAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    fn reported_manifest_version(&self) -> Option<String> {
        self.table.manifest_version.clone()
    }

    fn preview(&self, account: &str, tool: &str, _input: &Value) -> String {
        format!("{tool} through agenticos-web/{account}")
    }

    fn read_back(&self, _tool: &str, _input: &Value) -> crate::contract_fixture::Verified {
        crate::contract_fixture::Verified::Unknown
    }

    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }

    /// The saved `--web` login lives in `remote_auth`; there is no separate
    /// custody credential to load.
    fn credentialless(&self) -> bool {
        true
    }

    /// Plain `execute` has no account, so it cannot select the saved login;
    /// the gate always calls `execute_for_account` for this adapter.
    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        Err("agenticos-web requires the gate-authorized account".into())
    }

    fn execute_for_account(
        &self,
        account: &str,
        _credential: &[u8],
        tool: &str,
        input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        self.run(account, tool, input)
            .map_err(|error| error.to_string())
    }
}

pub fn attach(opts: &mut crate::daemon::ServeOptions, state_dir: &Path) -> Result<()> {
    if opts.platforms.contains_key(PLATFORM) {
        return Ok(());
    }
    opts.platforms.insert(
        PLATFORM.into(),
        std::sync::Arc::new(AgenticosWebAdapter::new(state_dir.to_path_buf())?),
    );
    Ok(())
}

fn target_for(org: &str) -> Result<RemoteTarget> {
    remote_cli::account_image_targets()?
        .into_iter()
        .find(|target| target.org == org)
        .ok_or_else(|| {
            Error::rejected(format!(
                "no saved AgenticOS login for '{org}' — run `cadence login --web` for it"
            ))
        })
}

fn required_str<'a>(input: &'a Value, field: &str) -> Result<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::rejected(format!("'{field}' must be a non-empty string")))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(root: &Path) -> AgenticosWebAdapter {
        AgenticosWebAdapter::new(root.to_path_buf()).unwrap()
    }

    #[test]
    fn a_tampered_staging_plan_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join(STAGING_DIR).join("stage-0123456789abcdef");
        std::fs::create_dir_all(staging.join("files")).unwrap();
        // `digest` does not cover the recorded canonical bytes.
        std::fs::write(
            staging.join("plan.json"),
            br#"{"slug":"abc","digest":"00","canonical":"{\"version\":\"1\",\"files\":[]}","files":[]}"#,
        )
        .unwrap();
        let error = adapter(dir.path())
            .read_staging("stage-0123456789abcdef")
            .err()
            .expect("the digest mismatch must refuse");
        assert!(error.to_string().contains("digest"), "{error}");
    }

    #[test]
    fn a_symlinked_staging_token_is_refused() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("files")).unwrap();
        std::fs::write(
            outside.path().join("plan.json"),
            br#"{"slug":"abc","digest":"00","canonical":"{}","files":[]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join(STAGING_DIR)).unwrap();
        symlink(
            outside.path(),
            dir.path().join(STAGING_DIR).join("stage-0123456789abcdef"),
        )
        .unwrap();
        assert!(adapter(dir.path())
            .read_staging("stage-0123456789abcdef")
            .is_err());
    }

    #[test]
    fn a_staging_token_outside_the_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter(dir.path());
        assert!(adapter.read_staging("../escape").is_err());
        assert!(adapter.read_staging("short").is_err());
    }
}
