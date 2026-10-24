//! Delegated digest (CAD-918).

use super::*;

/// A delegated approval: its action, recorded by the delegated verb
/// (`recorded_via: delegated:<alias>`) — never the operator's generic one.
pub(super) fn is_delegated(a: &ApprovalRec) -> bool {
    a.action == crate::delegation::DELEGATED_ACTION
        && a.recorded_via
            .as_deref()
            .is_some_and(|v| v.starts_with("delegated:"))
}

/// `cadence audit digest [--since 24h]`: every delegated approval in
/// the window, each with the commands that undo it — revoke the record,
/// and revert the merge if it landed. Read-only over the daemon store.
pub fn digest(state_dir: &Path, since: Option<&str>) -> Result<Value> {
    let since = parse_since(since.unwrap_or("24h"))?;
    let ev = store_evidence(&state_dir.join(STORE_FILE), false);
    if let Some(gap) = &ev.approvals_gap {
        return Err(Error::rejected(format!(
            "approval records unreadable: {gap}"
        )));
    }
    let rows: Vec<Value> = ev
        .approvals
        .iter()
        .filter(|a| is_delegated(a) && a.at >= since)
        .map(|a| {
            let mut j = approval_rec_json(a);
            j["revoked"] = json!(ev.revocations.contains_key(&a.id));
            j["revoke"] = json!(format!(
                "cadence audit revoke {} --source \"<who, where>\" --reason \"<why>\"",
                a.id
            ));
            // Each note must still hash to what the approval recorded.
            let now: Vec<Option<String>> = a
                .verdicts
                .iter()
                .map(|p| {
                    std::fs::read(p)
                        .ok()
                        .map(|b| crate::delegation::sha256_hex(&b))
                })
                .collect();
            let intact = now.len() == a.verdict_sha256.len()
                && now
                    .iter()
                    .zip(&a.verdict_sha256)
                    .all(|(n, r)| n.as_ref() == Some(r));
            j["verdicts_intact"] = json!(intact);
            // A revert only for a PR that merged, from its merge commit.
            let args = [
                "pr",
                "view",
                &a.pr.to_string(),
                "-R",
                &a.repo,
                "--json",
                "state,mergeCommit",
            ]
            .map(String::from);
            let merged = gh_text(&args)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .filter(|v| v["state"] == "MERGED")
                .and_then(|v| v["mergeCommit"]["oid"].as_str().map(str::to_string));
            j["revert"] = json!(merged.map(|oid| format!("git revert {oid}")));
            j
        })
        .collect();
    Ok(json!({"since": iso(since), "delegated": rows}))
}
