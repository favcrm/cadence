//! CAD-1158 acceptance check v2 — independently authored by
//! `cad1158-acceptance-author`. This file SUPERSEDES v1
//! (`/var/www/agent-notes/artifacts/cad1158_lease_off_smtp_refusal.rs`,
//! sha256 bd8c8f94b13c86669af93fac9d3429d65a6428041f6e8113074c0c67ba09de02),
//! which left two forward arms commented with an implementer-uncomment
//! instruction — that violated the frozen-bytes rule. v2 activates both
//! arms. The implementer copies these bytes UNCHANGED; they may not edit,
//! uncomment, weaken or replace them.
//!
//! Ticket: `src/platform/agenticos.rs::attach` must compose the trusted
//! `smtp.internal` relay independently of the Cadence lifecycle lease, but
//! ONLY from a dedicated, versioned, image-owned SMTP transport proof bound
//! to the fixed `smtp.internal` relay contract. An arbitrary
//! `CADENCE_AGENTICOS_URL` or `CADENCE_SMTP_INTERNAL_URL`, an app/RPC field,
//! a forged PM setting, or a mere catalog/hostname is NOT that proof.
//!
//! Selected admission tuple (parent facts, companion AOS ticket):
//! provider="smtp", origin="http://smtp.internal",
//! manifest_pin="smtp-internal-relay@2", transport="hosted-smtp-relay@1".
//!
//! These tests exercise the REAL composition guard — `platform::agenticos::
//! attach` itself, with verbatim `ServeOptions` and a verbatim
//! `lease::Hosted` — and prove the consequential bad case is refused: a
//! daemon whose hosted lifecycle lease is off (or absent) and whose
//! environment carries a caller-controlled `CADENCE_AGENTICOS_URL` and a
//! forged `CADENCE_SMTP_INTERNAL_URL` must NOT install `opts.smtp_internal`.
//! With no relay installed, `Shared::smtp_deliver` and `verify_hosted_smtp`
//! cannot hand the custodied SMTP secret to any URL-directed transport.
//! Refusal is proven through the guard itself, not a stubbed predicate or
//! a prepopulated `smtp_internal` (which would bypass startup).
//!
//! Baseline status (author-verified v1 shape at base 12c12338, private
//! clone): the four negative/control tests pass — current source already
//! refuses lease-off URL env. The two forward arms exercising the NEW
//! `hosted-smtp-relay@1` tuple are ACTIVE in v2 and are expected to FAIL
//! at base, because the current parser refuses the unknown transport
//! string inside `DeploymentMetadata::parse` (a not-yet-implemented
//! interface/setup failure, NOT a forged-caller behavioral red). They
//! pass only when the fix implements the tuple. Implementer: do not
//! soften them; their failure at base is the pending-interface evidence.
//!
//! Mounting: copy this file verbatim to
//! `tests/cad1158_lease_off_smtp_refusal.rs` (keep this exact name; the
//! test names inside are unchanged and do not depend on the file name).
//! `scripts/result-test-args` picks it up automatically (cargo reports
//! every `tests/*.rs` target), so CI and `scripts/pre-push --tests` run
//! it with the test-seam feature set; the file uses no test-seam symbols.
//! All types used are public API at base 12c12338:
//! `daemon::ServeOptions`, `lease::Hosted`,
//! `platform::agenticos::attach`, `platform::agenticos::PLATFORM`,
//! `platform::agenticos_external::{attach, PLATFORM}`,
//! `platform::deployments::DeploymentMetadata::parse`.
//!
//! Isolation follows the existing convention in
//! `src/platform/deployments/tests.rs`: each parent test self-relaunches
//! this test binary per env case under a one-shot marker var with
//! `--exact <name> --test-threads 1 --nocapture`, so the forged
//! environment never races the parallel suite. Child env strips every
//! other `CADENCE_*` composition input that could leak into `attach` or
//! `SmtpInternal::from_env`.

use cadence_agent::daemon::ServeOptions;
use cadence_agent::lease::Hosted;
use cadence_agent::platform::{agenticos, deployments::DeploymentMetadata};

/// Trusted composition metadata that exists at base (schema 1): the media
/// lease assertion must NOT authorize SMTP. Whatever dedicated versioned
/// SMTP transport string the shipped proof adds, this file does not spell
/// it — metadata is image-owned and the suite cannot forge it anyway.
const MEDIA_ONLY_CONFIG: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const EMPTY_CONFIG: &str = r#"{"schema":1,"providers":[]}"#;

/// The real deployed image file at the time of writing (verified against
/// AgenticOS `infra/runtime-image/provider-deployments.json`): a publish
/// pin for `api.internal` plus the MEDIA lease — and NO dedicated SMTP
/// admission. Composing against exactly this file must leave the relay
/// off, lease on or off.
const DEPLOYED_IMAGE_CONFIG: &str = r#"{"schema":1,"providers":[
    {"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"},
    {"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}
]}"#;

/// The selected dedicated SMTP admission, alongside the retained media
/// assertion — the shape the companion image entry will carry.
const SMTP_ADMITTED_CONFIG: &str = r#"{"schema":1,"providers":[
    {"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"},
    {"provider":"smtp","origin":"http://smtp.internal","manifest_pin":"smtp-internal-relay@2","transport":"hosted-smtp-relay@1"}
]}"#;

/// Metadata whose ONLY transport assertion is the new SMTP one.
const SMTP_ONLY_CONFIG: &str = r#"{"schema":1,"providers":[
    {"provider":"smtp","origin":"http://smtp.internal","manifest_pin":"smtp-internal-relay@2","transport":"hosted-smtp-relay@1"}
]}"#;

const ISOLATED: &str = "CADENCE_TEST_CAD1158_ISOLATED";

/// Relaunch this test binary so `test` runs under `extra_env` in a
/// single-test child. Forged env lives only in that child — never in the
/// parallel suite's process.
fn relaunch(test: &str, extra_env: &[(&str, &str)]) {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--test-threads", "1", "--nocapture"])
        .env(ISOLATED, "1")
        // Strip every composition input the ambient env could supply,
        // then layer the case's forged values back on top.
        .env_remove("CADENCE_AGENTICOS_URL")
        .env_remove("CADENCE_SMTP_INTERNAL_URL")
        .env_remove("CADENCE_HOSTED_EMAIL_FROM")
        .env_remove("CADENCE_HOSTED_EMAIL_FROM_NAME")
        .env_remove("CADENCE_AGENTICOS_EXTERNAL_URL");
    for (name, value) in extra_env {
        cmd.env(name, value);
    }
    let out = cmd.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{test}: {text}");
    assert!(text.contains("1 passed"), "{test}: {text}");
}

/// One raw `attach` call inside the isolated child. `hosted` is a verbatim
/// lifecycle config (`lease` unset or `"off"` = off; `file:<path>` = the
/// supported local lease double). `metadata` is verbatim trusted
/// composition metadata or `None` (the image carries no deployments file,
/// or none that asserts this transport).
fn attach_once(hosted: &Hosted, metadata: Option<&str>) -> ServeOptions {
    // Runs only under the isolation marker, in a single-threaded,
    // single-test child — never in the parent suite process.
    assert!(std::env::var_os(ISOLATED).is_some());
    let mut opts = ServeOptions {
        lease: Some(hosted.clone()),
        provider_deployments: metadata.map(|raw| {
            DeploymentMetadata::parse(raw.as_bytes()).expect("fixture metadata must parse")
        }),
        ..Default::default()
    };
    // THE REAL GUARD. No local reimplementation of its policy.
    agenticos::attach(&mut opts, hosted)
        .expect("attach itself must not error on refused composition");
    opts
}

/// Lease OFF, forged `CADENCE_AGENTICOS_URL`, forged
/// `CADENCE_SMTP_INTERNAL_URL`, and no dedicated trusted SMTP proof
/// (no metadata, empty metadata, or the unrelated MEDIA assertion). The
/// bad case: URL env alone must never install a relay that custody
/// secrets could be handed to.
#[test]
fn cad1158_lease_off_forged_urls_do_not_install_smtp_relay() {
    if std::env::var_os(ISOLATED).is_none() {
        for (case, a_url, s_url) in [
            ("both_forged", "http://127.0.0.1:3110", "http://127.0.0.1:3111"),
            ("smtp_url_alone", "", "http://127.0.0.1:3111"),
            ("agenticos_url_alone", "http://127.0.0.1:3110", ""),
        ] {
            let mut env: Vec<(&str, &str)> = vec![("CAD1158_CASE", case)];
            if !a_url.is_empty() {
                env.push(("CADENCE_AGENTICOS_URL", a_url));
            }
            if !s_url.is_empty() {
                env.push(("CADENCE_SMTP_INTERNAL_URL", s_url));
            }
            relaunch(
                "cad1158_lease_off_forged_urls_do_not_install_smtp_relay",
                &env,
            );
        }
        return;
    }
    // Child: the refusal must hold under every untrusted/absent metadata
    // state — including the exact deployed image file. Each iteration
    // re-runs the real attach.
    for metadata in [
        None,
        Some(EMPTY_CONFIG),
        Some(MEDIA_ONLY_CONFIG),
        Some(DEPLOYED_IMAGE_CONFIG),
    ] {
        let opts = attach_once(
            &Hosted {
                lease: Some("off".into()),
                ..Default::default()
            },
            metadata,
        );
        assert!(
            opts.smtp_internal.is_none(),
            "lease-off forged env activated the credential-carrying relay \
             (case {:?}, metadata present: {})",
            std::env::var("CAD1158_CASE"),
            metadata.is_some()
        );
        assert!(
            opts.hosted_email.is_none(),
            "lease-off forged env activated hosted_email"
        );
        // The adapter may legitimately register under the explicit URL —
        // existing self-hosted behavior this ticket retains — but it must
        // carry NO deployment pin and NO relay authority.
        if let Some(adapter) = opts.platforms.get(agenticos::PLATFORM) {
            assert_eq!(
                adapter.reported_manifest_version(),
                None,
                "forged env gained a trusted deployment assertion"
            );
        }
    }
}

/// Lease ABSENT (hosted config default = off) plus a real parseable
/// deployments file that a hostile composer might stretch into SMTP
/// authority: a media-lease transport for `api.internal` is a different
/// transport, and a pin for provider `agenticos` names the publish
/// manifest — never relay admission. Even combined with forged URLs it
/// must not install `smtp_internal`.
#[test]
fn cad1158_lease_off_unrelated_trusted_metadata_does_not_install_smtp_relay() {
    if std::env::var_os(ISOLATED).is_none() {
        relaunch(
            "cad1158_lease_off_unrelated_trusted_metadata_does_not_install_smtp_relay",
            &[
                ("CADENCE_AGENTICOS_URL", "http://127.0.0.1:3110"),
                ("CADENCE_SMTP_INTERNAL_URL", "http://127.0.0.1:3111"),
            ],
        );
        return;
    }
    let forged_origin_pin = r#"{"schema":1,"providers":[{"provider":"agenticos","origin":"http://127.0.0.1:3110","manifest_pin":"agenticos-manifest@1/publish_post@2"}]}"#;
    for metadata in [
        Some(MEDIA_ONLY_CONFIG),
        Some(forged_origin_pin),
        Some(DEPLOYED_IMAGE_CONFIG),
    ] {
        let opts = attach_once(&Hosted::default(), metadata);
        assert!(
            opts.smtp_internal.is_none(),
            "unrelated trusted metadata + forged env activated the relay"
        );
    }
}

/// Positive controls: the guard is live — SOME trusted input must produce
/// `smtp_internal.is_some()`, or the fix silently does nothing and this
/// check proves only that nothing ever turns on.
///
/// Arm A (existing, retained): a real lifecycle lease (`file:` spec — the
/// supported local double; `attach` composes the client, it does not
/// acquire or dial the lease, so a stateless file suffices).
///
/// Arm B (the shipped fix): the dedicated `hosted-smtp-relay@1` image
/// assertion installs the relay with the lifecycle lease OFF, and must
/// not activate `hosted_email` as a side effect. At base this arm fails
/// inside `DeploymentMetadata::parse` — the parser refuses the unknown
/// transport — which is the pending-interface evidence, by design.
#[test]
fn cad1158_trusted_composition_installs_smtp_relay_positive_control() {
    if std::env::var_os(ISOLATED).is_none() {
        relaunch(
            "cad1158_trusted_composition_installs_smtp_relay_positive_control",
            &[],
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let lease_file = root.path().join("lease.json");
    std::fs::write(&lease_file, "{}").unwrap();
    let hosted = Hosted {
        lease: Some(format!("file:{}", lease_file.display())),
        ..Default::default()
    };
    // Arm A: trusted lease composition. `SmtpInternal::from_env` reads
    // `CADENCE_SMTP_INTERNAL_URL`; the relauncher stripped it, so the
    // client composes against the fixed `smtp.internal` default.
    let opts = attach_once(&hosted, None);
    assert!(
        opts.smtp_internal.is_some(),
        "trusted lease composition failed to install smtp_internal — \
         the guard under test never produced the admitted outcome"
    );

    // Arm B: the dedicated SMTP admission, lease OFF.
    let opts = attach_once(
        &Hosted {
            lease: Some("off".into()),
            ..Default::default()
        },
        Some(SMTP_ADMITTED_CONFIG),
    );
    assert!(
        opts.smtp_internal.is_some(),
        "the dedicated hosted-smtp-relay@1 image assertion failed to install \
         smtp_internal under a disabled lifecycle lease"
    );
    // hosted_email stays on its lease-gated path — the SMTP assertion must
    // not activate it as a side effect.
    assert!(
        opts.hosted_email.is_none(),
        "the SMTP admission leaked into the hosted_email path"
    );
}

/// Media/SMTP admission separation, proven through real attach paths — no
/// imaginary predicate. A transport assertion must grant only its own
/// transport.
///
/// Arm 1 (existing, retained): MEDIA metadata admitted through the REAL
/// `agenticos_external::attach` composes the hosted media adapter, but the
/// SAME file never installs `smtp_internal` or `hosted_email`.
///
/// Arm 2 (the pitfall guard): metadata whose only transport is
/// `hosted-smtp-relay@1` must NOT register hosted media — this is the
/// `hosted_media()` find-the-first-transport pitfall the fix must close.
/// At base this arm fails inside `DeploymentMetadata::parse` (unknown
/// transport), which is the pending-interface evidence, by design.
#[test]
fn cad1158_transport_assertions_do_not_cross_authorize() {
    if std::env::var_os(ISOLATED).is_none() {
        relaunch("cad1158_transport_assertions_do_not_cross_authorize", &[]);
        return;
    }
    // Arm 1: MEDIA assertion admitted through the real agenticos_external
    // attach: hosted media registers, but nothing grants the SMTP relay.
    let mut opts = ServeOptions {
        lease: Some(Hosted::default()),
        provider_deployments: Some(
            DeploymentMetadata::parse(DEPLOYED_IMAGE_CONFIG.as_bytes()).unwrap(),
        ),
        ..Default::default()
    };
    cadence_agent::platform::agenticos_external::attach(&mut opts)
        .expect("trusted media metadata attaches");
    agenticos::attach(&mut opts, &Hosted::default()).unwrap();
    assert!(
        opts.platforms
            .contains_key(cadence_agent::platform::agenticos_external::PLATFORM),
        "media admission did not compose the hosted media adapter"
    );
    assert!(
        opts.smtp_internal.is_none(),
        "the MEDIA transport assertion granted SMTP relay authority"
    );
    assert!(
        opts.hosted_email.is_none(),
        "the MEDIA transport assertion activated hosted_email"
    );

    // Arm 2: SMTP-only metadata must not register hosted media. At base
    // this fails on parse — the parser refuses `hosted-smtp-relay@1` —
    // which is the pending-interface evidence, not a caller-forge red.
    let mut opts = ServeOptions {
        lease: Some(Hosted::default()),
        provider_deployments: Some(
            DeploymentMetadata::parse(SMTP_ONLY_CONFIG.as_bytes())
                .expect("the hosted-smtp-relay@1 tuple must parse once the fix lands"),
        ),
        ..Default::default()
    };
    cadence_agent::platform::agenticos_external::attach(&mut opts).unwrap();
    assert!(
        !opts
            .platforms
            .contains_key(cadence_agent::platform::agenticos_external::PLATFORM),
        "the SMTP transport assertion granted hosted media authority"
    );
}
