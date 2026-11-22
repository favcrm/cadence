//! Immutable Git and Wiki citation resolution for memory evidence analysis.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use super::{Resolution, MAX_CITED_BYTES};
use crate::error::Result;
use crate::issue::{project, Pm};
use crate::memory::policy::{EvidenceKind, EvidenceRef};

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Read `path` at `revision` from the project's first registered repo.
pub(super) fn git_blob(
    pm: &Pm,
    key: &str,
    source: &str,
    revision: &str,
) -> Result<(Resolution, Option<Vec<u8>>)> {
    let projects = project::list(&pm.dir)?;
    let Some(selected) = projects.iter().find(|p| p.key == key) else {
        return Ok((
            Resolution::Unsupported(format!("project '{key}' is not in the tracker")),
            None,
        ));
    };
    let Some(repo) = selected.repos.iter().find_map(|r| r.path.as_deref()) else {
        return Ok((
            Resolution::Unsupported(format!(
                "project '{key}' registers no repo to resolve Git evidence against"
            )),
            None,
        ));
    };
    let root = project::expand_home(repo);
    let revision = revision.strip_prefix("sha256:").unwrap_or(revision);
    if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok((
            Resolution::Missing("Git evidence must cite a 40-hex commit".into()),
            None,
        ));
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-replace-objects", "-C"])
        .arg(&root)
        .args(["cat-file", "-t", revision]);
    let commit = crate::reaper::output(&mut cmd)?;
    if !commit.status.success() || commit.stdout.trim_ascii() != b"commit" {
        return Ok((
            Resolution::Unsupported(format!("Git commit {revision} is unavailable")),
            None,
        ));
    }
    let spec = format!("{revision}:{source}");
    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-replace-objects", "-C"])
        .arg(&root)
        .args(["cat-file", "-s", &spec]);
    let size = crate::reaper::output(&mut cmd)?;
    if !size.status.success() {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["--no-replace-objects", "-C"])
            .arg(&root)
            .args(["ls-tree", "-z", revision, "--", source]);
        let tree = crate::reaper::output(&mut cmd)?;
        let resolution = if tree.status.success() && tree.stdout.is_empty() {
            Resolution::Missing(format!("{source} does not exist at {revision}"))
        } else {
            Resolution::Unsupported(format!("Git source {source} cannot be read at {revision}"))
        };
        return Ok((resolution, None));
    }
    let size: usize = String::from_utf8_lossy(&size.stdout)
        .trim()
        .parse()
        .unwrap_or(usize::MAX);
    if size > MAX_CITED_BYTES {
        return Ok((
            Resolution::Unsupported(format!(
                "{source} at {revision} is {size} bytes, above the {MAX_CITED_BYTES}-byte read bound"
            )),
            None,
        ));
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-replace-objects", "-C"])
        .arg(&root)
        .args(["cat-file", "blob", &spec]);
    let blob = crate::reaper::output(&mut cmd)?;
    if !blob.status.success() {
        return Ok((
            Resolution::Unsupported(format!("Git source {source} cannot be read at {revision}")),
            None,
        ));
    }
    if blob.stdout.len() > MAX_CITED_BYTES {
        return Ok((
            Resolution::Unsupported(format!("{source} at {revision} exceeds the read bound")),
            None,
        ));
    }
    Ok((
        Resolution::Resolved {
            bytes: blob.stdout.len(),
            sha256: sha256_hex(&blob.stdout),
        },
        Some(blob.stdout),
    ))
}

/// Resolve one citation for `caller`. Never falls back to HEAD or live bytes.
pub fn resolve(
    pm: &Pm,
    key: &str,
    caller: &crate::wiki::Caller,
    reference: &EvidenceRef,
) -> Result<(Resolution, Option<Vec<u8>>)> {
    match reference.kind {
        EvidenceKind::Git => git_blob(pm, key, &reference.source, &reference.revision),
        EvidenceKind::Wiki => {
            match crate::wiki::read_revision_for_evidence(
                pm,
                caller,
                &reference.source,
                &reference.revision,
                MAX_CITED_BYTES,
            ) {
                Ok(Some(value)) => {
                    let sha256 = value
                        .get("sha256")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if sha256.is_empty() {
                        return Ok((
                            Resolution::Missing(format!(
                                "wiki '{}' has no content-addressed original at {}",
                                reference.source, reference.revision
                            )),
                            None,
                        ));
                    }
                    let content = if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
                        text.as_bytes().to_vec()
                    } else if let Some(encoded) = value.get("data_base64").and_then(|v| v.as_str())
                    {
                        use base64::Engine as _;
                        match base64::engine::general_purpose::STANDARD.decode(encoded) {
                            Ok(bytes) => bytes,
                            Err(_) => {
                                return Ok((
                                    Resolution::Unsupported(
                                        "wiki original could not be decoded".into(),
                                    ),
                                    None,
                                ))
                            }
                        }
                    } else {
                        return Ok((
                            Resolution::Unsupported("wiki original bytes are unavailable".into()),
                            None,
                        ));
                    };
                    Ok((
                        Resolution::Resolved {
                            bytes: content.len(),
                            sha256,
                        },
                        Some(content),
                    ))
                }
                Ok(None) => Ok((
                    Resolution::Missing(format!(
                        "wiki '{}' is absent at {}",
                        reference.source, reference.revision
                    )),
                    None,
                )),
                Err(error) => Ok((
                    Resolution::Unsupported(format!(
                        "wiki citation '{}' is not readable by this reader: {error}",
                        reference.source
                    )),
                    None,
                )),
            }
        }
        EvidenceKind::NativeRecord => Ok((
            Resolution::Unsupported("native records are not readable by path".into()),
            None,
        )),
        EvidenceKind::External => Ok((
            Resolution::Unsupported("external sources are not fetched by this analysis".into()),
            None,
        )),
    }
}
