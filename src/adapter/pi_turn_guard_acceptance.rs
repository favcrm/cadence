//! CAD-1526 acceptance: the pi turn guard bounds the request message count.
//!
//! The gateway publishes a maximum number of `messages` per request (100, per
//! the AgenticOS inference contract and provider registry). A long session
//! that crosses it fails every later turn with `400 Field "messages" exceeds
//! the published maximum`, and compaction cannot recover it because
//! compaction is a size bound. The guard's context projection keeps the
//! request under the maximum; this test drives a history past the failure
//! point through the real guard source and asserts an answered request — the
//! fixture refuses anything over the published maximum, or a tool result
//! whose assistant call was dropped — rather than the 400 that ended every
//! later turn of the session in the ticket's evidence.

use std::path::PathBuf;
use std::process::Command;

fn adapter_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/adapter")
}

/// `node` resolved from PATH — `None` when the host has no Node, so the
/// fixture reports that it could not run rather than failing on a missing
/// interpreter.
fn node_on_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
}

#[test]
fn the_guard_bounds_messages_below_the_gateway_maximum() {
    let Some(node) = node_on_path() else {
        eprintln!("skipping: node is not on PATH, so the guard fixture cannot run");
        return;
    };
    let fixture = adapter_dir().join("pi_turn_guard.test.mjs");
    assert!(
        fixture.is_file(),
        "missing guard fixture {}",
        fixture.display()
    );
    // Both runtime branches: the deployed pi (0.87.1) takes the ungated path,
    // 1.0.0 the guidance-filtering path. The count bound applies to both.
    for version in ["0.87.1", "1.0.0"] {
        let output = Command::new(&node)
            .arg("--test")
            .arg(&fixture)
            .env("PI_STUB_VERSION", version)
            .output()
            .expect("run the guard fixture under node");
        assert!(
            output.status.success(),
            "pi {version}: the guard fixture failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
