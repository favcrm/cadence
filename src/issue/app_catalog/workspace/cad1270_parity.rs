//! CAD-1270 parity test: `check_offline` (the daemon-free `app check`)
//! and `install_check` (the daemon's `app catalog install-check`) agree
//! on the same bundle bytes — the ticket's required agreement over the
//! built-in CRM and Social Content packages plus malformed fixtures.
//! Parity is the pass/refusal verdict and, on a pass, the bundle digest;
//! stateful catalog admission (same-name installs, pending journals, the
//! tracker-self guard, the PM agent registry) stays install-check-only
//! by design and is excluded.

use super::*;

const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n---\n\n\
Why.\n\n## Do {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] done\n";

/// A minimal valid bundle `dir` with `manifest` as its app.md.
fn write_bundle(dir: &Path, manifest: &str) {
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    std::fs::write(dir.join("app.md"), manifest).unwrap();
    std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
}

fn manifest(extra: &str) -> String {
    format!(
        "---\napp: fixture-app\ntitle: Fixture\nversion: '1'\n{extra}needs:\n  connections: []\n---\n\nGuide.\n"
    )
}

/// A refusal report carries one of the stable stage codes and, when the
/// bundle's files were loaded, the digest and per-file bytes/cap table.
fn check_is_refusal(report: &Value, code: &str) {
    assert_eq!(report["ok"], json!(false), "{report}");
    assert_eq!(report["refusal"]["code"], json!(code), "{report}");
    assert!(
        report["refusal"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "{report}"
    );
}

#[test]
fn cad1270_check_and_install_check_agree_on_the_same_bytes() {
    let pm_dir = tempfile::tempdir().unwrap();
    let pm = Pm::init(pm_dir.path()).unwrap();
    let sources = tempfile::tempdir().unwrap();

    // --- valid packages: both embedded built-ins, their on-disk
    // workspace-apps/ copies, and a local fixture bundle.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources_ok: Vec<String> = vec![
        "builtin:crm".into(),
        "builtin:social-content".into(),
        repo.join("workspace-apps/crm").display().to_string(),
        repo.join("workspace-apps/social-content")
            .display()
            .to_string(),
    ];
    let on_disk = sources.path().join("fixture-app");
    write_bundle(&on_disk, &manifest(""));
    sources_ok.push(on_disk.display().to_string());

    for source in &sources_ok {
        let install = install_check(&pm, source)
            .unwrap_or_else(|e| panic!("install-check refused {source}: {e}"));
        let check =
            check_offline(source).unwrap_or_else(|e| panic!("check errored on {source}: {e}"));
        assert_eq!(check["ok"], json!(true), "{source}: {check}");
        assert_eq!(
            check["digest"], install["digest"],
            "digest divergence on {source}"
        );
        assert_eq!(check["name"], install["name"], "{source}");
        assert_eq!(check["version"], install["version"], "{source}");
        // Per-file bytes against cap are reported for loaded bytes.
        let files = check["files"].as_array().unwrap();
        assert_eq!(
            files.len(),
            install["files"].as_array().unwrap().len(),
            "{source}"
        );
        for row in files {
            assert!(row["bytes"].as_u64().unwrap() <= row["cap"].as_u64().unwrap());
        }
    }

    // --- malformed fixture 1: a non-screen member over the 256 KiB cap.
    let over_cap = sources.path().join("over-cap");
    write_bundle(&over_cap, &manifest(""));
    let big = "x".repeat(256 * 1024 + 1);
    std::fs::create_dir_all(over_cap.join("rubrics")).unwrap();
    std::fs::write(over_cap.join("rubrics/big.md"), &big).unwrap();
    let install = install_check(&pm, over_cap.to_str().unwrap());
    let check = check_offline(over_cap.to_str().unwrap()).unwrap();
    assert!(install.is_err(), "install-check accepted an over-cap file");
    check_is_refusal(&check, CHECK_INVENTORY);

    // --- malformed fixture 2: summary over the 160-char cap.
    let long_summary = sources.path().join("long-summary");
    write_bundle(
        &long_summary,
        &manifest(&format!("summary: '{}'\n", "s".repeat(161))),
    );
    let install = install_check(&pm, long_summary.to_str().unwrap());
    let check = check_offline(long_summary.to_str().unwrap()).unwrap();
    assert!(install.is_err(), "install-check accepted a >160 summary");
    check_is_refusal(&check, CHECK_CONTENT);
    // The digest of the loaded bytes is still reported, matching the
    // bundle install-check refused on.
    assert!(check["digest"].as_str().unwrap().starts_with("sha256:"));

    // --- malformed fixture 3: a contract the host registry does not know.
    let bad_requires = sources.path().join("bad-requires");
    write_bundle(
        &bad_requires,
        &manifest("").replace(
            "needs:\n  connections: []",
            "needs:\n  connections: []\n  requires: {contracts: {app-storage: [1]}}",
        ),
    );
    let install = install_check(&pm, bad_requires.to_str().unwrap());
    let check = check_offline(bad_requires.to_str().unwrap()).unwrap();
    assert!(
        install.is_err(),
        "install-check accepted an unknown contract requirement"
    );
    check_is_refusal(&check, CHECK_CONTENT);
    // The compat receipt inside the refusal names the unmet contract.
    assert_eq!(check["compatibility"]["ok"], json!(false), "{check}");
    let unmet = check["compatibility"]["unmet"].as_str().unwrap_or("");
    assert!(unmet.contains("app-storage"), "{check}");

    // --- malformed fixture 4: a screen member outside the package
    // grammar (a bare leaf under screens/, not screens/<tag>/<leaf>).
    let bad_screen = sources.path().join("bad-screen");
    write_bundle(&bad_screen, &manifest(""));
    std::fs::create_dir_all(bad_screen.join("screens")).unwrap();
    std::fs::write(bad_screen.join("screens/flat.js"), "x").unwrap();
    let install = install_check(&pm, bad_screen.to_str().unwrap());
    let check = check_offline(bad_screen.to_str().unwrap()).unwrap();
    assert!(
        install.is_err(),
        "install-check accepted a flat screens/ leaf"
    );
    check_is_refusal(&check, CHECK_INVENTORY);

    // --- the no-state-write property: a check pass and a refusal leave
    // the PM dir untouched (no catalog, journal, lock or tracker file).
    let pm_before: Vec<String> = walk(pm_dir.path())
        .into_iter()
        .filter(|p| !p.starts_with(".git"))
        .collect();
    check_offline(on_disk.to_str().unwrap()).unwrap();
    check_offline(over_cap.to_str().unwrap()).unwrap();
    let pm_after: Vec<String> = walk(pm_dir.path())
        .into_iter()
        .filter(|p| !p.starts_with(".git"))
        .collect();
    assert_eq!(pm_before, pm_after, "check wrote into the PM dir");

    // --- a remote source is refused as a transport, never fetched.
    let check = check_offline("https://example.invalid/repo.git").unwrap();
    check_is_refusal(&check, CHECK_TRANSPORT);
}

/// Relative pm-dir paths below `root`, sorted, for the no-write compare.
fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            out.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}
