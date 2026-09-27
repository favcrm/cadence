//! CAD-700 actual offline CLI tests; no daemon, issuer or network authentication.
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const SECRET: &str = "result-secret-sentinel-do-not-print";
struct Fixture {
    root: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c700-")
            .tempdir_in("/tmp")
            .unwrap();
        for name in ["home", "config", "tmp", "state"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        let defaults = root.path().join("config/cadence");
        fs::create_dir(&defaults).unwrap();
        fs::write(defaults.join("orgs.json"), "malformed-default-org").unwrap();
        fs::create_dir(defaults.join("remote-auth")).unwrap();
        fs::write(
            defaults.join("remote-auth/credential.json"),
            "malformed-credential",
        )
        .unwrap();
        Self { root }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    fn run(&self, verb: &str, outbox: &Path, org: &str, input: &[u8], no_home: bool) -> Output {
        self.run_pin(
            verb,
            outbox,
            [
                org,
                "https://gateway.example.invalid",
                "subject-1",
                "agent-1",
            ],
            input,
            no_home,
        )
    }
    fn run_pin(
        &self,
        verb: &str,
        outbox: &Path,
        pin: [&str; 4],
        input: &[u8],
        no_home: bool,
    ) -> Output {
        let mut file = tempfile::tempfile_in(self.root.path()).unwrap();
        file.write_all(input).unwrap();
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_cadence"));
        command
            .env_clear()
            .current_dir(self.root.path())
            .env("TMPDIR", self.path("tmp"))
            .env("CADENCE_SUITE_LOCK", self.path("suite.lock"))
            .env("CADENCE_STATE_DIR", self.path("state"))
            .env("CADENCE_TOKEN", "credential-secret-sentinel")
            .env("CADENCE_ISSUER", "http://unreachable.invalid")
            .env("CADENCE_ORG", "different-default")
            .env("CADENCE_ALIAS", "master")
            .env("CADENCE_PROFILE", "malformed-default-profile")
            .args(["remote", "result", verb, "--outbox-dir"])
            .arg(outbox)
            .args([
                "--org",
                pin[0],
                "--audience",
                pin[1],
                "--subject",
                pin[2],
                "--agent",
                pin[3],
            ])
            .stdin(Stdio::from(file));
        for name in ["RUSTUP_HOME", "CARGO_HOME"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if !no_home {
            command
                .env("HOME", self.path("home"))
                .env("XDG_CONFIG_HOME", self.path("config"));
        }
        cadence_agent::reaper::output(&mut command).unwrap()
    }
    fn retain(&self, org: &str, value: &Value) -> Output {
        self.run(
            "retain",
            &self.path("outbox"),
            org,
            value.to_string().as_bytes(),
            false,
        )
    }
}
fn wire(id: &str) -> Value {
    json!({"version":"hosted-cadence-result.v1","commandId":id,"kind":"agent_result","assignmentId":"assignment-1","taskId":"task-1","taskRevision":1,"turnId":"turn-1","reportedHeadSha":"a".repeat(40),"text":SECRET})
}
fn receipt(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains(SECRET));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    serde_json::from_slice(&output.stdout).unwrap()
}
fn refusal(output: &Output) {
    assert!(!output.status.success());
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(SECRET));
        assert!(!text.contains("credential-secret-sentinel"));
    }
}
#[test]
fn retain_reopen_and_pending_are_offline_metadata_only_without_home_or_daemon() {
    let f = Fixture::new();
    let input = wire("command-1").to_string();
    let first = receipt(&f.run("retain", &f.path("outbox"), "org-1", input.as_bytes(), true));
    assert_eq!(first["state"], "local_pending");
    assert_eq!(first["destination"]["org"], "org-1");
    let pending = receipt(&f.run("pending", &f.path("outbox"), "org-1", b"", false));
    assert_eq!(pending["receipts"], json!([first]));
    assert!(fs::read_dir(f.path("state")).unwrap().next().is_none());
    assert_eq!(
        fs::read_to_string(f.path("config/cadence/orgs.json")).unwrap(),
        "malformed-default-org"
    );
    assert_eq!(
        fs::read_to_string(f.path("config/cadence/remote-auth/credential.json")).unwrap(),
        "malformed-credential"
    );
}
#[test]
fn exact_retry_retains_original_receipt_bytes_and_destination_conflicts() {
    let f = Fixture::new();
    let first = receipt(&f.retain("org-1", &wire("command-1")));
    let before = fs::read(f.path("outbox/results.sqlite3")).unwrap();
    assert_eq!(receipt(&f.retain("org-1", &wire("command-1"))), first);
    assert_eq!(fs::read(f.path("outbox/results.sqlite3")).unwrap(), before);
    let original = [
        "org-1",
        "https://gateway.example.invalid",
        "subject-1",
        "agent-1",
    ];
    for (index, changed) in [
        (0, "org-2"),
        (1, "https://other.example.invalid"),
        (2, "subject-2"),
        (3, "agent-2"),
    ] {
        let mut pin = original;
        pin[index] = changed;
        refusal(&f.run_pin(
            "retain",
            &f.path("outbox"),
            pin,
            wire("command-1").to_string().as_bytes(),
            false,
        ));
    }
    let numeric = wire("command-1")
        .to_string()
        .replace("\"taskRevision\":1", "\"taskRevision\":1e0");
    assert_eq!(
        receipt(&f.run(
            "retain",
            &f.path("outbox"),
            "org-1",
            numeric.as_bytes(),
            false
        )),
        first
    );
    let mut text_changed = wire("command-1");
    text_changed["text"] = json!("changed-secret-marker");
    refusal(&f.retain("org-1", &text_changed));
    let mut changed = wire("command-1");
    changed["reportedHeadSha"] = json!("b".repeat(40));
    refusal(&f.retain("org-1", &changed));
    assert_eq!(fs::read(f.path("outbox/results.sqlite3")).unwrap(), before);
    let foreign = receipt(&f.run("pending", &f.path("outbox"), "org-2", b"", false));
    assert_eq!(foreign["receipts"], json!([]));
}
#[test]
fn malformed_oversized_and_duplicate_stdin_refuse_before_creating_custody() {
    let f = Fixture::new();
    let duplicate = wire("command-1")
        .to_string()
        .replacen('{', "{\"commandId\":\"duplicate\",", 1);
    let mut credential = wire("command-1");
    credential["accessToken"] = json!(SECRET);
    let mut extra = wire("command-1");
    extra["actor"] = json!("operator");
    for input in [
        b"not JSON".to_vec(),
        duplicate.into_bytes(),
        extra.to_string().into_bytes(),
        credential.to_string().into_bytes(),
        format!("{} trailing {SECRET}", wire("command-1")).into_bytes(),
        vec![b'x'; 65_537],
        vec![0xff],
    ] {
        refusal(&f.run("retain", &f.path("outbox"), "org-1", &input, false));
        assert!(!f.path("outbox").exists());
    }
    refusal(&f.run(
        "retain",
        &f.path("outbox"),
        &"x".repeat(129),
        wire("command-1").to_string().as_bytes(),
        false,
    ));
    assert!(!f.path("outbox").exists());
}
#[test]
fn pending_requires_existing_custody_and_rejects_relative_paths() {
    let f = Fixture::new();
    refusal(&f.run("pending", &f.path("missing"), "org-1", b"", false));
    assert!(!f.path("missing").exists());
    fs::create_dir(f.path("empty")).unwrap();
    fs::set_permissions(f.path("empty"), fs::Permissions::from_mode(0o700)).unwrap();
    refusal(&f.run("pending", &f.path("empty"), "org-1", b"", false));
    assert!(fs::read_dir(f.path("empty")).unwrap().next().is_none());
    refusal(&f.run(
        "retain",
        Path::new("relative"),
        "org-1",
        wire("cmd").to_string().as_bytes(),
        false,
    ));
    assert!(!f.path("relative").exists());
}
#[test]
fn foreign_and_symlink_storage_refuse_without_modifying_existing_database() {
    let f = Fixture::new();
    fs::create_dir(f.path("foreign")).unwrap();
    fs::set_permissions(f.path("foreign"), fs::Permissions::from_mode(0o700)).unwrap();
    let file = f.path("foreign/results.sqlite3");
    fs::write(&file, b"foreign-secret-sentinel").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    for verb in ["retain", "pending"] {
        refusal(&f.run(
            verb,
            &f.path("foreign"),
            "org-1",
            wire("cmd").to_string().as_bytes(),
            false,
        ));
        assert_eq!(fs::read(&file).unwrap(), b"foreign-secret-sentinel");
    }
    symlink(f.path("foreign"), f.path("link")).unwrap();
    refusal(&f.run("pending", &f.path("link"), "org-1", b"", false));
    assert_eq!(fs::read(&file).unwrap(), b"foreign-secret-sentinel");
}
#[test]
fn concurrent_cli_retries_recover_one_stable_receipt() {
    let f = std::sync::Arc::new(Fixture::new());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let f = f.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            receipt(&f.retain("org-1", &wire("cmd")))
        }));
    }
    let first = workers.remove(0).join().unwrap();
    let second = workers.remove(0).join().unwrap();
    assert_eq!(first, second);
    let rows = receipt(&f.run("pending", &f.path("outbox"), "org-1", b"", false));
    assert_eq!(rows["receipts"], json!([first]));
}

#[test]
fn pending_refuses_symlink_database_or_journal_and_preserves_external_bytes() {
    let f = Fixture::new();
    receipt(&f.retain("org-1", &wire("cmd")));
    let db = f.path("outbox/results.sqlite3");
    let before = fs::read(&db).unwrap();
    fs::create_dir(f.path("linked-db")).unwrap();
    fs::set_permissions(f.path("linked-db"), fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&db, f.path("linked-db/results.sqlite3")).unwrap();
    refusal(&f.run("pending", &f.path("linked-db"), "org-1", b"", false));
    assert_eq!(fs::read(&db).unwrap(), before);
    let foreign = f.path("outside-journal");
    fs::write(&foreign, b"external-journal-secret-sentinel").unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&foreign, f.path("outbox/results.sqlite3-journal")).unwrap();
    refusal(&f.run("pending", &f.path("outbox"), "org-1", b"", false));
    assert_eq!(fs::read(&db).unwrap(), before);
    assert_eq!(
        fs::read(foreign).unwrap(),
        b"external-journal-secret-sentinel"
    );
}
