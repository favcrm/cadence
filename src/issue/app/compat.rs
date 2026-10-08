//! CAD-1154 package ↔ host compatibility declaration and check.
//!
//! An `app.md` may declare `needs.requires` — what the package needs
//! from the host that would install or update it:
//!
//! ```yaml
//! needs:
//!   requires:
//!     core: ">=0.2.0 <2.0.0"                    # semver range on the host core
//!     contracts: {app-chat: [1], app-views: [1]} # exact host-contract majors
//! ```
//!
//! Three versions stay separate (design-plans
//! architecture-independent-apps-1.md, REQ-002): the package's own
//! `version:` string, the schema version a storage layer stamps later
//! (not this module — no storage contract exists yet), and the host
//! requirements this file parses and checks.
//!
//! **Backward compatibility**: a manifest without `requires:` is a
//! legacy package. Parsing succeeds with `None`, admission check
//! passes unconditionally, and contributes no `requires=` line to the
//! structural digest, preserving legacy approvals byte-for-byte. A
//! manifest that DOES carry `requires:` must be complete and well-formed —
//! a malformed supplied requirement
//! fails closed (parse refuses), never degrades to legacy.
//!
//! **No invented contracts**: the only host contracts a package may
//! name are the ones this build actually ships for, listed in
//! [`SUPPORTED_CONTRACTS`] — and an entry is a *parsed declaration
//! validator*, not necessarily a live capability: `app-actions` v1 is
//! still metadata-only until a dispatch path lands, which the receipt
//! and refusal wording state explicitly. There is deliberately no
//! storage or effect contract in the registry — a package naming
//! `app-schema`, `app-storage`, `app-effects` or anything else refuses
//! as an unknown contract.
//!
//! Pure and explicit: the check answers a receipt naming each checked
//! requirement; the caller supplies the host's own version and
//! supported contract majors — nothing here reaches for globals, reads
//! a file, or guesses a capability.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// `requires.core` upper bound on the version string itself — a range
/// is a handful of comparators, not a document.
const CORE_RANGE_CAP: usize = 120;

/// `requires.contracts` — a package may name at most this many
/// distinct host contracts. The registry itself is the real bound.
const MAX_REQUIRED_CONTRACTS: usize = 16;

/// Majors a name may require — the contract list is a closed set of
/// small integers, never a range.
const MAX_CONTRACT_MAJORS: usize = 8;

/// A dotted-tag contract name (`app-chat`, `app-views`) — bounded like
/// a manifest slot name.
const CONTRACT_NAME_CAP: usize = 32;

/// One prerelease identifier — semver §9 grammar (`[0-9A-Za-z-]+`),
/// numeric identifiers compared numerically and always below
/// alphanumeric ones (§11.4.3). Kept as data so ordering is the real
/// semver precedence, never a flattened tag.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PreId {
    /// Digits only, no leading zero (§9: a numeric identifier has no
    /// leading zeros). Parsed to u64 — `part.len() ≤ 10` holds it.
    Numeric(u64),
    /// Contains a letter or hyphen — lexical compare.
    Alpha(String),
}

impl Ord for PreId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        match (self, other) {
            (PreId::Numeric(a), PreId::Numeric(b)) => a.cmp(b),
            // §11.4.3: numeric identifiers sort below alphanumeric.
            (PreId::Numeric(_), PreId::Alpha(_)) => Less,
            (PreId::Alpha(_), PreId::Numeric(_)) => Greater,
            (PreId::Alpha(a), PreId::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for PreId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A semantic version: `MAJOR.MINOR.PATCH`, optional `-prerelease` and
/// `+build`. Parsed per semver §9 and ordered per §11 — a malformed
/// literal (`1.0.0-alpha..1`, `1.0.0+`, `1.0.0-01`) refuses, and
/// `1.0.0-alpha` never equals `1.0.0-beta`. Bounded at 64 bytes — a
/// core version is a number, not a payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// §11.4.1: a release sorts above every prerelease of the same
    /// triple; an empty `prere` is the release. Private — precedence
    /// is the type's own `Ord`, callers compare versions, not parts.
    prere: Vec<PreId>,
}

impl Ord for SemVer {
    /// Semver §11 precedence: numeric triple first, then prerelease —
    /// any prerelease below none, then identifier-by-identifier where
    /// a shorter prefix of the longer set sorts lower. Build metadata
    /// never participates (§10).
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.prere.is_empty(), other.prere.is_empty()) {
                (true, true) => Equal,
                (false, true) => Less,
                (true, false) => Greater,
                (false, false) => {
                    // §11.4.4: compare identifier-by-identifier; when
                    // every shared identifier is equal, the shorter
                    // set is the lower version.
                    let mut ord = Equal;
                    for (a, b) in self.prere.iter().zip(other.prere.iter()) {
                        match a.cmp(b) {
                            Equal => continue,
                            ne => {
                                ord = ne;
                                break;
                            }
                        }
                    }
                    if ord == Equal {
                        self.prere.len().cmp(&other.prere.len())
                    } else {
                        ord
                    }
                }
            })
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl SemVer {
    /// `MAJOR.MINOR.PATCH` with an optional `-pre`/`+build` suffix,
    /// validated per semver §9: dot-separated numeric identifiers with
    /// no leading zeros, prerelease a dot-separated non-empty
    /// identifier list whose numeric identifiers also carry no leading
    /// zeros, build a dot-separated non-empty identifier list. The
    /// whole literal is bounded before any splitting.
    pub fn parse(text: &str) -> Result<SemVer> {
        let raw = text.trim();
        if raw.is_empty() || raw.len() > 64 {
            return Err(Error::rejected(format!(
                "version '{raw}' — expected MAJOR.MINOR.PATCH, ≤64 chars"
            )));
        }
        // Split once on '+': build metadata is validated per §10 and
        // ignored for ordering. A second '+' inside the metadata is a
        // malformed literal, not a nested split.
        let (version, build) = match raw.split_once('+') {
            Some((v, b)) => (v, Some(b)),
            None => (raw, None),
        };
        if let Some(build) = build {
            if build.is_empty()
                || !build
                    .split('.')
                    .all(|id| !id.is_empty() && id.bytes().all(is_ident_byte))
            {
                return Err(Error::rejected(format!(
                    "version '{raw}': build metadata is non-empty dot-separated \
                     [0-9A-Za-z-] identifiers"
                )));
            }
        }
        // The first '-' opens the prerelease section; a '-' inside it
        // is an ordinary identifier byte.
        let (core, pre) = match version.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (version, None),
        };
        let mut parts = core.split('.');
        let num = |part: Option<&str>, what: &str| -> Result<u64> {
            let part = part.unwrap_or_default();
            if part.is_empty()
                || part.len() > 10
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(Error::rejected(format!(
                    "version '{raw}': {what} is not a plain number"
                )));
            }
            part.parse::<u64>()
                .map_err(|_| Error::rejected(format!("version '{raw}': {what} overflow")))
        };
        let major = num(parts.next(), "major")?;
        let minor = num(parts.next(), "minor")?;
        let patch = num(parts.next(), "patch")?;
        if parts.next().is_some() {
            return Err(Error::rejected(format!(
                "version '{raw}' — expected MAJOR.MINOR.PATCH"
            )));
        }
        let mut prere = Vec::new();
        if let Some(pre) = pre {
            if pre.is_empty() {
                return Err(Error::rejected(format!(
                    "version '{raw}': empty prerelease after '-'"
                )));
            }
            for id in pre.split('.') {
                if id.is_empty() || !id.bytes().all(is_ident_byte) {
                    return Err(Error::rejected(format!(
                        "version '{raw}': prerelease identifiers are non-empty \
                         [0-9A-Za-z-]"
                    )));
                }
                if prere.len() >= 8 {
                    return Err(Error::rejected(format!(
                        "version '{raw}': at most 8 prerelease identifiers"
                    )));
                }
                let numeric = id.bytes().all(|b| b.is_ascii_digit());
                if numeric && id.len() > 1 && id.starts_with('0') {
                    // §9: a numeric prerelease identifier has no
                    // leading zero.
                    return Err(Error::rejected(format!(
                        "version '{raw}': numeric prerelease '{id}' has a leading zero"
                    )));
                }
                if numeric && id.len() > 10 {
                    return Err(Error::rejected(format!(
                        "version '{raw}': numeric prerelease '{id}' overflows"
                    )));
                }
                prere.push(if numeric {
                    PreId::Numeric(id.parse().unwrap_or(u64::MAX))
                } else {
                    PreId::Alpha(id.to_string())
                });
            }
        }
        Ok(SemVer {
            major,
            minor,
            patch,
            prere,
        })
    }
}

/// `[0-9A-Za-z-]` — the identifier byte set of semver §9 prerelease
/// and build metadata alike.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

/// One `requires.core` comparator — `>=x`, `>x`, `<=x`, `<x`, `=x`
/// (bare `x` means `=x`). No caret/tilde/wildcard sugar: the package
/// names its floor and ceiling explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cmp {
    Gte,
    Gt,
    Lte,
    Lt,
    Eq,
}

/// A parsed `requires.core` range: an AND of bounded comparators.
#[derive(Clone, Debug)]
pub struct CoreRange(Vec<(Cmp, SemVer)>);

impl core::fmt::Display for SemVer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.prere.is_empty() {
            write!(f, "-")?;
            for (i, id) in self.prere.iter().enumerate() {
                if i > 0 {
                    write!(f, ".")?;
                }
                match id {
                    PreId::Numeric(n) => write!(f, "{n}")?,
                    PreId::Alpha(s) => write!(f, "{s}")?,
                }
            }
        }
        Ok(())
    }
}

impl CoreRange {
    /// The minimum the range requires, when it carries a lower bound —
    /// surfaced so a receipt can say "needs at least", not only pass/fail.
    pub fn minimum(&self) -> Option<SemVer> {
        self.0
            .iter()
            .filter(|(cmp, _)| matches!(cmp, Cmp::Gte | Cmp::Gt))
            .map(|(_, v)| v.clone())
            .max()
    }

    fn parse(text: &str) -> Result<CoreRange> {
        let raw = text.trim();
        if raw.is_empty() || raw.len() > CORE_RANGE_CAP {
            return Err(Error::rejected(format!(
                "requires.core — a bounded range like \">=0.2.0 <2.0.0\", ≤{CORE_RANGE_CAP} chars"
            )));
        }
        let mut clauses = Vec::new();
        for token in raw.split_whitespace() {
            if clauses.len() >= 8 {
                return Err(Error::rejected(
                    "requires.core — at most 8 space-separated comparators",
                ));
            }
            let (cmp, rest) = match token.get(..2) {
                Some(">=") => (Cmp::Gte, &token[2..]),
                Some("<=") => (Cmp::Lte, &token[2..]),
                _ => match token.get(..1) {
                    Some(">") => (Cmp::Gt, &token[1..]),
                    Some("<") => (Cmp::Lt, &token[1..]),
                    Some("=") => (Cmp::Eq, &token[1..]),
                    _ => (Cmp::Eq, token),
                },
            };
            let v = SemVer::parse(rest)
                .map_err(|e| Error::rejected(format!("requires.core comparator '{token}': {e}")))?;
            clauses.push((cmp, v));
        }
        if clauses.is_empty() {
            return Err(Error::rejected(
                "requires.core — at least one comparator like \">=0.2.0\"",
            ));
        }
        Ok(CoreRange(clauses))
    }

    fn holds(&self, version: &SemVer) -> bool {
        self.0.iter().all(|(cmp, v)| {
            let ord = version.cmp(v);
            match cmp {
                Cmp::Gte => ord != std::cmp::Ordering::Less,
                Cmp::Gt => ord == std::cmp::Ordering::Greater,
                Cmp::Lte => ord != std::cmp::Ordering::Greater,
                Cmp::Lt => ord == std::cmp::Ordering::Less,
                Cmp::Eq => ord == std::cmp::Ordering::Equal,
            }
        })
    }
}

/// What `requires:` parses to: a bounded core range plus the exact
/// contract majors the package needs (`name → sorted, distinct`).
/// `None` on a manifest that never declares it — the legacy shape.
#[derive(Clone, Debug, Default)]
pub struct Compat {
    /// The raw `requires.core` text, kept for the receipt and digest —
    /// the bytes the operator read, not just the normalized parse.
    pub core_range_text: Option<String>,
    pub core: Option<CoreRange>,
    pub contracts: BTreeMap<String, Vec<u64>>,
}

impl Compat {
    /// The compat of a manifest that never declares `requires:` — a
    /// legacy package, admitted under the pre-CAD-1154 policy.
    pub fn legacy() -> Compat {
        Compat::default()
    }

    /// Whether this manifest declared any `requires:` at all. A `None`
    /// compat is the legacy package — byte-compatible admission.
    pub fn declared(&self) -> bool {
        self.core.is_some() || !self.contracts.is_empty()
    }

    /// Canonical form for the gate digest: `requires=` covers exactly
    /// what admission enforces, so a `requires:` edit — even a wording
    /// change inside the range string — re-gates the app like a slot
    /// change does.
    pub fn digest_line(&self) -> String {
        match self.declared() {
            false => String::new(),
            true => format!(
                "requires={}\n",
                serde_json::to_string(&json!({
                    "core": self.core_range_text,
                    "contracts": self.contracts,
                }))
                .unwrap_or_default()
            ),
        }
    }

    /// Receipt describing one manifest — what it asked and whether the
    /// host meets each requirement. Reads use this even when the package
    /// is no longer compatible; admission uses `check` below.
    pub fn report(&self, host: &HostContracts) -> Value {
        let mut checks = Vec::new();
        let mut unmet = Vec::new();
        if let Some(range) = &self.core {
            let ok = range.holds(&host.core);
            checks.push(json!({
                "kind": "core",
                "required": self.core_range_text,
                "host": host.core.to_string(),
                "ok": ok,
            }));
            if !ok {
                unmet.push(format!(
                    "app requires core {} — this host is {}",
                    self.core_range_text.as_deref().unwrap_or("<unknown>"),
                    host.core
                ));
            }
        }
        for (name, majors) in &self.contracts {
            let entry = host.entry(name);
            let supported: &[u64] = entry.map(|(m, _)| m).unwrap_or(&[]);
            let missing: Vec<u64> = majors
                .iter()
                .filter(|m| !supported.contains(m))
                .cloned()
                .collect();
            // The receipt names the stage a contract is actually at —
            // "supported" here means a declared/validator-level
            // contract, never an implied executable capability.
            checks.push(json!({
                "kind": "contract",
                "contract": name,
                "required": majors,
                "supported": supported,
                "stage": entry.map(|(_, s)| s.as_str()),
                "ok": missing.is_empty(),
            }));
            if !missing.is_empty() {
                unmet.push(format!(
                    "app requires host contract '{name}' major {} — this host \
                     supports {}",
                    missing
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    if supported.is_empty() {
                        "none (unknown contract)".to_string()
                    } else {
                        supported
                            .iter()
                            .map(u64::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
            }
        }
        let ok = unmet.is_empty();
        json!({
            "schema": 1,
            "ok": ok,
            "compatible": ok,
            "unmet": unmet.first(),
            "declared": self.declared(),
            "core": self.core_range_text,
            "contracts": self.contracts,
            "host": {
                "core": host.core.to_string(),
                "contracts": host.supported.iter().map(|(name, majors, _)| (*name, *majors)).collect::<BTreeMap<_, _>>(),
            },
            "checks": checks,
        })
    }

    /// Admission refuses the first unmet requirement; the read receipt
    /// and this gate share the same per-requirement evaluation.
    pub fn check(&self, host: &HostContracts) -> Result<Value> {
        let report = self.report(host);
        if report["ok"] == false {
            return Err(Error::rejected(
                report["unmet"]
                    .as_str()
                    .unwrap_or("app host requirements are unmet"),
            ));
        }
        Ok(report)
    }

    /// A catalog read can still describe requirements when this build's
    /// version cannot be parsed. Host compatibility is then unknown.
    pub fn unknown_report(&self, reason: &str) -> Value {
        json!({
            "schema": 1,
            "ok": null,
            "compatible": null,
            "status": "unknown",
            "reason": reason,
            "declared": self.declared(),
            "core": self.core_range_text,
            "contracts": self.contracts,
            "host": null,
            "checks": [],
        })
    }
}

/// How much of a named host contract this build provides. A
/// requirement is satisfied only when the host *declares* the major —
/// the status field keeps the receipt honest about what that means:
/// `Validator` is a shipped strict parser (the package's declaration
/// is checked, nothing executes it); `Rendered`/`Sandboxed` are the
/// ui-side consumer stages. No entry here is a dispatched effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContractStage {
    /// A strict `contract/vN` validator exists — well-formed bytes
    /// pass, malformed refuse. No execution implied.
    Validator,
    /// A consumer renders or runs the descriptor in a sandbox.
    Sandboxed,
}

impl ContractStage {
    fn as_str(&self) -> &'static str {
        match self {
            ContractStage::Validator => "validator",
            ContractStage::Sandboxed => "sandboxed",
        }
    }
}

/// The host's own answers, supplied by the caller — never read from a
/// global so the same parser serves the CLI, the daemon and tests.
pub struct HostContracts<'a> {
    /// The running core's version.
    pub core: SemVer,
    /// `name → (majors, stage)` this build ships. Static —
    /// [`SUPPORTED_CONTRACTS`]; callers never extend it ad hoc.
    pub supported: &'a [(&'static str, &'static [u64], ContractStage)],
}

impl HostContracts<'_> {
    /// The registry entry for `name` — `None` for a contract the
    /// registry does not know, which is what makes an invented
    /// requirement refuse rather than silently pass.
    pub fn entry(&self, name: &str) -> Option<(&'static [u64], ContractStage)> {
        self.supported
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, majors, stage)| (*majors, *stage))
    }

    /// Majors the host supports for `name` — empty for a contract the
    /// registry does not know.
    pub fn supported(&self, name: &str) -> &'static [u64] {
        self.entry(name).map(|(majors, _)| majors).unwrap_or(&[])
    }
}

/// The versioned host contracts this build actually ships — the only
/// names `requires.contracts` may carry, each with the stage it is
/// *actually* at, so a receipt can never read as an executable promise:
/// `app-actions` is still a strict *parser* (CAD-964), not a dispatch
/// path; `app-chat`/`app-assistant`/`app-views`/`app-screens` are
/// validated by this build's strict parsers and consumed by shipped
/// surfaces, still not effect execution. Deliberately absent: an effect contract
/// does not exist — a package that declares `app-effects` refuses as
/// unknown (REQ-002's boundary — proposed syntax is unsupported until
/// its own admission lands).
pub const SUPPORTED_CONTRACTS: &[(&str, &[u64], ContractStage)] = &[
    // contracts/app-actions/v1 — src/issue/app_action.rs is the strict
    // validator; no host dispatch exists, so the stage stays honest.
    ("app-actions", &[1], ContractStage::Validator),
    // contracts/app-assistant/v1 — src/issue/app_assistant.rs validator
    // plus the board's assistant surface.
    ("app-assistant", &[1], ContractStage::Sandboxed),
    // contracts/app-chat/v1 — src/issue/app_chat.rs validator plus the
    // board's rendered chat surface.
    ("app-chat", &[1], ContractStage::Sandboxed),
    // src/issue/app_screen_decl.rs — the screens/<tag>/ package runs in
    // the board's sandbox, no live data bridge.
    ("app-screens", &[1], ContractStage::Sandboxed),
    // contracts/app-views/v1 — schema + the board's strict consumer.
    ("app-views", &[1], ContractStage::Sandboxed),
];

/// The host descriptor for this build — the version string the binary
/// was compiled with plus the shipped contract majors. `check` stays
/// pure; this is the one place the build answers for itself. A build
/// whose own `CARGO_PKG_VERSION` does not parse propagates the
/// refusal — there is no invented fallback version to hide behind.
pub fn host() -> Result<HostContracts<'static>> {
    let core = SemVer::parse(env!("CARGO_PKG_VERSION")).map_err(|e| {
        Error::internal(format!(
            "this build's own version {:?} is not a semantic version: {e}",
            env!("CARGO_PKG_VERSION")
        ))
    })?;
    Ok(HostContracts {
        core,
        supported: SUPPORTED_CONTRACTS,
    })
}

/// Parse the `requires:` sub-mapping of `needs:` — called from
/// `parse_manifest` with the `needs` mapping already split. `None`
/// maps to the legacy `Compat` (nothing declared); a malformed
/// supplied requirement fails closed with a named refusal.
///
/// Accepted shape only:
/// - `requires.core`: one string, a bounded AND of comparators
///   (`">=0.2.0 <2.0.0"`); a non-string, an empty range or a bad
///   version refuses.
/// - `requires.contracts`: mapping `name → [major,…]` of plain
///   positive integers; a bare major, a string, a range string or a
///   negative/float refuses.
///
/// Any other key under `requires:` refuses — unknown requirement kinds
/// are never skipped.
pub fn parse_requires(needs: Option<&serde_yaml::Mapping>) -> Result<Compat> {
    let empty = Compat {
        core_range_text: None,
        core: None,
        contracts: BTreeMap::new(),
    };
    let Some(needs) = needs else {
        return Ok(empty);
    };
    let Some(requires) = needs.get(serde_yaml::Value::String("requires".into())) else {
        return Ok(empty);
    };
    let serde_yaml::Value::Mapping(map) = requires else {
        return Err(Error::rejected(
            "app.md `needs.requires` is a mapping — `core:` a version range, \
             `contracts:` name → majors",
        ));
    };
    if map.is_empty() {
        return Err(Error::rejected(
            "app.md `needs.requires` must declare `core` or `contracts`",
        ));
    }
    let mut core_range_text = None;
    let mut core = None;
    let mut contracts = BTreeMap::new();
    for (key, value) in map {
        let k = key.as_str().unwrap_or_default();
        match k {
            "core" => {
                let Some(text) = value.as_str() else {
                    return Err(Error::rejected(
                        "app.md `needs.requires.core` is one range string — \
                         e.g. \">=0.2.0 <2.0.0\"",
                    ));
                };
                let range = CoreRange::parse(text)
                    .map_err(|e| Error::rejected(format!("app.md `needs.requires.core`: {e}")))?;
                core_range_text = Some(text.trim().to_string());
                core = Some(range);
            }
            "contracts" => {
                let serde_yaml::Value::Mapping(table) = value else {
                    return Err(Error::rejected(
                        "app.md `needs.requires.contracts` is a mapping — \
                         name → list of major versions",
                    ));
                };
                if table.is_empty() {
                    return Err(Error::rejected(
                        "app.md `needs.requires.contracts` must name at least one contract",
                    ));
                }
                if table.len() > MAX_REQUIRED_CONTRACTS {
                    return Err(Error::rejected(format!(
                        "app.md `needs.requires.contracts` — at most \
                         {MAX_REQUIRED_CONTRACTS} contracts"
                    )));
                }
                for (name, majors) in table {
                    let Some(name) = name.as_str() else {
                        return Err(Error::rejected(
                            "app.md `needs.requires.contracts` — contract names are strings",
                        ));
                    };
                    if name.is_empty()
                        || name.len() > CONTRACT_NAME_CAP
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                        || name.starts_with('-')
                        || name.ends_with('-')
                    {
                        return Err(Error::rejected(format!(
                            "app.md `needs.requires.contracts` — '{name}' is not a \
                             contract name ([a-z0-9-], ≤{CONTRACT_NAME_CAP})"
                        )));
                    }
                    // Fail closed on the majors list: only a sequence of
                    // plain integers parses — strings, ranges, floats and
                    // negatives are all malformed supplied requirements.
                    let list = match majors {
                        serde_yaml::Value::Sequence(items) => items,
                        serde_yaml::Value::Number(_) | serde_yaml::Value::String(_) => {
                            return Err(Error::rejected(format!(
                                "app.md `needs.requires.contracts.{name}` is a list of \
                                 majors — [{name}: [1]], never a bare number or range"
                            )));
                        }
                        _ => {
                            return Err(Error::rejected(format!(
                                "app.md `needs.requires.contracts.{name}` is a list of \
                                 positive integers"
                            )));
                        }
                    };
                    if list.is_empty() || list.len() > MAX_CONTRACT_MAJORS {
                        return Err(Error::rejected(format!(
                            "app.md `needs.requires.contracts.{name}` — 1 to \
                             {MAX_CONTRACT_MAJORS} majors"
                        )));
                    }
                    let mut parsed = Vec::with_capacity(list.len());
                    for item in list {
                        let Some(major) = item.as_u64() else {
                            return Err(Error::rejected(format!(
                                "app.md `needs.requires.contracts.{name}` majors are \
                                 positive integers — got {item:?}"
                            )));
                        };
                        if major == 0 || major > 99 {
                            return Err(Error::rejected(format!(
                                "app.md `needs.requires.contracts.{name}` major {major} — \
                                 a contract major is 1..99"
                            )));
                        }
                        if parsed.contains(&major) {
                            return Err(Error::rejected(format!(
                                "app.md `needs.requires.contracts.{name}` repeats \
                                 major {major}"
                            )));
                        }
                        parsed.push(major);
                    }
                    parsed.sort_unstable();
                    contracts.insert(name.to_string(), parsed);
                }
            }
            _ => {
                return Err(Error::rejected(format!(
                    "app.md `needs.requires.{k}` is unknown — v0 knows `core` and \
                     `contracts`; anything else refuses rather than being skipped"
                )));
            }
        }
    }
    Ok(Compat {
        core_range_text,
        core,
        contracts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `needs:` YAML into the mapping `parse_requires` expects.
    fn needs(indent_yaml: &str) -> serde_yaml::Mapping {
        let v: serde_yaml::Value = serde_yaml::from_str(&format!("needs:\n{indent_yaml}")).unwrap();
        v.get("needs").unwrap().as_mapping().unwrap().clone()
    }

    fn host_at(core: &str) -> HostContracts<'static> {
        HostContracts {
            core: SemVer::parse(core).unwrap(),
            supported: SUPPORTED_CONTRACTS,
        }
    }

    /// This build's own descriptor parses — `host()` never invents a
    /// fallback version (PM finding: a permissive default would let a
    /// broken build report a guessed floor).
    #[test]
    fn build_version_parses_and_host_reports_it() {
        let host = host().unwrap();
        let literal = env!("CARGO_PKG_VERSION");
        assert_eq!(
            host.core.to_string(),
            SemVer::parse(literal).unwrap().to_string(),
            "host reports the literal it parsed"
        );
    }

    #[test]
    fn absent_requires_is_the_legacy_package() {
        let compat = parse_requires(None).unwrap();
        assert!(!compat.declared());
        let compat = parse_requires(Some(&needs("  connections: [cms]"))).unwrap();
        assert!(!compat.declared());
        // Backward compatibility: a legacy manifest admits on any host.
        assert!(compat.check(&host_at("0.0.1")).is_ok());
    }

    #[test]
    fn core_range_admits_in_range_and_refuses_out() {
        let compat =
            parse_requires(Some(&needs("  requires:\n    core: '>=0.2.0 <2.0.0'"))).unwrap();
        assert!(compat.declared());
        assert_eq!(compat.core.as_ref().unwrap().minimum().unwrap().major, 0);
        assert!(compat.check(&host_at("0.2.0")).is_ok());
        assert!(compat.check(&host_at("1.9.9")).is_ok());
        let e = compat.check(&host_at("0.1.0")).unwrap_err();
        assert!(e.to_string().contains("requires core"), "{e}");
        let e = compat.check(&host_at("2.0.0")).unwrap_err();
        assert!(e.to_string().contains("requires core"), "{e}");
    }

    #[test]
    fn unknown_contract_refuses() {
        // A storage/effect contract does not exist — the registry never
        // invents one, so a package that names it refuses as unknown.
        let compat = parse_requires(Some(&needs(
            "  requires:\n    contracts: {app-storage: [1]}",
        )))
        .unwrap();
        let e = compat.check(&host_at("0.1.0")).unwrap_err();
        assert!(e.to_string().contains("app-storage"), "{e}");
        assert!(e.to_string().contains("none"), "{e}");
    }

    #[test]
    fn supported_and_unsupported_majors() {
        let compat = parse_requires(Some(&needs(
            "  requires:\n    contracts: {app-chat: [1], app-actions: [1]}",
        )))
        .unwrap();
        assert!(compat.check(&host_at("0.1.0")).is_ok());
        // Requiring a major the host does not ship refuses, naming it.
        let compat =
            parse_requires(Some(&needs("  requires:\n    contracts: {app-chat: [2]}"))).unwrap();
        let e = compat.check(&host_at("0.1.0")).unwrap_err();
        assert!(e.to_string().contains("app-chat"), "{e}");
        assert!(e.to_string().contains('2'), "{e}");
    }

    #[test]
    fn malformed_supplied_requires_fails_closed() {
        for bad in [
            "  requires: ''",                                 // not a mapping
            "  requires: {}",                                 // empty declaration
            "  requires: {contracts: {}}",                    // empty contract map
            "  requires:\n    core: 0.2",                     // non-string range
            "  requires:\n    core: '>=1.2'",                 // partial version
            "  requires:\n    core: '>=01.2.3'",              // leading zero
            "  requires:\n    core: ['>=1.2.3']",             // list, not string
            "  requires:\n    contracts: {app-chat: 1}",      // bare major
            "  requires:\n    contracts: {app-chat: 'v1'}",   // string major
            "  requires:\n    contracts: {app-chat: []}",     // empty list
            "  requires:\n    contracts: {app-chat: [0]}",    // zero major
            "  requires:\n    contracts: {app-chat: [-1]}",   // negative
            "  requires:\n    contracts: {app-chat: [1, 1]}", // repeated
            "  requires:\n    contracts: {AppChat: [1]}",     // bad name
            "  requires:\n    bogus: {}",                     // unknown key
            "  requires:\n    core: '>=0.1.0'\n    contracts: {app-chat: [2.5]}",
        ] {
            assert!(
                parse_requires(Some(&needs(bad))).is_err(),
                "{bad} must refuse, not degrade"
            );
        }
    }

    /// Semver §9 grammar: a malformed literal refuses at parse, never
    /// parses partially — `1.0.0-alpha..1`, `1.0.0+`, `1.0.0-01` and a
    /// second `+` all fail closed.
    #[test]
    fn semver_parse_bounds() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "1.2.x",
            "01.2.3",
            "1.0.0-",
            "1.0.0-alpha..1",
            "1.0.0-alpha.",
            "1.0.0-.x",
            "1.0.0+",
            "1.0.0+",
            "1.0.0+build.",
            "1.0.0+build..meta",
            "1.0.0+a+b",
            "1.0.0-01",
            "1.0.0-alpha_1",
        ] {
            assert!(SemVer::parse(bad).is_err(), "{bad}");
        }
        let v = SemVer::parse("1.2.3-beta.1+build").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.to_string(), "1.2.3-beta.1");
        // Release outranks prerelease at the same triple.
        assert!(SemVer::parse("1.0.0").unwrap() > SemVer::parse("1.0.0-beta").unwrap());
    }

    /// Semver §11.4 precedence — the real ordering: prerelease below
    /// release, numeric before alphanumeric, numeric by value,
    /// alphanumeric lexically, and a shorter prefix of equal
    /// identifiers below the longer. A requirement on one prerelease
    /// never admits another.
    #[test]
    fn semver_full_precedence() {
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for i in 0..chain.len() {
            for j in 0..chain.len() {
                let a = SemVer::parse(chain[i]).unwrap();
                let b = SemVer::parse(chain[j]).unwrap();
                let (ia, ib) = (chain[i], chain[j]);
                assert_eq!(a.cmp(&b), i.cmp(&j), "{ia} vs {ib}");
            }
        }
        // A package pinning one prerelease refuses another: `=1.0.0-alpha`
        // is not satisfied by `1.0.0-beta` — the exact bug the review caught.
        let compat = parse_requires(Some(&needs("  requires: {core: '=1.0.0-alpha'}"))).unwrap();
        let err = compat.check(&host_at("1.0.0-beta")).unwrap_err();
        assert!(err.to_string().contains("requires core"), "{err}");
        assert!(compat.check(&host_at("1.0.0-alpha")).is_ok());
        // A host on a prerelease sits below the stable floor a package
        // floors on — `>=1.0.0` refuses `1.0.0-rc.1`.
        let compat = parse_requires(Some(&needs("  requires: {core: '>=1.0.0'}"))).unwrap();
        assert!(compat.check(&host_at("1.0.0-rc.1")).is_err());
        assert!(compat.check(&host_at("1.0.0")).is_ok());
        // …and above it admits a prerelease still inside the range.
        let compat = parse_requires(Some(&needs("  requires: {core: '>=1.0.0-alpha'}"))).unwrap();
        assert!(compat.check(&host_at("1.0.0-beta")).is_ok());
    }

    /// The `app.md` frontmatter wires `needs.requires` through
    /// `parse_manifest` — a manifest that never declares it keeps
    /// parsing as the legacy package, and a malformed supplied
    /// requirement fails the manifest, not just the sub-parser.
    #[test]
    fn manifest_wires_requires() {
        let legacy = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  connections: []\n---\n\nG\n";
        let m = super::super::parse_manifest(legacy).unwrap();
        assert!(!m.requires.declared(), "absent requires is legacy");

        let declared = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: {core: \">=0.0.0\", contracts: {app-chat: [1]}}\n---\n\nG\n";
        let m = super::super::parse_manifest(declared).unwrap();
        assert!(m.requires.declared());
        assert_eq!(m.requires.contracts["app-chat"], vec![1]);

        // A malformed supplied requirement refuses the whole manifest —
        // it never degrades to the legacy package.
        let malformed = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: {contracts: {app-chat: [0]}}\n---\n\nG\n";
        assert!(super::super::parse_manifest(malformed).is_err());
        let nonmap = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: ''\n---\n\nG\n";
        assert!(super::super::parse_manifest(nonmap).is_err());
        // A malformed *version literal* inside a supplied range refuses
        // the manifest too — it never degrades to legacy.
        let bad_ver = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: {core: '>=1.0.0-alpha..1'}\n---\n\nG\n";
        assert!(super::super::parse_manifest(bad_ver).is_err());
        let bad_build = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: {core: '>=1.0.0+'}\n---\n\nG\n";
        assert!(super::super::parse_manifest(bad_build).is_err());
    }

    #[test]
    fn digest_line_changes_on_requires() {
        let legacy = Compat::legacy();
        assert!(legacy.digest_line().is_empty());
        let declared = parse_requires(Some(&needs(
            "  requires:\n    core: '>=0.1.0'\n    contracts: {app-chat: [1]}",
        )))
        .unwrap();
        let line = declared.digest_line();
        assert!(line.starts_with("requires="), "{line}");
        assert!(line.contains("app-chat"), "{line}");
        // Different requirement → different digest line (re-gate).
        let other = parse_requires(Some(&needs(
            "  requires:\n    core: '>=0.2.0'\n    contracts: {app-chat: [1]}",
        )))
        .unwrap();
        assert_ne!(declared.digest_line(), other.digest_line());
    }
}
