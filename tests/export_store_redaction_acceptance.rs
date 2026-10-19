//! CAD-1580: ticket-authored acceptance, independent of the exporter implementation.
//! All credentials are deterministic synthetic shapes; no daemon or live data is used.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use cadence_agent::backup;
use cadence_agent::backup::portable::{
    export_portable, export_portable_snapshot_selected, inspect_portable, restore_inactive,
    PortableExportOptions, PortableRestoreOptions, Scope, INACTIVE_MARKER,
};
use cadence_agent::issue::Pm;
use cadence_agent::store::Store;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn noise(seed: &str, len: usize) -> String {
    const ALPHANUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut value = String::with_capacity(len);
    let mut counter = 0u32;
    while value.len() < len {
        for byte in Sha256::digest(format!("{seed}:{counter}")) {
            if value.len() == len {
                break;
            }
            value.push(ALPHANUM[byte as usize % ALPHANUM.len()] as char);
        }
        counter += 1;
    }
    value
}

struct Fixture {
    root: TempDir,
    state: PathBuf,
    tracker: PathBuf,
    argv: String,
    provider: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad1580-")
            .tempdir()
            .unwrap();
        let state = root.path().join("state");
        let tracker = root.path().join("tracker");
        fs::create_dir(&state).unwrap();
        Pm::init(&tracker).unwrap();
        drop(Store::open(&state.join("cadence.sqlite3")).unwrap());
        // Deliberately not a registered native turn-token shape.
        let argv = format!("claude-{}", noise("cad1580-historical-argv", 32));
        let provider = format!("{}{}", ["gh", "p_"].concat(), noise("cad1580-provider", 36));
        let body = serde_json::json!({
            "historical": "keep this unrelated discussion",
            "command": format!("before --auth-token {argv} after"),
            "provider": provider,
            "unicode": "café"
        })
        .to_string();
        let blob = format!("blob before --auth-token {argv} blob after").into_bytes();
        let conn = Connection::open(state.join("cadence.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn.execute(
            "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,
                                generation,pid,created,updated,uid)
             VALUES('w1','codex','stdio','worker',?1,'workspace-write','idle',
                    'native-fixture-generation',4242,1,1,'agt-fixture-w1')",
            params![tracker.to_str().unwrap()],
        )
        .unwrap();
        for (id, value) in [
            ("redact-json", rusqlite::types::Value::Text(body)),
            ("redact-blob", rusqlite::types::Value::Blob(blob)),
            (
                "keep",
                rusqlite::types::Value::Text("unrelated historical work remains byte-exact".into()),
            ),
        ] {
            conn.execute(
                "INSERT INTO messages(id,alias,body,source,state,created)
                 VALUES(?1,'w1',?2,'cli','completed',1)",
                params![id, value],
            )
            .unwrap();
        }
        drop(conn);
        Self {
            root,
            state,
            tracker,
            argv,
            provider,
        }
    }

    fn opts(&self, enabled: bool) -> PortableExportOptions {
        PortableExportOptions {
            scope: Scope::Coordination,
            tracker: self.tracker.clone(),
            redact_store_secrets: enabled,
        }
    }

    fn db(&self) -> PathBuf {
        self.state.join("cadence.sqlite3")
    }
}

fn files(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, u32)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                collect(root, &path, out);
            } else {
                assert!(
                    metadata.is_file(),
                    "fixture has no symlinks or special files"
                );
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    (fs::read(&path).unwrap(), metadata.permissions().mode()),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    collect(root, root, &mut result);
    result
}

fn assert_redacted_database(db: &Path, fixture: &Fixture) {
    let bytes = fs::read(db).unwrap();
    for secret in [&fixture.argv, &fixture.provider] {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "credential bytes must not survive in live records or free pages"
        );
    }
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let json: String = conn
        .query_row(
            "SELECT body FROM messages WHERE id='redact-json'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let json: Value = serde_json::from_str(&json).expect("redaction preserves JSON syntax");
    assert_eq!(json["historical"], "keep this unrelated discussion");
    assert_eq!(json["unicode"], "café");
    let command = json["command"].as_str().unwrap();
    assert!(command.starts_with("before --auth-token ") && command.ends_with(" after"));
    let (kind, blob): (String, Vec<u8>) = conn
        .query_row(
            "SELECT typeof(body),body FROM messages WHERE id='redact-blob'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        kind, "blob",
        "redaction must not change SQLite storage class"
    );
    let blob = String::from_utf8(blob).unwrap();
    assert!(blob.starts_with("blob before --auth-token ") && blob.ends_with(" blob after"));
    let ordinary: String = conn
        .query_row("SELECT body FROM messages WHERE id='keep'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ordinary, "unrelated historical work remains byte-exact");
}

#[test]
fn cad1580_explicit_redaction_preserves_source_across_native_export_modes() {
    let fixture = Fixture::new();
    for mode in ["plain", "portable", "working-tree"] {
        if mode == "working-tree" {
            fs::write(
                fixture.tracker.join("uncommitted-note.md"),
                "ordinary dirty working tree\n",
            )
            .unwrap();
        }
        let source_db = fs::read(fixture.db()).unwrap();
        let source_tracker = files(&fixture.tracker);
        let refused = fixture.root.path().join(format!("{mode}-default"));
        let result = match mode {
            "plain" => backup::export(&fixture.state, &refused, false),
            "portable" => export_portable(&fixture.state, &refused, &fixture.opts(false)),
            _ => export_portable_snapshot_selected(
                &fixture.state,
                &refused,
                &fixture.opts(false),
                &[],
            ),
        };
        let error = result.expect_err("default export must refuse credentials");
        assert_eq!(error.code(), Some("secret_detected"));
        assert!(!error.to_string().contains(&fixture.argv));
        assert!(!error.to_string().contains(&fixture.provider));
        assert!(!refused.exists(), "refusal must not publish a package");

        let out = fixture.root.path().join(format!("{mode}-redacted"));
        let report = match mode {
            "plain" => backup::export(&fixture.state, &out, true),
            "portable" => export_portable(&fixture.state, &out, &fixture.opts(true)),
            _ => export_portable_snapshot_selected(&fixture.state, &out, &fixture.opts(true), &[]),
        }
        .expect("explicit private-copy redaction exports");
        assert_eq!(report["store_secret_redaction"]["enabled"], true);
        assert_eq!(report["store_secret_redaction"]["cells"], 2);
        assert_redacted_database(&out.join("cadence.sqlite3"), &fixture);
        if mode != "plain" {
            inspect_portable(&out).expect("redacted package passes native inspection");
            let destination = fixture.root.path().join(format!("{mode}-restored"));
            restore_inactive(
                &out,
                &destination,
                &PortableRestoreOptions { replace: false },
            )
            .unwrap();
            assert!(destination.join(INACTIVE_MARKER).is_file());
            assert_redacted_database(&destination.join("cadence.sqlite3"), &fixture);
            let conn = Connection::open_with_flags(
                destination.join("cadence.sqlite3"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let authority: (i64, Option<i64>, Option<String>) = conn
                .query_row(
                    "SELECT enabled,pid,generation FROM agents WHERE alias='w1'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(authority, (0, None, None), "restore remains inactive");
        }
        assert_eq!(
            fs::read(fixture.db()).unwrap(),
            source_db,
            "live history changed"
        );
        assert_eq!(
            files(&fixture.tracker),
            source_tracker,
            "source tracker or Git metadata changed"
        );
    }
}

#[test]
fn cad1580_independent_refusal_opt_in_does_not_bypass_final_secret_scan() {
    let fixture = Fixture::new();
    let mut invalid_utf8 = vec![0xff];
    invalid_utf8.extend_from_slice(fixture.provider.as_bytes());
    invalid_utf8.push(0xfe);
    let conn = Connection::open(fixture.db()).unwrap();
    conn.execute(
        "INSERT INTO messages(id,alias,body,source,state,created)
         VALUES('unredactable','w1',?1,'cli','completed',1)",
        params![invalid_utf8],
    )
    .unwrap();
    drop(conn);
    let source = fs::read(fixture.db()).unwrap();
    for portable in [false, true] {
        let out = fixture.root.path().join(if portable {
            "blocked-portable"
        } else {
            "blocked-plain"
        });
        let result = if portable {
            export_portable(&fixture.state, &out, &fixture.opts(true))
        } else {
            backup::export(&fixture.state, &out, true)
        };
        let error = result.expect_err("opt-in cannot publish a surviving blocker");
        assert_eq!(error.code(), Some("secret_detected"));
        assert!(!error.to_string().contains(&fixture.provider));
        assert!(!out.exists());
    }
    assert_eq!(fs::read(fixture.db()).unwrap(), source);
}
