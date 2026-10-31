//! The one versioned JSON entry point every non-native host calls.
//!
//! Input `{v: 1, op, input}`; output `{ok: true, op, value}` (`op` echoes the request) or
//! `{ok: false, code, message}`. The rules live in typed Rust functions
//! ([`manifest_preflight`]); [`evaluate`] only decodes, dispatches and encodes.
//! Domain input only: not caller authentication, and not a host
//! compatibility decision (the native host stays authoritative for that).
//!
//! `app.requirements.evaluate` is the one compatibility answer a non-native
//! host may quote. It parses the exact manifest with the same rules as the
//! native `needs.requires` parser and evaluates them against a target profile
//! the *caller* supplies. There is no default profile: the native registry
//! ([`crate::app_compat::SUPPORTED_CONTRACTS`]) describes this binary, not a
//! Worker, a tenant or any other host, and is never substituted here. An
//! absent or malformed target refuses; a valid target that does not meet the
//! package reports `compatible: false` with the per-requirement checks.
use crate::app_compat::{parse_requires, Compat, ContractStage, HostContracts, SemVer};
use crate::frontmatter::split_front;
use crate::issue::{self, ReadinessInput, ReadinessValue, ValidateInput, ValidateValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_yaml::Value as Yaml;
use std::collections::BTreeMap;

pub const CONTRACT_VERSION: u32 = 1;
/// Native `MAX_FILE_BYTES` (and the published bundle grammar's `fileBytes`).
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// A manifest can escape to six bytes per input byte inside the envelope.
pub const MAX_INPUT_BYTES: usize = 6 * MAX_MANIFEST_BYTES + 4_096;

/// `target.contracts` — a profile names at most this many host contracts,
/// the same bound the manifest's `requires.contracts` carries.
const MAX_TARGET_CONTRACTS: usize = 16;
/// Majors one profile entry may declare — the manifest's own contract bound.
const MAX_TARGET_MAJORS: usize = 8;
/// A contract name is a bounded dotted tag, as in the manifest grammar.
const CONTRACT_NAME_CAP: usize = 32;
/// A contract major is a small positive integer, as in the manifest grammar.
const CONTRACT_MAJOR_MAX: u64 = 99;

/// Closed set of refusal codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// The envelope is malformed, oversize, unversioned or names no known op.
    InvalidCommand,
    /// The manifest is not a well-formed `app.md` with a valid `needs.requires`.
    InvalidManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: Code,
    pub message: String,
}

fn refuse(code: Code, message: impl std::fmt::Display) -> Refusal {
    Refusal {
        code,
        message: message.to_string(),
    }
}

/// The request envelope.
#[derive(Deserialize, JsonSchema)]
pub struct Request {
    /// Contract version; only `1` exists.
    pub v: u32,
    #[serde(flatten)]
    pub operation: Operation,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "op", content = "input")]
pub enum Operation {
    /// Split the frontmatter, then check `needs.requires`.
    #[serde(rename = "app.manifest.preflight")]
    AppManifestPreflight(ManifestPreflightInput),
    /// Evaluate `needs.requires` against an explicit target profile.
    #[serde(rename = "app.requirements.evaluate")]
    AppRequirementsEvaluate(RequirementsInput),
    /// Check an issue id, project key, tags, status and priority.
    #[serde(rename = "issue.validate")]
    IssueValidate(ValidateInput),
    /// Whether an issue is ready or blocked, given its blockers' statuses.
    #[serde(rename = "issue.readiness")]
    IssueReadiness(ReadinessInput),
    /// Classify one delivery and answer what it needs to merge (CAD-1946).
    ///
    /// The evidence is the caller's own reads: the project's `delivery:`
    /// section, its recorded approval, the diff rows and the trusted-base list
    /// texts. Nothing here reads a file or a program, and no input carries a
    /// class.
    #[serde(rename = "delivery.requirements.evaluate")]
    DeliveryRequirementsEvaluate(DeliveryRequirementsInput),
}

/// Input of `delivery.requirements.evaluate`.
///
/// Every field is evidence the caller retrieved itself: the project's
/// `delivery:` section, its recorded approval, the diff rows and the
/// trusted-base list texts. No field carries a class, and nothing here reads a
/// file, a program or a network answer.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryRequirementsInput {
    /// The exact `delivery:` section text, or absent when the project has none.
    pub policy: Option<String>,
    /// The digest the project's work approval recorded for its delivery policy.
    pub approval_digest: Option<String>,
    /// The policy that approval recorded (absent = approved "no section").
    pub approval: Option<Value>,
    /// `git diff --raw -z --no-renames <merge-base> <head>` rows.
    pub changes: Vec<Value>,
    /// `git diff --numstat` total changed lines.
    pub lines: u64,
    /// The trusted base's `docs/roles/risk-paths.toml` text.
    pub risk_paths: Option<String>,
    /// The trusted base's `docs/roles/one-review-paths.toml` text.
    pub one_review_paths: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestPreflightInput {
    /// The exact `app.md` text.
    pub manifest: String,
}

/// Input of `app.requirements.evaluate`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequirementsInput {
    /// The exact `app.md` text.
    pub manifest: String,
    /// The host being asked. Required: there is no native, hosted or other
    /// default profile, and a missing one is refused, never assumed.
    pub target: TargetProfile,
}

/// A host's own capability profile, supplied by the caller. This crate never
/// substitutes the native registry ([`crate::app_compat::SUPPORTED_CONTRACTS`])
/// for it: native deliberately provides a different contract set (for example
/// it does not serve `app-storage`), so copying it would answer for the wrong
/// host. Contract names are host-owned data here, not entries in this build's
/// registry — a target that really serves a contract may name it.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetProfile {
    /// The running *host's* version: an exact semantic version. This is the
    /// platform the package would run on, never the rules-core/wasm release
    /// that computes the answer — those are different versions, and a caller
    /// that cannot prove the host's own version must refuse rather than send
    /// the rules version.
    pub core: String,
    /// Every host contract the target provides. An explicit empty map is a
    /// host that provides none; omitting the field is an absent profile and
    /// refuses.
    pub contracts: BTreeMap<String, TargetContract>,
}

/// One contract a target provides, with the stage it is actually at.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetContract {
    /// The contract majors this target serves, 1 to 8 values in 1..=99.
    pub majors: Vec<u64>,
    /// What the target actually does with the contract — never more.
    pub stage: TargetStage,
}

/// The stage a target declares for a contract. Closed set: an unknown stage
/// is a profile this core cannot evaluate and refuses.
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetStage {
    /// A strict validator exists; nothing executes the declaration.
    Validator,
    /// A consumer renders or runs the descriptor in a sandbox.
    Sandboxed,
}

impl From<TargetStage> for ContractStage {
    fn from(stage: TargetStage) -> Self {
        match stage {
            TargetStage::Validator => ContractStage::Validator,
            TargetStage::Sandboxed => ContractStage::Sandboxed,
        }
    }
}

/// Success value of `app.requirements.evaluate`: the native compatibility
/// receipt, typed. It is the value [`Compat::report`] produced for the
/// supplied target, so native and non-native hosts quote one receipt shape.
///
/// This type is the *published shape* of the receipt (the op emits the
/// receipt itself); `receipt_matches_the_published_shape` pins it.
///
/// **Authority**: the profile is caller data, not authenticated by this
/// function. `compatible: true` means "every declared requirement is met by
/// the exact profile supplied", never "this tenant is authorized to install".
/// A consumer that admits must first establish that the profile it sends is
/// its own current, authentic host profile, and must bind the answer to the
/// exact manifest bytes and profile it supplied.
///
/// `ok` here is the receipt's compatibility verdict (as in the native
/// receipt), not the envelope's `ok`, which only says the op ran.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequirementsValue {
    /// Receipt schema; `1` is the only version.
    #[schemars(extend("const" = 1))]
    pub schema: u32,
    /// Compatibility verdict, always equal to `compatible`.
    pub ok: bool,
    /// Whether every declared requirement is met by the target.
    pub compatible: bool,
    /// The first unmet requirement in the native wording; `null` when
    /// compatible. The full set is in `checks`.
    pub unmet: Option<String>,
    /// Whether the manifest declared any `needs.requires` at all.
    pub declared: bool,
    /// The declared `requires.core` range text; `null` when absent.
    pub core: Option<String>,
    /// The declared contract requirements, name → sorted majors.
    pub contracts: BTreeMap<String, Vec<u64>>,
    /// The target that was evaluated.
    pub host: RequirementsHost,
    /// One entry per checked requirement, in declaration order (core first,
    /// then contracts by name).
    pub checks: Vec<RequirementCheck>,
}

/// The version and contracts of the target that produced a receipt.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequirementsHost {
    /// The target's version, as it participated in precedence.
    pub core: String,
    /// The target's declared contracts, name → sorted majors.
    pub contracts: BTreeMap<String, Vec<u64>>,
}

/// One requirement and whether the target met it.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequirementCheck {
    /// The manifest's `requires.core` range against the target's version.
    Core {
        /// The declared range text.
        required: Option<String>,
        /// The target's version.
        host: String,
        /// Whether the target's version falls inside the range.
        ok: bool,
    },
    /// One declared contract major list against the target's profile.
    Contract {
        /// The contract the manifest requires.
        contract: String,
        /// The majors the manifest requires.
        required: Vec<u64>,
        /// The majors the target declares for `contract`; empty when the
        /// target does not provide it.
        supported: Vec<u64>,
        /// The target's stage for `contract`; `null` when it does not
        /// provide it.
        stage: Option<TargetStage>,
        /// Whether every required major is supported.
        ok: bool,
    },
}

/// Success value of `app.manifest.preflight`.
#[derive(Serialize, JsonSchema)]
pub struct ManifestPreflightValue {}

/// The success value of whichever operation ran, keyed by the echoed `op`.
#[derive(Serialize, JsonSchema)]
#[serde(tag = "op", content = "value")]
pub enum OpValue {
    #[serde(rename = "app.manifest.preflight")]
    Preflight(ManifestPreflightValue),
    /// The native receipt [`Compat::report`] produced; its published shape is
    /// [`RequirementsValue`], and the value is emitted as produced rather than
    /// re-decoded, so a shape drift is a test failure, never a runtime trap.
    #[serde(rename = "app.requirements.evaluate")]
    #[schemars(with = "RequirementsValue")]
    Requirements(Value),
    #[serde(rename = "issue.validate")]
    Validate(ValidateValue),
    #[serde(rename = "issue.readiness")]
    Readiness(ReadinessValue),
    /// The delivery-requirements document: the class, the reviews it needs and
    /// the policy it was decided under (CAD-1946).
    #[serde(rename = "delivery.requirements.evaluate")]
    DeliveryRequirements(Value),
}

/// Success response.
#[derive(Serialize, JsonSchema)]
pub struct Success {
    #[schemars(extend("const" = true))]
    pub ok: bool,
    #[serde(flatten)]
    pub result: OpValue,
}

/// Refusal response.
#[derive(Serialize, JsonSchema)]
pub struct Refused {
    #[schemars(extend("const" = false))]
    pub ok: bool,
    pub code: Code,
    /// Human-readable refusal reason.
    pub message: String,
}

/// Structural preflight only: admit a well-formed `needs.requires` block and
/// refuse a malformed one. Host compatibility is never inferred here.
pub fn manifest_preflight(manifest: &str) -> Result<(), Refusal> {
    manifest_compat(manifest).map(|_| ())
}

/// Parse the exact manifest into the same [`Compat`] the native installer
/// uses: bounded bytes, one frontmatter split, the native `parse_requires`.
/// A manifest that declares nothing is the legacy shape (`Compat::default`).
fn manifest_compat(manifest: &str) -> Result<Compat, Refusal> {
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(refuse(
            Code::InvalidCommand,
            "manifest exceeds its byte limit",
        ));
    }
    let (yaml, _) = split_front(manifest).map_err(|error| refuse(Code::InvalidManifest, error))?;
    // `serde_yaml::Value` is also the native representation: duplicate
    // mapping keys refuse, and scalar typing survives.
    let Yaml::Mapping(frontmatter) =
        serde_yaml::from_str::<Yaml>(yaml).map_err(|error| refuse(Code::InvalidManifest, error))?
    else {
        return Err(refuse(
            Code::InvalidManifest,
            "frontmatter must be a mapping",
        ));
    };
    let needs = match frontmatter.get("needs") {
        // No declaration is a legacy package, exactly as natively parsed.
        None => None,
        // An explicit non-mapping `needs` (including null) is malformed.
        Some(Yaml::Mapping(needs)) => Some(needs),
        Some(_) => return Err(refuse(Code::InvalidManifest, "needs must be a mapping")),
    };
    parse_requires(needs).map_err(|error| refuse(Code::InvalidManifest, error))
}

/// A target profile after validation: an owned, canonical form that
/// [`HostContracts`] can borrow.
struct CheckedTarget {
    core: SemVer,
    /// Name → sorted majors → stage, ordered by name.
    contracts: Vec<(String, Vec<u64>, ContractStage)>,
}

/// Validate a caller-supplied profile strictly and fail closed. This is the
/// only place a compatibility answer's host side enters, so a missing,
/// unknown or inconsistent profile refuses instead of degrading to a default.
fn check_target(target: &TargetProfile) -> Result<CheckedTarget, Refusal> {
    // `SemVer::parse` trims; a profile must carry the version exactly, so
    // `" 0.3.0"` is malformed rather than silently accepted.
    if target.core.is_empty() || target.core.trim() != target.core {
        return Err(refuse(
            Code::InvalidCommand,
            "target.core is one exact semantic version, without surrounding whitespace",
        ));
    }
    let core = SemVer::parse(&target.core)
        .map_err(|error| refuse(Code::InvalidCommand, format!("target.core: {error}")))?;
    if target.contracts.len() > MAX_TARGET_CONTRACTS {
        return Err(refuse(
            Code::InvalidCommand,
            format!("target.contracts — at most {MAX_TARGET_CONTRACTS} contracts"),
        ));
    }
    let mut contracts = Vec::with_capacity(target.contracts.len());
    // `BTreeMap` iteration is name-ordered, so the receipt is deterministic.
    for (name, entry) in &target.contracts {
        if !valid_contract_name(name) {
            return Err(refuse(
                Code::InvalidCommand,
                format!(
                    "target.contracts — '{name}' is not a contract name \
                     ([a-z0-9-], ≤{CONTRACT_NAME_CAP})"
                ),
            ));
        }
        if entry.majors.is_empty() || entry.majors.len() > MAX_TARGET_MAJORS {
            return Err(refuse(
                Code::InvalidCommand,
                format!("target.contracts.{name}.majors — 1 to {MAX_TARGET_MAJORS} majors"),
            ));
        }
        let mut majors = Vec::with_capacity(entry.majors.len());
        for &major in &entry.majors {
            if major == 0 || major > CONTRACT_MAJOR_MAX {
                return Err(refuse(
                    Code::InvalidCommand,
                    format!(
                        "target.contracts.{name}.majors — {major} is not a contract \
                         major (1..{CONTRACT_MAJOR_MAX})"
                    ),
                ));
            }
            if majors.contains(&major) {
                return Err(refuse(
                    Code::InvalidCommand,
                    format!("target.contracts.{name}.majors repeats {major}"),
                ));
            }
            majors.push(major);
        }
        majors.sort_unstable();
        contracts.push((name.clone(), majors, entry.stage.into()));
    }
    Ok(CheckedTarget { core, contracts })
}

/// The contract-name grammar of the manifest's `requires.contracts`: lowercase
/// ASCII letters, digits and `-`, no leading or trailing `-`, bounded. Kept in
/// lockstep with `app_compat`'s parser by
/// `target_names_match_the_manifest_contract_grammar`.
fn valid_contract_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= CONTRACT_NAME_CAP
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// One delivery classification from the evidence the caller gathered.
///
/// The policy, its approval, the diff rows and the trusted lists decide; a
/// malformed anything is a refusal, never a weaker requirement. The answer
/// names the resolved policy beside the requirements so a reader can see which
/// policy produced the class.
fn delivery_requirements_value(input: &DeliveryRequirementsInput) -> Result<Value, Refusal> {
    use crate::delivery::{effective, parse, Approved, DeliveryPolicy};
    use crate::delivery_requirements::{classify, Change, TrustedLists};

    let file = match input.policy.as_deref() {
        None => Ok(None),
        Some(text) => parse(text).map_err(|error| error.to_string()),
    };
    let approved = match input.approval_digest.as_deref() {
        None => None,
        Some(digest) => {
            let policy = match &input.approval {
                None => None,
                Some(value) => Some(
                    serde_json::from_value::<DeliveryPolicy>(value.clone())
                        .map_err(|error| refuse(Code::InvalidCommand, error.to_string()))?,
                ),
            };
            Some(Approved {
                digest: digest.to_string(),
                policy,
            })
        }
    };
    let resolved = effective("project", file, approved.as_ref());
    if let Some(note) = &resolved.note {
        // The policy in force is not the one the file declares. That is a fact
        // for the caller to surface, and the strict requirements below already
        // reflect it — never silently the file's weaker reading.
        let _ = note;
    }
    let mut changes = Vec::with_capacity(input.changes.len());
    for value in &input.changes {
        changes.push(
            serde_json::from_value::<Change>(value.clone())
                .map_err(|error| refuse(Code::InvalidCommand, error.to_string()))?,
        );
    }
    let lists = TrustedLists {
        risk_paths: input.risk_paths.as_deref(),
        one_review: input.one_review_paths.as_deref(),
    };
    let requirements = classify(&resolved, &changes, input.lines, &lists);
    serde_json::to_value(serde_json::json!({
        "policy": resolved.policy,
        "policy_source": resolved.source,
        "policy_digest": resolved.digest,
        "policy_note": resolved.note,
        "requirements": requirements,
    }))
    .map_err(|error| refuse(Code::InvalidCommand, error.to_string()))
}

fn dispatch(input: &[u8]) -> Result<OpValue, Refusal> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(refuse(
            Code::InvalidCommand,
            "command exceeds its byte limit",
        ));
    }
    // Duplicate envelope fields refuse here; `serde_json::Value` would
    // silently collapse them.
    serde_json::from_slice::<Yaml>(input)
        .map_err(|_| refuse(Code::InvalidCommand, "command must be unambiguous JSON"))?;
    let value: Value = serde_json::from_slice(input)
        .map_err(|_| refuse(Code::InvalidCommand, "command must be valid JSON"))?;
    let Some(object) = value.as_object() else {
        return Err(refuse(Code::InvalidCommand, "command must be an object"));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "v" | "op" | "input"))
    {
        return Err(refuse(Code::InvalidCommand, "unknown command field"));
    }
    if object.get("v").and_then(Value::as_u64) != Some(CONTRACT_VERSION.into()) {
        return Err(refuse(Code::InvalidCommand, "unsupported contract version"));
    }
    let request: Request = serde_json::from_value(value)
        .map_err(|error| refuse(Code::InvalidCommand, error.to_string()))?;
    match request.operation {
        Operation::AppManifestPreflight(input) => {
            manifest_preflight(&input.manifest)?;
            Ok(OpValue::Preflight(ManifestPreflightValue {}))
        }
        Operation::AppRequirementsEvaluate(input) => {
            // After the envelope decodes, the manifest is parsed before the
            // profile's semantic checks: a malformed package with a decodable
            // profile stays `invalid_manifest`.
            let compat = manifest_compat(&input.manifest)?;
            let target = check_target(&input.target)?;
            let supported: Vec<(&str, &[u64], ContractStage)> = target
                .contracts
                .iter()
                .map(|(name, majors, stage)| (name.as_str(), majors.as_slice(), *stage))
                .collect();
            let host = HostContracts {
                core: target.core,
                supported: &supported,
            };
            // One receipt source: the native evaluation, not a second copy.
            // Emitted as produced: `RequirementsValue` publishes the shape.
            Ok(OpValue::Requirements(compat.report(&host)))
        }
        Operation::DeliveryRequirementsEvaluate(input) => Ok(OpValue::DeliveryRequirements(
            delivery_requirements_value(&input)?,
        )),
        Operation::IssueValidate(input) => Ok(OpValue::Validate(issue::validate(&input))),
        Operation::IssueReadiness(input) => Ok(OpValue::Readiness(issue::readiness(&input))),
    }
}

/// Encode a refusal exactly as [`evaluate`] does.
pub fn refusal_bytes(code: Code, message: &str) -> Vec<u8> {
    encode(&Refused {
        ok: false,
        code,
        message: message.into(),
    })
}

fn encode(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).expect("bounded JSON result")
}

pub fn evaluate(input: &[u8]) -> Vec<u8> {
    match dispatch(input) {
        Ok(value) => encode(&Success {
            ok: true,
            result: value,
        }),
        Err(Refusal { code, message }) => refusal_bytes(code, &message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One `app.requirements.evaluate` envelope.
    fn request(manifest: &str, target: Value) -> Vec<u8> {
        let mut input = serde_json::json!({ "manifest": manifest });
        if !target.is_null() {
            input["target"] = target;
        }
        serde_json::to_vec(&serde_json::json!({
            "v": CONTRACT_VERSION,
            "op": "app.requirements.evaluate",
            "input": input,
        }))
        .unwrap()
    }

    /// A target profile JSON with `(name, majors, stage)` entries.
    fn profile(core: &str, contracts: &[(&str, &[u64], &str)]) -> Value {
        let mut table = serde_json::Map::new();
        for (name, majors, stage) in contracts {
            table.insert(
                (*name).to_string(),
                serde_json::json!({ "majors": majors, "stage": stage }),
            );
        }
        serde_json::json!({ "core": core, "contracts": table })
    }

    fn call(manifest: &str, target: Value) -> Value {
        serde_json::from_slice(&evaluate(&request(manifest, target))).unwrap()
    }

    fn refusal(manifest: &str, target: Value) -> (String, String) {
        let response = call(manifest, target);
        assert_eq!(response["ok"], false, "{response}");
        (
            response["code"].as_str().unwrap().to_string(),
            response["message"].as_str().unwrap().to_string(),
        )
    }

    const REQUIRES_STORAGE: &str = "---\nname: demo\nneeds:\n  requires:\n    \
        contracts: {app-storage: [1]}\n---\nbody\n";

    /// The delivery op must answer exactly what the native evaluator answers
    /// for the same evidence — one implementation, two callers.
    #[test]
    fn delivery_requirements_op_matches_the_native_evaluator() {
        use crate::delivery::{default_policy, effective};
        use crate::delivery_requirements::{classify, TrustedLists};

        let request = |input: Value| {
            serde_json::to_vec(&serde_json::json!({
                "v": CONTRACT_VERSION,
                "op": "delivery.requirements.evaluate",
                "input": input,
            }))
            .unwrap()
        };
        let lists = TrustedLists {
            risk_paths: Some(""),
            one_review: Some(""),
        };
        let native = classify(
            &effective("project", Ok(None), None),
            &[],
            0,
            &TrustedLists {
                risk_paths: Some(""),
                one_review: Some(""),
            },
        );
        let response: Value = serde_json::from_slice(&evaluate(&request(serde_json::json!({
            "policy": null,
            "approval_digest": null,
            "approval": null,
            "changes": [],
            "lines": 0,
            "risk_paths": "",
            "one_review_paths": "",
        }))))
        .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(
            response["value"]["requirements"],
            serde_json::to_value(&native).unwrap(),
            "the op and the native evaluator disagree"
        );
        assert_eq!(
            response["value"]["policy"],
            serde_json::to_value(default_policy()).unwrap()
        );
        let _ = lists;

        // A malformed section is the default policy plus its note, and the
        // strict requirements that note implies — never the file's weaker
        // reading, and never a silent default.
        let bad: Value = serde_json::from_slice(&evaluate(&request(serde_json::json!({
            "policy": "---\ndelivery: {bogus: 1}\n---\n",
            "approval_digest": null,
            "approval": null,
            "changes": [],
            "lines": 0,
            "risk_paths": null,
            "one_review_paths": null,
        }))))
        .unwrap();
        assert_eq!(bad["ok"], true, "{bad}");
        assert_eq!(bad["value"]["policy_source"], "default");
        assert!(
            bad["value"]["policy_note"]
                .as_str()
                .is_some_and(|note| note.starts_with("delivery_error:")),
            "{bad}"
        );
        assert_eq!(bad["value"]["requirements"]["class"], "strict", "{bad}");
        assert_eq!(
            bad["value"]["requirements"]["operator_approval"], true,
            "{bad}"
        );
    }

    #[test]
    fn hosted_profile_is_the_only_host_authority() {
        // `app-storage` is not in this build's native registry: only the
        // caller's profile may say a host serves it.
        let compatible = call(
            REQUIRES_STORAGE,
            profile("0.3.0", &[("app-storage", &[1], "sandboxed")]),
        );
        assert_eq!(compatible["value"]["compatible"], true, "{compatible}");
        assert_eq!(compatible["value"]["checks"][0]["ok"], true);

        // The same package on a host that does not declare it is refused by
        // the evaluation, never admitted by a native fallback.
        let incompatible = call(
            REQUIRES_STORAGE,
            profile("0.3.0", &[("app-chat", &[1], "sandboxed")]),
        );
        assert_eq!(incompatible["value"]["compatible"], false);
        assert_eq!(incompatible["value"]["checks"][0]["supported"], json!([]));
        assert_eq!(incompatible["value"]["checks"][0]["stage"], Value::Null);

        // A contract native ships is not compatible on a target that omits it.
        let native_only = call(
            "---\nneeds:\n  requires:\n    contracts: {crm-campaign-work: [1]}\n---\n",
            profile("0.3.0", &[("app-chat", &[1], "sandboxed")]),
        );
        assert_eq!(native_only["value"]["compatible"], false, "{native_only}");
    }

    #[test]
    fn receipt_is_the_native_report() {
        let manifest = "---\nneeds:\n  requires:\n    core: \">=0.2.0 <2.0.0\"\n    \
            contracts: {app-chat: [1]}\n---\n";
        let response = call(
            manifest,
            profile("0.3.0", &[("app-chat", &[1], "sandboxed")]),
        );
        let compat = manifest_compat(manifest).expect("the manifest parses");
        let host = HostContracts {
            core: SemVer::parse("0.3.0").unwrap(),
            supported: &[("app-chat", &[1], ContractStage::Sandboxed)],
        };
        assert_eq!(
            response["value"],
            compat.report(&host),
            "the op must quote the native receipt, not a second evaluation"
        );
    }

    #[test]
    fn receipt_matches_the_published_shape() {
        // The emitted receipt must decode into the type the success schema is
        // generated from, with no field left over or missing. This is the
        // compile-time-checked substitute for decoding at runtime.
        for (manifest, target) in [
            (
                "---\nneeds:\n  requires:\n    core: \">=0.2.0\"\n    \
                 contracts: {app-chat: [1], app-storage: [1]}\n---\n",
                profile("0.3.0", &[("app-chat", &[1], "sandboxed")]),
            ),
            ("---\nname: demo\n---\n", profile("0.3.0", &[])),
            (
                "---\nneeds:\n  requires:\n    core: \">=0.4.0\"\n---\n",
                profile("0.3.0", &[]),
            ),
        ] {
            let response = call(manifest, target);
            let typed: RequirementsValue =
                serde_json::from_value(response["value"].clone()).expect("published shape");
            assert_eq!(
                serde_json::to_value(&typed).expect("typed receipt serializes"),
                response["value"]
            );
        }
    }

    #[test]
    fn legacy_manifest_needs_nothing() {
        // No `requires:` is the legacy shape natively: nothing declared,
        // nothing to check, compatible even against an empty profile.
        let response = call("---\nname: demo\n---\nbody\n", profile("0.3.0", &[]));
        let value = &response["value"];
        assert_eq!(response["ok"], true);
        assert_eq!(value["declared"], false);
        assert_eq!(value["compatible"], true);
        assert_eq!(value["unmet"], Value::Null);
        assert_eq!(value["core"], Value::Null);
        assert_eq!(value["checks"], json!([]));
        assert_eq!(value["host"]["contracts"], json!({}));
    }

    #[test]
    fn empty_profile_never_admits_a_requirement() {
        let response = call(REQUIRES_STORAGE, profile("0.3.0", &[]));
        assert_eq!(response["value"]["compatible"], false, "{response}");
        assert_eq!(response["value"]["checks"][0]["ok"], false);
    }

    #[test]
    fn absent_or_malformed_profiles_refuse() {
        let manifest = "---\nname: demo\n---\n";

        // Absent: no default profile, not even the native registry.
        let (code, message) = refusal(manifest, Value::Null);
        assert_eq!(code, "invalid_command");
        assert!(message.contains("target"), "{message}");

        // Unknown field inside the profile.
        let (code, message) = refusal(
            manifest,
            json!({ "core": "0.3.0", "contracts": {}, "native": true }),
        );
        assert_eq!(code, "invalid_command");
        assert!(message.contains("native"), "{message}");

        // Unknown stage — a capability this core cannot evaluate.
        let (code, message) = refusal(manifest, profile("0.3.0", &[("app-chat", &[1], "native")]));
        assert_eq!(code, "invalid_command");
        assert!(message.contains("sandboxed"), "{message}");

        // Malformed version, and a version carrying surrounding whitespace.
        for core in ["0.3", "", " 0.3.0", "0.3.0 "] {
            let (code, message) = refusal(manifest, profile(core, &[]));
            assert_eq!(code, "invalid_command", "{core}: {message}");
            assert!(message.contains("target.core"), "{core}: {message}");
        }

        // Malformed contract entries.
        for target in [
            profile("0.3.0", &[("App-Chat", &[1], "sandboxed")]),
            profile("0.3.0", &[("app_chat", &[1], "sandboxed")]),
            profile("0.3.0", &[("-app-chat", &[1], "sandboxed")]),
            profile("0.3.0", &[("app-chat", &[], "sandboxed")]),
            profile("0.3.0", &[("app-chat", &[0], "sandboxed")]),
            profile("0.3.0", &[("app-chat", &[100], "sandboxed")]),
            profile("0.3.0", &[("app-chat", &[1, 1], "sandboxed")]),
            profile(
                "0.3.0",
                &[("app-chat", &[1, 2, 3, 4, 5, 6, 7, 8, 9], "sandboxed")],
            ),
        ] {
            let (code, _) = refusal(manifest, target);
            assert_eq!(code, "invalid_command");
        }

        // Too many contracts for a profile.
        let many: Vec<(String, Vec<u64>, &str)> = (0..=MAX_TARGET_CONTRACTS)
            .map(|i| (format!("app-c{i}"), vec![1], "validator"))
            .collect();
        let table: serde_json::Map<String, Value> = many
            .iter()
            .map(|(name, majors, stage)| {
                (name.clone(), json!({ "majors": majors, "stage": stage }))
            })
            .collect();
        let (code, _) = refusal(manifest, json!({ "core": "0.3.0", "contracts": table }));
        assert_eq!(code, "invalid_command");
    }

    #[test]
    fn malformed_manifest_stays_invalid_manifest() {
        // The manifest is parsed before the profile's semantic checks, so a
        // broken package with a decodable profile is `invalid_manifest`, not
        // a profile complaint.
        let (code, message) = refusal(
            "---\nneeds:\n  requires:\n    core: 2\n---\n",
            profile("0.3.0", &[]),
        );
        assert_eq!(code, "invalid_manifest");
        assert!(message.contains("needs.requires.core"), "{message}");
    }

    #[test]
    fn target_names_match_the_manifest_contract_grammar() {
        // The profile grammar must not drift from the manifest's. Both sides
        // run on the same candidates: `parse_requires` through a real manifest
        // and `valid_contract_name` directly.
        let long = "a".repeat(CONTRACT_NAME_CAP + 1);
        let candidates = [
            "app-chat",
            "app1",
            "app--chat",
            "a",
            "app_chat",
            "App-Chat",
            "-app",
            "app-",
            "",
            long.as_str(),
        ];
        for name in candidates {
            let mut contracts = serde_yaml::Mapping::new();
            contracts.insert(
                Yaml::String(name.to_string()),
                Yaml::Sequence(vec![Yaml::Number(1_u64.into())]),
            );
            let mut requires = serde_yaml::Mapping::new();
            requires.insert(Yaml::String("contracts".into()), Yaml::Mapping(contracts));
            let mut needs = serde_yaml::Mapping::new();
            needs.insert(Yaml::String("requires".into()), Yaml::Mapping(requires));
            let mut front = serde_yaml::Mapping::new();
            front.insert(Yaml::String("needs".into()), Yaml::Mapping(needs));
            let manifest = format!("---\n{}---\n", serde_yaml::to_string(&front).unwrap());
            assert_eq!(
                manifest_preflight(&manifest).is_ok(),
                valid_contract_name(name),
                "contract name {name:?}"
            );
        }
    }
}
