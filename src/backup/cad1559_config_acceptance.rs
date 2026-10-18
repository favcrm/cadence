//! CAD-1559 independent acceptance: a native `pm.yaml` may carry the real
//! `pi`, `host` and `hosted` runtime tables — they are declarative data an
//! installed workspace legitimately commits — but a credential-shaped value
//! in any of their String fields still refuses, and the receiver side
//! applies the same guard to a forged package whose member digests were
//! recomputed to hide it. Author: independent acceptance (not the
//! implementer). Written against the public export/inspect/inactive-restore
//! seams; the implementer must not edit or weaken it.
//!
//! The CLI's clean-tracker export is `export_portable_selected` with no
//! apps; `--snapshot-working-tree` is `export_portable_snapshot_selected`.
//! `inspect_portable` and `restore_inactive` are the receiver-side checks.
//! Fixtures are current `Store::open` databases and disposable Git trackers
//! under a private temp root. No daemon, lease, provider or pi process is
//! launched: the runtime tables are only bytes in the tracker. The
//! credential markers are built at runtime from a seeded generator plus a
//! split prefix, so this file itself contains no credential-shaped literal.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::backup::portable::{
    export_portable_selected, export_portable_snapshot_selected, inspect_portable,
    restore_inactive, PortableExportOptions, PortableRestoreOptions, Scope, INACTIVE_MARKER,
};
use crate::rollout::db_file;
use crate::store::Store;

const TICKET_REL: &str = "tickets/CAD-1559.md";
const TICKET_BODY: &str = "# CAD-1559 synthetic ticket fixture\n";
const TICKET_DIRTY: &str = "# CAD-1559 synthetic ticket fixture\n\nDirty working-tree edit.\n";
const PM_REL: &str = "pm/pm.yaml";
const WORKSPACE: &str = "synthetic-source-workspace-1559";

/// A credential-free `pm.yaml` that carries every admitted runtime table
/// together — pi pins and model policy, host overrides, the hosted lifecycle
/// lease — beside the ordinary schema/wiki/review/approvals metadata, with
/// comments and spacing a human editor leaves behind. Every field below is
/// declarative configuration, not a credential, and the block string fields
/// are exactly the surfaces a credential smuggled as a "model" or "provider"
/// name would ride in on.
const SAFE_CONFIG: &str = "# Operator configuration for the CAD-1559 fixture workspace.\n\
schema: 1\n\
notes_dir: /var/www/agent-notes\n\
statuses: [todo, doing, done]\n\
\n\
# wiki upload cap (bytes)\n\
wiki:\n  max_upload_bytes: 1048576\n\
\n\
review:\n  pair:\n    synthetic-worker: synthetic-reviewer\n  never:\n    synthetic-worker: [synthetic-other]\n\
\n\
approvals:\n  tailnet_logins: [synthetic-operator]\n\
\n\
# Pi agent runtime: provider package pins and the model vocabulary.\n\
pi:\n  providers:\n    - '@synthetic-org/pi-provider@1.4.0'\n    - synthetic-provider@2.0.0-rc.1\n  agentic_providers: [synthetic-agentic]\n  models:\n    allow:\n      - synthetic-provider/model-a\n      - synthetic-provider/model-b\n    master_allow: [synthetic-provider/model-a]\n    worker_allow: [synthetic-provider/model-b]\n    default:\n      master: synthetic-provider/model-a\n      worker: synthetic-provider/model-b\n\
\n\
# Host-level knobs and the lane/provider maps.\n\
host:\n  build_slots: 2\n  wal_checkpoint: true\n  priority_lanes: [reviewer, synth-release]\n  auto_stop_idle_secs_by_provider:\n    codex: 0\n    synth-provider: 7200\n\
\n\
# Hosted lifecycle: parse-only here — this test never acquires a lease.\n\
hosted:\n  lease: file:/var/lib/cad1559/hosted.lease\n  lease_ttl_secs: 900\n  lease_renew_secs: 300\n  flush_timeout_secs: 30\n";

/// `pi`, `host` and `hosted` present but empty — the shape an installed
/// 3762 exporter rejected when it treated any of these keys as
/// credential-bearing.
const EMPTY_BLOCKS_CONFIG: &str =
    "schema: 1\nnotes_dir: /var/www/agent-notes\npi: {}\nhost: {}\nhosted: {}\n";

const ALPHANUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// `n` pseudo-random characters, fixed by `seed` (the `secret` tests' recipe).
fn noise(seed: &str, n: usize) -> String {
    let mut out = String::with_capacity(n);
    let mut counter = 0u32;
    while out.len() < n {
        let block = Sha256::digest(format!("{seed}:{counter}").as_bytes());
        for b in block {
            if out.len() == n {
                break;
            }
            out.push(ALPHANUM[b as usize % ALPHANUM.len()] as char);
        }
        counter += 1;
    }
    out
}

/// A synthetic credential-shaped value: the documented `sk-ant-api03-`
/// provider-key shape over seeded noise. It is not a real secret, and no
/// fixture stores it in a literal a scanner would flag.
fn synthetic_key(seed: &str) -> String {
    let prefix = ["sk", "-ant-", "api03-"].concat();
    [prefix, noise(seed, 93)].concat()
}

fn root(owned: &tempfile::TempDir, name: &str) -> PathBuf {
    let dir = owned.path().join(name);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture_root(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(prefix).tempdir().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "cad1559-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad1559-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A clean tracker: one commit with a ticket and the given `pm/pm.yaml`.
fn make_tracker(root: &Path, config: &str) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(tracker.join("tickets")).unwrap();
    git(&tracker, &["init", "-q"]);
    fs::write(tracker.join(TICKET_REL), TICKET_BODY).unwrap();
    commit_config(&tracker, config);
    tracker
}

/// Commit `pm.yaml` bytes into an existing tracker's `pm/` directory.
fn commit_config(tracker: &Path, config: &str) {
    fs::create_dir_all(tracker.join("pm")).unwrap();
    fs::write(tracker.join(PM_REL), config).unwrap();
    git(tracker, &["add", "-A"]);
    git(tracker, &["commit", "-q", "-m", "CAD-1559 fixture"]);
}

/// A current-schema store with one coordination metadata row.
fn make_state(root: &Path) -> PathBuf {
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    drop(Store::open(&db_file(&state)).unwrap());
    Connection::open(db_file(&state))
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO connection_metadata(singleton, workspace_id) VALUES(1, ?1)",
            [WORKSPACE],
        )
        .unwrap();
    state
}

fn opts(tracker: &Path) -> PortableExportOptions {
    PortableExportOptions {
        scope: Scope::Coordination,
        tracker: tracker.to_path_buf(),
    }
}

fn restore_opts() -> PortableRestoreOptions {
    PortableRestoreOptions { replace: false }
}

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Identity of a quiesced source database: whole-file bytes already carry
/// schema and rows, so no separate per-row digest is taken.
fn quiesce(db: &Path) {
    Connection::open(db)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
}

fn db_sha(db: &Path) -> String {
    sha_hex(&fs::read(db).unwrap())
}

/// Everything the export must leave alone: source HEAD, working-tree
/// status, `pm.yaml` bytes and the quiesced store digest.
struct SourceStamp {
    head: String,
    status: String,
    pm_yaml: Vec<u8>,
    db_sha256: String,
}

fn source_stamp(tracker: &Path, state: &Path) -> SourceStamp {
    let db = db_file(state);
    quiesce(&db);
    SourceStamp {
        head: git(tracker, &["rev-parse", "HEAD"]),
        status: git(
            tracker,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        ),
        pm_yaml: fs::read(tracker.join(PM_REL)).unwrap(),
        db_sha256: db_sha(&db),
    }
}

/// HEAD and `pm.yaml` must be identical after the seam runs; the dirty
/// working-tree status must read exactly as it did before.
fn assert_source_unchanged(tracker: &Path, state: &Path, stamp: &SourceStamp) {
    assert_eq!(
        git(tracker, &["rev-parse", "HEAD"]),
        stamp.head,
        "source HEAD changed"
    );
    assert_eq!(
        git(
            tracker,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        stamp.status,
        "source working-tree status changed"
    );
    assert_eq!(
        fs::read(tracker.join(PM_REL)).unwrap(),
        stamp.pm_yaml,
        "source pm.yaml changed"
    );
    assert_eq!(
        db_sha(&db_file(state)),
        stamp.db_sha256,
        "source database changed"
    );
}

/// No `.<name>.partial-*` staging entry is left next to `out`.
fn no_partial(out: &Path) -> bool {
    let parent = out.parent().unwrap();
    let name = out.file_name().unwrap().to_string_lossy().into_owned();
    let prefix = format!(".{name}.partial-");
    fs::read_dir(parent)
        .unwrap()
        .flatten()
        .all(|entry| !entry.file_name().to_string_lossy().starts_with(&prefix))
}

fn read_manifest(out: &Path) -> Value {
    serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap()
}

/// Recompute every member digest and rewrite the manifest, so the only
/// defect left in a forged package is the bytes the test planted — exactly
/// what a producer-side checksum cannot catch.
fn rehash_manifest(out: &Path) {
    let mut manifest = read_manifest(out);
    let members: Vec<Value> = manifest["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let file = out.join(m["path"].as_str().unwrap());
            let bytes = fs::read(&file).unwrap();
            serde_json::json!({"path": m["path"], "bytes": bytes.len(), "sha256": sha_hex(&bytes)})
        })
        .collect();
    manifest["members"] = Value::Array(members);
    fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

/// The refused export published nothing and left no private staging dir.
fn assert_unpublished(out: &Path) {
    assert!(!out.exists(), "refused export published {}", out.display());
    assert!(no_partial(out), "refused export left a partial staging dir");
}

/// Refusals report a reason, never the credential itself. `marker` is the
/// decoded scalar, which must not appear in the error text either.
fn assert_refusal(err: &crate::error::Error, marker: Option<&str>, why: &str) {
    let text = err.to_string();
    assert!(!text.is_empty(), "{why}: empty refusal");
    if let Some(marker) = marker {
        assert!(
            !text.contains(marker),
            "{why}: refusal echoed the credential marker"
        );
    }
}

/// A runtime-table `pm.yaml` body that is structurally valid but whose named
/// String field holds the credential-shaped `value`.
fn config_with(runtime: &str) -> String {
    format!("schema: 1\nnotes_dir: /var/www/agent-notes\n{runtime}")
}

#[test]
fn cad1559_full_runtime_config_exports_inspects_and_restores_byte_exact() {
    let owned = fixture_root("cad1559-full-config-");
    let dir = root(&owned, "full");
    let tracker = make_tracker(&dir, SAFE_CONFIG);
    let state = make_state(&dir);
    let stamp = source_stamp(&tracker, &state);

    // Clean-tracker export (the CLI's default path).
    let out = dir.join("package");
    export_portable_selected(&state, &out, &opts(&tracker), &[])
        .expect("clean export admits pi/host/hosted config");
    let packaged_pm = out.join("tracker").join(PM_REL);
    assert_eq!(
        fs::read(&packaged_pm).unwrap(),
        stamp.pm_yaml,
        "packaged pm.yaml differs from the committed bytes"
    );
    assert_eq!(
        fs::read(out.join("tracker").join(TICKET_REL)).unwrap(),
        TICKET_BODY.as_bytes(),
        "ordinary tracker content is carried"
    );
    let manifest = read_manifest(&out);
    assert_eq!(manifest["format"], "cadence.portable/1");
    assert_eq!(manifest["pm_root"], "pm");

    // The receiver checks accept what the producer shipped.
    let inspected = inspect_portable(&out).expect("native inspect admits the package");
    assert_eq!(inspected["status"], "verified_coordination_package");
    let dest = dir.join("restored");
    let report = restore_inactive(&out, &dest, &restore_opts())
        .expect("inactive restore admits the package");
    assert_eq!(report["state"], "committed_inactive");
    assert!(
        dest.join(INACTIVE_MARKER).is_file(),
        "restored root commits inactive"
    );
    assert_eq!(
        fs::read(dest.join("portable-tracker").join(PM_REL)).unwrap(),
        stamp.pm_yaml,
        "restored pm.yaml differs from the source bytes"
    );
    assert!(
        crate::backup::portable::refuse_if_inactive(&dest).is_err(),
        "restored root stays fenced off from daemon start"
    );

    // Now dirty the tracker: a staged pm.yaml comment edit and an unstaged
    // ticket edit. The snapshot export must publish the working-tree bytes.
    let dirty_pm = format!("{}\n# staged comment added after commit\n", SAFE_CONFIG);
    fs::write(tracker.join(PM_REL), &dirty_pm).unwrap();
    git(&tracker, &["add", PM_REL]);
    fs::write(tracker.join(TICKET_REL), TICKET_DIRTY).unwrap();
    let dirty_stamp = source_stamp(&tracker, &state);

    let snap = dir.join("snapshot-package");
    export_portable_snapshot_selected(&state, &snap, &opts(&tracker), &[])
        .expect("dirty working-tree snapshot export admits runtime config");
    assert_eq!(
        fs::read(snap.join("tracker").join(PM_REL)).unwrap(),
        dirty_pm.as_bytes(),
        "snapshot pm.yaml is not the working-tree bytes"
    );
    assert_eq!(
        fs::read(snap.join("tracker").join(TICKET_REL)).unwrap(),
        TICKET_DIRTY.as_bytes(),
        "snapshot carries the dirty ticket bytes"
    );
    inspect_portable(&snap).expect("snapshot package inspects");
    let snap_dest = dir.join("restored-snapshot");
    restore_inactive(&snap, &snap_dest, &restore_opts())
        .expect("snapshot package restores inactive");

    // The dirty source — including the uncommitted edits — survives.
    assert_source_unchanged(&tracker, &state, &dirty_stamp);
}

#[test]
fn cad1559_empty_runtime_blocks_export_and_inspect() {
    let owned = fixture_root("cad1559-empty-blocks-");
    let dir = root(&owned, "empty");
    let tracker = make_tracker(&dir, EMPTY_BLOCKS_CONFIG);
    let state = make_state(&dir);
    let stamp = source_stamp(&tracker, &state);

    let out = dir.join("package");
    export_portable_selected(&state, &out, &opts(&tracker), &[])
        .expect("empty pi/host/hosted tables are harmless data");
    assert_eq!(
        fs::read(out.join("tracker").join(PM_REL)).unwrap(),
        EMPTY_BLOCKS_CONFIG.as_bytes()
    );
    inspect_portable(&out).expect("package with empty runtime tables inspects");
    assert_source_unchanged(&tracker, &state, &stamp);
}

/// Credential-shaped values under otherwise valid `pi`, `host` and `hosted`
/// String fields refuse at both export seams: nothing is published, the
/// source is untouched, and the refusal never echoes the value. Two vectors
/// cover what a value-only scan would miss: a decoded scalar whose YAML
/// escapes hide the shape (`s\x6B-ant-…` parses to `sk-ant-…`), and a
/// credential-shaped raw `#` comment that no parsed field ever holds.
#[test]
fn cad1559_export_refuses_credential_shaped_runtime_values() {
    let owned = fixture_root("cad1559-credential-");
    let key = synthetic_key("cad1559-key");
    // The whole scalar, with one character YAML-escaped: `s\x6B-ant-…` is
    // raw text, but the parsed value is exactly `sk-ant-…` — caught only if
    // the scan sees the decoded scalar, not the raw bytes.
    let escaped = key.replacen('k', "\\x6B", 1);
    assert!(
        !escaped.contains(key.as_str()),
        "fixture keeps only the escape"
    );

    let cases: Vec<(&str, String, Option<&str>)> = vec![
        (
            "pi provider pin",
            config_with(&format!("pi:\n  providers: [{key}]\n")),
            Some(&key),
        ),
        (
            "pi model default",
            config_with(&format!(
                "pi:\n  models:\n    default:\n      master: \"{key}\"\n"
            )),
            Some(&key),
        ),
        (
            "pi allow list entry (escaped scalar)",
            config_with(&format!(
                "pi:\n  models:\n    allow:\n      - \"{escaped}\"\n"
            )),
            Some(&key),
        ),
        (
            "host priority lane",
            config_with(&format!("host:\n  priority_lanes: [\"{key}\"]\n")),
            Some(&key),
        ),
        (
            "host provider map key",
            config_with(&format!(
                "host:\n  auto_stop_idle_secs_by_provider:\n    \"{key}\": 60\n"
            )),
            Some(&key),
        ),
        (
            "hosted lease",
            config_with(&format!("hosted:\n  lease: \"{key}\"\n")),
            Some(&key),
        ),
        (
            "credential-shaped YAML comment",
            config_with(&format!(
                "pi:\n  providers: [synthetic-provider@1.0.0]\n# prior key: {key}\n"
            )),
            Some(&key),
        ),
    ];

    for (i, (name, config, marker)) in cases.iter().enumerate() {
        let dir = root(&owned, &format!("cred-{i}"));
        let tracker = make_tracker(&dir, config);
        let state = make_state(&dir);
        // Control: the same fixture with the safe config exports, so each
        // refusal is this guard, not catalog/setup noise.
        let control = dir.join("control");
        commit_config(&tracker, SAFE_CONFIG);
        export_portable_selected(&state, &control, &opts(&tracker), &[])
            .expect("safe config control exports");
        fs::remove_dir_all(&control).unwrap();
        // Replay the bad config on the same tracker.
        commit_config(&tracker, config);
        let stamp = source_stamp(&tracker, &state);

        let clean_out = dir.join("out-clean");
        let err = export_portable_selected(&state, &clean_out, &opts(&tracker), &[])
            .expect_err(&format!("{name}: clean export must refuse"));
        assert_refusal(&err, *marker, name);
        assert_unpublished(&clean_out);

        let snap_out = dir.join("out-snapshot");
        let err = export_portable_snapshot_selected(&state, &snap_out, &opts(&tracker), &[])
            .expect_err(&format!("{name}: snapshot export must refuse"));
        assert_refusal(&err, *marker, name);
        assert_unpublished(&snap_out);

        assert_source_unchanged(&tracker, &state, &stamp);
    }
}

/// Unknown nested keys under a runtime table and mistyped or null runtime
/// blocks stay refused: the schema for the new tables is closed and
/// explicit, and a credential-shaped key name cannot smuggle a value in.
#[test]
fn cad1559_export_refuses_unknown_keys_and_mistyped_runtime_blocks() {
    let owned = fixture_root("cad1559-shape-");
    let key = synthetic_key("cad1559-shape-key");
    let cases: Vec<(&str, String)> = vec![
        (
            "credential-named nested key under pi",
            config_with(&format!("pi:\n  {key}: x\n")),
        ),
        (
            "unknown key under host",
            config_with("host:\n  credential_vault: on\n"),
        ),
        (
            "unknown key under hosted",
            config_with("hosted:\n  token_file: /tmp/x\n"),
        ),
        ("pi as a scalar", config_with("pi: 42\n")),
        ("pi null", config_with("pi:\n")),
        ("host null", config_with("host: null\n")),
        ("hosted null", config_with("hosted: ~\n")),
        ("pi sequence", config_with("pi:\n  - providers\n")),
    ];

    for (i, (name, config)) in cases.iter().enumerate() {
        let dir = root(&owned, &format!("shape-{i}"));
        let tracker = make_tracker(&dir, config);
        let state = make_state(&dir);
        let stamp = source_stamp(&tracker, &state);
        let out = dir.join("out");
        let err = export_portable_selected(&state, &out, &opts(&tracker), &[])
            .expect_err(&format!("{name}: export must refuse"));
        assert_refusal(&err, Some(&key), name);
        assert_unpublished(&out);
        assert_source_unchanged(&tracker, &state, &stamp);
    }
}

/// A package whose tracker carries a credential-shaped runtime value must be
/// refused even when every member digest and manifest hash was recomputed:
/// the receiver runs the same admission guard, so a forged producer or a
/// tampered transit copy cannot launder it through the checksums.
#[test]
fn cad1559_inspect_and_restore_refuse_forged_runtime_credential_package() {
    let owned = fixture_root("cad1559-forged-");
    let dir = root(&owned, "forged");
    let tracker = make_tracker(&dir, SAFE_CONFIG);
    let state = make_state(&dir);
    let key = synthetic_key("cad1559-forged-key");

    // Control: the unforged package inspects and restores, proving the only
    // defect below is the planted credential-shaped value.
    let control = dir.join("control-package");
    export_portable_selected(&state, &control, &opts(&tracker), &[])
        .expect("control export succeeds");
    inspect_portable(&control).expect("control package inspects");
    let control_dest = dir.join("control-restored");
    restore_inactive(&control, &control_dest, &restore_opts()).expect("control package restores");

    // Forge: swap the packaged pm.yaml for a schema-valid runtime config
    // carrying the marker, then recompute member digests and rewrite the
    // manifest so every checksum verifies.
    let forged_pm = config_with(&format!("hosted:\n  lease: \"{key}\"\n"));
    fs::write(
        dir.join("control-package").join("tracker").join(PM_REL),
        &forged_pm,
    )
    .unwrap();
    let package = dir.join("control-package");
    rehash_manifest(&package);

    let err = inspect_portable(&package)
        .expect_err("forged package must fail the receiver's own admission");
    assert_refusal(&err, Some(&key), "forged inspect");

    let dest = dir.join("forged-restored");
    let err = restore_inactive(&package, &dest, &restore_opts())
        .expect_err("forged package must be refused before any destination write");
    assert_refusal(&err, Some(&key), "forged restore");
    assert!(!dest.exists(), "refused restore created the destination");
    assert!(
        fs::read_dir(&dir).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".portable-stage-")),
        "refused restore left a staged destination"
    );
}
