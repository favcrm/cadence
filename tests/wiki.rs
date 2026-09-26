//! CAD-580: the wiki v1 store — daemon-RPC adversarial tests.
//!
//! Every guard gets its mutation proof here: an agent caller writing
//! outside its areas, a forged identity field, an unproven detached
//! child, a traversal string, an oversize upload, a stale `if_rev`.
//! The allowlist table itself is unit-tested in `src/wiki/mod.rs`.
//!
//! Agent callers are seam-asserted (`d.agent_rpc`) so the suite reads
//! the same in an agent pane and in CI; those tests need the
//! `test-seam` build and are `#[cfg]`-gated like the seam's own suite.

#![allow(clippy::disallowed_methods)]
// The daemon tests need common::TestDaemon's seam RPCs and the board
// tests need board_common's UI fixtures — both harnesses `#[path]`-load
// support/operator.rs, so in this one binary it is a module twice.
#![allow(clippy::duplicate_mod)]
mod board_common;
mod common;

use std::path::PathBuf;

use board_common::op;
use common::*;
use serde_json::{json, Value};
use tempfile::TempDir;

/// A tracker dir + a registered project + a live fixture daemon.
struct Fx {
    _root: TempDir,
    d: TestDaemon,
    pm: PathBuf,
    /// A git checkout the fixture project claims as its repo — an
    /// agent registered with it as `cwd` owns `projects/cadence/`.
    /// Read only by the seam-gated agent tests.
    #[allow(dead_code)]
    repo: PathBuf,
}

fn fx() -> Fx {
    fx_with_cap(None)
}

fn fx_with_cap(cap: Option<u64>) -> Fx {
    let root = TempDir::new().unwrap();
    let pm_dir = root.path().join("pm");
    cadence_agent::issue::Pm::init(&pm_dir).unwrap();
    if let Some(cap) = cap {
        // `Pm::init` already writes a `wiki:` section — set the cap on
        // the generated file, never a second `wiki:` key.
        let file = pm_dir.join("pm.yaml");
        let text = std::fs::read_to_string(&file).unwrap().replace(
            "max_upload_bytes: 104857600",
            &format!("max_upload_bytes: {cap}"),
        );
        assert!(
            text.contains(&format!("max_upload_bytes: {cap}")),
            "pm.yaml edit landed"
        );
        std::fs::write(&file, text).unwrap();
    }
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let init = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"])
        .output()
        .unwrap();
    assert!(init.status.success());
    std::fs::create_dir_all(pm_dir.join("cadence")).unwrap();
    std::fs::write(
        pm_dir.join("cadence/project.yaml"),
        format!(
            "key: cadence\nprefix: CAD\nrepos:\n- path: {}\n",
            repo.display()
        ),
    )
    .unwrap();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start();
    Fx {
        _root: root,
        d,
        pm: pm_dir,
        repo,
    }
}

fn vault(fx: &Fx) -> PathBuf {
    fx.pm.join("wiki")
}

/// The refusal text of a call that must not succeed.
fn refused(r: cadence_agent::Result<Value>) -> String {
    match r {
        Ok(v) => panic!("call succeeded where a refusal was required: {v}"),
        Err(e) => e.to_string(),
    }
}

fn write_op(d: &TestDaemon, path: &str, text: &str) -> Value {
    d.operator_rpc("wiki_write", json!({"path": path, "text": text}))
        .unwrap_or_else(|e| panic!("operator wiki_write {path}: {e}"))
}

fn read_op(d: &TestDaemon, path: &str) -> Value {
    d.operator_rpc("wiki_read", json!({"path": path}))
        .unwrap_or_else(|e| panic!("operator wiki_read {path}: {e}"))
}

fn read_op_err(d: &TestDaemon, path: &str) -> cadence_agent::Result<Value> {
    d.operator_rpc("wiki_read", json!({"path": path}))
}

#[test]
fn operator_roundtrip_write_read_ls_mv_rm_restore_history() {
    let fx = fx();
    let d = &fx.d;

    // mkdir + write + read + ls
    let out = d
        .operator_rpc("wiki_mkdir", json!({"path": "global/runbooks"}))
        .unwrap();
    assert_eq!(out["created"], true);
    let out = write_op(d, "global/runbooks/deploy.md", "deploy notes v1\n");
    let rev = out["rev"].as_str().unwrap().to_string();
    assert!(rev.starts_with("fnv1a:"), "rev: {rev}");
    let page = read_op(d, "global/runbooks/deploy.md");
    assert_eq!(page["kind"], "text");
    assert_eq!(page["text"], "deploy notes v1\n");
    assert_eq!(page["rev"], rev);
    let ls = d
        .operator_rpc("wiki_ls", json!({"path": "global/runbooks"}))
        .unwrap();
    assert_eq!(ls["entries"][0]["name"], "deploy.md");
    assert_eq!(ls["entries"][0]["kind"], "text");
    // the layout dirs the ensure step makes never leak into ls
    assert_eq!(
        ls["entries"].as_array().unwrap().len(),
        1,
        "hidden dirs (.trash/.blobs/.gitkeep) never list: {ls}"
    );

    // one commit per write, with an Actor trailer
    let hist = d
        .operator_rpc("wiki_history", json!({"path": "global/runbooks/deploy.md"}))
        .unwrap();
    assert_eq!(hist["commits"].as_array().unwrap().len(), 1);
    assert_eq!(hist["commits"][0]["actor"], "operator");
    assert!(hist["commits"][0]["subject"]
        .as_str()
        .unwrap()
        .contains("wiki: write global/runbooks/deploy.md"));

    // mv
    d.operator_rpc(
        "wiki_mv",
        json!({"from": "global/runbooks/deploy.md", "to": "global/runbooks/ship.md"}),
    )
    .unwrap();
    assert!(read_op_err(d, "global/runbooks/deploy.md").is_err());
    assert_eq!(
        read_op(d, "global/runbooks/ship.md")["text"],
        "deploy notes v1\n"
    );

    // rm → .trash, and the restore path mv's back
    let out = d
        .operator_rpc("wiki_rm", json!({"path": "global/runbooks/ship.md"}))
        .unwrap();
    let trash = out["trash"].as_str().unwrap().to_string();
    assert!(trash.starts_with(".trash/"), "{trash}");
    assert!(read_op_err(d, "global/runbooks/ship.md").is_err());
    d.operator_rpc(
        "wiki_mv",
        json!({"from": trash, "to": "global/runbooks/ship.md"}),
    )
    .unwrap();
    assert_eq!(
        read_op(d, "global/runbooks/ship.md")["text"],
        "deploy notes v1\n"
    );

    // search finds it
    let found = d
        .operator_rpc("wiki_search", json!({"q": "deploy notes"}))
        .unwrap();
    assert_eq!(found["matches"][0]["path"], "global/runbooks/ship.md");
}

#[test]
fn if_rev_optimistic_concurrency_conflicts_instead_of_losing_updates() {
    let fx = fx();
    let d = &fx.d;
    write_op(d, "global/page.md", "v1");
    let rev = read_op(d, "global/page.md")["rev"]
        .as_str()
        .unwrap()
        .to_string();

    // A stale rev conflicts — never silently overwrites.
    let stale = d
        .operator_rpc(
            "wiki_write",
            json!({"path": "global/page.md", "text": "v2", "if_rev": "fnv1a:deadbeef"}),
        )
        .unwrap();
    assert_eq!(stale["conflict"], "if_rev");
    assert_eq!(stale["current_rev"], rev);
    assert_eq!(read_op(d, "global/page.md")["text"], "v1");

    // "none" is create-only: it refuses on an existing page.
    let conflict = d
        .operator_rpc(
            "wiki_write",
            json!({"path": "global/page.md", "text": "v2", "if_rev": "none"}),
        )
        .unwrap();
    assert_eq!(conflict["conflict"], "if_rev");

    // Two writers fire with the SAME rev under a barrier — the lock
    // serializes them; one lands, the other sees the new rev and
    // conflicts. No lost update. Each thread proves its own operator
    // caller (the seam's assertion is thread-local).
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let results: Vec<Value> = std::thread::scope(|s| {
        let joins: Vec<_> = ["thread-a", "thread-b"]
            .iter()
            .map(|text| {
                let b = barrier.clone();
                let rev = rev.clone();
                s.spawn(move || {
                    b.wait();
                    d.operator_rpc(
                        "wiki_write",
                        json!({"path": "global/page.md", "text": text, "if_rev": rev}),
                    )
                })
            })
            .collect();
        barrier.wait();
        joins
            .into_iter()
            .map(|j| j.join().unwrap().unwrap())
            .collect()
    });
    let wins = results.iter().filter(|r| r["committed"] == true).count();
    let conflicts = results.iter().filter(|r| r["conflict"].is_string()).count();
    assert_eq!(
        (wins, conflicts),
        (1, 1),
        "one winner, one conflict — never a lost update: {results:?}"
    );
}

#[test]
fn blob_upload_hashes_caps_and_commits_pointer() {
    // A 16-byte cap keeps the fixture small; `pm.yaml`'s wiki section
    // configures it.
    let fx = fx_with_cap(Some(16));
    let d = &fx.d;
    let uploads = d.state.join("wiki-uploads");
    std::fs::create_dir_all(&uploads).unwrap();

    // A real SVG upload lands: content-addressed, mime-sniffed,
    // pointer committed.
    let tmp = uploads.join("tiny-svg");
    std::fs::write(&tmp, "<svg xmlns='http://www.w3.org/2000/svg'></svg>").unwrap();
    // — but the fixture's cap is 16 bytes, so THIS is refused first,
    // before the file moves anywhere.
    let err = refused(d.operator_rpc(
        "wiki_put_blob",
        json!({"path": "projects/cadence/icon.svg", "tmp": tmp}),
    ));
    assert!(err.contains("cap"), "oversize refusal names the cap: {err}");
    let blobs = cadence_agent::wiki::blobs_dir(&vault(&fx));
    assert!(
        !blobs.exists() || std::fs::read_dir(&blobs).unwrap().next().is_none(),
        "a refused upload never lands a blob"
    );
    assert!(read_op_err(d, "projects/cadence/icon.svg").is_err());
}

#[test]
fn blob_upload_roundtrip_and_tmp_confinement() {
    let fx = fx();
    let d = &fx.d;
    let uploads = d.state.join("wiki-uploads");
    std::fs::create_dir_all(&uploads).unwrap();

    // png bytes — magic-sniffed.
    let png: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];
    let tmp = uploads.join("logo.png");
    std::fs::write(&tmp, png).unwrap();
    let out = d
        .operator_rpc(
            "wiki_put_blob",
            json!({"path": "global/logo.png", "tmp": tmp}),
        )
        .unwrap();
    assert_eq!(out["kind"], "blob");
    assert_eq!(out["mime"], "image/png");
    assert_eq!(out["size"], png.len() as u64);
    let sha = out["sha256"].as_str().unwrap().to_string();
    assert_eq!(sha.len(), 64);

    // The pointer is committed; read returns blob metadata.
    let page = read_op(d, "global/logo.png");
    assert_eq!(page["kind"], "blob");
    assert_eq!(page["sha256"], sha);
    let blob_file = cadence_agent::wiki::blobs_dir(&vault(&fx)).join(&sha);
    assert_eq!(std::fs::read(&blob_file).unwrap(), png);
    // `ls` shows the logical name once — never the .blob pointer.
    let ls = d
        .operator_rpc("wiki_ls", json!({"path": "global"}))
        .unwrap();
    let names: Vec<&str> = ls["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"logo.png"), "{names:?}");
    assert!(!names.iter().any(|n| n.ends_with(".blob")), "{names:?}");

    // A tmp outside <state>/wiki-uploads/ is refused — the daemon
    // never renames an arbitrary caller path.
    let outside = fx._root.path().join("elsewhere.bin");
    std::fs::write(&outside, b"x").unwrap();
    let err = refused(d.operator_rpc(
        "wiki_put_blob",
        json!({"path": "global/e.bin", "tmp": outside}),
    ));
    assert!(err.contains("wiki-uploads"), "{err}");

    // A sha256 claim that does not match the file's real hash refuses.
    let tmp2 = uploads.join("mismatch.bin");
    std::fs::write(&tmp2, b"real bytes").unwrap();
    let err = refused(d.operator_rpc(
        "wiki_put_blob",
        json!({"path": "global/m.bin", "tmp": tmp2, "sha256": "0".repeat(64)}),
    ));
    assert!(err.contains("sha256"), "{err}");
}

#[test]
fn svg_blob_records_svg_mime() {
    // The board's "never inline" rule reads this mime — the store must
    // sniff SVG as image/svg+xml, not text/plain.
    let fx = fx();
    let d = &fx.d;
    let uploads = d.state.join("wiki-uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    let tmp = uploads.join("chart.svg");
    std::fs::write(
        &tmp,
        "<svg xmlns='http://www.w3.org/2000/svg'><script/></svg>",
    )
    .unwrap();
    let out = d
        .operator_rpc(
            "wiki_put_blob",
            json!({"path": "global/chart.svg", "tmp": tmp}),
        )
        .unwrap();
    assert_eq!(out["mime"], "image/svg+xml", "{out}");
}

#[test]
fn traversal_is_refused_everywhere() {
    let fx = fx();
    let d = &fx.d;
    for bad in [
        "../x",
        "a/../b",
        "a/./b",
        "/etc/passwd",
        "a//b",
        "a\\b",
        "a%2Fb",
        ".hidden/x",
        ".trash/x",
        ".blobs/aa",
        "global/x.blob",
        "global/.gitkeep",
    ] {
        for (method, params) in [
            ("wiki_read", json!({"path": bad})),
            ("wiki_write", json!({"path": bad, "text": "x"})),
            ("wiki_mkdir", json!({"path": bad})),
            ("wiki_rm", json!({"path": bad})),
        ] {
            let err = refused(d.operator_rpc(method, params));
            assert!(
                err.contains("refused"),
                "{method} '{bad}' must refuse, got: {err}"
            );
        }
    }
    // mv's endpoints are each checked too.
    for (from, to) in [
        ("../x", "global/y"),
        ("global/x", "../y"),
        ("global/x", ".trash/y"),
    ] {
        let err = refused(d.operator_rpc("wiki_mv", json!({"from": from, "to": to})));
        assert!(err.contains("refused"), "mv {from}→{to}: {err}");
    }

    // rev-298 F2 — load-bearing '..': the segment sits AFTER a root
    // the ACL accepts, so only normalize's '..' guard refuses it.
    // Without the guard each lands OUTSIDE its addressed prefix.
    for (bad, escaped) in [
        (
            "agents/w1/knowledge/../../projects/other/x",
            "agents/projects/other/x",
        ),
        ("agents/w1/knowledge/../../global/x", "agents/global/x"),
        ("global/../users/fable/x", "users/fable/x"),
    ] {
        for (method, params) in [
            ("wiki_read", json!({"path": bad})),
            ("wiki_write", json!({"path": bad, "text": "x"})),
            ("wiki_mkdir", json!({"path": bad})),
            ("wiki_rm", json!({"path": bad})),
        ] {
            let err = refused(d.operator_rpc(method, params));
            assert!(err.contains("refused"), "{method} '{bad}': {err}");
        }
        assert!(
            !vault(&fx).join(escaped).exists(),
            "'{bad}' escaped its prefix into '{escaped}'"
        );
    }
    // 'global/../../x' resolves above the vault — into the tracker.
    for method in ["wiki_read", "wiki_write", "wiki_mkdir", "wiki_rm"] {
        let params = if method == "wiki_write" {
            json!({"path": "global/../../x", "text": "x"})
        } else {
            json!({"path": "global/../../x"})
        };
        let err = refused(d.operator_rpc(method, params));
        assert!(err.contains("refused"), "{method} 'global/../../x': {err}");
    }
    assert!(
        !fx.pm.join("x").exists(),
        "'global/../../x' escaped the vault into the tracker"
    );
    // The CLI rides the same daemon gate — it cannot widen it.
    for bad in ["agents/w1/knowledge/../../global/x", "global/../../x"] {
        let (ok, out, err) = d.operator_cadence(&["wiki", "put", bad, "-m", "x"]);
        let all = format!("{out} {err}");
        assert!(!ok && all.contains("refused"), "cli put '{bad}': {all}");
        let (ok, out, err) = d.operator_cadence(&["wiki", "cat", bad]);
        let all = format!("{out} {err}");
        assert!(!ok && all.contains("refused"), "cli cat '{bad}': {all}");
    }
    assert!(!fx.pm.join("x").exists(), "cli write escaped the vault");
}

#[test]
fn refused_upload_never_deletes_a_shared_blob() {
    let fx = fx();
    let d = &fx.d;
    let uploads = d.state.join("wiki-uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    let stage = |name: &str, bytes: &[u8]| {
        let tmp = uploads.join(name);
        std::fs::write(&tmp, bytes).unwrap();
        tmp
    };
    let bytes: &[u8] = &[0x89, b'P', b'N', b'G', 9, 9, 9];
    let out = d
        .operator_rpc(
            "wiki_put_blob",
            json!({"path": "global/shared.png", "tmp": stage("a.png", bytes)}),
        )
        .unwrap();
    let sha = out["sha256"].as_str().unwrap().to_string();
    let blobs = cadence_agent::wiki::blobs_dir(&vault(&fx));
    let blob = blobs.join(&sha);
    assert!(blob.exists(), "blob landed: {}", blob.display());

    // A text page occupies the colliding name; the refused upload
    // carries identical bytes, so its deduped dest was already a
    // live page's content.
    write_op(d, "global/clash.md", "mine");
    let err = refused(d.operator_rpc(
        "wiki_put_blob",
        json!({"path": "global/clash.md", "tmp": stage("b.png", bytes)}),
    ));
    assert!(err.contains("text page exists"), "{err}");
    // rev-298 F3: the shared blob is the first page's — still there.
    let page = read_op(d, "global/shared.png");
    assert_eq!(page["kind"], "blob");
    assert_eq!(
        std::fs::read(&blob).unwrap(),
        bytes,
        "a refused upload deleted another page's blob"
    );

    // if_rev guards the pointer write — a stale token conflicts,
    // never overwrites, and never touches a deduped blob.
    let out = d
        .operator_rpc(
            "wiki_put_blob",
            json!({"path": "global/shared.png",
                   "tmp": stage("c.png", bytes),
                   "if_rev": "fnv1a:deadbeefdeadbeef"}),
        )
        .unwrap();
    assert_eq!(out["conflict"], "if_rev", "{out}");
    assert!(blob.exists(), "a conflicted upload left the shared blob");
    // Create-only ("none") conflicts on an existing pointer.
    let out = d
        .operator_rpc(
            "wiki_put_blob",
            json!({"path": "global/shared.png",
                   "tmp": stage("d.png", bytes),
                   "if_rev": "none"}),
        )
        .unwrap();
    assert_eq!(out["conflict"], "if_rev", "{out}");
    // And a conflicted upload that DID create its blob removes it —
    // .blobs carries no orphan from the refused write.
    let names = |dir: &std::path::Path| {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        v.sort();
        v
    };
    let before = names(&blobs);
    let out = d
        .operator_rpc(
            "wiki_put_blob",
            json!({"path": "global/orphan.png",
                   "tmp": stage("e.png", &[7u8; 64]),
                   "if_rev": "fnv1a:deadbeefdeadbeef"}),
        )
        .unwrap();
    assert_eq!(out["conflict"], "if_rev", "{out}");
    assert_eq!(
        names(&blobs),
        before,
        "a refused upload left its created blob behind"
    );
}

#[test]
fn profile_and_memory_views_are_read_only() {
    let fx = fx();
    let d = &fx.d;
    // A profile file exists for an agent dir under <pm>/agents/.
    let slug_dir = fx.pm.join("agents").join("w1");
    std::fs::create_dir_all(&slug_dir).unwrap();
    std::fs::write(slug_dir.join("SOUL.md"), "soul text\n").unwrap();
    std::fs::write(slug_dir.join("AGENT.md"), "agent text\n").unwrap();

    // Reads resolve through the view.
    let page = read_op(d, "agents/w1/profile/SOUL.md");
    assert_eq!(page["kind"], "text");
    assert_eq!(page["text"], "soul text\n");
    let ls = d
        .operator_rpc("wiki_ls", json!({"path": "agents/w1/profile"}))
        .unwrap();
    let names: Vec<&str> = ls["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"SOUL.md") && names.contains(&"AGENT.md"),
        "{names:?}"
    );

    // Writes are refused — for the OPERATOR too; profile writes go
    // through agent_file_write, never the wiki path.
    for path in [
        "agents/w1/profile/SOUL.md",
        "agents/w1/profile/new.md",
        "agents/w1/memory/cadence/x.md",
    ] {
        let err = refused(d.operator_rpc("wiki_write", json!({"path": path, "text": "x"})));
        assert!(err.contains("refused"), "{path}: {err}");
    }
    let err = refused(d.operator_rpc("wiki_rm", json!({"path": "agents/w1/profile/SOUL.md"})));
    assert!(err.contains("refused"), "{err}");
}

#[test]
fn users_area_is_member_private() {
    let fx = fx();
    let d = &fx.d;
    // A member's own write — relayed by the board as user:<handle> on
    // the board's operator connection.
    let out = d
        .operator_rpc(
            "wiki_write",
            json!({"path": "users/fable/notes.md", "text": "mine", "wiki_as": "user:fable"}),
        )
        .unwrap();
    assert_eq!(out["committed"], true);
    let page = d
        .operator_rpc(
            "wiki_read",
            json!({"path": "users/fable/notes.md", "wiki_as": "user:fable"}),
        )
        .unwrap();
    assert_eq!(page["text"], "mine");

    // Another member is refused — both ops.
    for method in ["wiki_read", "wiki_write"] {
        let params = if method == "wiki_write" {
            json!({"path": "users/fable/notes.md", "text": "x", "wiki_as": "user:other"})
        } else {
            json!({"path": "users/fable/notes.md", "wiki_as": "user:other"})
        };
        let err = refused(d.operator_rpc(method, params));
        assert!(err.contains("private"), "{method}: {err}");
    }
    // The public caller sees none of it.
    let err = refused(d.operator_rpc(
        "wiki_read",
        json!({"path": "users/fable/notes.md", "wiki_as": "public"}),
    ));
    assert!(err.contains("refused"), "{err}");
    // A forged member name that is not a handle never reaches the ACL.
    let err = refused(d.operator_rpc(
        "wiki_read",
        json!({"path": "users/fable/notes.md", "wiki_as": "user:../etc"}),
    ));
    assert!(err.contains("wiki_as"), "{err}");
}

#[test]
fn public_caller_reads_shared_areas_only() {
    let fx = fx();
    let d = &fx.d;
    write_op(d, "global/n.md", "shared");
    write_op(d, "projects/cadence/spec.md", "spec");
    write_op(d, "users/fable/secret.md", "private");
    std::fs::create_dir_all(fx.pm.join("agents").join("w1")).unwrap();
    std::fs::write(fx.pm.join("agents/w1/SOUL.md"), "soul\n").unwrap();
    write_op(d, "agents/w1/knowledge/k.md", "note");

    let pub_read =
        |path: &str| d.operator_rpc("wiki_read", json!({"path": path, "wiki_as": "public"}));
    assert!(pub_read("global/n.md").is_ok());
    assert!(pub_read("projects/cadence/spec.md").is_ok());
    assert!(pub_read("agents/w1/profile/SOUL.md").is_ok());
    assert!(pub_read("agents/w1/knowledge/k.md").is_ok());
    assert!(pub_read("users/fable/secret.md").is_err());
    let err = refused(d.operator_rpc(
        "wiki_write",
        json!({"path": "global/n.md", "text": "x", "wiki_as": "public"}),
    ));
    assert!(err.contains("refused"), "{err}");
}

// ---------- agent callers: seam-asserted (CAD-482) ----------

#[cfg(feature = "test-seam")]
#[test]
fn agent_writes_own_areas_only() {
    let fx = fx();
    let d = &fx.d;
    // The agent's cwd is the fixture project's repo → it owns
    // projects/cadence/ and its own knowledge/ — nothing else.
    d.register_pc("w1", "fake", "fake", fx.repo.to_str().unwrap())
        .unwrap();

    let w1_write = |path: &str| {
        d.agent_rpc(
            "w1",
            "wiki_write",
            json!({"path": path, "text": "agent note"}),
        )
    };
    // Own knowledge — allowed.
    assert!(w1_write("agents/w1/knowledge/notes.md").is_ok());
    // Own project — allowed (the cwd→project derivation works).
    assert!(w1_write("projects/cadence/design.md").is_ok());
    // global/ — refused.
    let err = refused(w1_write("global/policy.md"));
    assert!(err.contains("refused"), "{err}");
    // Another agent's knowledge — refused.
    let err = refused(w1_write("agents/w2/knowledge/notes.md"));
    assert!(err.contains("own"), "{err}");
    // Another project — refused.
    let err = refused(w1_write("projects/other/x.md"));
    assert!(err.contains("refused"), "{err}");
    // users/ — refused both ops.
    let err = refused(w1_write("users/fable/x.md"));
    assert!(err.contains("private"), "{err}");
    let err = refused(d.agent_rpc("w1", "wiki_read", json!({"path": "users/fable/notes.md"})));
    assert!(err.contains("private"), "{err}");
    // Shared reads still work.
    assert!(d
        .agent_rpc(
            "w1",
            "wiki_read",
            json!({"path": "projects/cadence/design.md"})
        )
        .is_ok());
}

#[cfg(feature = "test-seam")]
#[test]
fn agent_traversal_after_a_valid_root_is_refused() {
    let fx = fx();
    let d = &fx.d;
    d.register_pc("w1", "fake", "fake", fx.repo.to_str().unwrap())
        .unwrap();

    // rev-298 F2 — '..' AFTER a valid root: the ACL sees
    // agents/w1/knowledge and allows w1; only normalize's '..'
    // segment guard keeps the write from landing outside knowledge/.
    for bad in [
        "agents/w1/knowledge/../../projects/other/x",
        "agents/w1/knowledge/../../global/x",
    ] {
        let err = refused(d.agent_rpc("w1", "wiki_write", json!({"path": bad, "text": "x"})));
        assert!(err.contains("refused"), "{bad}: {err}");
    }
    assert!(!vault(&fx).join("agents/projects/other/x").exists());
    assert!(!vault(&fx).join("agents/global/x").exists());
    // The blob path runs the same normalize gate.
    let uploads = d.state.join("wiki-uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    let tmp = uploads.join("w1-trav.png");
    std::fs::write(&tmp, [0x89, b'P', b'N', b'G', 5]).unwrap();
    let err = refused(d.agent_rpc(
        "w1",
        "wiki_put_blob",
        json!({"path": "agents/w1/knowledge/../../projects/other/p.png",
               "tmp": tmp}),
    ));
    assert!(err.contains("refused"), "{err}");
    assert!(!vault(&fx).join("agents/projects/other/p.png").exists());
}

#[cfg(feature = "test-seam")]
#[test]
fn agent_forged_identity_fields_are_refused() {
    let fx = fx();
    let d = &fx.d;
    d.register_pc("w1", "fake", "fake", fx.repo.to_str().unwrap())
        .unwrap();
    d.register("w2");

    // `wiki_as` naming anyone but the caller itself — the whole point
    // of the caller rule.
    for claim in ["agent:w2", "operator", "user:fable", "public"] {
        let err = refused(d.agent_rpc(
            "w1",
            "wiki_write",
            json!({"path": "agents/w1/knowledge/x.md", "text": "x", "wiki_as": claim}),
        ));
        assert!(err.contains("never acts as another"), "{claim}: {err}");
    }
    // wiki_as naming itself is the board's relay shape — allowed.
    assert!(d
        .agent_rpc(
            "w1",
            "wiki_write",
            json!({"path": "agents/w1/knowledge/x.md", "text": "x",
                   "wiki_as": "agent:w1"}),
        )
        .is_ok());
    // Classic identity fields on the request — refused by the shared
    // field table before the ACL is ever consulted.
    for field in ["by", "as", "actor", "caller", "operator", "reviewer"] {
        let mut params = json!({"path": "agents/w1/knowledge/y.md", "text": "x"});
        params[field] = json!("w2");
        let err = refused(d.agent_rpc("w1", "wiki_write", params));
        assert!(
            err.contains("not accepted") || err.contains("identity"),
            "{field}: {err}"
        );
    }
    // A directory the forged write must never have created.
    assert!(!vault(&fx).join("agents/w1/knowledge/y.md").exists());
}

#[cfg(feature = "test-seam")]
#[test]
fn unproven_callers_are_refused() {
    let fx = fx();
    let d = &fx.d;
    write_op(d, "global/n.md", "shared");
    // A detached child — no agent identity, no operator proof — is
    // refused on reads and writes alike.
    for (method, params) in [
        ("wiki_ls", json!({"path": ""})),
        ("wiki_read", json!({"path": "global/n.md"})),
        ("wiki_write", json!({"path": "global/x.md", "text": "x"})),
        ("wiki_search", json!({"q": "shared"})),
        ("wiki_history", json!({"path": "global/n.md"})),
    ] {
        let err = refused(d.unproven_rpc(method, params));
        assert!(err.contains("refused"), "{method}: {err}");
    }
    // And wiki_as does not rescue it — the connection decides.
    let err = refused(d.unproven_rpc(
        "wiki_read",
        json!({"path": "global/n.md", "wiki_as": "public"}),
    ));
    assert!(err.contains("refused"), "{err}");
}

#[cfg(feature = "test-seam")]
#[test]
fn agent_upload_and_rm_follow_the_same_acl() {
    let fx = fx();
    let d = &fx.d;
    d.register_pc("w1", "fake", "fake", fx.repo.to_str().unwrap())
        .unwrap();
    let uploads = d.state.join("wiki-uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    let tmp = uploads.join("w1.png");
    std::fs::write(&tmp, [0x89, b'P', b'N', b'G', 1, 2, 3, 4]).unwrap();

    // Own area — lands.
    let out = d
        .agent_rpc(
            "w1",
            "wiki_put_blob",
            json!({"path": "agents/w1/knowledge/logo.png", "tmp": tmp}),
        )
        .unwrap();
    assert_eq!(out["kind"], "blob");
    // Someone else's — refused, and the tmp is still refused before move.
    let tmp2 = uploads.join("w1b.png");
    std::fs::write(&tmp2, [0x89, b'P', b'N', b'G', 1, 2, 3, 4]).unwrap();
    let err = refused(d.agent_rpc(
        "w1",
        "wiki_put_blob",
        json!({"path": "agents/w2/knowledge/logo.png", "tmp": tmp2}),
    ));
    assert!(err.contains("own"), "{err}");
    // rm on own page lands in trash; another agent cannot rm it back
    // or rm it at all.
    let out = d
        .agent_rpc(
            "w1",
            "wiki_rm",
            json!({"path": "agents/w1/knowledge/logo.png"}),
        )
        .unwrap();
    let trash = out["trash"].as_str().unwrap().to_string();
    let err = refused(d.agent_rpc(
        "w1",
        "wiki_mv",
        json!({"from": trash, "to": "agents/w1/knowledge/logo.png"}),
    ));
    assert!(
        err.contains("operator"),
        "agent restore is operator-only: {err}"
    );
    // The operator restores it.
    d.operator_rpc(
        "wiki_mv",
        json!({"from": trash, "to": "agents/w1/knowledge/logo.png"}),
    )
    .unwrap();
    assert_eq!(
        d.agent_rpc(
            "w1",
            "wiki_read",
            json!({"path": "agents/w1/knowledge/logo.png"})
        )
        .unwrap()["kind"],
        "blob"
    );
}

// ---------- board HTTP routes: at least as strict as the RPC ----------

/// A live board over the wiki fixture's daemon.
struct BFx {
    fx: Fx,
    port: u16,
    _board: board_common::BoardStop,
    host: String,
}

fn bfx() -> BFx {
    let fx = fx();
    let (port, board) = board_common::start_ui(fx.pm.clone(), fx.d.state.clone());
    BFx {
        fx,
        port,
        _board: board,
        host: op::board_host(port),
    }
}

/// A GET on the board, asserted as `who` when the seam is armed.
fn get_as(b: &BFx, path: &str, who: &str) -> (u16, String, String) {
    let req = op::request("GET", path, &b.host, None, None, "");
    op::raw(b.port, &op::assert_as(req, &b.fx.d.state, who))
}

/// A PUT /api/wiki/file, asserted as `who` when the seam is armed.
/// Only the seam-gated caller tests PUT as a non-session identity.
#[cfg(feature = "test-seam")]
fn put_as(b: &BFx, who: &str, body: &str) -> (u16, String, String) {
    let req = op::request("PUT", "/api/wiki/file", &b.host, None, None, body);
    op::raw(b.port, &op::assert_as(req, &b.fx.d.state, who))
}

/// A multipart POST /api/wiki/upload — binary-safe body. `who` asserts
/// the caller when the seam is armed; `op` carries a real operator
/// session (writes require one — the seam's operator assertion alone
/// is only ever `NoAgent` attribution).
fn upload_as(
    b: &BFx,
    who: &str,
    op: Option<&op::Session>,
    path_arg: &str,
    filename: &str,
    bytes: &[u8],
) -> (u16, String, String) {
    let boundary = "CADENCEWIKITEST";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut headers: Vec<String> = vec![
        format!("Content-Type: multipart/form-data; boundary={boundary}"),
        "X-Cadence-Board: 1".to_string(),
    ];
    if let Some(op) = op {
        headers.extend(op.headers().lines().map(str::to_string));
    } else {
        let seam = op::seam_headers(&b.fx.d.state, who);
        headers.extend(seam.lines().map(str::to_string));
    }
    let refs: Vec<&str> = headers.iter().map(String::as_str).collect();
    board_common::http_write(
        b.port,
        "POST",
        &format!("/api/wiki/upload?path={path_arg}"),
        &b.host,
        &refs,
        &body,
    )
}

/// The signed-in operator's GET (cookie + page key + operator assert).
fn op_get(op: &op::Session, b: &BFx, path: &str) -> (u16, String, String) {
    op::raw(b.port, &op.request("GET", path, ""))
}

#[test]
fn board_operator_write_read_upload_svg_and_ranges() {
    let b = bfx();
    let state = &b.fx.d.state;
    let op = board_common::sign_in(state, b.port);

    // PUT a text page — the operator session writes global/.
    let (s, _, body) = op::raw(
        b.port,
        &op.request(
            "PUT",
            "/api/wiki/file",
            r#"{"path":"global/note.md","text":"board v1"}"#,
        ),
    );
    assert_eq!(s, 200, "{body}");

    // GET it back as text JSON.
    let (s, _, body) = op_get(&op, &b, "/api/wiki/file?path=global/note.md");
    assert_eq!(s, 200, "{body}");
    assert!(body.contains("board v1"), "{body}");

    // ls shows it; history names the write; search finds it.
    let (s, _, body) = op_get(&op, &b, "/api/wiki/ls?path=global");
    assert_eq!(s, 200, "{body}");
    assert!(body.contains("note.md"), "{body}");
    let (s, _, body) = op_get(&op, &b, "/api/wiki/history?path=global/note.md");
    assert_eq!(s, 200, "{body}");
    assert!(body.contains("wiki: write global/note.md"), "{body}");
    let (s, _, body) = op_get(&op, &b, "/api/wiki/search?q=board%20v1");
    assert_eq!(s, 200, "{body}");
    assert!(body.contains("global/note.md"), "{body}");

    // mkdir + rm through the board.
    let (s, _, body) = op::raw(
        b.port,
        &op.request("POST", "/api/wiki/mkdir", r#"{"path":"global/dir"}"#),
    );
    assert_eq!(s, 200, "{body}");
    let (s, _, body) = op::raw(
        b.port,
        &op.request("POST", "/api/wiki/rm", r#"{"path":"global/dir"}"#),
    );
    assert_eq!(s, 200, "{body}");
    assert!(body.contains(".trash/"), "{body}");

    // An SVG upload is served as an octet-stream attachment — never
    // inline, whatever its sniffed mime.
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>";
    let (s, _, body) = upload_as(&b, "operator", Some(&op), "global/x.svg", "x.svg", svg);
    assert_eq!(s, 200, "{body}");
    let (s, head, body) = op_get(&op, &b, "/api/wiki/file?path=global/x.svg");
    assert_eq!(s, 200, "{body}");
    assert!(
        head.contains("Content-Disposition: attachment"),
        "svg must download, never inline: {head}"
    );
    assert!(head.contains("application/octet-stream"), "{head}");
    assert!(head.contains("nosniff"), "{head}");

    // A PNG comes back inline under a sandboxed CSP, with ranges.
    let png: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2];
    let (s, _, body) = upload_as(&b, "operator", Some(&op), "global/x.png", "x.png", png);
    assert_eq!(s, 200, "{body}");
    let (s, head, _) = op_get(&op, &b, "/api/wiki/file?path=global/x.png");
    assert_eq!(s, 200);
    assert!(head.contains("image/png"), "{head}");
    assert!(head.contains("sandbox"), "{head}");
    assert!(!head.contains("attachment"), "{head}");

    // ASCII payload for byte-exact range asserts (binary bytes don't
    // survive the lossy test client).
    let (s, _, body) = upload_as(
        &b,
        "operator",
        Some(&op),
        "global/d.bin",
        "d.bin",
        b"0123456789",
    );
    assert_eq!(s, 200, "{body}");
    let (s, head, body) = {
        let req = op.request("GET", "/api/wiki/file?path=global/d.bin", "");
        let req = req.replacen("\r\n\r\n", "\r\nRange: bytes=2-5\r\n\r\n", 1);
        op::raw(b.port, &req)
    };
    assert_eq!(s, 206, "{head}");
    assert!(head.contains("Content-Range: bytes 2-5/10"), "{head}");
    assert_eq!(body, "2345", "{body}");
}

#[test]
fn board_write_guards_and_path_stays_the_daemons() {
    let b = bfx();
    let op = board_common::sign_in(&b.fx.d.state, b.port);

    // No write guards at all (a bare cross-site-looking request) —
    // refused before a caller is ever derived.
    let (s, _, body) = board_common::http_write(
        b.port,
        "PUT",
        "/api/wiki/file",
        &b.host,
        &["Content-Type: application/json"],
        br#"{"path":"global/x.md","text":"x"}"#,
    );
    assert_eq!(s, 403, "{body}");
    assert!(body.contains("X-Cadence-Board"), "{body}");

    // The text/plain "simple" form type never passes the CT guard.
    let (s, _, body) = board_common::http_write(
        b.port,
        "PUT",
        "/api/wiki/file",
        &b.host,
        &["Content-Type: text/plain", "X-Cadence-Board: 1"],
        b"x",
    );
    assert_eq!(s, 403, "{body}");

    // A traversal path rides to the daemon and refuses there —
    // the HTTP layer never normalizes it into an escape itself.
    let (s, _, body) = op::raw(
        b.port,
        &op.request(
            "PUT",
            "/api/wiki/file",
            r#"{"path":"../outside.md","text":"x"}"#,
        ),
    );
    assert_eq!(s, 400, "{body}");
    assert!(body.contains("refused"), "{body}");
    assert!(!b.fx.pm.join("../outside.md").exists());
    let (s, _, body) = op_get(&op, &b, "/api/wiki/file?path=../../etc/passwd");
    assert!(s >= 400, "{body}");

    // rev-298 F2 — '..' AFTER a valid root rides the same daemon
    // gate over HTTP: nothing resolves out of its prefix (or the
    // vault) through the board either.
    for bad in ["global/../users/fable/x", "global/../../x"] {
        let (s, _, body) = op::raw(
            b.port,
            &op.request(
                "PUT",
                "/api/wiki/file",
                &format!(r#"{{"path":"{bad}","text":"x"}}"#),
            ),
        );
        assert_eq!(s, 400, "{bad}: {body}");
        assert!(body.contains("refused"), "{body}");
    }
    assert!(!vault(&b.fx).join("users/fable/x").exists());
    assert!(
        !b.fx.pm.join("x").exists(),
        "an HTTP write escaped the vault into the tracker"
    );
    let (s, _, body) = op_get(
        &op,
        &b,
        "/api/wiki/file?path=agents/w1/knowledge/../../global/x",
    );
    assert!(s >= 400, "{body}");
}

#[test]
fn board_unadmitted_and_unattributed_writes_refuse() {
    let b = bfx();

    // A GET an unattributed caller makes: refused, never silently
    // "public" — the board proves its caller before the daemon sees it.
    // (Unarmed runs attribute the ambient process instead, so this
    // still answers 4xx or a real page — never a 5xx.)
    let (s, _, _) = get_as(&b, "/api/wiki/ls?path=global", "unproven");
    assert_ne!(s / 100, 5);

    // A write route the table never listed is the operator's by
    // default — an agent-asserted POST to an unknown /api/wiki path.
    let req = op::request("POST", "/api/wiki/unknown", &b.host, None, None, "{}");
    let (s, _, _) = op::raw(b.port, &op::assert_as(req, &b.fx.d.state, "agent:w1"));
    assert!(s >= 400, "unlisted write must refuse an agent caller: {s}");
}

#[cfg(feature = "test-seam")]
#[test]
fn board_agent_writes_only_its_areas() {
    let b = bfx();
    b.fx.d
        .register_pc("w1", "fake", "fake", b.fx.repo.to_str().unwrap())
        .unwrap();

    // Own knowledge — allowed.
    let (s, _, body) = put_as(
        &b,
        "agent:w1",
        r#"{"path":"agents/w1/knowledge/n.md","text":"mine"}"#,
    );
    assert_eq!(s, 200, "{body}");
    // global/ — refused; the daemon ACL is the same one the RPC enforces.
    let (s, _, body) = put_as(&b, "agent:w1", r#"{"path":"global/policy.md","text":"x"}"#);
    assert_eq!(s, 400, "{body}");
    assert!(body.contains("refused"), "{body}");
    // Another agent's folder — refused.
    let (s, _, body) = put_as(
        &b,
        "agent:w1",
        r#"{"path":"agents/w2/knowledge/n.md","text":"x"}"#,
    );
    assert_eq!(s, 400, "{body}");
    // rev-298 F2 — '..' after its own valid root: only normalize's
    // segment guard stands between w1 and a write outside knowledge/.
    let (s, _, body) = put_as(
        &b,
        "agent:w1",
        r#"{"path":"agents/w1/knowledge/../../global/x.md","text":"x"}"#,
    );
    assert_eq!(s, 400, "{body}");
    assert!(body.contains("refused"), "{body}");
    assert!(!vault(&b.fx).join("agents/global/x.md").exists());
    // A forged claim inside the JSON body is overwritten by the
    // relay's own wiki_as — it can only shrink, never widen.
    let (s, _, body) = put_as(
        &b,
        "agent:w1",
        r#"{"path":"global/g.md","text":"x","wiki_as":"operator"}"#,
    );
    assert_eq!(s, 400, "{body}");

    // Reads: the shared tree answers; users/ stays private.
    let (s, _, body) = get_as(
        &b,
        "/api/wiki/file?path=agents/w1/knowledge/n.md",
        "agent:w1",
    );
    assert_eq!(s, 200, "{body}");
    let (s, _, _) = get_as(&b, "/api/wiki/ls?path=users/fable", "agent:w1");
    assert_eq!(s, 400);
    // An unproven peer is refused before the daemon is ever asked.
    let (s, _, body) = get_as(&b, "/api/wiki/ls?path=global", "unproven");
    assert_eq!(s, 403, "{body}");
    let (s, _, body) = put_as(
        &b,
        "unproven",
        r#"{"path":"agents/w1/knowledge/x.md","text":"x"}"#,
    );
    assert_eq!(s, 403, "{body}");
}

#[cfg(feature = "test-seam")]
#[test]
fn board_upload_and_caps_apply_per_caller() {
    let b = bfx();
    b.fx.d
        .register_pc("w1", "fake", "fake", b.fx.repo.to_str().unwrap())
        .unwrap();
    let bytes: &[u8] = &[0x89, b'P', b'N', b'G', 1, 2, 3, 4];
    // An agent upload lands only in its own folder…
    let (s, _, body) = upload_as(
        &b,
        "agent:w1",
        None,
        "agents/w1/knowledge/x.png",
        "x.png",
        bytes,
    );
    assert_eq!(s, 200, "{body}");
    // …and refuses elsewhere with the same ACL the RPC runs.
    let (s, _, body) = upload_as(&b, "agent:w1", None, "global/x.png", "x.png", bytes);
    assert_eq!(s, 400, "{body}");
    // Traversal in ?path= refuses too.
    let (s, _, _) = upload_as(&b, "agent:w1", None, "../escape.png", "x.png", bytes);
    assert_eq!(s, 400);
    // rev-298 F2 — and '..' after its own valid root.
    let (s, _, body) = upload_as(
        &b,
        "agent:w1",
        None,
        "agents/w1/knowledge/../../global/e.png",
        "x.png",
        bytes,
    );
    assert_eq!(s, 400, "{body}");
    assert!(!vault(&b.fx).join("agents/global/e.png").exists());
}
