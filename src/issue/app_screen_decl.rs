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

/// The one contract string this validator accepts.
const CONTRACT: &str = "app-screens/v1";
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
    app: String,
    entry: String,
    asset_count: usize,
    source_digest: String,
    sdk_digest: String,
    toolchain_digest: String,
    remote_images: Vec<String>,
}

impl IntegrityCheckedScreen {
    /// The declared remote image sets — each a name in the host's closed
    /// list, distinct; empty unless the screen opted in.
    pub fn remote_images(&self) -> &[String] {
        &self.remote_images
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
    /// Optional; `null`, a non-array or any member refuses.
    #[serde(default)]
    may: Vec<Value>,
    /// Optional named remote image sets (CAD-1123); each must be a host
    /// listed set, without repeats.
    #[serde(default)]
    remote_images: Vec<String>,
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

    Ok(IntegrityCheckedScreen {
        asset_count: decl.assets.len(),
        app: decl.app,
        entry: decl.entry,
        source_digest: decl.provenance.source_digest,
        sdk_digest: decl.provenance.sdk_digest,
        toolchain_digest: decl.provenance.toolchain_digest,
        remote_images: decl.remote_images,
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
fn check_declaration(decl: &RawDecl) -> Result<()> {
    if decl.contract != CONTRACT {
        return Err(Error::rejected(format!(
            "contract {:?} — the only accepted contract is {CONTRACT:?}",
            decl.contract
        )));
    }
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
    if !decl.may.is_empty() {
        return Err(Error::rejected(
            "`may` declares no methods — only an empty array (or its absence) is accepted",
        ));
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
