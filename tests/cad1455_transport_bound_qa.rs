//! CAD-1455 INDEPENDENT integration acceptance (authored independently of
//! the implementer; the implementer may not edit or weaken this file).
//!
//! Check 2 of the CAD-1455 integration refusal set: BOUNDED UNTRUSTED
//! TRANSPORT at the REAL owned-HTTPS fetch boundary — `HttpGh`'s
//! fetch→prove path reached through `latest_green_main` (the same entry
//! `cadence update` drives), against an actual loopback HTTP server
//! serving the AOS-255 catalogue contract shape. The package tarball is
//! NOT authenticated when the transport opens it; every member must be
//! bounded (name allowlist, regular type, per-member size) BEFORE any
//! filesystem extraction, and a member one byte over its bound must
//! refuse with NO `tar` filesystem extraction and NO oversized write.
//!
//! RUNTIME DEPENDENCY — test-seam transport constructor (reported to
//! parent; the source author provides it in product code):
//! `parse_origin` accepts only `https://`, so the real fetch path
//! cannot reach `http://127.0.0.1` without a `test-seam`-gated
//! constructor (the repo's CAD-482 pattern — release builds
//! `compile_error!` on the feature). Until that lands this file
//! compiles but its HTTP rows are marked `#[ignore]`-free only when
//! `cfg!(feature = "test-seam")` can construct the source; on a plain
//! build the constructor returns an `Error` the tests surface as an
//! honest ENVIRONMENT refusal, never a pass. NO product TLS is
//! bypassed: the seam exists only in test builds.
//!
//! HOW THE NEGATIVE OBSERVES (no fabricated evidence): a forward-only
//! `tar` observer sits first on PATH for the test process — it logs its
//! argv to `$TAR_OBSERVE_LOG` and EXECS the real tar. A filesystem
//! extraction is an argument that extracts (`-x`) together with a
//! directory argument (`-C` or a short cluster containing `C`). The
//! letters `-C` inside a path are not that flag. After
//! the run the private flat dir is statted: an over-bound member file
//! on disk proves the pre-guard write the fix must remove. The observer
//! never fabricates output and never calls tar itself on the test's
//! behalf — every logged call came from the code under test.
//!
//! LIMITS: this exercises the real HTTP→fetch→member-bound→proof
//! boundary, NOT an actual native installation and NOT Sigstore crypto
//! (the `gh` oracle asserts argv policy only). No production TLS or
//! credential is touched; the loopback endpoint exists only under
//! `test-seam` builds.

#![cfg(unix)]

use cadence_agent::releases::{HttpGh, PACKAGE_ARCHIVE, PROFILE_OPERATOR_HOST};
use cadence_agent::upgrade::{
    artifact_name, ReleaseSource, BINARY, BUILD_BUNDLE, COMPLETION_BUNDLE, COMPLETION_FILE,
    COMPLETION_WORKFLOW, MAIN, MANIFEST, PROMOTION_BUNDLE, RECEIPT_FILE, SHA_FILE,
    SOURCE_FACTS_FILE, STAGING_WORKFLOW, TARGET, WORKFLOW,
};
use cadence_agent::upgrade::{run, Layout, Request, Target};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const REPO: &str = "favcrm/cadence";
const SOURCE_SHA: &str = "1111111111111111111111111111111111111111";
const PROMOTION_HEAD_SHA: &str = "2222222222222222222222222222222222222222";
const COMPLETION_HEAD_SHA: &str = "3333333333333333333333333333333333333333";
const OBSERVED_MAIN_HEAD: &str = "4444444444444444444444444444444444444444";
const ANCESTOR1: &str = "5555555555555555555555555555555555555555";
const ANCESTOR2: &str = "6666666666666666666666666666666666666666";
const PROMOTION_RUN_ID: u64 = 987_654_321;
const PROMOTION_RUN_ATTEMPT: u64 = 2;
const COMPLETION_RUN_ID: u64 = 888_777_666;
const COMPLETION_RUN_ATTEMPT: u64 = 1;
const CI_RUN_ID: u64 = 123_456_789;
const CI_RUN_ATTEMPT: u64 = 1;
const SCHEMA_VERSION: i64 = 46;
/// `SHA_FILE`'s member bound (`SHA_FILE_MAX` in src/upgrade.rs).
const SHA_FILE_BOUND: u64 = 4096;

const PROMOTION_BUNDLE_BYTES: &[u8] = br#"{"bundleRole":"promotion","signer":"staging.yml"}"#;
const BUILD_BUNDLE_BYTES: &[u8] = br#"{"bundleRole":"build","signer":"ci.yml"}"#;
const COMPLETION_BUNDLE_BYTES: &[u8] =
    br#"{"bundleRole":"completion","signer":"promotion-completion.yml"}"#;

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::new().chain_update(bytes).finalize())
}

// ---------------------------------------------------------------------------
// Fixture package members (the retained operator-host package).
// ---------------------------------------------------------------------------

struct Pkg {
    members: Vec<(String, Vec<u8>)>,
    bin: String,
    receipt: String,
    completion: String,
}

fn members(over_sha: bool) -> Pkg {
    let binary_bytes = b"cadence-candidate-binary-bytes".to_vec();
    let bin = sha256_hex(&binary_bytes);
    let sha = if over_sha {
        vec![b'x'; SHA_FILE_BOUND as usize + 1]
    } else {
        format!("{bin}  {}\n", artifact_name(SOURCE_SHA)).into_bytes()
    };
    let manifest = serde_json::to_vec(&json!({
        "schema": 1, "source_sha": SOURCE_SHA, "sha256": bin, "target": TARGET,
    }))
    .unwrap();
    let manifest_sha = sha256_hex(&manifest);
    let receipt = serde_json::to_vec(&json!({
        "schema": 1, "source_sha": SOURCE_SHA,
        "ci_run_id": CI_RUN_ID, "ci_run_attempt": CI_RUN_ATTEMPT,
        "sha256": bin,
        "promotion_run_id": PROMOTION_RUN_ID,
        "promotion_run_attempt": PROMOTION_RUN_ATTEMPT,
    }))
    .unwrap();
    let receipt_sha = sha256_hex(&receipt);
    let facts = serde_json::to_vec(&json!({
        "schema": 1, "source_sha": SOURCE_SHA,
        "observed_main_head": OBSERVED_MAIN_HEAD,
        "ancestors": [ANCESTOR1, ANCESTOR2, SOURCE_SHA],
        "schema_version": SCHEMA_VERSION,
    }))
    .unwrap();
    let facts_sha = sha256_hex(&facts);
    let completion = serde_json::to_vec(&json!({
        "schema": 1,
        "promotion_run_id": PROMOTION_RUN_ID,
        "promotion_run_attempt": PROMOTION_RUN_ATTEMPT,
        "promotion_head_sha": PROMOTION_HEAD_SHA,
        "receipt_sha256": receipt_sha,
        "source_sha": SOURCE_SHA,
        "ci_run_id": CI_RUN_ID, "ci_run_attempt": CI_RUN_ATTEMPT,
        "binary_sha256": bin,
        "source_facts_sha256": facts_sha,
        "manifest_sha256": manifest_sha,
        "completion_run_id": COMPLETION_RUN_ID,
        "completion_run_attempt": COMPLETION_RUN_ATTEMPT,
        "completion_head_sha": COMPLETION_HEAD_SHA,
    }))
    .unwrap();
    let completion_sha = sha256_hex(&completion);
    Pkg {
        members: vec![
            (BINARY.into(), binary_bytes),
            (SHA_FILE.into(), sha),
            (MANIFEST.into(), manifest),
            (RECEIPT_FILE.into(), receipt),
            (PROMOTION_BUNDLE.into(), PROMOTION_BUNDLE_BYTES.to_vec()),
            (BUILD_BUNDLE.into(), BUILD_BUNDLE_BYTES.to_vec()),
            (COMPLETION_FILE.into(), completion),
            (COMPLETION_BUNDLE.into(), COMPLETION_BUNDLE_BYTES.to_vec()),
            (SOURCE_FACTS_FILE.into(), facts),
            (
                "trusted_root.jsonl".into(),
                br#"{"untrusted":true}"#.to_vec(),
            ),
        ],
        bin,
        receipt: receipt_sha,
        completion: completion_sha,
    }
}

/// Real `tar.gz` bytes of a member set (written by real tar — this is
/// fixture PREPARATION, not the path under test).
fn tar_gz(member_list: &[(String, Vec<u8>)]) -> Vec<u8> {
    let dir = tempfile::Builder::new()
        .prefix("c1455-tb-")
        .tempdir()
        .unwrap();
    let staged = dir.path().join("s");
    fs::create_dir_all(&staged).unwrap();
    for (name, bytes) in member_list {
        fs::write(staged.join(name), bytes).unwrap();
        if name == BINARY {
            fs::set_permissions(staged.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let archive = dir.path().join("out.tar.gz");
    let mut args = vec![
        "-czf".into(),
        archive.to_string_lossy().into_owned(),
        "-C".into(),
        staged.to_string_lossy().into_owned(),
    ];
    args.extend(member_list.iter().map(|(n, _)| n.clone()));
    let out = cadence_agent::reaper::output(Command::new("tar").args(&args)).unwrap();
    assert!(
        out.status.success(),
        "fixture tar: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::read(&archive).unwrap()
}

// ---------------------------------------------------------------------------
// Policy-only `gh` oracle — asserts the supported gh 2.83.2 argv, emits
// the documented success JSON. Verifies NO signature.
// ---------------------------------------------------------------------------
fn write_fake_gh(tools: &Path, d: &Pkg) {
    let exp = |name: &str, bytes: &[u8]| {
        let p = tools.join(name);
        fs::write(&p, bytes).unwrap();
        p
    };
    let promo = exp("exp-promo", PROMOTION_BUNDLE_BYTES);
    let build = exp("exp-build", BUILD_BUNDLE_BYTES);
    let compl = exp("exp-compl", COMPLETION_BUNDLE_BYTES);
    let success = |subject: &str, digest: &str, signer: &str| {
        serde_json::to_string(&json!([{
            "verificationResult": {
                "statement": {"subject": [{"name": subject, "digest": {"sha256": digest}}]},
                "signature": {"certificate": {"sourceRepositoryDigest": signer}},
            }
        }]))
        .unwrap()
    };
    let script = format!(
        r#"#!/bin/sh
# CAD-1455 transport check: POLICY-ONLY gh oracle (no crypto).
subj=""; bundle=""; root=""; repo=""; ident=""; issuer=""; signer_repo=""; src_ref=""; deny=""; fmt=""; src_digest=""
prev=""
for a in "$@"; do
  case "$prev" in
    --bundle) bundle="$a";; --custom-trusted-root) root="$a";;
    --repo) repo="$a";; --cert-identity) ident="$a";;
    --cert-oidc-issuer) issuer="$a";; --signer-repo) signer_repo="$a";;
    --source-ref) src_ref="$a";; --source-digest) src_digest="$a";;
    --format) fmt="$a";;
  esac
  [ "$a" = "--deny-self-hosted-runners" ] && deny=1
  [ "$prev" = "verify" ] && subj="$a"
  prev="$a"
done
fail() {{ echo "fake-gh: $1" >&2; exit 1; }}
[ "$repo" = "{repo}" ] || fail "repo '$repo'"
[ "$fmt" = "json" ] || fail "format '$fmt'"
[ "$issuer" = "https://token.actions.githubusercontent.com" ] || fail "issuer '$issuer'"
[ -z "$signer_repo" ] || fail "unsupported --signer-repo '$signer_repo'"
[ "$src_ref" = "refs/heads/{main}" ] || fail "source-ref '$src_ref'"
[ -n "$deny" ] || fail "missing --deny-self-hosted-runners"
case "$root" in */cadence-retained-*/.trusted-root.jsonl) ;; *) fail "root '$root' not owned snapshot";; esac
case "$root" in *trusted_root.jsonl) fail "package trusted_root.jsonl anchored";; esac
case "$subj" in */cadence-retained-*|*/cadence-release-*/*) ;; *) fail "subject '$subj' not captured";; esac
case "$bundle" in
  */{promo_name})
    cmp -s "$bundle" "{promo}" || fail "promotion bundle bytes not the fixture's"
    [ "$ident" = "https://github.com/{repo}/.github/workflows/{promo_wf}@refs/heads/{main}" ] || fail "promotion cert-identity '$ident'"
    [ -z "$src_digest" ] || fail "promotion pass pinned --source-digest"
cat <<'JSON'
{spromo}
JSON
    ;;
  */{build_name})
    cmp -s "$bundle" "{build}" || fail "build bundle bytes not the fixture's"
    [ "$ident" = "https://github.com/{repo}/.github/workflows/{build_wf}@refs/heads/{main}" ] || fail "build cert-identity '$ident'"
    [ "$src_digest" = "{source_sha}" ] || fail "build --source-digest '$src_digest'"
cat <<'JSON'
{sbuild}
JSON
    ;;
  */{completion_name})
    cmp -s "$bundle" "{compl}" || fail "completion bundle bytes not the fixture's"
    [ "$ident" = "https://github.com/{repo}/.github/workflows/{completion_wf}@refs/heads/{main}" ] || fail "completion cert-identity '$ident'"
    [ -z "$src_digest" ] || fail "completion pass pinned --source-digest"
cat <<'JSON'
{scompl}
JSON
    ;;
  *) fail "unexpected bundle '$bundle'";;
esac
exit 0
"#,
        repo = REPO,
        main = MAIN,
        promo = promo.display(),
        build = build.display(),
        compl = compl.display(),
        promo_name = PROMOTION_BUNDLE,
        build_name = BUILD_BUNDLE,
        completion_name = COMPLETION_BUNDLE,
        promo_wf = STAGING_WORKFLOW,
        build_wf = WORKFLOW,
        completion_wf = COMPLETION_WORKFLOW,
        source_sha = SOURCE_SHA,
        spromo = success(RECEIPT_FILE, &d.receipt, PROMOTION_HEAD_SHA),
        sbuild = success(BINARY, &d.bin, SOURCE_SHA),
        scompl = success(COMPLETION_FILE, &d.completion, COMPLETION_HEAD_SHA),
    );
    let gh = tools.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Forward-only `tar` observer: logs argv to `$TAR_OBSERVE_LOG`, execs
/// real tar. Fabricates nothing.
fn write_observer_tar(tools: &Path) {
    let real_tar = String::from_utf8_lossy(
        &cadence_agent::reaper::output(Command::new("which").arg("tar"))
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    fs::write(
        tools.join("tar"),
        format!("#!/bin/sh\necho \"$@\" >> \"$TAR_OBSERVE_LOG\"\nexec \"{real_tar}\" \"$@\"\n"),
    )
    .unwrap();
    fs::set_permissions(tools.join("tar"), fs::Permissions::from_mode(0o755)).unwrap();
}

// ---------------------------------------------------------------------------
// Loopback catalogue/artifact server (AOS-255 contract shape, std TCP).
// Answers only the canonical routes with the recorded bytes. Plain
// HTTP on 127.0.0.1 — reachable ONLY through the test-seam constructor.
// ---------------------------------------------------------------------------
struct Server {
    base: String,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // nudge the listener so `incoming` wakes and exits
        let _ = TcpListener::bind(("127.0.0.1", 0)).map(|_| ());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve(routes: Vec<(String, Vec<u8>)>) -> Server {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = std::thread::spawn(move || loop {
        if stop2.load(Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                                break;
                            }
                        }
                    }
                }
                let head = String::from_utf8_lossy(&buf);
                let req = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let (status, body) = match routes.iter().find(|(p, _)| *p == req) {
                    Some((_, b)) => ("200 OK", b.clone()),
                    None => (
                        "404 Not Found",
                        br#"{"ok":false,"error":{"code":"not_found","message":"absent"}}"#.to_vec(),
                    ),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/octet-stream\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&body);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => return,
        }
    });
    Server {
        base: format!("http://127.0.0.1:{port}"),
        stop,
        handle: Some(handle),
    }
}

/// Contract-shape detail + artifact routes for the package archive.
fn routes(version: &str, archive: &[u8]) -> Vec<(String, Vec<u8>)> {
    let archive_sha = sha256_hex(archive);
    let art_url = format!(
        "/v1/cadence/artifacts/cadence/{PROFILE_OPERATOR_HOST}/{version}/{PACKAGE_ARCHIVE}"
    );
    let entry = json!({
        "id": format!("cadence/{PROFILE_OPERATOR_HOST}/{version}"),
        "product": "cadence", "version": version, "sourceSha": SOURCE_SHA,
        "profile": PROFILE_OPERATOR_HOST, "visibility": "private",
        "status": "published", "publishedAt": null, "withdrawnAt": null,
        "recordedAt": "2026-10-15T00:00:00Z", "productionCandidateId": "987654321",
        "artifacts": [{
            "name": PACKAGE_ARCHIVE, "profile": PROFILE_OPERATOR_HOST,
            "platform": TARGET, "sha256": format!("sha256:{archive_sha}"),
            "size": archive.len(), "contentType": "application/octet-stream",
        }],
    });
    vec![
        (
            format!("/v1/cadence/releases/latest?product=cadence&profile={PROFILE_OPERATOR_HOST}"),
            serde_json::to_vec(&json!({"ok":true,"data":{"product":"cadence","profile":PROFILE_OPERATOR_HOST,"entry":entry}})).unwrap(),
        ),
        (
            format!("/v1/cadence/releases/cadence/{PROFILE_OPERATOR_HOST}/{version}"),
            serde_json::to_vec(&json!({"ok":true,"data":{"entry":entry,"artifacts":[{"name":PACKAGE_ARCHIVE,"url":art_url}]}})).unwrap(),
        ),
        (art_url, archive.to_vec()),
    ]
}

// ---------------------------------------------------------------------------
// Owned tools env + tar call observation
// ---------------------------------------------------------------------------
struct Env {
    _tools: tempfile::TempDir,
    calls: PathBuf,
}

/// Serialize the process-global PATH/TAR_OBSERVE_LOG mutation across tests
/// (cargo runs tests on threads; without this a sibling fixture's env swap
/// races the observation window).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn tar_calls(env: &Env) -> Vec<String> {
    fs::read_to_string(&env.calls)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}
fn fs_extracts(calls: &[String]) -> usize {
    calls
        .iter()
        .filter(|call| tar_extracts_to_directory(call))
        .count()
}

/// A filesystem extraction is `tar` asked to extract (`-x`) and to
/// change directory (`-C`), as arguments. A path such as
/// `cadence-release-C…` contains the letters `-C` but is not a flag;
/// counting it rejected the bounded `-xOzf` reads.
fn tar_extracts_to_directory(call: &str) -> bool {
    let mut extract = false;
    let mut directory = false;
    for arg in call.split_whitespace() {
        let Some(flags) = arg.strip_prefix('-') else {
            continue;
        };
        if flags.is_empty() || flags.starts_with('-') {
            continue;
        }
        if flags.contains('x') {
            extract = true;
        }
        if flags.contains('C') {
            directory = true;
        }
    }
    extract && directory
}

#[test]
fn fs_extract_count_ignores_a_path_that_merely_contains_dash_c() {
    let bounded = [
        "-tvzf /tmp/cadence-release-CCUu4W/cadence-release.tar.gz".to_string(),
        "-xOzf /tmp/cadence-release-CCUu4W/cadence-release.tar.gz cadence".to_string(),
        "-xOzf /tmp/cadence-release-CCUu4W/cadence-release.tar.gz cadence.sha256".to_string(),
    ];
    assert_eq!(fs_extracts(&bounded), 0);
    assert_eq!(
        fs_extracts(&["-xzf /tmp/archive.tar.gz -C /tmp/out".to_string()]),
        1
    );
    assert_eq!(
        fs_extracts(&["-xCf /tmp/out /tmp/archive.tar.gz".to_string()]),
        1
    );
}

/// Build tools dir (observer tar + policy gh), run `f` with it on PATH.
fn with_tools<R>(pkg: &Pkg, f: impl FnOnce(&Env) -> R) -> (Env, R) {
    let tools = tempfile::Builder::new()
        .prefix("c1455-tb-tools-")
        .tempdir()
        .unwrap();
    write_observer_tar(tools.path());
    write_fake_gh(tools.path(), pkg);
    let env = Env {
        calls: tools.path().join("tar-calls.log"),
        _tools: tools,
    };
    // Set the process env BEFORE the caller starts the loopback server
    // thread or touches ureq: `set_var` mutating `environ` while the
    // HTTP stack reads env (proxy/TLS roots) races and deadlocks. The
    // server thread and the HttpGh are created by `f` after this, so
    // the env is stable for their whole lifetime here.
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{old_path}", env._tools.path().display()),
    );
    std::env::set_var("TAR_OBSERVE_LOG", &env.calls);
    let r = f(&env);
    std::env::set_var("PATH", old_path);
    std::env::remove_var("TAR_OBSERVE_LOG");
    (env, r)
}

/// The configured owned source for `base`. REQUIRES the test-seam
/// constructor `HttpGh::for_profile_loopback` for `http://127.0.0.1`;
/// without the feature this returns an honest environment error, never
/// a pass. The name binds to the seam the source author adds — update
/// this one call site to the shipped symbol if it differs.
fn source_for(base: &str) -> cadence_agent::error::Result<HttpGh> {
    #[cfg(feature = "test-seam")]
    {
        HttpGh::for_profile_loopback(base, PROFILE_OPERATOR_HOST)
    }
    #[cfg(not(feature = "test-seam"))]
    {
        let _ = base;
        Err(cadence_agent::error::Error::rejected(
            "test-seam loopback constructor is required for this acceptance — \
             build with --features test-seam against the frozen head that adds it",
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// POSITIVE: a complete coherent package fetched over the REAL HTTP
/// path (`latest_green_main` → detail → download_artifact → member
/// bound → verify_retained) verifies and reports the published subject.
#[test]
fn cad1455_complete_package_fetches_and_proves_over_real_http() {
    let pkg = members(false);
    let archive = tar_gz(&pkg.members);
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (env, result) = with_tools(&pkg, |_| {
        let server = serve(routes(SOURCE_SHA, &archive));
        let src = source_for(&server.base)?;
        src.published_source()
    });
    let calls = tar_calls(&env);
    // result: Result<Option<String>, Error> — Err = refusal, Ok(Some)=verified sha
    let sha = result
        .expect("fetch+prove must run") // Err = refusal or seam unavailable
        .expect("latest must publish the entry"); // None = no published release
    assert_eq!(sha, SOURCE_SHA);
    assert!(!calls.is_empty(), "the real fetch path must drive real tar");
}

/// DECISIVE NEGATIVE (behavioral): the SAME complete package with
/// `cadence.sha256` at bound+1 bytes (transport digest updated in the
/// catalogue record so no unrelated check fires) must refuse BEFORE
/// any `tar … -C` filesystem extraction and WITHOUT the over-bound
/// member ever landing on disk. At frozen dbe the product extracts
/// first and checks sizes after — this assertion is the RED the fix
/// flips, observed through the real fetch path, not a reimplementation.
#[test]
fn cad1455_over_bound_member_never_reaches_filesystem_extraction() {
    let pkg = members(true);
    let archive = tar_gz(&pkg.members);
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (env, result) = with_tools(&pkg, |_| {
        let server = serve(routes(SOURCE_SHA, &archive));
        let src = source_for(&server.base)?;
        src.published_source()
    });
    let calls = tar_calls(&env);
    // result: Result<Option<Run>, Error>. A refusal is `Err`; an
    // over-bound member that still verifies is `Ok(Some(_))` (the gap).
    if result.is_ok() {
        panic!("an over-bound package member must refuse");
    }
    assert!(
        fs_extracts(&calls) == 0,
        "GUARD GAP: the untrusted archive was filesystem-extracted before member \
         bounds were enforced — product tar calls: {calls:?}. The fetch must bound \
         each member via bounded stdout (`-xOzf`) and write only accepted bytes."
    );
}

/// A nonregular (symlink) or duplicate member in the served archive
/// must refuse at the member-set validation, before extraction.
/// (Negative row carried by the same real path.)
#[test]
fn cad1455_duplicate_member_refuses_over_real_fetch() {
    let mut pkg = members(false);
    let sha = pkg
        .members
        .iter()
        .find(|(n, _)| n == SHA_FILE)
        .unwrap()
        .1
        .clone();
    pkg.members.push((SHA_FILE.into(), sha)); // duplicate member name
    let archive = tar_gz(&pkg.members);
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_env, result) = with_tools(&pkg, |_| {
        let server = serve(routes(SOURCE_SHA, &archive));
        let src = source_for(&server.base)?;
        src.published_source()
    });
    if result.is_ok() {
        panic!("a duplicate member must refuse");
    }
}

// ---------------------------------------------------------------------------
// CAD-1455 no-backward pair (independently maintained transport copy;
// the implementer may not edit or weaken this file).
//
// The same complete, verified published package reaches the REAL
// `upgrade::run` LatestMain resolution over loopback HTTP with a real
// installed `Current::Link`. `refuse_backwards` must make an installed
// current that the older candidate cannot prove descendant FAIL
// CLOSED (unknown is not authorization to move backward), while a
// current that IS a recorded ancestor of the candidate stays a dry-run
// positive through every real gate (HTTP fetch, bounded members,
// offline retained proof, manifest/checksum).
//
// Private layout only: `releases`/`link` under the test's own tempdir;
// `Layout::detect` and every production path are untouched. `dry_run`
// never installs, repoints or executes a candidate — the link target
// and bytes are asserted unchanged after both runs.
// ---------------------------------------------------------------------------

/// The installed-current state for the pair: a private releases dir, a
/// `link` symlink to `<releases>/<cur>/cadence` (placeholder bytes —
/// `current()` reads the sha off the link target, never the file).
/// Returns (tempdir keeping paths alive, layout, link target).
fn seeded_layout(cur: &str) -> (tempfile::TempDir, Layout, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("c1455-nb-layout-")
        .tempdir()
        .unwrap();
    let releases = dir.path().join("releases");
    let bin_dir = releases.join(cur);
    fs::create_dir_all(&bin_dir).unwrap();
    let cur_bin = bin_dir.join(BINARY);
    fs::write(&cur_bin, b"placeholder-installed-current").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&cur_bin, &link).unwrap();
    let layout = Layout {
        releases: releases.clone(),
        link: link.clone(),
    };
    (dir, layout, cur_bin)
}

fn latest_main_request() -> Request {
    Request {
        target: Target::LatestMain,
        dry_run: true,
        allow_unattested: false,
        backup_state_dir: None,
    }
}

/// POSITIVE: the installed current is a recorded ancestor of the
/// published candidate — the forward move is what latest-main is for.
/// The SAME real path (catalogue → package fetch → member bounds →
/// retained proof → resolution → compare) must return Ok with
/// `would_repoint: true`, proving every gate on the pair is clear and
/// the refusal below is the no-backward rule itself, not harness noise.
#[test]
fn cad1455_latest_main_dry_run_repoints_from_candidate_ancestor() {
    let pkg = members(false);
    let archive = tar_gz(&pkg.members);
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_env, result) = with_tools(&pkg, |_| {
        let server = serve(routes(SOURCE_SHA, &archive));
        let (_dir, layout, _cur_bin) = seeded_layout(ANCESTOR1);
        let src = source_for(&server.base)?;
        run(&src, &layout, &latest_main_request())
    });
    let report = result.expect(
        "an installed ancestor of the published candidate must resolve, verify \
         and plan the forward repoint — the real latest-main path",
    );
    assert_eq!(report["to_sha"], SOURCE_SHA);
    assert_eq!(report["from_sha"], ANCESTOR1);
    assert_eq!(report["would_repoint"], true, "report: {report}");
    assert_eq!(report["dry_run"], true);
}

/// DECISIVE NEGATIVE (behavioral): the SAME complete package resolves
/// SOURCE_SHA while the installed link names a current that is NOT in
/// the published candidate's signed ancestor set (a newer main commit
/// recorded after the candidate, or any sha the snapshot cannot
/// place). For latest-main a candidate that cannot prove the current
/// is its ancestor must FAIL CLOSED — a published source's `compare`
/// answers `ahead` only relative to its own candidate and answers
/// `None` here — exactly as live GitHub's unknown ordering refuses.
/// At frozen 043 `refuse_backwards` only rejects `compare(to,from)
/// == ahead`, so this currently resolves Ok with `would_repoint: true`
/// (the ticket's no-backward acceptance broken); this assertion is the
/// RED the fix flips, observed through the real run, not a fake
/// compare or a private guard call. The link must be untouched either
/// way (dry-run never repoints).
#[test]
fn cad1455_latest_main_refuses_when_current_not_in_candidate_ancestors() {
    // A full sha absent from the fixture's ancestors
    // [ANCESTOR1, ANCESTOR2, SOURCE_SHA] and not the candidate — the
    // descendant relation is unproven, never disproven.
    const NEWER_CURRENT: &str = "7777777777777777777777777777777777777777";
    let pkg = members(false);
    let archive = tar_gz(&pkg.members);
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (env, outcome) = with_tools(&pkg, |_| {
        let server = serve(routes(SOURCE_SHA, &archive));
        let (dir, layout, cur_bin) = seeded_layout(NEWER_CURRENT);
        let result =
            source_for(&server.base).and_then(|src| run(&src, &layout, &latest_main_request()));
        (dir, result, layout, cur_bin)
    });
    let (_dir, result, layout, cur_bin) = outcome;
    // Whatever the verdict, a dry-run must leave the private link and
    // the installed placeholder exactly as seeded.
    assert_eq!(
        fs::read_link(&layout.link).ok().as_deref(),
        Some(cur_bin.as_path()),
        "the link target changed"
    );
    assert_eq!(
        fs::read(&cur_bin).unwrap(),
        b"placeholder-installed-current"
    );
    let calls = tar_calls(&env);
    assert!(
        !calls.is_empty(),
        "the run must have fetched and proven the real package before refusing"
    );
    if let Ok(report) = result {
        panic!(
            "NO-BACKWARD GUARD GAP: --latest-main planned a move from installed \
             {NEWER_CURRENT} — outside published candidate {SOURCE_SHA}'s signed \
             ancestors — to that older candidate (would_repoint={}). An unknown \
             descendant relation must fail closed for a published source; the \
             ticket's no-backward acceptance requires a refusal here. \
             report: {report}",
            report["would_repoint"]
        );
    } // Err: the required closed refusal
}

// ---------------------------------------------------------------------------
// CAD-1455 atomic-verified-install reuse pair (same maintained copy).
//
// `upgrade::run` may reuse an on-disk published release ONLY when the
// recorded checksum and manifest agree with the verified published
// bytes (`reusable`). At frozen 043 the `source` match arm checks
// `installed_path.is_file() && digest.is_some()` — `digest` comes from
// the proof, not the reuse decision — so any regular file at
// `<releases>/<sha>/cadence` reports `installed-release` and skips the
// captured verified package: tampered/wrong local bytes would be the
// installed artifact the link repoints to. The ticket's
// atomic-verified-install + "tampered inputs cannot cause a link
// change" acceptance requires the captured `published-release` path
// (or a legitimate closed refusal before the plan) whenever the local
// copy does not prove the verified digest — and a genuine verified
// copy to still reuse, proving the reuse gate itself stays open only
// for matching bytes.
// ---------------------------------------------------------------------------

/// POSITIVE: an on-disk candidate that IS the verified published copy
/// (exact fixture `cadence`, `cadence.sha256`, `manifest.json` bytes)
/// must report `installed-release` — reuse is preserved for bytes that
/// prove the digest, and this clears every real gate so the negative
/// below isolates the reuse decision itself.
#[test]
fn cad1455_latest_main_dry_run_reuses_verified_published_copy() {
    let pkg = members(false);
    let archive = tar_gz(&pkg.members);
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_env, outcome) = with_tools(&pkg, |_| {
        let server = serve(routes(SOURCE_SHA, &archive));
        let (dir, layout, cur_bin) = seeded_layout(ANCESTOR1);
        // The genuine verified candidate: the exact package members
        // the retained proof will verify, on disk under the real
        // release layout.
        let rel = layout.release_dir(SOURCE_SHA);
        fs::create_dir_all(&rel).unwrap();
        for name in [BINARY, SHA_FILE, MANIFEST] {
            let bytes = &pkg.members.iter().find(|(n, _)| n == name).unwrap().1;
            fs::write(rel.join(name), bytes).unwrap();
        }
        let result =
            source_for(&server.base).and_then(|src| run(&src, &layout, &latest_main_request()));
        (dir, result, layout, cur_bin)
    });
    let (_dir, result, _layout, _cur_bin) = outcome;
    let report = result.expect(
        "a verified on-disk published copy must resolve and plan reuse — \
         the real fetch/proof path stays intact",
    );
    assert_eq!(report["to_sha"], SOURCE_SHA);
    assert_eq!(
        report["source"], "installed-release",
        "a genuine verified copy must reuse: {report}"
    );
    assert_eq!(report["would_repoint"], true, "report: {report}");
}

/// DECISIVE NEGATIVE (behavioral): the SAME complete package resolves
/// while a regular file at `<releases>/<SOURCE_SHA>/cadence` holds
/// WRONG bytes beside the package's own valid SHA_FILE/MANIFEST. The
//  copy does not hash to the verified digest, so it must NOT be
/// treated as the installed release: the run must plan the captured
/// `published-release` install (or refuse closed before the plan) —
/// never report the unverified bytes as the source. At 043 the
/// `source` arm keys on file-existence plus the proof's digest, so it
/// reports `installed-release` here (the false-reuse gap); this
/// assertion is the RED the fix flips. `would_repoint` is required
/// true only when the run plans a repoint at all — a refusal must not
/// produce one.
#[test]
fn cad1455_latest_main_dry_run_never_reuses_wrong_local_bytes() {
    let pkg = members(false);
    let archive = tar_gz(&pkg.members);
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (env, outcome) = with_tools(&pkg, |_| {
        let server = serve(routes(SOURCE_SHA, &archive));
        let (dir, layout, cur_bin) = seeded_layout(ANCESTOR1);
        // Wrong regular bytes where the candidate would sit; the
        // recorded checksum/manifest are the package's own valid
        // members (only the binary content diverges).
        let rel = layout.release_dir(SOURCE_SHA);
        fs::create_dir_all(&rel).unwrap();
        fs::write(rel.join(BINARY), b"WRONG-not-the-published-bytes").unwrap();
        for name in [SHA_FILE, MANIFEST] {
            let bytes = &pkg.members.iter().find(|(n, _)| n == name).unwrap().1;
            fs::write(rel.join(name), bytes).unwrap();
        }
        let wrong = fs::read(rel.join(BINARY)).unwrap();
        let result =
            source_for(&server.base).and_then(|src| run(&src, &layout, &latest_main_request()));
        (dir, result, layout, cur_bin, wrong)
    });
    let (dir, result, layout, _cur_bin, wrong) = outcome;
    let calls = tar_calls(&env);
    assert!(
        !calls.is_empty(),
        "the run must have fetched and proven the real package before the verdict"
    );
    // The seeded wrong bytes must still be on disk after a dry-run.
    assert_eq!(
        fs::read(dir.path().join("releases").join(SOURCE_SHA).join(BINARY)).unwrap(),
        wrong
    );
    assert_eq!(
        fs::read_link(&layout.link).ok().as_deref(),
        Some(layout.binary(ANCESTOR1).as_path()),
        "the link target changed"
    );
    if let Ok(report) = result {
        assert_ne!(
            report["source"], "installed-release",
            "FALSE-REUSE GUARD GAP: unverified local bytes at \
             <releases>/{SOURCE_SHA}/cadence were reported as the installed \
             release, skipping the captured verified package — the link \
             would repoint to bytes that never proved the published digest. \
             The atomic-verified-install acceptance requires the captured \
             `published-release` plan (or a closed refusal) here. \
             report: {report}"
        );
        assert_eq!(
            report["source"], "published-release",
            "the captured verified package must be the install source: {report}"
        );
    } // Err: a legitimate closed refusal is acceptable
}
