//! ADR 0006 §5.6 — the fake platform behind the shared
//! connected-platform fixture in `contracts/connected-platform/v1/`.
//!
//! Test support, not a live adapter: it simulates the platform side
//! the proxy and effect gate (CAD-366/CAD-506) talk to, deterministically
//! and with no network and no credentials. Its declared tool table is the
//! fixture's own `fake-tool-table.json`, embedded at compile time, so the
//! tested table is the published contract byte-for-byte.
//!
//! What it models:
//! - the declared tool table `{tool → {effect, scopes, label?}}` (C2) and
//!   the classification rules of C1–C3, including the manifest-version pin;
//! - `execute`: the platform call. Sends append to an observable execution
//!   log ("did it fire?"); every write carries an idempotency key derived
//!   from the `effect_id` and a repeated key returns the recorded outcome
//!   without executing again (C9);
//! - `read_back`: the post-execution state comparison that produces
//!   `outcome.verified` (C10), with knobs for mismatch and for platforms
//!   that offer no read-back;
//! - reviewed source artifacts a send derives from (`source_hash`, §5.4
//!   step 3) — the test mutates them to trigger `source_changed`;
//! - injected platform faults (`failed` outcomes).
//!
//! What it deliberately does not model: custody, grants, the gate's
//! staging/durability and the press itself — those are CAD-366/506's
//! side of the seam.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Repo-relative directory the shared fixture lives at. Both Cadence's
/// tests and AgenticOS consumers pin this path at a version (v1/).
pub const FIXTURE_DIR: &str = "contracts/connected-platform/v1";

/// The fake adapter's declared tool table — the fixture file itself.
pub const TOOL_TABLE_JSON: &str =
    include_str!("../contracts/connected-platform/v1/fake-tool-table.json");

/// The effect vocabulary (C1): exactly read | draft | send.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    Read,
    Draft,
    Send,
}

impl Effect {
    /// Classify a declared `effect` value. C3: a missing field, a
    /// malformed value or one outside the vocabulary — and a tool absent
    /// from the table entirely — all classify as `Send`. Omission fails
    /// into the gate.
    pub fn classify(declared: Option<&str>) -> Self {
        match declared {
            Some("read") => Self::Read,
            Some("draft") => Self::Draft,
            _ => Self::Send,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Draft => "draft",
            Self::Send => "send",
        }
    }
}

/// One tool-table entry. `effect` is kept raw (`Option<String>`): a real
/// adapter's reviewed table can carry a missing or out-of-vocabulary
/// value, and the malformed-table vectors depend on the fake loading it
/// without choking.
#[derive(Debug, Clone)]
pub struct ToolDecl {
    pub tool: String,
    pub effect: Option<String>,
    pub scopes: Vec<String>,
    pub label: Option<String>,
}

/// An adapter's declared tool table (§5.2) — `{tool, effect, scopes,
/// label}` entries pinned to the platform `manifest_version` they were
/// reviewed against.
#[derive(Debug, Clone)]
pub struct ToolTable {
    pub platform: String,
    /// The pinned manifest version; `None` means the table declares none.
    pub manifest_version: Option<String>,
    pub tools: Vec<ToolDecl>,
}

impl ToolTable {
    /// Lenient parse: shape errors are `Error::invalid`, but per-entry
    /// defects (a missing or unknown `effect`, a missing version) are
    /// kept as data so the malformed-table vectors can express them.
    pub fn from_json(v: &Value) -> Result<Self> {
        let map = v
            .as_object()
            .ok_or_else(|| Error::invalid("tool_table", "tool table must be a JSON object"))?;
        let platform = map
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let manifest_version = map
            .get("manifest_version")
            .and_then(Value::as_str)
            .map(str::to_string);
        let tools = map
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::invalid("tool_table", "tool table needs a tools array"))?
            .iter()
            .map(|t| {
                Ok(ToolDecl {
                    tool: t
                        .get("tool")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::invalid("tool_table", "tool entry needs a name"))?
                        .to_string(),
                    effect: t.get("effect").and_then(Value::as_str).map(str::to_string),
                    scopes: t
                        .get("scopes")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    label: t.get("label").and_then(Value::as_str).map(str::to_string),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            platform,
            manifest_version,
            tools,
        })
    }

    /// The table the shipped fake declares — the fixture file itself.
    pub fn standard() -> Self {
        Self::from_json(&serde_json::from_str(TOOL_TABLE_JSON).expect("fixture parses"))
            .expect("fixture tool table parses")
    }

    pub fn declared(&self, tool: &str) -> Option<&ToolDecl> {
        self.tools.iter().find(|t| t.tool == tool)
    }

    /// The declared effect of `tool`, classified per C1–C3.
    pub fn effect_of(&self, tool: &str) -> Effect {
        Effect::classify(self.declared(tool).and_then(|d| d.effect.as_deref()))
    }

    /// §5.2's manifest pin: calls are legal only while the platform
    /// reports exactly the version the table was reviewed against.
    /// Undeclared on either side is a mismatch.
    pub fn manifest_matches(&self, reported: Option<&str>) -> bool {
        self.manifest_version.is_some() && self.manifest_version.as_deref() == reported
    }
}

/// The gate's routing of a call, per C1–C3 and the manifest pin — the
/// rule CAD-506's gate must implement, expressed once here so the
/// fixture's vectors can be checked for internal consistency. A
/// manifest mismatch or an undeclared/unknown/absent effect is `Send`;
/// everything else is the declared class.
pub fn classify_call(
    table: &ToolTable,
    reported_manifest_version: Option<&str>,
    tool: &str,
) -> Effect {
    if !table.manifest_matches(reported_manifest_version) {
        return Effect::Send;
    }
    table.effect_of(tool)
}

/// `outcome.verified` (C10): the adapter's read-back verdict. `Unknown`
/// is a legitimate steady state where the platform offers no read-back —
/// it is not `false`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verified {
    True,
    False,
    Unknown,
}

impl Serialize for Verified {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::True => s.serialize_bool(true),
            Self::False => s.serialize_bool(false),
            Self::Unknown => s.serialize_str("unknown"),
        }
    }
}

impl<'de> Deserialize<'de> for Verified {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        match v {
            Value::Bool(true) => Ok(Self::True),
            Value::Bool(false) => Ok(Self::False),
            Value::String(ref s) if s == "unknown" => Ok(Self::Unknown),
            other => Err(serde::de::Error::custom(format!(
                "verified must be true, false or \"unknown\", got {other}"
            ))),
        }
    }
}

/// What the fake platform did: one attempted platform call. `ok:false`
/// rows are attempts that errored — traffic happened, the act did not
/// land.
#[derive(Debug, Clone)]
pub struct Execution {
    pub seq: u64,
    pub tool: String,
    pub input: Value,
    /// The idempotency key derived from `effect_id` (C9).
    pub idempotency_key: String,
    /// The expected content hash where the platform supports one (C9).
    pub expected_hash: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
}

/// Fake-adapter read-back knob — `given.adapter.read_back` in the
/// vectors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadBack {
    /// Compare recorded platform state against the approved input.
    Verify,
    /// Simulate divergence between approved input and platform state
    /// (outcome.verified=false → Needs-you).
    Mismatch,
    /// The platform offers no read-back (verified="unknown").
    Unknown,
}

/// The deterministic platform-side double. Every knob a vector's
/// `given.adapter` names lands here. Named `FakePlatform` — the platform
/// end of the seam — to stay clear of `adapter::fake::FakeAdapter`, the
/// provider/session double.
pub struct FakePlatform {
    table: ToolTable,
    /// The manifest version the "platform" currently reports — the
    /// platform side of the pin, never a call argument.
    reported_manifest_version: Mutex<Option<String>>,
    /// Reviewed source artifacts (name → bytes). `source_hash` hashes
    /// them; a send's `input.source` names the one it derives from.
    sources: Mutex<HashMap<String, Vec<u8>>>,
    /// Every attempted platform call, in order — the observable
    /// "did it fire / did it fire twice" surface.
    executions: Mutex<Vec<Execution>>,
    /// idempotency_key → the outcome already recorded for it (C9).
    outcomes: Mutex<HashMap<String, std::result::Result<Value, String>>>,
    /// tool → injected platform error.
    fails: Mutex<HashMap<String, String>>,
    /// What a successful call applied, for read-back comparison:
    /// tool → input.
    applied: Mutex<HashMap<String, Value>>,
    read_back: Mutex<ReadBack>,
    seq: AtomicU64,
}

impl FakePlatform {
    /// The fixture's own table (`fake-tool-table.json`) with the platform
    /// reporting the table's pinned manifest version.
    pub fn standard() -> Self {
        Self::new(ToolTable::standard())
    }

    pub fn new(table: ToolTable) -> Self {
        let reported = table.manifest_version.clone();
        Self {
            table,
            reported_manifest_version: Mutex::new(reported),
            sources: Mutex::new(HashMap::new()),
            executions: Mutex::new(Vec::new()),
            outcomes: Mutex::new(HashMap::new()),
            fails: Mutex::new(HashMap::new()),
            applied: Mutex::new(HashMap::new()),
            read_back: Mutex::new(ReadBack::Verify),
            seq: AtomicU64::new(0),
        }
    }

    pub fn table(&self) -> &ToolTable {
        &self.table
    }

    /// What the platform reports now — `None` models an undeclared
    /// manifest version (gates as send).
    pub fn reported_manifest_version(&self) -> Option<String> {
        self.reported_manifest_version.lock().unwrap().clone()
    }
    pub fn set_reported_manifest_version(&self, v: Option<String>) {
        *self.reported_manifest_version.lock().unwrap() = v;
    }

    /// Inject a platform fault: calls to `tool` error with `error`.
    pub fn fail_tool(&self, tool: &str, error: &str) {
        self.fails
            .lock()
            .unwrap()
            .insert(tool.to_string(), error.to_string());
    }

    pub fn set_read_back(&self, mode: ReadBack) {
        *self.read_back.lock().unwrap() = mode;
    }

    /// Create or replace a reviewed source artifact — the "edit" a
    /// vector's `edit_source` step performs.
    pub fn write_source(&self, source: &str, content: &str) {
        self.sources
            .lock()
            .unwrap()
            .insert(source.to_string(), content.as_bytes().to_vec());
    }

    /// `sha256:<hex>` of a source artifact — the value a staged row pins
    /// as `source_hash` and Execute re-verifies before firing.
    pub fn source_hash(&self, source: &str) -> Option<String> {
        self.sources.lock().unwrap().get(source).map(|bytes| {
            let digest: String = Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("sha256:{digest}")
        })
    }

    /// The rendered preview the press reviews (§5.4) — deterministic,
    /// bounded, in the platform's terms.
    pub fn preview(&self, tool: &str, input: &Value) -> String {
        let mut text = format!("{tool} on {}: {input}", self.table.platform);
        const PREVIEW_CAP: usize = 512;
        if text.len() > PREVIEW_CAP {
            text.truncate(PREVIEW_CAP);
            text.push('…');
        }
        text
    }

    /// Perform a call against the platform. Reads, drafts and (on accept)
    /// sends all come through here; what the gate allows through is the
    /// gate's business. Idempotent on `idempotency_key` (C9): a repeated
    /// key returns the recorded outcome without a second execution.
    pub fn execute(
        &self,
        tool: &str,
        input: &Value,
        idempotency_key: &str,
        expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        if let Some(outcome) = self.outcomes.lock().unwrap().get(idempotency_key) {
            return outcome.clone();
        }
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let injected = self.fails.lock().unwrap().get(tool).cloned();
        let (ok, error, result) = match injected {
            Some(error) => (false, Some(error.clone()), Err(error)),
            None => {
                self.applied
                    .lock()
                    .unwrap()
                    .insert(tool.to_string(), input.clone());
                (
                    true,
                    None,
                    Ok(json!({
                        "tool": tool,
                        "applied": input,
                        "platform_ref": format!("fixture://{tool}#{seq}"),
                    })),
                )
            }
        };
        self.executions.lock().unwrap().push(Execution {
            seq,
            tool: tool.to_string(),
            input: input.clone(),
            idempotency_key: idempotency_key.to_string(),
            expected_hash: expected_hash.map(str::to_string),
            ok,
            error,
        });
        self.outcomes
            .lock()
            .unwrap()
            .insert(idempotency_key.to_string(), result.clone());
        result
    }

    /// §5.4 step 6 read-back: does recorded platform state match the
    /// approved input? Honors the `read_back` knob; `Verify` compares the
    /// last applied input for `tool` against `expected`.
    pub fn read_back(&self, tool: &str, expected: &Value) -> Verified {
        match *self.read_back.lock().unwrap() {
            ReadBack::Mismatch => Verified::False,
            ReadBack::Unknown => Verified::Unknown,
            ReadBack::Verify => match self.applied.lock().unwrap().get(tool) {
                Some(applied) if applied == expected => Verified::True,
                _ => Verified::False,
            },
        }
    }

    /// Every attempted platform call, in order.
    pub fn executions(&self) -> Vec<Execution> {
        self.executions.lock().unwrap().clone()
    }
    pub fn execution_count(&self) -> usize {
        self.executions.lock().unwrap().len()
    }
    /// Attempts against `tool` — including ones that errored.
    pub fn executions_of(&self, tool: &str) -> usize {
        self.executions
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.tool == tool)
            .count()
    }
}

/// The pending-effect record (§5.4) as a typed view — for tests that
/// build or inspect rows. The JSON Schema in the fixture is
/// authoritative; this mirrors it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEffect {
    pub request: String,
    /// Always `"effect"`.
    pub kind: String,
    pub agent: String,
    pub platform: String,
    pub account: String,
    pub tool: String,
    /// Always `"send"` — reads and drafts never pend.
    pub effect: String,
    pub input_summary: String,
    /// The exact staged call arguments the press approves.
    pub input: Value,
    /// The rendered bounded artifact the press reviews.
    pub preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub effect_id: String,
    /// waiting → decided → executing → done|failed; or declined, closed,
    /// reconcile (§5.4 `state` enum).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PendingDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<PendingOutcome>,
}

/// `decision` — `{by, at, reason?}`; `by` is `{member, role, rule}`, the
/// verified presser and the rule that authorised them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDecision {
    pub by: PendingPresser,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPresser {
    pub member: String,
    pub role: String,
    pub rule: String,
}

/// `outcome` — the platform result or error, plus the read-back verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub verified: Verified,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_table_loads_the_fixture_file() {
        let table = ToolTable::standard();
        assert_eq!(table.platform, "fixture");
        assert_eq!(table.manifest_version.as_deref(), Some("1"));
        assert_eq!(table.tools.len(), 3);
        assert_eq!(table.effect_of("widgets.list"), Effect::Read);
        assert_eq!(table.effect_of("widgets.preview"), Effect::Draft);
        assert_eq!(table.effect_of("widgets.publish"), Effect::Send);
        assert_eq!(
            table.declared("widgets.publish").unwrap().label.as_deref(),
            Some("deploy")
        );
    }

    #[test]
    fn classify_fails_into_send() {
        let table = ToolTable::standard();
        // Undeclared tool, missing effect, unknown value — all send (C3).
        assert_eq!(table.effect_of("widgets.exfiltrate"), Effect::Send);
        assert_eq!(Effect::classify(None), Effect::Send);
        assert_eq!(Effect::classify(Some("teleport")), Effect::Send);
        // Manifest mismatch or undeclared pins every call as send.
        assert_eq!(
            classify_call(&table, Some("2"), "widgets.list"),
            Effect::Send
        );
        assert_eq!(classify_call(&table, None, "widgets.list"), Effect::Send);
        assert_eq!(
            classify_call(&table, Some("1"), "widgets.list"),
            Effect::Read
        );
    }

    #[test]
    fn execute_is_idempotent_on_the_key() {
        let adapter = FakePlatform::standard();
        let input = json!({"widget": "w1"});
        let first = adapter.execute("widgets.publish", &input, "eff-1", None);
        let second = adapter.execute("widgets.publish", &input, "eff-1", None);
        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(adapter.execution_count(), 1);
        // A different effect_id is a different write.
        adapter
            .execute("widgets.publish", &input, "eff-2", None)
            .unwrap();
        assert_eq!(adapter.execution_count(), 2);
    }

    #[test]
    fn fault_and_read_back_knobs() {
        let adapter = FakePlatform::standard();
        adapter.fail_tool("widgets.publish", "platform rejected");
        let err = adapter
            .execute("widgets.publish", &json!({"widget": "w1"}), "eff-1", None)
            .unwrap_err();
        assert_eq!(err, "platform rejected");
        assert!(!adapter.executions()[0].ok);

        adapter.set_read_back(ReadBack::Unknown);
        assert_eq!(
            adapter.read_back("widgets.list", &json!({})),
            Verified::Unknown
        );
        adapter.set_read_back(ReadBack::Mismatch);
        assert_eq!(
            adapter.read_back("widgets.list", &json!({})),
            Verified::False
        );
    }

    #[test]
    fn read_back_compares_applied_state() {
        let adapter = FakePlatform::standard();
        adapter
            .execute("widgets.publish", &json!({"widget": "w1"}), "eff-1", None)
            .unwrap();
        assert_eq!(
            adapter.read_back("widgets.publish", &json!({"widget": "w1"})),
            Verified::True
        );
        assert_eq!(
            adapter.read_back("widgets.publish", &json!({"widget": "w2"})),
            Verified::False
        );
    }

    #[test]
    fn source_edits_change_the_hash() {
        let adapter = FakePlatform::standard();
        assert_eq!(adapter.source_hash("deploy-plan"), None);
        adapter.write_source("deploy-plan", "v1");
        let h1 = adapter.source_hash("deploy-plan").unwrap();
        assert!(h1.starts_with("sha256:"));
        adapter.write_source("deploy-plan", "v2");
        assert_ne!(adapter.source_hash("deploy-plan").unwrap(), h1);
    }
}
