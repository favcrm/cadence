// Mutation caught: replace the queue stamp error mapping through
// assignment_custody_failed with a bare `?` after `self.with_ledger(...)`.

const CAD2096_AUTHORED_ISSUE: &str = "SYN-1";
const CAD2096_AUTHORED_TARGET: &str = "user_target";

struct Cad2096AuthoredTransport {
    lock_path: PathBuf,
    posts: usize,
    sent_key: Option<String>,
}

impl CoordHttp for Cad2096AuthoredTransport {
    fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        assert_eq!(method, "POST", "unexpected request during queue");
        assert_eq!(path, assignment_prefix(ORG));
        self.posts += 1;
        assert_eq!(self.posts, 1, "the hosted queue POST was replayed");
        let request = body.expect("queue request body");
        assert_eq!(request["issue_id"], CAD2096_AUTHORED_ISSUE);
        assert_eq!(request["target"], CAD2096_AUTHORED_TARGET);
        assert_eq!(request["action"], "dispatch");
        let key = request["idempotency_key"].as_str().expect("durable key");
        assert!(!key.is_empty());
        self.sent_key = Some(key.to_owned());

        let mut receipt = original();
        receipt["state"] = json!("queued");
        receipt["claim_id"] = Value::Null;
        receipt["client_agent_id"] = Value::Null;
        // Replace the ledger lock after the remote commit, producing a real
        // filesystem error when the coordinator next admits the ledger.
        fs::remove_file(&self.lock_path).unwrap();
        fs::create_dir(&self.lock_path).unwrap();
        Ok(Reply { status: 200, body: receipt })
    }
}

#[test]
fn cad2096_authored_confirmed_receipt_survives_failed_ledger_lock() {
    let authority = tempfile::tempdir().unwrap();
    fs::set_permissions(authority.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let lock_path = authority.path().join(".cli-local-assign.lock");
    let transport = Cad2096AuthoredTransport {
        lock_path: lock_path.clone(),
        posts: 0,
        sent_key: None,
    };
    let mut coordinator = Coordinator::new(transport, ORG.into(), authority.path().into()).unwrap();

    let confirmed_error = coordinator
        .queue(CAD2096_AUTHORED_ISSUE, "dispatch", Some(CAD2096_AUTHORED_TARGET))
        .err()
        .expect("the local custody update must fail");
    assert_eq!(coordinator.http.posts, 1);
    assert!(fs::symlink_metadata(&lock_path).unwrap().is_dir());
    let sent_key = coordinator.http.sent_key.clone().expect("POST captured its key");
    assert_eq!(confirmed_error.kind(), "internal");
    let message = confirmed_error.to_string();
    let lower = message.to_ascii_lowercase();
    assert!(lower.contains("confirmed") && lower.contains("queue"), "{message}");
    assert!(!lower.contains("unknown") && !lower.contains("nothing was"), "{message}");
    assert!(message.contains(ORG) && message.contains(ID), "{message}");
    assert!(message.contains("generation=1"), "{message}");
    assert!(message.contains(&format!("idempotency_key={sent_key}")), "{message}");
    assert_eq!(message.matches(&sent_key).count(), 1, "{message}");
    assert!(lower.contains("local") && lower.contains("directory"), "{message}");

    // The same real fault refuses an identical invocation before dispatch;
    // that refusal must not inherit the prior call confirmation.
    let refused = coordinator
        .queue(CAD2096_AUTHORED_ISSUE, "dispatch", Some(CAD2096_AUTHORED_TARGET))
        .err()
        .expect("the broken ledger lock must refuse a new dispatch");
    assert_eq!(coordinator.http.posts, 1, "local refusal must not replay the POST");
    let refusal = refused.to_string();
    assert!(!refusal.to_ascii_lowercase().contains("confirmed"), "{refusal}");
    assert!(message.contains(&refusal), "confirmed error lost the local failure: {message}; refusal: {refusal}");
}
