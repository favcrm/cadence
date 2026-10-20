//! CAD-1429: local Publish Web freeze. Turns one plain HTML file or one
//! already-built static folder into a bounded, byte-frozen manifest. It reads
//! bytes and hashes them; it never runs project code, installs packages or
//! follows symlinks.

use crate::error::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

#[path = "web_publish_fs.rs"]
mod anchored_fs;

pub const MAX_FILES: usize = 2_000;
pub const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const ENTRY: &str = "index.html";
/// `WEB_SITE_MANIFEST_MAX_BYTES` in the contract: the upload body cap.
pub const MANIFEST_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Served MIME types by extension, matching the AgenticOS contract
/// (`WEB_SITE_MIME_BY_EXTENSION`). An extension outside it is refused.
const MIME_BY_EXTENSION: &[(&str, &str)] = &[
    ("html", "text/html; charset=utf-8"),
    ("htm", "text/html; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("js", "text/javascript; charset=utf-8"),
    ("mjs", "text/javascript; charset=utf-8"),
    ("json", "application/json; charset=utf-8"),
    ("txt", "text/plain; charset=utf-8"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("ico", "image/x-icon"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("webmanifest", "application/manifest+json"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("pdf", "application/pdf"),
];

pub fn mime_for(path: &str) -> Result<&'static str> {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    let ext = ext.to_ascii_lowercase();
    MIME_BY_EXTENSION
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, m)| *m)
        .ok_or_else(|| reject(format!("'{path}' has an extension this site cannot serve")))
}

/// One file of a frozen site: the bytes read once and their SHA-256.
pub struct FrozenFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub mime: &'static str,
}

/// A frozen static site. `digest` is lowercase hex SHA-256 over the canonical
/// manifest JSON, the same identity the AgenticOS contract recomputes.
pub struct FrozenSite {
    pub files: Vec<FrozenFile>,
    pub manifest: Value,
    pub digest: String,
    pub total_bytes: u64,
    /// The exact canonical manifest bytes `digest` covers.
    pub canonical: String,
}

fn reject(message: impl Into<String>) -> Error {
    Error::rejected(message.into())
}

/// Lowercase letters, digits and inner hyphens, 3 to 63 characters: a DNS
/// label, so the slug can name a hostname without later rewriting.
pub fn validate_slug(slug: &str) -> Result<()> {
    let valid = (3..=63).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(reject(
            "--name must be 3-63 lowercase letters, digits or hyphens, without a leading or trailing hyphen",
        ))
    }
}

/// Freeze `input`: one `.html` file (published as `index.html`) or a folder
/// whose root holds `index.html`. Every refusal names the offending path.
pub fn freeze(input: &Path) -> Result<FrozenSite> {
    let meta = fs::symlink_metadata(input)
        .map_err(|_| reject(format!("cannot read '{}'", input.display())))?;
    if meta.file_type().is_symlink() {
        return Err(reject("symlinks are refused; pass the real file or folder"));
    }
    let mut found: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0u64;
    if meta.is_file() {
        if input.extension().and_then(|e| e.to_str()) != Some("html") {
            return Err(reject(
                "a single file must be an .html page; it is published as index.html",
            ));
        }
        // Read once; the single file's total is its real frozen byte count.
        let (file, size) = anchored_fs::open_regular(input)
            .map_err(|_| reject("cannot read the selected regular HTML file"))?;
        let bytes = read_bounded(file, "selected HTML file", size)?;
        total = bytes.len() as u64;
        found.push((ENTRY.to_string(), bytes));
    } else if meta.is_dir() {
        if input.join("package.json").exists() && !input.join(ENTRY).exists() {
            return Err(reject(
                "this is a source project, not a build output. Run your project's build \
                 (for example `npm run build`) and upload its dist/ folder; cadence never runs it",
            ));
        }
        let root = anchored_fs::open_root(input)
            .map_err(|_| reject("cannot open the selected site folder safely"))?;
        walk(&root, "", &mut found, &mut total)?;
    } else {
        return Err(reject(
            "only a regular .html file or a folder can be published",
        ));
    }
    if !found.iter().any(|(p, _)| p == ENTRY) {
        return Err(reject(
            "no index.html at the root; pass the built output folder (for example dist/)",
        ));
    }
    let mut seen = BTreeSet::new();
    for (path, _) in &found {
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(reject(format!(
                "'{path}' collides with another path case-insensitively; rename it"
            )));
        }
    }
    found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut files = Vec::with_capacity(found.len());
    for (path, bytes) in found {
        let mime = mime_for(&path)?;
        files.push(FrozenFile {
            sha256: hex(&Sha256::digest(&bytes)),
            path,
            bytes,
            mime,
        });
    }
    // Canonical manifest bytes, fixed key order and no whitespace, exactly as
    // the AgenticOS contract's `canonicalWebSiteManifestJson` writes them.
    // A `serde_json::Value` would sort keys, so the string is built here.
    let mut canonical = String::from("{\"version\":\"1\",\"files\":[");
    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            canonical.push(',');
        }
        canonical.push_str(&format!(
            "{{\"path\":{},\"sha256\":\"{}\",\"bytes\":{},\"mime\":{}}}",
            json_string(&file.path)?,
            file.sha256,
            file.bytes.len(),
            json_string(file.mime)?,
        ));
    }
    canonical.push_str("]}");
    if canonical.len() > MANIFEST_MAX_BYTES {
        return Err(reject("the manifest exceeds the 4 MiB upload body limit"));
    }
    let digest = hex(&Sha256::digest(canonical.as_bytes()));
    let manifest: Value =
        serde_json::from_str(&canonical).map_err(|_| reject("manifest could not be encoded"))?;
    Ok(FrozenSite {
        files,
        manifest,
        digest,
        total_bytes: total,
        canonical,
    })
}

fn walk(dir: &File, rel: &str, out: &mut Vec<(String, Vec<u8>)>, total: &mut u64) -> Result<()> {
    let entries = anchored_fs::entries(dir)
        .map_err(|_| reject("cannot enumerate the selected site folder safely"))?;
    for name in entries {
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        check_path(&path)?;
        let child = anchored_fs::open_child(dir, &name)
            .map_err(|_| reject(format!("cannot safely open '{path}'")))?;
        match child {
            anchored_fs::Child::Directory(child_dir) => walk(&child_dir, &path, out, total)?,
            anchored_fs::Child::Regular(file, size) => {
                if out.len() >= MAX_FILES {
                    return Err(reject(format!(
                        "more than {MAX_FILES} files; the release is too large"
                    )));
                }
                let bytes = read_bounded(file, &path, size)?;
                *total = total.saturating_add(bytes.len() as u64);
                if *total > MAX_TOTAL_BYTES {
                    return Err(reject("the site exceeds the 100 MiB total limit"));
                }
                out.push((path.clone(), bytes));
            }
            anchored_fs::Child::Other => {
                return Err(reject(format!("'{path}' is not a regular file")));
            }
        }
    }
    Ok(())
}

/// Refuse a path before reading it. A faithful port of the AgenticOS contract
/// (`isSafeWebSitePath` and `isRefusedWebSiteFile` in `web-sites.ts`): the
/// host and the CLI must accept and refuse the same paths.
fn check_path(path: &str) -> Result<()> {
    if !is_safe_web_site_path(path) {
        return Err(reject(format!(
            "'{path}' is not a safe site path (ASCII segments, no traversal, dotfiles or node_modules)"
        )));
    }
    if is_refused_web_site_file(path) {
        return Err(reject(format!(
            "'{path}' is a source map, server, build or credential file and is never published"
        )));
    }
    Ok(())
}

/// The one contract validator for a DECODED artifact path. Both the freezer and
/// the upload-file transport guard call it, so a path that the freezer would
/// refuse is also refused before a request names it.
pub(crate) fn validate_artifact_path(decoded: &str) -> Result<()> {
    check_path(decoded)
}

/// Decode one canonically percent-encoded `:encodedPath` segment for an upload
/// file route. The encoding must be exactly the canonical form the client
/// produces (only unreserved characters and `/` are written literally; `/` as
/// `%2F`), so a double-encoded or malformed-escape segment is refused rather
/// than decoded leniently. The result is validated with `validate_artifact_path`.
pub(crate) fn decode_artifact_segment(encoded: &str) -> Result<String> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .ok_or_else(|| reject("malformed percent-escape in an upload path"))?;
                let text = std::str::from_utf8(hex)
                    .map_err(|_| reject("malformed percent-escape in an upload path"))?;
                let byte = u8::from_str_radix(text, 16)
                    .map_err(|_| reject("malformed percent-escape in an upload path"))?;
                // Canonical: uppercase hex only, and never a second encoding.
                if text != format!("{byte:02X}") {
                    return Err(reject("non-canonical percent-escape in an upload path"));
                }
                out.push(byte);
                i += 3;
            }
            b @ (b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~') => {
                out.push(b);
                i += 1;
            }
            _ => return Err(reject("unencoded reserved character in an upload path")),
        }
    }
    let decoded = String::from_utf8(out).map_err(|_| reject("upload path is not UTF-8"))?;
    // A decoded `%` is a double encoding attempt, and `/` can only arrive as %2F.
    if decoded.contains('%') {
        return Err(reject("double-encoded upload path is refused"));
    }
    validate_artifact_path(&decoded)?;
    // Canonical re-encoding must equal the input: no alternate spellings.
    if encode_artifact_path(&decoded) != encoded {
        return Err(reject("upload path is not in canonical encoding"));
    }
    Ok(decoded)
}

/// The canonical `:encodedPath` for a decoded artifact path (client and guard).
pub fn encode_artifact_path(decoded: &str) -> String {
    decoded
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `WEB_SITE_PATH_MAX_BYTES` in the contract.
const PATH_MAX_BYTES: usize = 512;

/// `PATH_SEGMENT` in the contract: a leading ASCII letter, digit or `_`,
/// then up to 127 of `[A-Za-z0-9._ -]`.
fn is_safe_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    if !(first.is_ascii_alphanumeric() || *first == b'_') || rest.len() > 127 {
        return false;
    }
    rest.iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b' ' | b'-'))
}

fn is_safe_web_site_path(path: &str) -> bool {
    if path.is_empty() || path.len() > PATH_MAX_BYTES {
        return false;
    }
    if path
        .chars()
        .any(|c| (c as u32) < 0x20 || c as u32 == 0x7f || c == '\\')
    {
        return false;
    }
    if path.starts_with('/') || path.ends_with('/') {
        return false;
    }
    path.split('/').all(|segment| {
        segment != "node_modules" && is_safe_segment(segment) && !segment.ends_with('.')
    })
}

/// `isRefusedWebSiteFile` in the contract, by basename and path segments.
fn is_refused_web_site_file(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    let lower = base.to_ascii_lowercase();
    lower.ends_with(".map")
        || matches!(
            lower.as_str(),
            "package.json" | "package-lock.json" | "pnpm-lock.yaml"
        )
        || lower.starts_with("wrangler.")
        || lower == "_headers"
        || lower == "_redirects"
        || lower.ends_with(".env")
        || lower.contains(".env.")
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|e| lower.ends_with(e))
        || path.split('/').any(|s| s == "node_modules")
}

/// Read at most the per-file limit. `declared` is the size from metadata;
/// the read cap is independent of it, so a file that grows is still refused.
fn read_bounded(file: File, name: &str, declared: u64) -> Result<Vec<u8>> {
    if declared > MAX_FILE_BYTES {
        return Err(reject(format!(
            "'{name}' exceeds the 10 MiB per-file limit"
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(reject(format!(
            "'{name}' exceeds the 10 MiB per-file limit"
        )));
    }
    Ok(bytes)
}

fn json_string(text: &str) -> Result<String> {
    serde_json::to_string(text).map_err(|_| reject("manifest could not be encoded"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rules_match_a_dns_label() {
        assert!(validate_slug("my-company").is_ok());
        assert!(validate_slug("ab").is_err());
        assert!(validate_slug("-bad").is_err());
        assert!(validate_slug("Bad_Name").is_err());
    }

    /// CAD-1429: the built-React shape. The client's canonical encoding of a
    /// nested asset decodes back to the same contract path, and the encoded
    /// traversal, double-encoded, malformed and non-canonical forms are refused.
    #[test]
    fn encoded_artifact_paths_round_trip_and_refuse_bad_encodings() {
        for path in ["index.html", "assets/app-abc123.js", "assets/css/site.css"] {
            let encoded = encode_artifact_path(path);
            assert_eq!(
                decode_artifact_segment(&encoded).unwrap(),
                path,
                "{encoded}"
            );
        }
        assert_eq!(
            encode_artifact_path("assets/app-abc123.js"),
            "assets%2Fapp-abc123.js"
        );
        assert_eq!(
            encode_artifact_path("assets/css/site.css"),
            "assets%2Fcss%2Fsite.css"
        );
        for bad in [
            "..%2Fsecret.js",
            "assets%2F..%2Fx.js",
            "assets%252Fapp.js",
            "assets%2fapp.js",
            "assets%2",
            "assets%ZZ.js",
            "assets/app.js",
            "assets%5Capp.js",
            "assets%2F.env",
            "app.js.map",
        ] {
            assert!(decode_artifact_segment(bad).is_err(), "{bad} decoded");
        }
    }

    /// The contract's own refusal and acceptance vectors
    /// (`packages/contracts/src/web-sites.test.ts`, `isSafeWebSitePath`).
    #[test]
    fn path_rules_match_the_contract_vectors() {
        for bad in [
            "../x",
            "a/../b",
            "/abs",
            "a//b",
            "a/./b",
            "a\\b",
            ".env",
            "a/.git/x",
            "x.",
            "a\u{0}b",
            "node_modules/x",
            "a/b.",
            "bad:name.js",
            "name\u{7f}.js",
            "ü.css",
        ] {
            assert!(!is_safe_web_site_path(bad), "{bad:?} should be unsafe");
        }
        assert!(is_safe_web_site_path("assets/app-1.js"));
        assert!(is_safe_web_site_path("_next/chunk.js"));
        let long = "a".repeat(129);
        assert!(!is_safe_web_site_path(&long), "129-char segment");
        let too_many_bytes = format!("{}/x", "a".repeat(510));
        assert!(!is_safe_web_site_path(&too_many_bytes), "over 512 bytes");
    }

    /// CAD-1429: a single `.html` input reports its real frozen byte count
    /// in `total_bytes`, matching the frozen file and the manifest entry.
    #[test]
    fn single_html_file_reports_its_real_total_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let input = dir.path().join("page.html");
        let html = b"<!doctype html><title>cad-1429</title><p>hello</p>";
        std::fs::write(&input, html).unwrap();
        let site = freeze(&input).unwrap();
        assert_eq!(site.files.len(), 1);
        let frozen = &site.files[0];
        assert_eq!(frozen.path, ENTRY);
        assert!(site.total_bytes > 0);
        assert_eq!(site.total_bytes, frozen.bytes.len() as u64);
        assert_eq!(site.total_bytes, html.len() as u64);
        assert_eq!(
            site.manifest["files"][0]["bytes"].as_u64(),
            Some(site.total_bytes)
        );
    }

    #[cfg(unix)]
    #[test]
    fn selected_file_symlink_is_rejected_before_read() {
        let dir = tempfile::TempDir::new().unwrap();
        let outside = dir.path().join("outside.html");
        let selected = dir.path().join("selected.html");
        std::fs::write(&outside, b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, &selected).unwrap();
        assert!(freeze(&selected).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn held_root_descriptor_stays_inside_original_tree_after_rename() {
        let temp = tempfile::TempDir::new().unwrap();
        let selected = temp.path().join("site");
        let moved = temp.path().join("site-original");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&selected).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(selected.join(ENTRY), b"original safe content").unwrap();
        std::fs::write(outside.join(ENTRY), b"secret outsider content").unwrap();
        let root = anchored_fs::open_root(&selected).unwrap();
        std::fs::rename(&selected, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &selected).unwrap();

        let mut found = Vec::new();
        let mut total = 0;
        walk(&root, "", &mut found, &mut total).unwrap();
        assert_eq!(
            found,
            vec![(ENTRY.to_string(), b"original safe content".to_vec())]
        );
        assert_eq!(total, b"original safe content".len() as u64);
    }

    #[cfg(unix)]
    #[test]
    fn held_nested_directory_stays_anchored_and_counts_frozen_bytes() {
        let temp = tempfile::TempDir::new().unwrap();
        let selected = temp.path().join("site");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(selected.join("assets")).unwrap();
        std::fs::create_dir_all(outside.join("assets")).unwrap();
        let safe = b"safe bundle";
        std::fs::write(selected.join("assets/app.js"), safe).unwrap();
        std::fs::write(outside.join("assets/app.js"), b"secret bundle").unwrap();
        let root = anchored_fs::open_root(&selected).unwrap();
        let held_child = match anchored_fs::open_child(&root, "assets").unwrap() {
            anchored_fs::Child::Directory(dir) => dir,
            _ => panic!("assets directory was not opened as a directory"),
        };
        std::fs::rename(selected.join("assets"), selected.join("assets-original")).unwrap();
        std::os::unix::fs::symlink(outside.join("assets"), selected.join("assets")).unwrap();

        let mut found = Vec::new();
        let mut total = 0;
        walk(&held_child, "assets", &mut found, &mut total).unwrap();
        assert_eq!(found, vec![("assets/app.js".to_string(), safe.to_vec())]);
        assert_eq!(
            total,
            found
                .iter()
                .map(|(_, bytes)| bytes.len() as u64)
                .sum::<u64>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_rejected_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let temp = tempfile::TempDir::new().unwrap();
        let fifo = temp.path().join("pipe.txt");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: mkfifo reads a NUL-terminated path and creates only this test fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let root = anchored_fs::open_root(temp.path()).unwrap();
        assert!(matches!(
            anchored_fs::open_child(&root, "pipe.txt").unwrap(),
            anchored_fs::Child::Other
        ));
    }

    /// Hosted-refused file classes stay refused before any read.
    #[test]
    fn refused_files_are_refused_before_read() {
        for bad in [
            "app.js.map",
            "wrangler.toml",
            "wrangler.jsonc",
            "package.json",
            "pnpm-lock.yaml",
            "_headers",
            "_redirects",
            ".env.production",
            "config/server.pem",
            "keys/app.key",
            "node_modules/x.js",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
        assert!(check_path("assets/app-3f9a.js").is_ok());
    }
}
