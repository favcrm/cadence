//! Rendering.

use super::*;

pub(super) fn approval_rec_json(a: &ApprovalRec) -> Value {
    json!({
        "id": a.id, "source": a.source, "action": a.action,
        "head_sha": a.head_sha,
        "scope": {"repo": a.repo, "pr": a.pr},
        "recorded_via": a.recorded_via, "recorded_at": a.at,
        "approver": a.approver, "verdicts": a.verdicts,
        "verdict_sha256": a.verdict_sha256, "reviewers": a.reviewers,
    })
}

pub(super) fn approval_json(row: &Row) -> Value {
    let v = &row.approval;
    let mut j = json!({
        "required": match row.class.as_deref() {
            Some(c) => json!(c == "human"),
            None => Value::Null,
        },
        "state": v.state,
        "reason": v.reason,
        "record": v.record.as_ref().map(approval_rec_json),
        "before_merge": v.record.as_ref().zip(row.merged_at).map(|(a, m)| a.at <= m),
        "revocation": v.revocation.as_ref().map(|r| json!({
            "source": r.source, "reason": r.reason, "revoked_at": r.at,
            "before_merge": row.merged_at.map(|m| r.at <= m),
        })),
        "other_heads": v.other_heads.iter().map(approval_rec_json).collect::<Vec<_>>(),
        // No record is verified until CAD-280 (see APPROVAL_UNVERIFIED).
        "verified": v.record.as_ref().map(|_| false),
        "note": v.record.as_ref().map(|_| APPROVAL_UNVERIFIED),
    });
    if v.state.is_empty() {
        j["state"] = json!("unknown");
    }
    j
}

pub(super) fn claimed_count(rows: &[Row]) -> usize {
    rows.iter()
        .filter(|r| r.approval.state == "operator-claimed")
        .count()
}

/// The summary's approval clause: present whenever the run has a human
/// row or a claimed approval, so the digest never reads a bound record
/// as proof.
pub(super) fn approvals_clause(rows: &[Row]) -> String {
    let claimed = claimed_count(rows);
    if claimed == 0
        && !rows
            .iter()
            .any(|r| matches!(r.class.as_deref(), Some("human" | "delegated")))
    {
        return String::new();
    }
    let delegated = rows.iter().filter(|r| r.delegated.state == "delegated");
    let delegated = delegated.count();
    format!(
        ", {claimed} approval(s) operator-claimed ({APPROVAL_UNVERIFIED}), {delegated} delegated"
    )
}

/// The text row's approval line — for human-class rows, and for any
/// row an approval record binds to.
pub(super) fn approval_line(row: &Row) -> Option<String> {
    let v = &row.approval;
    if row.class.as_deref() != Some("human") && v.record.is_none() {
        return None;
    }
    let mut line = format!("approval {}", v.state);
    if let Some(a) = &v.record {
        let when = match row.merged_at {
            Some(m) if a.at <= m => "before merge",
            Some(_) => "after merge",
            None => "merge time unknown",
        };
        line.push_str(&format!(
            " · id {} · source \"{}\" · head {} · recorded {} ({when})",
            a.id,
            a.source,
            short(&a.head_sha),
            iso(a.at)
        ));
    }
    if let Some(r) = &v.revocation {
        line.push_str(&format!(
            " · revoked {} by \"{}\": {}",
            iso(r.at),
            r.source,
            r.reason
        ));
    }
    if let Some(reason) = &v.reason {
        line.push_str(&format!(" — {reason}"));
    }
    if v.record.is_some() {
        line.push_str(&format!(" · {APPROVAL_UNVERIFIED}"));
    }
    Some(line)
}

pub(super) fn row_json(row: &Row) -> Value {
    let unknowns: Vec<Value> = row
        .unknowns
        .iter()
        .map(|(f, r)| json!({"field": f, "reason": r}))
        .collect();
    json!({
        "pr": row.pr,
        "title": row.title,
        "merge_sha": row.merge_sha,
        "merged_at": row.merged_at,
        "landed_head": row.landed_head,
        "reviewed_head": row.reviewed_head,
        "contains_head": row.contains_head,
        "qa_verdict_status": row.qa_verdict_status,
        "qa_verdict_creator": row.qa_verdict_creator,
        "status_post_hoc": row.status_post_hoc,
        "verdict": row.verdict,
        "verdict_post_hoc": row.verdict_post_hoc,
        "reviewer": row.reviewer,
        "merger": row.merger,
        "class": row.class,
        "trigger": row.trigger,
        "gates": row.gates,
        "gate_summary": {
            "suite": row.gate_suite,
            "stress": row.gate_stress,
            "flakes": row.gate_flakes,
        },
        "auditor_check": row.auditor_check,
        "residue": row.residue,
        "project": row.project,
        "issues": row.issues,
        "outcome": {
            "tree_match": row.tree_match,
            "smoke": row.smoke,
            "daemon_restart": row.daemon_restart,
            "revert": row.revert,
        },
        "flags": row.flags,
        "structural": row.structural,
        "approval": approval_json(row),
        "workflow_authorization": row.workflow_authorization,
        // CAD-918: apart from the operator's; null when not in play.
        "delegated_approval": (!row.delegated.state.is_empty()).then(|| json!({
            "state": row.delegated.state, "reason": row.delegated.reason,
            "record": row.delegated.record.as_ref().map(approval_rec_json),
            "revoked": row.delegated.revocation.is_some(),
        })),
        "evidence_unavailable": if row.evidence_gaps.is_empty() {
            Value::Null
        } else {
            json!(row.evidence_gaps.join("; "))
        },
        "unknowns": unknowns,
    })
}

pub(super) fn print_json(
    repo: &Path,
    default_ref: &str,
    rows: &[Row],
    flagged: usize,
    structural: Option<&Structural>,
    opts: &AuditOptions,
    since: f64,
) {
    let structural: Vec<Value> = structural
        .map(|st| {
            json!({
                "code": "shared-github-identity",
                "match": "reviewer==merger",
                "identity": st.identity,
                "rows": st.rows,
                "explanation": SHARED_IDENTITY_EXPLANATION,
            })
        })
        .into_iter()
        .collect();
    let merges: Vec<Value> = rows.iter().map(row_json).collect();
    let mut by_class = json!({});
    for r in rows {
        let k = r.class.clone().unwrap_or_else(|| "unknown".into());
        by_class[k] = json!(by_class[&k].as_u64().unwrap_or(0) + 1);
    }
    println!(
        "{}",
        crate::output::json_text(&json!({
            "schema": "cadence.audit/1",
            "repo": repo.display().to_string(),
            "default_ref": default_ref,
            "since": opts.since,
            "since_epoch": if since > 0.0 { json!(since) } else { Value::Null },
            "filters": {"class": opts.class, "project": opts.project, "limit": opts.limit},
            "merges": merges,
            "summary": {
                "rows": rows.len(),
                "flagged": flagged,
                "flags": rows.iter().flat_map(|r| r.flags.clone()).collect::<Vec<_>>(),
                "structural": structural,
                "approvals": {
                    "operator_claimed": claimed_count(rows),
                    "verified": false,
                    "note": APPROVAL_UNVERIFIED,
                },
                "by_class": by_class,
            },
        }))
        .unwrap_or_default()
    );
}

pub(super) fn render_text(
    repo: &Path,
    default_ref: &str,
    rows: &[Row],
    flagged: usize,
    structural: Option<&Structural>,
    opts: &AuditOptions,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let or = |o: &Option<String>| o.clone().unwrap_or_else(|| "unknown".into());
    let _ = writeln!(
        out,
        "AUDIT {} ({} merges on {}){}",
        repo.display(),
        rows.len(),
        default_ref,
        opts.since
            .as_deref()
            .map(|s| format!(" since {s}"))
            .unwrap_or_default()
    );
    if rows.is_empty() {
        let _ = writeln!(out, "  no merges in range");
    }
    for row in rows {
        let flag = if row.flags.is_empty() {
            String::new()
        } else {
            format!("  FLAG[{}]", row.flags.join(","))
        };
        let pr = row
            .pr
            .map(|n| format!("#{n}"))
            .unwrap_or_else(|| "?".into());
        let _ = writeln!(out, "{pr} {}{}", row.title, flag);
        if !row.evidence_gaps.is_empty() {
            let _ = writeln!(
                out,
                "    unknown — evidence unavailable: {}",
                row.evidence_gaps.join("; ")
            );
        }
        let _ = writeln!(
            out,
            "    merge {} · merged_at {} · merger {}",
            &row.merge_sha[..9.min(row.merge_sha.len())],
            row.merged_at
                .map(|t| crate::issue::time::iso(t as i64))
                .unwrap_or_else(|| "unknown".into()),
            or(&row.merger),
        );
        let _ = writeln!(
            out,
            "    reviewed_head {} · landed_head {} · contains_head {}",
            row.reviewed_head
                .as_deref()
                .map(|s| &s[..9.min(s.len())])
                .unwrap_or("unknown"),
            row.landed_head
                .as_deref()
                .map(|s| &s[..9.min(s.len())])
                .unwrap_or("unknown"),
            row.contains_head,
        );
        let _ = writeln!(
            out,
            "    verdict {}{} · qa-verdict {}{} · reviewer {} · reviewer@gh {}",
            or(&row.verdict),
            if row.verdict_post_hoc {
                " (post-merge)"
            } else {
                ""
            },
            or(&row.qa_verdict_status),
            if row.status_post_hoc {
                " (post-merge)"
            } else {
                ""
            },
            or(&row.reviewer),
            or(&row.qa_verdict_creator),
        );
        let _ = writeln!(
            out,
            "    class {} · trigger {}",
            or(&row.class),
            or(&row.trigger),
        );
        if row.gates.is_empty() {
            let _ = writeln!(out, "    gates unknown (no gate summary in verdict note)");
        } else {
            let _ = writeln!(
                out,
                "    gates suite={} stress={} flakes={}",
                row.gate_suite.as_deref().unwrap_or("none recorded"),
                row.gate_stress.as_deref().unwrap_or("none recorded"),
                row.gate_flakes.as_deref().unwrap_or("none recorded"),
            );
        }
        if let Some(a) = &row.auditor_check {
            let _ = writeln!(out, "    auditor_check {a}");
        } else {
            let _ = writeln!(out, "    auditor_check unknown (none recorded)");
        }
        if !row.residue.is_empty() {
            let _ = writeln!(out, "    residue {}", row.residue.join(" "));
        }
        let _ = writeln!(
            out,
            "    outcome tree_match={} smoke={} daemon_restart={} revert={}",
            row.tree_match, row.smoke, row.daemon_restart, row.revert,
        );
        if let Some(line) = approval_line(row) {
            let _ = writeln!(out, "    {line}");
        }
        if let Some(workflow) = &row.workflow_authorization {
            let _ = writeln!(
                out, "    workflow grant {}: {} · native review at merge={} · not a per-head operator approval",
                workflow["grant_id"].as_str().unwrap_or("?"),
                workflow["state"].as_str().unwrap_or("unknown"),
                workflow["native_review_at_merge"],
            );
        }
        if !row.delegated.state.is_empty() {
            let v = &row.delegated;
            let rec = v.record.as_ref().map_or(String::new(), |a| {
                let by = a.approver.as_deref().unwrap_or("?");
                format!(
                    " · id {} · by {by} · reviewers {}",
                    a.id,
                    a.reviewers.join(", ")
                )
            });
            let why = v
                .reason
                .as_ref()
                .map_or(String::new(), |r| format!(" — {r}"));
            let _ = writeln!(out, "    delegated approval {}{rec}{why}", v.state);
        }
        for (field, reason) in &row.unknowns {
            let _ = writeln!(out, "    unknown[{field}] {reason}");
        }
    }
    if let Some(st) = structural {
        let _ = writeln!(
            out,
            "structural: {} merge(s) share GitHub identity {} — {}",
            st.rows, st.identity, SHARED_IDENTITY_EXPLANATION
        );
    }
    let _ = writeln!(
        out,
        "summary: {} row(s), {} flagged{}{}{}",
        rows.len(),
        flagged,
        approvals_clause(rows),
        if flagged > 0 { " — " } else { "" },
        if flagged > 0 {
            rows.iter()
                .flat_map(|r| r.flags.clone())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            String::new()
        },
    );
    out
}
