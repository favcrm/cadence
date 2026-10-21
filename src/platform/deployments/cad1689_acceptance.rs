//! CAD-1689 independent refusal acceptance. Reviewer-written from the ticket
//! (cc-rev-spec, Claude Opus), not by the implementer. Implementers must not
//! edit or weaken this file.
//!
//! The hosted WhatsApp door opens without `hosted.lease` only from the
//! image-owned `hosted-whatsapp-door@1` admission, read through the real
//! root-owned metadata reader (`read_components`, here at a fixture root with
//! the expected owner as a parameter) and composed by the real
//! `agenticos::attach`. Every case runs with the lease off, twice: once with
//! no `CADENCE_AGENTICOS_URL` and once with a hostile explicit URL, each in a
//! relaunched child so the process environment of other tests is untouched.
//!
//! Refused (door stays closed, or the loader refuses the file):
//! (a) metadata with no WhatsApp admission entry (empty, or other providers);
//! (b) the exact WhatsApp provider/origin/pin with no transport, or with a
//!     different transport: a bare pin is not the admission;
//! (c) the WhatsApp transport on a wrong origin, provider or pin: the file is
//!     refused outright;
//! (d) a group- or world-writable file, a file not owned by the expected
//!     owner (production: root), or a symlink: the loader refuses;
//! (e) an explicit `CADENCE_AGENTICOS_URL` alone, lease off, no admission.
//! Positive: the exact entry, loaded from a correctly owned 0644 file, opens
//! the door with the lease off, and grants no email or SMTP authority.

use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

const ISOLATED: &str = "CADENCE_TEST_CAD1689_ISOLATED";
const TEST: &str =
    "platform::deployments::cad1689_acceptance::whatsapp_door_opens_only_from_the_image_admission";
const REFUSED: &str = "trusted provider deployment metadata refused";
const EXACT: (&str, &str, &str, &str) = (
    "whatsapp",
    "http://api.internal",
    "whatsapp-door@1",
    "hosted-whatsapp-door@1",
);

fn entry(provider: &str, origin: &str, pin: &str, transport: Option<&str>) -> serde_json::Value {
    let mut entry = serde_json::json!({"provider":provider,"origin":origin,"manifest_pin":pin});
    if let Some(transport) = transport {
        entry["transport"] = serde_json::json!(transport);
    }
    entry
}

fn doc(entries: Vec<serde_json::Value>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"schema":1,"providers":entries})).unwrap()
}

fn exact() -> serde_json::Value {
    entry(EXACT.0, EXACT.1, EXACT.2, Some(EXACT.3))
}

/// Write `bytes` at `<root>/provider-deployments.json` with `mode` and load
/// it through the real pinned reader, expecting `owner`.
fn load_at(bytes: &[u8], mode: u32, owner: u32) -> Result<Option<DeploymentMetadata>> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let file = dir.path().join("provider-deployments.json");
    std::fs::write(&file, bytes).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
    read_fixture(dir.path(), "provider-deployments.json", owner)
}

/// Compose a lease-off daemon from trusted metadata through the real attach.
fn attach_lease_off(metadata: DeploymentMetadata) -> crate::daemon::ServeOptions {
    let mut opts = crate::daemon::ServeOptions {
        provider_deployments: Some(metadata),
        ..Default::default()
    };
    let hosted = crate::lease::Hosted::default();
    crate::platform::agenticos::attach(&mut opts, &hosted).expect("attach");
    opts
}

fn assert_refused(result: Result<Option<DeploymentMetadata>>, case: &str) {
    match result {
        Err(error) => assert!(
            error.to_string().contains(REFUSED),
            "{case}: wrong refusal: {error}"
        ),
        Ok(_) => panic!("{case}: the loader admitted it"),
    }
}

#[test]
fn whatsapp_door_opens_only_from_the_image_admission() {
    if std::env::var_os(ISOLATED).is_none() {
        for url in [None, Some("http://127.0.0.1:9")] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
                .env(ISOLATED, "1");
            match url {
                Some(url) => child.env("CADENCE_AGENTICOS_URL", url),
                None => child.env_remove("CADENCE_AGENTICOS_URL"),
            };
            let out = child.output().unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.status.success(), "{url:?}: {text}");
            assert!(text.contains("1 passed"), "{url:?}: {text}");
        }
        return;
    }
    let uid = unsafe { libc::geteuid() };

    // Positive: the exact entry from a correctly owned 0644 file opens the
    // door with the lease off, and is no email or SMTP authority.
    let loaded = load_at(&doc(vec![exact()]), 0o644, uid)
        .expect("exact admission loads")
        .expect("file present");
    let opts = attach_lease_off(loaded);
    assert!(
        opts.hosted_whatsapp.is_some(),
        "exact admission must open the door"
    );
    assert!(
        opts.hosted_email.is_none(),
        "the WhatsApp admission is no email door"
    );
    assert!(
        opts.smtp_internal.is_none(),
        "the WhatsApp admission is no SMTP relay"
    );

    // (a) + (e): no admission entry, with or without an explicit URL.
    for (case, bytes) in [
        ("empty providers", doc(vec![])),
        (
            "agenticos pin only",
            doc(vec![entry(
                "agenticos",
                "http://api.internal",
                "agenticos-manifest@1/publish_post@2",
                None,
            )]),
        ),
        (
            "smtp admission only",
            doc(vec![entry(
                "smtp",
                "http://smtp.internal",
                "smtp-internal-relay@2",
                Some("hosted-smtp-relay@1"),
            )]),
        ),
    ] {
        let metadata = DeploymentMetadata::parse(&bytes)
            .unwrap_or_else(|e| panic!("{case}: fixture must parse: {e}"));
        assert!(
            attach_lease_off(metadata).hosted_whatsapp.is_none(),
            "{case}: door opened without its admission"
        );
    }

    // (b) the exact tuple without the admission transport is a bare pin.
    let bare = DeploymentMetadata::parse(&doc(vec![entry(EXACT.0, EXACT.1, EXACT.2, None)]))
        .expect("a bare pin entry parses");
    assert!(
        attach_lease_off(bare).hosted_whatsapp.is_none(),
        "a bare WhatsApp pin opened the door"
    );
    for transport in [
        "hosted-smtp-relay@1",
        "hosted-media-lease@1",
        "hosted-business-documents@1",
        "hosted-whatsapp-door@2",
    ] {
        assert_refused(
            load_at(
                &doc(vec![entry(EXACT.0, EXACT.1, EXACT.2, Some(transport))]),
                0o644,
                uid,
            ),
            transport,
        );
    }

    // (c) the admission transport on any other tuple refuses the whole file.
    for (case, provider, origin, pin) in [
        ("wrong origin", EXACT.0, "http://evil.example", EXACT.2),
        ("https origin", EXACT.0, "https://api.internal", EXACT.2),
        (
            "origin with port",
            EXACT.0,
            "http://api.internal:80",
            EXACT.2,
        ),
        ("wrong provider", "whatsapp_business", EXACT.1, EXACT.2),
        ("wrong pin", EXACT.0, EXACT.1, "whatsapp-door@2"),
        ("empty pin", EXACT.0, EXACT.1, ""),
    ] {
        assert_refused(
            load_at(
                &doc(vec![entry(provider, origin, pin, Some(EXACT.3))]),
                0o644,
                uid,
            ),
            case,
        );
    }

    // (d) ownership and mode: the reader refuses before any admission.
    let good = doc(vec![exact()]);
    assert_refused(load_at(&good, 0o664, uid), "group-writable");
    assert_refused(load_at(&good, 0o646, uid), "world-writable");
    assert_refused(load_at(&good, 0o644, uid.wrapping_add(1)), "wrong owner");
    if uid != 0 {
        // Production expects root (owner 0); a file owned by this uid is refused.
        assert_refused(load_at(&good, 0o644, 0), "not root-owned");
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let real = dir.path().join("real.json");
    std::fs::write(&real, &good).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&real, dir.path().join("provider-deployments.json")).unwrap();
    assert_refused(
        read_fixture(dir.path(), "provider-deployments.json", uid),
        "symlink",
    );
    // A writable metadata directory is refused too.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
    assert_refused(
        read_fixture(dir.path(), "real.json", uid),
        "writable directory",
    );
}
