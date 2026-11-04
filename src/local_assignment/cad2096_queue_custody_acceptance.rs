// Independent CAD-2096 acceptance: a confirmed hosted queue survives local custody failure.
// Included inside cad2009_assignment_acceptance; all paths are disposable fixture paths.
const QUEUE_ISSUE: &str = "SYN-1";
const QUEUE_TARGET: &str = "user_target";

struct QueueFake {
    lock: PathBuf,
    arm_after_post: bool,
    posts: usize,
    key: Option<String>,
    committed: Option<Value>,
}

impl QueueFake {
    fn new(lock: PathBuf, arm_after_post: bool) -> Self {
        Self {
            lock,
            arm_after_post,
            posts: 0,
            key: None,
            committed: None,
        }
    }
}

impl CoordHttp for QueueFake {
    fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        if method == "GET" {
            assert_eq!(path, assignment_prefix(ORG));
            assert!(body.is_none());
            let rows: Vec<Value> = self.committed.iter().cloned().collect();
            return Ok(Reply {
                status: 200,
                body: json!({"version": VERSION, "assignments": rows}),
            });
        }
        assert_eq!(method, "POST");
        assert_eq!(path, assignment_prefix(ORG));
        self.posts += 1;
        assert_eq!(self.posts, 1, "confirmed queue POST was replayed");
        let body = body.unwrap();
        let key = body["idempotency_key"]
            .as_str()
            .expect("queue body carries idempotency_key")
            .to_owned();
        assert!(body.get("operation_id").is_none());
        assert_eq!(
            body,
            &json!({
                "version": VERSION, "organization_id": ORG, "idempotency_key": key,
                "issue_id": QUEUE_ISSUE, "action": "dispatch", "execution": "local",
                "identity_kind": "local_worker", "target": QUEUE_TARGET,
            })
        );
        self.key = Some(key);
        let mut row = original();
        row["state"] = json!("queued");
        row["claim_id"] = Value::Null;
        row["client_agent_id"] = Value::Null;
        row["issue_id"] = json!(QUEUE_ISSUE);
        self.committed = Some(row.clone());
        if self.arm_after_post {
            // Real private_lock failure (EISDIR) on the next ledger admission; no mocking.
            fs::remove_file(&self.lock).unwrap();
            fs::create_dir(&self.lock).unwrap();
        }
        Ok(Reply {
            status: 200,
            body: row,
        })
    }
}

fn queue_fixture(arm_after_post: bool) -> (tempfile::TempDir, PathBuf, Coordinator<QueueFake>) {
    let authority = tempfile::tempdir().unwrap();
    fs::set_permissions(authority.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let lock = authority.path().join(".cli-local-assign.lock");
    let producer = QueueFake::new(lock.clone(), arm_after_post);
    let coord = Coordinator::new(producer, ORG.into(), authority.path().into()).unwrap();
    (authority, lock, coord)
}

/// Every backticked `cadence ...` command the message tells the operator to run.
fn named_commands(message: &str) -> Vec<Vec<String>> {
    message
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("cadence "))
        .map(|span| span.split_whitespace().map(str::to_owned).collect())
        .collect()
}

/// Parse with the real CLI definition: the bin built beside this test binary.
fn command_parses(words: &[String]) -> bool {
    let bin = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("cadence");
    assert!(
        bin.exists(),
        "build the cadence bin (cargo build --bin cadence) so named commands can be parsed: {}",
        bin.display()
    );
    std::process::Command::new(bin)
        .args(&words[1..])
        .arg("--help")
        .env_clear()
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn cad2096_confirmed_queue_names_remote_commit_when_local_record_fails() {
    let (_authority, lock, mut coord) = queue_fixture(true);
    let outcome = coord.queue(QUEUE_ISSUE, "dispatch", Some(QUEUE_TARGET));
    assert_eq!(coord.http.posts, 1);
    assert!(
        fs::symlink_metadata(&lock).unwrap().is_dir(),
        "real local write failure was armed"
    );
    let key = coord.http.key.clone().unwrap();
    // Repair only the fixture's lock path, then reconcile by read only.
    fs::remove_dir(&lock).unwrap();
    let status = coord.status().unwrap();
    assert_eq!(
        coord.http.posts, 1,
        "read reconciliation must not repeat the queue POST"
    );
    assert_eq!(status["assignments"][0]["assignment_id"], ID);
    assert_eq!(status["assignments"][0]["state"], "queued");
    assert_eq!(status["assignments"][0]["generation"], 1);

    let error = outcome
        .err()
        .expect("confirmed remote queue still has a local custody failure");
    assert_ne!(
        error.kind(),
        "unknown",
        "the correlated queue receipt was usable: {error}"
    );
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    for required in [ORG, ID, key.as_str()] {
        assert!(
            message.contains(required),
            "missing confirmed-effect identity {required:?}: {message}"
        );
    }
    assert!(lower.contains("confirmed"), "must affirm the remote queue: {message}");
    assert!(lower.contains("queue"), "must name the queue action: {message}");
    assert!(
        ["generation=1", "generation 1", "generation: 1"]
            .iter()
            .any(|field| lower.contains(field)),
        "missing generation identity: {message}"
    );
    assert!(
        lower.contains("local") && lower.contains("directory"),
        "preserve the real local error: {message}"
    );
    assert!(
        !lower.contains("unknown") && !lower.contains("nothing was"),
        "do not relabel a confirmed remote effect: {message}"
    );
    assert!(
        lower.contains("retry") || lower.contains("replay"),
        "{message}"
    );
    let commands = named_commands(&message);
    assert!(
        !commands.is_empty(),
        "must name a reconciliation command: {message}"
    );
    for words in &commands {
        assert!(
            command_parses(words),
            "named command does not parse in the real CLI: {words:?} in {message}"
        );
    }
}

#[test]
fn cad2096_queue_refused_before_dispatch_does_not_claim_confirmation() {
    // Refused by validation, before any ledger write or POST.
    let (_authority, _lock, mut coord) = queue_fixture(false);
    let error = coord
        .queue("not a hosted id", "dispatch", Some(QUEUE_TARGET))
        .err()
        .expect("invalid issue is refused");
    assert_eq!(coord.http.posts, 0);
    assert_eq!(error.kind(), "rejected");
    assert!(!error.to_string().to_ascii_lowercase().contains("confirmed"));

    // Refused because the local ledger cannot be admitted at all: still no POST.
    let (_authority, lock, mut coord) = queue_fixture(false);
    let _ = fs::remove_file(&lock);
    fs::create_dir(&lock).unwrap();
    let error = coord
        .queue(QUEUE_ISSUE, "dispatch", Some(QUEUE_TARGET))
        .err()
        .expect("unwritable ledger refuses before dispatch");
    assert_eq!(coord.http.posts, 0, "nothing may be sent");
    let lower = error.to_string().to_ascii_lowercase();
    assert!(!lower.contains("confirmed"), "{lower}");
    assert!(!lower.contains(ID), "{lower}");
}
