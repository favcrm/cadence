//! Unconsumed-inbox policy (CAD-251): when is a mailbox "stale", who
//! owns it, and the warning text a sender or the overview sees.
//!
//! Pure functions over the store's consumer evidence
//! ([`crate::store::Store::inbox_consumer`]) — the daemon attaches the
//! result to `agent_list` rows and `agent_send` receipts, and records
//! `inbox_unconsumed` events for routed deliveries. Nothing here
//! refuses, drops or expires a message: a stale inbox keeps every row
//! until its consumer drains it.

use serde_json::{json, Value};

/// Warn above this many unread messages (`inbox_warn_unread`).
pub const WARN_UNREAD: u64 = 50;
/// …when no `inbox_read` landed within this window (`inbox_warn_idle_secs`).
pub const WARN_IDLE_SECS: u64 = 86_400;
/// The owner of an inbox that is its own group root.
pub const OPERATOR: &str = "operator";

/// The per-inbox thresholds — defaults overridden by the agent's
/// `inbox_warn_unread` / `inbox_warn_idle_secs` params.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub unread: u64,
    pub idle_secs: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            unread: WARN_UNREAD,
            idle_secs: WARN_IDLE_SECS,
        }
    }
}

/// A non-negative integer param — JSON number or its decimal string
/// (`agent set k=v` stores strings).
fn param_u64(params: Option<&Value>, key: &str) -> Option<u64> {
    let v = params?.get(key)?;
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

impl Policy {
    pub fn from_params(params: Option<&Value>) -> Self {
        let d = Self::default();
        Self {
            unread: param_u64(params, "inbox_warn_unread").unwrap_or(d.unread),
            idle_secs: param_u64(params, "inbox_warn_idle_secs").unwrap_or(d.idle_secs),
        }
    }
}

/// The inbox's owner: the root of its `params.upstream` chain, or
/// [`OPERATOR`] when the inbox is its own root. `upstream_of` answers
/// one hop; a cycle or an over-long chain stops at the last alias seen.
pub fn owner_of(alias: &str, upstream_of: impl Fn(&str) -> Option<String>) -> String {
    let mut seen = vec![alias.to_string()];
    let mut current = alias.to_string();
    while let Some(up) = upstream_of(&current) {
        if seen.contains(&up) || seen.len() > 16 {
            break;
        }
        seen.push(up.clone());
        current = up;
    }
    if current == alias {
        OPERATOR.to_string()
    } else {
        current
    }
}

/// `42s`, `13m`, `5h`, `2d`.
pub fn fmt_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// The health block for one inbox. `consumer` is the store evidence
/// (`unread`, `oldest_unread_at`, `last_received_at`, `last_read_at`).
/// Idle time runs from the last `inbox_read`, else from the oldest
/// unread message — a mailbox nobody ever read goes stale once its
/// backlog is older than the window. Stale = unread above the
/// threshold AND idle at least the window.
pub fn health(alias: &str, consumer: &Value, policy: Policy, owner: &str, now: f64) -> Value {
    let unread = consumer["unread"].as_u64().unwrap_or(0);
    let age = |at: Option<f64>| at.map(|t| (now - t).max(0.0) as u64);
    let oldest_age = age(consumer["oldest_unread_at"].as_f64());
    let last_read_at = consumer["last_read_at"].as_f64();
    let idle = age(last_read_at).or(oldest_age).unwrap_or(0);
    let stale = unread > policy.unread && idle >= policy.idle_secs;
    let warning = stale.then(|| {
        let read = match last_read_at {
            Some(_) => format!("no inbox_read for {}", fmt_age(idle)),
            None => "never read".to_string(),
        };
        format!(
            "inbox '{alias}' has no consumer: {unread} unread (oldest {}), {read}; \
             owner {owner}. The message is queued, not dropped — drain with \
             `cadence inbox {alias}`",
            fmt_age(oldest_age.unwrap_or(0)),
        )
    });
    json!({
        "unread": unread,
        "oldest_unread_age_secs": oldest_age,
        "last_read_at": last_read_at,
        "last_received_at": consumer["last_received_at"],
        "idle_secs": idle,
        "threshold": {"unread": policy.unread, "idle_secs": policy.idle_secs},
        "stale": stale,
        "owner": owner,
        "warning": warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn consumer(unread: u64, oldest: Option<f64>, last_read: Option<f64>) -> Value {
        json!({"unread": unread, "oldest_unread_at": oldest,
               "last_received_at": oldest, "last_read_at": last_read})
    }

    #[test]
    fn policy_reads_numbers_and_strings() {
        assert_eq!(Policy::from_params(None), Policy::default());
        let p = Policy::from_params(Some(
            &json!({"inbox_warn_unread": "5", "inbox_warn_idle_secs": 60}),
        ));
        assert_eq!(
            p,
            Policy {
                unread: 5,
                idle_secs: 60
            }
        );
        // Garbage falls back to the default rather than disabling.
        let p = Policy::from_params(Some(&json!({"inbox_warn_unread": "many"})));
        assert_eq!(p.unread, WARN_UNREAD);
    }

    #[test]
    fn stale_needs_backlog_and_idle_window() {
        let p = Policy {
            unread: 2,
            idle_secs: 100,
        };
        let now = 10_000.0;
        // Over threshold, never read, backlog older than the window.
        let h = health(
            "obs",
            &consumer(3, Some(now - 200.0), None),
            p,
            "operator",
            now,
        );
        assert_eq!(h["stale"], true, "{h}");
        let w = h["warning"].as_str().unwrap();
        assert!(
            w.contains("obs") && w.contains("3 unread") && w.contains("never read"),
            "{w}"
        );
        // Read recently: idle runs from the read, not the backlog.
        let h = health(
            "obs",
            &consumer(3, Some(now - 200.0), Some(now - 10.0)),
            p,
            "pm",
            now,
        );
        assert_eq!(h["stale"], false, "{h}");
        assert!(h["warning"].is_null());
        // At the threshold is not above it.
        let h = health("obs", &consumer(2, Some(now - 200.0), None), p, "pm", now);
        assert_eq!(h["stale"], false, "{h}");
        // Young backlog on a never-read inbox is not stale yet.
        let h = health("obs", &consumer(9, Some(now - 5.0), None), p, "pm", now);
        assert_eq!(h["stale"], false, "{h}");
        assert_eq!(h["oldest_unread_age_secs"], 5);
    }

    #[test]
    fn owner_follows_upstream_chain() {
        let up = |a: &str| match a {
            "inbox" => Some("pm".to_string()),
            "pm" => Some("root".to_string()),
            "loop-a" => Some("loop-b".to_string()),
            "loop-b" => Some("loop-a".to_string()),
            _ => None,
        };
        assert_eq!(owner_of("inbox", up), "root");
        assert_eq!(owner_of("root", up), OPERATOR);
        // A cycle stops instead of spinning.
        assert_eq!(owner_of("loop-a", up), "loop-b");
    }
}
