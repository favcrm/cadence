//! CAD-1298: the ONE shared delivery-requirements evaluator. Given the
//! resolved delivery policy ([`delivery_policy::effective`]'s output —
//! approved or defaulted, never a raw file) and trusted diff evidence —
//! the same `(status, old_mode, new_mode, path)` rows plus changed lines
//! that `git diff --raw -z --no-renames <merge-base> <head>` and
//! `--numstat` produce — it answers what a delivery needs to merge:
//! how many independent reviews and of which kind, whether an exact-head
//! operator approval is required, and whether the full PR check floor
//! applies.
//!
//! The evaluator is pure: it never reads files, never calls `gh`, and
//! never accepts a caller-supplied class or a requirements JSON — every
//! input is evidence the caller retrieved itself (the daemon's own read
//! or the enqueue script's git/gh reads). Trusted-base lists
//! (`docs/roles/risk-paths.toml`, `docs/roles/one-review-paths.toml`)
//! arrive as texts read at the PR's base, so a PR can never classify
//! itself under rules it edited.
//!
//! Classes: `sensitive` (a risk-trigger/human/excluded path, or an
//! oversize diff — full checks, one independent security-capable
//! combined review plus a real exact-head operator approval),
//! `consequential` (known ordinary paths — one independent combined
//! review), `routine` (the narrow allowlist — required base CI plus
//! ticket outcome evidence, zero mandatory reviews, no fabricated
//! PASS), and `strict` — not a class the profile grants but the
//! fail-closed fallback: profile inactive/absent/malformed, lists
//! unreadable, or diff evidence incomplete (deletes, renames, mode or
//! type changes, unknown top-level paths, empty diff). `strict` reports
//! exactly today's legacy requirements — one combined review on the
//! one-review list else two (Standards + Spec/security), the operator
//! floor on human-class paths/size — never fewer.

use serde::{Deserialize, Serialize};

use crate::issue::delivery_policy::{default_policy, Resolved};

/// One changed file of the PR diff — the shape `git diff --raw -z
/// --no-renames` emits and `scripts/enqueue-reviewed` collects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The raw status letter: `A`, `M`, `D`, `R`, `C`, `T`, `U`, `X`, `B`.
    pub status: String,
    /// Octal mode text (`"100644"`); `"000000"` on the absent side of an add.
    pub old_mode: String,
    pub new_mode: String,
    /// Repo-root-relative path.
    pub path: String,
}

/// The trusted-base list texts, read at the PR's base commit (the same
/// reads `scripts/enqueue-reviewed` makes through
/// `gh api repos/<repo>/contents/<file>?ref=<base>`). `None` means
/// unreadable — which fails closed, never to a weaker requirement.
#[derive(Clone, Copy, Debug, Default)]
pub struct TrustedLists<'a> {
    pub risk_paths: Option<&'a str>,
    pub one_review: Option<&'a str>,
}

/// Whether an approved solo-operator profile is in force, and when it
/// is not, why not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    /// `source == "approved"` and the approved policy carries a valid
    /// `solo_operator` profile.
    Active,
    /// The in-force policy carries no `solo_operator` section.
    NoProfile,
    /// A `delivery:` section exists but is not the approved one
    /// (`delivery_unapproved`) — the legacy requirements stay.
    NotApproved,
    /// The section is malformed with no approved policy
    /// (`delivery_error`) — the defaults apply, legacy requirements.
    Malformed,
}

/// What class the evidence places the delivery in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryClass {
    /// Activated profile, allowlisted ordinary documentation only.
    Routine,
    /// Activated profile, ordinary listed paths.
    Consequential,
    /// Activated profile, a sensitive boundary matched — or a human
    /// trigger the legacy loop already requires the operator for.
    Sensitive,
    /// No relaxation applies: the profile is not in force, the trusted
    /// lists are unavailable, or the diff evidence is incomplete. The
    /// requirements are exactly the legacy ones.
    Strict,
}

/// The kind of independent review `reviews` counts toward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    /// No review required (routine under an activated profile only).
    None,
    /// One combined `Review (standards+spec)` verdict.
    Combined,
    /// Two verdicts — Standards and Spec/security — by distinct
    /// reviewers.
    StandardsAndSpec,
}

/// What a delivery needs to merge, derived from the class.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Requirements {
    pub class: DeliveryClass,
    /// Independent verdict count on the exact head: `0` only for
    /// routine under an activated profile.
    pub reviews: u32,
    pub review_kind: ReviewKind,
    /// The combined review must be filed by a security-capable
    /// reviewer (sensitive class).
    pub security_capable: bool,
    /// A `ui/` change additionally needs a Browser QA pass.
    pub browser_qa: bool,
    /// An exact-head operator-connection approval is required.
    pub operator_approval: bool,
    /// The full PR check floor applies. `false` only for routine —
    /// where the required base-branch checks are still all required.
    pub full_checks: bool,
    /// Machine-readable reasons: why this class, what evidence decided.
    pub reasons: Vec<String>,
    /// Digest of the policy the requirements were computed under —
    /// evidence binds to it and a policy change invalidates them.
    pub policy_digest: String,
    pub activation: Activation,
}

/// The initial routine allowlist (CAD-1298, deliberately narrow):
/// ordinary `docs/**/*.md` plus the two top-level project docs. Every
/// routine path must additionally be a regular file (`100644`, never
/// `100755` — an executable doc is not routine), status `A`/`M` with no
/// mode change, and survive the sensitive check (policy/role/security
/// docs are in `risk-paths.toml` `trigger7` and excluded before they
/// can read as routine).
const ROUTINE_EXACT: &[&str] = &["README.md", "CONTRIBUTING.md"];

/// Legacy human-class path triggers — the same lists
/// `scripts/enqueue-reviewed` encodes (AGENTS.md and
/// `docs/roles/risk-classes.md` triggers 4/6/7). This module is the one
/// Rust home for them; the script keeps its copy until it consumes the
/// shared evaluator.
const HUMAN_PREFIXES: &[&str] = &[
    ".github/",
    "scripts/",
    "docs/roles/",
    ".config/",
    ".cargo/",
    "src/audit/",
    "src/delegation/",
    "tests/common/",
    "config/",
];
const HUMAN_EXACT: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "ui/package.json",
    "cadence-review.toml",
    "src/review.rs",
    "docs/TEAM.md",
    "docs/CHARTER.md",
    "AGENTS.md",
    "build.rs",
    "rust-toolchain.toml",
    "clippy.toml",
    "src/rollout.rs",
    "src/update.rs",
    "src/upgrade.rs",
    "src/cli/rollout.rs",
    "src/cli/daemon.rs",
    "src/cli/update.rs",
    "src/cli/upgrade.rs",
    "src/audit.rs",
    "src/delegation.rs",
    "src/issue/delivery_policy.rs",
    "src/issue/delivery_requirements.rs",
    "docs/AUDIT.md",
    "tests/safety_floor.rs",
];
const HUMAN_BASENAMES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "pnpm-lock.yaml",
    "package-lock.json",
    ".npmrc",
    "yarn.lock",
    "bun.lockb",
    "build.rs",
    "rust-toolchain.toml",
];
/// risk-classes trigger 5: a diff over this many changed lines is
/// human-class.
const HUMAN_LINES: u64 = 1500;
const REGULAR_MODES: &[&str] = &["100644", "100755"];

/// The `risk-paths.toml` tables whose paths make a delivery sensitive
/// under the profile — and human-class under the legacy rules. Every
/// required table must parse; a missing one fails closed.
const SENSITIVE_TABLES: &[&str] = &[
    "schema", "trigger1", "trigger3", "trigger4", "trigger6", "trigger7",
];

/// The parsed trusted-base lists. `None` anywhere refuses the parse —
/// a partial list is never matched as if complete.
struct Lists {
    /// `risk-paths.toml` [`SENSITIVE_TABLES`] globs (matched with
    /// [`crate::review::glob_match`], the grammar that file declares).
    sensitive: Vec<String>,
    human: Vec<String>,
    one_review_include: Vec<String>,
    one_review_exclude: Vec<String>,
}

fn parse_lists(lists: &TrustedLists) -> Option<Lists> {
    let risk: toml::Value = toml::from_str(lists.risk_paths?).ok()?;
    let one: toml::Value = toml::from_str(lists.one_review?).ok()?;
    let strings = |doc: &toml::Value, keys: &[&str]| -> Option<Vec<String>> {
        let mut node = doc;
        for k in keys {
            node = node.get(k)?;
        }
        node.as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let mut sensitive = Vec::new();
    let mut human = Vec::new();
    for table in SENSITIVE_TABLES {
        let paths = strings(&risk, &[table, "paths"])?;
        for path in &paths {
            crate::issue::areas::check_path(path).ok()?;
        }
        if matches!(*table, "schema" | "trigger4" | "trigger6" | "trigger7") {
            human.extend(paths.iter().cloned());
        }
        sensitive.extend(paths);
    }
    let include = strings(&one, &["one_review_include"])?;
    let exclude = strings(&one, &["one_review_exclude"])?;
    for path in include.iter().chain(&exclude) {
        crate::issue::areas::check_path(path).ok()?;
    }
    Some(Lists {
        sensitive,
        human,
        one_review_include: include,
        one_review_exclude: exclude,
    })
}

// ---- one-review glob grammar (docs/roles/one-review-paths.toml) -------
// `*` matches zero or more non-`/` bytes inside one segment; `**` as a
// WHOLE segment matches zero or more whole segments; `?`, `[` and every
// other byte is literal. A pattern without `**` anchors to exactly its
// segment count — this is NOT `areas::matches` (a plain path there
// covers a directory; here it is one file).

fn one_seg(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut c) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while c < s.len() {
        match pat.get(p) {
            Some(b'*') => {
                star = Some((p + 1, c));
                p += 1;
            }
            Some(&pc) if pc == s[c] => {
                p += 1;
                c += 1;
            }
            _ => match star {
                Some((sp, sc)) => {
                    p = sp;
                    c = sc + 1;
                    star = Some((p, c));
                }
                None => return false,
            },
        }
    }
    while pat.get(p) == Some(&b'*') {
        p += 1;
    }
    p == pat.len()
}

fn one_glob(glob: &str, path: &str) -> bool {
    let gs: Vec<&str> = glob.split('/').collect();
    let ps: Vec<&str> = path.split('/').collect();
    fn go(gs: &[&str], ps: &[&str], i: usize, j: usize) -> bool {
        if i == gs.len() {
            return j == ps.len();
        }
        if gs[i] == "**" {
            return (j..=ps.len()).any(|k| go(gs, ps, i + 1, k));
        }
        j < ps.len() && one_seg(gs[i].as_bytes(), ps[j].as_bytes()) && go(gs, ps, i + 1, j + 1)
    }
    go(&gs, &ps, 0, 0)
}

// ---- diff evidence ---------------------------------------------------

/// Why this change set cannot carry a classification, if it cannot.
/// Mirrors the disqualifiers `one_review_qualifies` and
/// `pr_changes` apply: empty diff, non-`A`/`M` status, non-regular or
/// changed modes, malformed mode text, unsafe path shape.
fn evidence_refusal(changes: &[Change]) -> Option<String> {
    if changes.is_empty() {
        return Some("the diff evidence is empty".to_string());
    }
    for c in changes {
        if c.status.len() != 1 || !"AMDRTCUXB".contains(&c.status) {
            return Some(format!(
                "change {:?} has an unknown status {:?}",
                c.path, c.status
            ));
        }
        for m in [&c.old_mode, &c.new_mode] {
            if m.len() != 6 || !m.bytes().all(|b| b.is_ascii_digit()) {
                return Some(format!("change {:?} has a malformed mode {m:?}", c.path));
            }
        }
        let p = c.path.as_str();
        if p.is_empty()
            || p.starts_with('/')
            || p.contains('\\')
            || p.contains("..")
            || p.chars().any(|ch| ch.is_control())
            || p.split('/').any(|s| s.is_empty() || s == ".")
        {
            return Some(format!("change has an unsafe path {:?}", c.path));
        }
    }
    None
}

/// Can this single change sit on the one-review list — status `A`/`M`,
/// regular modes, no mode change?
fn reviewable_change(c: &Change) -> bool {
    match c.status.as_str() {
        "A" => c.old_mode == "000000" && REGULAR_MODES.contains(&c.new_mode.as_str()),
        "M" => c.old_mode == c.new_mode && REGULAR_MODES.contains(&c.new_mode.as_str()),
        _ => false,
    }
}

/// CAD-957, verbatim semantics of `scripts/enqueue-reviewed`'s
/// `one_review_qualifies`: every change is a reviewable add/modify whose
/// path matches an include and no exclude (exclude wins).
fn one_review_qualifies(changes: &[Change], lists: &Lists) -> bool {
    !changes.is_empty()
        && evidence_refusal(changes).is_none()
        && changes.iter().all(|c| {
            reviewable_change(c)
                && lists
                    .one_review_include
                    .iter()
                    .any(|g| one_glob(g, &c.path))
                && !lists
                    .one_review_exclude
                    .iter()
                    .any(|g| one_glob(g, &c.path))
        })
}

/// Legacy human-class by path: the prefix/exact/basename lists plus the
/// sensitive `risk-paths` globs (matched with that file's own grammar,
/// `crate::review::glob_match`).
fn human_path(path: &str, lists: Option<&Lists>) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    let segs: Vec<&str> = path.split('/').collect();
    if HUMAN_EXACT.contains(&path)
        || HUMAN_PREFIXES.iter().any(|p| path.starts_with(p))
        || HUMAN_BASENAMES.contains(&base)
        || segs[..segs.len() - 1]
            .iter()
            .any(|s| *s == ".github" || *s == ".cargo")
    {
        return true;
    }
    match lists {
        // Unreadable lists fail closed: treat the path as human.
        None => true,
        Some(l) => l.human.iter().any(|g| crate::review::glob_match(g, path)),
    }
}

/// The `docs/**` ordinary-markdown admit rule of the routine allowlist.
fn routine_path(path: &str) -> bool {
    (path.starts_with("docs/") || ROUTINE_EXACT.contains(&path)) && path.ends_with(".md")
}

fn activation_of(resolved: &Resolved) -> Activation {
    if resolved.policy.validate().is_err()
        || resolved.digest != crate::issue::delivery_policy::digest(&resolved.policy)
    {
        return Activation::Malformed;
    }
    if resolved.source == "approved" && resolved.policy.solo_operator.is_some() {
        return Activation::Active;
    }
    let note = resolved.note.as_deref().unwrap_or("");
    if note.starts_with("delivery_error") {
        Activation::Malformed
    } else if note.starts_with("delivery_unapproved") {
        Activation::NotApproved
    } else {
        Activation::NoProfile
    }
}

/// The requirements today's loop demands — the `strict` answer. `lists`
/// may be partially absent: each absent input fails closed the same way
/// `scripts/enqueue-reviewed` does (an unreadable one-review list keeps
/// two reviews; unreadable risk lists make every path human; verdict
/// notes declaring `Risk: human` still add the operator check
/// downstream, noted in `reasons`).
fn strict_requirements(
    resolved: &Resolved,
    activation: Activation,
    changes: &[Change],
    lines: u64,
    lists: Option<&Lists>,
    mut reasons: Vec<String>,
) -> Requirements {
    let qualifies = lists.is_some_and(|l| one_review_qualifies(changes, l));
    if lists.is_none() {
        reasons.push(
            "the trusted-base lists are unreadable — two reviews and the operator floor hold"
                .to_string(),
        );
    }
    let human_paths: Vec<&str> = changes
        .iter()
        .map(|c| c.path.as_str())
        .filter(|p| human_path(p, lists))
        .collect();
    let operator = !human_paths.is_empty() || lines > HUMAN_LINES || lists.is_none();
    if !human_paths.is_empty() {
        reasons.push(format!(
            "human-class paths: {}",
            human_paths[..human_paths.len().min(5)].join(", ")
        ));
    }
    if lines > HUMAN_LINES {
        reasons.push(format!("{lines} changed lines is over {HUMAN_LINES}"));
    }
    reasons.push(
        "a head-pinned verdict whose Risk is not `auto` additionally forces the \
         operator check (evaluated by enqueue on the notes, not here)"
            .to_string(),
    );
    Requirements {
        class: DeliveryClass::Strict,
        reviews: if qualifies { 1 } else { 2 },
        review_kind: if qualifies {
            ReviewKind::Combined
        } else {
            ReviewKind::StandardsAndSpec
        },
        security_capable: false,
        browser_qa: changes.iter().any(|c| c.path.starts_with("ui/")),
        operator_approval: operator,
        full_checks: true,
        reasons,
        policy_digest: resolved.digest.clone(),
        activation,
    }
}

/// The evaluator — see the module header. `resolved` is the effective
/// policy (approved or defaulted, with its digest); `changes`+`lines`
/// are the trusted diff evidence; `lists` the trusted-base list texts.
/// No input carries a class: the evidence alone decides.
pub fn classify(
    resolved: &Resolved,
    changes: &[Change],
    lines: u64,
    lists: &TrustedLists,
) -> Requirements {
    let activation = activation_of(resolved);
    let parsed = parse_lists(lists);
    let mut reasons = Vec::new();

    if activation != Activation::Active {
        reasons.push(format!(
            "no approved solo-operator profile is in force ({activation:?}) — \
             the legacy requirements apply"
        ));
        return strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            parsed.as_ref(),
            reasons,
        );
    }
    reasons.push("approved solo-operator profile in force".to_string());

    let Some(lists) = parsed else {
        reasons.push("the trusted-base lists could not be read or parsed".to_string());
        return strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            parsed.as_ref(),
            reasons,
        );
    };
    if let Some(why) = evidence_refusal(changes) {
        reasons.push(format!("incomplete diff evidence: {why}"));
        return strict_requirements(resolved, activation, changes, lines, Some(&lists), reasons);
    }

    if changes.iter().any(|c| !reviewable_change(c)) {
        reasons.push(
            "deletion, rename, type or mode change requires legacy strict review".to_string(),
        );
        return strict_requirements(resolved, activation, changes, lines, Some(&lists), reasons);
    }

    // Sensitive wins over everything: risk triggers, the human lists,
    // the one-review exclude paths (trust boundary/identity/secrets —
    // the same paths the two-review floor guards) and the size trigger.
    let sensitive: Vec<&str> = changes
        .iter()
        .map(|c| c.path.as_str())
        .filter(|p| {
            human_path(p, Some(&lists))
                || lists
                    .sensitive
                    .iter()
                    .any(|g| crate::review::glob_match(g, p))
                || lists.one_review_exclude.iter().any(|g| one_glob(g, p))
        })
        .collect();
    if !sensitive.is_empty() || lines > HUMAN_LINES {
        if !sensitive.is_empty() {
            reasons.push(format!(
                "sensitive paths matched: {}",
                sensitive[..sensitive.len().min(5)].join(", ")
            ));
        }
        if lines > HUMAN_LINES {
            reasons.push(format!("{lines} changed lines is over {HUMAN_LINES}"));
        }
        reasons.push("sensitive or policy-triggering changes retain the existing strict review, operator, Browser QA and full-check floors".to_string());
        return strict_requirements(resolved, activation, changes, lines, Some(&lists), reasons);
    }

    // Anything outside the known include list — or a change shape the
    // list grammar refuses (delete, rename, mode/type change) — falls
    // back to strict rather than guessing.
    if !one_review_qualifies(changes, &lists) {
        reasons.push(
            "a path or change shape is outside the known one-review include list".to_string(),
        );
        return strict_requirements(resolved, activation, changes, lines, Some(&lists), reasons);
    }

    // Routine: every change a regular 100644 `A`/`M` ordinary-doc
    // markdown file. An executable doc, a non-markdown doc or any other
    // extension is consequential, never routine.
    if changes
        .iter()
        .all(|c| reviewable_change(c) && c.new_mode == "100644" && routine_path(&c.path))
    {
        reasons.push("every change is ordinary documentation on the routine allowlist".to_string());
        return Requirements {
            class: DeliveryClass::Routine,
            reviews: 0,
            review_kind: ReviewKind::None,
            security_capable: false,
            browser_qa: false,
            operator_approval: false,
            full_checks: false,
            reasons,
            policy_digest: resolved.digest.clone(),
            activation,
        };
    }

    reasons.push("known ordinary paths — one independent combined review".to_string());
    Requirements {
        class: DeliveryClass::Consequential,
        reviews: 1,
        review_kind: ReviewKind::Combined,
        security_capable: false,
        browser_qa: changes.iter().any(|c| c.path.starts_with("ui/")),
        operator_approval: false,
        full_checks: true,
        reasons,
        policy_digest: resolved.digest.clone(),
        activation,
    }
}

/// `Resolved` for a plain default policy — a convenience for callers
/// (the enqueue inspector) that have no file or approval context and
/// want the documented "no profile" answer.
pub fn default_resolved() -> Resolved {
    Resolved {
        policy: default_policy(),
        source: "default",
        digest: crate::issue::delivery_policy::digest(&default_policy()),
        note: None,
    }
}

/// A change shape helper for callers that collected `git diff --raw -z
/// --no-renames` fields (status letter, old mode, new mode, path).
pub fn change(status: &str, old_mode: &str, new_mode: &str, path: &str) -> Change {
    Change {
        status: status.to_string(),
        old_mode: old_mode.to_string(),
        new_mode: new_mode.to_string(),
        path: path.to_string(),
    }
}
