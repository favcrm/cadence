//! Operator approval (CAD-217).

use super::*;

/// Bind the row to the operator approval in force at merge time for
/// the exact landed head, then decide what the row's class makes of it.
/// Only human-class rows can flag; other rows show a bound record for
/// context and are otherwise `not-required` (or `unknown` without a
/// risk class).
pub(super) fn enrich_approval(ev: &StoreEvidence, slug: Option<&str>, row: &mut Row) {
    let delegated = bind_approval_for(ev, slug, row, crate::delegation::DELEGATED_ACTION);
    row.delegated = if delegated.record.is_some() || row.class.as_deref() == Some("delegated") {
        delegated
    } else {
        ApprovalView::default()
    };
    let view = bind_approval(ev, slug, row);
    row.approval = match row.class.as_deref() {
        Some("human") => {
            if view.state == "unknown" {
                row.unknowns
                    .push(("approval".into(), view.reason.clone().unwrap_or_default()));
            }
            view
        }
        _ if view.record.is_some() && view.state != "missing" => view,
        Some(_) => ApprovalView {
            state: "not-required".into(),
            other_heads: view.other_heads,
            ..Default::default()
        },
        None => ApprovalView {
            state: "unknown".into(),
            reason: Some(
                view.reason
                    .filter(|_| view.state == "unknown")
                    .unwrap_or_else(|| {
                        "no risk class recorded — whether the human gate applied is unknown".into()
                    }),
            ),
            other_heads: view.other_heads,
            ..Default::default()
        },
    };
}

pub(super) fn bind_approval(ev: &StoreEvidence, slug: Option<&str>, row: &Row) -> ApprovalView {
    bind_approval_for(ev, slug, row, "merge")
}

/// [`bind_approval`] for one approval action: `merge` (the operator's)
/// or `delegated-merge` (a designated agent's, CAD-918). A delegated
/// record carries `recorded_via: delegated:<alias>`; any other is not
/// one, whatever its action says.
pub(super) fn bind_approval_for(
    ev: &StoreEvidence,
    slug: Option<&str>,
    row: &Row,
    action: &str,
) -> ApprovalView {
    let unknown = |reason: String| ApprovalView {
        state: "unknown".into(),
        reason: Some(reason),
        ..Default::default()
    };
    if let Some(gap) = &ev.approvals_gap {
        return unknown(gap.clone());
    }
    let (Some(landed), Some(merged_at)) = (row.landed_head.as_deref(), row.merged_at) else {
        return unknown("no landed head to bind an approval to".into());
    };
    // Scope: a merge approval for this PR in this repo (the repo is
    // checked whenever the audit knows its own: gh's slug, or the
    // checkout's origin in fixture runs).
    let in_scope = |a: &&ApprovalRec| {
        (if action == "merge" {
            a.action == action
        } else {
            is_delegated(a)
        }) && row.pr == Some(a.pr)
            && slug.is_none_or(|s| s.eq_ignore_ascii_case(&a.repo))
    };
    let (bound, other_heads): (Vec<&ApprovalRec>, Vec<&ApprovalRec>) = ev
        .approvals
        .iter()
        .filter(in_scope)
        .partition(|a| a.head_sha.eq_ignore_ascii_case(landed));
    let other_heads: Vec<ApprovalRec> = other_heads.into_iter().cloned().collect();
    let revoked = |a: &ApprovalRec| ev.revocations.get(&a.id).cloned();
    let before = |a: &&&ApprovalRec| a.at <= merged_at;
    let view = |state: &str, a: &ApprovalRec, reason: Option<String>| ApprovalView {
        state: state.into(),
        reason,
        record: Some(a.clone()),
        revocation: revoked(a),
        other_heads: other_heads.clone(),
    };
    if let Some(a) = bound
        .iter()
        .filter(before)
        .find(|a| revoked(a).is_none_or(|r| r.at > merged_at))
    {
        // In force at merge time. A later revocation is shown, but the
        // question is what held when the merge happened.
        let reason = revoked(a).map(|r| format!("revoked after the merge ({})", iso(r.at)));
        let state = if action == "merge" {
            "operator-claimed"
        } else {
            "delegated"
        };
        return view(state, a, reason);
    }
    if let Some(a) = bound.iter().find(before) {
        let reason = format!("approval {} was revoked before the merge", a.id);
        return view("revoked", a, Some(reason));
    }
    if let Some(a) = bound.first() {
        let reason = format!(
            "only a post-merge record names the landed head (approval {} recorded {})",
            a.id,
            iso(a.at)
        );
        return view("missing", a, Some(reason));
    }
    let reason = match other_heads.first() {
        Some(o) => format!(
            "no approval names landed head {} — approval {} names head {}, and an \
             approval never carries over to another head",
            short(landed),
            o.id,
            short(&o.head_sha)
        ),
        None => format!("no approval record names landed head {}", short(landed)),
    };
    ApprovalView {
        state: "missing".into(),
        reason: Some(reason),
        other_heads,
        ..Default::default()
    }
}

/// CAD-959: the first whitespace token of a note's `From:`, lower-cased,
/// minus backticks and trailing punctuation. `From:` is free text
/// (`cc13-pm standards reviewer`); this is the ONE identity used for both
/// the author exclusion and the distinct-reviewer count.
pub(crate) fn reviewer_identity(from: &str) -> String {
    from.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches('`')
        .trim_end_matches([',', ';', ':', '.', '(', ')'])
        .to_lowercase()
}

/// The `YYYYMMDD-HHMMSS` (or `YYYYMMDDTHHMMSSZ`) stamp a note file name
/// starts with, as 14 digits. `None` when the name carries no stamp.
pub(super) fn name_stamp(name: &str) -> Option<String> {
    let b = name.as_bytes();
    let digits = |r: std::ops::Range<usize>| {
        b.get(r.clone())
            .filter(|s| s.iter().all(u8::is_ascii_digit))
            .map(|s| String::from_utf8_lossy(s).into_owned())
    };
    let date = digits(0..8)?;
    let sep = *b.get(8)?;
    if sep != b'-' && sep != b'T' {
        return None;
    }
    let time = digits(9..15)?;
    Some(format!("{date}{time}"))
}

/// `(kind, result)` from a verdict note's title `# Verdict: <ID> <KIND> — <result>`.
/// Kind is matched exactly: `standards`, `spec-security`, `browser-qa`,
/// `combined` (`Review (standards+spec)`), else `other`.
pub(super) fn title_kind_and_result(title: &str, issue: &str) -> (&'static str, String) {
    let body = title.trim_start_matches('#').trim();
    let body = body.strip_prefix("Verdict:").unwrap_or(body).trim();
    let body = body.strip_prefix(issue).unwrap_or(body).trim();
    let (kind, result) = match body.rsplit_once(" — ").or_else(|| body.rsplit_once(" - ")) {
        Some((k, r)) => (k.trim(), r.trim()),
        None => (body, ""),
    };
    let kind = match kind.to_lowercase().as_str() {
        "standards" | "standards review" => "standards",
        "spec/security" | "spec/security review" => "spec-security",
        "browser qa" | "browser qa review" => "browser-qa",
        "review (standards+spec)" => "combined",
        _ => "other",
    };
    let result = result
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    (kind, result)
}

/// CAD-959: head-pinned verdict notes for `issue` / PR `pr` / full `head`,
/// parsed by [`parse_note`] so there is one parser. Notes of the issue that
/// do not bind to this PR and head are listed under `skipped` with a
/// reason, never dropped silently. Symlinks are skipped, never followed.
/// CAD-918: the delegated approval reads its notes through this too.
///
/// AOS-473: the document says `origin:"local"`. The merge gate counts a
/// hosted document by attested principal and an unmarked one as local, so
/// the marker is what keeps these notes under today's rule.
pub(crate) fn verdicts_in(dir: &Path, issue: &str, pr: u64, head: &str) -> Result<Value> {
    let read = std::fs::read_dir(dir).map_err(|e| {
        Error::rejected(format!("cannot read the notes dir {}: {e}", dir.display()))
    })?;
    let mut names: Vec<(String, PathBuf)> = read
        .flatten()
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .filter(|(n, _)| n.ends_with(".md"))
        .collect();
    names.sort();
    let (mut verdicts, mut skipped) = (Vec::new(), Vec::new());
    for (name, path) in names {
        let skip = |why: String| json!({"name": name, "reason": why});
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            if name.contains("-verdict") {
                skipped.push(skip(
                    "a symlink; notes are never followed through links".into(),
                ));
            }
            continue;
        }
        if !meta.is_file() || !name.contains("-verdict") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let note = parse_note(&path, &name, &text);
        if note.issue.as_deref() != Some(issue) {
            continue;
        }
        // An unstamped note is still listed (stamp ""): a caller must be able
        // to refuse on it, never lose it.
        let stamp = name_stamp(&name).unwrap_or_default();
        if !note.prs.contains(&pr) {
            skipped.push(skip(format!("does not name PR #{pr}")));
            continue;
        }
        match note.head_sha.as_deref() {
            None => {
                skipped.push(skip("no `head <sha>` line".into()));
                continue;
            }
            Some(h) if !h.eq_ignore_ascii_case(head) => {
                skipped.push(skip(format!("names head {h}, not the full head {head}")));
                continue;
            }
            Some(_) => {}
        }
        let title = text.lines().next().unwrap_or("");
        let (kind, title_result) = title_kind_and_result(title, issue);
        let section = note
            .verdict_section
            .clone()
            .unwrap_or_default()
            .to_lowercase();
        let inline = note
            .verdict_inline
            .clone()
            .unwrap_or_default()
            .to_lowercase();
        // Title, section and inline line must agree where present; any
        // disagreement is a `conflict`, never a pass.
        let mut words: Vec<&String> = [&title_result, &section, &inline]
            .into_iter()
            .filter(|w| !w.is_empty())
            .collect();
        words.dedup();
        let outcome = match words.as_slice() {
            [one] => (*one).clone(),
            _ => "conflict".to_string(),
        };
        let risk_raw = text
            .lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix("Risk:"))
            .and_then(|r| r.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        verdicts.push(json!({
            "name": name, "path": path.display().to_string(), "stamp": stamp,
            "reviewer": reviewer_identity(note.from.as_deref().unwrap_or("")),
            "from": note.from, "kind": kind, "outcome": outcome,
            "title_outcome": title_result, "section_outcome": section, "inline_outcome": inline,
            "risk": risk_raw, "head": head,
        }));
    }
    Ok(
        json!({"origin": "local", "issue": issue, "pr": pr, "head": head,
              "verdicts": verdicts, "skipped": skipped}),
    )
}

/// `cadence audit verdicts`: read-only; one JSON document.
pub fn verdicts_check(notes_dir: Option<&Path>, issue: &str, pr: u64, head: &str) -> Result<Value> {
    verdicts_in(notes_dir.unwrap_or(Path::new(NOTES_DIR)), issue, pr, head)
}

/// CAD-959: is an operator approval in force for `head` of PR `pr` right
/// now? The pre-merge read `scripts/enqueue-reviewed` needs: unlike an
/// audit row there is no merge time, so "in force" means a `merge`
/// record naming exactly this repo, PR and full head that no revocation
/// has withdrawn. An approval for another head never counts. States:
/// `in-force` (a claim, like every approval until CAD-280), `revoked`,
/// `missing`, `unknown` (the store could not answer).
pub(super) fn approval_in_force(ev: &StoreEvidence, repo: &str, pr: u64, head: &str) -> Value {
    if let Some(gap) = &ev.approvals_gap {
        return json!({"state": "unknown", "reason": gap});
    }
    let bound: Vec<&ApprovalRec> = ev
        .approvals
        .iter()
        .filter(|a| {
            a.action == "merge"
                && a.pr == pr
                && a.repo.eq_ignore_ascii_case(repo)
                && a.head_sha.eq_ignore_ascii_case(head)
        })
        .collect();
    if let Some(a) = bound.iter().find(|a| !ev.revocations.contains_key(&a.id)) {
        return json!({"state": "in-force", "approval_id": a.id, "source": a.source,
                      "action": a.action, "recorded_via": a.recorded_via, "verified": false,
                      "note": APPROVAL_UNVERIFIED});
    }
    if let Some(a) = bound.first() {
        return json!({"state": "revoked", "approval_id": a.id,
                      "reason": format!("approval {} was revoked", a.id)});
    }
    json!({"state": "missing",
           "reason": format!("no merge approval names head {} of {repo}#{pr}", short(head))})
}

/// `cadence audit approval`: read-only, no daemon. Exit 0 only when an
/// approval is in force for exactly this head.
pub fn approval_check(state_dir: &Path, repo: &str, pr: u64, head: &str) -> (Value, i32) {
    let ev = store_evidence(&state_dir.join(STORE_FILE), false);
    let v = approval_in_force(&ev, repo, pr, head);
    let code = i32::from(v["state"] != "in-force");
    (v, code)
}

/// CAD-1106: the ticket scope pre-approvals recorded for `issue`, each with
/// whether a revocation withdrew it. Read-only; the caller (the enqueue
/// script) decides which one counts: it compares the digest with the
/// ticket body as it reads now and requires `recorded_via`
/// `operator-connection`. `unknown` when the store cannot answer.
pub(super) fn scope_approvals(ev: &StoreEvidence, issue: &str) -> Value {
    if let Some(gap) = &ev.approvals_gap {
        return json!({"state": "unknown", "reason": gap});
    }
    let rows: Vec<Value> = ev
        .scopes
        .iter()
        .filter(|s| s.issue.eq_ignore_ascii_case(issue))
        .map(|s| {
            json!({"approval_id": s.id, "digest": s.digest, "source": s.source,
                   "recorded_via": s.recorded_via,
                   "revoked": ev.revocations.contains_key(&s.id)})
        })
        .collect();
    json!({"state": "ok", "issue": issue, "approvals": rows})
}

/// `cadence audit scope --issue X`: read-only, no daemon. Exit 0 when the
/// store answered (an empty list is an answer), 1 when it could not.
pub fn scope_check(state_dir: &Path, issue: &str) -> (Value, i32) {
    let ev = store_evidence(&state_dir.join(STORE_FILE), false);
    let v = scope_approvals(&ev, issue);
    let code = i32::from(v["state"] != "ok");
    (v, code)
}

pub(super) fn short(sha: &str) -> &str {
    &sha[..9.min(sha.len())]
}

pub(super) fn iso(at: f64) -> String {
    crate::issue::time::iso(at as i64)
}
