//! CAD-1254 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! Drives the real install guards — the workspace catalog install
//! (`snapshot` + `validate_texts`) and the legacy `app::install`
//! (`bundle_files` + `validate`) — with on-disk bundles:
//! (a) a screen JS of 384 KiB + 1 B is refused;
//! (b) a non-screen file (a rubric) of 256 KiB + 1 B is refused;
//! (c) control: a screen JS of exactly 384 KiB installs.
use super::*;

const SCREEN_CAP: usize = 393_216;
const FILE_CAP: usize = 262_144;

const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n---\n\n\
Why.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n";

/// `len` bytes of plain ASCII text in short lines.
fn body(len: usize) -> String {
    let mut s = String::with_capacity(len + 80);
    while s.len() < len {
        s.push_str("// padding line for the screen asset size cap check\n");
    }
    s.truncate(len);
    s
}

/// A bundle named `app` with one extra file `rel` of `len` bytes.
fn bundle(dir: &Path, app: &str, rel: &str, len: usize) {
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    std::fs::write(
        dir.join("app.md"),
        format!(
            "---\napp: {app}\ntitle: Cap\nversion: '1'\nneeds:\n  connections: []\n---\n\nGuide.\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = body(len);
    std::fs::write(&path, &text).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), len as u64);
    // A screen asset ships with its app-screens/v1 declaration, so the
    // only thing that can refuse it is its size.
    if let Some(leaf) = rel.strip_prefix("screens/board/") {
        let sha = |t: &str| {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(t.as_bytes()))
        };
        let decl = serde_json::json!({
            "contract": "app-screens/v1",
            "app": app,
            "entry": leaf,
            "assets": [{"name": leaf, "media_type": "text/javascript",
                        "sha256": sha(&text), "size": text.len()}],
            "provenance": {"source_digest": sha("s"), "sdk_digest": sha("k"),
                           "toolchain_digest": sha("t")},
            "may": [],
        });
        std::fs::write(dir.join("screens/board/screens.json"), decl.to_string()).unwrap();
    }
}

#[test]
fn cad1254_screen_asset_cap_refuses_oversize_and_keeps_256k_elsewhere() {
    let pm_dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let sources = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let pm = Pm::init(pm_dir.path()).unwrap();
    crate::issue::write::project_add(
        &pm,
        "legacy",
        "LEG",
        &[repo.path().display().to_string()],
        &[],
        &[],
        None,
    )
    .unwrap();

    let cases: [(&str, &str, usize, Option<u64>); 3] = [
        // (a) screen JS one byte over the screen cap.
        (
            "big-screen",
            "screens/board/client.js",
            SCREEN_CAP + 1,
            Some(SCREEN_CAP as u64),
        ),
        // (b) non-screen file one byte over the 256 KiB cap.
        (
            "big-rubric",
            "rubrics/r.md",
            FILE_CAP + 1,
            Some(FILE_CAP as u64),
        ),
        // (c) control: screen JS exactly at the screen cap.
        ("ok-screen", "screens/board/client.js", SCREEN_CAP, None),
    ];
    for (app_name, rel, len, refused_at) in cases {
        let src = sources.path().join(app_name);
        bundle(&src, app_name, rel, len);
        let ws = install(&pm, state.path(), src.to_str().unwrap(), None);
        let legacy = app::install(&pm, "legacy", src.to_str().unwrap(), state.path(), "accept");
        match refused_at {
            Some(cap) => {
                // The workspace snapshot reads each member through
                // `Root::read(path, app::file_cap(name))`; a regular,
                // singly-linked file over its cap is refused with exactly
                // this message (the size is the only failing condition).
                let ws = ws
                    .expect_err(&format!("workspace install accepted {rel} of {len} B"))
                    .to_string();
                assert!(
                    ws.ends_with("catalog requires a bounded regular file without hard links"),
                    "workspace refusal for {rel} is not the size cap: {ws}"
                );
                let legacy = legacy
                    .expect_err(&format!("legacy install accepted {rel} of {len} B"))
                    .to_string();
                assert!(
                    legacy.contains(&format!("{rel} is {len} bytes — a file is at most {cap}")),
                    "legacy refusal for {rel} is not the size cap: {legacy}"
                );
            }
            None => {
                ws.unwrap_or_else(|e| panic!("workspace install refused {rel} of {len} B: {e}"));
                // The legacy gate is `app::validate` (`bundle_files` + the
                // per-file read check), which `app::install` runs before it
                // copies anything. Legacy install cannot yet copy any
                // nested `screens/<tag>/` member (its `copy_verified`
                // creates only the leaf's parent dir), so the control
                // asserts the gate itself and that install's refusal, if
                // any, is not the size cap.
                app::validate(&src, &std::collections::HashSet::new(), &[])
                    .unwrap_or_else(|e| panic!("legacy gate refused {rel} of {len} B: {e}"));
                if let Err(e) = legacy {
                    let e = e.to_string();
                    assert!(
                        !e.contains("a file is at most") && !e.contains("failed integrity"),
                        "legacy install refused {rel} of {len} B on size: {e}"
                    );
                }
            }
        }
    }
}
