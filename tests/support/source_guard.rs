//! Source inventory for the process-env mutation guard (CAD-643).
//!
//! Roots must be real directories. Any symlink root or entry is refused,
//! including Rust file links and directory links: an owned source cannot
//! hide behind a link to an unscanned tree. Only regular `.rs` files are
//! scanned, with deterministic deduplication and path-bearing I/O errors.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

fn path_error(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn invalid_source(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{}: {reason}", path.display()),
    )
}

/// Discover regular Rust sources, refusing links and unreadable input.
pub fn rust_sources(roots: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let mut sources = BTreeSet::new();
    for root in roots {
        collect_rust_sources(root, &mut sources)?;
    }
    Ok(sources.into_iter().collect())
}

fn collect_rust_sources(directory: &Path, sources: &mut BTreeSet<PathBuf>) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(directory).map_err(|e| path_error(directory, e))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_source(
            directory,
            "source root must be a regular directory, not a symlink",
        ));
    }
    for entry in std::fs::read_dir(directory).map_err(|e| path_error(directory, e))? {
        let entry = entry.map_err(|e| path_error(directory, e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| path_error(&path, e))?;
        if kind.is_symlink() {
            return Err(invalid_source(
                &path,
                "symlink in owned source tree is refused",
            ));
        }
        if kind.is_dir() {
            collect_rust_sources(&path, sources)?;
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "rs") {
            sources.insert(path);
        }
    }
    Ok(())
}

/// Apply the existing guard predicate/exemptions to every discovered source.
pub fn process_env_offenders(sources: &[PathBuf]) -> io::Result<Vec<String>> {
    let set = concat!("std::env::", "set_var");
    let remove = concat!("std::env::", "remove_var");
    let lock = concat!("ENV_", "LOCK");
    // Not test config: `PATH` is prepended once per process so children
    // run the binary under test, `GL_TOKEN` exists only inside the
    // forge-credential test that sets and removes it, and
    // `CADENCE_PI_COMMAND` is the probe's own scrub — it must not
    // leak into the real `pi` child it spawns (status_prompt_probe is
    // `#[ignore]`d; the removal is its whole point).
    let exempt = ["\"PATH\"", "\"GL_TOKEN\"", "\"CADENCE_PI_COMMAND\""];
    let mut offenders = Vec::new();
    for path in sources {
        let src = std::fs::read_to_string(path).map_err(|e| path_error(path, e))?;
        offenders.extend(
            src.lines()
                .enumerate()
                .filter(|(_, l)| {
                    l.contains(lock)
                        || ((l.contains(set) || l.contains(remove))
                            && !exempt.iter().any(|v| l.contains(v)))
                })
                .map(|(i, l)| format!("{}:{}: {}", path.display(), i + 1, l.trim())),
        );
    }
    Ok(offenders)
}
