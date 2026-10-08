//! Independent CAD-1195 acceptance for the managed Pi launcher boundary.
//!
//! This fixture models the ordinary master PATH wrapper and its managed
//! install metadata. The child probe applies the actual Landlock policy via
//! `confine::exec`; policy-list inspection alone is not the boundary check.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

const PROBE_ENV: &str = "CAD1195_LANDLOCK_PROBE";
const TEST_NAME: &str =
    "adapter::pi::cad1195_acceptance::managed_pi_launcher_confinement_acceptance";

struct ManagedFixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    pointer: PathBuf,
    release: PathBuf,
    private: PathBuf,
    xdg_data: PathBuf,
    node_root: PathBuf,
    node_file: PathBuf,
    node_private_file: PathBuf,
    node_private: PathBuf,
    state: PathBuf,
}

impl ManagedFixture {
    fn new(pointer_contents: &str, pointer_symlink: Option<&Path>) -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad1195-pi-")
            .tempdir()
            .unwrap();
        let root_path = root.path().to_path_buf();
        let wrapper_dir = root_path.join(".pi/agent/bin");
        let install = root_path.join(".pi/agent/install");
        let release = install.join("releases/1.1.0");
        let bin_dir = release.join("node_modules/.bin");
        let private_dir = root_path.join(".pi-private-sibling");
        std::fs::create_dir_all(&wrapper_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&private_dir).unwrap();
        std::fs::create_dir_all(root_path.join(".local/bin")).unwrap();

        // The installed wrapper's Node lookup is rooted at
        // ${XDG_DATA_HOME:-$HOME/.local/share}/pi-node, not under ~/.pi/agent.
        let xdg_data = root_path.join("xdg-data");
        let node_root = xdg_data.join("pi-node");
        let node_leaf = node_root.join("node-v22.1.0");
        let node_file = node_leaf.join("bin/node");
        let node_private_root = xdg_data.join("private-node");
        let node_private_leaf = node_private_root.join("node-v-private");
        let node_private_file = node_private_leaf.join("bin/node");
        let node_private = node_private_root.join("unrelated-private/sentinel");
        std::fs::create_dir_all(node_file.parent().unwrap()).unwrap();
        std::fs::create_dir_all(node_private_leaf.join("bin")).unwrap();
        std::fs::create_dir_all(node_private.parent().unwrap()).unwrap();
        std::fs::write(&node_file, "managed node runtime\n").unwrap();
        std::fs::set_permissions(&node_file, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&node_private_file, "private escaped node runtime\n").unwrap();
        std::fs::set_permissions(&node_private_file, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::write(
            &node_private,
            "private node sibling must remain unreadable\n",
        )
        .unwrap();
        std::os::unix::fs::symlink("node-v22.1.0", node_root.join("current")).unwrap();
        std::os::unix::fs::symlink("node-v-private", node_private_root.join("current")).unwrap();

        let wrapper = wrapper_dir.join("pi");
        std::fs::write(
            &wrapper,
            "#!/bin/sh\nversion=$(cat \"$(dirname \"$0\")/../install/current-version\")\nexec \"$(dirname \"$0\")/../install/releases/$version/node_modules/.bin/pi\" \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&wrapper, root_path.join(".local/bin/pi")).unwrap();

        let pointer = install.join("current-version");
        if let Some(target) = pointer_symlink {
            std::os::unix::fs::symlink(target, &pointer).unwrap();
        } else {
            std::fs::write(&pointer, pointer_contents).unwrap();
        }

        let package = release.join("node_modules/@earendil-works/pi-coding-agent");
        std::fs::create_dir_all(package.join("dist/bundle")).unwrap();
        std::fs::write(
            package.join("package.json"),
            "{\"name\":\"@earendil-works/pi-coding-agent\"}\n",
        )
        .unwrap();
        let package_cli = package.join("dist/bundle/cli.js");
        std::fs::write(&package_cli, "#!/usr/bin/env node\n// fixture\n").unwrap();
        std::fs::set_permissions(&package_cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(
            "../@earendil-works/pi-coding-agent/dist/bundle/cli.js",
            bin_dir.join("pi"),
        )
        .unwrap();

        let private = private_dir.join("sentinel");
        std::fs::write(&private, "private sibling must remain unreadable\n").unwrap();
        let state = root_path.join("state");
        std::fs::create_dir_all(&state).unwrap();
        Self {
            _root: root,
            root: root_path,
            pointer,
            release,
            private,
            xdg_data,
            node_root,
            node_file,
            node_private_file,
            node_private,
            state,
        }
    }

    fn policy(&self) -> crate::confine::Policy {
        let env = ProviderEnv::default();
        env.set(
            "PATH",
            self.root.join(".local/bin").to_string_lossy().to_string(),
        );
        env.set("HOME", self.root.to_string_lossy().to_string());
        env.set("XDG_DATA_HOME", self.xdg_data.to_string_lossy().to_string());
        env.set("CADENCE_CONFINE_COMMAND", "/usr/bin/true");
        env.set(
            "CADENCE_PM_DIR",
            self.root.join("no-pm-here").to_string_lossy().to_string(),
        );
        pi_master_confinement(&env, &self.state).1
    }
}

/// Verifies the managed pointer/release are usable under the actual guard,
/// while an unrelated sibling (including an attacker-selected escaped
/// location from malformed or symlinked metadata) is still denied.
#[test]
fn managed_pi_launcher_confinement_acceptance() {
    if let Ok(root) = std::env::var(PROBE_ENV) {
        let fixture = fixture_from_root(PathBuf::from(root));
        let policy = fixture.policy();
        let path = std::env::var_os("CAD1195_PROBE_PATH")
            .map(PathBuf::from)
            .unwrap();
        let command = vec!["/bin/cat".to_string(), path.to_string_lossy().into_owned()];
        // This applies Landlock to the child process and replaces it with
        // cat; it is the kernel guard, not an assertion about Policy vectors.
        let error = crate::confine::exec(&policy, &command).unwrap_err();
        panic!("guard probe could not exec: {error}");
    }

    let valid = ManagedFixture::new("1.1.0\n", None);
    let policy = valid.policy();
    assert!(
        policy.read.iter().any(|path| path == &valid.pointer),
        "managed current-version pointer absent from policy: {policy:?}"
    );
    assert!(
        policy.read.iter().any(|path| path == &valid.release),
        "managed current release absent from policy: {policy:?}"
    );
    assert_guard_read(&valid, &valid.pointer, Some("1.1.0"));
    let package_cli = valid
        .release
        .join("node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js");
    assert_guard_read(&valid, &package_cli, Some("// fixture"));
    assert_guard_read(&valid, &valid.node_file, Some("managed node runtime"));
    assert_guard_read(&valid, &valid.private, None);
    assert_guard_read(&valid, &valid.node_private, None);

    // A traversing pointer and a symlinked pointer to a private sibling may
    // not make that sibling readable through the real confinement guard.
    let traversing = ManagedFixture::new("../../../.pi-private-sibling\n", None);
    assert_guard_read(&traversing, &traversing.private, None);

    let escaped = ManagedFixture::new(
        "1.1.0\n",
        Some(Path::new("../../../.pi-private-sibling/sentinel")),
    );
    assert_guard_read(&escaped, &escaped.private, None);

    // The wrapper's effective XDG_DATA_HOME/pi-node anchor is the boundary:
    // a current link outside it or a symlinked anchor must not authorize the
    // private target tree.
    let current_escape = ManagedFixture::new("1.1.0\n", None);
    replace_symlink(
        &current_escape.node_root.join("current"),
        Path::new("../private-node/current"),
    );
    assert_guard_read(&current_escape, &current_escape.node_private_file, None);
    assert_guard_read(&current_escape, &current_escape.node_private, None);

    let anchor_escape = ManagedFixture::new("1.1.0\n", None);
    std::fs::remove_dir_all(&anchor_escape.node_root).unwrap();
    std::os::unix::fs::symlink(
        anchor_escape.xdg_data.join("private-node"),
        &anchor_escape.node_root,
    )
    .unwrap();
    assert_guard_read(&anchor_escape, &anchor_escape.node_private_file, None);
    assert_guard_read(&anchor_escape, &anchor_escape.node_private, None);

    // A current link to the pi-node anchor itself is not a version leaf;
    // granting it would expose a sibling unrelated to the selected Node.
    let broad_root = ManagedFixture::new("1.1.0\n", None);
    replace_symlink(&broad_root.node_root.join("current"), Path::new("."));
    let broad_node = broad_root.node_root.join("bin/node");
    std::fs::create_dir_all(broad_node.parent().unwrap()).unwrap();
    std::fs::write(&broad_node, "broad-root node\n").unwrap();
    std::fs::set_permissions(&broad_node, std::fs::Permissions::from_mode(0o755)).unwrap();
    let broad_sibling = broad_root.node_root.join("unrelated-private/sentinel");
    std::fs::create_dir_all(broad_sibling.parent().unwrap()).unwrap();
    std::fs::write(&broad_sibling, "broad Node root must not be readable\n").unwrap();
    assert_guard_read(&broad_root, &broad_sibling, None);
}

fn replace_symlink(path: &Path, target: &Path) {
    std::fs::remove_file(path).unwrap();
    std::os::unix::fs::symlink(target, path).unwrap();
}

fn assert_guard_read(fixture: &ManagedFixture, path: &Path, expected_content: Option<&str>) {
    let output = run_guard_probe(fixture, path);
    if let Some(expected_content) = expected_content {
        assert!(
            output.status.success(),
            "expected confined read of {} to succeed; stderr: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected_content),
            "guarded read of {} did not return expected content: {}",
            path.display(),
            String::from_utf8_lossy(&output.stdout)
        );
    } else {
        assert!(
            !output.status.success(),
            "real Landlock guard unexpectedly read {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Permission denied"),
            "{} failed without a permission-denial diagnostic: {}",
            path.display(),
            stderr
        );
        assert!(!String::from_utf8_lossy(&output.stdout)
            .contains("private sibling must remain unreadable"));
    }
}

fn run_guard_probe(fixture: &ManagedFixture, path: &Path) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(TEST_NAME)
        .arg("--nocapture")
        .env(PROBE_ENV, fixture.root.as_os_str())
        .env("CAD1195_PROBE_PATH", path.as_os_str())
        .output()
        .unwrap()
}

fn fixture_from_root(root: PathBuf) -> ManagedFixture {
    let pointer = root.join(".pi/agent/install/current-version");
    let release = root.join(".pi/agent/install/releases/1.1.0");
    let private = root.join(".pi-private-sibling/sentinel");
    let xdg_data = root.join("xdg-data");
    let node_root = xdg_data.join("pi-node");
    let node_file = node_root.join("node-v22.1.0/bin/node");
    let node_private_file = xdg_data.join("private-node/node-v-private/bin/node");
    let node_private = xdg_data.join("private-node/unrelated-private/sentinel");
    ManagedFixture {
        _root: tempfile::Builder::new()
            .prefix("cad1195-probe-owner-")
            .tempdir()
            .unwrap(),
        root: root.clone(),
        pointer,
        release,
        private,
        xdg_data,
        node_root,
        node_file,
        node_private_file,
        node_private,
        state: root.join("state"),
    }
}
