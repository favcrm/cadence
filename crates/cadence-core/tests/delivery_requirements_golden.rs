//! CAD-2386: golden acceptance corpus for `delivery_requirements::classify`.
//! The fixture (`tests/golden/delivery_requirements.json`) was recorded
//! from the hard-coded rules against this repository's own
//! `docs/roles/{risk-paths,one-review-paths}.toml`; the test re-reads those
//! lists, recomputes every case and asserts exact `Requirements` equality.
//! Moving the rules into role data must not change a single output.
use cadence_core::delivery::{default_policy, digest, Resolved};
use cadence_core::delivery_requirements::{
    classify, default_resolved, Change, Requirements, TrustedLists,
};
use serde_json::{json, Value};

fn resolved(kind: &str) -> Resolved {
    match kind {
        "default" => default_resolved(),
        "custom" => {
            let mut p = default_policy();
            p.max_revise += 1;
            Resolved {
                digest: digest(&p),
                policy: p,
                source: "default",
                note: None,
            }
        }
        "unapproved" => Resolved {
            note: Some("delivery_unapproved: recorder".into()),
            ..default_resolved()
        },
        "malformed" => Resolved {
            note: Some("delivery_error: recorder".into()),
            ..default_resolved()
        },
        other => panic!("unknown policy kind {other}"),
    }
}

#[test]
fn classify_matches_golden_corpus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let risk = std::fs::read_to_string(root.join("docs/roles/risk-paths.toml")).unwrap();
    let one = std::fs::read_to_string(root.join("docs/roles/one-review-paths.toml")).unwrap();
    let fx: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden/delivery_requirements.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let shapes = fx["shapes"].as_array().unwrap();
    let templates = fx["reason_templates"].as_array().unwrap();
    let cases = fx["cases"].as_array().unwrap();
    assert!(cases.len() > 300, "fixture shrank: {} cases", cases.len());

    let mut failures = Vec::new();
    for case in cases {
        let id = case[0].as_str().unwrap();
        let pol = case[1].as_str().unwrap();
        let lk = case[2].as_str().unwrap();
        let lines = case[3].as_u64().unwrap();
        let changes: Vec<Change> = case[4]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                // "status old new path" — the path may contain spaces.
                let mut f = c.as_str().unwrap().splitn(4, ' ');
                Change {
                    status: f.next().unwrap().into(),
                    old_mode: f.next().unwrap().into(),
                    new_mode: f.next().unwrap().into(),
                    path: f.next().unwrap().into(),
                }
            })
            .collect();
        let lists = match lk {
            "repo" => TrustedLists {
                risk_paths: Some(&risk),
                one_review: Some(&one),
            },
            "none" => TrustedLists {
                risk_paths: None,
                one_review: None,
            },
            "risk_none" => TrustedLists {
                risk_paths: None,
                one_review: Some(&one),
            },
            "one_none" => TrustedLists {
                risk_paths: Some(&risk),
                one_review: None,
            },
            "garbage" => TrustedLists {
                risk_paths: Some("not = [toml"),
                one_review: Some("x"),
            },
            other => panic!("unknown lists kind {other}"),
        };
        let r = resolved(pol);
        assert_eq!(
            r.digest,
            fx["digests"][pol].as_str().unwrap(),
            "digest for policy {pol}"
        );
        let sh = &shapes[case[5].as_u64().unwrap() as usize];
        let reasons: Vec<Value> = case[6]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| match x.as_u64() {
                Some(i) => templates[i as usize].clone(),
                None => x.clone(),
            })
            .collect();
        let expected: Requirements = serde_json::from_value(json!({
            "class": sh[0], "reviews": sh[1], "review_kind": sh[2], "security_capable": sh[3],
            "browser_qa": sh[4], "operator_approval": sh[5], "full_checks": sh[6],
            "reasons": reasons, "policy_digest": fx["digests"][pol], "activation": sh[7],
        }))
        .unwrap();
        let got = classify(&r, &changes, lines, &lists);
        if got != expected {
            failures.push(format!(
                "{id} [{pol}/{lk}/{lines}]\n  want {expected:?}\n  got  {got:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} golden cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
