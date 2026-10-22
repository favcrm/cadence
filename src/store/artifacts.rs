//! CAD-1726: agent canvas artifacts.
//!
//! One row per stored version of an artifact an agent put on a turn.
//! Bytes live content-addressed under `<state>/artifacts/<sha256>`
//! (0700 dir, 0600 files, temp + rename); the table is the only index and
//! the daemon-minted `id` + `version` the only handle a caller ever sees.
//! A row is bound to ONE conversation (`thread_id`) and carries the same
//! scope label chat attachments use (`home` or `app:<install>@<thread>`),
//! which the daemon re-proves on every read.
//!
//! [`ArtifactStore`] is the seam: the local implementation is here; the
//! hosted one is [`HostedArtifacts`], which refuses until AOS-346 lands
//! (a hosted state dir is snapshotted whole and capped, so artifact bytes
//! never go to workspace custody there).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::threads::{NewEntry, KIND_ARTIFACT, ROLE_AGENT};
use super::{now, Store, StoreConn};
use crate::error::{Error, Result};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS artifacts(
    id TEXT NOT NULL,
    version INTEGER NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    digest TEXT NOT NULL,
    scope TEXT NOT NULL,
    context_id TEXT NOT NULL DEFAULT '',
    thread_id TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created REAL NOT NULL,
    PRIMARY KEY(id, version));
CREATE INDEX IF NOT EXISTS artifacts_thread ON artifacts(thread_id);
";

/// Custody leaf under the daemon state dir.
pub const ARTIFACTS_DIR: &str = "artifacts";
/// One version is at most 10 MiB; above it nothing is stored.
pub const ARTIFACT_MAX_BYTES: usize = 10 * 1024 * 1024;
/// The newest versions kept per id; older ones are pruned.
pub const ARTIFACT_KEEP_VERSIONS: i64 = 20;
const TITLE_MAX: usize = 120;
pub const ARTIFACT_KINDS: &[&str] = &["md", "pdf", "html", "image", "csv"];

/// Serializes blob writes against prune deletes, so a prune can never
/// remove a blob a concurrent put has written but not yet indexed.
static CUSTODY: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct Artifact {
    pub id: String,
    pub version: i64,
    pub kind: String,
    pub title: String,
    pub bytes: u64,
    pub digest: String,
    pub scope: String,
    pub context_id: String,
    pub thread_id: String,
    pub created_by: String,
    pub created: f64,
}

impl Artifact {
    /// The card payload a thread entry carries: never the content.
    pub fn card(&self) -> Value {
        json!({
            "id": self.id, "version": self.version, "kind": self.kind,
            "title": self.title, "bytes": self.bytes, "digest": self.digest,
        })
    }

    pub fn to_json(&self) -> Value {
        let mut v = self.card();
        v["created"] = json!(self.created);
        v["created_by"] = json!(self.created_by);
        v
    }

    /// The served content type, from the kind and (for images) the bytes.
    pub fn mime(&self, bytes: &[u8]) -> &'static str {
        mime_of(&self.kind, bytes)
    }
}

pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("art-").is_some_and(|rest| {
        !rest.is_empty() && rest.len() <= 40 && rest.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if std::str::from_utf8(bytes).is_ok_and(|t| t.contains("<svg")) {
        Some("image/svg+xml")
    } else {
        None
    }
}

fn mime_of(kind: &str, bytes: &[u8]) -> &'static str {
    match kind {
        "md" => "text/markdown; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "pdf" => "application/pdf",
        _ => image_mime(bytes).unwrap_or("application/octet-stream"),
    }
}

/// Refuse bytes that do not match the declared kind — the kind decides how
/// the board renders them, so it is checked against the real content.
fn check_content(kind: &str, bytes: &[u8]) -> Result<()> {
    let ok = match kind {
        "md" | "csv" | "html" => std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0),
        "pdf" => bytes.starts_with(b"%PDF-"),
        "image" => image_mime(bytes).is_some(),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "the file is not a valid {kind} artifact"
        )))
    }
}

fn clean_title(title: &str) -> Result<String> {
    let title: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::rejected("an artifact needs a title"));
    }
    Ok(title.chars().take(TITLE_MAX).collect())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn valid_digest(d: &str) -> bool {
    d.len() == 64 && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What a put binds a new version to; all of it is daemon-proven.
pub struct PutArtifact<'a> {
    pub id: Option<&'a str>,
    pub kind: &'a str,
    pub title: &'a str,
    pub thread_id: &'a str,
    pub scope: &'a str,
    pub context_id: &'a str,
    pub created_by: &'a str,
    pub message_id: &'a str,
}

/// The artifact store seam (CAD-1726). Scope proofs happen in the daemon
/// before `bytes` is called; an implementation only stores and serves.
pub trait ArtifactStore {
    fn put(&self, req: PutArtifact<'_>, bytes: &[u8]) -> Result<Artifact>;
    fn meta(&self, id: &str, version: i64) -> Result<Option<Artifact>>;
    fn versions(&self, id: &str) -> Result<Vec<Artifact>>;
    fn bytes(&self, artifact: &Artifact) -> Result<Vec<u8>>;
}

/// The hosted backend: AOS-346 is not built, so every call refuses. No
/// network code against a guessed API.
pub struct HostedArtifacts;

fn hosted_refusal() -> Error {
    Error::rejected("hosted artifact store unavailable (AOS-346)")
}

impl ArtifactStore for HostedArtifacts {
    fn put(&self, _: PutArtifact<'_>, _: &[u8]) -> Result<Artifact> {
        Err(hosted_refusal())
    }
    fn meta(&self, _: &str, _: i64) -> Result<Option<Artifact>> {
        Err(hosted_refusal())
    }
    fn versions(&self, _: &str) -> Result<Vec<Artifact>> {
        Err(hosted_refusal())
    }
    fn bytes(&self, _: &Artifact) -> Result<Vec<u8>> {
        Err(hosted_refusal())
    }
}

/// The local backend: the store's table plus `<state>/artifacts/`.
pub struct LocalArtifacts<'a> {
    pub store: &'a Store,
    pub state_dir: &'a Path,
}

fn row_artifact(r: &rusqlite::Row) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: r.get("id")?,
        version: r.get("version")?,
        kind: r.get("kind")?,
        title: r.get("title")?,
        bytes: r.get::<_, i64>("bytes")? as u64,
        digest: r.get("digest")?,
        scope: r.get("scope")?,
        context_id: r.get("context_id")?,
        thread_id: r.get("thread_id")?,
        created_by: r.get("created_by")?,
        created: r.get("created")?,
    })
}

impl LocalArtifacts<'_> {
    fn dir(&self) -> PathBuf {
        self.state_dir.join(ARTIFACTS_DIR)
    }

    fn write_blob(&self, digest: &str, bytes: &[u8]) -> Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let dir = self.dir();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let dest = dir.join(digest);
        if dest.exists() {
            return Ok(());
        }
        let tmp = dir.join(format!(".tmp-{digest}-{}", std::process::id()));
        let result = (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &dest)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(result?)
    }

    fn prune_blobs(&self, digests: &[String]) {
        for digest in digests {
            if !valid_digest(digest) {
                continue;
            }
            let held = self
                .store
                .conn()
                .query_row(
                    "SELECT 1 FROM artifacts WHERE digest=? LIMIT 1",
                    [digest],
                    |_| Ok(()),
                )
                .optional();
            if matches!(held, Ok(None)) {
                let _ = std::fs::remove_file(self.dir().join(digest));
            }
        }
    }
}

impl ArtifactStore for LocalArtifacts<'_> {
    fn put(&self, req: PutArtifact<'_>, bytes: &[u8]) -> Result<Artifact> {
        if !ARTIFACT_KINDS.contains(&req.kind) {
            return Err(Error::rejected(format!(
                "artifact kind must be one of {}",
                ARTIFACT_KINDS.join(", ")
            )));
        }
        if bytes.len() > ARTIFACT_MAX_BYTES {
            return Err(Error::rejected(format!(
                "artifact is {} bytes — over the {ARTIFACT_MAX_BYTES}-byte limit; nothing was stored",
                bytes.len()
            )));
        }
        if bytes.is_empty() {
            return Err(Error::rejected("artifact is empty"));
        }
        check_content(req.kind, bytes)?;
        let title = clean_title(req.title)?;
        if let Some(id) = req.id {
            if !valid_id(id) {
                return Err(Error::rejected(
                    "artifact id must be a daemon-minted `art-…` id",
                ));
            }
        }
        let digest = hex(&Sha256::digest(bytes));
        let _custody = CUSTODY.lock().unwrap_or_else(|e| e.into_inner());
        self.write_blob(&digest, bytes)?;
        let written = self.store.write_tx(|tx| {
            let (id, version) = match req.id {
                Some(id) => {
                    let latest = tx
                        .query_opt(
                            "SELECT * FROM artifacts WHERE id=? ORDER BY version DESC LIMIT 1",
                            [id],
                            row_artifact,
                        )?
                        .ok_or_else(|| Error::rejected(format!("unknown artifact '{id}'")))?;
                    // A new version only ever joins the artifact's own
                    // conversation and scope.
                    if latest.thread_id != req.thread_id
                        || latest.scope != req.scope
                        || latest.context_id != req.context_id
                    {
                        return Err(Error::rejected("artifact belongs to another conversation"));
                    }
                    (latest.id, latest.version + 1)
                }
                None => (
                    format!("art-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]),
                    1,
                ),
            };
            let created = now();
            tx.execute(
                "INSERT INTO artifacts(id,version,kind,title,bytes,digest,scope,context_id,
                                       thread_id,created_by,created)
                 VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    id,
                    version,
                    req.kind,
                    title,
                    bytes.len() as i64,
                    digest,
                    req.scope,
                    req.context_id,
                    req.thread_id,
                    req.created_by,
                    created
                ],
            )?;
            let pruned = tx.query_vec(
                "SELECT digest FROM artifacts WHERE id=? AND version<=?",
                params![id, version - ARTIFACT_KEEP_VERSIONS],
                |r| r.get::<_, String>(0),
            )?;
            tx.execute(
                "DELETE FROM artifacts WHERE id=? AND version<=?",
                params![id, version - ARTIFACT_KEEP_VERSIONS],
            )?;
            let artifact = Artifact {
                id,
                version,
                kind: req.kind.to_string(),
                title,
                bytes: bytes.len() as u64,
                digest: digest.clone(),
                scope: req.scope.to_string(),
                context_id: req.context_id.to_string(),
                thread_id: req.thread_id.to_string(),
                created_by: req.created_by.to_string(),
                created,
            };
            let text = format!("Artifact \"{}\" v{}", artifact.title, artifact.version);
            Store::thread_write_by_id_in(
                tx,
                req.thread_id,
                NewEntry {
                    role: ROLE_AGENT,
                    kind: KIND_ARTIFACT,
                    text: &text,
                    payload: Some(artifact.card()),
                    message_id: Some(req.message_id),
                },
            )?;
            Ok((artifact, pruned))
        });
        let (artifact, pruned) = match written {
            Ok(done) => done,
            Err(e) => {
                // A refused put leaves no orphan blob behind.
                self.prune_blobs(std::slice::from_ref(&digest));
                return Err(e);
            }
        };
        self.prune_blobs(&pruned);
        Ok(artifact)
    }

    fn meta(&self, id: &str, version: i64) -> Result<Option<Artifact>> {
        Ok(self
            .store
            .conn()
            .query_row(
                "SELECT * FROM artifacts WHERE id=? AND version=?",
                params![id, version],
                row_artifact,
            )
            .optional()?)
    }

    fn versions(&self, id: &str) -> Result<Vec<Artifact>> {
        Ok(self.store.conn().query_vec(
            "SELECT * FROM artifacts WHERE id=? ORDER BY version",
            [id],
            row_artifact,
        )?)
    }

    fn bytes(&self, artifact: &Artifact) -> Result<Vec<u8>> {
        if !valid_digest(&artifact.digest) {
            return Err(Error::internal("artifact digest is malformed"));
        }
        let bytes = std::fs::read(self.dir().join(&artifact.digest))
            .map_err(|_| Error::rejected("artifact bytes are no longer available"))?;
        if bytes.len() as u64 != artifact.bytes || hex(&Sha256::digest(&bytes)) != artifact.digest {
            return Err(Error::rejected("artifact bytes do not match their digest"));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewAgent;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Store, String) {
        let dir = TempDir::new().unwrap();
        let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        s.register_agent(&NewAgent {
            alias: "master",
            provider: "fake",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        let thread = s.ensure_thread("master").unwrap().id;
        (dir, s, thread)
    }

    fn req<'a>(id: Option<&'a str>, thread: &'a str) -> PutArtifact<'a> {
        PutArtifact {
            id,
            kind: "md",
            title: "Weekly report",
            thread_id: thread,
            scope: "home",
            context_id: "",
            created_by: "master",
            message_id: "m1",
        }
    }

    /// Versions chain under one id, the card lands in the thread with the
    /// full digest, 20 are kept, an oversize put stores nothing, and a
    /// second conversation cannot add a version.
    #[test]
    fn put_versions_prunes_and_refuses() {
        let (dir, s, thread) = fixture();
        let backend = LocalArtifacts {
            store: &s,
            state_dir: dir.path(),
        };
        let first = backend.put(req(None, &thread), b"# one").unwrap();
        assert_eq!((first.version, first.bytes), (1, 5));
        let entries = s.thread_entries_of(&thread, 0, 100).unwrap();
        let card = entries.iter().find(|e| e.kind == "artifact").unwrap();
        assert_eq!(
            card.payload.as_ref().unwrap()["digest"],
            json!(first.digest)
        );
        for n in 2..=22 {
            let v = backend
                .put(req(Some(&first.id), &thread), format!("# v{n}").as_bytes())
                .unwrap();
            assert_eq!(v.version, n);
        }
        let kept = backend.versions(&first.id).unwrap();
        assert_eq!((kept.first().unwrap().version, kept.len()), (3, 20));
        assert!(backend.meta(&first.id, 1).unwrap().is_none());
        let big = vec![b'a'; ARTIFACT_MAX_BYTES + 1];
        assert!(backend.put(req(None, &thread), &big).is_err());
        assert!(backend.put(req(Some(&first.id), "other"), b"x").is_err());
        let blobs = std::fs::read_dir(dir.path().join(ARTIFACTS_DIR))
            .unwrap()
            .count();
        assert_eq!(
            blobs, 20,
            "pruned blobs are removed, refused puts store none"
        );
        assert!(HostedArtifacts.put(req(None, &thread), b"x").is_err());
    }
}
