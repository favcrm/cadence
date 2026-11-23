//! CAD-2018: only a connection-proven operator may inspect wiki trash.
//! Actor labels and claimed sessions cannot turn an agent into an operator.

use super::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};

#[test]
fn trash_reads_refuse_unproven_and_forged_callers_at_the_daemon_guard() {
    let dir = tempfile::Builder::new().prefix("c2018").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = crate::daemon::ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "worker",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: &cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let pid = std::process::id();
    let calls: [(&str, Value); 2] = [
        (crate::wiki::trash::LIST_RPC, json!({})),
        (
            crate::wiki::trash::ITEMS_RPC,
            json!({"from": ".trash/1700000000-aaaaaaaa/global/secret.md"}),
        ),
    ];
    for (method, params) in calls {
        for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
            let error = scoped(who.clone(), || shared.dispatch(method, &params, pid))
                .expect_err("an unproven caller must not inspect trash")
                .to_string();
            assert!(
                error.contains("operator action") || error.contains("not provably the operator"),
                "{method}: {who:?} was not stopped by the operator guard: {error}"
            );
        }
    }

    let listed = scoped(Asserted::Operator, || {
        shared.dispatch(crate::wiki::trash::LIST_RPC, &json!({}), pid)
    })
    .expect("a proven operator may inspect the empty trash");
    assert_eq!(listed["landings"], json!([]));

    for field in [
        json!({"actor": "operator"}),
        json!({"wiki_as": "operator"}),
        json!({"public_session": {"role": "operator"}}),
    ] {
        let result = scoped(Asserted::Operator, || {
            shared.dispatch(crate::wiki::trash::LIST_RPC, &field, pid)
        });
        assert!(result.is_err(), "identity claims must be refused: {field}");
    }
}

#[test]
fn trash_cursor_cannot_skip_items_in_another_landing_of_the_same_folder() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-cursor")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    for name in ["a", "b", "c"] {
        crate::wiki::write(
            &pm,
            &operator,
            &format!("global/many/{name}.md"),
            name,
            None,
        )
        .unwrap();
    }
    let first = crate::wiki::rm(&pm, &operator, "global/many").unwrap();
    let first_from = first["trash"].as_str().unwrap();
    let page = crate::wiki::trash::items(&pm, &operator, first_from, Some(1), None).unwrap();
    let cursor = page["next_cursor"]
        .as_str()
        .expect("the first landing has another page");

    for name in ["a", "b"] {
        crate::wiki::write(
            &pm,
            &operator,
            &format!("global/many/{name}.md"),
            name,
            None,
        )
        .unwrap();
    }
    let second = crate::wiki::rm(&pm, &operator, "global/many").unwrap();
    let second_from = second["trash"].as_str().unwrap();
    assert!(
        crate::wiki::trash::items(&pm, &operator, second_from, Some(1), Some(cursor)).is_err(),
        "a cursor from the first deletion must not hide entries in the second"
    );
    let first_page = crate::wiki::trash::items(&pm, &operator, second_from, Some(1), None).unwrap();
    assert_eq!(first_page["entries"][0]["path"], "global/many/a.md");
}

#[test]
fn partial_legacy_restore_keeps_the_remaining_landing_discoverable() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-partial")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    for name in ["a", "b"] {
        crate::wiki::write(
            &pm,
            &operator,
            &format!("global/manual/{name}.md"),
            name,
            None,
        )
        .unwrap();
    }
    let removed = crate::wiki::rm(&pm, &operator, "global/manual").unwrap();
    let from = removed["trash"].as_str().unwrap();
    crate::wiki::mv(
        &pm,
        &operator,
        &format!("{from}/a.md"),
        "global/recovered-a.md",
    )
    .unwrap();

    let listed = crate::wiki::trash::list(&pm, &operator, None, None).unwrap();
    assert!(
        listed["landings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["from"] == from),
        "a descendant restore must not hide the remaining landing"
    );
    let items = crate::wiki::trash::items(&pm, &operator, from, None, None).unwrap();
    assert_eq!(items["entries"][0]["path"], "global/manual/b.md");
    crate::wiki::mv(&pm, &operator, from, "global/recovered-manual").unwrap();
    assert_eq!(
        crate::wiki::read(&pm, &operator, "global/recovered-manual/b.md").unwrap()["text"],
        "b"
    );
}

#[test]
fn a_legal_delimiter_in_the_original_path_does_not_hide_a_landing() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-delimiter")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    let path = "global/a → .trash/1-00000000/x.md";
    crate::wiki::write(&pm, &operator, path, "synthetic", None).unwrap();
    let removed = crate::wiki::rm(&pm, &operator, path).unwrap();
    let from = removed["trash"].as_str().unwrap();
    let listed = crate::wiki::trash::list(&pm, &operator, None, None).unwrap();
    assert!(listed["landings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["from"] == from));
    let items = crate::wiki::trash::items(&pm, &operator, from, None, None).unwrap();
    assert_eq!(items["entries"][0]["path"], path);
}

#[cfg(unix)]
#[test]
fn restore_refuses_incomplete_pointers_collisions_and_a_symlinked_blob_root() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::Builder::new()
        .prefix("c2018-unsafe")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    crate::wiki::write(&pm, &operator, "global/safety/a.md", "safe", None).unwrap();
    let removed = crate::wiki::rm(&pm, &operator, "global/safety").unwrap();
    let from = removed["trash"].as_str().unwrap();
    let vault = crate::wiki::vault_dir(&pm).unwrap();
    let landing = vault.join(from);
    let blobs = crate::wiki::blobs_dir(&vault);
    let sha = "a".repeat(64);
    std::fs::write(blobs.join(&sha), b"x").unwrap();
    let pointer = landing.join("attachment.blob");

    std::fs::write(&pointer, json!({"sha256": sha, "size": 1}).to_string()).unwrap();
    assert!(crate::wiki::mv(&pm, &operator, from, "global/recovered").is_err());
    assert!(
        landing.exists(),
        "a refused restore must leave the landing intact"
    );

    std::fs::write(
        &pointer,
        json!({"sha256": sha, "size": 1, "mime": "application/octet-stream", "name": "attachment"})
            .to_string(),
    )
    .unwrap();
    std::fs::write(landing.join("attachment"), b"conflicting text").unwrap();
    assert!(crate::wiki::mv(&pm, &operator, from, "global/recovered").is_err());
    std::fs::remove_file(landing.join("attachment")).unwrap();

    let outside = dir.path().join("outside");
    std::fs::rename(&blobs, &outside).unwrap();
    symlink(&outside, &blobs).unwrap();
    assert!(crate::wiki::mv(&pm, &operator, from, "global/recovered").is_err());
    assert!(
        landing.exists(),
        "an unconfined blob store must not consume the landing"
    );
}

#[test]
fn public_owner_can_restore_but_members_and_forged_sessions_cannot() {
    use crate::operator_auth::{BoardUser, PublicRealm};

    let dir = tempfile::Builder::new()
        .prefix("c2018-owner")
        .tempdir()
        .unwrap();
    let pm_path = dir.path().join("pm");
    let pm = crate::issue::Pm::init(&pm_path).unwrap();
    let realm = PublicRealm {
        host: "wiki.board.localhost:3111".to_string(),
        issuer: "http://127.0.0.1:3999".to_string(),
        company: "synthetic-company".to_string(),
    };
    crate::board_identity::write_config(
        dir.path(),
        &crate::board_identity::Config {
            host: realm.host.clone(),
            issuer: realm.issuer.clone(),
            company: realm.company.clone(),
        },
    )
    .unwrap();
    let opts = crate::daemon::ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_path.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let now = shared.operator_now();
    let open = |handle: &str, role: &str| {
        shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: format!("subject-{handle}"),
                    email: format!("{handle}@example.test"),
                    name: format!("Synthetic {handle}"),
                    role: role.to_string(),
                    handle: handle.to_string(),
                },
                &format!("jti-{handle}"),
                now + 60,
                "synthetic-test",
                realm.clone(),
                now,
            )
            .unwrap()
            .unwrap()
            .token
    };
    let owner = open("owner", "operator");
    let member = open("member", "member");
    let operator = crate::wiki::Caller::Operator;
    crate::wiki::write(&pm, &operator, "global/owned.md", "retained", None).unwrap();
    let removed = crate::wiki::rm(&pm, &operator, "global/owned.md").unwrap();
    let from = removed["trash"].as_str().unwrap();
    let pid = std::process::id();

    for params in [
        json!({"from": from, "to": "global/recovered.md", "wiki_as": "user:member", "public_session": member.as_str()}),
        json!({"from": from, "to": "global/recovered.md", "wiki_as": "operator", "public_session": member.as_str()}),
        json!({"from": from, "to": "global/recovered.md", "wiki_as": "user:other", "public_session": owner.as_str()}),
        json!({"from": from, "to": "global/recovered.md", "wiki_as": "user:member", "public_session": member.as_str(), "role": "operator"}),
    ] {
        assert!(
            scoped(Asserted::Operator, || shared
                .dispatch("wiki_mv", &params, pid))
            .is_err(),
            "a member or forged actor must not restore"
        );
    }
    assert!(crate::wiki::read(&pm, &operator, "global/recovered.md").is_err());

    let result = scoped(Asserted::Operator, || {
        shared.dispatch(
            "wiki_mv",
            &json!({"from": from, "to": "global/recovered.md", "wiki_as": "user:owner", "public_session": owner}),
            pid,
        )
    })
    .expect("a verified public owner may restore");
    assert_eq!(result["moved"], true);
    assert_eq!(
        crate::wiki::read(&pm, &operator, "global/recovered.md").unwrap()["text"],
        "retained"
    );
}

#[test]
fn restore_preserves_the_full_logical_path_budget() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-limit")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    let mut segments = vec!["global".to_string()];
    segments.extend((0..30).map(|_| "d".repeat(64)));
    segments.push("f".repeat(91));
    let path = segments.join("/");
    assert_eq!(segments.len(), 32);
    assert_eq!(path.len(), 2048);
    crate::wiki::write(&pm, &operator, &path, "boundary", None).unwrap();
    let removed = crate::wiki::rm(&pm, &operator, &path).unwrap();
    let from = removed["trash"].as_str().unwrap();
    let restored = crate::wiki::mv(&pm, &operator, from, &path).unwrap();
    assert_eq!(restored["moved"], true);
    assert_eq!(
        crate::wiki::read(&pm, &operator, &path).unwrap()["text"],
        "boundary"
    );
}

#[test]
fn directory_restore_rejects_destinations_that_make_children_unaddressable() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-destination")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    crate::wiki::write(&pm, &operator, "global/manual/a.md", "retained", None).unwrap();
    let removed = crate::wiki::rm(&pm, &operator, "global/manual").unwrap();
    let from = removed["trash"].as_str().unwrap();

    let mut deep = vec!["global".to_string()];
    deep.extend((0..31).map(|_| "d".to_string()));
    let deep = deep.join("/");
    let mut long = vec!["global".to_string()];
    long.extend((0..30).map(|_| "d".repeat(64)));
    long.push("f".repeat(91));
    let long = long.join("/");
    assert_eq!(deep.split('/').count(), 32);
    assert_eq!(long.len(), 2048);

    for destination in [&deep, &long] {
        assert!(
            crate::wiki::mv(&pm, &operator, from, destination).is_err(),
            "a valid root must not make its child exceed the wiki path budget"
        );
    }
    let listed = crate::wiki::trash::items(&pm, &operator, from, None, None).unwrap();
    assert_eq!(listed["entries"][0]["path"], "global/manual/a.md");
    let mut boundary = vec!["global".to_string()];
    boundary.extend((0..29).map(|_| "d".repeat(64)));
    boundary.push("f".repeat(151));
    let boundary = boundary.join("/");
    assert_eq!(boundary.len() + "/a.md".len(), 2048);
    crate::wiki::mv(&pm, &operator, from, &boundary).unwrap();
    assert_eq!(
        crate::wiki::read(&pm, &operator, &format!("{boundary}/a.md")).unwrap()["text"],
        "retained"
    );
}

#[test]
fn large_empty_directory_tree_has_unknown_totals_but_remains_browsable() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-wide")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    crate::wiki::write(&pm, &operator, "global/wide/page.md", "kept", None).unwrap();
    let removed = crate::wiki::rm(&pm, &operator, "global/wide").unwrap();
    let from = removed["trash"].as_str().unwrap();
    let landing = crate::wiki::vault_dir(&pm).unwrap().join(from);
    for index in 0..10_001 {
        let child = landing.join(format!("empty-{index:05}"));
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join(".gitkeep"), "").unwrap();
    }

    let listed = crate::wiki::trash::list(&pm, &operator, None, None).unwrap();
    let row = listed["landings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["from"] == from)
        .unwrap();
    assert!(
        row["item_count"].is_null(),
        "a capped walk cannot claim an exact total"
    );
    assert!(
        row["size"].is_null(),
        "a capped walk cannot claim an exact byte count"
    );
    assert_eq!(row["restorable"], true, "large does not mean unsafe");
    let first = crate::wiki::trash::items(&pm, &operator, from, Some(1), None).unwrap();
    let cursor = first["next_cursor"]
        .as_str()
        .expect("large directory has another page");
    let second = crate::wiki::trash::items(&pm, &operator, from, Some(1), Some(cursor)).unwrap();
    assert_ne!(first["entries"][0]["path"], second["entries"][0]["path"]);
}

#[test]
fn old_partial_restore_keeps_a_real_empty_folder_visible() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let dir = tempfile::Builder::new()
        .prefix("c2018-history")
        .tempdir()
        .unwrap();
    let pm_path = dir.path().join("pm");
    let pm = crate::issue::Pm::init(&pm_path).unwrap();
    let operator = crate::wiki::Caller::Operator;
    crate::wiki::mkdir(&pm, &operator, "global/manual/empty").unwrap();
    let vault = crate::wiki::vault_dir(&pm).unwrap();
    for index in 0..513 {
        std::fs::write(vault.join(format!("global/manual/{index:03}.md")), "x").unwrap();
    }
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&pm_path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    git(&["add", "wiki/global/manual"]);
    git(&[
        "-c",
        "user.name=Wiki Test",
        "-c",
        "user.email=wiki@example.test",
        "commit",
        "-m",
        "seed files",
    ]);
    let removed = crate::wiki::rm(&pm, &operator, "global/manual").unwrap();
    let from = removed["trash"].as_str().unwrap();
    let branch = git(&["symbolic-ref", "--short", "HEAD"]);
    let branch = std::str::from_utf8(&branch.stdout).unwrap().trim();
    let head = git(&["rev-parse", "HEAD"]);
    let head = std::str::from_utf8(&head.stdout).unwrap().trim();

    // Build real Git rename history in one process, rather than starting 513
    // git processes. Each commit removes one real tracked landing file and
    // records the same subject as the legacy CLI's descendant move.
    let mut importer = Command::new("git")
        .arg("-C")
        .arg(&pm_path)
        .arg("fast-import")
        .arg("--quiet")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let input = importer.stdin.as_mut().unwrap();
        for index in 0..513 {
            let source = format!("{from}/{index:03}.md");
            let destination = format!("global/recovered-{index:03}.md");
            let subject = format!("wiki: mv {source} → {destination}");
            write!(
                input,
                "commit refs/heads/{branch}\ncommitter Wiki Test <wiki@example.test> 1700000000 +0000\ndata {}\n{subject}\n",
                subject.len(),
            )
            .unwrap();
            if index == 0 {
                writeln!(input, "from {head}").unwrap();
            }
            write!(input, "R wiki/{source} wiki/{destination}\n\n").unwrap();
            std::fs::rename(vault.join(&source), vault.join(&destination)).unwrap();
        }
    }
    drop(importer.stdin.take());
    assert!(
        importer.wait().unwrap().success(),
        "fixture history import failed"
    );
    git(&["read-tree", "HEAD"]);

    let listed = crate::wiki::trash::list(&pm, &operator, None, None).unwrap();
    assert!(
        listed["landings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["from"] == from),
        "a history page of descendant moves must not hide the remaining empty folder"
    );
    let items = crate::wiki::trash::items(&pm, &operator, from, None, None).unwrap();
    assert_eq!(items["entries"][0]["path"], "global/manual/empty");
    crate::wiki::mv(&pm, &operator, from, "global/recovered-manual").unwrap();
    assert!(vault
        .join("global/recovered-manual/empty/.gitkeep")
        .is_file());
}

#[test]
fn trash_items_paginate_nested_directories_without_skips_or_duplicates() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-nested")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    for path in [
        "global/manual/a/1.md",
        "global/manual/a/sub/2.md",
        "global/manual/b/3.md",
        "global/manual/c.md",
    ] {
        crate::wiki::write(&pm, &operator, path, "x", None).unwrap();
    }
    crate::wiki::mkdir(&pm, &operator, "global/manual/empty").unwrap();
    let removed = crate::wiki::rm(&pm, &operator, "global/manual").unwrap();
    let from = removed["trash"].as_str().unwrap();
    let mut cursor = None;
    let mut paths = Vec::new();
    for _ in 0..12 {
        let page =
            crate::wiki::trash::items(&pm, &operator, from, Some(1), cursor.as_deref()).unwrap();
        for row in page["entries"].as_array().unwrap() {
            paths.push(row["path"].as_str().unwrap().to_string());
        }
        cursor = page["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none(), "pagination must finish");
    assert_eq!(
        paths,
        [
            "global/manual/a",
            "global/manual/a/1.md",
            "global/manual/a/sub",
            "global/manual/a/sub/2.md",
            "global/manual/b",
            "global/manual/b/3.md",
            "global/manual/c.md",
            "global/manual/empty",
        ]
    );
}

#[test]
fn restore_and_trash_listing_accept_legacy_long_names() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-name")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    crate::wiki::mkdir(&pm, &operator, "global/manual").unwrap();
    let vault = crate::wiki::vault_dir(&pm).unwrap();
    let mut names = Vec::new();
    for length in [250, 251, 255] {
        let name = format!("{}.md", "f".repeat(length - 3));
        let folder = "d".repeat(length);
        std::fs::write(vault.join("global/manual").join(&name), "long name").unwrap();
        let empty = vault.join("global/manual").join(&folder);
        std::fs::create_dir(&empty).unwrap();
        std::fs::write(empty.join(".gitkeep"), "").unwrap();
        names.push((name, folder));
    }
    let removed = crate::wiki::rm(&pm, &operator, "global/manual").unwrap();
    let from = removed["trash"].as_str().unwrap();
    let items = crate::wiki::trash::items(&pm, &operator, from, None, None).unwrap();
    let entries = items["entries"].as_array().unwrap();
    for (name, folder) in &names {
        assert!(entries
            .iter()
            .any(|entry| entry["path"] == format!("global/manual/{name}")));
        assert!(entries
            .iter()
            .any(|entry| entry["path"] == format!("global/manual/{folder}")));
    }
    crate::wiki::mv(&pm, &operator, from, "global/restored").unwrap();
    for (name, folder) in names {
        assert_eq!(
            crate::wiki::read(&pm, &operator, &format!("global/restored/{name}")).unwrap()["text"],
            "long name"
        );
        assert!(vault.join("global/restored").join(folder).is_dir());
    }
}

#[test]
fn trash_landings_paginate_newest_first_without_duplicates() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-list")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    let mut expected = Vec::new();
    for name in ["a", "b", "c"] {
        let path = format!("global/{name}.md");
        crate::wiki::write(&pm, &operator, &path, name, None).unwrap();
        let removed = crate::wiki::rm(&pm, &operator, &path).unwrap();
        expected.push(
            removed["trash"]
                .as_str()
                .unwrap()
                .split('/')
                .nth(1)
                .unwrap()
                .to_string(),
        );
    }
    expected.sort_by_key(|key| {
        let (stamp, nonce) = key.split_once('-').unwrap();
        std::cmp::Reverse((stamp.parse::<u64>().unwrap(), nonce.to_string()))
    });
    let mut cursor = None;
    let mut actual = Vec::new();
    for _ in 0..4 {
        let page = crate::wiki::trash::list(&pm, &operator, Some(1), cursor.as_deref()).unwrap();
        for landing in page["landings"].as_array().unwrap() {
            actual.push(landing["landing_id"].as_str().unwrap().to_string());
        }
        cursor = page["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none(), "pagination must finish");
    assert_eq!(actual, expected);
}

#[test]
fn trash_cursor_keeps_distinct_legacy_keys_with_the_same_numeric_stamp() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-legacy-keys")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    let trash = crate::wiki::vault_dir(&pm).unwrap().join(".trash");
    for key in ["1-00000000", "01-00000000"] {
        let folder = trash.join(key).join("global");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("page.md"), "legacy").unwrap();
    }
    let first = crate::wiki::trash::list(&pm, &operator, Some(1), None).unwrap();
    assert_eq!(first["landings"][0]["landing_id"], "1-00000000");
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a second legacy landing exists");
    let second = crate::wiki::trash::list(&pm, &operator, Some(1), Some(cursor)).unwrap();
    assert_eq!(second["landings"][0]["landing_id"], "01-00000000");
    assert!(second["next_cursor"].is_null());
}

#[test]
fn trash_endpoints_refuse_malformed_and_wrong_kind_cursors() {
    let dir = tempfile::Builder::new()
        .prefix("c2018-invalid-cursor")
        .tempdir()
        .unwrap();
    let pm = crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
    let operator = crate::wiki::Caller::Operator;
    let mut first_from = None;
    for name in ["a", "b"] {
        let path = format!("global/{name}.md");
        crate::wiki::write(&pm, &operator, &path, name, None).unwrap();
        let deleted = crate::wiki::rm(&pm, &operator, &path).unwrap();
        if first_from.is_none() {
            first_from = deleted["trash"].as_str().map(str::to_owned);
        }
    }
    let from = first_from.as_deref().unwrap();
    let page = crate::wiki::trash::items(&pm, &operator, from, Some(1), None).unwrap();
    assert_eq!(page["entries"][0]["path"], "global/a.md");

    let too_long = "a".repeat(5000);
    for invalid in ["", "!!!", too_long.as_str()] {
        assert!(crate::wiki::trash::list(&pm, &operator, Some(1), Some(invalid)).is_err());
        assert!(crate::wiki::trash::items(&pm, &operator, from, Some(1), Some(invalid)).is_err());
    }
    let list_page = crate::wiki::trash::list(&pm, &operator, Some(1), None).unwrap();
    let list_cursor = list_page["next_cursor"]
        .as_str()
        .expect("two landings have two pages");
    assert!(crate::wiki::trash::items(&pm, &operator, from, Some(1), Some(list_cursor)).is_err());
}
