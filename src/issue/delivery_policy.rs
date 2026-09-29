//! CAD-826 (CAD-814 slice 1): the per-project delivery policy —
//! validated, operator-approved data in `<pm>/<key>/PROJECT.md`
//! frontmatter under a `delivery:` key. Nothing in the delivery loop
//! reads it yet (`src/delivery.rs` is unchanged); slice 2 consumes the
//! resolved policy. This module owns the types, the strict parse, the
//! defaults (the same constants the loop uses today), the canonical
//! digest and the pure [`effective`] resolver.
//!
//! Like the work-model gates (CAD-405), the key sits in PROJECT.md and
//! not `project.yaml` (strict, `deny_unknown_fields` — an older binary
//! would refuse the whole project file) and it only takes effect once
//! the operator approves it — through the same
//! `project_work_approve` approval, which records the resolved policy
//! and its digest. Readers fall back to the approved policy, then the
//! defaults, and report `delivery_unapproved`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{model, parse, work};

/// The `focus` values an `agent` review may declare — the review
/// flavours the delivery loop knows how to request.
pub const FOCI: &[&str] = &["general", "standards", "spec-security", "browser-qa"];

/// A project's delivery policy — `PROJECT.md` `delivery:` or the
/// defaults. [`parse`] builds it through the strict raw types and
/// [`DeliveryPolicy::validate`]; `serde` round-trips the validated
/// form (the approval payload's `delivery` key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryPolicy {
    /// How an approved PR merges.
    pub merge: MergePolicy,
    /// REVISE verdicts a ticket takes before the operator decides.
    pub max_revise: u32,
    /// The named reviews a `risk` rule may require.
    pub reviews: BTreeMap<String, Review>,
    /// Risk rules: a delivery must satisfy every review `require`d by
    /// every rule whose `when` holds. A rule with no `when` always
    /// applies.
    pub risk: Vec<RiskRule>,
    /// Over this a delivery is "heavy".
    pub heavy: SizeLimit,
    /// Over this a delivery cannot merge through the loop.
    pub oversized: SizeLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePolicy {
    pub method: MergeMethod,
    pub queue: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

/// One named review a rule may require. `kind` picks the variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Review {
    /// An independent agent review; `focus` is one of [`FOCI`].
    Agent { focus: String },
    /// A GitHub App check (a bot). `mode: advisory` never blocks;
    /// `required` blocks like an agent review.
    Check { app: String, mode: CheckMode },
    /// The operator decides.
    Operator,
}

impl Review {
    /// A review a delivery must wait for — everything except an
    /// advisory `check` (a bot's verdict is advisory input).
    pub fn blocking(&self) -> bool {
        !matches!(
            self,
            Self::Check {
                mode: CheckMode::Advisory,
                ..
            }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckMode {
    Advisory,
    Required,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskRule {
    /// The conditions under which the rule applies — ANY of them.
    /// Absent: the rule always applies.
    pub when: Option<RiskWhen>,
    /// Names in `reviews` the delivery must satisfy.
    pub require: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskWhen {
    /// Repo-relative path globs (`*`, `**`, `?`); slice 3 owns
    /// matching, this slice only validates the syntax.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub lines_over: Option<u64>,
    #[serde(default)]
    pub files_over: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizeLimit {
    pub lines_over: u64,
    pub files_over: u64,
}

/// `PROJECT.md` frontmatter for this reader — lenient: the file
/// carries keys other readers own (`agents`, `stages`, `areas`, …).
#[derive(Deserialize)]
struct Front {
    #[serde(default)]
    delivery: Option<serde_yaml::Value>,
}

/// The raw `delivery:` section — every level `deny_unknown_fields`,
/// then each raw node converts to its validated type with an error
/// that names the offending key. `reviews` and `risk` are required.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDelivery {
    #[serde(default)]
    merge: Option<RawMerge>,
    #[serde(default)]
    max_revise: Option<u32>,
    reviews: BTreeMap<String, RawReview>,
    risk: Vec<RiskRule>,
    #[serde(default)]
    heavy: Option<RawSize>,
    #[serde(default)]
    oversized: Option<RawSize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMerge {
    #[serde(default)]
    method: Option<MergeMethod>,
    #[serde(default)]
    queue: Option<bool>,
}

/// A flat review: `kind` plus every key any variant could take, so
/// the conversion can refuse the keys a kind does not own with an
/// exact per-kind message (an internally tagged enum cannot).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReview {
    kind: String,
    #[serde(default)]
    focus: Option<String>,
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSize {
    #[serde(default)]
    lines_over: Option<u64>,
    #[serde(default)]
    files_over: Option<u64>,
}

fn err(msg: impl std::fmt::Display) -> Error {
    Error::rejected(format!("PROJECT.md delivery: {msg}"))
}

impl RawReview {
    fn into_review(self, name: &str) -> Result<Review> {
        let refused = |key: &str, has: &Option<String>| -> Result<()> {
            if has.is_some() {
                return Err(err(format!(
                    "reviews.{name}: a '{}' review takes no '{key}'",
                    self.kind
                )));
            }
            Ok(())
        };
        match self.kind.as_str() {
            "agent" => {
                refused("app", &self.app)?;
                refused("mode", &self.mode)?;
                let focus = self.focus.ok_or_else(|| {
                    err(format!("reviews.{name}: an 'agent' review needs a 'focus'"))
                })?;
                if !FOCI.contains(&focus.as_str()) {
                    return Err(err(format!(
                        "reviews.{name}.focus '{focus}': not a known focus ({})",
                        FOCI.join(", ")
                    )));
                }
                Ok(Review::Agent { focus })
            }
            "check" => {
                refused("focus", &self.focus)?;
                let app = self.app.ok_or_else(|| {
                    err(format!("reviews.{name}: a 'check' review needs an 'app'"))
                })?;
                if !valid_app(&app) {
                    return Err(err(format!(
                        "reviews.{name}.app '{app}': not a GitHub App slug \
                         (^[a-z0-9][a-z0-9-]{{0,99}}$) or an App id (all digits)"
                    )));
                }
                let mode = match self.mode.as_deref() {
                    None => {
                        return Err(err(format!(
                            "reviews.{name}: a 'check' review needs a 'mode' \
                             (advisory|required)"
                        )))
                    }
                    Some("advisory") => CheckMode::Advisory,
                    Some("required") => CheckMode::Required,
                    Some(m) => {
                        return Err(err(format!("reviews.{name}.mode '{m}': advisory|required")))
                    }
                };
                Ok(Review::Check { app, mode })
            }
            "operator" => {
                refused("focus", &self.focus)?;
                refused("app", &self.app)?;
                refused("mode", &self.mode)?;
                Ok(Review::Operator)
            }
            other => Err(err(format!(
                "reviews.{name}.kind '{other}': unknown kind (agent|check|operator)"
            ))),
        }
    }
}

impl RawDelivery {
    fn into_policy(self) -> Result<DeliveryPolicy> {
        let merge = self.merge.unwrap_or(RawMerge {
            method: None,
            queue: None,
        });
        let size = |raw: Option<RawSize>, lines: u64, files: u64| {
            let raw = raw.unwrap_or(RawSize {
                lines_over: None,
                files_over: None,
            });
            SizeLimit {
                lines_over: raw.lines_over.unwrap_or(lines),
                files_over: raw.files_over.unwrap_or(files),
            }
        };
        let heavy = size(
            self.heavy,
            crate::delivery::HEAVY_LINES,
            crate::delivery::HEAVY_FILES,
        );
        let oversized = size(
            self.oversized,
            crate::delivery::OVERSIZED_LINES,
            crate::delivery::OVERSIZED_FILES,
        );
        let mut reviews = BTreeMap::new();
        for (name, raw) in self.reviews {
            reviews.insert(name.clone(), raw.into_review(&name)?);
        }
        Ok(DeliveryPolicy {
            merge: MergePolicy {
                method: merge.method.unwrap_or(MergeMethod::Squash),
                queue: merge.queue.unwrap_or(true),
            },
            max_revise: self.max_revise.unwrap_or(crate::delivery::MAX_REVISE),
            reviews,
            risk: self.risk,
            heavy,
            oversized,
        })
    }
}

/// The policy today's loop implements — used when there is no
/// `delivery:` section. Built from the `delivery.rs` constants, never
/// literals.
pub fn default_policy() -> DeliveryPolicy {
    DeliveryPolicy {
        merge: MergePolicy {
            method: MergeMethod::Squash,
            queue: true,
        },
        max_revise: crate::delivery::MAX_REVISE,
        reviews: BTreeMap::from([(
            "review".to_string(),
            Review::Agent {
                focus: "general".to_string(),
            },
        )]),
        risk: vec![RiskRule {
            when: None,
            require: vec!["review".to_string()],
        }],
        heavy: SizeLimit {
            lines_over: crate::delivery::HEAVY_LINES,
            files_over: crate::delivery::HEAVY_FILES,
        },
        oversized: SizeLimit {
            lines_over: crate::delivery::OVERSIZED_LINES,
            files_over: crate::delivery::OVERSIZED_FILES,
        },
    }
}

/// The `delivery:` section of a PROJECT.md text. `Ok(None)` when there
/// is no frontmatter or no `delivery` key; a malformed section is an
/// error that names the offending key or value.
pub fn parse(text: &str) -> Result<Option<DeliveryPolicy>> {
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return Ok(None);
    }
    let (yaml, _) = parse::split_front(trimmed)?;
    let front: Front = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("PROJECT.md frontmatter: {e}")))?;
    let Some(value) = front.delivery else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let raw: RawDelivery = serde_yaml::from_value(value).map_err(err)?;
    let policy = raw.into_policy()?;
    policy.validate()?;
    Ok(Some(policy))
}

/// Strict load: no file/section is `Ok(None)`; a symlinked, unreadable
/// or malformed one refuses — an approval never guesses.
pub fn load(pm_dir: &Path, key: &str) -> Result<Option<DeliveryPolicy>> {
    match work::read_project_md(pm_dir, key)? {
        None => Ok(None),
        Some(text) => parse(&text).map_err(|e| Error::rejected(format!("{key}: {e}"))),
    }
}

/// `sha256:<hex>` of the policy's canonical JSON — struct fields in
/// declaration order, `reviews` as a BTreeMap — so equivalent YAML
/// spellings (reordered keys, defaults written out or left implicit)
/// share one digest.
pub fn digest(p: &DeliveryPolicy) -> String {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::to_vec(p).unwrap_or_default();
    let hash = Sha256::digest(&canonical);
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// The policy a project runs under, and why. `source` is `default`,
/// `file` or `approved`; `digest` is always the digest of `policy`;
/// `note` is `Some("delivery_unapproved: …")`/`"delivery_error: …"`
/// when the file's section is not the one in force.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub policy: DeliveryPolicy,
    pub source: &'static str,
    pub digest: String,
    pub note: Option<String>,
}

/// The delivery half of a `project_work_approve` payload: the recorded
/// digest plus the policy it approved (`None` = approved "no
/// section").
#[derive(Clone, Debug, PartialEq)]
pub struct Approved {
    pub digest: String,
    pub policy: Option<DeliveryPolicy>,
}

/// The delivery half of a `project_work_approve` payload, or `None`
/// when the payload is an old-format approval (no `delivery_digest`
/// key) or its `delivery` no longer deserializes and validates.
pub fn approved_from(payload: &Value) -> Option<Approved> {
    let digest = payload["delivery_digest"].as_str()?.to_string();
    let policy = match payload.get("delivery") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let p: DeliveryPolicy = serde_json::from_value(v.clone()).ok()?;
            p.validate().ok()?;
            Some(p)
        }
    };
    Some(Approved { digest, policy })
}

/// Project key → its recorded delivery approval.
pub type DeliveryApprovals = HashMap<String, Approved>;

/// Every project's latest `project_work_approve` delivery half, from
/// the daemon. An unreachable daemon is an empty map — unapproved
/// reads as the defaults, the fail-safe direction.
pub fn fetch_approvals(state_dir: &Path) -> DeliveryApprovals {
    crate::client::rpc(state_dir, "project_work_approvals", json!({}))
        .ok()
        .and_then(|v| v["approvals"].as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| approved_from(&v).map(|a| (k, a)))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve a project's policy from what was read from its PROJECT.md
/// (`Err` carries the parse error) and its latest
/// `project_work_approve` delivery half, if any. Pure.
pub fn effective(
    key: &str,
    file: Result<Option<DeliveryPolicy>>,
    approved: Option<&Approved>,
) -> Resolved {
    let resolved = |policy: DeliveryPolicy, source: &'static str, note: Option<String>| {
        let digest = digest(&policy);
        Resolved {
            policy,
            source,
            digest,
            note,
        }
    };
    let approve_cmd = format!("`cadence issue project approve-work {key}`");
    match file {
        Ok(Some(p)) => {
            let dg = digest(&p);
            // The approval is checked first: while a custom policy is
            // approved, ANY different section — including a
            // default-equivalent one — keeps the approved policy in
            // force, otherwise an agent could swap it for the weaker
            // defaults with no note.
            if let Some(a) = approved {
                if a.digest == dg && a.policy.is_some() {
                    return resolved(p, "approved", None);
                }
                if let Some(q) = &a.policy {
                    return resolved(
                        q.clone(),
                        "approved",
                        Some(format!(
                            "delivery_unapproved: {key}/PROJECT.md delivery changed since \
                             approval ({dg}) — the approved policy ({}) stays in force \
                             until {approve_cmd}",
                            a.digest
                        )),
                    );
                }
            }
            if dg == digest(&default_policy()) {
                return resolved(p, "file", None);
            }
            resolved(
                default_policy(),
                "default",
                Some(format!(
                    "delivery_unapproved: {key}/PROJECT.md delivery differs from the \
                     default and is not operator-approved ({dg}) — the default applies \
                     until {approve_cmd}"
                )),
            )
        }
        Ok(None) => match approved.and_then(|a| a.policy.clone()) {
            Some(q) => resolved(
                q,
                "approved",
                Some(format!(
                    "delivery_unapproved: {key}/PROJECT.md delivery was removed since \
                     approval — the approved policy stays in force until {approve_cmd}"
                )),
            ),
            None => resolved(default_policy(), "default", None),
        },
        Err(e) => match approved.and_then(|a| a.policy.clone()) {
            Some(q) => resolved(
                q,
                "approved",
                Some(format!(
                    "delivery_unapproved: {key}/PROJECT.md delivery is malformed ({e}) \
                     — the approved policy stays in force"
                )),
            ),
            None => resolved(
                default_policy(),
                "default",
                Some(format!(
                    "delivery_error: {key}/PROJECT.md delivery is malformed ({e}) \
                     — the default applies"
                )),
            ),
        },
    }
}

/// A `check` `app`: a GitHub App slug (`^[a-z0-9][a-z0-9-]{0,99}$`) or
/// an App id (all digits).
fn valid_app(app: &str) -> bool {
    if !app.is_empty() && app.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    !app.is_empty()
        && app.len() <= 100
        && app.as_bytes()[0] != b'-'
        && app
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A `when.paths` entry: a repo-relative glob — non-empty, no leading
/// `/`, no `..` segment, no `\`, no control characters and none of
/// `[` `]` `{` `}`; `*`, `**` and `?` are the only wildcards (slice 3
/// owns their semantics).
fn check_glob(path: &str) -> Result<()> {
    // Callers add the `PROJECT.md delivery:` prefix (and the rule index).
    let bad = |msg: &str| Error::rejected(format!("when.paths entry '{path}': {msg}"));
    if path.is_empty() {
        return Err(bad("empty"));
    }
    if path.starts_with('/') {
        return Err(bad("absolute — paths are repo-relative"));
    }
    if path.contains('\\') {
        return Err(bad("a '\\' — repo-relative paths use '/'"));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(bad("a control character"));
    }
    if path.contains('[') || path.contains(']') || path.contains('{') || path.contains('}') {
        return Err(bad("only '*', '**' and '?' wildcards are allowed"));
    }
    if path.split('/').any(|seg| seg == "..") {
        return Err(bad("a '..' segment"));
    }
    Ok(())
}

impl DeliveryPolicy {
    /// Every rule fails closed: a malformed section is refused whole,
    /// never partially applied.
    pub fn validate(&self) -> Result<()> {
        if self.reviews.is_empty() {
            return Err(err("reviews must name at least one review"));
        }
        for name in self.reviews.keys() {
            if !model::valid_tag(name) {
                return Err(err(format!("reviews.{name}: not a valid name")));
            }
        }
        if self.risk.is_empty() {
            return Err(err("risk must hold at least one rule"));
        }
        let mut floor = false;
        for (i, rule) in self.risk.iter().enumerate() {
            if rule.require.is_empty() {
                return Err(err(format!(
                    "risk[{i}].require must name at least one review"
                )));
            }
            let mut seen = std::collections::HashSet::new();
            for req in &rule.require {
                if !seen.insert(req) {
                    return Err(err(format!("risk[{i}].require names '{req}' twice")));
                }
                match self.reviews.get(req) {
                    None => {
                        return Err(err(format!(
                            "risk[{i}].require names undefined review '{req}'"
                        )))
                    }
                    Some(r) if !r.blocking() => {
                        return Err(err(format!(
                            "risk[{i}].require names '{req}', an advisory check — \
                             advisory reviews never block: use mode: required or drop \
                             it from require"
                        )))
                    }
                    _ => {}
                }
            }
            match &rule.when {
                None => {
                    if rule.require.iter().any(|r| self.reviews[r].blocking()) {
                        floor = true;
                    }
                }
                Some(when) => {
                    if when.paths.is_empty()
                        && when.lines_over.is_none()
                        && when.files_over.is_none()
                    {
                        return Err(err(format!("risk[{i}].when has no condition")));
                    }
                    for (key, v) in [
                        ("lines_over", when.lines_over),
                        ("files_over", when.files_over),
                    ]
                    .into_iter()
                    .filter_map(|(k, v)| v.map(|v| (k, v)))
                    {
                        if v < 1 {
                            return Err(err(format!("risk[{i}].when.{key} must be >= 1")));
                        }
                    }
                    for path in &when.paths {
                        check_glob(path).map_err(|e| err(format!("risk[{i}].{e}")))?;
                    }
                }
            }
        }
        if !floor {
            return Err(err(
                "risk must hold one unconditional rule (no `when`) whose require \
                 includes a blocking review (agent, operator or a required check)",
            ));
        }
        if !(1..=10).contains(&self.max_revise) {
            return Err(err(format!(
                "max_revise {} is outside 1..=10",
                self.max_revise
            )));
        }
        for (name, size) in [("heavy", &self.heavy), ("oversized", &self.oversized)] {
            if size.lines_over < 1 || size.files_over < 1 {
                return Err(err(format!("{name} sizes must be >= 1")));
            }
        }
        if self.heavy.lines_over >= self.oversized.lines_over
            || self.heavy.files_over >= self.oversized.files_over
        {
            return Err(err("heavy must be under oversized on both fields"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_delivery(yaml: &str) -> Result<Option<DeliveryPolicy>> {
        parse(&format!("---\n{yaml}\n---\n# P\n"))
    }

    fn err_of(yaml: &str) -> String {
        parse_delivery(yaml).unwrap_err().to_string()
    }

    /// A custom policy (≠ the defaults) that validates.
    fn custom() -> DeliveryPolicy {
        parse_delivery(
            "delivery:\n  reviews:\n    review: {kind: agent, focus: general}\n    \
             standards: {kind: agent, focus: standards}\n    \
             devin: {kind: check, app: devin, mode: advisory}\n    \
             gate: {kind: operator}\n  risk:\n    - require: [review]\n    \
             - when: {paths: [\"ui/**\"]}\n      require: [review, gate]\n  max_revise: 3\n",
        )
        .unwrap()
        .unwrap()
    }

    fn approval_for(p: &DeliveryPolicy) -> Approved {
        Approved {
            digest: digest(p),
            policy: Some(p.clone()),
        }
    }

    #[test]
    fn default_policy_is_the_loop_constants() {
        let d = default_policy();
        assert_eq!(d.max_revise, crate::delivery::MAX_REVISE);
        assert_eq!(d.heavy.lines_over, crate::delivery::HEAVY_LINES);
        assert_eq!(d.heavy.files_over, crate::delivery::HEAVY_FILES);
        assert_eq!(d.oversized.lines_over, crate::delivery::OVERSIZED_LINES);
        assert_eq!(d.oversized.files_over, crate::delivery::OVERSIZED_FILES);
        assert_eq!(
            d.merge,
            MergePolicy {
                method: MergeMethod::Squash,
                queue: true
            }
        );
        assert_eq!(
            d.reviews,
            BTreeMap::from([(
                "review".to_string(),
                Review::Agent {
                    focus: "general".to_string()
                }
            )])
        );
        assert_eq!(
            d.risk,
            vec![RiskRule {
                when: None,
                require: vec!["review".to_string()]
            }]
        );
        d.validate().unwrap();
    }

    #[test]
    fn parse_none_without_frontmatter_or_key() {
        assert_eq!(parse("# plain markdown\n").unwrap(), None);
        assert_eq!(
            parse("---\nproject: x\nagents: {dev: 1}\n---\n").unwrap(),
            None
        );
        assert_eq!(parse_delivery("delivery: null\n").unwrap(), None);
    }

    #[test]
    fn the_full_example_parses_and_reparses() {
        let p = parse_delivery(
            "project: x\ndelivery:\n  merge: {method: merge, queue: false}\n  max_revise: 4\n  \
             reviews:\n    standards: {kind: agent, focus: standards}\n    \
             spec: {kind: agent, focus: spec-security}\n    browser: {kind: agent, focus: browser-qa}\n    \
             devin: {kind: check, app: devin-ai-integration, mode: advisory}\n    \
             operator: {kind: operator}\n  \
             risk:\n    - require: [standards, spec]\n    \
             - when: {paths: [\"ui/**\"]}\n      require: [browser]\n    \
             - when: {paths: [\".github/**\"], lines_over: 1500, files_over: 40}\n      \
             require: [operator]\n  \
             heavy: {lines_over: 100, files_over: 3}\n  \
             oversized: {lines_over: 900, files_over: 20}\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.merge.method, MergeMethod::Merge);
        assert!(!p.merge.queue);
        assert_eq!(p.max_revise, 4);
        assert_eq!(p.reviews.len(), 5);
        assert_eq!(
            p.reviews["devin"],
            Review::Check {
                app: "devin-ai-integration".into(),
                mode: CheckMode::Advisory
            }
        );
        assert!(!p.reviews["devin"].blocking());
        assert!(p.reviews["operator"].blocking());
        assert_eq!(p.risk.len(), 3);
        assert_eq!(p.risk[1].when.as_ref().unwrap().paths, vec!["ui/**"]);
        assert_eq!(p.risk[2].when.as_ref().unwrap().lines_over, Some(1500));
        assert_eq!(p.heavy.lines_over, 100);
        assert_eq!(p.oversized.files_over, 20);
        // The digest is stable across parse → serialize → parse.
        let reparsed: DeliveryPolicy =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        reparsed.validate().unwrap();
        assert_eq!(digest(&p), digest(&reparsed));
        assert!(digest(&p).starts_with("sha256:"));
    }

    #[test]
    fn unknown_keys_and_variants_are_refused_naming_the_key() {
        for (yaml, key) in [
            ("delivery: {bogus: 1}", "bogus"),
            ("delivery: {merge: {bogus: 1}}", "bogus"),
            ("delivery: {merge: {method: fold}}", "fold"),
            ("delivery: {reviews: {r: {kind: agent, focus: general, bogus: 1}}, risk: [{require: [r]}]}", "bogus"),
            ("delivery: {reviews: {r: {kind: check, app: a, mode: advisory, bogus: 1}}, risk: [{require: [r]}]}", "bogus"),
            ("delivery: {reviews: {r: {kind: operator, bogus: 1}}, risk: [{require: [r]}]}", "bogus"),
            ("delivery: {reviews: {r: {kind: robot}}, risk: [{require: [r]}]}", "robot"),
            ("delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{bogus: 1, require: [r]}]}", "bogus"),
            ("delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{when: {bogus: 1}, require: [r]}]}", "bogus"),
            ("delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}], heavy: {bogus: 3}}", "bogus"),
            ("delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}], oversized: {bogus: 3}}", "bogus"),
            // unknown enum variants
            ("delivery: {reviews: {r: {kind: check, app: a, mode: sometimes}}, risk: [{require: [r]}]}", "sometimes"),
        ] {
            let e = err_of(yaml);
            assert!(e.contains(key), "{yaml} -> {e}");
            assert!(e.starts_with("PROJECT.md delivery:"), "{yaml} -> {e}");
        }
        // …while keys other readers own still pass.
        let ok = parse_delivery(
            "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}]}\n\
             stages: [a, b]\nareas: {}\n",
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// Validation rules 1–11, each a failing case that names WHY the
    /// rule refused — a case failing for another reason would hide a
    /// missing guard.
    #[test]
    fn validation_rules_each_refuse() {
        for (yaml, why) in [
            // 1: reviews non-empty; names are tags.
            ("delivery: {reviews: {}, risk: [{require: [r]}]}", "at least one review"),
            (
                "delivery: {reviews: {Bad: {kind: agent, focus: general}}, risk: [{require: [Bad]}]}",
                "not a valid name",
            ),
            // 2: agent needs focus in FOCI; app/mode refused.
            (
                "delivery: {reviews: {r: {kind: agent}}, risk: [{require: [r]}]}",
                "needs a 'focus'",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: madeup}}, risk: [{require: [r]}]}",
                "not a known focus",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general, app: x}}, risk: [{require: [r]}]}",
                "takes no 'app'",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general, mode: advisory}}, risk: [{require: [r]}]}",
                "takes no 'mode'",
            ),
            // 3: check needs a slug-or-id app and a mode; focus refused.
            (
                "delivery: {reviews: {r: {kind: check, mode: advisory}}, risk: [{require: [r]}]}",
                "needs an 'app'",
            ),
            (
                "delivery: {reviews: {r: {kind: check, app: '-x', mode: advisory}}, risk: [{require: [r]}]}",
                "not a GitHub App slug",
            ),
            (
                "delivery: {reviews: {r: {kind: check, app: 'X', mode: advisory}}, risk: [{require: [r]}]}",
                "not a GitHub App slug",
            ),
            (
                "delivery: {reviews: {r: {kind: check, app: a}}, risk: [{require: [r]}]}",
                "needs a 'mode'",
            ),
            (
                "delivery: {reviews: {r: {kind: check, app: a, mode: advisory, focus: general}}, risk: [{require: [r]}]}",
                "takes no 'focus'",
            ),
            // 4: operator refuses focus/app/mode.
            (
                "delivery: {reviews: {r: {kind: operator, focus: general}}, risk: [{require: [r]}]}",
                "takes no 'focus'",
            ),
            (
                "delivery: {reviews: {r: {kind: operator, mode: advisory}}, risk: [{require: [r]}]}",
                "takes no 'mode'",
            ),
            // 6: risk non-empty; require non-empty, no dups, defined.
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: []}",
                "at least one rule",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: []}]}",
                "require must name at least one review",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r, r]}]}",
                "twice",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [nope]}]}",
                "undefined review 'nope'",
            ),
            // 7: require may not name an advisory check.
            (
                "delivery: {reviews: {r: {kind: check, app: ok, mode: advisory}, \
                           g: {kind: operator}}, risk: [{require: [r, g]}]}",
                "advisory check",
            ),
            // 8: when needs a condition; sizes >= 1; path syntax.
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, \
                           risk: [{require: [r]}, {when: {}, require: [r]}]}",
                "has no condition",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, \
                           risk: [{require: [r]}, {when: {lines_over: 0}, require: [r]}]}",
                "when.lines_over must be >= 1",
            ),
            (
                "delivery: {reviews: {r: {kind: operator}}, \
                           risk: [{require: [r]}, {when: {paths: ['']}, require: [r]}]}",
                "'': empty",
            ),
            (
                "delivery: {reviews: {r: {kind: operator}}, \
                           risk: [{require: [r]}, {when: {paths: ['/abs']}, require: [r]}]}",
                "absolute",
            ),
            (
                "delivery: {reviews: {r: {kind: operator}}, \
                           risk: [{require: [r]}, {when: {paths: ['a/../b']}, require: [r]}]}",
                "'..' segment",
            ),
            (
                "delivery: {reviews: {r: {kind: operator}}, \
                           risk: [{require: [r]}, {when: {paths: ['a\\\\b']}, require: [r]}]}",
                "a '\\'",
            ),
            (
                "delivery: {reviews: {r: {kind: operator}}, \
                           risk: [{require: [r]}, {when: {paths: ['a[bc]']}, require: [r]}]}",
                "wildcards",
            ),
            (
                "delivery: {reviews: {r: {kind: operator}}, \
                           risk: [{require: [r]}, {when: {paths: ['a{b}']}, require: [r]}]}",
                "wildcards",
            ),
            // 9: the safety floor — no unconditional blocking require.
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, \
                           risk: [{when: {paths: [a]}, require: [r]}]}",
                "unconditional rule",
            ),
            (
                "delivery: {reviews: {r: {kind: operator}, d: {kind: check, app: a, mode: required}}, \
                           risk: [{when: {paths: [x]}, require: [d]}]}",
                "unconditional rule",
            ),
            // 10: max_revise in 1..=10.
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}], max_revise: 0}",
                "outside 1..=10",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}], max_revise: 11}",
                "outside 1..=10",
            ),
            // 11: sizes >= 1, heavy strictly under oversized.
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}], \
                           heavy: {lines_over: 0}}",
                "heavy sizes must be >= 1",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}], \
                           oversized: {files_over: 0}}",
                "oversized sizes must be >= 1",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}], \
                           oversized: {lines_over: 100}}",
                "heavy must be under oversized",
            ),
            (
                "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}], \
                           heavy: {files_over: 50}}",
                "heavy must be under oversized",
            ),
        ] {
            let e = err_of(yaml);
            assert!(e.contains(why), "{yaml} -> {e}");
            assert!(e.starts_with("PROJECT.md delivery:"), "{yaml} -> {e}");
        }
        // …and the floor accepts an agent, an operator or a required
        // check; `?`, `*` and `**` are all legal wildcards.
        for yaml in [
            "delivery: {reviews: {g: {kind: operator}}, risk: [{require: [g]}]}",
            "delivery: {reviews: {c: {kind: check, app: '12345', mode: required}}, risk: [{require: [c]}]}",
            "delivery: {reviews: {r: {kind: agent, focus: general}}, \
                       risk: [{require: [r]}, {when: {paths: ['a?b', '**', 'x/**/y']}, require: [r]}]}",
        ] {
            assert!(parse_delivery(yaml).unwrap().is_some(), "{yaml}");
        }
    }

    #[test]
    fn digest_is_canonical() {
        let a = parse_delivery(
            "delivery: {reviews: {r: {kind: agent, focus: general}}, \
             risk: [{require: [r]}], max_revise: 5}\n",
        )
        .unwrap()
        .unwrap();
        // Keys reordered, every default written out — one digest.
        let b = parse_delivery(
            "delivery:\n  merge: {method: squash, queue: true}\n  max_revise: 5\n  \
             reviews: {r: {kind: agent, focus: general}}\n  \
             risk: [{require: [r]}]\n  \
             heavy: {lines_over: 800, files_over: 10}\n  \
             oversized: {lines_over: 3000, files_over: 40}\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(digest(&a), digest(&b));
        let c = parse_delivery(
            "delivery: {reviews: {r: {kind: agent, focus: general}}, \
             risk: [{require: [r]}], max_revise: 6}\n",
        )
        .unwrap()
        .unwrap();
        assert_ne!(digest(&a), digest(&c));
        assert_ne!(digest(&default_policy()), digest(&a));
    }

    // The `effective` resolution table, one test per row.
    #[test]
    fn effective_no_section_no_approval_is_default() {
        let e = effective("demo", Ok(None), None);
        assert_eq!((e.source, &e.note), ("default", &None));
        assert_eq!(e.policy, default_policy());
        assert_eq!(e.digest, digest(&default_policy()));
    }

    #[test]
    fn effective_section_equal_to_default_is_file() {
        // No approval, and an approved "no section" (policy None):
        // a default-equivalent section is the file's own, in force.
        for approval in [
            None,
            Some(Approved {
                digest: digest(&default_policy()),
                policy: None,
            }),
        ] {
            let e = effective("demo", Ok(Some(default_policy())), approval.as_ref());
            assert_eq!((e.source, &e.note), ("file", &None));
            assert_eq!(e.policy, default_policy());
        }
    }

    /// Regression: while a custom policy is approved, rewriting
    /// `delivery:` to a default-equivalent section must NOT silently
    /// drop the stronger approved policy — it resolves as `approved`
    /// with the "changed since approval" note, exactly like a removal.
    #[test]
    fn effective_default_equivalent_edit_keeps_approved_custom() {
        let q = custom();
        let a = approval_for(&q);
        let e = effective("demo", Ok(Some(default_policy())), Some(&a));
        assert_eq!((e.source, e.policy), ("approved", q.clone()));
        assert_eq!(e.digest, digest(&q));
        let n = e.note.unwrap();
        assert!(
            n.starts_with("delivery_unapproved: demo/PROJECT.md delivery changed since approval"),
            "{n}"
        );
        assert!(n.contains(&digest(&default_policy())), "{n}");
    }

    #[test]
    fn effective_matching_digest_is_approved() {
        let p = custom();
        let a = approval_for(&p);
        let e = effective("demo", Ok(Some(p.clone())), Some(&a));
        assert_eq!((e.source, &e.note), ("approved", &None));
        assert_eq!(e.policy, p);
        assert_eq!(e.digest, digest(&p));
    }

    #[test]
    fn effective_custom_unapproved_is_default_plus_note() {
        let p = custom();
        for approval in [
            None,
            Some(Approved {
                digest: "sha256:x".into(),
                policy: None,
            }),
        ] {
            let e = effective("demo", Ok(Some(p.clone())), approval.as_ref());
            assert_eq!((e.source, e.policy), ("default", default_policy()));
            let n = e.note.unwrap();
            assert!(
                n.starts_with(
                    "delivery_unapproved: demo/PROJECT.md delivery differs from \
                 the default and is not operator-approved"
                ),
                "{n}"
            );
            assert!(
                n.contains(&digest(&p)) && n.contains("approve-work demo"),
                "{n}"
            );
        }
        // An old-format approval (approved_from → None at the call
        // site) resolves the same.
        let e = effective("demo", Ok(Some(p)), None);
        assert_eq!(e.source, "default");
    }

    #[test]
    fn effective_changed_since_approval_keeps_approved() {
        let q = custom();
        let a = approval_for(&q);
        let mut p = q.clone();
        p.max_revise = 9;
        let e = effective("demo", Ok(Some(p.clone())), Some(&a));
        assert_eq!((e.source, e.policy), ("approved", q.clone()));
        assert_eq!(e.digest, digest(&q));
        let n = e.note.unwrap();
        assert!(
            n.starts_with("delivery_unapproved: demo/PROJECT.md delivery changed since approval"),
            "{n}"
        );
        assert!(n.contains(&digest(&p)) && n.contains(&digest(&q)), "{n}");
    }

    #[test]
    fn effective_removed_since_approval_keeps_approved() {
        let q = custom();
        let a = approval_for(&q);
        let e = effective("demo", Ok(None), Some(&a));
        assert_eq!((e.source, e.policy), ("approved", q));
        assert!(e.note.unwrap().starts_with(
            "delivery_unapproved: demo/PROJECT.md delivery was removed since approval"
        ));
    }

    #[test]
    fn effective_no_section_approved_no_section_is_default() {
        let a = Approved {
            digest: digest(&default_policy()),
            policy: None,
        };
        let e = effective("demo", Ok(None), Some(&a));
        assert_eq!((e.source, &e.note), ("default", &None));
    }

    #[test]
    fn effective_malformed_with_approval_keeps_approved() {
        let q = custom();
        let a = approval_for(&q);
        let bad = Error::rejected("delivery: risk is empty");
        let e = effective("demo", Err(bad), Some(&a));
        assert_eq!((e.source, e.policy), ("approved", q));
        let n = e.note.unwrap();
        assert!(
            n.starts_with("delivery_unapproved: demo/PROJECT.md delivery is malformed"),
            "{n}"
        );
        assert!(n.contains("risk is empty"), "{n}");
    }

    #[test]
    fn effective_malformed_without_approval_is_default_error() {
        for approval in [
            None,
            Some(Approved {
                digest: "sha256:x".into(),
                policy: None,
            }),
        ] {
            let bad = Error::rejected("delivery: risk is empty");
            let e = effective("demo", Err(bad), approval.as_ref());
            assert_eq!((e.source, e.policy), ("default", default_policy()));
            let n = e.note.unwrap();
            assert!(
                n.starts_with("delivery_error: demo/PROJECT.md delivery is malformed"),
                "{n}"
            );
        }
    }

    #[test]
    fn approved_from_payload() {
        let p = custom();
        let payload = json!({
            "project": "demo",
            "digest": "sha256:work",
            "delivery": serde_json::to_value(&p).unwrap(),
            "delivery_digest": digest(&p),
        });
        let a = approved_from(&payload).unwrap();
        assert_eq!(a.digest, digest(&p));
        assert_eq!(a.policy, Some(p.clone()));

        // Approved "no section".
        let a = approved_from(&json!({
            "delivery": null,
            "delivery_digest": digest(&default_policy()),
        }))
        .unwrap();
        assert_eq!(a.policy, None);

        // Old-format approval: no `delivery_digest` key.
        assert_eq!(
            approved_from(&json!({"project": "demo", "digest": "sha256:w"})),
            None
        );

        // A `delivery` that no longer validates is no approval.
        let mut stale = json!({
            "delivery": serde_json::to_value(&p).unwrap(),
            "delivery_digest": digest(&p),
        });
        stale["delivery"]["risk"] = json!([]);
        assert_eq!(approved_from(&stale), None);
        let mut bad = json!({"delivery": {"bogus": 1}, "delivery_digest": "sha256:x"});
        assert_eq!(approved_from(&bad), None);
        bad["delivery"] = json!("text");
        assert_eq!(approved_from(&bad), None);
    }

    #[test]
    fn other_project_md_readers_ignore_delivery() {
        for delivery in [
            "delivery: {reviews: {r: {kind: agent, focus: general}}, risk: [{require: [r]}]}\n",
            "delivery: {bogus: malformed}\n",
        ] {
            let text = format!(
                "---\nstages: [shape, ship]\n{delivery}\
                 areas:\n  ui:\n    paths: [\"ui/**\"]\n    owner: pm\n---\n# P\n"
            );
            let cfg = crate::issue::work::parse_config(&text).unwrap();
            assert_eq!(cfg.stage_ids(), vec!["shape", "ship"]);
            assert!(crate::issue::areas::parse_config(&text).is_ok());
        }
    }
}
