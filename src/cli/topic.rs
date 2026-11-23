//! CAD-1412: `cadence topic` — an agent reads and replies in a human room it
//! is a member of. Reads require the daemon to identify the cursor format;
//! publishes forward to the daemon. The daemon derives the caller from the
//! connection (the agent's own pane); this module adds no authority, no
//! create, subscribe or request verb.

use super::*;

#[derive(Subcommand)]
pub(crate) enum TopicAction {
    /// Read one page of a topic you are a member of (oldest first).
    /// `--before <cursor>` pages back; `--limit` is 1-100.
    Read {
        /// Opaque topic id, as named in the Ask instruction.
        topic: String,
        /// Opaque backward cursor, or a proven operator's numeric position.
        #[arg(long)]
        before: Option<String>,
        /// Page size (1-100, default 50).
        #[arg(long)]
        limit: Option<i64>,
    },
    /// Post a reply in a topic you are a member of.
    Publish {
        /// Opaque topic id, as named in the Ask instruction.
        topic: String,
        /// Reply body.
        text: String,
        /// Idempotency key; a retry with the same id and text is a duplicate.
        #[arg(long = "id")]
        client_id: Option<String>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: TopicAction) -> Result<i32> {
    match action {
        TopicAction::Read {
            topic,
            before,
            limit,
        } => {
            let mut params = json!({
                "topic": topic,
                "cursor_format": cadence_core::board_api::TOPIC_CURSOR_FORMAT,
            });
            if let Some(before) = before {
                params["before"] = match before.parse::<i64>() {
                    Ok(position) => json!(position),
                    Err(_) => json!(before),
                };
            }
            if let Some(limit) = limit {
                params["limit"] = json!(limit);
            }
            let page = client::rpc(&state_dir, "topic_read", params)?;
            match page[cadence_core::board_api::READER_SCOPE_FIELD].as_str() {
                Some(cadence_core::board_api::TOPIC_OPERATOR_SCOPE_ACK) => {
                    if !page["cursor"].is_i64() || !page["posts"].is_array() {
                        return Err(Error::rejected("topic read: malformed operator page"));
                    }
                }
                Some(cadence_core::board_api::TOPIC_SCOPE_ACK) => {
                    let opaque = |value: Option<&Value>| {
                        matches!(value, Some(Value::Null) | Some(Value::String(_)))
                    };
                    let rows = page["posts"].as_array().ok_or_else(|| {
                        Error::rejected("topic read: scoped page has no posts")
                    })?;
                    if page["cursor_format"].as_str() != Some(cadence_core::board_api::TOPIC_CURSOR_FORMAT)
                        || !opaque(page.get("cursor"))
                        || !opaque(page.get("before"))
                        || rows.iter().any(|row| {
                            row.get("seq").is_some()
                                || row["id"].as_str().is_none_or(str::is_empty)
                        })
                    {
                        return Err(Error::rejected(
                            "topic read: invalid scoped page; no posts were printed",
                        ));
                    }
                }
                _ => return Err(Error::rejected(
                    "topic read: the daemon did not confirm a scoped or operator page; no posts were printed",
                )),
            }
            print_json(&page);
            Ok(0)
        }
        TopicAction::Publish {
            topic,
            text,
            client_id,
        } => {
            let id = client_id.unwrap_or_else(|| {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default();
                format!("reply-{nanos}")
            });
            let params = json!({"topic": topic, "text": text, "id": id});
            print_json(&client::rpc(&state_dir, "topic_publish", params)?);
            Ok(0)
        }
    }
}
