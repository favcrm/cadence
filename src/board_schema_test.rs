use serde_json::{json, Value};

pub(crate) fn assert_matches(file: &str, def: &str, producer: Value) {
    let path = crate::contract_fixture::workspace_root()
        .join("contracts/board-api/v1")
        .join(file);
    let doc: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    // Close every typed object so an unknown producer field is refused.
    let mut defs = doc["$defs"].clone();
    for d in defs.as_object_mut().unwrap().values_mut() {
        if d.get("properties").is_some() {
            d["additionalProperties"] = json!(false);
        }
    }
    let schema = json!({"$ref": format!("#/$defs/{def}"), "$defs": defs});
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator.iter_errors(&producer).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{def} drifted from its producer: {errors:?}\n{producer}");
}
