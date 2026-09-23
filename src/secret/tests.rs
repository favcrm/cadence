//! Every token here is synthetic and is built at runtime from a seeded
//! generator plus a prefix. No literal in this file has a credential shape,
//! so gitleaks in CI has nothing to flag.

use super::*;

const ALPHANUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// `n` pseudo-random characters from `alphabet`, fixed by `seed`.
fn noise_from(seed: &str, n: usize, alphabet: &[u8]) -> String {
    let mut out = String::with_capacity(n);
    let mut counter = 0u32;
    while out.len() < n {
        let block = Sha256::digest(format!("{seed}:{counter}").as_bytes());
        for b in block {
            if out.len() == n {
                break;
            }
            out.push(alphabet[b as usize % alphabet.len()] as char);
        }
        counter += 1;
    }
    out
}

fn noise(seed: &str, n: usize) -> String {
    noise_from(seed, n, ALPHANUM)
}

/// A token: `prefix` joined to seeded noise.
fn token(prefix: &str, seed: &str, n: usize) -> String {
    [prefix, &noise(seed, n)].concat()
}

fn rules(findings: &[Finding]) -> Vec<&str> {
    findings.iter().map(|f| f.rule.as_str()).collect()
}

/// The bare-token families. Each sits alone on its own line, with no
/// keyword nearby.
fn bare_tokens() -> Vec<(&'static str, String)> {
    vec![
        ("cadence-figma-token", token("figd_", "figma", 40)),
        (
            "cadence-anthropic-key",
            token(&["sk", "-ant-", "api03-"].concat(), "anthropic", 93),
        ),
        (
            "cadence-openai-project-key",
            token(&["sk", "-proj-"].concat(), "openai", 48),
        ),
        (
            "cadence-github-fine-grained-pat",
            token(&["github", "_pat_"].concat(), "ghfg", 82),
        ),
        (
            "cadence-gitlab-pat",
            token(&["gl", "pat-"].concat(), "gitlab", 20),
        ),
        (
            "cadence-npm-token",
            token(&["np", "m_"].concat(), "npm", 36),
        ),
        (
            "cadence-devin-key",
            token(&["dv", "n_"].concat(), "devin", 32),
        ),
        (
            "cadence-google-api-key",
            token(&["AI", "za"].concat(), "gcp", 35),
        ),
    ]
}

#[test]
fn bare_tokens_on_their_own_line_block_with_the_named_rule() {
    for (rule, tok) in bare_tokens() {
        let text = format!("Summary of the change\n\n{tok}\n\nthanks\n");
        let found = scan(&text, None).unwrap();
        assert_eq!(rules(&found), vec![rule], "{rule}");
        let f = &found[0];
        assert_eq!((f.line, f.column), (3, 1), "{rule}");
        assert_eq!(f.severity, Severity::Block, "{rule}");
        // The redacted form is a short prefix. The value itself never
        // appears, whether the finding is read or serialized.
        assert!(f.redacted.ends_with('…'), "{rule}: {}", f.redacted);
        assert!(f.redacted.chars().count() <= 5, "{rule}: {}", f.redacted);
        let body = &tok[tok.len() - 12..];
        let wire = serde_json::to_string(&found).unwrap();
        assert!(!wire.contains(body), "{rule}: {wire}");
        assert!(!format!("{f:?}").contains(body), "{rule}");
    }
}

#[test]
fn bare_token_mid_prose_is_found_at_its_column() {
    let tok = token("figd_", "prose", 40);
    let text = format!("line one\nplease use {tok} for the API\n");
    let found = scan(&text, None).unwrap();
    assert_eq!(rules(&found), vec!["cadence-figma-token"]);
    assert_eq!((found[0].line, found[0].column), (2, 12));
}

#[test]
fn gitleaks_pack_rules_fire() {
    let ghp = token(&["gh", "p_"].concat(), "classic", 36);
    let found = scan(&format!("{ghp}\n"), None).unwrap();
    assert_eq!(rules(&found), vec!["github-pat"]);
    assert_eq!(found[0].severity, Severity::Block);

    let upper = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let aws = ["AK", "IA", &noise_from("aws", 16, upper)].concat();
    let found = scan(&format!("creds\n{aws}\n"), None).unwrap();
    assert_eq!(rules(&found), vec!["aws-access-token"]);
}

#[test]
fn generic_api_key_is_warn_only() {
    let text = format!("config:\n  api_key = \"{}\"\n", noise("generic", 24));
    let found = scan(&text, None).unwrap();
    assert_eq!(rules(&found), vec!["generic-api-key"]);
    assert_eq!(found[0].severity, Severity::Warn);
    let warn = guard_with("test write", &text, &Allowlist::default()).unwrap();
    assert_eq!(rules(&warn), vec!["generic-api-key"]);
}

#[test]
fn argv_rule_blocks_secret_named_flags_and_assignments() {
    let value = noise("argv", 24);
    for text in [
        format!("run it: tool --figma-api-key {value} --verbose"),
        format!("tool --auth-token={value}"),
        format!("export SERVICE_TOKEN={value}"),
        format!("PGPASSWORD={value} psql"),
    ] {
        let found = scan(&text, None).unwrap();
        assert!(
            found
                .iter()
                .any(|f| f.rule == "cadence-argv-secret" && f.severity == Severity::Block),
            "{text}: {found:?}"
        );
    }
}

#[test]
fn argv_rule_ignores_placeholders_paths_and_prose() {
    for text in [
        "cadence message result m-1 --token <turn_id> --text done",
        "curl -H x --token $SERVICE_TOKEN",
        "--api-key-file /home/someone/.config/service/key.txt",
        "the token budget is 4000 and the auth module is fine",
        "--key 9e89785f0c1d2b3a4e5f60718293a4b5c6d7e8f9",
        "--token 123e4567-e89b-12d3-a456-426614174000",
        "SERVICE_URL=https://example.com/a/b/c/d/e/f/g",
        "--password correcthorsebatterystaple",
    ] {
        let found = scan(text, None).unwrap();
        assert!(found.is_empty(), "{text}: {found:?}");
    }
}

#[test]
fn guard_refuses_with_rule_named_and_no_value() {
    let tok = token("figd_", "guard", 40);
    let text = format!("please review\n{tok}\n");
    let err = guard_with("issue comment", &text, &Allowlist::default()).unwrap_err();
    assert_eq!(err.code(), Some("secret_detected"));
    let msg = err.to_string();
    assert!(msg.contains("rule cadence-figma-token"), "{msg}");
    assert!(msg.contains("line 2"), "{msg}");
    assert!(!msg.contains(&tok[5..]), "{msg}");
    assert!(!msg.contains(&tok[8..20]), "{msg}");
}

#[test]
fn operator_allowlist_by_rule_and_by_fingerprint() {
    let a = token("figd_", "allow-a", 40);
    let b = token("figd_", "allow-b", 40);
    let text = format!("{a}\n{b}\n");
    let found = scan(&text, None).unwrap();
    assert_eq!(found.len(), 2);

    let by_rule = Allowlist::parse("[[allow]]\nrule = \"cadence-figma-token\"\n").unwrap();
    assert!(guard_with("w", &text, &by_rule).unwrap().is_empty());

    let fp = &found[0].fingerprint;
    let by_fp = Allowlist::parse(&format!(
        "[[allow]]\nrule = \"cadence-figma-token\"\nfingerprint = \"{fp}\"\nreason = \"fixture\"\n"
    ))
    .unwrap();
    let err = guard_with("w", &text, &by_fp).unwrap_err().to_string();
    assert!(err.contains("line 2"), "{err}");
    assert!(!err.contains("line 1"), "{err}");

    let (payload, blocking) = report(&text, None, &by_fp).unwrap();
    assert!(blocking);
    assert_eq!(payload["allowlisted"], 1);
    assert_eq!(payload["blocking"], 1);
}

#[test]
fn malformed_allowlist_fails_closed() {
    assert!(Allowlist::parse("[[allow]]\nfingerprint = \"x\"\n").is_err());
    assert!(Allowlist::parse("[[allow]]\nrule = \"\"\n").is_err());
    assert!(Allowlist::parse("allow_all = true\n").is_err());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(ALLOWLIST_FILE), "not toml [").unwrap();
    let err = Allowlist::load(dir.path()).unwrap_err().to_string();
    assert!(err.contains("Refusing to write"), "{err}");
    // A missing file is simply an empty allowlist.
    let empty = tempfile::tempdir().unwrap();
    assert!(Allowlist::load(empty.path()).unwrap().allow.is_empty());
}

#[test]
fn clean_text_is_clean() {
    let text = "Fixes the relay retry. Ran cargo test --lib secret; 12 passed.\n\
                Commit 9e89785f0c1d2b3a4e5f60718293a4b5c6d7e8f9, PR #122.\n\
                The api key rotation is documented in docs/SESSION.md.\n";
    assert!(scan(text, None).unwrap().is_empty());
}

/// Every pattern in the vendored pack and the cadence set compiles under the
/// `regex` crate, including every allowlist pattern. The scanner compiles
/// lazily, so without this a bad pattern would only surface when its keyword
/// first appears.
#[test]
fn every_pattern_compiles() {
    let pack = pack().unwrap();
    assert!(pack.rules.len() > 200, "{}", pack.rules.len());
    let mut all: Vec<&Lazy> = Vec::new();
    let allows = std::iter::once(&pack.global).chain(pack.rules.iter().flat_map(|r| &r.allows));
    for a in allows {
        all.extend(&a.regexes);
        all.extend(&a.paths);
    }
    for r in &pack.rules {
        all.extend(r.regex.iter());
        all.extend(r.path.iter());
    }
    for lazy in all {
        if let Err(e) = lazy.get() {
            panic!("{}: {e}", lazy.src);
        }
    }
}

#[test]
fn rule_ids_are_unique() {
    let pack = pack().unwrap();
    let mut ids: Vec<&str> = pack.rules.iter().map(|r| r.id.as_str()).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len());
}

fn repo_file(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

#[test]
fn zero_findings_on_cargo_lock() {
    let found = scan(&repo_file("Cargo.lock"), Some("Cargo.lock")).unwrap();
    assert!(found.is_empty(), "{found:?}");
}

/// Synthetic credentials that existing unit tests use as scrubber inputs,
/// by file, rule and fingerprint. Upstream gitleaks v8.30.1 reports the same
/// gitleaks-rule spans when run on `src/`. Fingerprints survive line moves.
/// An entry may disappear; a new finding fails the test.
const KNOWN_FIXTURES: &[(&str, &str, &str)] = &[
    ("src/store.rs", "generic-api-key", "319256d3b4381405"),
    ("src/doctor/host.rs", "jwt", "500ea9399688915b"),
    ("src/doctor/host.rs", "curl-auth-header", "79366ebed2e93bf8"),
    (
        "src/doctor/host.rs",
        "cadence-argv-secret",
        "38ca332b7f3cb6b0",
    ),
    ("src/issue/report.rs", "generic-api-key", "372da3044cd499ab"),
];

/// The repo's own source has no credential-shaped string beyond the known
/// scrubber fixtures above. The vendored pack is excluded by gitleaks' own
/// global path allowlist (`gitleaks\.toml`), as it is in CI.
#[test]
fn no_new_findings_in_src() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stack = vec![root.join("src")];
    let mut scanned = 0;
    let mut hits = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            scanned += 1;
            for f in scan(&text, Some(&rel)).unwrap() {
                let known = KNOWN_FIXTURES.iter().any(|(file, rule, fp)| {
                    *file == rel && *rule == f.rule && *fp == f.fingerprint
                });
                if !known {
                    hits.push(format!(
                        "{rel}:{}:{} {} {}",
                        f.line, f.column, f.rule, f.fingerprint
                    ));
                }
            }
        }
    }
    assert!(scanned > 20, "{scanned}");
    assert!(hits.is_empty(), "{hits:#?}");
}

#[test]
fn vendored_pack_matches_its_pinned_header() {
    assert!(PACK_TOML.contains(&format!("# Release:  gitleaks {GITLEAKS_VERSION}")));
    assert!(PACK_TOML.contains(&format!("# Commit:   {GITLEAKS_COMMIT}")));
    assert!(PACK_TOML.contains("# MIT License"));
    let upstream = PACK_TOML
        .split_once("# END CADENCE HEADER\n")
        .map(|(_, rest)| rest)
        .unwrap();
    let digest: String = Sha256::digest(upstream.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(
        PACK_TOML.contains(&digest),
        "upstream body changed: {digest}"
    );
}

#[test]
fn re2_literal_braces_are_escaped() {
    assert_eq!(re2_braces(r"^\$(?:\d+|{\d+})$"), r"^\$(?:\d+|\{\d+\})$");
    assert_eq!(re2_braces(r"a{2,5}b{3}c{1,}"), r"a{2,5}b{3}c{1,}");
    assert_eq!(re2_braces(r"[{}]x{2}"), r"[{}]x{2}");
    assert_eq!(re2_braces(r"[[:alnum:]{]{4}"), r"[[:alnum:]{]{4}");
    assert_eq!(re2_braces(r"\{\{[ \t]*}}"), r"\{\{[ \t]*\}\}");
}

/// CAD-319: `redact_text` replaces each span in place and keeps the
/// surrounding text, multi-byte characters included; clean text is
/// returned unchanged.
#[test]
fn redact_text_replaces_only_the_secret_spans() {
    let a = token(&["gh", "p_"].concat(), "redact-a", 36);
    let b = token(&["sk-", "ant-"].concat(), "redact-b", 40);
    let text = format!("héllo {a} — ünd {b} ✓");
    let out = redact_text(&text).unwrap();
    assert!(!out.contains(&a) && !out.contains(&b), "{out}");
    assert!(out.starts_with("héllo [redacted:"), "{out}");
    assert!(out.contains(" — ünd [redacted:"), "{out}");
    assert!(out.ends_with(" ✓"), "{out}");
    assert_eq!(redact_text("nothing here ✓").unwrap(), "nothing here ✓");
}
