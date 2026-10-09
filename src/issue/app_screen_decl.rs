//! CAD-500 R4: a pure declaration/integrity validator for the
//! `app-screens/v1` screen-package contract.
//!
//! [`validate_map`] checks one closed JSON declaration against a
//! caller-supplied map of UTF-8 asset bodies: exact
//! contract/app/entry grammar, flat asset names against a fixed
//! extension→media mapping, declared sha256/size against the actual
//! bytes, and finite bounds at every stage — raw manifest bytes and
//! bracket depth before parsing, node count after a bounded
//! `serde_json::Value` pass, then a closed typed decode that keeps
//! duplicate JSON fields refused.
//!
//! ## What this proves — and what it does not
//!
//! A returned [`IntegrityCheckedScreen`] proves only that the
//! declaration and the supplied bodies agree: every declared asset is
//! present, every supplied body is declared, and each body's UTF-8
//! byte length and sha256 match its declaration. The declared
//! provenance digests (source/SDK/toolchain) are checked for *syntax*
//! only — they are inert metadata, never recomputed against anything.
//!
//! Returning the checked value grants **no** approval, source-rebuild
//! attestation, mount, execution or effect authority. Asset bodies
//! stay caller-owned inert strings: this validator does not sanitize
//! JS/CSS/SVG/JSON for execution and does not claim code safety.
//! There is no actor, endpoint, filesystem, network, environment or
//! installed-path surface here — the function touches nothing but its
//! two arguments.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::model;

/// Largest raw manifest, bytes — checked before any parse.
const MAX_MANIFEST_BYTES: usize = 32_768;
/// Deepest `{}`/`[]` nesting — an iterative quote/escape-aware scan
/// refuses deeper manifests before serde sees them.
const MAX_DEPTH: usize = 32;
/// Most JSON nodes in the manifest — counted by an iterative walk over
/// the bounded `Value` tree.
const MAX_NODES: usize = 4_096;
/// Most assets, declared and supplied alike.
const MAX_ASSETS: usize = 32;
/// Largest single `.js` asset body — the screen entry/leaves get the
/// larger script budget (CAD-1254: the install gate's screen-asset cap).
const MAX_JS_BYTES: u64 = crate::issue::app::MAX_SCREEN_ASSET_BYTES;
/// Largest non-js asset body, UTF-8 bytes.
const MAX_ASSET_BYTES: u64 = 131_072;
/// Largest sum of every supplied asset body, bytes.
const MAX_TOTAL_BYTES: u64 = 524_288;

/// The per-leaf bound: `.js` gets the larger screen-script budget,
/// every other asset the flat cap. Keyed on the leaf name's extension.
fn asset_bound(name: &str) -> u64 {
    if name.rsplit_once('.').map(|(_, e)| e) == Some("js") {
        MAX_JS_BYTES
    } else {
        MAX_ASSET_BYTES
    }
}

/// The read-only contract — a mounted frame may only receive the
/// host-pushed projection (its `may` is empty).
const CONTRACT_V1: &str = "app-screens/v1";
/// CAD-1177: the action-capable contract. Identical integrity/asset
/// grammar to v1, plus an optional `host_contract` pin, a `may`
/// allowlist of bridge methods and a bounded `tools` alias→slot map.
/// A screen that does not declare them stays as read-only as v1.
const CONTRACT_V2: &str = "app-screens/v2";
/// The only host contract a v2 screen may pin to call tools.
pub const HOST_CONTRACT_ACTIONS: &str = "screen-actions.v1";
/// The only bridge method a v2 `may` may list today.
pub const MAY_TOOLS_INVOKE: &str = "tools.invoke";
/// The one entry point — pinned, and it must be a declared, supplied
/// asset.
const ENTRY: &str = "client.js";

/// CAD-1123 (operator decision Q1): the closed set of named remote image
/// origins a screen may opt into with `remote_images`. A declaration
/// names a set, never a host, so a package cannot widen the frame's
/// egress beyond what the host lists here. The browser loads these
/// directly into the frame; the daemon never fetches them.
const REMOTE_IMAGE_SETS: &[(&str, &[&str])] = &[(
    "instagram-cdn",
    &["https://*.cdninstagram.com", "https://*.fbcdn.net"],
)];

/// The CSP `img-src` sources one declared remote image set admits, or
/// `None` for a name the host does not list.
pub fn remote_image_sources(name: &str) -> Option<&'static [&'static str]> {
    REMOTE_IMAGE_SETS
        .iter()
        .find(|(set, _)| *set == name)
        .map(|(_, hosts)| *hosts)
}

/// A declaration that passed every check [`validate_map`] makes.
///
/// Carries small validated metadata only — never asset bodies and
/// never host paths. Fields are private, there is no `Deserialize`
/// and no unchecked constructor, so a value can only come out of the
/// validator. See the module docs: this value is integrity evidence,
/// not authority of any kind.
#[derive(Debug)]
pub struct IntegrityCheckedScreen {
    contract: String,
    app: String,
    entry: String,
    asset_count: usize,
    source_digest: String,
    sdk_digest: String,
    toolchain_digest: String,
    remote_images: Vec<String>,
    /// v2 only: the declared `may` methods (each currently only
    /// `tools.invoke`). Empty on v1 and on a v2 that declares none.
    may: Vec<String>,
    /// v2 only: declared logical tool alias → declared capability slot.
    /// The alias is app-owned vocabulary the frame calls; the slot is
    /// the `needs.capabilities` key it resolves through. Empty on v1.
    tools: BTreeMap<String, String>,
}

impl IntegrityCheckedScreen {
    /// The declared remote image sets — each a name in the host's closed
    /// list, distinct; empty unless the screen opted in.
    pub fn remote_images(&self) -> &[String] {
        &self.remote_images
    }
    /// The contract this declaration passed under (`app-screens/v1` or
    /// `app-screens/v2`).
    pub fn contract(&self) -> &str {
        &self.contract
    }
    /// The declared `may` bridge methods (v2 only; empty on v1).
    pub fn may(&self) -> &[String] {
        &self.may
    }
    /// The declared tool alias→capability-slot map (v2 only; empty on v1).
    pub fn tools(&self) -> &BTreeMap<String, String> {
        &self.tools
    }
    /// The declared app tag (`model::valid_tag` grammar).
    pub fn app(&self) -> &str {
        &self.app
    }
    /// The entry asset name — always `client.js` in v1.
    pub fn entry(&self) -> &str {
        &self.entry
    }
    /// How many assets the declaration lists (1..=32).
    pub fn asset_count(&self) -> usize {
        self.asset_count
    }
    /// Declared provenance digest — syntax checked, inert; no source
    /// rebuild is recomputed or implied.
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    /// Declared SDK digest — syntax checked, inert.
    pub fn sdk_digest(&self) -> &str {
        &self.sdk_digest
    }
    /// Declared toolchain digest — syntax checked, inert.
    pub fn toolchain_digest(&self) -> &str {
        &self.toolchain_digest
    }
}

/// The closed top-level `app-screens/v1` shape. `deny_unknown_fields`
/// refuses stray keys; serde's own duplicate-field error refuses
/// repeated ones. `may` is the optional method list — the contract
/// carries none, so only absence or `[]` passes the explicit check.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecl {
    contract: String,
    app: String,
    entry: String,
    assets: Vec<RawAsset>,
    provenance: RawProvenance,
    /// Optional; `null`, a non-array or any member refuses. v1 requires
    /// empty; v2 admits only the closed `MAY_TOOLS_INVOKE` verb.
    #[serde(default)]
    may: Vec<Value>,
    /// Optional named remote image sets (CAD-1123); each must be a host
    /// listed set, without repeats.
    #[serde(default)]
    remote_images: Vec<String>,
    /// v2 only: the host contract this screen was built against. Must be
    /// exactly `screen-actions.v1` when present (v2 with `may`/`tools`)
    /// — an unknown or missing contract keeps the screen read-only.
    #[serde(default)]
    host_contract: Option<String>,
    /// v2 only: logical alias → capability-slot declaration. Each alias
    /// is app-owned; each slot must name a `needs.capabilities` key the
    /// installed app declares (checked at invoke, never trusted here).
    #[serde(default)]
    tools: BTreeMap<String, String>,
}

/// One declared asset member — closed, all four fields required.
/// `size: u64` makes negative/float/string/null refuse through serde
/// typing alone.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsset {
    name: String,
    media_type: String,
    sha256: String,
    size: u64,
}

/// Declared provenance — required, closed, digest *syntax* only.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvenance {
    source_digest: String,
    sdk_digest: String,
    toolchain_digest: String,
}

/// Validate one `app-screens/v1` declaration against caller-supplied
/// UTF-8 asset bodies.
///
/// `manifest` is the raw JSON text (a separate argument — never an
/// asset-map member). `assets` maps flat asset names to bodies; it is
/// borrowed read-only and is identical on success and on refusal.
///
/// Check order is part of the contract: supplied count/size bounds,
/// raw manifest byte cap and bracket depth before any parse; a
/// bounded `Value` pass only to enforce the node budget; then the
/// closed typed decode of the *original* raw text (so duplicate JSON
/// fields still refuse — a `Value` map would hide them); then the
/// grammar and the declared↔supplied integrity match. Resource bounds
/// precede body hashing; integrity refusals can occur while comparing bodies.
pub fn validate_map(
    manifest: &str,
    assets: &BTreeMap<String, String>,
) -> Result<IntegrityCheckedScreen> {
    // Supplied-side bounds first — before anything copies or hashes a
    // byte. The map is already caller-owned; only lengths are read.
    if assets.len() > MAX_ASSETS {
        return Err(Error::rejected(format!(
            "{} supplied assets — at most {MAX_ASSETS}",
            assets.len()
        )));
    }
    let mut total_bytes = 0u64;
    for (name, body) in assets.iter() {
        let len = body.len() as u64;
        let bound = asset_bound(name);
        if len > bound {
            return Err(Error::rejected(format!(
                "asset {name:?} body is {len} bytes — at most {bound}"
            )));
        }
        total_bytes += len;
    }
    if total_bytes > MAX_TOTAL_BYTES {
        return Err(Error::rejected(format!(
            "asset bodies total {total_bytes} bytes — at most {MAX_TOTAL_BYTES}"
        )));
    }

    // Raw byte cap and bracket depth before serde touches the text.
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(Error::rejected(format!(
            "manifest is {} bytes — at most {MAX_MANIFEST_BYTES}",
            manifest.len()
        )));
    }
    check_depth(manifest)?;

    // The bounded `Value` parse exists only for the node budget — it
    // decides nothing about validity (malformed input still refuses
    // here) and never feeds semantics.
    let value: Value = serde_json::from_str(manifest)
        .map_err(|e| Error::rejected(format!("manifest is not valid JSON: {e}")))?;
    check_nodes(&value)?;

    // The closed typed decode reads the ORIGINAL raw text so duplicate
    // fields at every struct level refuse; a `Value` map would hide them.
    let decl: RawDecl = serde_json::from_str(manifest)
        .map_err(|e| Error::rejected(format!("manifest declaration: {e}")))?;

    check_declaration(&decl)?;
    check_assets(&decl, assets)?;

    let tools = decl
        .tools
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(IntegrityCheckedScreen {
        contract: decl.contract,
        asset_count: decl.assets.len(),
        app: decl.app,
        entry: decl.entry,
        source_digest: decl.provenance.source_digest,
        sdk_digest: decl.provenance.sdk_digest,
        toolchain_digest: decl.provenance.toolchain_digest,
        remote_images: decl.remote_images,
        may: decl
            .may
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        tools,
    })
}

/// Iterative, quote/escape-aware bracket scan: refuse `{}`/`[]`
/// nesting deeper than [`MAX_DEPTH`] before serde parses. String
/// contents — escaped quotes and literal brackets included — never
/// count as structure. Malformedness stays serde's job; this only
/// bounds the one thing serde's recursive descent must be shielded
/// from.
fn check_depth(raw: &str) -> Result<()> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    // `"`, `\` and the brackets are all single bytes <0x80, so a byte
    // walk cannot split a UTF-8 sequence.
    for &b in raw.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(Error::rejected(format!(
                        "manifest nesting exceeds depth {MAX_DEPTH}"
                    )));
                }
            }
            // A closer without an opener is malformed, not deep — serde
            // refuses it; the saturating_sub keeps the scan panic-free.
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Post-parse node budget: an iterative walk (heap stack, never
/// recursion) counting every `Value` — each scalar, each array
/// element, each object member's value, and each container once.
fn check_nodes(value: &Value) -> Result<()> {
    let mut nodes = 0usize;
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES {
            return Err(Error::rejected(format!(
                "manifest exceeds {MAX_NODES} JSON nodes"
            )));
        }
        match v {
            Value::Array(items) => {
                if nodes + stack.len() + items.len() > MAX_NODES {
                    return Err(Error::rejected("manifest exceeds JSON node budget"));
                }
                stack.extend(items.iter());
            }
            Value::Object(map) => {
                if nodes + stack.len() + map.len() > MAX_NODES {
                    return Err(Error::rejected("manifest exceeds JSON node budget"));
                }
                stack.extend(map.values());
            }
            _ => {}
        }
    }
    Ok(())
}

/// The closed grammar beyond serde's structural decode: exact
/// contract/app/entry, an effectively empty `may`, at most
/// [`MAX_ASSETS`] uniquely-named assets each carrying the pinned
/// `client.js` entry, name/media/digest grammar per member, and
/// provenance digest syntax.
/// A v2 logical alias grammar: `word` or dotted `word.word`, each word
/// lowercase-alnum starting with a letter (e.g. `instagram.read`,
/// `image.generate`). Bounded, never a provider/tool/account/route name.
fn valid_tool_alias(alias: &str) -> bool {
    if alias.is_empty() || alias.len() > 64 {
        return false;
    }
    alias.split('.').all(|word| {
        !word.is_empty()
            && word.len() <= 32
            && word.as_bytes()[0].is_ascii_lowercase()
            && word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

fn check_declaration(decl: &RawDecl) -> Result<()> {
    let is_v2 = match decl.contract.as_str() {
        c if c == CONTRACT_V1 => false,
        c if c == CONTRACT_V2 => true,
        other => {
            return Err(Error::rejected(format!(
                "contract {other:?} — the only accepted contracts are \
                 {CONTRACT_V1:?} and {CONTRACT_V2:?}",
            )))
        }
    };
    if !model::valid_tag(&decl.app) {
        return Err(Error::rejected(format!(
            "app {:?} — 1-32 lowercase letters, digits or hyphens, not starting with a hyphen",
            decl.app
        )));
    }
    if decl.entry != ENTRY {
        return Err(Error::rejected(format!(
            "entry {:?} — the only entry is {ENTRY:?}",
            decl.entry
        )));
    }
    // `may` is a string allowlist of bridge methods. v1 admits none;
    // v2 admits only the closed set, each a string, no repeats. A
    // non-string member or a method outside the set refuses (fail closed).
    let mut may_seen = HashSet::new();
    for member in &decl.may {
        let Some(name) = member.as_str() else {
            return Err(Error::rejected("`may` members are strings"));
        };
        if !is_v2 || name != MAY_TOOLS_INVOKE {
            return Err(Error::rejected(format!(
                "`may` method {name:?} is not a method this contract grants"
            )));
        }
        if !may_seen.insert(name) {
            return Err(Error::rejected(format!(
                "`may` method {name:?} is declared twice"
            )));
        }
    }
    // `host_contract`/`tools` are v2-only action declarations. Declaring
    // either under v1 refuses — a v1 screen stays read-only and cannot
    // smuggle a contract it was not reviewed against.
    if !is_v2 && (decl.host_contract.is_some() || !decl.tools.is_empty()) {
        return Err(Error::rejected(
            "host_contract and tools require contract app-screens/v2",
        ));
    }
    if let Some(host) = &decl.host_contract {
        if host != HOST_CONTRACT_ACTIONS {
            return Err(Error::rejected(format!(
                "host_contract {host:?} — the only served contract is {HOST_CONTRACT_ACTIONS:?}",
            )));
        }
    }
    // A screen that declares tools or a may-method must pin the host
    // contract it was built against; the pin alone grants nothing.
    if (!decl.tools.is_empty() || !decl.may.is_empty()) && decl.host_contract.is_none() {
        return Err(Error::rejected(
            "a screen declaring may/tools must pin host_contract",
        ));
    }
    // `tools` is a bounded alias→slot map (≤8, matching the app
    // capability-slot ceiling). Each alias is closed grammar; each slot
    // is a tag grammar capability key. The actual slot existence is
    // proven at invoke against the installed app — here we only prove
    // the declaration is well-formed and bounded.
    if decl.tools.len() > 8 {
        return Err(Error::rejected(format!(
            "{} declared tools — at most 8",
            decl.tools.len()
        )));
    }
    for (alias, slot) in &decl.tools {
        if !valid_tool_alias(alias) {
            return Err(Error::rejected(format!(
                "tool alias {alias:?} — lowercase words joined by dots"
            )));
        }
        if !model::valid_tag(slot) {
            return Err(Error::rejected(format!(
                "tool {alias:?} slot {slot:?} — must name a capability slot"
            )));
        }
    }
    let mut sets = HashSet::new();
    for name in &decl.remote_images {
        if remote_image_sources(name).is_none() {
            return Err(Error::rejected(format!(
                "remote_images {name:?} — not a host-listed image set"
            )));
        }
        if !sets.insert(name.as_str()) {
            return Err(Error::rejected(format!(
                "remote_images {name:?} is declared twice"
            )));
        }
    }
    if decl.assets.is_empty() {
        return Err(Error::rejected(format!(
            "at least the {ENTRY:?} entry asset must be declared"
        )));
    }
    if decl.assets.len() > MAX_ASSETS {
        return Err(Error::rejected(format!(
            "{} declared assets — at most {MAX_ASSETS}",
            decl.assets.len()
        )));
    }
    let mut names = HashSet::with_capacity(decl.assets.len());
    let mut entry_declared = false;
    for asset in &decl.assets {
        check_asset_name(&asset.name)?;
        check_media(&asset.name, &asset.media_type)?;
        check_digest(&asset.sha256, "asset sha256")?;
        let bound = asset_bound(&asset.name);
        if asset.size > bound {
            return Err(Error::rejected(format!(
                "asset {:?} declares {} bytes — at most {bound}",
                asset.name, asset.size
            )));
        }
        if !names.insert(asset.name.as_str()) {
            return Err(Error::rejected(format!(
                "asset {:?} is declared twice",
                asset.name
            )));
        }
        entry_declared |= asset.name == ENTRY;
    }
    if !entry_declared {
        return Err(Error::rejected(format!(
            "entry {ENTRY:?} must be one of the declared assets"
        )));
    }
    // Provenance is declared syntax — checked for shape, never
    // recomputed against a rebuild.
    check_digest(&decl.provenance.source_digest, "provenance.source_digest")?;
    check_digest(&decl.provenance.sdk_digest, "provenance.sdk_digest")?;
    check_digest(
        &decl.provenance.toolchain_digest,
        "provenance.toolchain_digest",
    )?;
    Ok(())
}

/// A flat leaf `<stem>.<ext>`: the stem is `model::valid_tag` (which
/// already forbids slash, backslash, percent, control, non-ASCII and
/// dot-containing stems) and the single final extension is one of the
/// mapped four.
fn check_asset_name(name: &str) -> Result<()> {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return Err(Error::rejected(format!(
            "asset name {name:?} — expected a flat <name>.<ext> leaf"
        )));
    };
    if !matches!(ext, "js" | "css" | "svg" | "json") {
        return Err(Error::rejected(format!(
            "asset name {name:?} — the only extensions are js, css, svg, json"
        )));
    }
    if !model::valid_tag(stem) {
        return Err(Error::rejected(format!(
            "asset name {name:?} — the stem must be a 1-32 char tag (lowercase, digits, hyphens)"
        )));
    }
    Ok(())
}

/// The fixed extension→media mapping; `client.js` therefore must be
/// JavaScript media.
fn check_media(name: &str, media_type: &str) -> Result<()> {
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let want = match ext {
        "js" => "text/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        _ => {
            return Err(Error::rejected(format!(
                "asset name {name:?} has no mapped media type"
            )))
        }
    };
    if media_type != want {
        return Err(Error::rejected(format!(
            "asset {name:?} declares media_type {media_type:?} — {want:?} is required"
        )));
    }
    Ok(())
}

/// `sha256:` plus exactly 64 lowercase hex digits — the only digest
/// syntax anywhere in the declaration.
fn check_digest(digest: &str, what: &str) -> Result<()> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(Error::rejected(format!(
            "{what} {digest:?} — expected sha256:<64 lowercase hex>"
        )));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::rejected(format!(
            "{what} {digest:?} — expected sha256:<64 lowercase hex>"
        )));
    }
    Ok(())
}

/// The declared↔supplied match: every declared asset exists in the
/// map with exactly the declared UTF-8 byte length and sha256, and
/// every supplied name is declared — no undeclared body passes.
fn check_assets(decl: &RawDecl, assets: &BTreeMap<String, String>) -> Result<()> {
    for asset in &decl.assets {
        let Some(body) = assets.get(&asset.name) else {
            return Err(Error::rejected(format!(
                "declared asset {:?} is not supplied",
                asset.name
            )));
        };
        if asset.size != body.len() as u64 {
            return Err(Error::rejected(format!(
                "asset {:?} declares {} bytes — the body is {}",
                asset.name,
                asset.size,
                body.len()
            )));
        }
        let actual = format!("sha256:{:x}", Sha256::digest(body.as_bytes()));
        if asset.sha256 != actual {
            return Err(Error::rejected(format!(
                "asset {:?} sha256 does not match its body",
                asset.name
            )));
        }
    }
    let declared: HashSet<&str> = decl.assets.iter().map(|a| a.name.as_str()).collect();
    for name in assets.keys() {
        if !declared.contains(name.as_str()) {
            return Err(Error::rejected(format!(
                "supplied asset {name:?} is not declared"
            )));
        }
    }
    Ok(())
}
