// Independent CAD-2009 acceptance: a known remote result survives local admission failure.
// Included inside cad2009_assignment_acceptance; all paths are disposable fixture paths.
struct ConfirmedThenLocalAdmissionFailure {
    lock: PathBuf,
    committed: Value,
    reports: usize,
}

impl CoordHttp for ConfirmedThenLocalAdmissionFailure {
    fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        if method == "GET" {
            assert_eq!(path, assignment_prefix(ORG));
            assert!(body.is_none());
            return Ok(Reply {
                status: 200,
                body: json!({
                    "version": VERSION, "assignments": [self.committed.clone()],
                }),
            });
        }
        assert_eq!(method, "POST");
        assert_eq!(path, format!("{}/{ID}/report", assignment_prefix(ORG)));
        self.reports += 1;
        assert_eq!(self.reports, 1, "confirmed report was replayed");
        assert_eq!(
            body.unwrap(),
            &json!({
                "version": VERSION, "organization_id": ORG, "expected_generation": 1,
                "claim_id": CLAIM, "state": "result", "text": TEXT, "reported_head_sha": SHA,
            })
        );
        self.committed["state"] = json!("result");
        self.committed["text"] = json!(TEXT);
        self.committed["reported_head_sha"] = json!(SHA);
        // Commit and return a complete correlated receipt, but make the next real
        // private_lock admission fail with EISDIR. No chmod, euid assumption or mock error.
        fs::remove_file(&self.lock).unwrap();
        fs::create_dir(&self.lock).unwrap();
        Ok(Reply {
            status: 200,
            body: self.committed.clone(),
        })
    }
}

#[test]
fn cad2009_confirmed_result_names_remote_commit_when_local_admission_fails() {
    let authority = tempfile::tempdir().unwrap();
    fs::set_permissions(authority.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let lock = authority.path().join(".cli-local-assign.lock");
    let producer = ConfirmedThenLocalAdmissionFailure {
        lock: lock.clone(),
        committed: original(),
        reports: 0,
    };
    let mut coord = Coordinator::new(producer, ORG.into(), authority.path().into()).unwrap();
    let assignment = parse_assignment(&original()).unwrap();
    coord.remember_discovered(&assignment).unwrap();
    coord.reserve_kickoff(&assignment).unwrap();
    let outcome = coord.report(ID, "result", Some(TEXT), Some(SHA), None);
    assert_eq!(coord.http.reports, 1);
    assert!(
        fs::symlink_metadata(&lock).unwrap().is_dir(),
        "real admission failure was armed"
    );
    // Repair only our fixture's lock path to perform a read, never a report replay.
    fs::remove_dir(&lock).unwrap();
    let status = coord.status().unwrap();
    assert_eq!(
        coord.http.reports, 1,
        "read reconciliation must not repeat the report"
    );
    assert_eq!(status["assignments"][0]["state"], "result");
    assert_eq!(status["assignments"][0]["claim_id"], CLAIM);
    assert_eq!(status["assignments"][0]["generation"], 1);
    assert_eq!(status["assignments"][0]["reported_head_sha"], SHA);
    let error = outcome.expect_err("confirmed remote result still has a local custody failure");
    assert_ne!(
        error.kind(),
        "unknown",
        "the correlated result receipt was usable: {error}"
    );
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    for required in [ORG, ID, CLAIM, "result"] {
        assert!(
            message.contains(required),
            "missing confirmed-effect identity {required:?}: {message}"
        );
    }
    assert!(
        lower.contains("confirmed"),
        "must affirm the remote effect: {message}"
    );
    assert!(
        lower.contains("local") && lower.contains("directory"),
        "preserve the real local directory/lock admission error: {message}"
    );
    assert!(
        ["generation=1", "generation 1", "generation: 1"]
            .iter()
            .any(|field| lower.contains(field)),
        "missing generation identity: {message}"
    );
    assert!(
        lower.contains("inspect") || lower.contains("reconcil"),
        "{message}"
    );
    assert!(
        lower.contains("retry") || lower.contains("replay"),
        "{message}"
    );
    assert!(
        !lower.contains("unknown"),
        "do not relabel a confirmed remote effect: {message}"
    );
    assert!(!lower.contains("nothing was recorded"), "{message}");
}
