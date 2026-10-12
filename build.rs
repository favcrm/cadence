//! Compile the build's git identity into the binary: the deploy-drift
//! check compares the *running daemon's* commit against the tracker's
//! default branch, and `daemon_info`/`/api/meta`/`--version` report it.
//! Every value falls back to `unknown` when git or a repo is absent —
//! consumers treat `unknown` as "cannot tell", never as zero.

// Runs in cargo, never in the daemon: the CAD-308 spawn registry
// (`cadence_agent::reaper`) does not apply to build scripts.
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A hung `git` (credential prompt, `safe.directory` refusal waiting
/// on input) must never stall a build — five seconds per call, then
/// kill and fall back to `unknown`.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn git(checkout: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(checkout)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + GIT_TIMEOUT;
    let out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait_with_output().ok()?,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    };
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Existing directory watches catch loose-ref creation/deletion, including
/// packing and unpacking. A missing watched file makes Cargo rerun forever.
/// Watch packed-refs only while present: creating it cannot supersede a loose
/// ref until that ref changes/disappears under the watched refs directory.
/// Watch the nearest existing ref parent, not objects, index or reflogs.
pub(crate) fn checkout_watch_paths(checkout: &Path) -> Vec<PathBuf> {
    let dotgit = checkout.join(".git");
    let mut paths = Vec::new();
    let gitdir = if dotgit.is_file() {
        paths.push(dotgit.clone());
        let Ok(text) = std::fs::read_to_string(&dotgit) else {
            return paths;
        };
        let Some(rest) = text.trim().strip_prefix("gitdir:") else {
            return paths;
        };
        checkout.join(rest.trim())
    } else {
        dotgit
    };
    let Ok(gitdir) = gitdir.canonicalize() else {
        return paths;
    };
    let head = gitdir.join("HEAD");
    if !head.is_file() {
        // An incomplete Git directory can acquire HEAD later.
        paths.push(gitdir);
        return paths;
    }
    paths.push(head.clone());
    let Ok(head_text) = std::fs::read_to_string(&head) else {
        return paths;
    };
    let Some(reference) = head_text.trim().strip_prefix("ref:") else {
        return paths;
    };
    // Worktree commondir paths are relative to the worktree's Git directory.
    let commondir = gitdir.join("commondir");
    let common = if commondir.is_file() {
        paths.push(commondir.clone());
        std::fs::read_to_string(&commondir)
            .ok()
            .map(|c| gitdir.join(c.trim()))
            .unwrap_or_else(|| gitdir.clone())
    } else {
        gitdir
    };
    let Ok(common) = common.canonicalize() else {
        return paths;
    };
    let mut ref_parent = common.join(reference.trim());
    ref_parent.pop();
    // Retain an existing ancestor if pruning removed a nested ref directory.
    // Its directory watch observes that directory/ref being created again.
    while !ref_parent.is_dir() {
        if !ref_parent.pop() {
            return paths;
        }
    }
    paths.push(ref_parent);
    let packed = common.join("packed-refs");
    if packed.is_file() {
        paths.push(packed);
    }
    paths.sort();
    paths.dedup();
    paths
}

fn watch_checkout() {
    for path in checkout_watch_paths(Path::new(".")) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn main() {
    // The watched files keep the baked commit fresh on commit or
    // checkout; source edits still re-run the script on their own.
    watch_checkout();
    let unknown = || "unknown".to_string();
    for (var, args) in [
        ("CADENCE_BUILD_COMMIT", vec!["rev-parse", "HEAD"]),
        (
            "CADENCE_BUILD_TIME",
            vec!["show", "-s", "--format=%cI", "HEAD"],
        ),
        (
            "CADENCE_BUILD_REMOTE",
            vec!["config", "--get", "remote.origin.url"],
        ),
        ("CADENCE_BUILD_ROOT", vec!["rev-parse", "--show-toplevel"]),
    ] {
        println!(
            "cargo:rustc-env={var}={}",
            git(Path::new("."), &args).unwrap_or_else(unknown)
        );
    }
    native_fixture_root();
}

// ---------- CAD-1159 SEC-002 fixture PUBLIC root intake ----------
//
// The owner stages an elected PUBLIC image-verification root in the fixed
// private build input `.cadence/native-fixture-public-root-v1.txt`
// (repository-relative, no env/argv-customizable path). This script emits
// typed public constants into OUT_DIR; only the cfg-gated
// `fixture_admission` module includes them — every other artifact links a
// zero-length slice that can never satisfy the compiled-root lookup. A
// valid fixture compilation with a missing or malformed descriptor FAILS
// the build; the stub exists solely so invalid feature combinations reach
// the crate-level compile_error marker instead of dying here.

/// Fixed repository-relative PRIVATE build input — never caller-selected.
const FIXTURE_DESCRIPTOR: &str = ".cadence/native-fixture-public-root-v1.txt";
/// Exact first line of the seven-line descriptor.
const DESCRIPTOR_TAG: &str = "cadence-native-fixture-public-root-v1";
const DESCRIPTOR_MAX_BYTES: usize = 2048;
/// Owner-elected window bound: at most two hours (15-minute resource
/// lifetimes and 300-second request lifetimes are unchanged elsewhere).
const MAX_ELECTION_WINDOW_MS: u64 = 7_200_000;
const MAX_SAFE_MS: u64 = 9007199254740991;
const MAX_KEY_VERSION: u64 = 2147483647;

struct FixtureElection {
    kid: String,
    key_version: u64,
    public_key: [u8; 32],
    source: String,
    not_before_ms: u64,
    expires_at_ms: u64,
}

fn fail(reason: &str) -> ! {
    // Refusal names the field and reason only — never descriptor bytes,
    // key material or the source blob.
    panic!("native-fixture public root: {reason}");
}

fn is_kid(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn non_zero_hex(text: &str) -> bool {
    text.bytes().any(|b| b != b'0')
}
fn canonical_int(text: &str, lo: u64, hi: u64) -> u64 {
    if text.is_empty() || text.len() > 16 || !text.bytes().all(|b| b.is_ascii_digit()) {
        fail("integer field is not canonical digits");
    }
    if text.len() > 1 && text.starts_with('0') {
        fail("integer field carries a non-canonical leading zero");
    }
    let value: u64 = text
        .parse()
        .unwrap_or_else(|_| fail("integer field out of range"));
    if value < lo || value > hi {
        fail("integer field outside its declared bounds");
    }
    value
}

fn descriptor_field<'t>(lines: &[&'t str], index: usize, name: &str) -> &'t str {
    lines[index]
        .strip_prefix(name)
        .unwrap_or_else(|| fail("descriptor field missing, duplicated, renamed or out of order"))
}

fn parse_fixture_descriptor(text: &str) -> FixtureElection {
    let lines: Vec<&str> = text.split('\n').collect();
    let lines: &[&str] = match lines.as_slice() {
        [all @ .., ""] if all.len() == 7 => all, // one optional trailing LF
        all if all.len() == 7 => all,
        _ => fail("descriptor is not exactly seven lines"),
    };
    if lines[0] != DESCRIPTOR_TAG {
        fail("descriptor tag line mismatch");
    }
    let kid = descriptor_field(lines, 1, "kid=").to_string();
    if !is_kid(&kid) {
        fail("kid is not 1..64 ascii [alnum._-]");
    }
    let key_version = canonical_int(
        descriptor_field(lines, 2, "keyVersion="),
        1,
        MAX_KEY_VERSION,
    );
    let public_hex = descriptor_field(lines, 3, "publicKey=");
    if !is_lower_hex(public_hex, 64) || !non_zero_hex(public_hex) {
        fail("publicKey is not 64 lower-hex non-zero");
    }
    let mut public_key = [0u8; 32];
    for (i, byte) in public_key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&public_hex[i * 2..i * 2 + 2], 16)
            .unwrap_or_else(|_| fail("publicKey is not lower-hex"));
    }
    let source = descriptor_field(lines, 4, "source=").to_string();
    if !is_lower_hex(&source, 40) || !non_zero_hex(&source) {
        fail("source is not 40 lower-hex non-zero");
    }
    let not_before_ms = canonical_int(descriptor_field(lines, 5, "notBeforeMs="), 1, MAX_SAFE_MS);
    let expires_at_ms = canonical_int(descriptor_field(lines, 6, "expiresAtMs="), 1, MAX_SAFE_MS);
    if expires_at_ms <= not_before_ms {
        fail("expiresAtMs must exceed notBeforeMs");
    }
    if expires_at_ms - not_before_ms > MAX_ELECTION_WINDOW_MS {
        fail("election window exceeds two hours");
    }
    FixtureElection {
        kid,
        key_version,
        public_key,
        source,
        not_before_ms,
        expires_at_ms,
    }
}

/// On Linux the descriptor must be a process-owned regular file, mode
/// 0600, under a private `.cadence` parent, held open through the checks.
/// Foreign-dir or mismatched-owner staging is a REFUSAL — this script
/// never chmods or repairs paths it does not own. Other platforms (never a
/// fixture target) refuse outright.
#[cfg(unix)]
fn read_fixture_descriptor(path: &Path) -> String {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let parent = path
        .parent()
        .unwrap_or_else(|| fail("descriptor has no parent directory"));
    // The resolved private dir must be the repository's own `.cadence`:
    // a symlinked or re-rooted parent resolves elsewhere and refuses.
    let canonical_parent = parent
        .canonicalize()
        .unwrap_or_else(|_| fail(".cadence parent does not resolve (or is missing)"));
    let canonical_root = Path::new(".")
        .canonicalize()
        .unwrap_or_else(|_| fail("build root does not canonicalize"));
    if canonical_parent != canonical_root.join(".cadence") {
        fail(".cadence parent is not the repository-relative build input dir");
    }
    let euid = canonical_euid().unwrap_or_else(|| fail("cannot determine build euid"));
    let parent_meta = std::fs::metadata(&canonical_parent)
        .unwrap_or_else(|_| fail(".cadence parent metadata unreadable"));
    if parent_meta.uid() != euid || parent_meta.mode() & 0o777 != 0o700 {
        fail(".cadence parent is not private process-owned mode 0700 — refusing to repair a foreign path");
    }
    // Open WITHOUT following a symlinked tail, then hold the fd while
    // re-checking inode/uid/mode so a swap between open and read cannot
    // substitute bytes.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc_open_nofollow())
        .open(path)
        .unwrap_or_else(|_| fail("descriptor unreadable or a symlink"));
    let meta = file
        .metadata()
        .unwrap_or_else(|_| fail("descriptor metadata unreadable on the open fd"));
    let path_meta = std::fs::symlink_metadata(path)
        .unwrap_or_else(|_| fail("descriptor path metadata unreadable"));
    if !meta.is_file() || meta.uid() != euid || meta.mode() & 0o777 != 0o600 {
        fail("descriptor is not a process-owned regular file mode 0600");
    }
    if !path_meta.is_file() || path_meta.ino() != meta.ino() || path_meta.dev() != meta.dev() {
        fail("descriptor path swapped beneath the held open fd");
    }
    if meta.len() > DESCRIPTOR_MAX_BYTES as u64 {
        fail("descriptor exceeds 2048 bytes");
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    let mut held = file;
    (&mut held)
        .take(DESCRIPTOR_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap_or_else(|_| fail("descriptor read failed"));
    if bytes.len() > DESCRIPTOR_MAX_BYTES || bytes.iter().any(|b| !b.is_ascii()) {
        fail("descriptor is not bounded ASCII");
    }
    let after = held
        .metadata()
        .unwrap_or_else(|_| fail("descriptor metadata unreadable after read"));
    let after_path = std::fs::symlink_metadata(path)
        .unwrap_or_else(|_| fail("descriptor path unreadable after read"));
    if after.dev() != meta.dev()
        || after.ino() != meta.ino()
        || after.uid() != meta.uid()
        || after.mode() != meta.mode()
        || after.len() != meta.len()
        || after.mtime() != meta.mtime()
        || after.mtime_nsec() != meta.mtime_nsec()
        || after.ctime() != meta.ctime()
        || after.ctime_nsec() != meta.ctime_nsec()
        || bytes.len() as u64 != meta.len()
        || !after_path.is_file()
        || after_path.dev() != meta.dev()
        || after_path.ino() != meta.ino()
    {
        fail("descriptor changed across held read");
    }
    drop(held);
    String::from_utf8(bytes).unwrap_or_else(|_| fail("descriptor is not utf8/ascii"))
}

#[cfg(not(unix))]
fn read_fixture_descriptor(_path: &Path) -> String {
    fail("native-fixture descriptor intake requires Unix ownership checks")
}

/// O_NOFOLLOW on the open without new dependencies: std exposes the flag
/// through `OpenOptions::custom_flags`; the constant is Linux-stable.
#[cfg(target_os = "linux")]
fn libc_open_nofollow() -> i32 {
    // O_NOFOLLOW (Linux): refuse to open through a symlinked final component.
    0o400000
}
#[cfg(all(unix, not(target_os = "linux")))]
fn libc_open_nofollow() -> i32 {
    // O_NOFOLLOW (BSD/macOS).
    0x0100
}

/// The build process's euid from /proc on Linux — std has no uid API and
/// pulling libc in as a build-dependency only for getuid is not justified.
#[cfg(target_os = "linux")]
fn canonical_euid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}
#[cfg(all(unix, not(target_os = "linux")))]
fn canonical_euid() -> Option<u32> {
    None
}

fn emit_fixture_root(election: Option<&FixtureElection>) {
    let mut out = String::new();
    out.push_str(
        "pub(crate) struct GeneratedFixtureKey {\n    pub(crate) kid: &'static str,\n    pub(crate) version: u64,\n    pub(crate) public_key: [u8; 32],\n    pub(crate) source: &'static str,\n    pub(crate) not_before_ms: u64,\n    pub(crate) expires_at_ms: u64,\n}\n",
    );
    match election {
        Some(e) => {
            out.push_str("pub(crate) const NATIVE_FIXTURE_IMAGE_AUTHORITY_KEYS: &[GeneratedFixtureKey] = &[GeneratedFixtureKey {");
            out.push_str(&format!("\n    kid: {:?},", e.kid));
            out.push_str(&format!("\n    version: {},", e.key_version));
            out.push_str(&format!("\n    public_key: {:?},", e.public_key));
            out.push_str(&format!("\n    source: {:?},", e.source));
            out.push_str(&format!("\n    not_before_ms: {},", e.not_before_ms));
            out.push_str(&format!(
                "\n    expires_at_ms: {},\n}}];\n",
                e.expires_at_ms
            ));
        }
        None => out.push_str(
            "pub(crate) const NATIVE_FIXTURE_IMAGE_AUTHORITY_KEYS: &[GeneratedFixtureKey] = &[];\n",
        ),
    }
    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR");
    std::fs::write(Path::new(&out_dir).join("native_fixture_root.rs"), out)
        .expect("emit native_fixture_root.rs");
}

fn native_fixture_root() {
    let path = Path::new(FIXTURE_DESCRIPTOR);
    // Rebuild when the owner stages or rotates the private descriptor.
    println!("cargo:rerun-if-changed={FIXTURE_DESCRIPTOR}");
    let valid_combo = std::env::var_os("CARGO_FEATURE_NATIVE_FIXTURE_ADMISSION").is_some()
        && std::env::var_os("CARGO_FEATURE_TEST_SEAM").is_some()
        && std::env::var("PROFILE").ok().as_deref() == Some("debug");
    if !valid_combo {
        // Every other compilation — all release shapes, and debug builds
        // without the pair — emits only the empty stub. It can never
        // authenticate (the compiled-root lookup stays empty), and the
        // crate-level compile_error, not this script, refuses the
        // forbidden feature combination. The descriptor is never read.
        emit_fixture_root(None);
        return;
    }
    if !path.is_file() {
        fail("feature combo requires the owner-elected private descriptor — missing");
    }
    let text = read_fixture_descriptor(path);
    let election = parse_fixture_descriptor(&text);
    emit_fixture_root(Some(&election));
}
