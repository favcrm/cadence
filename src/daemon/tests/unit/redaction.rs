use super::*;

#[test]
fn redact_tokens_withholds_values_and_quotes() {
    let tok = "pty-g1-0123456789abcdef";
    let short = "fake-turn-1";
    let mut v = json!({
        "agent": {"awaiting_report": {"turn_id": tok, "message": "m1"}},
        "events": [{"payload": {"turn_id": tok}}, {"text": format!("token {tok} here")}],
        "other": "pty-g1-0123456789abcdeX",
        "fake": short,
        "later": "fake-turn-10",
    });
    redact_tokens(&mut v, &[tok, short]);
    assert!(v["agent"]["awaiting_report"]["turn_id"].is_null(), "{v}");
    assert_eq!(v["agent"]["awaiting_report"]["message"], "m1");
    assert!(v["events"][0]["payload"]["turn_id"].is_null(), "{v}");
    assert_eq!(v["events"][1]["text"], "token [turn token withheld] here");
    assert_eq!(v["other"], "pty-g1-0123456789abcdeX");
    // A short token is withheld only as a `turn_id` value — the
    // same text elsewhere is someone else's data.
    assert_eq!(v["fake"], short, "{v}");
    assert_eq!(v["later"], "fake-turn-10");
    let mut row = json!({"id": "t-1", "turn_id": "t-1"});
    redact_tokens(&mut row, &["t-1"]);
    assert_eq!(row, json!({"id": "t-1", "turn_id": null}));
    // An id equal to a (long) token's text is still an id: only the
    // token-bearing field and prose change (review, CI 35936820152).
    let mut row = json!({"id": tok, "message": tok, "message_id": tok,
                             "entries": [{"message": tok, "text": format!("see {tok}")}],
                             "turn_id": tok});
    redact_tokens(&mut row, &[tok]);
    assert_eq!(row["id"], tok);
    assert_eq!(row["message"], tok);
    assert_eq!(row["message_id"], tok);
    assert_eq!(row["entries"][0]["message"], tok);
    assert_eq!(row["entries"][0]["text"], "see [turn token withheld]");
    assert!(row["turn_id"].is_null(), "{row}");
    // Prose under an id-named key is still masked (a Devin cloud
    // event carries provider text as `message`).
    let mut ev = json!({"event_id": "e1", "message": format!("done, token is {tok}")});
    redact_tokens(&mut ev, &[tok]);
    assert_eq!(ev["message"], "done, token is [turn token withheld]");
    let all = withhold_all_turn_ids(json!({"m": [{"turn_id": "x", "id": "m1"}]}));
    assert!(all["m"][0]["turn_id"].is_null() && all["m"][0]["id"] == "m1");
}

#[test]
fn redact_tokens_withholds_the_wait_turn_key() {
    let tok = "pty-g1-0123456789abcdef";
    let short = "fake-turn-1";
    let mut v = json!({"alias": "w", "reason": "approval_pending",
                           "message": "m1", "turn": tok});
    redact_tokens(&mut v, &[tok]);
    assert!(v["turn"].is_null(), "{v}");
    assert_eq!(v["message"], "m1");
    let mut v = json!({"turn": short});
    redact_tokens(&mut v, &[short]);
    assert!(v["turn"].is_null(), "{v}");
    // A non-token string under `turn` is untouched.
    let mut v = json!({"turn": "idle"});
    redact_tokens(&mut v, &[tok, short]);
    assert_eq!(v["turn"], "idle");
    // Objects still recurse: prose inside is masked.
    let mut v = json!({"turn": {"state": "working",
                                      "summary": format!("did {tok} ok")}});
    redact_tokens(&mut v, &[tok]);
    assert_eq!(v["turn"]["summary"], "did [turn token withheld] ok");
    let all = withhold_all_turn_ids(json!({"turn": "x", "turn_id": "y"}));
    assert!(all["turn"].is_null() && all["turn_id"].is_null(), "{all}");
}
