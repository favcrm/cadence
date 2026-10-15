//! CAD-1455 INDEPENDENT acceptance (authored independently of the
//! implementer; the implementer may not edit or weaken this file).
//!
//! Ticket requirement being proved: an already-published operator-host
//! installation may avoid the GitHub API, but it must still carry
//! authenticated proof that the COMPLETED `workflow_dispatch` promotion
//! on `main` succeeded (run id, attempt and gates). A valid build
//! attestation plus a signed MID-RUN promotion receipt alone cannot
//! authorize install — the receipt is attested while the run it names
//! is still in progress, so the promotion may later fail.
//!
//! Frozen under test: `67f9f957947887bec9ea84e80d90485d13da8756`,
//! `OfflineGh::verify_retained -> Result<RetainedProof>`.
//!
//! Package shape the real verifier demands (frozen source): binary +
//! `cadence.sha256` + `manifest.json` + the closed-schema
//! `production-candidate.json` receipt, promotion and build Sigstore
//! bundles, the closed-schema `promotion-completion.json` completion
//! receipt + its bundle, and the closed-schema `source-facts.json` —
//! all captured into the verifier's owned `cadence-retained-*`
//! workspace before any read, so the caller's mutable paths are never
//! consulted again.
//!
//! Tests:
//!   - POSITIVE: a complete coherent package — three distinct run
//!     heads (build/source, promotion signer, completion signer),
//!     completion receipt binding the exact receipt/manifest/facts
//!     digests and run ids, signed source facts naming the same
//!     candidate — must return `Ok(RetainedProof)` whose captured
//!     workspace is still alive and holds the verified bytes.
//!   - NEGATIVE: the SAME otherwise-COMPLETE package minus ONLY the
//!     completion receipt + bundle (facts and manifest stay) must
//!     refuse, no matter how far into verification the check happens —
//!     missing authenticated final-success proof is the sole deficit.
//!   - FORGED promotion bundle: otherwise complete package, wrong
//!     bundle bytes — refused at the crypto step, so the missing-
//!     finality guard is never the thing hiding a forged bundle.
//!   - MISMATCHED bound digests: completion receipt binding different
//!     source-facts/manifest bytes — refused at the real binding check.
//!
//! FAKE GH — POLICY/GUARD EVIDENCE ONLY, NOT CRYPTO OR PRODUCER
//! QUALIFICATION. A bounded local fake `gh` stands in for an
//! already-successful cryptographic verifier: it asserts the exact
//! offline argv that REAL gh 2.83.2 SUPPORTS (subject inside
//! `cadence-retained-*`, `--bundle` the handed path's bytes via `cmp`
//! against fixture bytes, `--custom-trusted-root` the verifier's own
//! `.trusted-root.jsonl` snapshot inside that workspace, `--repo`,
//! `--cert-oidc-issuer`, exact `--cert-identity`/`--source-ref`/
//! `--source-digest` policy per role, `--deny-self-hosted-runners`,
//! `--format json`), then emits the documented gh 2.83.2
//! `gh attestation verify --format json` success shape —
//! `verificationResult.statement.subject[].name/.digest.sha256` and
//! `verificationResult.signature.certificate.sourceRepositoryDigest`
//! — for the handed subject. It implements NO signature, finality,
//! run-status or source-ancestry check; it is never crypto evidence.
//!
//! SUPPORTED-ARGV VARIANT (independent copy; the original driver at
//! `cad1455_retained_release_acceptance.rs`, sha 8dd10718…76f, is
//! preserved unchanged). Real gh 2.83.2 was probed against a genuine
//! main-CI bundle+root and proved two things that shape this oracle:
//!   * `--signer-repo` CANNOT combine with `--cert-identity`: gh 2.83.2
//!     rejects the flag group
//!     `[cert-identity cert-identity-regex signer-repo signer-workflow]`
//!     with exit 1 before any crypto ("if any flags in the group …
//!     were all set"). An oracle that REQUIRED `--signer-repo` along-
//!     side `--cert-identity` would demand an invocation real gh
//!     refuses — that masks a CLI-contract gap, not a guard.
//!   * `--repo <owner>/<repo>` is NOT discovery-only: it enforces the
//!     certificate's `sourceRepositoryURI` (and owner URI) — the same
//!     genuine bundle under `--repo cli/cli` refused with
//!     "expected SourceRepositoryOwnerURI to be https://github.com/cli,
//!     got https://github.com/favcrm", and under `--repo favcrm/other`
//!     with "expected SourceRepositoryURI to be …/favcrm/other, got
//!     …/favcrm/cadence". Source-repo binding therefore already comes
//!     from `--repo`; the exact signer WORKFLOW is pinned by the full
//!     `--cert-identity` SAN (`<repo>/.github/workflows/<wf>@refs/heads/main`),
//!     which is strictly stronger than `--signer-repo` (a repo-only
//!     check). So this oracle requires `--repo` + per-role
//!     `--cert-identity` and REFUSES a presented `--signer-repo`,
//!     matching what gh 2.83.2 would do with `--cert-identity` set.
//!     The verified certificate's real fields (genuine capture):
//!     `sourceRepositoryURI` = https://github.com/favcrm/cadence,
//!     `sourceRepositoryOwnerURI` = https://github.com/favcrm — the values
//!     `--repo favcrm/cadence` enforces.
//!
//! Boundary covered: `OfflineGh::verify_retained` (the retained
//! trust-gate entry). The missing/forged cases exercise the real
//! helper; the HTTP/install/link transport boundary remains pending
//! elsewhere. The refusing paths also snapshot the caller's package
//! dir and require no mutation. State the non-execution plainly: the
//! fixture binary is hashed and handed to gh as a subject; nothing in
//! verify_retained executes it here, and the captured copy is what
//! stays alive — not the source paths.

use cadence_agent::error::Error;
use cadence_agent::upgrade::{
    artifact_name, OfflineGh, ReleaseSource, BINARY, BUILD_BUNDLE, COMPLETION_BUNDLE,
    COMPLETION_FILE, COMPLETION_WORKFLOW, MAIN, MANIFEST, PROMOTION_BUNDLE, RECEIPT_FILE, SHA_FILE,
    SOURCE_FACTS_FILE, STAGING_WORKFLOW, TARGET, WORKFLOW,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const REPO: &str = "favcrm/cadence";
/// `source_sha`: the commit the CI build produced the binary from.
/// Full lowercase hex; synthetic but shape-exact.
const SOURCE_SHA: &str = "1111111111111111111111111111111111111111";
/// The `staging.yml` promotion run's head — distinct from the source
/// and completion heads (the promotion job runs at its own commit).
const PROMOTION_HEAD_SHA: &str = "2222222222222222222222222222222222222222";
/// The `promotion-completion.yml` run's own head — a third distinct
/// commit; the certificate's source-repository digest is authoritative.
const COMPLETION_HEAD_SHA: &str = "3333333333333333333333333333333333333333";
/// Main head the producer observed when it proved ancestry.
const OBSERVED_MAIN_HEAD: &str = "4444444444444444444444444444444444444444";
/// A bounded deduplicated ancestor list ending at the candidate.
const ANCESTOR1: &str = "5555555555555555555555555555555555555555";
const ANCESTOR2: &str = "6666666666666666666666666666666666666666";

const PROMOTION_RUN_ID: u64 = 987_654_321;
const PROMOTION_RUN_ATTEMPT: u64 = 2;
const COMPLETION_RUN_ID: u64 = 888_777_666;
const COMPLETION_RUN_ATTEMPT: u64 = 1;
const CI_RUN_ID: u64 = 123_456_789;
const CI_RUN_ATTEMPT: u64 = 1;

/// SCHEMA_VERSION in `src/rollout.rs` at SOURCE_SHA — the closed
/// source-facts schema requires it; the fixture records the value a
/// producer would have parsed at publication time.
const SCHEMA_VERSION: i64 = 46;

/// Opaque stand-in bundle bytes. The verifier never reads them (gh
/// consumes the bundle); the oracle demands THESE exact bytes so a
/// swapped or forged bundle fails closed. Markers keep roles distinct.
const PROMOTION_BUNDLE_BYTES: &[u8] = br#"{"bundleRole":"promotion","signer":"staging.yml"}"#;
const BUILD_BUNDLE_BYTES: &[u8] = br#"{"bundleRole":"build","signer":"ci.yml"}"#;
const COMPLETION_BUNDLE_BYTES: &[u8] =
    br#"{"bundleRole":"completion","signer":"promotion-completion.yml"}"#;

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::new().chain_update(bytes).finalize())
}

/// One `gh attestation verify --format json` success entry for
/// `subject`/`digest`, certified at `signer_sha`. This is the
/// documented authentic-output shape the verifier reads:
/// `verificationResult.statement.subject` +
/// `signature.certificate.sourceRepositoryDigest`.
fn verify_success_json(subject: &str, digest: &str, signer_sha: &str) -> String {
    serde_json::to_string(&json!([
        {
            "verificationResult": {
                "statement": {
                    "subject": [{"name": subject, "digest": {"sha256": digest}}]
                },
                "signature": {
                    "certificate": {
                        "sourceRepositoryDigest": signer_sha
                    }
                }
            }
        }
    ]))
    .unwrap()
}

/// The closed-schema source-facts file the completion receipt binds.
/// Ancestors are deduplicated, all full SHAs, ending at the candidate;
/// schema_version mirrors `src/rollout.rs` at SOURCE_SHA.
fn source_facts_bytes() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1,
        "source_sha": SOURCE_SHA,
        "observed_main_head": OBSERVED_MAIN_HEAD,
        "ancestors": [ANCESTOR1, ANCESTOR2, SOURCE_SHA],
        "schema_version": SCHEMA_VERSION,
    }))
    .unwrap()
}

fn manifest_bytes(bin_digest: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1,
        "source_sha": SOURCE_SHA,
        "sha256": bin_digest,
        "target": TARGET,
    }))
    .unwrap()
}

fn receipt_bytes(bin_digest: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1,
        "source_sha": SOURCE_SHA,
        "ci_run_id": CI_RUN_ID,
        "ci_run_attempt": CI_RUN_ATTEMPT,
        "sha256": bin_digest,
        "promotion_run_id": PROMOTION_RUN_ID,
        "promotion_run_attempt": PROMOTION_RUN_ATTEMPT,
    }))
    .unwrap()
}

/// The closed-schema completion receipt, binding the proven promotion
/// run + head, the exact promoted receipt/manifest/facts digests, the
/// promoted source and binary digests, and this run's own id + head.
fn completion_bytes(
    receipt_sha: &str,
    bin_digest: &str,
    facts_sha: &str,
    manifest_sha: &str,
) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1,
        "promotion_run_id": PROMOTION_RUN_ID,
        "promotion_run_attempt": PROMOTION_RUN_ATTEMPT,
        "promotion_head_sha": PROMOTION_HEAD_SHA,
        "receipt_sha256": receipt_sha,
        "source_sha": SOURCE_SHA,
        "ci_run_id": CI_RUN_ID,
        "ci_run_attempt": CI_RUN_ATTEMPT,
        "binary_sha256": bin_digest,
        "source_facts_sha256": facts_sha,
        "manifest_sha256": manifest_sha,
        "completion_run_id": COMPLETION_RUN_ID,
        "completion_run_attempt": COMPLETION_RUN_ATTEMPT,
        "completion_head_sha": COMPLETION_HEAD_SHA,
    }))
    .unwrap()
}

/// Write a complete coherent package in `pkg`: binary, checksum,
/// manifest, closed receipt, three bundles, closed completion receipt,
/// closed source facts, plus the untrusted `trusted_root.jsonl`
/// transport copy. Returns the digests the oracle is built over.
struct Digests {
    bin: String,
    receipt: String,
    completion: String,
}

fn write_package(pkg: &Path) -> Digests {
    // Stand-in binary bytes: hashed and handed to gh as a subject,
    // never executed inside verify_retained.
    let binary_bytes = b"cadence-candidate-binary-bytes".to_vec();
    let bin = sha256_hex(&binary_bytes);
    fs::write(pkg.join(BINARY), &binary_bytes).unwrap();
    fs::set_permissions(pkg.join(BINARY), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        pkg.join(SHA_FILE),
        format!("{bin}  {}\n", artifact_name(SOURCE_SHA)),
    )
    .unwrap();

    let manifest_bytes = manifest_bytes(&bin);
    fs::write(pkg.join(MANIFEST), &manifest_bytes).unwrap();
    let manifest = sha256_hex(&manifest_bytes);

    let receipt_bytes = receipt_bytes(&bin);
    fs::write(pkg.join(RECEIPT_FILE), &receipt_bytes).unwrap();
    let receipt = sha256_hex(&receipt_bytes);

    let facts_bytes = source_facts_bytes();
    fs::write(pkg.join(SOURCE_FACTS_FILE), &facts_bytes).unwrap();
    let facts = sha256_hex(&facts_bytes);

    let completion_bytes = completion_bytes(&receipt, &bin, &facts, &manifest);
    fs::write(pkg.join(COMPLETION_FILE), &completion_bytes).unwrap();
    let completion = sha256_hex(&completion_bytes);

    fs::write(pkg.join(PROMOTION_BUNDLE), PROMOTION_BUNDLE_BYTES).unwrap();
    fs::write(pkg.join(BUILD_BUNDLE), BUILD_BUNDLE_BYTES).unwrap();
    fs::write(pkg.join(COMPLETION_BUNDLE), COMPLETION_BUNDLE_BYTES).unwrap();

    // The untrusted transport copy every package carries. The oracle
    // separately asserts --custom-trusted-root names the verifier's
    // own captured `.trusted-root.jsonl`, never this file.
    fs::write(pkg.join("trusted_root.jsonl"), br#"{"untrusted":true}"#).unwrap();

    Digests {
        bin,
        receipt,
        completion,
    }
}

/// Write the fake `gh` oracle into `tools/gh`. POLICY/GUARD ORACLE
/// ONLY: no cryptography, no finality, no install policy. It fails
/// closed on any argv that is not the exact offline policy, on a
/// subject outside the verifier's owned capture workspace, on a
/// trusted-root path that is not the verifier's owned embedded
/// snapshot, or on bundle bytes that are not the fixture's — so argv
/// drift or forged material cannot pass silently. On exact match it
/// prints the authentic success JSON for the handed subject.
fn write_fake_gh(tools: &Path, d: &Digests) -> PathBuf {
    // Expected bundle bytes as files so the oracle can `cmp` the
    // handed bundle against them — content, not pathname.
    let expected_promotion = tools.join("expected-promotion-bundle");
    let expected_build = tools.join("expected-build-bundle");
    let expected_completion = tools.join("expected-completion-bundle");
    fs::write(&expected_promotion, PROMOTION_BUNDLE_BYTES).unwrap();
    fs::write(&expected_build, BUILD_BUNDLE_BYTES).unwrap();
    fs::write(&expected_completion, COMPLETION_BUNDLE_BYTES).unwrap();

    let gh = tools.join("gh");
    let script = format!(
        r#"#!/bin/sh
# CAD-1455 acceptance POLICY ORACLE -- not a signature verifier, no
# finality or source checks. Asserts the exact offline argv that real
# gh 2.83.2 supports and the fixture bundle bytes, then emits the
# documented gh 2.83.2 `gh attestation verify --format json` success
# shape for the handed subject's digest. Fails closed on any drift.
# gh 2.83.2 mutexes --signer-repo with --cert-identity: a presented
# --signer-repo alongside --cert-identity is an UNSUPPORTED argv and
# must refuse (mirroring gh's own flag-group exit), never silently
# pass; --repo already enforces the cert sourceRepositoryURI and
# --cert-identity already pins the exact signer workflow SAN.
subj=""; bundle=""; root=""; repo=""; ident=""; issuer=""; signer_repo=""; src_ref=""; deny=""; fmt=""; src_digest=""
prev=""
for a in "$@"; do
  case "$prev" in
    --bundle) bundle="$a";;
    --custom-trusted-root) root="$a";;
    --repo) repo="$a";;
    --cert-identity) ident="$a";;
    --cert-oidc-issuer) issuer="$a";;
    --signer-repo) signer_repo="$a";;
    --source-ref) src_ref="$a";;
    --source-digest) src_digest="$a";;
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
# --signer-repo is not a supported partner of --cert-identity in gh
# 2.83.2; a correct invocation must not carry it at all.
[ -z "$signer_repo" ] || fail "unsupported --signer-repo '$signer_repo'"
[ "$src_ref" = "refs/heads/{main}" ] || fail "source-ref '$src_ref'"
[ -n "$deny" ] || fail "missing --deny-self-hosted-runners"
# The trusted root must be the verifier's OWN captured snapshot inside
# a cadence-retained-* workspace -- never the package's
# trusted_root.jsonl or any other anchored file.
case "$root" in */cadence-retained-*/.trusted-root.jsonl) ;; *) fail "root '$root' not owned snapshot";; esac
case "$root" in *trusted_root.jsonl) fail "package trusted_root.jsonl anchored";; esac
# Subject arg must be the captured copy inside the owned workspace.
case "$subj" in */cadence-retained-*) ;; *) fail "subject '$subj' not captured";; esac
case "$bundle" in
  */{promo_name})
    cmp -s "$bundle" "{expected_promotion}" || fail "promotion bundle bytes not the fixture's"
    [ "$ident" = "https://github.com/{repo}/.github/workflows/{promo_wf}@refs/heads/{main}" ] || fail "promotion cert-identity '$ident'"
    [ -z "$src_digest" ] || fail "promotion pass pinned --source-digest"
cat <<'JSON'
{success_promo}
JSON
    ;;
  */{build_name})
    cmp -s "$bundle" "{expected_build}" || fail "build bundle bytes not the fixture's"
    [ "$ident" = "https://github.com/{repo}/.github/workflows/{build_wf}@refs/heads/{main}" ] || fail "build cert-identity '$ident'"
    [ "$src_digest" = "{source_sha}" ] || fail "build --source-digest '$src_digest'"
cat <<'JSON'
{success_build}
JSON
    ;;
  */{completion_name})
    cmp -s "$bundle" "{expected_completion}" || fail "completion bundle bytes not the fixture's"
    [ "$ident" = "https://github.com/{repo}/.github/workflows/{completion_wf}@refs/heads/{main}" ] || fail "completion cert-identity '$ident'"
    [ -z "$src_digest" ] || fail "completion pass pinned --source-digest"
cat <<'JSON'
{success_completion}
JSON
    ;;
  *) fail "unexpected bundle '$bundle'";;
esac
exit 0
"#,
        repo = REPO,
        main = MAIN,
        promo_name = PROMOTION_BUNDLE,
        build_name = BUILD_BUNDLE,
        completion_name = COMPLETION_BUNDLE,
        promo_wf = STAGING_WORKFLOW,
        build_wf = WORKFLOW,
        completion_wf = COMPLETION_WORKFLOW,
        source_sha = SOURCE_SHA,
        expected_promotion = expected_promotion.display(),
        expected_build = expected_build.display(),
        expected_completion = expected_completion.display(),
        success_promo = verify_success_json(RECEIPT_FILE, &d.receipt, PROMOTION_HEAD_SHA),
        success_build = verify_success_json(BINARY, &d.bin, SOURCE_SHA),
        success_completion =
            verify_success_json(COMPLETION_FILE, &d.completion, COMPLETION_HEAD_SHA),
    );
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

fn offline_with_gh(gh: &Path) -> OfflineGh {
    let mut src = OfflineGh::new(REPO);
    src.program = gh.to_path_buf();
    src
}

/// Fresh package + oracle + source under one owned temp root.
struct Fixture {
    _root: tempfile::TempDir,
    pkg: PathBuf,
}

fn fixture() -> (Fixture, OfflineGh) {
    let root = tempfile::Builder::new()
        .prefix("c1455-acceptance-")
        .tempdir()
        .unwrap();
    let pkg = root.path().join("package");
    let tools = root.path().join("tools");
    fs::create_dir_all(&pkg).unwrap();
    fs::create_dir_all(&tools).unwrap();
    let digests = write_package(&pkg);
    let gh = write_fake_gh(&tools, &digests);
    let src = offline_with_gh(&gh);
    (Fixture { _root: root, pkg }, src)
}

/// Map of relative path -> sha256 for a directory, used to prove
/// verify_retained never mutates the caller's package.
fn snapshot_tree(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.push((rel, sha256_hex(&fs::read(&p).unwrap())));
            }
        }
    }
    out.sort();
    out
}

/// POSITIVE: a complete coherent package — closed receipt, binary
/// digest agreement across receipt/sha256/manifest, three distinct
/// verified bundles with three distinct run heads, a closed completion
/// receipt binding the exact receipt/manifest/facts digests and run
/// ids, and closed source facts naming the same candidate — must
/// return `Ok(RetainedProof)` with the captured workspace still alive
/// and the verified bytes intact. The caller's package is untouched.
#[test]
fn cad1455_completed_promotion_evidence_verifies_and_stays_alive() {
    let (fx, src) = fixture();
    let before = snapshot_tree(&fx.pkg);

    let proof = src
        .verify_retained(&fx.pkg)
        .expect("a complete coherent package must verify");

    // The captured workspace is owned and alive: the same verified
    // bytes are readable from it, and it is a different directory from
    // the caller's package — the install step consumes these copies,
    // not the mutable source paths.
    assert!(
        proof.captured.dir.is_dir(),
        "captured workspace must still be alive"
    );
    assert_ne!(
        proof.captured.dir, fx.pkg,
        "the captured copy must be the verifier's own workspace, not the caller's dir"
    );
    for name in [
        BINARY,
        RECEIPT_FILE,
        MANIFEST,
        SHA_FILE,
        COMPLETION_FILE,
        SOURCE_FACTS_FILE,
    ] {
        assert_eq!(
            sha256_hex(&fs::read(fx.pkg.join(name)).unwrap()),
            sha256_hex(&fs::read(proof.captured.dir.join(name)).unwrap()),
            "captured {name} must be byte-identical to the package member"
        );
    }
    assert_eq!(proof.source_sha, SOURCE_SHA);
    assert_eq!(proof.completion.promotion_run_id, PROMOTION_RUN_ID);
    assert_eq!(
        proof.completion.promotion_run_attempt,
        PROMOTION_RUN_ATTEMPT
    );
    assert_eq!(proof.completion.source_sha, SOURCE_SHA);
    assert_eq!(proof.completion.completion_run_id, COMPLETION_RUN_ID);
    assert_eq!(proof.completion.completion_head_sha, COMPLETION_HEAD_SHA);
    assert_eq!(proof.facts.source_sha, SOURCE_SHA);
    assert_eq!(proof.facts.schema_version, SCHEMA_VERSION);
    assert_eq!(
        proof.facts.ancestors.last().map(String::as_str),
        Some(SOURCE_SHA)
    );

    assert_eq!(
        before,
        snapshot_tree(&fx.pkg),
        "verify_retained mutated the caller's package dir"
    );
}

/// NEGATIVE: the SAME otherwise-COMPLETE package minus ONLY the
/// completion evidence (`promotion-completion.json` +
/// `completion-attestation.jsonl`) must refuse. The source facts and
/// manifest stay in place, so the SOLE deficit is the missing
/// authenticated final-success proof — a mid-run promotion receipt
/// alone never authorizes install, and no unrelated missing-file
/// refusal can mask a finality-enforcement regression. The refusal
/// happens before any execution or install mutation, regardless of
/// where in the verification order it fires.
#[test]
fn cad1455_missing_completion_evidence_must_not_authorize() {
    let (fx, src) = fixture();
    // Remove ONLY the completed-promotion proof: receipt + bundle.
    // Facts and manifest remain — the missing authenticated finality
    // evidence is the sole deficit under test.
    fs::remove_file(fx.pkg.join(COMPLETION_FILE)).unwrap();
    fs::remove_file(fx.pkg.join(COMPLETION_BUNDLE)).unwrap();

    let before = snapshot_tree(&fx.pkg);
    let result = src.verify_retained(&fx.pkg);

    assert_eq!(
        before,
        snapshot_tree(&fx.pkg),
        "verify_retained mutated the caller's package dir"
    );

    match result {
        Err(Error::Rejected(msg)) => {
            assert!(
                is_finality_refusal(&msg),
                "refused, but not for missing completed-promotion proof: {msg}"
            );
        }
        Err(other) => panic!(
            "verify_retained failed with a non-refusal error (setup/compile \
             problem, not a gate decision): {other}"
        ),
        Ok(_) => panic!(
            "CAD-1455 GUARD GAP: a mid-run promotion receipt + valid build \
             attestation authorized a retained install with no authenticated \
             completed-promotion proof (run {PROMOTION_RUN_ID}/attempt \
             {PROMOTION_RUN_ATTEMPT} may later fail)"
        ),
    }
}

/// The refusal must concern missing final-success proof, not an
/// unrelated earlier step — otherwise the fixture never reached the
/// guard and the run cannot count as a real refusal.
fn is_finality_refusal(msg: &str) -> bool {
    let m = msg.to_lowercase();
    let names_finality = m.contains("complet")
        || m.contains("conclud")
        || m.contains("final")
        || m.contains("in progress")
        || m.contains("in-progress")
        || m.contains("success")
        || m.contains("eligib")
        || m.contains("outcome")
        || m.contains("unproven");
    let not_a_syntax_or_crypto_miss = !(m.contains("did not verify")
        || m.contains("not valid json")
        || m.contains("schema")
        || m.contains("subject ")
        || m.contains("sha256 mismatch")
        || m.contains("is not a regular file"));
    names_finality && not_a_syntax_or_crypto_miss
}

/// FORGED promotion bundle: the otherwise COMPLETE package keeps its
/// completion evidence, so the finality guard is satisfied and cannot
/// hide the forgery — the refusal must come from the crypto step on
/// the wrong bytes, proving authorization binds to verified content,
/// not file presence.
#[test]
fn cad1455_forged_promotion_bundle_is_refused() {
    let (fx, src) = fixture();
    fs::write(
        fx.pkg.join(PROMOTION_BUNDLE),
        b"{\"forged\":true,\"claim\":\"success\"}",
    )
    .unwrap();
    let msg = match src.verify_retained(&fx.pkg) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a forged promotion bundle must refuse"),
    };
    assert!(
        msg.contains("promotion") || msg.contains("attestation") || msg.contains("bundle"),
        "forged promotion bundle refusal should name the evidence: {msg}"
    );
}

/// MISMATCHED bound digests: the completion receipt claims different
/// source-facts/manifest bytes than the package carries — the real
/// binding check must refuse even though every bundle still verifies.
/// This exercises the digest-binding guard the completion receipt
/// exists to enforce, not a missing-file refusal.
#[test]
fn cad1455_mismatched_bound_facts_are_refused() {
    let (fx, src) = fixture();
    // Rewrite source-facts with different bytes while keeping the
    // completion receipt's bound digest — the digests now disagree.
    fs::write(
        fx.pkg.join(SOURCE_FACTS_FILE),
        serde_json::to_vec(&json!({
            "schema": 1,
            "source_sha": SOURCE_SHA,
            "observed_main_head": OBSERVED_MAIN_HEAD,
            "ancestors": [SOURCE_SHA],
            "schema_version": SCHEMA_VERSION,
        }))
        .unwrap(),
    )
    .unwrap();
    let msg = match src.verify_retained(&fx.pkg) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("source-facts bytes that disagree with the bound digest must refuse"),
    };
    assert!(
        msg.contains("facts") || msg.contains("digest") || msg.contains("completion"),
        "mismatched bound facts refusal should name the binding: {msg}"
    );
}
