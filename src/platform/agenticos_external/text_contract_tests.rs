use super::*;
use serde_json::json;

#[test]
fn text_generation_contract_is_closed_and_bounded() {
    let input = json!({"messages":[{"role":"system","content":"Keep claims grounded."},{"role":"user","content":"Draft a caption."}]});
    let clean = validate_text_input(&input).unwrap();
    assert_eq!(clean["model"], "z-ai/glm-5.3-flash");
    assert_eq!(clean["maxOutputTokens"], 4096);
    for bad in [
        json!({"messages":[{"role":"assistant","content":"x"}]}),
        json!({"messages":[{"role":"user","content":"x","tools":[]}]}),
        json!({"messages":[{"role":"user","content":"x"}],"query":{}}),
        json!({"messages":[{"role":"user","content":"x"}],"model":"other"}),
        json!({"messages":[{"role":"user","content":"x"}],"maxOutputTokens":8193}),
    ] {
        assert!(validate_text_input(&bad).is_err());
    }
    let good = json!({"text":"A grounded social caption.","finishReason":"stop","usage":{"inputTokens":9,"outputTokens":5,"totalTokens":14,"cachedInputTokens":null,"reasoningOutputTokens":null}});
    assert_eq!(validate_text_result(&good).unwrap(), good);
    assert!(validate_text_result(
        &json!({"text":"x","finishReason":"stop","usage":null,"providerCost":1})
    )
    .is_err());
    assert!(validate_text_result(&json!({"text":"","finishReason":"stop","usage":null})).is_err());
}

#[test]
fn text_tool_is_available_only_under_actual_manifest_v4_pin() {
    let v3 = AgenticosExternalAdapter::with_deployment_pin(
        "https://api.example.test",
        Some(MANIFEST_PIN),
    )
    .unwrap();
    assert_eq!(v3.table().manifest_version.as_deref(), Some(MANIFEST_PIN));
    assert!(!v3.table().tools.iter().any(|t| t.tool == TEXT_TOOL));
    let v4 = AgenticosExternalAdapter::with_deployment_pin(
        "https://api.example.test",
        Some(TEXT_MANIFEST_PIN),
    )
    .unwrap();
    assert_eq!(
        v4.table().manifest_version.as_deref(),
        Some(TEXT_MANIFEST_PIN)
    );
    assert!(v4.table().tools.iter().any(|t| t.tool == TEXT_TOOL));
    v4.connection_descriptor()
        .unwrap()
        .validate(v4.table())
        .unwrap();
}
