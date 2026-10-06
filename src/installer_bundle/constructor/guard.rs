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
    super::grant::parse_launch(&serde_json::to_value(&parsed.launch).unwrap()).unwrap();
    super::grant::parse_lineage(&serde_json::to_value(&parsed.lineage).unwrap()).unwrap();
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

    // Additive four-P2 UNIT checks, not a new Root issuer or native baseline.
    // The SAME production ingress helper runs before Root task/history effects.
    let longest = "a".repeat(64);
    for valid in ["a", "worker-9", longest.as_str()] {
        super::dispatcher::require_task_alias(valid).unwrap();
    }
    let too_long = "a".repeat(65);
    for invalid in [
        "",
        "合成研究員🔎",
        too_long.as_str(),
        "Upper",
        "two words",
        "under_score",
        "-leading",
    ] {
        match super::dispatcher::require_task_alias(invalid) {
            Err(crate::Error::OutcomeUnknown(message)) => {
                assert_eq!(message, "fixed installer UNKNOWN — no authority or retry")
            }
            _ => panic!("incompatible alias bypassed real Root pre-effect identifier guard"),
        }
        assert_eq!(
            std::fs::read_to_string("/proc/thread-self/children").unwrap(),
            children,
            "alias UNIT probe retained a child"
        );
    }

    // These public Binding selectors are DATA ONLY. No StoreOwnerGrant,
    // authenticated opening, runtime, kernel/current Proof or issuer is made.
    // Call the actual ordinary-current SQL predicate, not copied SQL logic.
    let binding = crate::store::Binding {
        database_id: "unit-database".into(),
        incarnation: "unit-incarnation".into(),
        database_epoch: 7,
        operation: "unit-operation".into(),
        purpose: crate::store::Purpose::Open,
        path: "/srv/cadence/protected/store/cadence.db".into(),
        challenge: vec![1; 32],
        attempt: "unit-attempt".into(),
        artifact: "unit-artifact".into(),
        deadline_unix: 1, // Expired DATA is deliberately NOT an activation permit.
        source: None,
    };
    let sql_dir = tempfile::tempdir().unwrap();
    for (case, setup, matching) in [
        ("matching", "", true),
        (
            "stale-positive-latch-epoch",
            "UPDATE closure_state SET epoch=8 WHERE id=1",
            false,
        ),
        (
            "zero-latch-epoch",
            "UPDATE closure_state SET epoch=0 WHERE id=1",
            false,
        ),
        (
            "extra-latch",
            "INSERT INTO closure_state VALUES(2,0,0,7)",
            false,
        ),
        ("missing-latch", "DELETE FROM closure_state", false),
    ] {
        let path = sql_dir.path().join(format!("{case}.db"));
        let fixture = rusqlite::Connection::open(&path).unwrap();
        // Ordinary malformed SQL datasets deliberately allow id=2 so the
        // GLOBAL cardinality guard cannot hide an extra row behind WHERE id=1.
        // Hold the setup connection (without further writes) to retain real
        // committed WAL pages; the comparison reader is genuinely READ_ONLY.
        fixture.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE schema_version(version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES(32);
             CREATE TABLE store_incarnation(database_id TEXT NOT NULL,incarnation TEXT NOT NULL,
                 epoch INTEGER NOT NULL,operation TEXT NOT NULL);
             INSERT INTO store_incarnation VALUES('unit-database','unit-incarnation',7,'unit-operation');
             CREATE TABLE closure_state(id INTEGER PRIMARY KEY,closed INTEGER NOT NULL,
                 witness_done INTEGER NOT NULL,epoch INTEGER NOT NULL);
             INSERT INTO closure_state VALUES(1,0,0,7);",
        ).unwrap();
        if !setup.is_empty() {
            fixture.execute_batch(setup).unwrap();
        }
        assert!(
            std::fs::metadata(format!("{}-wal", path.display()))
                .unwrap()
                .len()
                > 0,
            "fixture did not retain committed WAL pages"
        );
        let reader = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert!(reader.is_readonly(rusqlite::DatabaseName::Main).unwrap());
        reader.execute_batch("BEGIN").unwrap(); // SAME snapshot shape as real source().
        let before_version: i64 = reader
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        let before_changes: i64 = reader
            .query_row("SELECT total_changes()", [], |r| r.get(0))
            .unwrap();
        // SQL NULL here records actual absence, not a default-open latch.
        let before_latch: Option<String> = reader.query_row(
            "SELECT group_concat(id || ':' || closed || ':' || witness_done || ':' || epoch,';')
             FROM (SELECT * FROM closure_state ORDER BY id)", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(before_changes, 0);
        // Mandatory common preceding predicates match even for every negative.
        let identity: (String, String, i64, String) = reader
            .query_row(
                "SELECT database_id,incarnation,epoch,operation FROM store_incarnation",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            identity,
            (
                binding.database_id.clone(),
                binding.incarnation.clone(),
                7,
                binding.operation.clone()
            )
        );
        assert_eq!(
            reader
                .query_row("SELECT version FROM schema_version", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            32
        );
        if case == "missing-latch" {
            assert!(before_latch.is_none(), "missing-latch setup retained a row");
            assert_eq!(
                reader
                    .query_row("SELECT count(*) FROM closure_state", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        } else {
            assert!(before_latch.is_some(), "non-missing dataset lost its latch");
            assert_eq!(
                reader
                    .query_row(
                        "SELECT closed,witness_done FROM closure_state WHERE id=1",
                        [],
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                    )
                    .unwrap(),
                (0, 0)
            );
        }

        let result = super::capture::check_database_current(&reader, &binding);
        if matching {
            result.expect("matching READ_ONLY SQL DATA baseline refused");
        } else {
            match result {
                Err(crate::Error::OutcomeUnknown(message)) => assert_eq!(
                    message, "fixed installer UNKNOWN — no authority or retry",
                    "{case} refused at a different boundary"
                ),
                _ => panic!("{case} bypassed actual ordinary-current latch predicate"),
            }
        }
        assert!(reader.is_readonly(rusqlite::DatabaseName::Main).unwrap());
        assert_eq!(
            reader
                .query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            before_changes,
            "{case} wrote through comparison reader"
        );
        assert_eq!(
            reader
                .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            before_version,
            "{case} committed a fixture repair"
        );
        assert_eq!(reader.query_row(
            "SELECT group_concat(id || ':' || closed || ':' || witness_done || ':' || epoch,';')
             FROM (SELECT * FROM closure_state ORDER BY id)", [], |r| r.get::<_,Option<String>>(0),
        ).unwrap(), before_latch, "{case} changed actual latch rows");
        assert_eq!(
            std::fs::read_to_string("/proc/thread-self/children").unwrap(),
            children
        );
    }
}
