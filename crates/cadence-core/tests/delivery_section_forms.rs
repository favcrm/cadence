//! CAD-2708: every stored hosted `delivery:` section form in the shared
//! fixture resolves, through `section_front`, to the owner-approved digest
//! it names, or to no policy when it names none. The hosted service runs a
//! byte-identical copy of the fixture through its own wrapper and pinned
//! core, so the CLI and the hosted gate read the same policy from the same
//! stored text.
use cadence_core::delivery::{digest, parse, section_front};
use serde_json::Value;

#[test]
fn every_stored_form_resolves_to_its_fixture_digest() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/delivery_section_forms.json"
    );
    let fixture: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let forms = fixture["forms"].as_array().unwrap();
    assert!(forms.len() >= 10, "fixture went missing");
    for form in forms {
        let name = form["name"].as_str().unwrap();
        let text = form["text"].as_str().unwrap();
        let got = parse(&section_front(text))
            .ok()
            .flatten()
            .map(|policy| digest(&policy));
        assert_eq!(got.as_deref(), form["digest"].as_str(), "{name}");
    }
}

/// CAD-2715: the core op wraps `policy_section` itself. For every stored form
/// it answers byte-for-byte as it does for `policy: section_front(text)`,
/// with and without the owner flag, so a host can send the stored text as it
/// is and keep no wrapper of its own.
#[test]
fn policy_section_answers_as_the_wrapped_policy_for_every_stored_form() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/delivery_section_forms.json"
    );
    let fixture: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let evaluate = |policy: Value, section: Value, owner: bool| {
        let request = serde_json::json!({"v": 1, "op": "delivery.requirements.evaluate", "input": {
            "policy": policy, "policy_section": section, "policy_owner_approved": owner,
            "changes": [{"status": "M", "old_mode": "100644", "new_mode": "100644",
                         "path": "src/components/nav/SiteNav.tsx"}],
            "lines": 12, "lists_absent": true}});
        cadence_core::evaluate(&serde_json::to_vec(&request).unwrap())
    };
    for form in fixture["forms"].as_array().unwrap() {
        let name = form["name"].as_str().unwrap();
        let text = form["text"].as_str().unwrap();
        for owner in [false, true] {
            let wrapped = evaluate(Value::from(section_front(text)), Value::Null, owner);
            let section = evaluate(Value::Null, Value::from(text), owner);
            let answer: Value = serde_json::from_slice(&section).unwrap();
            assert_eq!(answer["ok"], true, "{name} owner={owner}: {answer}");
            if owner {
                if let Some(digest) = form["digest"].as_str() {
                    assert_eq!(answer["value"]["policy_source"], "approved", "{name}");
                    assert_eq!(answer["value"]["policy_digest"], digest, "{name}");
                }
            }
            assert_eq!(
                String::from_utf8(section).unwrap(),
                String::from_utf8(wrapped).unwrap(),
                "{name} owner={owner}"
            );
        }
    }
}
