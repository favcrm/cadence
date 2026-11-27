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
//! arrive as texts read at the PR's anchor (the default branch's live tip for
//! a PR based on it, else the PR's base), so a PR can never classify
//! itself under rules it edited.
//!
//! `classify` returns one of three classes:
//!
//! - `routine` — the narrow allowlist: required base CI plus ticket
//!   outcome evidence, zero mandatory reviews, no fabricated PASS.
//! - `consequential` — known ordinary paths: one independent combined
//!   review and the full check floor, no operator approval. A repository
//!   that declares no trusted lists gets this class plus the operator's
//!   approval ([`classify_without_lists`], the product default).
//! - `strict` — the fail-closed floor: an activation other than the
//!   default lean policy (a malformed section, a custom section that is
//!   not operator-approved or was removed after approval, or a policy that
//!   is not the default once its solo-operator and integration-branch
//!   fields are cleared), lists unreadable, incomplete or unsafe diff
//!   evidence, a delete/rename/type/mode change, a path or change shape
//!   outside the known one-review include list, or a risk-trigger,
//!   human-class, excluded or oversize path. It reports the legacy
//!   requirements: one combined review on the one-review list else two
//!   (Standards + Spec/security), and the operator floor on human-class
//!   paths and over `HUMAN_LINES` changed lines. With unreadable lists it
//!   asks no Browser QA (operator decision 2026-11-17, CAD-2386): the
//!   operator floor they force is the human check.
//!
//! An owner-approved custom delivery policy (CAD-2413) is evaluated instead:
//! its applying `risk` rules name the reviews (`required_reviews`,
//! `required_checks`) and the class is `consequential`, but every strict
//! condition above except "outside the one-review include list" still
//! applies as a floor that the policy cannot lower (operator decision D1,
//! 2026-11-17).
//!
//! `DeliveryClass::Sensitive` exists in the type but `classify` does not
//! currently return it: sensitive conditions resolve to `strict`. No class
//! sets `security_capable` today.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::delivery::{default_policy, Resolved};

/// One changed file of the PR diff — the shape `git diff --raw -z
/// --no-renames` emits and `scripts/enqueue-reviewed` collects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Change {
    /// The raw status letter: `A`, `M`, `D`, `R`, `C`, `T`, `U`, `X`, `B`.
    pub status: String,
    /// Octal mode text (`"100644"`); `"000000"` on the absent side of an add.
    pub old_mode: String,
    pub new_mode: String,
    /// Repo-root-relative path.
    pub path: String,
}

/// The trusted list texts, read at the PR's anchor commit (the same
/// reads `scripts/enqueue-reviewed` makes through
/// `gh api repos/<repo>/contents/<file>?ref=<anchor>`). `None` means
/// unreadable — which fails closed to the strict floor with the operator.
#[derive(Clone, Copy, Debug, Default)]
pub struct TrustedLists<'a> {
    pub risk_paths: Option<&'a str>,
    pub one_review: Option<&'a str>,
}

/// Whether default lean classification applies, and if not, why strict
/// requirements are retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    /// A valid default-equivalent policy is in force.
    Active,
    /// The policy is custom or its provenance is unknown. The serialized
    /// name is retained for compatibility with existing requirements readers.
    NoProfile,
    /// A `delivery:` section exists but is not the approved one
    /// (`delivery_unapproved`) — the legacy requirements stay.
    NotApproved,
    /// The section is malformed with no approved policy
    /// (`delivery_error`) — the defaults apply, legacy requirements.
    Malformed,
}

/// What class the evidence places the delivery in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryClass {
    /// Activated profile, allowlisted ordinary documentation only.
    Routine,
    /// Activated profile, ordinary listed paths.
    Consequential,
    /// Reserved: `classify` resolves sensitive conditions to `Strict` and
    /// does not currently return this variant.
    Sensitive,
    /// No relaxation applies: the profile is not in force, the trusted
    /// lists are unavailable, or the diff evidence is incomplete. The
    /// requirements are the legacy ones (see the module header).
    Strict,
}

/// The kind of independent review `reviews` counts toward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    /// No review required (routine under a valid default-equivalent policy).
    None,
    /// One combined `Review (standards+spec)` verdict.
    Combined,
    /// Two verdicts — Standards and Spec/security — by distinct
    /// reviewers.
    StandardsAndSpec,
    /// CAD-2413: an approved policy requires only the Standards review.
    Standards,
    /// CAD-2413: an approved policy requires only the Spec/security review.
    SpecSecurity,
}

/// CAD-2413: one review an approved policy's applying rules require, so a
/// gate can count it by kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequiredReview {
    /// The review's name in the policy's `reviews` map.
    pub name: String,
    /// `agent`, `check` or `operator`.
    pub kind: String,
    /// An `agent` review's focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// An owner-nominated profile for this slot; actual review authorization
    /// still depends on the host's assignment and runtime checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// A `check` review's GitHub App.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// A `check` review's mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// CAD-2413: a GitHub App check an approved policy requires on the head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequiredCheck {
    pub name: String,
    pub app: String,
}

/// What a delivery needs to merge, derived from the class.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Requirements {
    pub class: DeliveryClass,
    /// Independent verdict count on the exact head: `0` for routine under a
    /// valid default-equivalent policy, or when an approved policy's applying
    /// rules name no code review (CAD-2413).
    pub reviews: u32,
    pub review_kind: ReviewKind,
    /// The combined review must be filed by a security-capable
    /// reviewer (sensitive class).
    pub security_capable: bool,
    /// A change to a path in the repository's `browser_qa` table needs a
    /// Browser QA pass (never when the lists are unreadable).
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
    /// CAD-2413: the reviews an approved custom policy's applying rules
    /// require, in rule order. Absent when no approved policy was evaluated,
    /// so a gate can tell "the policy requires none" from "no policy".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_reviews: Option<Vec<RequiredReview>>,
    /// CAD-2413: the GitHub App checks those rules require; absent likewise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_checks: Option<Vec<RequiredCheck>>,
}

// CAD-2386: this module carries no repository's path rules. The routine
// allowlist, the operator floor and the Browser QA paths are tables of the
// repository's own trusted `risk-paths.toml` (`routine`, `operator`,
// `browser_qa`), read at the anchor like the trigger tables. A missing or
// malformed table fails closed.

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

/// CAD-2386: the repository's operator floor, Browser QA and routine tables.
/// Required like [`SENSITIVE_TABLES`]: a missing one refuses the parse.
const OPERATOR_TABLE: &str = "operator";
const BROWSER_QA_TABLE: &str = "browser_qa";
const ROUTINE_TABLE: &str = "routine";

/// The parsed trusted lists (read at the diff's anchor: main's live tip for a
/// main-based PR, else the PR's base). `None` anywhere refuses the parse —
/// a partial list is never matched as if complete.
struct Lists {
    /// `risk-paths.toml` [`SENSITIVE_TABLES`] globs (matched with
    /// [`crate::delivery_rules::glob_match`], the grammar that file declares).
    sensitive: Vec<String>,
    human: Vec<String>,
    /// The `operator` table: the repository's operator-class paths. It may
    /// repeat trigger-table entries; both are human-class.
    operator: Vec<String>,
    /// The `browser_qa` table: paths whose change needs a Browser QA pass.
    browser_qa: Vec<String>,
    /// The `routine` table: ordinary documentation the routine class admits.
    routine: Vec<String>,
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
    // One `risk-paths.toml` table's checked globs; any defect refuses.
    let table = |name: &str| -> Option<Vec<String>> {
        let paths = strings(&risk, &[name, "paths"])?;
        for path in &paths {
            crate::delivery_rules::check_path(path).ok()?;
        }
        Some(paths)
    };
    let mut sensitive = Vec::new();
    let mut human = Vec::new();
    for name in SENSITIVE_TABLES {
        let paths = table(name)?;
        if matches!(*name, "schema" | "trigger4" | "trigger6" | "trigger7") {
            human.extend(paths.iter().cloned());
        }
        sensitive.extend(paths);
    }
    let operator = table(OPERATOR_TABLE)?;
    let browser_qa = table(BROWSER_QA_TABLE)?;
    let routine = table(ROUTINE_TABLE)?;
    let include = strings(&one, &["one_review_include"])?;
    let exclude = strings(&one, &["one_review_exclude"])?;
    for path in include.iter().chain(&exclude) {
        crate::delivery_rules::check_path(path).ok()?;
    }
    Some(Lists {
        sensitive,
        human,
        operator,
        browser_qa,
        routine,
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

/// Does any glob of `globs` (this file's grammar) match `path`?
fn any_glob<'a>(mut globs: impl Iterator<Item = &'a String>, path: &str) -> bool {
    globs.any(|g| crate::delivery_rules::glob_match(g, path))
}

/// Human-class by path: the repository's `operator` table plus its human
/// trigger tables (matched with that file's own grammar,
/// `crate::delivery_rules::glob_match`).
fn human_path(path: &str, lists: Option<&Lists>) -> bool {
    match lists {
        // Unreadable lists fail closed: treat the path as human.
        None => true,
        Some(l) => any_glob(l.operator.iter().chain(&l.human), path),
    }
}

/// Does any change need a Browser QA pass? The repository's `browser_qa`
/// table decides. Unreadable lists require none (operator decision
/// 2026-11-17, CAD-2386): they already force the operator's approval, which
/// is the human check there.
fn browser_qa_needed(changes: &[Change], lists: Option<&Lists>) -> bool {
    match lists {
        None => false,
        Some(l) => changes
            .iter()
            .any(|c| any_glob(l.browser_qa.iter(), &c.path)),
    }
}

/// The repository's `routine` table: ordinary documentation.
fn routine_path(path: &str, lists: &Lists) -> bool {
    any_glob(lists.routine.iter(), path)
}

pub(crate) fn activation_of(resolved: &Resolved) -> Activation {
    if resolved.policy.validate().is_err()
        || resolved.digest != crate::delivery::digest(&resolved.policy)
    {
        return Activation::Malformed;
    }
    let note = resolved.note.as_deref().unwrap_or("");
    if note.starts_with("delivery_error") {
        return Activation::Malformed;
    }
    if note.starts_with("delivery_unapproved") {
        return Activation::NotApproved;
    }

    let mut policy = resolved.policy.clone();
    policy.solo_operator = None;
    // CAD-1561: the integration target widens which PR base the policy
    // admits — it does not change the requirements level. An approved
    // default-equivalent-plus-target policy stays Active.
    policy.integration_branch = None;
    policy.ci_only = None;
    if matches!(resolved.source, "default" | "file" | "approved") && policy == default_policy() {
        Activation::Active
    } else {
        Activation::NoProfile
    }
}

/// The requirements today's loop demands — the `strict` answer. `lists`
/// may be unreadable: an unreadable one-review list keeps two reviews and
/// unreadable risk lists make every path human, like
/// `scripts/enqueue-reviewed` (which, unlike this engine, still asks Browser
/// QA for `ui/` then). `absent` says the caller proved the repository
/// declares no lists, which the reasons report instead of "unreadable".
/// Verdict notes declaring `Risk: human` still add the operator check
/// downstream, noted in `reasons`.
fn strict_requirements(
    resolved: &Resolved,
    activation: Activation,
    changes: &[Change],
    lines: u64,
    lists: Option<&Lists>,
    absent: bool,
    mut reasons: Vec<String>,
) -> Requirements {
    let qualifies = lists.is_some_and(|l| one_review_qualifies(changes, l));
    if lists.is_none() {
        reasons.push(if absent {
            "the repository declares no trusted lists — two reviews and the operator floor hold"
                .to_string()
        } else {
            "the trusted lists are unreadable — two reviews and the operator floor hold".to_string()
        });
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
        browser_qa: browser_qa_needed(changes, lists),
        operator_approval: operator,
        full_checks: true,
        reasons,
        policy_digest: resolved.digest.clone(),
        activation,
        required_reviews: None,
        required_checks: None,
    }
}

/// The evaluator — see the module header. `resolved` is the effective
/// policy (approved or defaulted, with its digest); `changes`+`lines`
/// are the trusted diff evidence; `lists` the trusted list texts.
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

    // CAD-2413: an owner-approved custom policy is evaluated (opt-in, operator
    // decision 2026-11-17). Anything else not Active keeps the strict floor.
    if activation == Activation::NoProfile && resolved.source == "approved" {
        return classify_approved(resolved, activation, changes, lines, parsed.as_ref());
    }

    if activation != Activation::Active {
        reasons.push(format!(
            "default lean classification does not apply ({activation:?}) — \
             the strict requirements apply"
        ));
        return strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            parsed.as_ref(),
            false,
            reasons,
        );
    }
    reasons.push("default lean delivery policy in force".to_string());

    let Some(lists) = parsed else {
        reasons.push("the trusted lists could not be read or parsed".to_string());
        return strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            parsed.as_ref(),
            false,
            reasons,
        );
    };
    if let Some(why) = evidence_refusal(changes) {
        reasons.push(format!("incomplete diff evidence: {why}"));
        return strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            Some(&lists),
            false,
            reasons,
        );
    }

    if changes.iter().any(|c| !reviewable_change(c)) {
        reasons.push(
            "deletion, rename, type or mode change requires legacy strict review".to_string(),
        );
        return strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            Some(&lists),
            false,
            reasons,
        );
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
                    .any(|g| crate::delivery_rules::glob_match(g, p))
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
        return strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            Some(&lists),
            false,
            reasons,
        );
    }

    // Anything outside the known include list — or a change shape the
    // list grammar refuses (delete, rename, mode/type change) — falls
    // back to strict rather than guessing.
    if !one_review_qualifies(changes, &lists) {
        reasons.push(
            "a path or change shape is outside the known one-review include list".to_string(),
        );
        return strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            Some(&lists),
            false,
            reasons,
        );
    }

    // Routine: every change a regular 100644 `A`/`M` ordinary-doc
    // markdown file. An executable doc, a non-markdown doc or any other
    // extension is consequential, never routine.
    if changes
        .iter()
        .all(|c| reviewable_change(c) && c.new_mode == "100644" && routine_path(&c.path, &lists))
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
            required_reviews: None,
            required_checks: None,
        };
    }

    reasons.push("known ordinary paths — one independent combined review".to_string());
    Requirements {
        class: DeliveryClass::Consequential,
        reviews: 1,
        review_kind: ReviewKind::Combined,
        security_capable: false,
        browser_qa: browser_qa_needed(changes, Some(&lists)),
        operator_approval: false,
        full_checks: true,
        reasons,
        policy_digest: resolved.digest.clone(),
        activation,
        required_reviews: None,
        required_checks: None,
    }
}

/// The reviews an approved policy's applying rules require, in rule order,
/// each name once, with the reasons naming the rules that applied.
fn applying_reviews(
    policy: &crate::delivery::DeliveryPolicy,
    changes: &[Change],
    lines: u64,
    reasons: &mut Vec<String>,
    omit_default_floor: bool,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (i, rule) in policy.risk.iter().enumerate() {
        let why = match &rule.when {
            None => Some("always".to_string()),
            Some(when) => {
                let path = changes
                    .iter()
                    .find(|c| {
                        when.paths
                            .iter()
                            .any(|g| crate::delivery_rules::area_match(g, &c.path))
                    })
                    .map(|c| format!("path {}", c.path));
                let over_lines = when
                    .lines_over
                    .filter(|n| lines > *n)
                    .map(|n| format!("{lines} changed lines over {n}"));
                let over_files = when
                    .files_over
                    .filter(|n| changes.len() as u64 > *n)
                    .map(|n| format!("{} changed files over {n}", changes.len()));
                path.or(over_lines).or(over_files)
            }
        };
        if let Some(why) = why {
            reasons.push(format!("policy rule {i} applies: {why}"));
            for name in &rule.require {
                if omit_default_floor
                    && rule.when.is_none()
                    && name == "review"
                    && matches!(policy.reviews.get(name), Some(crate::delivery::Review::Agent { focus, profile: None }) if focus == "general")
                {
                    continue;
                }
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
        }
    }
    names
}

fn ci_only_protected_path(path: &str) -> bool {
    const DIRECTORIES: &[&str] = &[
        ".github",
        ".cadence",
        ".claude",
        ".agents",
        "agents",
        "skills",
        "workflows",
    ];
    const BASENAMES: &[&str] = &[
        "agents.md",
        "claude.md",
        "skill.md",
        "project.md",
        "codeowners",
        "cadence-review.toml",
        "delivery.yaml",
        "delivery.yml",
    ];
    let basename = path.rsplit('/').next().unwrap_or_default();
    path.get(.."docs/roles/".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("docs/roles/"))
        || path.split('/').any(|part| {
            DIRECTORIES
                .iter()
                .any(|name| part.eq_ignore_ascii_case(name))
        })
        || BASENAMES
            .iter()
            .any(|name| basename.eq_ignore_ascii_case(name))
}

/// Hosted-only classification. The caller vouches for the protected-base,
/// complete diff and selected repository; this function grants no authority.
pub(crate) fn hosted_ci_only(
    resolved: &Resolved,
    changes: &[Change],
    lines: u64,
    lists: &TrustedLists,
    selected_repo: &str,
    both_absent: bool,
) -> Option<Requirements> {
    use crate::delivery::{CheckMode, Review};
    let scope = resolved.policy.ci_only.as_ref()?;
    if resolved.source != "approved"
        || resolved.note.is_some()
        || selected_repo != scope.repo
        || evidence_refusal(changes).is_some()
        || lines > HUMAN_LINES
        || lines > resolved.policy.heavy.lines_over
        || changes.len() as u64 > resolved.policy.heavy.files_over
    {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    if changes.iter().any(|c| {
        !reviewable_change(c)
            || c.new_mode != "100644"
            || !scope
                .paths
                .iter()
                .any(|p| crate::delivery_rules::area_match(p, &c.path))
            || c.path.chars().any(crate::delivery_rules::is_format)
            || ci_only_protected_path(&c.path)
            || !seen.insert(&c.path)
    }) {
        return None;
    }
    let activation = activation_of(resolved);
    if !matches!(activation, Activation::Active | Activation::NoProfile) {
        return None;
    }
    let parsed = if both_absent {
        if lists.risk_paths.is_some() || lists.one_review.is_some() {
            return None;
        }
        None
    } else {
        Some(parse_lists(lists)?)
    };
    if let Some(lists) = parsed.as_ref() {
        if browser_qa_needed(changes, Some(lists))
            || changes.iter().any(|c| {
                human_path(&c.path, Some(lists))
                    || any_glob(lists.sensitive.iter(), &c.path)
                    || lists
                        .one_review_exclude
                        .iter()
                        .any(|g| one_glob(g, &c.path))
            })
        {
            return None;
        }
    }
    let mut reasons = vec!["owner-approved hosted CI-only scope applies".to_string()];
    let names = applying_reviews(&resolved.policy, changes, lines, &mut reasons, true);
    let mut required_reviews = Vec::new();
    let mut required_checks = Vec::new();
    for name in names {
        let Review::Check {
            app,
            mode: CheckMode::Required,
        } = resolved.policy.reviews.get(&name)?
        else {
            return None;
        };
        required_checks.push(RequiredCheck {
            name: name.clone(),
            app: app.clone(),
        });
        required_reviews.push(RequiredReview {
            name,
            kind: "check".into(),
            focus: None,
            profile: None,
            app: Some(app.clone()),
            mode: Some("required".into()),
        });
    }
    Some(Requirements {
        class: DeliveryClass::Consequential,
        reviews: 0,
        review_kind: ReviewKind::None,
        security_capable: false,
        browser_qa: false,
        operator_approval: false,
        full_checks: true,
        reasons,
        policy_digest: resolved.digest.clone(),
        activation,
        required_reviews: Some(required_reviews),
        required_checks: Some(required_checks),
    })
}

/// The review that satisfies both `a` and `b`. A single-axis review never
/// stands in for `Combined` (one verdict covering both axes), so joining a
/// single axis with anything other than itself or `None` needs both axes.
fn review_join(a: ReviewKind, b: ReviewKind) -> ReviewKind {
    use ReviewKind::*;
    match (a, b) {
        (x, None) | (None, x) => x,
        (x, y) if x == y => x,
        _ => StandardsAndSpec,
    }
}

/// The verdict count a review kind needs.
fn review_count(kind: ReviewKind) -> u32 {
    match kind {
        ReviewKind::None => 0,
        ReviewKind::Combined | ReviewKind::Standards | ReviewKind::SpecSecurity => 1,
        ReviewKind::StandardsAndSpec => 2,
    }
}

/// CAD-2413: requirements under an owner-approved custom policy. The policy's
/// applying rules decide the reviews; the trusted lists' floor is applied on
/// top and never lowered (operator decision 2026-11-17).
fn classify_approved(
    resolved: &Resolved,
    activation: Activation,
    changes: &[Change],
    lines: u64,
    lists: Option<&Lists>,
) -> Requirements {
    use crate::delivery::{CheckMode, Review};
    let mut reasons = vec!["an approved delivery policy is in force".to_string()];
    let names = applying_reviews(&resolved.policy, changes, lines, &mut reasons, false);
    let (mut standards, mut spec, mut general) = (false, false, false);
    let (mut operator, mut browser_qa) = (false, false);
    let mut required_reviews = Vec::new();
    let mut required_checks = Vec::new();
    for name in &names {
        let Some(review) = resolved.policy.reviews.get(name) else {
            continue;
        };
        let mut entry = RequiredReview {
            name: name.clone(),
            kind: String::new(),
            focus: None,
            profile: None,
            app: None,
            mode: None,
        };
        match review {
            Review::Agent { focus, profile } => {
                entry.kind = "agent".into();
                entry.focus = Some(focus.clone());
                entry.profile.clone_from(profile);
                // A profile assigned by policy never replaces human approval.
                operator |= profile.is_some();
                match focus.as_str() {
                    "standards" => standards = true,
                    "spec-security" => spec = true,
                    "browser-qa" => browser_qa = true,
                    "general" => general = true,
                    // `parse` admits only `FOCI`; anything else fails closed
                    // to both axes.
                    _ => (standards, spec) = (true, true),
                }
            }
            Review::Check { app, mode } => {
                entry.kind = "check".into();
                entry.app = Some(app.clone());
                let required = matches!(mode, CheckMode::Required);
                entry.mode = Some(if required { "required" } else { "advisory" }.into());
                if required {
                    required_checks.push(RequiredCheck {
                        name: name.clone(),
                        app: app.clone(),
                    });
                }
            }
            Review::Operator => {
                entry.kind = "operator".into();
                operator = true;
            }
        }
        required_reviews.push(entry);
    }
    let (reviews, review_kind) = match (standards, spec, general) {
        (true, true, _) => (2, ReviewKind::StandardsAndSpec),
        (true, false, _) => (1, ReviewKind::Standards),
        (false, true, _) => (1, ReviewKind::SpecSecurity),
        (false, false, true) => (1, ReviewKind::Combined),
        (false, false, false) => (0, ReviewKind::None),
    };
    // The repository's own `[browser_qa]` table applies on top of a policy.
    browser_qa |= browser_qa_needed(changes, lists);

    // The floor (operator decision D1): anything the trusted lists, the size
    // limit or the diff evidence would make strict stays at least strict.
    let floor = match lists {
        None => true,
        Some(l) => {
            evidence_refusal(changes).is_some()
                || changes.iter().any(|c| !reviewable_change(c))
                || lines > HUMAN_LINES
                || changes.iter().any(|c| {
                    let p = c.path.as_str();
                    human_path(p, Some(l))
                        || l.sensitive
                            .iter()
                            .any(|g| crate::delivery_rules::glob_match(g, p))
                        || l.one_review_exclude.iter().any(|g| one_glob(g, p))
                })
        }
    };
    if floor {
        let mut floor_reasons = reasons.clone();
        floor_reasons.push(
            "the trusted-list floor applies: the policy cannot lower these requirements"
                .to_string(),
        );
        let strict = strict_requirements(
            resolved,
            activation,
            changes,
            lines,
            lists,
            false,
            floor_reasons,
        );
        let review_kind = review_join(strict.review_kind, review_kind);
        let reviews = review_count(review_kind).max(strict.reviews).max(reviews);
        return Requirements {
            class: DeliveryClass::Strict,
            reviews,
            review_kind,
            browser_qa: strict.browser_qa || browser_qa,
            operator_approval: strict.operator_approval || operator,
            full_checks: true,
            required_reviews: Some(required_reviews),
            required_checks: Some(required_checks),
            ..strict
        };
    }
    reasons.push("the approved policy decides: no trusted-list floor applies".to_string());
    Requirements {
        class: DeliveryClass::Consequential,
        reviews,
        review_kind,
        security_capable: false,
        browser_qa,
        operator_approval: operator,
        full_checks: true,
        reasons,
        policy_digest: resolved.digest.clone(),
        activation,
        required_reviews: Some(required_reviews),
        required_checks: Some(required_checks),
    }
}

/// CAD-2386: the product default for a repository that declares no trusted
/// lists — the caller proved both files are absent at the anchor (a definite
/// "not found", never a failed read). Under the default lean policy it is one
/// combined review plus the operator's approval, with no path rules and no
/// Browser QA. Anything else keeps the fail-closed strict floor: a custom or
/// unapproved policy (only an approved one's rules are evaluated, and only with lists) and incomplete diff
/// evidence.
pub fn classify_without_lists(resolved: &Resolved, changes: &[Change], lines: u64) -> Requirements {
    let activation = activation_of(resolved);
    let mut reasons = Vec::new();
    if activation != Activation::Active {
        reasons.push(format!(
            "default lean classification does not apply ({activation:?}) — \
             the strict requirements apply"
        ));
        return strict_requirements(resolved, activation, changes, lines, None, true, reasons);
    }
    reasons.push("default lean delivery policy in force".to_string());
    if let Some(why) = evidence_refusal(changes) {
        reasons.push(format!("incomplete diff evidence: {why}"));
        return strict_requirements(resolved, activation, changes, lines, None, true, reasons);
    }
    reasons.push(
        "the repository declares no trusted lists — the product default applies: \
         one combined review and the operator's approval"
            .to_string(),
    );
    Requirements {
        class: DeliveryClass::Consequential,
        reviews: 1,
        review_kind: ReviewKind::Combined,
        security_capable: false,
        browser_qa: false,
        operator_approval: true,
        full_checks: true,
        reasons,
        policy_digest: resolved.digest.clone(),
        activation,
        required_reviews: None,
        required_checks: None,
    }
}

/// `Resolved` for a plain default policy — a convenience for callers
/// (the enqueue inspector) that have no file or approval context and
/// want the documented "no profile" answer.
pub fn default_resolved() -> Resolved {
    Resolved {
        policy: default_policy(),
        source: "default",
        digest: crate::delivery::digest(&default_policy()),
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
