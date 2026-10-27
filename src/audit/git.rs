//! Git.

use super::*;

/// Every subprocess runs under `run_bounded` — a hung `git` or `gh`
/// cannot stall the audit.
pub(super) fn git(repo: &Path, args: &[String]) -> std::result::Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    let out = crate::proc::run_bounded(&mut cmd, GIT_TIMEOUT)
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `origin/HEAD` symref, else origin/main, origin/master, then local.
pub(super) fn default_ref(repo: &Path) -> Result<String> {
    if let Ok(sym) = git(
        repo,
        &["symbolic-ref".into(), "refs/remotes/origin/HEAD".into()],
    ) {
        let sym = sym.trim().to_string();
        if let Some(short) = sym.strip_prefix("refs/remotes/") {
            return Ok(short.to_string());
        }
    }
    for cand in ["origin/main", "origin/master", "main", "master"] {
        if git(repo, &["rev-parse".into(), "--verify".into(), cand.into()]).is_ok() {
            return Ok(cand.to_string());
        }
    }
    Err(Error::rejected(
        "no default branch found (origin/HEAD, origin/main, origin/master, main, master)",
    ))
}

/// `sha \x1f committer-epoch \x1f parent-count \x1f subject` per
/// first-parent commit on the ref — a real merge yields its merge
/// commit, not one row per side-branch commit.
pub(super) fn merge_commits(
    repo: &Path,
    reference: &str,
) -> Result<Vec<(String, i64, usize, String)>> {
    let out = git(
        repo,
        &[
            "log".into(),
            "--first-parent".into(),
            reference.into(),
            "--format=%H%x1f%ct%x1f%P%x1f%s".into(),
        ],
    )
    .map_err(Error::rejected)?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\x1f');
            Some((
                p.next()?.to_string(),
                p.next()?.parse().ok()?,
                p.next()?.split(' ').filter(|s| !s.is_empty()).count(),
                p.next().unwrap_or("").to_string(),
            ))
        })
        .collect())
}

/// The PR number from a merge subject: `Merge pull request #N …` for
/// real merges, `(#N)` anywhere for squashes (the last occurrence — a
/// trailing ` (rebased)` marker does not hide it).
pub(super) fn pr_number(subject: &str) -> Option<u64> {
    if let Some(rest) = subject.trim_start().strip_prefix("Merge pull request #") {
        return rest
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok();
    }
    subject
        .rmatch_indices("(#")
        .filter_map(|(i, _)| {
            let tail = &subject[i + 2..];
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            (tail.as_bytes().get(digits.len()) == Some(&b')')).then(|| digits.parse().ok())?
        })
        .next()
}

/// Ticket ids (`PREFIX-NN`, any registered project prefix) mentioned in
/// `text`, deduplicated in encounter order. A token is a maximal run of
/// ASCII letters, digits, `-` and `_`; one balanced pair of outer underscores
/// (markdown emphasis `_AOS-225_`) is stripped, an unmatched one is not. It
/// binds only when its prefix passes `check_prefix` (1-8 uppercase
/// letters/digits, first a letter) and the rest is one hyphen plus nonempty
/// ASCII digits. Lowercase, overlong, partial or suffixed runs (`xAOS-225`,
/// `AOS-225x`, `AOS-225_suffix`, `_AOS-225`) bind nothing; other surrounding
/// punctuation does not stop a match.
pub(super) fn issue_ids(text: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for run in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        let token = run
            .strip_prefix('_')
            .and_then(|r| r.strip_suffix('_'))
            .unwrap_or(run);
        let Some((prefix, number)) = token.split_once('-') else {
            continue;
        };
        let bound = cadence_core::issue::check_prefix(prefix).is_ok()
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit());
        if bound && !ids.iter().any(|id| id == token) {
            ids.push(token.to_string());
        }
    }
    ids
}

pub(super) fn row_from_subject(merge_sha: &str, at: i64, parents: usize, subject: &str) -> Row {
    let pr = pr_number(subject);
    let title = match (pr, subject.rfind(" (#")) {
        (Some(_), Some(i)) => subject[..i].to_string(),
        _ => subject.to_string(),
    };
    let mut row = Row {
        pr,
        is_root: parents == 0,
        title,
        merge_sha: merge_sha.to_string(),
        merged_at: Some(at as f64),
        issues: issue_ids(subject),
        contains_head: "unknown (not checked)".into(),
        tree_match: "unknown (not checked)".into(),
        smoke: "unknown (no smoke record)".into(),
        daemon_restart: "unknown (no restart record)".into(),
        revert: "no".into(),
        ..Default::default()
    };
    if row.pr.is_none() {
        row.unknowns
            .push(("pr".into(), "subject has no (#NN) suffix".into()));
    }
    row
}
