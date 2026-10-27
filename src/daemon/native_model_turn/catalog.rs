//! The operator-provisioned, versioned native model route profile.
//!
//! Only the root owner may provision this file; a caller, an environment
//! variable, an agent process or a request field can never redirect it. It
//! pins:
//!
//! - the exact Node executable and the immutable private SDK module graph
//!   (every file the child imports, by digest);
//! - the pinned published Devin provider entry and the installed Pi 1.4
//!   package root the child resolves against;
//! - one reviewed route per provider: the authoritative server origin(s) and
//!   the frozen catalog the native role/model/effort maps through;
//! - the finite owner-approved request/concurrency limits and the output cap.
//!
//! The catalog mapping is a faithful port of the published `pi-devin@0.2.1`
//! resolution (`familyToModels`/`resolveModelUid`): variant suffix → thinking
//! level map, preferred default, and the exact `model_uid` the provider would
//! send. There is no fallback catalog and no effort substitution.
//!
//! Absent profile = no qualified route. The broker loads the root-owned route
//! facts once for its lifetime and hands the validated SDK pins to the private
//! child, which re-verifies the module digests before importing.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::protected_runtime::strict;

/// The only path this profile is read from. Root-owned, never writable by
/// the group/other and never resolved through a symlink.
pub(crate) const PROFILE_PATH: &str = "/var/lib/cadence/runtime-owner/native-model.json";
const MAX_PROFILE_BYTES: usize = 256 * 1024;
const MAX_MANIFEST_BYTES: usize = 512 * 1024;
const MAX_MANIFEST_ENTRIES: usize = 4096;
const MAX_CATALOG_FAMILIES: usize = 1024;
const MAX_CATALOG_VARIANTS: usize = 4096;

fn refused(message: &str) -> Error {
    Error::invalid("route_unavailable", message)
}

/// The installed SDK's exact provider id for this route.
pub(crate) const PROVIDER: &str = "devin";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModulePin {
    pub path: String,
    pub digest: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SdkPins {
    pub node_path: String,
    pub node_digest: String,
    pub sdk_root: String,
    pub pi_devin_entry: String,
    pub pi_ai_root: String,
    /// The broker-owned child entry inside the pinned SDK root.
    pub child_entry: String,
    pub modules: Vec<ModulePin>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScopeLimit {
    pub requests_per_minute: u32,
    pub unresolved: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnerLimits {
    pub agent: ScopeLimit,
    pub account: ScopeLimit,
    pub workspace: ScopeLimit,
}

#[derive(Clone, Debug)]
pub(crate) struct VariantFacts {
    pub model_uid: String,
    pub max_context_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct FamilyFacts {
    pub family_uid: String,
    pub family_label: String,
    pub variants: Vec<VariantFacts>,
    /// `defaultUid` — the Pi model id this family registers.
    pub model_id: String,
    pub thinking_level_map: Vec<(String, String)>,
    pub reasoning: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RouteFacts {
    pub provider: String,
    pub origins: Vec<String>,
    pub default_origin: String,
    pub catalog_revision: u64,
    pub families: Vec<FamilyFacts>,
    pub max_output_tokens: u32,
    pub limits: OwnerLimits,
    pub sdk: SdkPins,
    /// The exact approved loaded tool declarations, provisioned by the
    /// operator from the immutable image material.
    pub approved_tools: Vec<super::dto::NativeToolV1>,
    /// The digest of the whole approved declaration set.
    pub approved_tools_digest: String,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeModelProfile {
    pub routes: Vec<RouteFacts>,
}

impl NativeModelProfile {
    pub(crate) fn route(&self, provider: &str) -> Option<&RouteFacts> {
        self.routes.iter().find(|route| route.provider == provider)
    }

    pub(crate) fn load_default() -> Result<Option<Self>> {
        Self::load_path(Path::new(PROFILE_PATH))
    }

    /// Absent is `Ok(None)` (no qualified route); a present but invalid or
    /// unsafe profile is an error — never a silent default.
    pub(crate) fn load_path(path: &Path) -> Result<Option<Self>> {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(refused("the native model profile is unreadable")),
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(refused(
                "the native model profile must be a regular file, not a symlink",
            ));
        }
        if !path.is_absolute() {
            return Err(refused("the native model profile path must be absolute"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                return Err(refused(
                    "the native model profile must be root-owned and not group/other writable",
                ));
            }
        }
        if meta.len() == 0 || meta.len() > MAX_PROFILE_BYTES as u64 {
            return Err(refused("the native model profile size is out of bounds"));
        }
        verify_root_owned_parents(path)?;
        let raw =
            std::fs::read(path).map_err(|_| refused("the native model profile is unreadable"))?;
        let value =
            strict::parse(&raw).map_err(|_| refused("the native model profile is malformed"))?;
        Self::parse(value).map(Some)
    }

    fn parse(value: Value) -> Result<Self> {
        let m = strict::object_with(&value, &["v", "profile_revision", "routes"], &[])?;
        if strict::safe_u64(m, "v")? != 1 {
            return Err(refused("the native model profile version is unsupported"));
        }
        decimal_u64(m, "profile_revision")?;
        let routes_value = m.get("routes").ok_or_else(|| refused("no route"))?;
        let routes_map = routes_value
            .as_object()
            .ok_or_else(|| refused("the native model profile routes are malformed"))?;
        if routes_map.len() != 1 {
            return Err(refused(
                "the native model profile must declare exactly one qualified route",
            ));
        }
        let mut routes = Vec::with_capacity(routes_map.len());
        for (provider, route) in routes_map {
            if provider != PROVIDER {
                return Err(refused(
                    "the native model profile declares an unqualified provider",
                ));
            }
            routes.push(RouteFacts::parse(provider.clone(), route)?);
        }
        Ok(Self { routes })
    }
}

impl RouteFacts {
    fn parse(provider: String, value: &Value) -> Result<Self> {
        let m = strict::object_with(
            value,
            &[
                "origins",
                "default_origin",
                "catalog_revision",
                "catalog",
                "max_output_tokens",
                "limits",
                "sdk",
                "approved_tools_path",
                "approved_tools_digest",
            ],
            &[],
        )?;
        let origins_value = m.get("origins").ok_or_else(|| refused("no origin"))?;
        let origins_items = origins_value
            .as_array()
            .ok_or_else(|| refused("the native model origins are malformed"))?;
        if origins_items.is_empty() || origins_items.len() > 8 {
            return Err(refused("the native model origin set is out of bounds"));
        }
        let mut origins = Vec::with_capacity(origins_items.len());
        for item in origins_items {
            let text = item
                .as_str()
                .ok_or_else(|| refused("the native model origin is malformed"))?;
            origins.push(canonical_origin(text)?);
        }
        let default_origin = canonical_origin(
            m.get("default_origin")
                .and_then(Value::as_str)
                .ok_or_else(|| refused("the native model default origin is missing"))?,
        )?;
        if !origins.iter().any(|origin| origin == &default_origin) {
            return Err(refused("the native model default origin is not allowed"));
        }
        let max_output_tokens = strict::positive_u64(m, "max_output_tokens")?;
        if max_output_tokens > dto_cap() as u64 {
            return Err(refused("the native model output cap is out of bounds"));
        }
        let catalog_revision = decimal_u64(m, "catalog_revision")?;
        let families = parse_catalog(m.get("catalog").ok_or_else(|| refused("no catalog"))?)?;
        let limits = parse_limits(m.get("limits").ok_or_else(|| refused("no limits"))?)?;
        let sdk = SdkPins::parse(m.get("sdk").ok_or_else(|| refused("no sdk pins"))?)?;
        let tools_path = absolute(
            strict::string(m, "approved_tools_path")?,
            "approved_tools_path",
        )?;
        let tools_digest = strict::string(m, "approved_tools_digest")?.to_string();
        strict::digest(&tools_digest)?;
        let (approved_tools, approved_tools_digest) =
            Self::load_approved_tools(&tools_path, &tools_digest)?;
        Ok(Self {
            provider,
            origins,
            default_origin,
            catalog_revision,
            families,
            max_output_tokens: max_output_tokens as u32,
            limits,
            sdk,
            approved_tools,
            approved_tools_digest,
        })
    }

    fn load_approved_tools(
        path: &str,
        expected: &str,
    ) -> Result<(Vec<super::dto::NativeToolV1>, String)> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|_| refused("the approved tool declarations are unreadable"))?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(refused(
                "the approved tool declarations must be a regular file",
            ));
        }
        if meta.len() == 0 || meta.len() > MAX_MANIFEST_BYTES as u64 {
            return Err(refused(
                "the approved tool declaration size is out of bounds",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                return Err(refused(
                    "the approved tool declarations must be root-owned and not writable",
                ));
            }
        }
        let raw = std::fs::read(path)
            .map_err(|_| refused("the approved tool declarations are unreadable"))?;
        if cadence_core::connection_dispatch::exact_byte_digest(&raw) != expected {
            return Err(refused(
                "the approved tool declaration digest does not match",
            ));
        }
        let value = strict::parse(&raw)
            .map_err(|_| refused("the approved tool declarations are malformed"))?;
        let m = strict::object_with(&value, &["v", "tools"], &[])?;
        if strict::safe_u64(m, "v")? != 1 {
            return Err(refused(
                "the approved tool declaration version is unsupported",
            ));
        }
        let items = m
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| refused("the approved tool declarations are malformed"))?;
        if items.len() > super::dto::MAX_ACTIVE_TOOLS {
            return Err(refused("too many approved tool declarations"));
        }
        let mut tools = Vec::with_capacity(items.len());
        let mut names = std::collections::BTreeSet::new();
        for item in items {
            let tool = super::dto::NativeToolV1::parse(item)
                .map_err(|_| refused("an approved tool declaration is malformed"))?;
            if !names.insert(tool.name.clone()) {
                return Err(refused("an approved tool name is declared twice"));
            }
            tools.push(tool);
        }
        let mut canonical_items = Vec::with_capacity(tools.len());
        for tool in &tools {
            canonical_items.push(tool.to_json());
        }
        let digest = cadence_core::connection_dispatch::material_digest_for_domain(
            "cadence-native-model-tools-v1",
            &Value::Array(canonical_items),
        );
        Ok((tools, digest))
    }

    /// Validate that `origin` is exactly one of the pinned route origins.
    pub(crate) fn require_origin(&self, origin: &str) -> Result<String> {
        let origin = canonical_origin(origin)?;
        if !self.origins.iter().any(|allowed| allowed == &origin) {
            return Err(refused("the model origin is not approved"));
        }
        Ok(origin)
    }

    /// Resolve the exact provider `model_uid` for one configured
    /// role/model/effort, preserving the configured provider and origin.
    pub(crate) fn resolve(
        &self,
        model: &str,
        effort: Option<&str>,
        source_origin: Option<&str>,
    ) -> Result<ResolvedRoute> {
        let (provider, model_id) = split_model(model);
        if !provider.is_empty() && provider != self.provider {
            return Err(refused(
                "the configured model belongs to a different provider",
            ));
        }
        let family = self
            .families
            .iter()
            .find(|family| family.model_id == model_id)
            .ok_or_else(|| refused("the configured model is not in the frozen catalog"))?;
        let origin = match source_origin {
            Some(origin) => self.require_origin(origin)?,
            None => self.default_origin.clone(),
        };
        let thinking_level_map: Value = {
            let mut map = serde_json::Map::new();
            for (level, uid) in &family.thinking_level_map {
                map.insert(level.clone(), json!(uid));
            }
            Value::Object(map)
        };
        let reasoning = family.reasoning && !family.thinking_level_map.is_empty();
        let (model_uid, effort_name) = match (reasoning, effort) {
            (true, Some(effort)) => {
                let uid = family
                    .thinking_level_map
                    .iter()
                    .find(|(level, _)| level.as_str() == effort)
                    .map(|(_, uid)| uid.clone())
                    .ok_or_else(|| {
                        refused("the configured effort is not supported by this model")
                    })?;
                (uid, effort.to_string())
            }
            (true, None) => {
                let (level, uid) = PREFERRED
                    .iter()
                    .find_map(|level| {
                        family
                            .thinking_level_map
                            .iter()
                            .find(|(key, _)| key.as_str() == *level)
                            .map(|(key, uid)| (key.clone(), uid.clone()))
                    })
                    .ok_or_else(|| refused("the configured model has no supported effort"))?;
                (uid, level)
            }
            (false, None) => (model_id.to_string(), "off".to_string()),
            (false, Some("off")) => (model_id.to_string(), "off".to_string()),
            (false, Some(_)) => {
                return Err(refused(
                    "the configured effort is not supported by this model",
                ))
            }
        };
        let variant = family
            .variants
            .iter()
            .find(|variant| variant.model_uid == model_uid)
            .ok_or_else(|| refused("the resolved model uid is not in the frozen catalog"))?;
        let mut max_output_tokens = self.max_output_tokens;
        if let Some(limit) = variant.max_output_tokens {
            if limit > 0 && limit < max_output_tokens as u64 {
                max_output_tokens = limit as u32;
            }
        }
        Ok(ResolvedRoute {
            provider: self.provider.clone(),
            origin,
            model_id: model_id.to_string(),
            model_uid,
            reasoning,
            effort: effort_name,
            thinking_level_map,
            max_output_tokens,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedRoute {
    pub provider: String,
    pub origin: String,
    pub model_id: String,
    pub model_uid: String,
    pub reasoning: bool,
    pub effort: String,
    pub thinking_level_map: Value,
    pub max_output_tokens: u32,
}

fn dto_cap() -> u32 {
    super::dto::MAX_OUTPUT_TOKENS_CAP
}

fn split_model(model: &str) -> (&str, &str) {
    match model.split_once('/') {
        Some((provider, id)) => (provider, id),
        None => ("", model),
    }
}

fn canonical_origin(value: &str) -> Result<String> {
    let trimmed = value.trim_end_matches('/');
    if trimmed.is_empty() || trimmed.len() > 200 {
        return Err(refused("the native model origin is malformed"));
    }
    let rest = trimmed
        .strip_prefix("https://")
        .ok_or_else(|| refused("the native model origin must be https"))?;
    if rest.is_empty()
        || rest.contains('/')
        || rest.contains('?')
        || rest.contains('#')
        || rest.contains('@')
        || rest.contains(' ')
    {
        return Err(refused("the native model origin is malformed"));
    }
    Ok(trimmed.to_string())
}

/// Canonical positive decimal string, bounded by `i64::MAX`.
fn decimal_u64(m: &serde_json::Map<String, Value>, key: &str) -> Result<u64> {
    let text = m
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| refused("the native model revision is missing"))?;
    if text.is_empty()
        || text.len() > 19
        || (text.as_bytes()[0] == b'0' && text.len() != 1)
        || !text.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(refused("the native model revision is malformed"));
    }
    let parsed: u64 = text
        .parse()
        .map_err(|_| refused("the native model revision is malformed"))?;
    if parsed == 0 || parsed > i64::MAX as u64 {
        return Err(refused("the native model revision is out of range"));
    }
    Ok(parsed)
}

fn parse_limits(value: &Value) -> Result<OwnerLimits> {
    let m = strict::object(value, &["agent", "account", "workspace"])?;
    Ok(OwnerLimits {
        agent: parse_scope_limit(m.get("agent").ok_or_else(|| refused("no agent limit"))?)?,
        account: parse_scope_limit(
            m.get("account")
                .ok_or_else(|| refused("no account limit"))?,
        )?,
        workspace: parse_scope_limit(
            m.get("workspace")
                .ok_or_else(|| refused("no workspace limit"))?,
        )?,
    })
}

fn parse_scope_limit(value: &Value) -> Result<ScopeLimit> {
    let m = strict::object(value, &["requests_per_minute", "unresolved"])?;
    let requests = strict::positive_u64(m, "requests_per_minute")?;
    let unresolved = strict::positive_u64(m, "unresolved")?;
    if requests > 10_000 || unresolved > 1_000 {
        return Err(refused("the native model limits are out of bounds"));
    }
    Ok(ScopeLimit {
        requests_per_minute: requests as u32,
        unresolved: unresolved as u32,
    })
}

fn parse_catalog(value: &Value) -> Result<Vec<FamilyFacts>> {
    let m = strict::object_with(value, &["v", "families"], &[])?;
    if strict::safe_u64(m, "v")? != 1 {
        return Err(refused("the native model catalog version is unsupported"));
    }
    let families = m
        .get("families")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("the native model catalog is malformed"))?;
    if families.is_empty() || families.len() > MAX_CATALOG_FAMILIES {
        return Err(refused(
            "the native model catalog family count is out of bounds",
        ));
    }
    let mut parsed = Vec::with_capacity(families.len());
    for family in families {
        let m = strict::object_with(family, &["family", "variants"], &["family_uid"])?;
        let family_label = strict::string(m, "family")?.to_string();
        let family_uid = match m.get("family_uid") {
            None | Some(Value::Null) => String::new(),
            Some(value) => value
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 200)
                .ok_or_else(|| refused("the native model family uid is malformed"))?
                .to_string(),
        };
        let variants_value = m
            .get("variants")
            .and_then(Value::as_array)
            .ok_or_else(|| refused("the native model variants are malformed"))?;
        if variants_value.is_empty() || variants_value.len() > MAX_CATALOG_VARIANTS {
            return Err(refused("the native model variant count is out of bounds"));
        }
        let mut variants = Vec::with_capacity(variants_value.len());
        for variant in variants_value {
            let m = strict::object_with(
                variant,
                &["model_uid"],
                &[
                    "max_context_tokens",
                    "max_output_tokens",
                    "cost_tier",
                    "label",
                ],
            )?;
            let model_uid = strict::string(m, "model_uid")?.to_string();
            if model_uid.is_empty()
                || model_uid.len() > 200
                || model_uid.bytes().any(|b| b.is_ascii_control())
            {
                return Err(refused("the native model variant uid is malformed"));
            }
            variants.push(VariantFacts {
                model_uid,
                max_context_tokens: optional_positive(m, "max_context_tokens")?,
                max_output_tokens: optional_positive(m, "max_output_tokens")?,
            });
        }
        let (model_id, thinking_level_map, reasoning) = family_models(&family_uid, &variants);
        parsed.push(FamilyFacts {
            family_uid,
            family_label,
            variants,
            model_id,
            thinking_level_map,
            reasoning,
        });
    }
    Ok(parsed)
}

fn optional_positive(m: &serde_json::Map<String, Value>, key: &str) -> Result<Option<u64>> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let n = value
                .as_u64()
                .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
                .ok_or_else(|| refused("the native model catalog number is malformed"))?;
            Ok(Some(n))
        }
    }
}

// The following port of `pi-devin@0.2.1` `models.ts` is deliberate: the
// provider's numeric mapping is reproduced exactly so the broker can name
// the `model_uid` the wire will carry and the plugin never picks a fallback.

fn variant_key(uid: &str) -> Option<&'static str> {
    const SUFFIXES: &[&str] = &[
        "none-priority",
        "low-priority",
        "medium-priority",
        "high-priority",
        "xhigh-priority",
        "max-priority",
        "low-fast",
        "medium-fast",
        "high-fast",
        "xhigh-fast",
        "max-fast",
        "thinking-1m",
        "thinking",
        "none",
        "low",
        "medium",
        "high",
        "xhigh",
        "max",
        "minimal",
    ];
    for suffix in SUFFIXES {
        if uid == *suffix {
            return Some(suffix);
        }
        if uid.len() > suffix.len() + 1 {
            let start = uid.len() - suffix.len() - 1;
            if uid.as_bytes()[start] == b'-' && &uid[start + 1..] == *suffix {
                return Some(suffix);
            }
        }
    }
    None
}

fn thinking_from_suffix(suffix: Option<&str>) -> Option<&'static str> {
    let suffix = match suffix {
        None => return Some("high"),
        Some(suffix) => suffix,
    };
    match suffix {
        "none" | "none-priority" => Some("off"),
        "minimal" => Some("minimal"),
        _ => {
            if suffix.starts_with("low") {
                Some("low")
            } else if suffix.starts_with("medium") {
                Some("medium")
            } else if suffix.starts_with("high") && !suffix.starts_with("xhigh") {
                Some("high")
            } else if suffix.starts_with("xhigh") {
                Some("xhigh")
            } else if suffix.starts_with("max") {
                Some("max")
            } else if suffix.contains("thinking") {
                Some("high")
            } else {
                None
            }
        }
    }
}

const THINKING_ORDER: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const PREFERRED: &[&str] = &["high", "medium", "max", "xhigh", "low", "minimal", "off"];

fn family_models(
    family_uid: &str,
    variants: &[VariantFacts],
) -> (String, Vec<(String, String)>, bool) {
    let usable: Vec<&VariantFacts> = variants
        .iter()
        .filter(|variant| match variant_key(&variant.model_uid) {
            None => true,
            Some(key) => !key.contains("priority") && !key.contains("fast") && key != "thinking-1m",
        })
        .collect();
    let source: Vec<&VariantFacts> = if usable.is_empty() {
        variants.iter().collect()
    } else {
        usable
    };
    let mut map: Vec<(String, String)> = Vec::new();
    for variant in &source {
        if let Some(level) = thinking_from_suffix(variant_key(&variant.model_uid)) {
            if !map.iter().any(|(key, _)| key.as_str() == level) {
                map.push((level.to_string(), variant.model_uid.clone()));
            }
        }
    }
    let default_uid = PREFERRED
        .iter()
        .find_map(|level| {
            map.iter()
                .find(|(key, _)| key.as_str() == *level)
                .map(|(_, uid)| uid.clone())
        })
        .or_else(|| source.first().map(|variant| variant.model_uid.clone()))
        .unwrap_or_else(|| family_uid.to_string());
    let mapped_levels = THINKING_ORDER
        .iter()
        .filter(|level| map.iter().any(|(key, _)| key.as_str() == **level))
        .count();
    let ordered = THINKING_ORDER
        .iter()
        .filter_map(|level| {
            map.iter()
                .find(|(key, _)| key.as_str() == *level)
                .map(|(_, uid)| (level.to_string(), uid.clone()))
        })
        .collect();
    let _ = mapped_levels;
    (default_uid, ordered, map.len() > 1)
}

impl SdkPins {
    fn parse(value: &Value) -> Result<Self> {
        let m = strict::object(
            value,
            &[
                "node_path",
                "node_digest",
                "sdk_root",
                "manifest_path",
                "manifest_digest",
                "pi_devin_entry",
                "pi_ai_root",
            ],
        )?;
        let node_path = absolute(strict::string(m, "node_path")?, "node_path")?;
        let node_digest = digest(strict::string(m, "node_digest")?)?;
        let sdk_root = absolute(strict::string(m, "sdk_root")?, "sdk_root")?
            .trim_end_matches('/')
            .to_string();
        let manifest_path = absolute(strict::string(m, "manifest_path")?, "manifest_path")?;
        let manifest_digest = digest(strict::string(m, "manifest_digest")?)?;
        let pi_devin_entry = absolute(strict::string(m, "pi_devin_entry")?, "pi_devin_entry")?;
        let root_meta = std::fs::symlink_metadata(&sdk_root)
            .map_err(|_| refused("the native model SDK root is unreadable"))?;
        if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
            return Err(refused("the native model SDK root must be a directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if root_meta.uid() != 0 || root_meta.mode() & 0o022 != 0 {
                return Err(refused(
                    "the native model SDK root must be root-owned and not writable",
                ));
            }
        }
        let pi_ai_root = absolute(strict::string(m, "pi_ai_root")?, "pi_ai_root")?;
        if !path_within(&sdk_root, &node_path)
            && node_path != "/usr/bin/node"
            && node_path != "/usr/local/bin/node"
            && !node_path.starts_with("/opt/")
        {
            return Err(refused("the pinned node path is outside the operator tree"));
        }
        if !path_within(&sdk_root, &pi_devin_entry) || !path_within(&sdk_root, &pi_ai_root) {
            return Err(refused("the pinned provider graph is outside the SDK root"));
        }
        let modules = Self::load_manifest(&manifest_path, &manifest_digest, &sdk_root)?;
        let child_entry = format!("{sdk_root}/child.mjs");
        if !path_within(&sdk_root, &child_entry)
            || !modules.iter().any(|module| module.path == child_entry)
        {
            return Err(refused(
                "the broker-owned SDK child entry is not pinned in the manifest",
            ));
        }
        Ok(Self {
            node_path,
            node_digest,
            sdk_root,
            pi_devin_entry,
            pi_ai_root,
            child_entry,
            modules,
        })
    }

    fn load_manifest(path: &str, expected: &str, sdk_root: &str) -> Result<Vec<ModulePin>> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|_| refused("the native model SDK manifest is unreadable"))?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(refused(
                "the native model SDK manifest must be a regular file",
            ));
        }
        if meta.len() == 0 || meta.len() > MAX_MANIFEST_BYTES as u64 {
            return Err(refused(
                "the native model SDK manifest size is out of bounds",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                return Err(refused(
                    "the native model SDK manifest must be root-owned and not writable",
                ));
            }
        }
        let raw = std::fs::read(path)
            .map_err(|_| refused("the native model SDK manifest is unreadable"))?;
        if cadence_core::connection_dispatch::exact_byte_digest(&raw) != expected {
            return Err(refused(
                "the native model SDK manifest digest does not match",
            ));
        }
        let value = strict::parse(&raw)
            .map_err(|_| refused("the native model SDK manifest is malformed"))?;
        let m = strict::object_with(&value, &["v", "modules"], &[])?;
        if strict::safe_u64(m, "v")? != 1 {
            return Err(refused(
                "the native model SDK manifest version is unsupported",
            ));
        }
        let items = m
            .get("modules")
            .and_then(Value::as_array)
            .ok_or_else(|| refused("the native model SDK modules are malformed"))?;
        if items.is_empty() || items.len() > MAX_MANIFEST_ENTRIES {
            return Err(refused(
                "the native model SDK module count is out of bounds",
            ));
        }
        let mut modules = Vec::with_capacity(items.len());
        for item in items {
            let m = strict::object(item, &["path", "digest"])?;
            let path = absolute(strict::string(m, "path")?, "module path")?;
            if !path_within(sdk_root, &path) {
                return Err(refused("an SDK module path escapes the SDK root"));
            }
            modules.push(ModulePin {
                path,
                digest: digest(strict::string(m, "digest")?)?,
            });
        }
        Ok(modules)
    }

    pub(crate) fn to_plan_json(&self) -> Value {
        json!({
            "node_path": self.node_path,
            "node_digest": self.node_digest,
            "sdk_root": self.sdk_root,
            "pi_devin_entry": self.pi_devin_entry,
            "pi_ai_root": self.pi_ai_root,
            "modules": self.modules.iter().map(|m| json!({"path": m.path, "digest": m.digest})).collect::<Vec<_>>(),
        })
    }
}

fn digest(value: &str) -> Result<String> {
    strict::digest(value)?;
    Ok(value.to_string())
}

fn absolute(value: &str, what: &str) -> Result<String> {
    if value.len() > 512 || !value.starts_with('/') || value.contains('\0') {
        return Err(refused(&format!("the native model {what} is not absolute")));
    }
    let path = PathBuf::from(value);
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(refused(&format!(
            "the native model {what} is not normalized"
        )));
    }
    Ok(value.to_string())
}

fn path_within(root: &str, value: &str) -> bool {
    let root = Path::new(root);
    let value = Path::new(value);
    value.starts_with(root) && value != root
}

fn verify_root_owned_parents(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut current = path.parent();
        while let Some(parent) = current {
            let meta = std::fs::symlink_metadata(parent)
                .map_err(|_| refused("the native model profile parents are unreadable"))?;
            if meta.is_symlink() || !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                return Err(refused(
                    "the native model profile must live under root-owned non-writable parents",
                ));
            }
            current = parent.parent();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
