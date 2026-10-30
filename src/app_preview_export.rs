//! Offline exports for the canonical app-screen frame and deterministic fixtures.
//!
//! Fixture generation runs only a trusted bundled host producer. Package
//! source is validated as data, but is never sent to or executed by Node.

use crate::daemon::{render_frame_html, SCREEN_KIT_CSS, SCREEN_KIT_VERSION};
use crate::{Error, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;

const FIXTURE_CONTRACT: &str = "app-screen-projection-fixtures/8";
const FRAME_KIT_ENTRY: &str = "client.js";
const PRODUCER: &str = include_str!("app_preview_export/producer.mjs");
const INPUT_CAP: usize = 1024 * 1024;
const OUTPUT_CAP: usize = 8 * 1024 * 1024;
const STDERR_CAP: usize = 32 * 1024;

/// Write a fresh export directory, optionally mounting a checked local bundle.
pub fn write_frame_kit(
    out: &Path,
    bundle: Option<&Path>,
    parent_origin: &str,
    integrity_only: bool,
) -> Result<i32> {
    if integrity_only && bundle.is_none() {
        return Err(Error::invalid(
            "bundle_required",
            "--integrity-only requires --bundle",
        ));
    }
    validate_origin(parent_origin)?;
    if out.exists() {
        return Err(Error::rejected(
            "frame-kit output directory already exists; choose a new directory",
        ));
    }
    let (tag, css, js, metadata) = match bundle {
        None => (
            "offline-preview".to_string(),
            String::new(),
            String::new(),
            json!({"screen":null,"projection_fixture_contract":FIXTURE_CONTRACT,"kit":SCREEN_KIT_VERSION}),
        ),
        Some(path) => {
            let (files, validated) =
                crate::issue::app_catalog::workspace::snapshot_validated_local(
                    path,
                    integrity_only,
                )?;
            let tags = crate::issue::app_screen_pkg::tags_in(&files)?;
            if tags.len() != 1 {
                return Err(Error::rejected(
                    "frame-kit requires exactly one unambiguous app screen",
                ));
            }
            let pkg = crate::issue::app_screen_pkg::extract(&files, &tags[0])?;
            if pkg.contract != "app-screens/v2" {
                return Err(Error::rejected(
                    "frame-kit bundle must declare app-screens/v2",
                ));
            }
            if pkg.app != validated.manifest.app {
                return Err(Error::rejected(
                    "screen declaration app does not match the validated app manifest",
                ));
            }
            let js = pkg
                .assets
                .get(FRAME_KIT_ENTRY)
                .ok_or_else(|| {
                    Error::rejected("screen declaration entry is not a validated JavaScript asset")
                })?
                .clone();
            let css = pkg
                .assets
                .iter()
                .filter(|(name, _)| name.ends_with(".css"))
                .map(|(_, body)| body.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let digest = crate::issue::app_catalog::workspace::bundle_digest(&files);
            let mut metadata = json!({
                "screen":{"tag":pkg.tag,"app":pkg.app,"contract":pkg.contract,"may":pkg.may,
                    "tools":pkg.tools,"entry":FRAME_KIT_ENTRY,"bundle_digest":digest},
                "projection_fixture_contract":FIXTURE_CONTRACT,
                "kit":SCREEN_KIT_VERSION
            });
            if integrity_only {
                metadata["qualification"] = preview_qualification(validated.compatibility);
            }
            (pkg.tag, css, js, metadata)
        }
    };
    let html = render_frame_html(
        "0".repeat(64).as_str(),
        &tag,
        "1".repeat(64).as_str(),
        0,
        &css,
        &js,
        parent_origin,
    )?;
    std::fs::create_dir(out)
        .map_err(|error| Error::internal(format!("create preview export directory: {error}")))?;
    let write = (|| -> Result<()> {
        std::fs::write(out.join("frame.html"), html)
            .map_err(|error| Error::internal(format!("write preview frame: {error}")))?;
        std::fs::write(out.join("kit.css"), SCREEN_KIT_CSS)
            .map_err(|error| Error::internal(format!("write preview kit CSS: {error}")))?;
        std::fs::write(
            out.join("metadata.json"),
            serde_json::to_vec_pretty(&metadata)
                .map_err(|error| Error::internal(format!("encode frame metadata: {error}")))?,
        )
        .map_err(|error| Error::internal(format!("write frame metadata: {error}")))?;
        Ok(())
    })();
    if let Err(error) = write {
        return Err(Error::internal(format!(
            "incomplete frame-kit export remains at {}: {error}",
            out.display()
        )));
    }
    println!(
        "Exported offline preview frame and host kit to {}",
        out.display()
    );
    Ok(0)
}

fn validate_origin(origin: &str) -> Result<()> {
    let invalid = || {
        Error::invalid(
            "parent_origin",
            "expected a canonical exact http(s) origin with a valid host and port",
        )
    };
    // Accept only the exact serialized spelling used as the browser origin:
    // lowercase http(s), canonical IP text, lowercase ASCII DNS names and decimal ports.
    let uri = origin.parse::<ureq::http::Uri>().map_err(|_| invalid())?;
    let scheme = uri.scheme_str().ok_or_else(invalid)?;
    if !matches!(scheme, "http" | "https") {
        return Err(invalid());
    }
    let authority = uri.authority().ok_or_else(invalid)?;
    let host = authority.host();
    if host.is_empty() || origin != format!("{scheme}://{authority}") {
        return Err(invalid());
    }

    let canonical_host =
        if let Some(ipv6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            ipv6.parse::<std::net::Ipv6Addr>()
                .is_ok_and(|ip| host == format!("[{ip}]"))
        } else if let Ok(ipv4) = host.parse::<std::net::Ipv4Addr>() {
            host == ipv4.to_string()
        } else {
            host.len() <= 253
                && host.is_ascii()
                && host == host.to_ascii_lowercase()
                && host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
                && !host.rsplit('.').next().is_some_and(|label| {
                    label.bytes().all(|b| b.is_ascii_digit())
                        || label.strip_prefix("0x").is_some_and(|hex| {
                            !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                })
        };
    if !canonical_host {
        return Err(invalid());
    }

    let valid_authority = match authority.port() {
        Some(port) => {
            let port = port.as_u16();
            authority.as_str() == format!("{host}:{port}")
        }
        None => authority.as_str() == host,
    };
    if !valid_authority {
        return Err(invalid());
    }
    Ok(())
}

/// Generate labelled, synthetic canonical PUSH values without daemon, PM,
/// provider, installation state or execution of package assets.
pub fn write_fixtures(bundle: &Path, json_output: bool, integrity_only: bool) -> Result<i32> {
    if !json_output {
        return Err(Error::invalid(
            "json_required",
            "app fixtures requires --json",
        ));
    }
    let (files, validated) =
        crate::issue::app_catalog::workspace::snapshot_validated_local(bundle, integrity_only)?;
    let tags = crate::issue::app_screen_pkg::tags_in(&files)?;
    if tags.len() != 1 {
        return Err(Error::rejected(
            "fixtures requires exactly one unambiguous app screen",
        ));
    }
    let pkg = crate::issue::app_screen_pkg::extract(&files, &tags[0])?;
    if pkg.contract != "app-screens/v2" || pkg.app != validated.manifest.app {
        return Err(Error::rejected(
            "fixtures requires a matching app-screens/v2 declaration",
        ));
    }
    let input = json!({
        "contract":FIXTURE_CONTRACT,
        "bundle_digest":crate::issue::app_catalog::workspace::bundle_digest(&files),
        "manifest":{"app":validated.manifest.app,"title":validated.manifest.title,
            "version":validated.manifest.version,"summary":validated.manifest.summary,
            "capabilities":validated.manifest.capabilities},
        "inventory":files.keys().collect::<Vec<_>>(),
        "screen":{"tag":pkg.tag,"contract":pkg.contract,"may":pkg.may,"tools":pkg.tools},
        "kit":SCREEN_KIT_VERSION
    });
    let bytes = serde_json::to_vec(&input)
        .map_err(|e| Error::internal(format!("encode fixture input: {e}")))?;
    if bytes.len() > INPUT_CAP {
        return Err(Error::rejected("fixture producer input exceeds its bound"));
    }
    let output = run_producer(&bytes)?;
    let mut result: Value = serde_json::from_slice(&output)
        .map_err(|_| Error::internal("trusted fixture producer returned invalid JSON"))?;
    if integrity_only {
        result["qualification"] = preview_qualification(validated.compatibility);
    }
    println!(
        "{}",
        serde_json::to_string(&result)
            .map_err(|e| Error::internal(format!("encode fixture output: {e}")))?
    );
    Ok(0)
}

fn preview_qualification(native_compatibility: Value) -> Value {
    let mut qualification = json!({
        "validation": "package-integrity",
        "target_compatibility": null,
        "authority": "static-preview-only; no installation, approval or runtime admission"
    });
    qualification["native_compatibility"] = native_compatibility;
    qualification
}

struct Captured {
    bytes: Vec<u8>,
    overflow: bool,
}

fn capture_bounded<R: Read + Send + 'static>(
    reader: R,
    cap: usize,
) -> JoinHandle<std::io::Result<Captured>> {
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut bytes = Vec::with_capacity(cap.min(16 * 1024));
        let mut overflow = false;
        let mut chunk = [0_u8; 8192];
        loop {
            let count = reader.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            let remaining = cap.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..count.min(remaining)]);
            overflow |= count > remaining;
        }
        Ok(Captured { bytes, overflow })
    })
}

fn run_producer(input: &[u8]) -> Result<Vec<u8>> {
    let temp = tempfile::Builder::new()
        .prefix("cad-fixture-")
        .tempdir()
        .map_err(|e| Error::internal(format!("create fixture runtime directory: {e}")))?;
    let script = temp.path().join("producer.mjs");
    let home = temp.path().join("h");
    let xdg_config = temp.path().join("c");
    let xdg_cache = temp.path().join("x");
    for dir in [&home, &xdg_config, &xdg_cache] {
        std::fs::create_dir(dir)
            .map_err(|e| Error::internal(format!("create isolated fixture state: {e}")))?;
    }
    std::fs::write(&script, PRODUCER)
        .map_err(|e| Error::internal(format!("write bundled fixture producer: {e}")))?;
    let mut command = Command::new("node");
    command
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("TMPDIR", temp.path());
    let mut child = crate::reaper::spawn(&mut command)
        .map_err(|e| Error::rejected(format!("Node.js is required for app fixtures: {e}")))?;
    let stdout = capture_bounded(
        child
            .stdout
            .take()
            .ok_or_else(|| Error::internal("fixture stdout unavailable"))?,
        OUTPUT_CAP,
    );
    let stderr = capture_bounded(
        child
            .stderr
            .take()
            .ok_or_else(|| Error::internal("fixture stderr unavailable"))?,
        STDERR_CAP,
    );
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| Error::internal("fixture stdin unavailable"))?
        .write_all(input);
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::internal(format!(
            "write fixture producer input: {error}"
        )));
    }
    let status = child
        .wait()
        .map_err(|e| Error::internal(format!("wait for fixture producer: {e}")))?;
    let stdout = stdout
        .join()
        .map_err(|_| Error::internal("fixture stdout reader panicked"))?
        .map_err(|e| Error::internal(format!("read fixture stdout: {e}")))?;
    let stderr = stderr
        .join()
        .map_err(|_| Error::internal("fixture stderr reader panicked"))?
        .map_err(|e| Error::internal(format!("read fixture stderr: {e}")))?;
    if stdout.overflow || stderr.overflow {
        return Err(Error::rejected("fixture producer output exceeds its bound"));
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr.bytes);
        return Err(Error::rejected(format!(
            "trusted fixture producer failed ({}): {}",
            status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string()),
            detail.trim()
        )));
    }
    Ok(stdout.bytes)
}
