//! Flags.

use super::*;

pub(super) fn flag_row(row: &mut Row) {
    // `reviewer==merger` compares GitHub identities only: the
    // `qa-verdict` status's `creator.login` against `mergedBy.login`.
    // A verdict note's `From:` is an agent alias — a different
    // namespace that is rendered but never feeds the flag (an alias
    // string-matching a login would be coincidence, not proof).
    let same_identity = row
        .qa_verdict_creator
        .as_deref()
        .zip(row.merger.as_deref())
        .is_some_and(|(a, b)| !a.is_empty() && a.eq_ignore_ascii_case(b));
    if same_identity {
        row.flags.push("reviewer==merger".into());
    }
    // A passing verdict on the exact head that landed: the verdict
    // note/table says pass AND the head it names is the squash-merged
    // `headRefOid` — or the `qa-verdict` commit status is SUCCESS on
    // that head. A verdict written after the merge (or a status set
    // post-merge) does not clear it: at merge time the head was
    // unreviewed.
    let verdict_pass = row
        .verdict
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case("pass"))
        && !row.verdict_post_hoc
        || row.native_workflow_review;
    // Heads "agree" when the strings match — or when the merge-content
    // check proved the reviewed change landed anyway (`headRefOid`
    // moves if an undeleted branch is pushed; the patch-id is the
    // stable evidence).
    let heads_agree = match (&row.reviewed_head, &row.landed_head) {
        (Some(r), Some(l)) => {
            r.len() >= 7
                && (l.starts_with(&r[..r.len().min(l.len())])
                    || r.starts_with(&l[..l.len().min(r.len())]))
        }
        // With no reviewed head recorded there is nothing to bind —
        // the merge_sha is what a note would name.
        (None, _) => true,
        _ => false,
    } || row.contains_head.starts_with("yes")
        || row.native_workflow_review;
    let status_ok = row
        .qa_verdict_status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("SUCCESS"))
        && !row.status_post_hoc;
    // The flag is "sources answered and said no". When a channel the
    // verdict could come through did not answer — gh down, notes dir
    // unreadable, store unopenable, the reviewed head absent from the
    // clone — the row is unknown, never an accusation.
    let mut gaps = !row.evidence_gaps.is_empty();
    if !heads_agree && row.reviewed_head.is_some() && row.contains_head.starts_with("unknown") {
        // The note named a head we cannot verify against the merge —
        // the binding question is unanswerable, not negative.
        gaps = true;
    }
    if !(verdict_pass && heads_agree) && !status_ok && !row.is_root && !gaps {
        row.flags.push("no-passing-verdict".into());
    }
    // The human-class gate (CAD-217): an operator approval for the
    // exact landed head must have been in force at merge time. An
    // unreadable store is `unknown` — reported, never flagged.
    if row.class.as_deref() == Some("human") {
        match row.approval.state.as_str() {
            "missing" => row.flags.push("approval-missing".into()),
            "revoked" => row.flags.push("approval-revoked".into()),
            _ => {}
        }
    }
    // CAD-918: a delegated-class merge needs a delegated approval in
    // force, or the operator's.
    if row.class.as_deref() == Some("delegated") && row.approval.state != "operator-claimed" {
        match row.delegated.state.as_str() {
            "missing" => row.flags.push("approval-missing".into()),
            "revoked" => row.flags.push("approval-revoked".into()),
            _ => {}
        }
    }
}

/// The fleet-level fact behind a structural match (CAD-207).
#[derive(Debug)]
pub(super) struct Structural {
    /// The one GitHub login every merge and every qa-verdict status in
    /// the run was made with.
    pub(super) identity: String,
    /// Rows whose `reviewer==merger` match it explains.
    pub(super) rows: usize,
}

pub(super) const SHARED_IDENTITY_EXPLANATION: &str =
    "the qa-verdict status and the merge were made \
     by the same GitHub account on every row that has both — the whole fleet pushes \
     through one token, so reviewer==merger cannot tell rows apart here; reported \
     once, not flagged (docs/AUDIT.md)";

/// Every GitHub login the whole fetch names, beyond the rendered
/// window: `mergedBy` of every merged PR `gh pr list` returned, and the
/// `qa-verdict` creator on every one of their landed heads. `None` when
/// any of those heads' statuses is not in hand — a fixture has them
/// all (a missing key is "no statuses"); a live run fetches only the
/// rendered rows', so a PR outside the window, or one whose fetch
/// failed, leaves the fleet undecided (CAD-287): its creator could be
/// the second identity that makes a self-review a finding.
pub(super) fn fleet_logins(gh: &Gh) -> Option<Vec<String>> {
    let in_hand = |p: &Value| {
        p["headRefOid"]
            .as_str()
            .is_some_and(|h| gh.statuses.contains_key(h))
    };
    if !gh.fixture && !gh.prs.values().all(in_hand) {
        return None;
    }
    let mergers = gh
        .prs
        .values()
        .filter_map(|p| p["mergedBy"]["login"].as_str().map(str::to_string));
    let creators = gh.statuses.values().filter_map(|s| qa_verdict_state(s)?.1);
    Some(mergers.chain(creators).collect())
}

/// When every GitHub identity the fleet names (`fleet`, plus the rows'
/// own mergers and qa-verdict creators) is ONE account,
/// `reviewer==merger` holds by construction on every row that has both
/// — a property of the fleet's token, not of any merge. Those matches
/// move from `flags` to `structural` and are summarized once. When the
/// fleet shows more than one identity, a row whose status creator is
/// its merger deviates and keeps its flag — however narrow the window.
pub(super) fn apply_structural(rows: &mut [Row], fleet: &[String]) -> Option<Structural> {
    let mut logins: Vec<&str> = rows
        .iter()
        .flat_map(|r| [r.merger.as_deref(), r.qa_verdict_creator.as_deref()])
        .flatten()
        .chain(fleet.iter().map(String::as_str))
        .filter(|s| !s.is_empty())
        .collect();
    logins.sort_by_key(|s| s.to_ascii_lowercase());
    logins.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let [identity] = logins.as_slice() else {
        return None;
    };
    let identity = identity.to_string();
    let mut matched = 0;
    for row in rows.iter_mut() {
        if let Some(i) = row.flags.iter().position(|f| f == "reviewer==merger") {
            row.flags.remove(i);
            row.structural.push("reviewer==merger".into());
            matched += 1;
        }
    }
    (matched > 0).then_some(Structural {
        identity,
        rows: matched,
    })
}
