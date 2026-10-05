//! CAD-1159 ticket-derived acceptance guard, independently authored by
//! aos159-constructor-guard. The product implementer must not edit/weaken it.
//! Calls the real production authenticator, not a mock root, observer or owner.
//! Synthetic signing below demonstrates attacker control, NOT accepted trust.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde_json::{json, Value};

// Independent encoding of the paired public v1 contract: recursively sorted
// object keys, compact JSON, then domain NUL base64url(header).base64url(payload).
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            let fields: Vec<_> = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical(&object[key])
                    )
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        _ => serde_json::to_string(value).unwrap(),
    }
}

fn attacker_attestation(key: &Ed25519KeyPair, header: &Value, payload: &Value) -> String {
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(canonical(header)),
        URL_SAFE_NO_PAD.encode(canonical(payload))
    );
    let mut message = b"cadence.native-image-qualification.v1\0".to_vec();
    message.extend_from_slice(signed.as_bytes());
    let signature = key.sign(&message);
    // A cryptographically valid self-signature must still NOT elect a root.
    UnparsedPublicKey::new(&ED25519, key.public_key().as_ref())
        .verify(&message, signature.as_ref())
        .unwrap();
    format!("{}.{}", signed, URL_SAFE_NO_PAD.encode(signature.as_ref()))
}

#[test]
fn caller_selected_build_key_pid_and_perfect_diagnostics_cannot_elect_authority() {
    const NOW: u64 = 1_800_000_000_000;
    let attacker = Ed25519KeyPair::from_seed_unchecked(&[0x59; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(attacker.public_key().as_ref());
    let image = format!("registry.invalid/cadence@sha256:{}", "a".repeat(64));
    let operation = "11590000-0000-4000-8000-000000000001";
    let header = json!({
        "alg": "Ed25519", "issuer": "agenticos-native-image-owner",
        "keyVersion": 1, "kid": "cad1159-attacker",
        "type": "native-image-qualification", "version": 1
    });
    let payload = json!({
        "version": 1, "image": image, "source": "b".repeat(40),
        "artifacts": {
            "constructor": "1".repeat(64), "client": "2".repeat(64),
            "carrier": "3".repeat(64), "observer": "4".repeat(64),
            "supervisor": "5".repeat(64), "helper": "6".repeat(64),
            "node": "7".repeat(64), "piGraph": "8".repeat(64),
            "policy": "9".repeat(64)
        },
        "notBeforeMs": NOW - 1000, "expiresAtMs": NOW + 10_000,
        "receiptTrust": [{ "issuer": "agenticos-native-owner",
            "kid": "attacker-receipt", "keyVersion": 1, "publicKey": public_key }],
        "grantTrust": [{ "issuer": "agenticos-native-owner",
            "kid": "attacker-grant", "keyVersion": 1, "publicKey": public_key }]
    });
    let bootstrap = json!({
        "version": 1, "type": "configure", "operation": operation,
        "barrierNonce": "11590000-0000-4000-8000-000000000002",
        "expiresAtMs": NOW + 10_000,
        "launch": { "epoch": 1, "request": {
            "challenge": operation, "image": image, "purpose": "fresh_start",
            "identity": { "company": "cad1159-company", "instance": "cad1159-instance",
                "backend": "native", "tier": "basic", "generation": 1,
                "imageLane": "baseline" }
        } },
        "imageAttestation": attacker_attestation(&attacker, &header, &payload),
        "lineage": { "reference": "cad1159-untrusted-lineage", "databaseEpoch": 1 }
    });

    // Validate the actual production types/parsers, not merely JSON syntax.
    // The schema comes from platform 6b5502e85d40174c1f5de313c10ccda24dee7bce.
    let parsed: super::Configure = serde_json::from_value(bootstrap.clone()).unwrap();
    super::grant::parse_launch(&parsed.launch).unwrap();
    super::grant::parse_lineage(&parsed.lineage).unwrap();
    let _: super::Header = super::canonical(canonical(&header).as_bytes()).unwrap();
    let _: super::Manifest = super::canonical(canonical(&payload).as_bytes()).unwrap();

    let mut embedded_key = bootstrap.clone();
    embedded_key["imageTrust"] = json!([{ "issuer": "agenticos-native-image-owner",
        "kid": "cad1159-attacker", "keyVersion": 1, "publicKey": public_key }]);

    let mut selected_process = bootstrap.clone();
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let starttime = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_ascii_whitespace()
        .nth(19)
        .unwrap();
    selected_process["installer"] = json!({
        "pid": std::process::id(), "starttime": starttime, "uid": 21000, "gid": 21000
    });
    selected_process["diagnostic"] = json!({
        "pid": std::process::id(), "securebits": 239, "keepcaps": 0,
        "capInh": 0, "capPrm": 0, "capEff": 0, "capBnd": 0, "capAmb": 0,
        "groups": [], "noNewPrivs": 1, "measured": true,
        "constructionAuthority": true, "ownerAuthority": true
    });

    let mut wrong_image = bootstrap.clone();
    wrong_image["launch"]["request"]["image"] = json!(format!(
        "registry.invalid/cadence@sha256:{}",
        "c".repeat(64)
    ));
    let mut wrong_operation = bootstrap.clone();
    wrong_operation["operation"] = json!("11590000-0000-4000-8000-000000000003");

    // Readback checks this calling task retains no newly created child. It does
    // not establish historical absence of transient spawn/reap. Final reviewers
    // must also confirm the real authentication path precedes ALL child effects.
    let children = std::fs::read_to_string("/proc/thread-self/children").unwrap();
    for (case, frame) in [
        ("well-formed self-signed untrusted build", bootstrap),
        ("caller-supplied root chooser", embedded_key),
        (
            "caller-selected PID and perfect diagnostic",
            selected_process,
        ),
        ("image different from signed manifest", wrong_image),
        ("operation different from launch challenge", wrong_operation),
    ] {
        let bytes = canonical(&frame).into_bytes();
        assert!(
            bytes.len() < 32768,
            "guard must reach bounded authentication"
        );
        let result = super::authenticate_bootstrap(&bytes, NOW);
        if case == "well-formed self-signed untrusted build" {
            // This must reach the REAL immutable-key lookup, not a syntax,
            // timeout, geteuid or child-construction refusal. No test key is
            // inserted into that lookup and no positive authority is mocked.
            match &result {
                Err(crate::Error::Rejected(message)) => assert_eq!(
                    message, "constructor image authority unconfigured or untrusted",
                    "baseline did not reach independent trust refusal"
                ),
                _ => panic!("baseline did not reach independent trust refusal"),
            }
        }
        assert!(
            result.is_err(),
            "{case} produced authenticated construction input"
        );
        assert_eq!(
            std::fs::read_to_string("/proc/thread-self/children").unwrap(),
            children,
            "{case} retained a child before bootstrap authentication"
        );
    }
}
