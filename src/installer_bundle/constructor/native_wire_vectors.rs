//! Cross-language wire vectors shared with AgenticOS v2 PR349 (AOS-159).
//!
//! `fixtures/native-wire-vectors.v1.json` is committed byte-identically in the
//! AgenticOS repository (`apps/api/src/runtime/fixtures/`). Both suites pin its
//! sha256, so drift on either side fails. The AgenticOS side produces every
//! signed artifact and every frame its real `OwnedInstallerConstruction` emits;
//! this module consumes those bytes. It also produces the constructor's own
//! frames: the same structured input is re-encoded through this crate's
//! serializers and must reproduce the committed bytes.
//!
//! ALL KEYS IN THE FIXTURE ARE SYNTHETIC AND TEST-ONLY. They are the public
//! halves of keys derived from a public label, never provisioned and never
//! production keys.
//!
//! Test-only wiring: this module is mounted from `lifecycle.rs` behind
//! `#[cfg(test)]` so it can reach the private release, authorization and
//! exchange types. No production behaviour changes.
use super::super::{
    authenticate_bootstrap, authenticate_image_attestation, canonical, decode, wire, Configure,
    Header, Manifest, QualifiedBootstrap, DOMAIN as IMAGE_DOMAIN,
};
use super::{
    authenticate, pi_reply, store_reply, Command, Query, Readback, Release, Request, Response,
    StorePurpose, DOMAIN as RUNTIME_DOMAIN,
};
use crate::daemon::supervisor_grant as grant;
use crate::error::Error;
use crate::protected_pi_profile::authority::OperationScope;
use crate::protected_pi_profile::purpose::{authenticate_operation, PiKeyRecord};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const FIXTURE: &str = include_str!("fixtures/native-wire-vectors.v1.json");
/// sha256 of the fixture. The AgenticOS suite pins the SAME digest.
const FIXTURE_SHA256: &str = "c8a147c245ca62bd916f3832a6c2fa53809ee8afdd8a12d5da546fc503e1a117";
const PI_DOMAIN: &[u8] = b"cadence.protected-pi-launch.v1\0";

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}
fn text(v: &Value) -> &str {
    v.as_str().unwrap()
}
fn num(v: &Value) -> u64 {
    v.as_u64().unwrap()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn token(parts: &Value) -> String {
    parts
        .as_array()
        .unwrap()
        .iter()
        .map(text)
        .collect::<Vec<_>>()
        .join(".")
}
fn expand(line: &str, v: &Value) -> String {
    let arts = &v["artifacts"];
    line.replace(
        text(&v["placeholders"]["image"]),
        &token(&arts["imageAttestation"]["positive"]["parts"]),
    )
    .replace(
        text(&v["placeholders"]["runtime"]),
        &token(&arts["runtimeAuthorization"]["positive"]["parts"]),
    )
}
/// `Value` is a sorted map (no `preserve_order`), so this is the recursively
/// sorted compact encoding the constructor's serializers emit.
fn sorted(v: &Value) -> String {
    String::from_utf8(serde_json::to_vec(v).unwrap()).unwrap()
}
fn now(v: &Value) -> u64 {
    num(&v["clock"]["nowMs"])
}
fn frames(v: &Value) -> &Vec<Value> {
    v["frames"].as_array().unwrap()
}
fn emitted(v: &Value, id: &str) -> String {
    let f = frames(v)
        .iter()
        .find(|f| f["id"] == id)
        .unwrap_or_else(|| panic!("frame {id}"));
    expand(text(&f["emitted"]), v)
}
fn is_unconfigured(e: &Error) -> bool {
    matches!(e, Error::Rejected(m) if m.contains("image authority unconfigured"))
}

/// The bootstrap the constructor holds after `authenticate_bootstrap`, built by
/// hand because that function cannot succeed without compiled image pins.
fn bootstrap(v: &Value) -> QualifiedBootstrap {
    let s = &v["scenario"];
    let manifest: Manifest =
        serde_json::from_value(v["artifacts"]["imageAttestation"]["manifest"].clone()).unwrap();
    QualifiedBootstrap {
        manifest,
        image_attestation: token(&v["artifacts"]["imageAttestation"]["positive"]["parts"]),
        launch: grant::parse_launch(&s["launch"]).unwrap(),
        lineage: grant::parse_lineage(&s["lineage"]).unwrap(),
        operation: text(&s["operation"]).into(),
        barrier_nonce: text(&s["barrierNonce"]).into(),
        expires_at_ms: num(&s["configureExpiresAtMs"]),
        authenticated_at_ms: now(v),
    }
}
fn binding(v: &Value, b: &QualifiedBootstrap) -> String {
    let s = &v["scenario"];
    let installer: wire::Installer = serde_json::from_value(s["installer"].clone()).unwrap();
    let recipient: wire::Recipient = serde_json::from_value(s["recipient"].clone()).unwrap();
    wire::binding(b, &installer, &recipient).unwrap()
}
fn ring_verify(domain: &[u8], parts: &Value, public_key: &Value) -> bool {
    let p: Vec<&str> = parts.as_array().unwrap().iter().map(text).collect();
    let mut message = domain.to_vec();
    message.extend_from_slice(format!("{}.{}", p[0], p[1]).as_bytes());
    let key = decode(text(public_key), 32).unwrap();
    let signature = decode(p[2], 64).unwrap();
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(&message, &signature)
        .is_ok()
}
fn sequence_of(v: &Value, id: &str) -> u64 {
    let parsed: Value = serde_json::from_str(&emitted(v, id)).unwrap();
    num(&parsed["sequence"])
}

#[test]
fn fixture_digest_is_pinned_and_labelled_test_only() {
    assert_eq!(
        hex(&Sha256::digest(FIXTURE.as_bytes())),
        FIXTURE_SHA256,
        "native-wire-vectors.v1.json drifted from the pinned bytes; it must stay identical in both repositories"
    );
    let v = fixture();
    assert_eq!(v["schema"], "native-wire-vectors.v1");
    assert!(text(&v["testOnly"]).starts_with("SYNTHETIC TEST-ONLY"));
    assert!(!FIXTURE.contains("PRIVATE"));
}

#[test]
fn image_attestation_vector_reaches_the_pin_lookup_and_its_signature_verifies_independently() {
    let v = fixture();
    let a = &v["artifacts"]["imageAttestation"];
    let image = text(&a["manifest"]["image"]);
    let bound = num(&v["scenario"]["configureExpiresAtMs"]);
    let good = token(&a["positive"]["parts"]);
    let parts: Vec<&str> = good.split('.').collect();
    // The exact bytes pass every structural gate: canonical header and manifest,
    // exact fields, lifetime, artifact hashes and trust rings. The production
    // verifier then refuses only because its compiled pin table is EMPTY and has
    // no test seam, so the signature step itself is checked independently below.
    let err = authenticate_image_attestation(&good, now(&v), Some(image), bound)
        .err()
        .unwrap();
    assert!(is_unconfigured(&err), "{err:?}");
    let header: Header = canonical(&decode(parts[0], 256).unwrap()).unwrap();
    assert_eq!(header.kid, text(&v["keys"]["image"]["kid"]));
    assert_eq!(header.key_version, 1);
    assert_eq!(header.issuer, "agenticos-native-image-owner");
    let manifest: Manifest = canonical(&decode(parts[1], 20000).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&manifest).unwrap(), a["manifest"]);
    assert_eq!(IMAGE_DOMAIN, format!("{}\0", text(&a["domain"])).as_bytes());
    let public = &v["keys"]["image"]["publicKey"];
    assert!(ring_verify(IMAGE_DOMAIN, &a["positive"]["parts"], public));
    // Altered byte: the original signature no longer verifies.
    assert!(!ring_verify(
        IMAGE_DOMAIN,
        &a["negatives"]["alteredByte"]["parts"],
        public
    ));
    // Wrong kid: signed correctly, but the header names no pinned identity.
    let wrong = &a["negatives"]["wrongKid"]["parts"];
    assert!(ring_verify(IMAGE_DOMAIN, wrong, public));
    let wrong_header: Header =
        canonical(&decode(text(&wrong.as_array().unwrap()[0]), 256).unwrap()).unwrap();
    assert_ne!(wrong_header.kid, header.kid);
    // Expired: refused BEFORE the pin lookup, at the exact expiry boundary.
    let expired = num(&a["negatives"]["expired"]["nowMs"]);
    assert_eq!(expired, num(&a["manifest"]["expiresAtMs"]));
    let err = authenticate_image_attestation(&good, expired, Some(image), bound)
        .err()
        .unwrap();
    assert!(!is_unconfigured(&err), "{err:?}");
    // A different expected image is refused before the lookup as well.
    let other = "registry.example/other@sha256:aa";
    let err = authenticate_image_attestation(&good, now(&v), Some(other), bound)
        .err()
        .unwrap();
    assert!(!is_unconfigured(&err), "{err:?}");
}

#[test]
fn runtime_authorization_vector_is_accepted_and_every_negative_refused() {
    let v = fixture();
    let a = &v["artifacts"]["runtimeAuthorization"];
    let b = bootstrap(&v);
    let bound = binding(&v, &b);
    // Cross-language equality of the canonical enrollment binding bytes.
    assert_eq!(bound, text(&v["scenario"]["bindingJson"]));
    let generation = text(&v["scenario"]["recipient"]["generation"]);
    let release = |authorization: String| Release {
        version: 1,
        kind: "runtime-release".into(),
        operation: b.operation.clone(),
        barrier_nonce: b.barrier_nonce.clone(),
        authorization,
    };
    let ok = authenticate(
        &release(token(&a["positive"]["parts"])),
        &b,
        &bound,
        generation,
        now(&v),
    )
    .unwrap();
    assert_eq!(ok.expires_at_ms, num(&a["payload"]["expiresAtMs"]));
    let public = &v["keys"]["runtime"]["publicKey"];
    assert!(ring_verify(RUNTIME_DOMAIN, &a["positive"]["parts"], public));
    // The altered token is well-formed and canonical: only its signature fails.
    assert!(!ring_verify(
        RUNTIME_DOMAIN,
        &a["negatives"]["alteredByte"]["parts"],
        public
    ));
    assert!(ring_verify(
        RUNTIME_DOMAIN,
        &a["negatives"]["wrongKid"]["parts"],
        public
    ));
    for name in ["alteredByte", "wrongKid"] {
        let t = token(&a["negatives"][name]["parts"]);
        assert!(
            authenticate(&release(t), &b, &bound, generation, now(&v)).is_err(),
            "{name}"
        );
    }
    let expired = num(&a["negatives"]["expired"]["nowMs"]);
    assert_eq!(expired, num(&a["payload"]["expiresAtMs"]));
    let t = token(&a["negatives"]["expired"]["parts"]);
    assert!(authenticate(&release(t.clone()), &b, &bound, generation, expired).is_err());
    // A different binding or generation never accepts the same signed bytes.
    assert!(authenticate(&release(t.clone()), &b, "{}", generation, now(&v)).is_err());
    assert!(authenticate(&release(t), &b, &bound, &"0".repeat(32), now(&v)).is_err());
    // Without the distinct runtime ring, runtime stays unavailable.
    let mut no_ring = bootstrap(&v);
    no_ring.manifest.runtime_trust = None;
    let good = release(token(&a["positive"]["parts"]));
    assert!(authenticate(&good, &no_ring, &bound, generation, now(&v)).is_err());
}

#[test]
fn pi_authorization_vector_with_number_array_digests_is_accepted_and_every_negative_refused() {
    let v = fixture();
    let a = &v["artifacts"]["piAuthorization"];
    let scope: OperationScope = serde_json::from_value(a["payload"]["scope"].clone()).unwrap();
    // OperationScope's digests are 32-number arrays on the wire, not hex strings.
    let round = serde_json::to_string(&scope).unwrap();
    assert!(round.contains("\"helper_sha256\":[221,221,"), "{round}");
    assert_eq!(serde_json::to_value(&scope).unwrap(), a["payload"]["scope"]);
    let reference = text(&a["payload"]["reference"]);
    let image_expiry = num(&a["runtimeImageExpiryMs"]);
    let k = &v["keys"]["pi"];
    let public: [u8; 32] = decode(text(&k["publicKey"]), 32)
        .unwrap()
        .try_into()
        .unwrap();
    let keys: Vec<PiKeyRecord> = vec![(
        text(&k["issuer"]).into(),
        text(&k["kid"]).into(),
        num(&k["keyVersion"]),
        public,
    )];
    let check = |scope: &OperationScope, reference: &str, parts: &Value, at: u64| {
        authenticate_operation(scope, reference, &token(parts), &keys, at, image_expiry)
    };
    let good = &a["positive"]["parts"];
    assert!(check(&scope, reference, good, now(&v)).is_ok());
    assert!(ring_verify(PI_DOMAIN, good, &k["publicKey"]));
    assert!(!ring_verify(
        PI_DOMAIN,
        &a["negatives"]["alteredByte"]["parts"],
        &k["publicKey"]
    ));
    assert!(ring_verify(
        PI_DOMAIN,
        &a["negatives"]["wrongKid"]["parts"],
        &k["publicKey"]
    ));
    assert!(check(
        &scope,
        reference,
        &a["negatives"]["alteredByte"]["parts"],
        now(&v)
    )
    .is_err());
    assert!(check(
        &scope,
        reference,
        &a["negatives"]["wrongKid"]["parts"],
        now(&v)
    )
    .is_err());
    let expired = num(&a["negatives"]["expired"]["nowMs"]);
    assert_eq!(expired, num(&a["payload"]["expiresAtMs"]));
    assert!(check(
        &scope,
        reference,
        &a["negatives"]["expired"]["parts"],
        expired
    )
    .is_err());
    // A different reference or scope never matches the same signed bytes.
    assert!(check(&scope, &"e1".repeat(16), good, now(&v)).is_err());
    let mut other = scope.clone();
    other.epoch += 1;
    assert!(check(&other, reference, good, now(&v)).is_err());
}

/// The constructor's own request builder, reproduced exactly: `exchange` turns
/// the typed request into a sorted `Value`, then adds the four common fields.
fn exchange_bytes(request: Request<'_>, sequence: u64, v: &Value) -> String {
    let s = &v["scenario"];
    let mut value = serde_json::to_value(request).unwrap();
    let obj = value.as_object_mut().unwrap();
    obj.insert("version".into(), 1.into());
    obj.insert("operation".into(), text(&s["operation"]).into());
    obj.insert("barrierNonce".into(), text(&s["barrierNonce"]).into());
    obj.insert("sequence".into(), sequence.into());
    String::from_utf8(wire::json(&value).unwrap()).unwrap()
}

fn runtime_request_bytes(v: &Value, id: &str, f: &Value) -> String {
    let scope: Option<OperationScope> = f
        .get("scope")
        .map(|s| serde_json::from_value(s.clone()).unwrap());
    let reference = f.get("reference").map(text).unwrap_or("");
    let request = match id {
        "request-runtime-current" => Request::RuntimeCurrent,
        "request-store-startup" => Request::StoreStartup,
        "request-store-acquire" => Request::StoreAcquire {
            purpose: StorePurpose::Close,
            attempt: text(&f["attempt"]),
        },
        "request-store-consume" => Request::StoreConsume { reference },
        "request-store-current" => Request::StoreCurrent { reference },
        "request-store-database-current" => Request::StoreDatabaseCurrent { reference },
        "request-pi-acquire" => Request::PiAcquire {
            scope: scope.as_ref().unwrap(),
        },
        "request-pi-consume" => Request::PiConsume {
            reference,
            scope: scope.as_ref().unwrap(),
        },
        "request-pi-current" => Request::PiCurrent {
            reference,
            scope: scope.as_ref().unwrap(),
        },
        "request-runtime-daemon-ready" => Request::RuntimeDaemonReady { reference },
        "request-runtime-serving-ready" => Request::RuntimeServingReady { reference },
        "request-task-event" => Request::TaskEvent {
            task: text(&f["task"]),
            part: num(&f["part"]),
            bytes: text(&f["bytes"]),
        },
        "request-task-retired" => Request::TaskRetired {
            task: text(&f["task"]),
        },
        other => panic!("unmapped runtime request {other}"),
    };
    exchange_bytes(request, num(&f["sequence"]), v)
}

#[test]
fn constructor_frames_reproduce_the_committed_bytes_exactly() {
    let v = fixture();
    let s = &v["scenario"];
    let installer: wire::Installer = serde_json::from_value(s["installer"].clone()).unwrap();
    let recipient: wire::Recipient = serde_json::from_value(s["recipient"].clone()).unwrap();
    let common = |kind: Value, extra: Value| {
        let mut o = json!({"version":1,"type":kind,"operation":s["operation"],"barrierNonce":s["barrierNonce"]});
        for (k, val) in extra.as_object().unwrap() {
            o[k] = val.clone();
        }
        String::from_utf8(wire::json(&o).unwrap()).unwrap()
    };
    // constructed: the exact object `run` sends.
    let constructed = String::from_utf8(
        wire::json(
            &json!({"version":1,"type":"constructed","operation":s["operation"],
            "barrierNonce":s["barrierNonce"],"installer":installer,"recipient":recipient}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(constructed, emitted(&v, "constructed"));
    // Owner requests: `owner_request` serializes the typed Kind.
    for (id, kind, sequence) in [
        ("request-owner-prepared", wire::Kind::Prepared, 1),
        ("request-owner-consume", wire::Kind::Consume, 2),
        (
            "request-owner-consumed-current",
            wire::Kind::ConsumedCurrent,
            3,
        ),
    ] {
        let line = common(
            serde_json::to_value(kind).unwrap(),
            json!({"sequence": sequence}),
        );
        assert_eq!(line, emitted(&v, id), "{id}");
    }
    // ack: the exact object `finish` sends.
    let ack = common(
        json!("ack"),
        json!({"recipientGeneration": s["recipient"]["generation"]}),
    );
    assert_eq!(ack, emitted(&v, "ack"));
    // Runtime requests through the real `Request` enum and the `exchange` shape.
    let mut seen = 0;
    for f in frames(&v) {
        let id = text(&f["id"]);
        if f["producer"] == "rust"
            && id.starts_with("request-")
            && !id.starts_with("request-owner-")
        {
            let parsed: Value = serde_json::from_str(text(&f["emitted"])).unwrap();
            assert_eq!(
                runtime_request_bytes(&v, id, &parsed),
                text(&f["emitted"]),
                "{id}"
            );
            seen += 1;
        }
    }
    assert_eq!(seen, 13);
    // Readback results: the exact envelope the exchange loop answers with.
    let sequence = sequence_of(&v, "request-task-retired");
    for r in v["readbacks"].as_array().unwrap() {
        let id = format!("runtime-readback-result-{}", text(&r["query"]["type"]));
        let line = String::from_utf8(
            wire::json(
                &json!({"version":1,"type":"runtime-readback-result","operation":s["operation"],
                "barrierNonce":s["barrierNonce"],"sequence":sequence,"data":r["data"]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(line, emitted(&v, &id), "{id}");
    }
    // Every Rust-produced frame is its own sorted compact canonical form.
    for f in frames(&v) {
        if f["producer"] == "rust" {
            assert_eq!(f["emitted"], f["canonical"], "{}", f["id"]);
        }
    }
}

#[test]
fn typescript_frames_decode_validate_and_re_encode_to_the_canonical_bytes() {
    let v = fixture();
    let b = bootstrap(&v);
    let bound = binding(&v, &b);
    let retired = sequence_of(&v, "request-task-retired");
    for f in frames(&v) {
        let id = text(&f["id"]);
        let line = expand(text(&f["emitted"]), &v);
        // The same structured input encodes to identical canonical bytes.
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(sorted(&parsed), expand(text(&f["canonical"]), &v), "{id}");
        assert!(line.len() < 65536, "{id}");
        if f["producer"] != "ts" {
            continue;
        }
        if !id.starts_with("runtime-") || id == "runtime-release" {
            assert!(line.is_ascii(), "bootstrap frame {id} must stay ASCII");
        }
        match id {
            "configure" => {
                let typed: Configure = serde_json::from_str(&line).unwrap();
                assert_eq!(
                    sorted(&serde_json::to_value(&typed).unwrap()),
                    sorted(&parsed)
                );
                // Every gate passes except the compiled (empty) image pin table.
                let err = authenticate_bootstrap(line.as_bytes(), now(&v))
                    .err()
                    .unwrap();
                assert!(is_unconfigured(&err), "{err:?}");
            }
            "release" => {
                let release: wire::Release = serde_json::from_str(&line).unwrap();
                assert_eq!(
                    String::from_utf8(release.frame(&b).unwrap()).unwrap(),
                    format!(
                        "enrolled-install-r3 {} {}\n",
                        text(&v["scenario"]["grant"]),
                        text(&v["scenario"]["receipt"])
                    )
                );
            }
            "owner-prepared" | "owner-consumed" | "owner-consumed-current" => {
                let response: wire::OwnerResponse = serde_json::from_str(&line).unwrap();
                let (kind, sequence) = match id {
                    "owner-prepared" => (wire::Kind::Prepared, 1),
                    "owner-consumed" => (wire::Kind::Consume, 2),
                    _ => (wire::Kind::ConsumedCurrent, 3),
                };
                response.validate(&b, &bound, sequence, kind).unwrap();
                assert_eq!(
                    sorted(&serde_json::to_value(&response).unwrap()),
                    sorted(&parsed)
                );
            }
            "owner-unknown" => {
                let response: wire::OwnerResponse = serde_json::from_str(&line).unwrap();
                assert!(response.current.is_none());
                assert!(response
                    .validate(&b, &bound, 1, wire::Kind::Prepared)
                    .is_err());
            }
            "runtime-release" => {
                let release: Release = serde_json::from_str(&line).unwrap();
                assert_eq!(
                    release.authorization,
                    token(&v["artifacts"]["runtimeAuthorization"]["positive"]["parts"])
                );
            }
            _ if id.starts_with("runtime-owner-") => {
                runtime_owner(&v, &b, &bound, id, &line);
            }
            _ if id.starts_with("runtime-readback-") => {
                let query: Readback = serde_json::from_str(&line).unwrap();
                assert_eq!(query.kind, "runtime-readback");
                assert_eq!(query.sequence, retired);
                let want = v["readbacks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| format!("runtime-readback-{}", text(&r["query"]["type"])) == id)
                    .unwrap();
                assert_eq!(
                    serde_json::to_value::<Query>(query.query).unwrap(),
                    want["query"]
                );
            }
            other => panic!("unhandled ts frame {other}"),
        }
    }
}

fn runtime_owner(v: &Value, b: &QualifiedBootstrap, bound: &str, id: &str, line: &str) {
    let reply: Response = serde_json::from_str(line).unwrap();
    assert_eq!((reply.version, reply.kind.as_str()), (1, "runtime-owner"));
    assert_eq!(reply.operation, b.operation);
    assert_eq!(reply.barrier_nonce, b.barrier_nonce);
    let lease = now(v) + 20_000;
    assert_eq!(reply.lease_expires_at_ms, lease);
    assert!(lease > now(v) && lease <= now(v) + 30_000);
    assert!(lease <= num(&v["artifacts"]["runtimeAuthorization"]["payload"]["expiresAtMs"]));
    assert_eq!(reply.current.binding_json, bound);
    assert_eq!(reply.current.phase, "consumed");
    assert_eq!(reply.current.closure, "open");
    assert_eq!(reply.current.epoch, b.launch.epoch);
    assert_eq!(reply.current.lineage.reference, b.lineage.reference);
    assert_eq!(
        reply.current.lineage.database_epoch,
        b.lineage.database_epoch
    );
    assert_eq!(reply.current.global, text(&v["scenario"]["global"]));
    assert_eq!(reply.current.company, text(&v["scenario"]["company"]));
    let with_command = id == "runtime-owner-task-event";
    assert_eq!(reply.commands.len(), usize::from(with_command));
    if with_command {
        match &reply.commands[0] {
            Command::Task {
                task,
                alias,
                model,
                prompt,
            } => {
                assert_eq!(task, text(&v["scenario"]["task"]));
                assert_eq!(alias, "synthetic-alias");
                assert_eq!(model, "openai-codex/gpt-6.1-sol");
                assert_eq!(prompt, "合成 prompt 🌙 \"quoted\"\nline");
            }
            _ => panic!("expected a task command"),
        }
    }
    let scope: OperationScope =
        serde_json::from_value(v["artifacts"]["piAuthorization"]["payload"]["scope"].clone())
            .unwrap();
    let store_phase = match id {
        "runtime-owner-store-startup" | "runtime-owner-store-acquire" => Some("issued"),
        "runtime-owner-store-consume"
        | "runtime-owner-store-current"
        | "runtime-owner-store-database-current" => Some("consumed"),
        _ => None,
    };
    let (has_store, has_pi) = (reply.store.is_some(), reply.pi.is_some());
    let again: Response = serde_json::from_str(line).unwrap();
    if let Some(phase) = store_phase {
        let store = store_reply(again, Some(phase)).unwrap();
        assert_eq!(store["phase"], phase);
        assert!(!has_pi);
    } else if id.starts_with("runtime-owner-pi-") {
        let pi = pi_reply(again, &scope).unwrap();
        assert_eq!(pi["reference"], v["scenario"]["piReference"]);
        assert!(!has_store);
    } else {
        assert!(!has_store && !has_pi, "{id}");
    }
}

#[test]
fn negative_frames_are_refused_by_the_rust_consumers() {
    let v = fixture();
    let b = bootstrap(&v);
    let bound = binding(&v, &b);
    let mut checked = 0;
    for n in v["negativeFrames"].as_array().unwrap() {
        if n["consumer"] != "rust" {
            continue;
        }
        let line = expand(text(&n["emitted"]), &v);
        match text(&n["id"]) {
            "configure-extra-field" => {
                let err = authenticate_bootstrap(line.as_bytes(), now(&v))
                    .err()
                    .unwrap();
                assert!(
                    !is_unconfigured(&err),
                    "unknown field must refuse before the pin lookup"
                );
            }
            "release-extra-field" => {
                assert!(serde_json::from_str::<wire::Release>(&line).is_err());
            }
            "owner-unknown-current-null" => {
                let r: wire::OwnerResponse = serde_json::from_str(&line).unwrap();
                assert!(r.validate(&b, &bound, 1, wire::Kind::Prepared).is_err());
            }
            "runtime-owner-extra-field" => {
                assert!(serde_json::from_str::<Response>(&line).is_err());
            }
            other => panic!("unhandled negative {other}"),
        }
        checked += 1;
    }
    assert_eq!(checked, 4);
}

#[test]
fn frames_the_typescript_consumer_refuses_are_not_constructor_shapes_either() {
    // The AgenticOS suite drives these through the real class and expects a
    // refusal. Here we confirm the constructor has no typed form that could
    // have produced them.
    let v = fixture();
    let mut checked = 0;
    for n in v["negativeFrames"].as_array().unwrap() {
        if n["consumer"] != "ts" {
            continue;
        }
        let parsed: Value = serde_json::from_str(&expand(text(&n["emitted"]), &v)).unwrap();
        match text(&n["id"]) {
            "constructed-extra-field" => assert!(parsed.get("extra").is_some()),
            "runtime-request-bad-purpose" => {
                assert!(serde_json::from_value::<StorePurpose>(parsed["purpose"].clone()).is_err());
            }
            "owner-request-replayed-sequence"
            | "ack-wrong-recipient-generation"
            | "readback-result-wrong-sequence" => {}
            other => panic!("unhandled negative {other}"),
        }
        checked += 1;
    }
    assert_eq!(checked, 5);
}
