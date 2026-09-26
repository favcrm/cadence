//! CAD-535: `cadence events` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    alias: Option<String>,
    job: Option<String>,
    after: Option<i64>,
    wait: u64,
    follow: bool,
) -> Result<i32> {
    let (method, key) = match (alias, job) {
        (Some(a), None) => ("agent_events", json!({"alias": a})),
        (None, Some(j)) => ("job_events", json!({"job": j})),
        (Some(_), Some(_)) => {
            return Err(Error::rejected("events takes an alias or --job, not both"))
        }
        (None, None) => return Err(Error::rejected("events needs an alias or --job <job>")),
    };
    // No --after: the default page is the newest 50 (oldest
    // first within it). --follow anchors there too — history
    // below the page is a `has_older` flag, not a flood.
    let mut cursor = match after {
        Some(cursor) => cursor,
        None => {
            let mut req = key.clone();
            req["tail"] = json!(true);
            let page = client::rpc(&state_dir, method, req)?;
            print_json(&page);
            if !follow {
                return Ok(0);
            }
            page.get("cursor").and_then(Value::as_i64).unwrap_or(0)
        }
    };
    loop {
        let mut req = key.clone();
        req["after"] = json!(cursor);
        req["wait"] = json!(if follow { 25 } else { wait });
        let page = client::rpc(&state_dir, method, req)?;
        let empty = page
            .get("events")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        if !empty || !follow {
            print_json(&page);
        }
        cursor = page.get("cursor").and_then(Value::as_i64).unwrap_or(cursor);
        if !follow {
            return Ok(0);
        }
    }
}
