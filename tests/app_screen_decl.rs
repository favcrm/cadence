//! CAD-500: pure app-screen declaration/integrity validator contract.
//!
//! `validate_map` checks one closed `app-screens/v1` JSON declaration against a
//! caller-supplied UTF-8 asset map: exact contract/app/entry grammar, closed
//! serde structs at every level, declared asset name/media/size/sha256 against
//! actual bodies, and finite bounds (manifest <=32768 raw bytes, bracket depth
//! <=32, <=4096 nodes, <=32 assets, <=131072 bytes each, <=524288 aggregate).
//! Provenance digests are declared syntax only — no rebuild or approval is
//! proved. Pure library surface: no actor, endpoint, filesystem or network.
//!
//! Byte boundaries use valid declarations padded with JSON whitespace. Depth
//! and node fixtures cannot fit the closed schema, so their specific diagnostic
//! establishes that the budget check refused them before schema decoding.

use cadence_agent::issue::app_screen_decl::validate_map;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const CLIENT: &str = "console.log('crm screen');\n";
const STYLES: &str = "body { margin: 0; font-family: sans-serif; }\n";
const ICON: &str =
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"><rect width=\"1\" height=\"1\"/></svg>\n";
const SEED: &str = "{\"rows\": []}\n";

fn sha256(body: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
}

fn asset(name: &str, media_type: &str, body: &str) -> Value {
    json!({
        "name": name,
        "media_type": media_type,
        "sha256": sha256(body),
        "size": body.len(),
    })
}

fn provenance() -> Value {
    json!({
        "source_digest": sha256("crm screen source tree"),
        "sdk_digest": sha256("cadence app sdk 1.0.0"),
        "toolchain_digest": sha256("rustc 1.84.0 x86_64-unknown-linux-gnu"),
    })
}

/// The standard valid declaration: client.js plus three more declared assets,
/// every hash and byte size computed from the real fixture bodies.
fn declaration() -> Value {
    json!({
        "contract": "app-screens/v1",
        "app": "crm",
        "entry": "client.js",
        "assets": [
            asset("client.js", "text/javascript", CLIENT),
            asset("styles.css", "text/css", STYLES),
            asset("icon.svg", "image/svg+xml", ICON),
            asset("seed.json", "application/json", SEED),
        ],
        "provenance": provenance(),
        "may": [],
    })
}

fn assets() -> BTreeMap<String, String> {
    [
        ("client.js", CLIENT),
        ("styles.css", STYLES),
        ("icon.svg", ICON),
        ("seed.json", SEED),
    ]
    .into_iter()
    .map(|(name, body)| (name.to_string(), body.to_string()))
    .collect()
}

fn client_only() -> BTreeMap<String, String> {
    BTreeMap::from([("client.js".to_string(), CLIENT.to_string())])
}

fn check(manifest: &Value, assets: &BTreeMap<String, String>) -> bool {
    validate_map(&manifest.to_string(), assets).is_ok()
}

/// `[ [{} ...] ]` whose bracket nesting reaches `depth` (a well-formed JSON
/// fragment; enclosing objects add their own levels).
fn nested_body(depth: usize) -> String {
    format!("{}{{}}{}", "[".repeat(depth), "]".repeat(depth))
}

#[test]
fn cad500_valid_declaration_with_computed_hashes_passes_and_exposes_readers() {
    let checked = validate_map(&declaration().to_string(), &assets()).unwrap();
    assert_eq!(checked.app(), "crm");
    assert_eq!(checked.entry(), "client.js");
    assert_eq!(checked.asset_count(), 4);
    // Provenance is declared syntax, returned read-only; no rebuild is claimed.
    assert_eq!(checked.source_digest(), sha256("crm screen source tree"));
    assert_eq!(checked.sdk_digest(), sha256("cadence app sdk 1.0.0"));
    assert_eq!(
        checked.toolchain_digest(),
        sha256("rustc 1.84.0 x86_64-unknown-linux-gnu")
    );
    let debug = format!("{checked:?}");
    assert!(debug.contains("IntegrityCheckedScreen"));
}

#[test]
fn cad500_minimal_client_only_set_passes_and_may_is_optional() {
    let mut decl = json!({
        "contract": "app-screens/v1",
        "app": "crm",
        "entry": "client.js",
        "assets": [asset("client.js", "text/javascript", CLIENT)],
        "provenance": provenance(),
    });
    // Absent `may` behaves as an empty method list.
    let checked = validate_map(&decl.to_string(), &client_only()).unwrap();
    assert_eq!(checked.asset_count(), 1);
    // An explicit empty array is equally valid.
    decl["may"] = json!([]);
    let checked = validate_map(&decl.to_string(), &client_only()).unwrap();
    assert_eq!(checked.asset_count(), 1);
}

#[test]
fn cad500_caller_asset_map_is_unchanged_on_success_and_refusal() {
    let manifest = declaration().to_string();
    let map = assets();
    let before = map.clone();
    validate_map(&manifest, &map).unwrap();
    assert_eq!(map, before, "success must not mutate the caller map");

    let mut bad = declaration();
    bad["assets"][0]["sha256"] = json!(sha256("forged body"));
    assert!(validate_map(&bad.to_string(), &map).is_err());
    assert_eq!(map, before, "refusal must not mutate the caller map");
}

#[test]
fn cad500_non_object_and_malformed_manifests_refuse() {
    for raw in [
        "",
        "{ \"contract\": ",
        "[1,2,3]",
        "\"app-screens/v1\"",
        "42",
        "null",
    ] {
        assert!(
            validate_map(raw, &assets()).is_err(),
            "{raw:?} refuses before any field logic"
        );
    }
}

#[test]
fn cad500_top_level_is_closed_and_required_fields_must_be_present_and_typed() {
    let mut extra = declaration();
    extra["runtime"] = json!("iframe");
    assert!(!check(&extra, &assets()), "unknown top-level key refuses");

    for field in ["contract", "app", "entry", "assets", "provenance"] {
        let mut missing = declaration();
        missing.as_object_mut().unwrap().remove(field);
        assert!(!check(&missing, &assets()), "missing {field} refuses");
        let mut nulled = declaration();
        nulled[field] = json!(null);
        assert!(!check(&nulled, &assets()), "null {field} refuses");
    }
    for (field, wrong) in [
        ("contract", json!(1)),
        ("app", json!(42)),
        ("entry", json!(["client.js"])),
        ("assets", json!({"name": "client.js"})),
        ("provenance", json!("declared")),
    ] {
        let mut bad = declaration();
        bad[field] = wrong;
        assert!(!check(&bad, &assets()), "wrong-typed {field} refuses");
    }
}

#[test]
fn cad500_nested_asset_and_provenance_objects_are_closed_and_required() {
    for key in ["name", "media_type", "sha256", "size"] {
        let mut decl = declaration();
        decl["assets"][0].as_object_mut().unwrap().remove(key);
        assert!(!check(&decl, &assets()), "asset missing {key} refuses");
        let mut decl = declaration();
        decl["assets"][0][key] = json!(null);
        assert!(!check(&decl, &assets()), "asset null {key} refuses");
    }
    let mut decl = declaration();
    decl["assets"][0]["etag"] = json!("W/1");
    assert!(!check(&decl, &assets()), "unknown asset key refuses");

    for key in ["source_digest", "sdk_digest", "toolchain_digest"] {
        let mut decl = declaration();
        decl["provenance"].as_object_mut().unwrap().remove(key);
        assert!(!check(&decl, &assets()), "provenance missing {key} refuses");
        let mut decl = declaration();
        decl["provenance"][key] = json!(null);
        assert!(!check(&decl, &assets()), "provenance null {key} refuses");
    }
    let mut decl = declaration();
    decl["provenance"]["builder"] = json!("ci");
    assert!(!check(&decl, &assets()), "unknown provenance key refuses");
}

#[test]
fn cad500_duplicate_json_fields_refuse_at_every_struct_level() {
    let base = declaration().to_string();
    // Top level: repeat a required field and the optional one.
    let dup_contract = base.replacen(
        "\"contract\":\"app-screens/v1\"",
        "\"contract\":\"app-screens/v1\",\"contract\":\"app-screens/v1\"",
        1,
    );
    assert!(validate_map(&dup_contract, &assets()).is_err());
    assert!(validate_map(
        &base.replace("\"may\":[]", "\"may\":[],\"may\":[]"),
        &assets()
    )
    .is_err());
    // Asset member level (client.js is the only JavaScript entry, so its
    // media_type key is unique in the serialized text).
    let dup_media = base.replace(
        "\"media_type\":\"text/javascript\"",
        "\"media_type\":\"text/javascript\",\"media_type\":\"text/javascript\"",
    );
    assert!(validate_map(&dup_media, &assets()).is_err());
    // Provenance level.
    let dup_sdk = base.replace(
        "\"sdk_digest\":",
        "\"sdk_digest\":\"sha256:0000000000000000000000000000000000000000000000000000000000000000\",\"sdk_digest\":",
    );
    assert!(validate_map(&dup_sdk, &assets()).is_err());
}

#[test]
fn cad500_contract_app_entry_and_may_grammar_is_exact() {
    for contract in ["app-screens/v0", "app-screens/v2", "APP-SCREENS/V1", ""] {
        let mut decl = declaration();
        decl["contract"] = json!(contract);
        assert!(!check(&decl, &assets()), "contract {contract:?} refuses");
    }
    for app in [
        "",
        "CRM",
        "crm app",
        "crm_app",
        "-crm",
        "café",
        "a]b",
        &"a".repeat(33),
    ] {
        let mut decl = declaration();
        decl["app"] = json!(app);
        assert!(!check(&decl, &assets()), "app {app:?} refuses");
    }
    for app in ["crm2", "c", &"a".repeat(32)] {
        let mut decl = declaration();
        decl["app"] = json!(app);
        assert!(check(&decl, &assets()), "app {app:?} is a valid tag");
    }
    for entry in [
        "Client.js",
        "client.JS",
        "main.js",
        "dir/client.js",
        "",
        "client",
    ] {
        let mut decl = declaration();
        decl["entry"] = json!(entry);
        assert!(!check(&decl, &assets()), "entry {entry:?} refuses");
    }
    for may in [
        json!(null),
        json!(["net"]),
        json!("net"),
        json!({}),
        json!([""]),
    ] {
        let mut decl = declaration();
        decl["may"] = may.clone();
        assert!(!check(&decl, &assets()), "may {may} refuses");
    }
}

#[test]
fn cad500_asset_names_are_flat_tag_stems_with_one_mapped_extension() {
    for (name, media) in [
        ("dir/a.js", "text/javascript"),
        ("a\\b.js", "text/javascript"),
        ("a%20b.js", "text/javascript"),
        ("a.js.js", "text/javascript"),
        ("a.min.js", "text/javascript"),
        (".hidden.js", "text/javascript"),
        ("-lead.js", "text/javascript"),
        ("Upper.js", "text/javascript"),
        ("café.js", "text/javascript"),
        ("a\u{0}b.js", "text/javascript"),
        ("toolongname0123456789012345678901.js", "text/javascript"),
        ("a.png", "image/png"),
        ("a.txt", "text/plain"),
        ("a.html", "text/html"),
        ("noext", "text/plain"),
        ("a.", "text/plain"),
    ] {
        let mut decl = declaration();
        decl["assets"].as_array_mut().unwrap().pop();
        let mut map = assets();
        map.remove("seed.json");
        let body = "x";
        decl["assets"]
            .as_array_mut()
            .unwrap()
            .push(asset(name, media, body));
        map.insert(name.to_string(), body.to_string());
        assert!(
            validate_map(&decl.to_string(), &map).is_err(),
            "name {name:?} refuses"
        );
    }
    // Boundary stems still pass: 1-char and 32-char tags, digits and hyphen.
    let long_stem = format!("{}.json", "a".repeat(32));
    for (name, media) in [
        ("a.js", "text/javascript"),
        ("z9-0.css", "text/css"),
        (long_stem.as_str(), "application/json"),
    ] {
        let body = "x";
        let mut decl = declaration();
        decl["assets"].as_array_mut().unwrap().pop();
        let mut map = assets();
        map.remove("seed.json");
        decl["assets"]
            .as_array_mut()
            .unwrap()
            .push(asset(name, media, body));
        map.insert(name.to_string(), body.to_string());
        assert!(
            validate_map(&decl.to_string(), &map).is_ok(),
            "name {name:?} is a valid flat leaf"
        );
    }
}

#[test]
fn cad500_media_type_must_match_the_fixed_extension_mapping() {
    let mut decl = declaration();
    decl["assets"][0]["media_type"] = json!("application/javascript");
    assert!(
        !check(&decl, &assets()),
        "client.js must be JavaScript media"
    );

    for (index, wrong) in [
        (1, "application/json"), // styles.css must be text/css
        (2, "image/png"),        // icon.svg must be image/svg+xml
        (3, "text/json"),        // seed.json must be application/json
    ] {
        let mut decl = declaration();
        decl["assets"][index]["media_type"] = json!(wrong);
        assert!(
            !check(&decl, &assets()),
            "{wrong} refuses for that extension"
        );
    }
}

#[test]
fn cad500_sha256_and_size_are_checked_against_actual_utf8_bytes() {
    for hash in [
        sha256("forged body"),                // real digest, wrong body
        sha256(CLIENT).to_uppercase(),        // uppercase hex refuses
        "a".repeat(64),                       // bare hex without the sha256: prefix
        "sha256:abc".into(),                  // too short
        format!("{}0", sha256(CLIENT)),       // 65 hex digits
        format!("sha256:{}", "g".repeat(64)), // non-hex
        format!("sha1:{}", "a".repeat(40)),   // wrong algorithm label
    ] {
        let mut decl = declaration();
        decl["assets"][0]["sha256"] = json!(hash);
        assert!(!check(&decl, &assets()), "sha256 {hash:?} refuses");
    }
    for size in [
        json!(CLIENT.len() - 1),
        json!(CLIENT.len() + 1),
        json!(-1),
        json!(4.5),
        json!("29"),
        json!(null),
    ] {
        let mut decl = declaration();
        decl["assets"][0]["size"] = size.clone();
        assert!(!check(&decl, &assets()), "size {size} refuses");
    }
    // Unicode counts bytes, not characters: "é日" is 2 chars and 5 UTF-8 bytes.
    let unicode = "é日".to_string();
    let mut decl = declaration();
    decl["assets"].as_array_mut().unwrap().pop();
    decl["assets"]
        .as_array_mut()
        .unwrap()
        .push(asset("data.json", "application/json", &unicode));
    let mut map = assets();
    map.remove("seed.json");
    map.insert("data.json".into(), unicode.clone());
    for size in [unicode.len(), unicode.chars().count()] {
        decl["assets"][3]["size"] = json!(size);
        let ok = validate_map(&decl.to_string(), &map).is_ok();
        assert_eq!(ok, size == unicode.len(), "size {size} vs byte length");
    }
}

#[test]
fn cad500_provenance_digests_are_declared_syntax_only() {
    for key in ["source_digest", "sdk_digest", "toolchain_digest"] {
        for digest in [
            sha256("x").to_uppercase(),           // uppercase hex
            "a".repeat(64),                       // missing prefix
            "sha256:abc".into(),                  // short
            format!("sha256:{}", "g".repeat(64)), // non-hex
            format!("sha256:{}", "0".repeat(65)), // 65 digits
        ] {
            let mut decl = declaration();
            decl["provenance"][key] = json!(digest);
            assert!(!check(&decl, &assets()), "{key} {digest:?} refuses");
        }
        let mut decl = declaration();
        decl["provenance"][key] = json!(null);
        assert!(!check(&decl, &assets()), "{key} null refuses");
    }
    // Any well-formed digest is accepted as inert declared metadata — it is
    // not recomputed against anything in this slice.
    let mut decl = declaration();
    decl["provenance"]["source_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    assert!(check(&decl, &assets()));
}

#[test]
fn cad500_declared_and_supplied_asset_sets_must_match_exactly() {
    // Duplicate declared names refuse even though the map holds one body.
    let mut dup = declaration();
    let repeated = dup["assets"][3].clone();
    dup["assets"].as_array_mut().unwrap().push(repeated);
    assert!(!check(&dup, &assets()));

    // Supplied-but-undeclared file refuses.
    let mut extra_map = assets();
    extra_map.insert("extra.js".into(), "x".into());
    assert!(validate_map(&declaration().to_string(), &extra_map).is_err());

    // Declared-but-missing file refuses.
    let mut short_map = assets();
    short_map.remove("icon.svg");
    assert!(validate_map(&declaration().to_string(), &short_map).is_err());

    // Same count with matching membership passes; re-adding the dropped file
    // on top of the otherwise-matching set still refuses.
    let mut swapped = declaration();
    swapped["assets"].as_array_mut().unwrap().pop();
    swapped["assets"]
        .as_array_mut()
        .unwrap()
        .push(asset("other.json", "application/json", "x"));
    let mut map = assets();
    map.remove("seed.json");
    map.insert("other.json".into(), "x".into());
    assert!(
        validate_map(&swapped.to_string(), &map).is_ok(),
        "matching membership passes"
    );
    map.insert("seed.json".into(), SEED.into());
    assert!(validate_map(&swapped.to_string(), &map).is_err());
}

#[test]
fn cad500_entry_must_be_declared_and_is_pinned_to_client_js() {
    let mut decl = declaration();
    decl["entry"] = json!("styles.css");
    assert!(!check(&decl, &assets()), "entry is pinned to client.js");

    let mut decl = declaration();
    decl["assets"]
        .as_array_mut()
        .unwrap()
        .retain(|a| a["name"] != "client.js");
    let mut map = assets();
    map.remove("client.js");
    assert!(
        validate_map(&decl.to_string(), &map).is_err(),
        "entry must remain a declared, supplied asset"
    );
}

#[test]
fn cad500_declared_and_supplied_counts_cap_at_32() {
    // 32 total assets is the exact boundary and passes.
    let mut decl = declaration();
    decl["assets"] = json!([asset("client.js", "text/javascript", CLIENT)]);
    let mut map = client_only();
    for i in 0..31usize {
        let name = format!("a-{i:02}.json");
        map.insert(name.clone(), "x".into());
        decl["assets"]
            .as_array_mut()
            .unwrap()
            .push(asset(&name, "application/json", "x"));
    }
    assert_eq!(map.len(), 32);
    assert!(
        validate_map(&decl.to_string(), &map).is_ok(),
        "32 assets pass"
    );

    // 33 declared assets refuse even though the map mirrors them exactly.
    let name = "a-31.json";
    map.insert(name.into(), "x".into());
    decl["assets"]
        .as_array_mut()
        .unwrap()
        .push(asset(name, "application/json", "x"));
    assert_eq!(map.len(), 33);
    assert!(
        validate_map(&decl.to_string(), &map).is_err(),
        "33 declared assets refuse"
    );

    // 33 supplied assets against 32 declarations refuse on the map side too.
    let mut decl = declaration();
    decl["assets"] = json!([asset("client.js", "text/javascript", CLIENT)]);
    let mut map = client_only();
    for i in 0..32usize {
        let name = format!("a-{i:02}.json");
        map.insert(name.clone(), "x".into());
        if i < 31 {
            decl["assets"]
                .as_array_mut()
                .unwrap()
                .push(asset(&name, "application/json", "x"));
        }
    }
    assert_eq!(map.len(), 33);
    assert!(validate_map(&decl.to_string(), &map).is_err());
}

#[test]
fn cad500_asset_and_aggregate_byte_caps_bind_exactly() {
    // One asset at the 131072-byte boundary passes; one over refuses.
    let boundary = "x".repeat(131_072);
    let mut decl = declaration();
    decl["assets"] = json!([
        asset("client.js", "text/javascript", CLIENT),
        asset("big.json", "application/json", &boundary),
    ]);
    let mut map = client_only();
    map.insert("big.json".into(), boundary);
    assert!(
        validate_map(&decl.to_string(), &map).is_ok(),
        "131072 passes"
    );

    let over = "x".repeat(131_073);
    let mut decl = declaration();
    decl["assets"] = json!([
        asset("client.js", "text/javascript", CLIENT),
        asset("big.json", "application/json", &over),
    ]);
    let mut map = client_only();
    map.insert("big.json".into(), over);
    assert!(
        validate_map(&decl.to_string(), &map).is_err(),
        "131073 refuses"
    );

    // Aggregate boundary: client.js (27B) + three 131065B + one 131066B asset
    // lands exactly on 524288; every piece is under the per-asset cap, so only
    // the aggregate decides.
    let sizes = [131_065usize, 131_065, 131_065, 131_066];
    assert_eq!(sizes.iter().sum::<usize>() + CLIENT.len(), 524_288);
    let mut decl = declaration();
    decl["assets"] = json!([asset("client.js", "text/javascript", CLIENT)]);
    let mut map = client_only();
    for (i, size) in sizes.iter().enumerate() {
        let name = format!("b-{i}.json");
        let body = "x".repeat(*size);
        map.insert(name.clone(), body.clone());
        decl["assets"]
            .as_array_mut()
            .unwrap()
            .push(asset(&name, "application/json", &body));
    }
    assert!(
        validate_map(&decl.to_string(), &map).is_ok(),
        "524288 passes"
    );

    // One byte over the aggregate refuses while staying under the per-asset
    // cap, so the refusal must come from the total.
    let grown = "x".repeat(131_067);
    map.insert("b-3.json".into(), grown.clone());
    decl["assets"][4] = asset("b-3.json", "application/json", &grown);
    assert!(
        validate_map(&decl.to_string(), &map).is_err(),
        "524289 refuses"
    );

    // A declared size at the per-asset cap refuses even though the
    // actual body is small — the declaration is checked, not just the bytes.
    let mut decl = declaration();
    decl["assets"][1]["size"] = json!(131_072);
    assert!(!check(&decl, &assets()));
}

#[test]
fn cad500_manifest_raw_length_is_bounded_before_parsing() {
    let base = declaration().to_string();
    let boundary = format!("{base}{}", " ".repeat(32_768 - base.len()));
    assert_eq!(boundary.len(), 32_768);
    assert!(
        validate_map(&boundary, &assets()).is_ok(),
        "32768 raw bytes pass"
    );
    let over = format!("{boundary} ");
    assert_eq!(over.len(), 32_769);
    assert!(
        validate_map(&over, &assets()).is_err(),
        "32769 raw bytes refuse"
    );
}

#[test]
fn cad500_bracket_depth_over_32_refuses() {
    for array_depth in [31, 38] {
        // Enclosing object + array_depth arrays + innermost object.
        let manifest = format!("{{\"d\":{}}}", nested_body(array_depth));
        serde_json::from_str::<Value>(&manifest).unwrap();
        let error = validate_map(&manifest, &assets()).unwrap_err().to_string();
        assert!(
            error.contains("depth"),
            "expected depth diagnostic: {error}"
        );
    }
    // A valid declaration can encode characters through JSON Unicode escapes.
    // A scanner that treats escaped quote characters as closing quotes would
    // miscount this schema-invalid string; it must reach schema decoding instead.
    let mut decl = declaration();
    decl["app"] = json!(format!("{}{}", "\\\"".repeat(40), "[".repeat(40)));
    let manifest = decl.to_string();
    serde_json::from_str::<Value>(&manifest).unwrap();
    let error = validate_map(&manifest, &assets()).unwrap_err().to_string();
    assert!(
        !error.contains("depth"),
        "string contents are not structural depth: {error}"
    );
    let escaped_valid = declaration()
        .to_string()
        .replace("\"crm\"", "\"\\u0063rm\"");
    assert!(validate_map(&escaped_valid, &assets()).is_ok());
}

#[test]
fn cad500_node_count_over_4096_refuses_under_raw_and_depth_limits() {
    let manifest = format!(
        "{{\"contract\":\"app-screens/v1\",\"d\":[{}]}}",
        "0,".repeat(4_999) + "0"
    );
    assert!(manifest.len() < 32_768);
    serde_json::from_str::<Value>(&manifest).unwrap();
    let error = validate_map(&manifest, &assets()).unwrap_err().to_string();
    assert!(error.contains("node"), "expected node diagnostic: {error}");
    assert!(validate_map(&declaration().to_string(), &assets()).is_ok());
}

#[test]
fn cad500_declaration_metadata_is_inert_and_grants_no_authority() {
    // A traversal-shaped entry is refused by the grammar, not interpreted.
    let mut decl = declaration();
    decl["entry"] = json!("../../etc/passwd");
    assert!(!check(&decl, &assets()));

    // Asset bodies are opaque checked bytes: a hostile-looking script matches
    // its declared hash and passes — this validator proves integrity only and
    // does not claim to sanitize code for execution.
    let body = "fetch('https://evil.example'); /* inert text */";
    let mut decl = declaration();
    decl["assets"] = json!([
        asset("client.js", "text/javascript", body),
        asset("styles.css", "text/css", STYLES),
    ]);
    let map = BTreeMap::from([
        ("client.js".to_string(), body.to_string()),
        ("styles.css".to_string(), STYLES.to_string()),
    ]);
    let checked = validate_map(&decl.to_string(), &map).unwrap();
    assert_eq!(checked.asset_count(), 2);
}
