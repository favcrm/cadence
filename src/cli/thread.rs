//! CAD-535: `cadence thread` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum ThreadAction {
    /// Print the thread's entries (oldest first) and the cursor to
    /// continue from — for debugging the chat.
    Show {
        /// Agent alias or provider-native id.
        alias: String,
        /// Opaque continuation, or a proven operator's numeric position.
        #[arg(long)]
        after: Option<String>,
        /// Page size (1-500).
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
}

pub(super) fn run(state_dir: PathBuf, action: ThreadAction) -> Result<i32> {
    match action {
        ThreadAction::Show {
            alias,
            after,
            limit,
        } => {
            let mut params = json!({
                "alias": alias, "limit": limit,
                "cursor_format": cadence_core::board_api::THREAD_CURSOR_FORMAT,
            });
            if let Some(raw) = &after {
                if raw.is_empty() {
                    return Err(Error::rejected("thread show: --after cannot be empty"));
                }
                params["after"] = match raw.parse::<i64>() {
                    Ok(position) => json!(position),
                    Err(_) => json!(raw),
                };
            }
            let page = validated_thread_page(
                client::rpc(&state_dir, "thread_read", params)?,
                after.as_deref(),
            )?;
            print_json(&page);
            Ok(0)
        }
    }
}

/// Never print a page whose daemon has not confirmed its reader scope.
fn validated_thread_page(mut page: Value, after: Option<&str>) -> Result<Value> {
    let ack = page
        .as_object_mut()
        .and_then(|object| object.remove(cadence_core::board_api::READER_SCOPE_FIELD));
    match ack.as_ref().and_then(Value::as_str) {
        Some(cadence_core::board_api::THREAD_OPERATOR_SCOPE_ACK) => {
            if !page["cursor"].is_i64() || !page["entries"].is_array() {
                return Err(Error::rejected("thread show: malformed operator page"));
            }
        }
        Some(cadence_core::board_api::THREAD_OPAQUE_SCOPE_ACK) => {
            let rows = page["entries"].as_array().ok_or_else(|| {
                Error::rejected("thread show: scoped page has no entries")
            })?;
            let opaque = |value: Option<&Value>| match value {
                Some(Value::Null) => true,
                Some(Value::String(token)) => token.strip_prefix("csc1_").is_some_and(|body| !body.is_empty()),
                _ => false,
            };
            if page.get("reply_seq").is_some() || page.get("scanned_before").is_some()
                || page["cursor_format"].as_str() != Some(cadence_core::board_api::THREAD_CURSOR_FORMAT)
                || !opaque(page.get("cursor")) || !opaque(page.get("before_cursor"))
                || page["more_after"].as_bool().is_none()
                || rows.iter().any(|row| row.get("seq").is_some()
                    || row["id"].as_str().is_none_or(|id| id.strip_prefix("csrid_").is_none_or(str::is_empty)))
                || (!rows.is_empty() && (page["cursor"].is_null() || page["before_cursor"].is_null()))
                || (rows.is_empty() && page["cursor"].as_str() != after)
            {
                return Err(Error::rejected("thread show: invalid scoped page; no entries were printed"));
            }
        }
        _ => return Err(Error::rejected(
            "thread show: the daemon did not confirm a scoped or operator page; no entries were printed",
        )),
    }
    Ok(page)
}
#[cfg(test)]
mod cad2215_direct_page_acceptance {
    use super::*;

    const MARKER: &str = "CAD2215_CLI_UNTRUSTED_ENTRY";
    const OPAQUE_ACK: &str = "cad2215.thread.opaque.v1";
    const OPERATOR_ACK: &str = "cad2215.thread.operator.v1";

    fn scoped_page() -> Value {
        json!({
            "alias":"worker", "thread":null, "cursor_format":"opaque-v1",
            "cursor":"csc1_valid-shape", "before_cursor":"csc1_valid-backward",
            "more_after":false, "more_before":null,
            "entries":[{"id":"csrid_stable", "thread":"t", "role":"agent",
                        "kind":"message", "text":MARKER, "payload":null,
                        "message":null, "created":1.0}],
            "_reader_scope":OPAQUE_ACK,
        })
    }

    fn operator_page() -> Value {
        json!({
            "alias":"worker", "thread":null, "cursor":13, "reply_seq":13,
            "more_after":false,
            "entries":[{"seq":13,"thread":"t","role":"agent","kind":"message",
                        "text":MARKER,"payload":null,"message":null,"created":1.0}],
            "_reader_scope":OPERATOR_ACK,
        })
    }

    #[test]
    fn refuses_unproven_or_malformed_pages_before_printing() {
        let page = scoped_page();
        let mut missing = page.clone();
        missing.as_object_mut().unwrap().remove("_reader_scope");
        let mut wrong = page.clone();
        wrong["_reader_scope"] = json!("old-daemon");
        let mut numeric_cursor = page.clone();
        numeric_cursor["cursor"] = json!(13);
        let mut numeric_row = page.clone();
        numeric_row["entries"][0]["seq"] = json!(13);
        let mut malformed_entries = page.clone();
        malformed_entries["entries"] = json!({"text":MARKER});
        let mut missing_backward = page.clone();
        missing_backward
            .as_object_mut()
            .unwrap()
            .remove("before_cursor");
        let mut empty_token = page.clone();
        empty_token["cursor"] = json!("csc1_");
        let mut empty_id = page.clone();
        empty_id["entries"][0]["id"] = json!("csrid_");
        for (name, reply) in [
            ("missing ack", missing),
            ("wrong ack", wrong),
            ("numeric scoped cursor", numeric_cursor),
            ("numeric scoped row", numeric_row),
            ("malformed entries", malformed_entries),
            ("missing backward cursor", missing_backward),
            ("empty opaque token", empty_token),
            ("empty opaque row id", empty_id),
        ] {
            let result = validated_thread_page(reply, None);
            assert!(result.is_err(), "{name} produced a page: {result:?}");
            assert!(
                !format!("{result:?}").contains(MARKER),
                "{name} leaked untrusted entry through error"
            );
        }
    }

    #[test]
    fn accepts_confirmed_operator_and_scoped_pages_without_internal_ack() {
        for (name, reply) in [
            ("operator numeric", operator_page()),
            ("scoped opaque", scoped_page()),
        ] {
            let page = validated_thread_page(reply, None)
                .unwrap_or_else(|error| panic!("{name} refused: {error}"));
            assert_eq!(page["entries"][0]["text"], MARKER);
            assert!(
                page.get("_reader_scope").is_none(),
                "internal acknowledgement leaked"
            );
        }
    }
}
