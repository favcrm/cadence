//! Stable needs-row ordering and aggregation by subject.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::Item;

/// Urgency order: kind rank ascending, then oldest first inside a kind.
pub(super) fn sort_needs(needs: &mut [Item]) {
    needs.sort_by(|a, b| a.rank.cmp(&b.rank).then(b.age.cmp(&a.age)));
}

/// One row per subject (CAD-252): rows naming the same agent, issue or
/// PR collapse into the most severe one (lowest rank; ties keep emit
/// order), which lists every cause most severe first under `causes`.
/// `kind`/`cause` stay the primary cause, so a consumer that predates
/// `causes` still reads one sensible row. The CLI and the board both
/// render this merged list — the merge lives here and nowhere else.
pub(super) fn merge_by_subject(items: Vec<Item>) -> Vec<Item> {
    let mut order: Vec<(&'static str, String)> = Vec::new();
    let mut groups: HashMap<(&'static str, String), Vec<Item>> = HashMap::new();
    for it in items {
        if !groups.contains_key(&it.subject) {
            order.push(it.subject.clone());
        }
        groups.entry(it.subject.clone()).or_default().push(it);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let mut group = groups.remove(&key)?;
            group.sort_by_key(|i| i.rank);
            let causes: Vec<Value> = group
                .iter()
                .map(|i| {
                    json!({
                        "cause": i.json["kind"], "title": i.json["title"],
                        "age": i.age, "command": i.json["command"],
                        "audience": i.json["audience"], "since": i.since,
                    })
                })
                .collect();
            let mut agents: Vec<String> = group.iter().flat_map(|i| i.agents.clone()).collect();
            agents.sort();
            agents.dedup();
            // The row is for whoever its most urgent cause is for — a
            // team primary with an escalated cause is the operator's.
            let urgent = group.iter().min_by_key(|i| i.audience)?;
            let (audience, reason) = (urgent.audience, urgent.json["audience_reason"].clone());
            let mut primary = group.into_iter().next()?;
            primary.json["causes"] = json!(causes);
            primary.agents = agents;
            if primary.audience != audience {
                primary.audience = audience;
                primary.json["audience"] = json!(audience.as_str());
                primary.json["audience_reason"] = reason;
            }
            Some(primary)
        })
        .collect()
}
