//! CAD-1455 INDEPENDENT public-client publication-authority acceptance.
//!
//! Independent QA module (NOT the implementer; the implementer may not
//! edit or weaken this file). Wired as a `#[cfg(all(test, feature =
//! "test-seam"))]` unit module inside the `cadence` bin crate so it can
//! reach the sanctioned seam `super::fetch_verified_loopback` — the
//! REAL `fetch_verified_with` driven against a loopback `HttpGh`. No
//! production validator is altered; the seam is the only injection.
//!
//! Under test: `client_update::fetch_verified_with` at frozen head
//! `23c6a1f8d72713f024667dd2fca594747b2f7fe7`. A `ci.yml` build
//! attestation is minted BEFORE `Publish the GitHub Release`, so build
//! proof alone must never authorize the install: the owned route must
//! additionally require an AUTHENTICATED post-publication receipt
//! (`publication-receipt.json` + `publication-attestation.jsonl`) bound
//! to repo, tag, source SHA and the exact per-target archive digests.
//!
//! OBSERVABLE GUARD: the candidate `cadence` member is a benign
//! labelled fixture SCRIPT that appends to `$CANDIDATE_EXECUTED` on
//! every run and prints the expected version/help marker. A missing or
//! unproven receipt must leave that marker ABSENT — the refusal lands
//! before `candidate --version`, exactly where the receipt gate sits.
//!
//! STAGE-CORRECT `gh` ORACLE (policy only, no crypto): the shared
//! `OfflineGh::verify_bundle` runs `gh attestation verify` TWICE on the
//! tarball — first with `--source-digest` ABSENT (manifest not yet
//! read), then pinned to the manifest source — and once on the receipt
//! with `--source-digest` pinned. The oracle asserts the exact argv a
//! real `gh 2.83.2` accepts (repo, `--cert-identity`
//! `<repo>/.github/workflows/ci.yml@refs/tags/<tag>`, issuer, `--source-
//! ref`, `--deny-self-hosted-runners`, `--format json`, `--custom-
//! trusted-root` the verifier-owned snapshot) and emits the documented
//! `--format json` success shape. It requires `--source-digest` ONLY on
//! the later passes — never on the initial pre-manifest pass — so the
//! receipt gap is not masked by a wrong early requirement.
//!
//! LIMITS: argv policy only — no Sigstore crypto, no real GitHub
//! release, no real R2, no real cadence binary and no link switch. This
//! is the HTTP→pre-execution authority boundary.
#![cfg(unix)]

use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

const REPO: &str = "favcrm/cadence";
const SOURCE_SHA: &str = "1111111111111111111111111111111111111111";
const TAG: &str = "v0.2.0";
const TARGET_TRIPLE: &str = "x86_64-linux";
const RUNNER: &str = "ubuntu-22.04";
const ASSET: &str = "cadence-v0.2.0-x86_64-linux.tar.gz";
const PROFILE_PUBLIC_CLIENT: &str = "public-client";
// The AOS-255 release namespace's product segment — a release id, not a
// worktree branch: `cadence/<profile>/<tag>` on the owned source.
const PRODUCT: &str = "cadence";

/// Member names the consumer requires (bind the shipped constants).
const PUB_RECEIPT_NAME: &str = cadence_agent::upgrade::PUBLICATION_RECEIPT; // publication-receipt.json
const PUB_RECEIPT_BUNDLE_NAME: &str = cadence_agent::upgrade::PUBLICATION_BUNDLE; // publication-attestation.jsonl

const BUILD_BUNDLE_BYTES: &[u8] = br#"{"bundleRole":"build","signer":"ci.yml"}"#;
const PUB_BUNDLE_BYTES: &[u8] = br#"{"bundleRole":"publication-receipt","signer":"ci.yml"}"#;

// Serialize env mutation across tests.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn sha256_hex(b: &[u8]) -> String {
    format!("{:x}", Sha256::new().chain_update(b).finalize())
}

/// Benign labelled stand-in for the release's `cadence` member. Logs
/// every invocation to $CANDIDATE_EXECUTED, prints the version and the
/// client-only update marker the real checks require. NEVER real cadence.
fn candidate_script() -> String {
    format!(
        "#!/bin/sh\necho \"$@\" >> \"$CANDIDATE_EXECUTED\"\ncase \"$1\" in\n  --version) echo \"cadence 0.2.0+{SOURCE_SHA}\";;\n  update) echo \"Upgrade a client-only installation\";;\nesac\nexit 0\n"
    )
}

fn manifest_bytes(bin_digest: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1, "version": TAG, "source_sha": SOURCE_SHA,
        "target": TARGET_TRIPLE, "runner": RUNNER, "sha256": bin_digest,
        "features": ["ui"], "checks": ["fmt","clippy","test","build","ui"],
        "run_id": 123456789u64, "run_attempt": 1,
        "rustc": "r", "cargo": "c", "built_at": "2026-10-15T00:00:00Z",
    }))
    .unwrap()
}

/// The post-publication receipt the consumer binds: repo/tag/source and
/// the per-ASSET-name archive digests (BTreeMap key = asset name).
fn publication_receipt(archive_digest: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1, "repo": REPO, "tag": TAG,
        "source_sha": SOURCE_SHA,
        "archives": { ASSET: format!("sha256:{archive_digest}") },
        "published_at": "2026-10-15T00:00:00Z",
    }))
    .unwrap()
}

/// Real tar.gz of the exact member set [cadence, cadence.sha256, manifest.json].
fn release_archive() -> (Vec<u8>, String) {
    let dir = tempfile::Builder::new()
        .prefix("c1455-pc-")
        .tempdir()
        .unwrap();
    let staged = dir.path().join("s");
    fs::create_dir_all(&staged).unwrap();
    let cand = staged.join("cadence");
    fs::write(&cand, candidate_script()).unwrap();
    fs::set_permissions(&cand, fs::Permissions::from_mode(0o755)).unwrap();
    let bin_digest = sha256_hex(&fs::read(&cand).unwrap());
    fs::write(
        staged.join("cadence.sha256"),
        format!("{bin_digest}  cadence\n"),
    )
    .unwrap();
    fs::write(staged.join("manifest.json"), manifest_bytes(&bin_digest)).unwrap();
    let archive = dir.path().join(ASSET);
    let out = cadence_agent::reaper::output(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staged)
            .args(["cadence", "cadence.sha256", "manifest.json"]),
    )
    .unwrap();
    assert!(out.status.success());
    let bytes = fs::read(&archive).unwrap();
    let sha = sha256_hex(&bytes);
    (bytes, sha)
}

// --- policy-only gh oracle ---------------------------------------------------
// Asserts the real OfflineGh::verify_bundle argv; emits gh --format json
// success. Stage-correct: --source-digest may be absent or equal
// SOURCE_SHA; a wrong value refuses.
fn write_gh(tools: &Path, archive_sha: &str, receipt_sha: &str) {
    let exp_build = tools.join("exp-build.jsonl");
    fs::write(&exp_build, BUILD_BUNDLE_BYTES).unwrap();
    let exp_rcpt = tools.join("exp-receipt.jsonl");
    fs::write(&exp_rcpt, PUB_BUNDLE_BYTES).unwrap();
    let tag_ref = format!("refs/tags/{TAG}");
    let sbuild = gh_success(ASSET, archive_sha, SOURCE_SHA);
    let srcpt = gh_success(PUB_RECEIPT_NAME, receipt_sha, SOURCE_SHA);
    let script = format!(
        r#"#!/bin/sh
# CAD-1455 public-client oracle: POLICY-ONLY (no crypto).
subj=""; bundle=""; repo=""; ident=""; issuer=""; src_ref=""; deny=""; fmt=""; src_digest=""; swf=""
prev=""
for a in "$@"; do
  case "$prev" in
    --bundle) bundle="$a";; --repo) repo="$a";; --cert-identity) ident="$a";;
    --cert-oidc-issuer) issuer="$a";; --source-ref) src_ref="$a";;
    --source-digest) src_digest="$a";; --format) fmt="$a";; --signer-workflow) swf="$a";;
    verify) subj="$a";;
  esac
  [ "$a" = "--deny-self-hosted-runners" ] && deny=1
  prev="$a"
done
fail() {{ echo "fake-gh: $1" >&2; exit 1; }}
echo "verify subject=$subj digest=${{src_digest:-NONE}} bundle=$(basename "$bundle")" >> "$GH_VERIFY_LOG"
[ "$repo" = "{repo}" ] || fail "repo '$repo'"
[ "$fmt" = "json" ] || fail "format '$fmt'"
[ "$issuer" = "https://token.actions.githubusercontent.com" ] || fail "issuer '$issuer'"
[ "$src_ref" = "{tag_ref}" ] || fail "source-ref '$src_ref'"
[ -n "$deny" ] || fail "missing --deny-self-hosted-runners"
[ "$ident" = "https://github.com/{repo}/.github/workflows/{wf}@{tag_ref}" ] || fail "cert-identity '$ident'"
# stage-correct: absent or equal source sha only
if [ -n "$src_digest" ] && [ "$src_digest" != "{source_sha}" ]; then fail "--source-digest '$src_digest'"; fi
case "$(basename "$bundle")" in
  exp-build.jsonl|build-attestation-*.jsonl)
    cmp -s "$bundle" "{exp_build}" || fail "build bundle bytes differ"
cat <<'JSON'
{sbuild}
JSON
    ;;
  exp-receipt.jsonl|{rcpt_name})
    cmp -s "$bundle" "{exp_rcpt}" || fail "receipt bundle bytes differ"
cat <<'JSON'
{srcpt}
JSON
    ;;
  *) fail "unexpected bundle '$bundle'";;
esac
exit 0
"#,
        repo = REPO,
        wf = cadence_agent::upgrade::WORKFLOW,
        tag_ref = tag_ref,
        exp_build = exp_build.display(),
        exp_rcpt = exp_rcpt.display(),
        source_sha = SOURCE_SHA,
        rcpt_name = PUB_RECEIPT_BUNDLE_NAME,
        sbuild = sbuild,
        srcpt = srcpt,
    );
    let gh = tools.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
}

fn gh_success(subject: &str, digest: &str, signer: &str) -> String {
    serde_json::to_string(&json!([{
        "verificationResult": {
            "statement": {"subject": [{"name": subject, "digest": {"sha256": digest}}]},
            "signature": {"certificate": {"sourceRepositoryDigest": signer}},
        }
    }]))
    .unwrap()
}

// --- loopback catalogue server ------------------------------------------------
struct Server {
    base: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
fn serve(routes: Vec<(String, Vec<u8>)>) -> Server {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = std::thread::spawn(move || loop {
        if stop2.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((mut s, _)) => {
                let mut buf = Vec::new();
                let mut c = [0u8; 8192];
                loop {
                    match s.read(&mut c) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&c[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                                break;
                            }
                        }
                    }
                }
                let req = String::from_utf8_lossy(&buf)
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let (st, b) = match routes.iter().find(|(p, _)| *p == req) {
                    Some((_, b)) => ("200 OK", b.clone()),
                    None => (
                        "404 Not Found",
                        br#"{"ok":false,"error":{"code":"not_found","message":"absent"}}"#.to_vec(),
                    ),
                };
                let r = format!(
                    "HTTP/1.1 {st}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    b.len()
                );
                let _ = s.write_all(r.as_bytes());
                let _ = s.write_all(&b);
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

/// Detail + artifact routes for the public-client tag. `with_receipt`
/// controls whether the published entry lists/carries the receipt pair.
fn client_routes(with_receipt: bool) -> (Vec<(String, Vec<u8>)>, String, String) {
    let (archive, archive_sha) = release_archive();
    let sha_name = format!("{ASSET}.sha256");
    let sha_bytes = format!("{archive_sha}  {ASSET}\n").into_bytes();
    let sha_sha = sha256_hex(&sha_bytes);
    let bundle_name = format!("build-attestation-{TARGET_TRIPLE}.jsonl");
    let receipt = publication_receipt(&archive_sha);
    let receipt_sha = sha256_hex(&receipt);
    let art = |n: &str| format!("/v1/cadence/artifacts/cadence/{PROFILE_PUBLIC_CLIENT}/{TAG}/{n}");

    let mut meta = vec![
        json!({"name":ASSET,"profile":PROFILE_PUBLIC_CLIENT,"platform":TARGET_TRIPLE,"sha256":format!("sha256:{archive_sha}"),"size":archive.len(),"contentType":"application/octet-stream"}),
        json!({"name":sha_name,"profile":PROFILE_PUBLIC_CLIENT,"platform":TARGET_TRIPLE,"sha256":format!("sha256:{sha_sha}"),"size":sha_bytes.len(),"contentType":"text/plain"}),
        json!({"name":bundle_name,"profile":PROFILE_PUBLIC_CLIENT,"platform":TARGET_TRIPLE,"sha256":format!("sha256:{}",sha256_hex(BUILD_BUNDLE_BYTES)),"size":BUILD_BUNDLE_BYTES.len(),"contentType":"application/json"}),
    ];
    let mut urls = vec![
        json!({"name":ASSET,"url":art(ASSET)}),
        json!({"name":sha_name,"url":art(&sha_name)}),
        json!({"name":bundle_name,"url":art(&bundle_name)}),
    ];
    let mut objs = vec![
        (art(ASSET), archive.clone()),
        (art(&sha_name), sha_bytes),
        (art(&bundle_name), BUILD_BUNDLE_BYTES.to_vec()),
    ];
    if with_receipt {
        meta.push(json!({"name":PUB_RECEIPT_NAME,"profile":PROFILE_PUBLIC_CLIENT,"platform":"publication","sha256":format!("sha256:{receipt_sha}"),"size":receipt.len(),"contentType":"application/json"}));
        meta.push(json!({"name":PUB_RECEIPT_BUNDLE_NAME,"profile":PROFILE_PUBLIC_CLIENT,"platform":"publication","sha256":format!("sha256:{}",sha256_hex(PUB_BUNDLE_BYTES)),"size":PUB_BUNDLE_BYTES.len(),"contentType":"application/json"}));
        urls.push(json!({"name":PUB_RECEIPT_NAME,"url":art(PUB_RECEIPT_NAME)}));
        urls.push(json!({"name":PUB_RECEIPT_BUNDLE_NAME,"url":art(PUB_RECEIPT_BUNDLE_NAME)}));
        objs.push((art(PUB_RECEIPT_NAME), receipt));
        objs.push((art(PUB_RECEIPT_BUNDLE_NAME), PUB_BUNDLE_BYTES.to_vec()));
    }
    let entry = json!({
        "id":format!("{PRODUCT}/{PROFILE_PUBLIC_CLIENT}/{TAG}"),"product":PRODUCT,"version":TAG,
        "sourceSha":SOURCE_SHA,"profile":PROFILE_PUBLIC_CLIENT,"visibility":"public","status":"published",
        "publishedAt":"2026-10-15T00:00:00Z","withdrawnAt":null,"recordedAt":"2026-10-15T00:00:00Z",
        "productionCandidateId":null,"artifacts":meta,
    });
    let mut routes = vec![
        (format!("/v1/cadence/releases/cadence/{PROFILE_PUBLIC_CLIENT}/{TAG}"),
         serde_json::to_vec(&json!({"ok":true,"data":{"entry":entry,"artifacts":urls}})).unwrap()),
        (format!("/v1/cadence/releases/latest?product=cadence&profile={PROFILE_PUBLIC_CLIENT}"),
         serde_json::to_vec(&json!({"ok":true,"data":{"product":PRODUCT,"profile":PROFILE_PUBLIC_CLIENT,"entry":entry}})).unwrap()),
    ];
    routes.extend(objs);
    (routes, archive_sha, receipt_sha)
}

struct Env {
    _tools: tempfile::TempDir,
    executed: PathBuf,
    verify_log: PathBuf,
    _t: tempfile::TempDir,
}

/// Tools dir (gh oracle) + a private HOME/TMPDIR; sets PATH and marker
/// env BEFORE any HTTP/ureq work to avoid environ mutation races.
fn make_env(archive_sha: &str, receipt_sha: &str) -> Env {
    let tools = tempfile::Builder::new()
        .prefix("c1455-pc-tools-")
        .tempdir()
        .unwrap();
    write_gh(tools.path(), archive_sha, receipt_sha);
    let t = tempfile::Builder::new()
        .prefix("c1455-pc-home-")
        .tempdir()
        .unwrap();
    let executed = tools.path().join("executed.log");
    let verify_log = tools.path().join("verify.log");
    let real_tar = String::from_utf8_lossy(
        &cadence_agent::reaper::output(Command::new("which").arg("tar"))
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    let real_dir = Path::new(&real_tar).parent().unwrap().to_path_buf();
    std::env::set_var(
        "PATH",
        format!("{}:{}", tools.path().display(), real_dir.display()),
    );
    std::env::set_var("CANDIDATE_EXECUTED", &executed);
    std::env::set_var("GH_VERIFY_LOG", &verify_log);
    Env {
        executed,
        verify_log,
        _tools: tools,
        _t: t,
    }
}

fn candidate_ran(e: &Env) -> bool {
    e.executed.exists()
}
fn verify_asks(e: &Env) -> Vec<String> {
    fs::read_to_string(&e.verify_log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// POSITIVE: complete published client release — archive, checksum,
/// manifest, ci.yml build bundle AND the authenticated post-publication
/// receipt — authorizes and the candidate runs (marker present).
#[test]
fn cad1455_client_release_with_publication_receipt_authorizes() {
    let (routes, archive_sha, receipt_sha) = client_routes(true);
    let _guard = ENV_LOCK.lock().unwrap();
    let env = make_env(&archive_sha, &receipt_sha);
    let server = serve(routes);
    let r = super::fetch_verified_loopback(TAG, TARGET_TRIPLE, RUNNER, &server.base);
    let asks = verify_asks(&env);
    assert!(
        asks.iter()
            .any(|s| s.contains(PUB_RECEIPT_NAME) || s.contains("exp-receipt")),
        "the consumer never asked gh to verify the post-publication receipt: {asks:?}"
    );
    assert!(
        r.is_ok(),
        "the complete published release must verify: {r:?}"
    );
    assert!(
        candidate_ran(&env),
        "the verified release's candidate must have run"
    );
}

/// DECISIVE NEGATIVE: the SAME complete published release minus ONLY the
/// post-publication receipt (archive/checksum/manifest/build-bundle and
/// the public `published` catalogue entry all coherent). Build proof
/// alone must NOT authorize: the candidate marker stays ABSENT.
#[test]
fn cad1455_missing_publication_receipt_never_authorizes_client() {
    let (routes, archive_sha, receipt_sha) = client_routes(false);
    let _guard = ENV_LOCK.lock().unwrap();
    let env = make_env(&archive_sha, &receipt_sha);
    let server = serve(routes);
    let r = super::fetch_verified_loopback(TAG, TARGET_TRIPLE, RUNNER, &server.base);
    assert!(
        !candidate_ran(&env),
        "GUARD GAP: a public-client release with NO post-publication receipt reached \
         candidate execution — its ci.yml build bundle alone authorized install. \
         gh asks: {:?}; result: {:?}",
        verify_asks(&env),
        r
    );
    assert!(r.is_err(), "a receipt-less published release must refuse");
}
