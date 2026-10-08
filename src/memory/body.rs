//! Memory body parsing: the fact block plus the `**Why:**` and
//! `**How to apply:**` sections every memory file carries. Extraction
//! and first-line helpers only — the contract caps (`FACT_MAX_BYTES`)
//! and the lint that enforces them stay in the parent.

/// The body contract: a fact block (≤5 non-empty lines) followed by
/// `**Why:**` and `**How to apply:**` markers. Returns
/// `(fact_lines, why, how)`; sections may span multiple lines.
pub(super) fn body_parts(body: &str) -> (Vec<String>, String, String) {
    let mut fact = Vec::new();
    let mut why = Vec::new();
    let mut how = Vec::new();
    let mut section = 0; // 0 = fact, 1 = why, 2 = how
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("**Why:**") {
            section = 1;
            if !rest.trim().is_empty() {
                why.push(rest.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("**How to apply:**") {
            section = 2;
            if !rest.trim().is_empty() {
                how.push(rest.trim().to_string());
            }
            continue;
        }
        match section {
            0 => {
                if !t.is_empty() || !fact.is_empty() {
                    fact.push(line.to_string());
                }
            }
            1 => why.push(line.to_string()),
            _ => how.push(line.to_string()),
        }
    }
    let trim = |v: &mut Vec<String>| {
        while v.first().is_some_and(|l| l.trim().is_empty()) {
            v.remove(0);
        }
        while v.last().is_some_and(|l| l.trim().is_empty()) {
            v.pop();
        }
    };
    trim(&mut fact);
    trim(&mut why);
    trim(&mut how);
    (fact, why.join("\n"), how.join("\n"))
}

/// The one-line fact used in lessons files and list views.
pub fn fact_line(body: &str) -> String {
    body_parts(body).0.first().cloned().unwrap_or_default()
}

/// The first `**How to apply:**` line — briefings and lessons carry it.
pub fn apply_line(body: &str) -> String {
    let (_, _, how) = body_parts(body);
    how.lines().next().unwrap_or_default().trim().to_string()
}
