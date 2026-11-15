//! CAD-2215 signed-board FTS corpus isolation for private user wiki pages.
//! Synthetic wiki documents use real signed board writes and indexed reads.
use super::*;

fn put_fixture(rig: &Rig, cookie: &str, path: &str, text: &str) {
    let mut response = http_agent()
        .put(&format!(
            "http://127.0.0.1:{}/api/wiki/file",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Cookie", cookie)
        .header("Content-Type", "application/json")
        .send_json(json!({"path":path,"text":text}))
        .expect("wiki fixture write failed");
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap();
    assert_eq!(status, 200, "wiki fixture write {path} failed: {body}");
}
fn search(rig: &Rig, cookie: &str) -> Value {
    let mut response = http_agent()
        .get(&format!(
            "http://127.0.0.1:{}/api/wiki/search?q=corpuscanary",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header("Cookie", cookie)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .call()
        .expect("wiki search failed");
    let status = response.status().as_u16();
    let body: Value = response
        .body_mut()
        .read_json()
        .expect("wiki search not JSON");
    assert_eq!(status, 200, "wiki search refused: {body}");
    body
}
fn hit<'a>(body: &'a Value, path: &str) -> &'a Value {
    body["matches"]
        .as_array()
        .expect("search matches missing")
        .iter()
        .find(|row| row["path"].as_str() == Some(path))
        .unwrap_or_else(|| panic!("search omitted {path}: {body}"))
}

#[test]
fn another_users_private_matching_page_does_not_change_visible_search_scores() {
    let rig = start(&[]);
    let alice = login(&rig, "alice", "member");
    let bob = login(&rig, "bob", "member");
    let owner = login(&rig, "cad2215-search-owner", "owner");
    put_fixture(
        &rig,
        &owner,
        "global/corpus-canary.md",
        "# Visible\ncorpuscanary public document\n",
    );
    put_fixture(
        &rig,
        &owner,
        "users/alice/corpus-canary.md",
        "# Alice\ncorpuscanary alice private document\n",
    );
    let before = search(&rig, &alice);
    let global_before = hit(&before, "global/corpus-canary.md").clone();
    let own_before = hit(&before, "users/alice/corpus-canary.md").clone();
    assert!(
        global_before["score"].as_f64().is_some(),
        "global score missing: {global_before}"
    );
    assert!(
        own_before["score"].as_f64().is_some(),
        "own score missing: {own_before}"
    );
    // Adding a matching private document changes a shared FTS corpus's BM25
    // statistics even if the unauthorized row is filtered after scoring.
    put_fixture(
        &rig,
        &owner,
        "users/bob/corpus-canary.md",
        "# Bob\ncorpuscanary corpuscanary corpuscanary bob private document\n",
    );
    let bob_view = search(&rig, &bob);
    assert!(
        hit(&bob_view, "users/bob/corpus-canary.md")
            .to_string()
            .contains("bob private"),
        "Bob cannot find his indexed private page: {bob_view}"
    );
    let operator_view = search(&rig, &owner);
    assert!(
        hit(&operator_view, "users/bob/corpus-canary.md")
            .to_string()
            .contains("bob private"),
        "operator cannot recover Bob's indexed page: {operator_view}"
    );
    let after = search(&rig, &alice);
    assert_eq!(
        hit(&after, "global/corpus-canary.md"),
        &global_before,
        "Bob's private document changed Alice's global search hit or score"
    );
    assert_eq!(
        hit(&after, "users/alice/corpus-canary.md"),
        &own_before,
        "Bob's private document changed Alice's personal search hit or score"
    );
    assert!(
        !after["matches"].to_string().contains("users/bob/")
            && !after["matches"].to_string().contains("bob private"),
        "Alice saw Bob's private indexed page: {after}"
    );
    // Synthetic old v3 cache, built from actual v4 indexed rows: v3 placed
    // all users' documents in the global FTS table. Preserve the real current
    // vault-tree revision so schema mismatch, not an unrelated stale tree,
    // is what must force migration before the next signed search.
    let cache_path = rig._dir.path().join("wiki-search.sqlite3");
    let cache = rusqlite::Connection::open(cache_path).unwrap();
    cache.busy_timeout(Duration::from_secs(5)).unwrap();
    let revision_before: String = cache
        .query_row(
            "SELECT value FROM wiki_index_meta WHERE key='revision'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let tx = cache.unchecked_transaction().unwrap();
    for table in ["wiki_index_u_616c696365", "wiki_index_u_626f62"] {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "fresh cache omitted expected user FTS table {table}"
        );
        tx.execute_batch(&format!(
            "INSERT INTO wiki_index_global(path,title,heading,body,line,rev,source) \
             SELECT path,title,heading,body,line,rev,source FROM {table}; DROP TABLE {table};"
        ))
        .unwrap();
    }
    assert_eq!(
        tx.execute(
            "UPDATE wiki_index_meta SET value='wiki-fts-v3' WHERE key='schema'",
            []
        )
        .unwrap(),
        1
    );
    tx.commit().unwrap();
    let private_in_global: i64 = cache
        .query_row(
            "SELECT count(*) FROM wiki_index_global WHERE path='users/bob/corpus-canary.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        private_in_global > 0,
        "old-cache fixture lacks Bob's private indexed row"
    );
    let old_schema: String = cache
        .query_row(
            "SELECT value FROM wiki_index_meta WHERE key='schema'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_schema, "wiki-fts-v3");
    let old_revision: String = cache
        .query_row(
            "SELECT value FROM wiki_index_meta WHERE key='revision'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        old_revision, revision_before,
        "fixture accidentally invalidated the revision"
    );
    drop(cache);

    // The normal authenticated search must rebuild before returning any
    // score. Both previously visible hit rows, including BM25, remain stable.
    let migrated = search(&rig, &alice);
    assert_eq!(
        hit(&migrated, "global/corpus-canary.md"),
        &global_before,
        "v3 global corpus affected Alice's result or score"
    );
    assert_eq!(
        hit(&migrated, "users/alice/corpus-canary.md"),
        &own_before,
        "v3 personal corpus affected Alice's result or score"
    );
    assert!(!migrated["matches"].to_string().contains("users/bob/"));
    let cache = rusqlite::Connection::open(rig._dir.path().join("wiki-search.sqlite3")).unwrap();
    let new_schema: String = cache
        .query_row(
            "SELECT value FROM wiki_index_meta WHERE key='schema'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(new_schema, "wiki-fts-v3", "search retained the v3 cache");
    let private_in_global: i64 = cache
        .query_row(
            "SELECT count(*) FROM wiki_index_global WHERE path='users/bob/corpus-canary.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        private_in_global, 0,
        "v3 private row was not removed from the global corpus"
    );
}
