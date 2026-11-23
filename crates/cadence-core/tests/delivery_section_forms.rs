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
