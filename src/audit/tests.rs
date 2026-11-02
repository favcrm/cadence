use super::*;

#[test]
fn pr_number_from_subject() {
    assert_eq!(
        pr_number("session start|end: one-verb session gate (CAD-92) (#63)"),
        Some(63)
    );
    // A trailing qualifier after the squash suffix must not hide
    // the number; real merge commits parse the prefix form.
    assert_eq!(pr_number("audit log (#63) (rebased)"), Some(63));
    assert_eq!(
        pr_number("Merge pull request #64 from favcrm/branch"),
        Some(64)
    );
    assert_eq!(pr_number("plain commit"), None);
    assert_eq!(pr_number("merge: no number (#)"), None);
}

#[test]
fn issue_ids_finds_all() {
    assert_eq!(
        issue_ids("codex: approval (CAD-143, CAD-126) (#72)"),
        vec!["CAD-143".to_string(), "CAD-126".to_string()]
    );
    assert!(issue_ids("nothing").is_empty());
}

#[test]
fn parse_since_forms() {
    let now = crate::issue::time::now_epoch() as f64;
    assert!((parse_since("24h").unwrap() - (now - 86400.0)).abs() < 2.0);
    assert!((parse_since("7d").unwrap() - (now - 7.0 * 86400.0)).abs() < 2.0);
    assert_eq!(parse_iso("1970-01-02").unwrap(), 86400);
    assert_eq!(parse_iso("1970-01-01T01:00:00Z").unwrap(), 3600);
    assert!(parse_since("garbage").is_err());
}

#[test]
fn parse_note_extracts_fields() {
    let text = "# Verdict: CAD-92 — pass\n> From: `qa-1`\n> Issue: `CAD-92`\n\n## Verdict\npass — head `7896dd2735035c0c67e246039cb495231702941c`\n\n## Risk\n**Risk: notify (deletion paths; fourth round)**\n\n**What an auditor should check after the merge:** one handoff lands.\n\nResidue **CAD-147** stands.\n";
    let n = parse_note(Path::new("/tmp/x-verdict.md"), "x-verdict.md", text);
    assert_eq!(n.kind, "verdict");
    assert_eq!(n.from.as_deref(), Some("qa-1"));
    assert!(n.shas.iter().any(|s| s.starts_with("7896dd2")));
    assert_eq!(n.class.as_deref(), Some("notify"));
    assert!(n.trigger.as_deref().unwrap().contains("deletion"));
    assert_eq!(n.auditor_check.as_deref(), Some("one handoff lands."));
    assert!(n.residue.contains(&"CAD-147".to_string()));
}

#[test]
fn hex_shas_finds_tokens() {
    let v = hex_shas("head `7896dd2735035c0c67e246039cb495231702941c` and main 07ca3013");
    assert!(v.contains(&"7896dd2735035c0c67e246039cb495231702941c".to_string()));
    assert!(v.contains(&"07ca3013".to_string()));
    // All-letter abbrevs are real (`cabbebe`); all-digit tokens stay
    // out of general context (filename dates) but count on a head line.
    assert!(hex_shas("cabbebe").contains(&"cabbebe".to_string()));
    assert!(hex_shas("20260920-141226").is_empty());
    assert!(hex_shas_ctx("5428215", true).contains(&"5428215".to_string()));
}

fn head_line(text: &str) -> Option<String> {
    parse_note(Path::new("/tmp/n-verdict.md"), "n-verdict.md", text).head_sha
}

#[test]
fn head_line_parses_forms() {
    assert_eq!(
        head_line("pass — head `7896dd2735035c0c67e246039cb495231702941c`").as_deref(),
        Some("7896dd2735035c0c67e246039cb495231702941c")
    );
    assert_eq!(
        head_line("pass — head: 5428215").as_deref(),
        Some("5428215")
    );
    // `ahead`/`overhead` are not head lines.
    assert_eq!(head_line("pass — ahead 7896dd2"), None);
    // A case-changing multibyte char before `head` must not panic —
    // `ẞ`.len() grows under to_lowercase, so lowercase offsets
    // cannot index the original text.
    assert_eq!(
        head_line("verdict ẞ head 7896dd27").as_deref(),
        Some("7896dd27")
    );
    assert_eq!(
        head_line("verdict İ head 7896dd27").as_deref(),
        Some("7896dd27")
    );
}

fn flagged_row() -> Row {
    Row {
        pr: Some(1),
        merge_sha: "ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab".into(),
        landed_head: Some("7896dd2735035c0c67e246039cb495231702941c".into()),
        contains_head: "yes (patch-id match)".into(),
        ..Default::default()
    }
}

#[test]
fn flag_reviewer_eq_merger_uses_github_namespace() {
    // Same GitHub login posted the qa-verdict status and merged —
    // this is the only identity pair the flag compares.
    let mut r = flagged_row();
    r.merger = Some("cc-syntax".into());
    r.qa_verdict_creator = Some("cc-syntax".into());
    r.qa_verdict_status = Some("SUCCESS".into());
    flag_row(&mut r);
    assert!(r.flags.iter().any(|f| f == "reviewer==merger"));
    assert!(!r.flags.iter().any(|f| f == "no-passing-verdict"));

    // The note `From:` is an agent alias — matching the merger's
    // login is coincidence, not proof. It must never flag.
    let mut r = flagged_row();
    r.merger = Some("qa-1".into());
    r.reviewer = Some("qa-1".into());
    r.qa_verdict_creator = Some("qa-bot".into());
    r.qa_verdict_status = Some("SUCCESS".into());
    flag_row(&mut r);
    assert!(r.flags.is_empty(), "{:?}", r.flags);
}

#[test]
fn flag_no_passing_verdict_only_when_sources_answered() {
    // Sources all answered, none proves a pass on the landed head.
    let mut r = flagged_row();
    flag_row(&mut r);
    assert_eq!(r.flags, vec!["no-passing-verdict".to_string()]);

    // gh down / notes unreadable → evidence unavailable, not an
    // accusation: no flag.
    let mut r = flagged_row();
    r.evidence_gaps
        .push("status fetch failed: gh: timeout".into());
    flag_row(&mut r);
    assert!(r.flags.is_empty());

    // Reviewed head exists but isn't in the clone — the binding is
    // unanswerable, not negative.
    let mut r = flagged_row();
    r.reviewed_head = Some("deadd00".into());
    r.contains_head = "unknown (head not in local object store — never fetched)".into();
    flag_row(&mut r);
    assert!(r.flags.is_empty());

    // A pass verdict on the landed head clears it.
    let mut r = flagged_row();
    r.verdict = Some("pass".into());
    r.reviewed_head = r.landed_head.clone();
    flag_row(&mut r);
    assert!(r.flags.is_empty(), "{:?}", r.flags);

    // A SUCCESS qa-verdict status clears it.
    let mut r = flagged_row();
    r.qa_verdict_status = Some("SUCCESS".into());
    flag_row(&mut r);
    assert!(r.flags.is_empty(), "{:?}", r.flags);
}

#[test]
fn flag_post_hoc_verdict_does_not_clear() {
    // A pass verdict written after the merge proves nothing about
    // merge-time review.
    let mut r = flagged_row();
    r.verdict = Some("pass".into());
    r.reviewed_head = r.landed_head.clone();
    r.verdict_post_hoc = true;
    flag_row(&mut r);
    assert!(r.flags.iter().any(|f| f == "no-passing-verdict"));

    // Same for a status posted after the merge.
    let mut r = flagged_row();
    r.qa_verdict_status = Some("SUCCESS".into());
    r.status_post_hoc = true;
    flag_row(&mut r);
    assert!(r.flags.iter().any(|f| f == "no-passing-verdict"));
}

fn opts() -> AuditOptions {
    AuditOptions {
        since: None,
        class: None,
        project: None,
        json: false,
        limit: None,
        repo: None,
        notes_dir: None,
        merge_report: None,
        cwd: PathBuf::from("/"),
        state_dir: PathBuf::from("/"),
    }
}

/// A merged row with a qa-verdict status by `creator` (or none).
fn fleet_row(pr: u64, merger: &str, creator: Option<&str>) -> Row {
    let mut r = flagged_row();
    r.pr = Some(pr);
    r.title = format!("work {pr}");
    r.merger = Some(merger.into());
    if let Some(c) = creator {
        r.qa_verdict_status = Some("SUCCESS".into());
        r.qa_verdict_creator = Some(c.into());
    }
    r
}

#[test]
fn digest_reports_shared_identity_once_and_flags_only_real_findings() {
    // CAD-207's live run: 34 merges whose status and merge share the
    // fleet's one GitHub account, 2 with no passing verdict.
    let mut rows: Vec<Row> = (15..=50)
        .map(|n| {
            let creator = (n != 43 && n != 50).then_some("cc-syntax");
            fleet_row(n, "cc-syntax", creator)
        })
        .collect();
    for r in &mut rows {
        flag_row(r);
    }
    let st = apply_structural(&mut rows, &[]).expect("one shared identity");
    assert_eq!((st.identity.as_str(), st.rows), ("cc-syntax", 34));
    let flagged: Vec<(u64, Vec<String>)> = rows
        .iter()
        .filter(|r| !r.flags.is_empty())
        .map(|r| (r.pr.unwrap(), r.flags.clone()))
        .collect();
    assert_eq!(
        flagged,
        vec![
            (43, vec!["no-passing-verdict".to_string()]),
            (50, vec!["no-passing-verdict".to_string()])
        ]
    );
    let text = render_text(Path::new("/r"), "origin/main", &rows, 2, Some(&st), &opts());
    assert_eq!(text.matches("FLAG[").count(), 2, "{text}");
    assert_eq!(text.matches("structural:").count(), 1, "{text}");
    assert_eq!(text.matches("reviewer==merger").count(), 1, "{text}");
}

#[test]
fn digest_keeps_reviewer_eq_merger_when_identities_differ() {
    // The fleet has a separate QA identity: the row whose status was
    // posted by its own merger deviates and keeps its flag.
    let mut rows = vec![
        fleet_row(1, "ops-bot", Some("qa-bot")),
        fleet_row(2, "ops-bot", Some("ops-bot")),
    ];
    for r in &mut rows {
        flag_row(r);
    }
    assert!(apply_structural(&mut rows, &[]).is_none());
    assert!(rows[0].flags.is_empty(), "{:?}", rows[0].flags);
    assert_eq!(rows[1].flags, vec!["reviewer==merger".to_string()]);
    assert!(rows[1].structural.is_empty());
    // Login case does not split one identity in two.
    let mut rows = vec![
        fleet_row(1, "CC-Syntax", Some("cc-syntax")),
        fleet_row(2, "cc-syntax", None),
    ];
    for r in &mut rows {
        flag_row(r);
    }
    assert_eq!(apply_structural(&mut rows, &[]).map(|s| s.rows), Some(1));
    assert!(rows[0].flags.is_empty(), "{:?}", rows[0].flags);
    // #2 has no status at all — its real finding still flags.
    assert_eq!(rows[1].flags, vec!["no-passing-verdict".to_string()]);
}

#[test]
fn digest_decides_identity_over_the_fleet_not_the_window() {
    // A narrow window (`--limit 1`) holds one self-merge; outside it
    // the fleet shows a separate QA identity. The window alone looks
    // like one shared account — the fleet says otherwise: flag.
    let mut rows = vec![fleet_row(3, "ops-1", Some("ops-1"))];
    flag_row(&mut rows[0]);
    let gh = Gh {
        prs: HashMap::from([
            (1, json!({"mergedBy": {"login": "ops-1"}})),
            (3, json!({"mergedBy": {"login": "ops-1"}})),
        ]),
        statuses: HashMap::from([(
            "h1".to_string(),
            json!({"statuses": [{"context": "qa-verdict", "state": "success",
                                 "creator": {"login": "qa-bot"}}]}),
        )]),
        fixture: true,
        ..Default::default()
    };
    let fleet = fleet_logins(&gh).expect("a fixture holds every status");
    assert_eq!(fleet.len(), 3);
    assert!(apply_structural(&mut rows, &fleet).is_none());
    assert_eq!(rows[0].flags, vec!["reviewer==merger".to_string()]);
    // Without the outside identity it is structural, as before.
    let mut rows = vec![fleet_row(3, "ops-1", Some("ops-1"))];
    flag_row(&mut rows[0]);
    let fleet = vec!["ops-1".to_string()];
    assert_eq!(apply_structural(&mut rows, &fleet).map(|s| s.rows), Some(1));
    assert!(rows[0].flags.is_empty());
}

#[test]
fn live_fleet_is_undecided_until_every_fetched_status_is_in_hand() {
    // CAD-287: live, a merged PR whose landed-head status was never
    // fetched (outside the window) may hide a second identity — the
    // mergers alone decide nothing.
    let status = json!({"statuses": [{"context": "qa-verdict", "state": "success",
                                      "creator": {"login": "ops-1"}}]});
    let mut gh = Gh {
        prs: HashMap::from([
            (
                1,
                json!({"headRefOid": "h1", "mergedBy": {"login": "ops-1"}}),
            ),
            (
                3,
                json!({"headRefOid": "h3", "mergedBy": {"login": "ops-1"}}),
            ),
        ]),
        statuses: HashMap::from([("h3".to_string(), status.clone())]),
        ..Default::default()
    };
    assert!(fleet_logins(&gh).is_none());
    // A fixture's missing key is "no statuses on this head" — decided.
    gh.fixture = true;
    assert_eq!(fleet_logins(&gh).map(|f| f.len()), Some(3));
    // Live with every head's status in hand — decided.
    gh.fixture = false;
    gh.statuses.insert("h1".into(), status);
    assert_eq!(fleet_logins(&gh).map(|f| f.len()), Some(4));
    // A PR with no landed head cannot be fetched — undecided.
    gh.prs.insert(4, json!({"mergedBy": {"login": "ops-1"}}));
    assert!(fleet_logins(&gh).is_none());
}

const HEAD: &str = "7896dd2735035c0c67e246039cb495231702941c";
const OLD_HEAD: &str = "1111111111111111111111111111111111111111";
const MERGED: f64 = 1_000_000.0;

fn rec(id: &str, head: &str, at: f64) -> ApprovalRec {
    ApprovalRec {
        id: id.into(),
        source: "operator in chat".into(),
        action: "merge".into(),
        head_sha: head.into(),
        repo: "x/y".into(),
        pr: 1,
        recorded_via: Some("operator-connection".into()),
        approver: None,
        verdicts: vec![],
        verdict_sha256: vec![],
        reviewers: vec![],
        at,
    }
}

/// CAD-918: a delegated record is reported apart from the operator's
/// and never clears a human-class merge; an operator-recorded
/// `delegated-merge` is no delegated approval at all.
#[test]
fn delegated_approval_is_reported_apart_and_never_clears_human() {
    let mut d = rec("d", HEAD, MERGED - 60.0);
    d.action = crate::delegation::DELEGATED_ACTION.into();
    d.recorded_via = Some("delegated:pm-d".into());
    let r = human(&ev(vec![d.clone()], &[]), None);
    assert_eq!(r.approval.state, "missing");
    assert_eq!(r.delegated.state, "delegated");
    assert_eq!(r.flags, vec!["approval-missing".to_string()]);
    assert_eq!(row_json(&r)["delegated_approval"]["state"], "delegated");
    let delegated_row = |e: &StoreEvidence| {
        let mut r = flagged_row();
        r.landed_head = Some(HEAD.into());
        r.merged_at = Some(MERGED);
        r.class = Some("delegated".into());
        r.qa_verdict_status = Some("SUCCESS".into());
        enrich_approval(e, None, &mut r);
        flag_row(&mut r);
        r
    };
    assert!(delegated_row(&ev(vec![d.clone()], &[])).flags.is_empty());
    d.recorded_via = Some("operator-connection".into());
    let r = delegated_row(&ev(vec![d], &[]));
    assert_eq!(r.delegated.state, "missing");
    assert_eq!(r.flags, vec!["approval-missing".to_string()]);
}

fn revoke(at: f64) -> Revocation {
    Revocation {
        source: "operator".into(),
        reason: "head moved".into(),
        at,
    }
}

/// A human-class row's approval state and flags under `ev`.
fn human(ev: &StoreEvidence, slug: Option<&str>) -> Row {
    let mut r = flagged_row();
    r.landed_head = Some(HEAD.into());
    r.merged_at = Some(MERGED);
    r.class = Some("human".into());
    r.qa_verdict_status = Some("SUCCESS".into());
    enrich_approval(ev, slug, &mut r);
    flag_row(&mut r);
    r
}

fn ev(approvals: Vec<ApprovalRec>, revocations: &[(&str, Revocation)]) -> StoreEvidence {
    StoreEvidence {
        approvals,
        revocations: revocations
            .iter()
            .map(|(id, r)| (id.to_string(), r.clone()))
            .collect(),
        opened: true,
        ..Default::default()
    }
}

#[test]
fn approval_binds_exact_landed_head_in_force_at_merge() {
    // Approved before the merge for the exact landed head.
    let r = human(&ev(vec![rec("a", HEAD, MERGED - 60.0)], &[]), Some("x/y"));
    assert_eq!(r.approval.state, "operator-claimed");
    assert_eq!(r.approval.record.as_ref().unwrap().id, "a");
    assert!(r.flags.is_empty(), "{:?}", r.flags);
    let line = approval_line(&r).unwrap();
    assert!(line.contains("(before merge)"), "{line}");
    assert!(line.contains("unverified until CAD-280"), "{line}");
    let j = approval_json(&r);
    assert_eq!(j["verified"], false, "{j}");
    assert_eq!(j["note"], "unverified until CAD-280", "{j}");

    // Revoked AFTER the merge: it was in force when the merge ran.
    let e = ev(
        vec![rec("a", HEAD, MERGED - 60.0)],
        &[("a", revoke(MERGED + 60.0))],
    );
    let r = human(&e, None);
    assert_eq!(r.approval.state, "operator-claimed");
    assert!(r
        .approval
        .reason
        .as_deref()
        .unwrap()
        .contains("after the merge"));
    assert!(r.flags.is_empty());

    // Revoked BEFORE the merge: revoked, flagged.
    let e = ev(
        vec![rec("a", HEAD, MERGED - 60.0)],
        &[("a", revoke(MERGED - 30.0))],
    );
    let r = human(&e, None);
    assert_eq!(r.approval.state, "revoked");
    assert_eq!(r.flags, vec!["approval-revoked".to_string()]);

    // One revoked, another still in force: operator-claimed.
    let e = ev(
        vec![rec("a", HEAD, MERGED - 60.0), rec("b", HEAD, MERGED - 10.0)],
        &[("a", revoke(MERGED - 30.0))],
    );
    assert_eq!(human(&e, None).approval.state, "operator-claimed");

    // Only recorded after the merge: missing, the record shown.
    let r = human(&ev(vec![rec("a", HEAD, MERGED + 60.0)], &[]), None);
    assert_eq!(r.approval.state, "missing");
    assert!(r.approval.record.is_some());
    assert_eq!(r.flags, vec!["approval-missing".to_string()]);

    // An approval for an older head never counts for the landed one.
    let r = human(&ev(vec![rec("a", OLD_HEAD, MERGED - 60.0)], &[]), None);
    assert_eq!(r.approval.state, "missing");
    assert_eq!(r.approval.other_heads.len(), 1);
    assert!(r
        .approval
        .reason
        .as_deref()
        .unwrap()
        .contains("never carries"));
    assert_eq!(r.flags, vec!["approval-missing".to_string()]);

    // Another PR, another repo, or another action: out of scope.
    let mut other_pr = rec("a", HEAD, MERGED - 60.0);
    other_pr.pr = 2;
    let mut other_action = rec("b", HEAD, MERGED - 60.0);
    other_action.action = "deploy".into();
    let e = ev(vec![other_pr, other_action], &[]);
    assert_eq!(human(&e, None).approval.state, "missing");
    let e = ev(vec![rec("a", HEAD, MERGED - 60.0)], &[]);
    assert_eq!(human(&e, Some("other/repo")).approval.state, "missing");
    // The fixture/no-slug path still binds on head + PR.
    assert_eq!(human(&e, None).approval.state, "operator-claimed");
}

fn vnote(dir: &Path, name: &str, title: &str, from: &str, head: &str, risk: &str) {
    let res = title.rsplit(" — ").next().unwrap_or("pass");
    let res = res.split_whitespace().next().unwrap_or("pass");
    let text = format!(
        "{title}\n> Issue: CAD-9\n> From: {from}\n\n## Verdict\n{res} — PR #5, head {head}\n\n{risk}\n"
    );
    std::fs::write(dir.join(name), text).unwrap();
}

#[test]
fn verdicts_bind_head_and_pr_and_report_what_they_skip() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (h, old) = (HEAD, OLD_HEAD);
    vnote(
        d,
        "20261001-070000-a-verdict.md",
        "# Verdict: CAD-9 Standards review — pass",
        "cc13-pm standards reviewer (Claude)",
        h,
        "Risk: auto",
    );
    vnote(
        d,
        "20261001-070100-b-verdict.md",
        "# Verdict: CAD-9 Spec/security review — pass",
        "`rev-b`",
        h,
        "Risk: human(3)",
    );
    vnote(
        d,
        "20261001-070200-c-verdict.md",
        "# Verdict: CAD-9 Browser QA — revise",
        "rev-c",
        h,
        "Risk: auto",
    );
    vnote(
        d,
        "20261001-070300-d-verdict.md",
        "# Verdict: CAD-9 Review (standards+spec) — pass",
        "rev-d",
        old,
        "Risk: auto",
    );
    vnote(
        d,
        "20261001-070400-e-verdict.md",
        "# Verdict: CAD-9 Inspection of respect — pass",
        "rev-e",
        h,
        "",
    );
    vnote(
        d,
        "nostamp-verdict.md",
        "# Verdict: CAD-9 Standards review — pass",
        "rev-f",
        h,
        "",
    );
    std::fs::write(d.join("20261001-070500-g-verdict.md"),
        format!("# Verdict: CAD-9 Standards review — pass\n> Issue: CAD-9\n> From: g\n\n## Verdict\npass — head {h}\n")).unwrap();
    std::fs::write(
        d.join("20261001-070600-h-verdict.md"),
        format!("# Verdict: CAD-1 Standards review — pass\n> Issue: CAD-1\n> From: rev-h\n\n## Verdict\npass — PR #5, head {h}\n"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        d.join("20261001-070000-a-verdict.md"),
        d.join("20261001-070700-l-verdict.md"),
    )
    .unwrap();
    let v = verdicts_in(d, "CAD-9", 5, h).unwrap();
    // AOS-473: the marker is what keeps these notes on the local counting rule.
    assert_eq!(v["origin"], "local");
    let rows: Vec<(String, String, String, String)> = v["verdicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["reviewer"].as_str().unwrap().into(),
                r["kind"].as_str().unwrap().into(),
                r["outcome"].as_str().unwrap().into(),
                r["risk"].as_str().unwrap().into(),
            )
        })
        .collect();
    // Prose after the alias never becomes part of the identity.
    assert_eq!(
        rows[0],
        (
            "cc13-pm".into(),
            "standards".into(),
            "pass".into(),
            "auto".into()
        )
    );
    assert_eq!(
        rows[1],
        (
            "rev-b".into(),
            "spec-security".into(),
            "pass".into(),
            "human(3)".into()
        )
    );
    assert_eq!(rows[2].1, "browser-qa");
    assert_eq!(rows[2].2, "revise");
    // The exact-title rule: a substring never makes a kind.
    assert_eq!(rows[3].1, "other");
    assert_eq!(rows[3].3, "");
    // The unstamped note stays in the list (stamp ""), so a caller can refuse on it.
    assert_eq!(rows.len(), 5, "{rows:?}");
    assert!(v["verdicts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["stamp"] == "" && r["reviewer"] == "rev-f"));
    let why: Vec<String> = v["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}: {}",
                r["name"].as_str().unwrap(),
                r["reason"].as_str().unwrap()
            )
        })
        .collect();
    let has = |n: &str, r: &str| why.iter().any(|w| w.contains(n) && w.contains(r));
    assert!(has("070300", "not the full head"), "{why:?}");
    assert!(has("070500", "does not name PR #5"), "{why:?}");
    assert!(has("070700", "symlink"), "{why:?}");
    // Another issue's note is neither a verdict nor a skip.
    assert!(!why.iter().any(|w| w.contains("070600")), "{why:?}");
}

#[test]
fn verdict_title_and_section_disagreement_is_not_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!("# Verdict: CAD-9 Standards review — pass\n> Issue: CAD-9\n> From: r\n\n## Verdict\nrevise — PR #5, head {HEAD}\n");
    std::fs::write(dir.path().join("20261001-070000-x-verdict.md"), text).unwrap();
    let v = verdicts_in(dir.path(), "CAD-9", 5, HEAD).unwrap();
    assert_eq!(v["verdicts"][0]["outcome"], "conflict");
}

#[test]
fn a_body_from_line_never_renames_the_reviewer() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!("# Verdict: CAD-9 Standards review — pass\n> Issue: CAD-9\n> From: author\n\n## Verdict\npass — PR #5, head {HEAD}\n\nFrom: rev-y\n");
    std::fs::write(dir.path().join("20261001-070000-x-verdict.md"), text).unwrap();
    let v = verdicts_in(dir.path(), "CAD-9", 5, HEAD).unwrap();
    assert_eq!(v["verdicts"][0]["reviewer"], "author");
}

#[test]
fn an_inline_verdict_line_cannot_override_the_section() {
    let dir = tempfile::tempdir().unwrap();
    // title pass, section revise, later inline "Verdict: pass".
    let text = format!("# Verdict: CAD-9 Standards review — pass\n> Issue: CAD-9\n> From: r\n\n## Verdict\nrevise — PR #5, head {HEAD}\n\nVerdict: pass (round 1)\n");
    std::fs::write(dir.path().join("20261001-070000-x-verdict.md"), text).unwrap();
    let v = verdicts_in(dir.path(), "CAD-9", 5, HEAD).unwrap();
    assert_eq!(v["verdicts"][0]["outcome"], "conflict");
    assert_eq!(v["verdicts"][0]["section_outcome"], "revise");
    // Title and inline agree against the section: still a conflict.
    let n = parse_note(Path::new("/x-verdict.md"), "x-verdict.md", &format!("# Verdict: CAD-9 Standards review — pass\n> Issue: CAD-9\n\n## Verdict\nrevise — head {HEAD}\n\nVerdict: pass\n"));
    assert_eq!(
        n.verdict.as_deref(),
        Some("revise"),
        "the section outranks an inline line"
    );
}

#[test]
fn title_section_and_inline_that_agree_are_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!("# Verdict: CAD-9 Standards review — pass\n> Issue: CAD-9\n> From: r\n\n## Verdict\npass — PR #5, head {HEAD}\n\nVerdict: pass\n");
    std::fs::write(dir.path().join("20261001-070000-x-verdict.md"), text).unwrap();
    let v = verdicts_in(dir.path(), "CAD-9", 5, HEAD).unwrap();
    assert_eq!(v["verdicts"][0]["outcome"], "pass");
}

#[test]
fn name_stamps_come_from_the_file_name_only() {
    assert_eq!(
        name_stamp("20261001-070151-x.md").as_deref(),
        Some("20261001070151")
    );
    assert_eq!(
        name_stamp("20261001T065132Z-x.md").as_deref(),
        Some("20261001065132")
    );
    assert_eq!(name_stamp("x-20261001-070151.md"), None);
    assert_eq!(name_stamp("2026100-070151-x.md"), None);
}

#[test]
fn approval_filter_ignores_other_actions() {
    let mut deploy = rec("a", HEAD, 1.0);
    deploy.action = "deploy".into();
    let e = ev(vec![deploy], &[]);
    assert_eq!(approval_in_force(&e, "x/y", 1, HEAD)["state"], "missing");
}

#[test]
fn pre_merge_approval_binds_only_the_exact_head_and_scope() {
    let in_force = |e: &StoreEvidence, repo: &str, pr: u64, head: &str| {
        approval_in_force(e, repo, pr, head)["state"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let e = ev(vec![rec("a", HEAD, 1.0)], &[]);
    let slug = e.approvals[0].repo.clone();
    assert_eq!(in_force(&e, &slug, 1, HEAD), "in-force");
    // An approval for an older head never covers a newer one.
    assert_eq!(in_force(&e, &slug, 1, OLD_HEAD), "missing");
    // Another PR or repo is out of scope.
    assert_eq!(in_force(&e, &slug, 2, HEAD), "missing");
    assert_eq!(in_force(&e, "other/repo", 1, HEAD), "missing");
    // A revoked approval is not in force, unless another one is.
    let revoked = ev(vec![rec("a", HEAD, 1.0)], &[("a", revoke(2.0))]);
    assert_eq!(in_force(&revoked, &slug, 1, HEAD), "revoked");
    let both = ev(
        vec![rec("a", HEAD, 1.0), rec("b", HEAD, 3.0)],
        &[("a", revoke(2.0))],
    );
    assert_eq!(in_force(&both, &slug, 1, HEAD), "in-force");
    // A store that cannot answer is unknown, never in force.
    let gap = StoreEvidence {
        approvals_gap: Some("no store".into()),
        ..Default::default()
    };
    assert_eq!(in_force(&gap, &slug, 1, HEAD), "unknown");
}

#[test]
fn scope_approvals_list_per_issue_with_revocation_and_gap() {
    let scope = |id: &str, issue: &str, via: Option<&str>| ScopeRec {
        id: id.into(),
        issue: issue.into(),
        digest: "d".repeat(64),
        source: "operator".into(),
        recorded_via: via.map(str::to_string),
    };
    let e = StoreEvidence {
        scopes: vec![
            scope("s1", "CAD-9", Some("operator-connection")),
            scope("s2", "CAD-9", None),
            scope("s3", "CAD-8", Some("operator-connection")),
        ],
        revocations: [("s2".to_string(), revoke(2.0))].into(),
        opened: true,
        ..Default::default()
    };
    let v = scope_approvals(&e, "cad-9");
    assert_eq!(v["state"], "ok");
    let rows = v["approvals"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{v}");
    assert_eq!(rows[0]["approval_id"], "s1");
    assert_eq!(rows[0]["revoked"], false);
    assert_eq!(rows[1]["revoked"], true);
    assert_eq!(rows[1]["recorded_via"], Value::Null);
    assert_eq!(scope_approvals(&e, "CAD-7")["approvals"], json!([]));
    let gap = StoreEvidence {
        approvals_gap: Some("no store".into()),
        ..Default::default()
    };
    assert_eq!(scope_approvals(&gap, "CAD-9")["state"], "unknown");
}

/// CAD-1106 against a real store: the verb reads what the daemon's
/// writers recorded (digest, recorded_via, revocation) and never mixes
/// in merge approvals or another ticket.
#[test]
fn scope_check_reads_real_store_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(&dir.path().join(STORE_FILE)).unwrap();
    let digest = crate::delegation::scope_digest("body\n");
    let (_, live) = store
        .record_scope_approval("CAD-9", &digest, "op", "operator-connection")
        .unwrap();
    let (_, agent) = store
        .record_scope_approval("CAD-9", &"f".repeat(64), "op", "agent:x")
        .unwrap();
    store
        .record_scope_approval("CAD-8", &digest, "op", "operator-connection")
        .unwrap();
    store
        .revoke_approval(&agent, "op", "why", "operator-connection")
        .unwrap();
    let (v, code) = scope_check(dir.path(), "CAD-9");
    assert_eq!(code, 0, "{v}");
    let rows = v["approvals"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{v}");
    assert_eq!(rows[0]["approval_id"], live.as_str());
    assert_eq!(rows[0]["digest"], digest.as_str());
    assert_eq!(rows[0]["recorded_via"], "operator-connection");
    assert_eq!(rows[0]["revoked"], false);
    assert_eq!(rows[1]["recorded_via"], "agent:x");
    assert_eq!(rows[1]["revoked"], true);
    // No store at all: unknown, exit 1.
    let (v, code) = scope_check(&dir.path().join("none"), "CAD-9");
    assert_eq!((v["state"].as_str(), code), (Some("unknown"), 1));
}

#[test]
fn approval_unknown_evidence_is_reported_not_flagged() {
    let gap = StoreEvidence {
        approvals_gap: Some("store /s unreadable".into()),
        ..Default::default()
    };
    let r = human(&gap, None);
    assert_eq!(r.approval.state, "unknown");
    assert!(r.flags.is_empty(), "{:?}", r.flags);
    assert!(r.unknowns.iter().any(|(f, _)| f == "approval"));

    // No landed head to bind: unknown too.
    let mut r = flagged_row();
    r.landed_head = None;
    r.class = Some("human".into());
    r.qa_verdict_status = Some("SUCCESS".into());
    enrich_approval(&ev(vec![], &[]), None, &mut r);
    flag_row(&mut r);
    assert_eq!(r.approval.state, "unknown");
    assert!(!r.flags.iter().any(|f| f.starts_with("approval")));

    // Non-human classes never flag: not-required without a record,
    // the record shown when one binds; an unknown class is unknown.
    let e = ev(vec![rec("a", HEAD, MERGED - 60.0)], &[]);
    for (class, want) in [
        (Some("auto"), "operator-claimed"),
        (None, "operator-claimed"),
    ] {
        let mut r = flagged_row();
        r.landed_head = Some(HEAD.into());
        r.merged_at = Some(MERGED);
        r.class = class.map(str::to_string);
        enrich_approval(&e, None, &mut r);
        assert_eq!(r.approval.state, want);
    }
    let empty = ev(vec![], &[]);
    for (class, want) in [(Some("notify"), "not-required"), (None, "unknown")] {
        let mut r = flagged_row();
        r.merged_at = Some(MERGED);
        r.class = class.map(str::to_string);
        enrich_approval(&empty, None, &mut r);
        flag_row(&mut r);
        assert_eq!(r.approval.state, want);
        assert!(!r.flags.iter().any(|f| f.starts_with("approval")));
        assert!(approval_line(&r).is_none());
    }
}

#[test]
fn flag_root_commit_exempt() {
    let mut r = Row {
        is_root: true,
        merge_sha: "ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab".into(),
        contains_head: "unknown (no head recorded)".into(),
        ..Default::default()
    };
    flag_row(&mut r);
    assert!(r.flags.is_empty());
}
