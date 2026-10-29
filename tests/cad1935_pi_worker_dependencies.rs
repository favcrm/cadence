//! Independent CAD-1935 acceptance. Implementer must transcribe verbatim.
//! Synthetic pinned packages; real settings, resolver, worker policy and Landlock.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use cadence_agent::adapter::ProviderEnv;

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const TOKEN: &str = "CAD1935_REQUIRED";
const PROBE: &str = r#"
const fs = require("node:fs");
const {createRequire} = require("node:module");
const [mode, entry, target] = process.argv.slice(1);
try {
  if (mode === "import") console.log(createRequire(entry)(target));
  else if (mode === "read") process.stdout.write(fs.readFileSync(target));
  else if (mode === "open-write") {
    const fd = fs.openSync(target, "r+"); fs.closeSync(fd); console.log("OPENED");
  } else throw new Error("unknown probe mode");
} catch (e) {
  console.error("CAD1935_ERROR:" + e.code);
  console.error(JSON.stringify({node: process.version, mode, entry, target,
    message: e.message, path: e.path, syscall: e.syscall, stack: e.stack}));
  process.exit(17);
}
"#;

fn package(root: &Path, name: &str, manifest: &str, relative: &str, body: &str) {
    let parsed: serde_json::Value = serde_json::from_str(manifest)
        .unwrap_or_else(|error| panic!("fixture manifest for {name}: {error}"));
    assert_eq!(parsed["name"].as_str(), Some(name), "fixture manifest name");
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join(relative).parent().unwrap()).unwrap();
    std::fs::write(dir.join("package.json"), manifest).unwrap();
    std::fs::write(dir.join(relative), body).unwrap();
}

struct ProbePaths<'a> {
    node: &'a Path,
    cwd: &'a Path,
    home: &'a Path,
}

fn probe(
    stage: &str,
    paths: &ProbePaths<'_>,
    policy: Option<&cadence_agent::confine::Policy>,
    mode: &str,
    entry: &Path,
    target: &Path,
) -> Output {
    eprintln!(
        "CAD1935_BEGIN stage={stage} guarded={} mode={mode} node={} entry={} target={}",
        policy.is_some(),
        paths.node.display(),
        entry.display(),
        target.display()
    );
    let mut command = if let Some(policy) = policy {
        let mut command = Command::new(BINARY);
        command
            .arg("confine")
            .args(policy.to_args())
            .arg("--")
            .arg(paths.node);
        command
    } else {
        Command::new(paths.node)
    };
    // Project lint requires the subreaper. It captures the same stdio as
    // `Command::output` and does not change the Landlock assertions.
    let output = cadence_agent::reaper::output(
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", paths.home)
            .env("TMPDIR", paths.cwd)
            .current_dir(paths.cwd)
            .arg("-e")
            .arg(PROBE)
            .arg(mode)
            .arg(entry)
            .arg(target),
    )
    .unwrap_or_else(|error| panic!("stage={stage}: spawn failed: {error}"));
    eprintln!(
        "CAD1935_END stage={stage} status={} stdout={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn succeeds(stage: &str, output: &Output, expected: &str) {
    assert!(
        output.status.success(),
        "stage={stage}: probe failed: {output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(expected),
        "stage={stage}: expected {expected:?}: {output:?}"
    );
}

fn denied(stage: &str, output: &Output) {
    assert!(
        !output.status.success(),
        "stage={stage}: unexpected access: {output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("CAD1935_ERROR:EACCES"),
        "stage={stage}: expected kernel EACCES, not absent fixture or launcher failure: {output:?}"
    );
    assert!(
        output.stdout.is_empty(),
        "stage={stage}: denied probe leaked bytes: {output:?}"
    );
}

#[test]
fn pinned_worker_extension_imports_without_sibling_authority() {
    assert!(
        cadence_agent::confine::abi_version() >= 1,
        "CAD-1935 requires a real Landlock kernel; unavailable is not a pass"
    );
    let node = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path).find_map(|dir| {
                use std::os::unix::fs::PermissionsExt;
                let candidate = dir.join("node");
                let meta = std::fs::metadata(&candidate).ok()?;
                (meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                    .then(|| candidate.canonicalize().unwrap())
            })
        })
        .expect("CAD-1935 requires an existing Node executable; do not install one here");
    let root = tempfile::Builder::new()
        .prefix("cad1935-worker-")
        .tempdir_in("/tmp")
        .unwrap();
    let path = |relative: &str| root.path().join(relative);
    for dir in [
        "home",
        "pm",
        "state",
        "bin",
        "work",
        "pi-agent/npm/node_modules",
        "private",
    ] {
        std::fs::create_dir_all(path(dir)).unwrap();
    }
    let npm = path("pi-agent/npm/node_modules");
    package(
        &npm,
        "pi-cad1935",
        r#"{"name":"pi-cad1935","version":"1.0.0","pi":{"extensions":["./dist/index.js"]},"dependencies":{"@cad1935/runtime":"1.0.0"}}"#,
        "dist/index.js",
        "module.exports=require(\"@cad1935/runtime\");\n",
    );
    package(
        &npm,
        "@cad1935/runtime",
        r#"{"name":"@cad1935/runtime","version":"1.0.0","dependencies":{"@cad1935/codec":"1.0.0"}}"#,
        "index.js",
        "module.exports=require(\"@cad1935/codec\");\n",
    );
    package(
        &npm,
        "@cad1935/codec",
        r#"{"name":"@cad1935/codec","version":"1.0.0"}"#,
        "index.js",
        "module.exports=\"CAD1935_REQUIRED\";\n",
    );
    package(
        &npm,
        "@cad1935/unrelated",
        r#"{"name":"@cad1935/unrelated","version":"1.0.0"}"#,
        "index.js",
        "module.exports=\"CAD1935_UNRELATED\";\n",
    );
    let sentinel = path("private/sentinel");
    std::fs::write(&sentinel, "CAD1935_PRIVATE").unwrap();
    let pi = path("bin/pi");
    std::fs::write(&pi, format!("#!{}\n// never executed\n", node.display())).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let digest = cadence_agent::pi_policy::package_digest(&npm.join("pi-cad1935")).unwrap();
    std::fs::write(
        path("pm/pm.yaml"),
        format!("pi:\n  providers: [\"pi-cad1935@1.0.0#{digest}\"]\n"),
    )
    .unwrap();
    let store =
        Arc::new(cadence_agent::store::Store::open(&path("state/cadence.sqlite3")).unwrap());
    store.import_legacy_settings(&path("pm")).unwrap();
    store
        .register_agent(&cadence_agent::store::NewAgent {
            alias: "cad1935-worker",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: path("work").to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: Some(r#"{"confine":true}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let agent = store.agent("cad1935-worker").unwrap();
    assert!(cadence_agent::adapter::pi::worker_confined(&agent));
    let env = ProviderEnv::default().with_settings_store(Arc::clone(&store));
    let search = std::env::join_paths([
        path("bin"),
        node.parent().unwrap().to_path_buf(),
        PathBuf::from(BINARY).parent().unwrap().to_path_buf(),
    ])
    .unwrap();
    env.set("PATH", search.to_str().unwrap());
    for (key, value) in [
        ("HOME", path("home")),
        ("XDG_DATA_HOME", path("home/.local/share")),
        ("PI_CODING_AGENT_DIR", path("pi-agent")),
        ("CADENCE_PM_DIR", path("pm")),
        ("CADENCE_PI_COMMAND", pi),
        ("CARGO_HOME", path("home/.cargo")),
        ("RUSTUP_HOME", path("home/.rustup")),
        ("SCCACHE_DIR", path("home/.cache/sccache")),
    ] {
        env.set(key, value.to_str().unwrap());
    }
    env.set("CADENCE_CONFINE_COMMAND", BINARY);
    for key in [
        "CADENCE_PI_WORKER_CONFINE_READ",
        "CADENCE_PI_WORKER_CONFINE_WRITE",
        "CADENCE_MASTER_CONFINE_READ",
        "CADENCE_MASTER_CONFINE_WRITE",
    ] {
        env.set(key, "");
    }
    let policy = cadence_agent::adapter::pi::pi_worker_confinement(&env, &path("state"), &agent).1;
    for forbidden in [&npm, &path("home"), &path("private"), &path("pi-agent")] {
        assert!(
            !policy
                .read
                .iter()
                .chain(&policy.write)
                .any(|grant| forbidden.starts_with(grant)),
            "worker policy widened to {forbidden:?}: {policy:?}"
        );
    }
    let entry = npm.join("pi-cad1935/dist/index.js");
    let runtime = npm.join("@cad1935/runtime/index.js");
    let codec = npm.join("@cad1935/codec/index.js");
    let unrelated = npm.join("@cad1935/unrelated/index.js");
    let cwd = path("work");
    let home = path("home");
    let paths = ProbePaths {
        node: &node,
        cwd: &cwd,
        home: &home,
    };
    let run = |stage: &str, guarded: bool, mode: &str, target: &Path| {
        probe(
            stage,
            &paths,
            if guarded { Some(&policy) } else { None },
            mode,
            &entry,
            target,
        )
    };
    // Controls distinguish a real policy failure from broken Node/fixture/pin setup.
    let stage = "control-unconfined-pinned-entry-import";
    succeeds(stage, &run(stage, false, "import", &entry), TOKEN);
    let stage = "control-unconfined-unrelated-byte-read";
    succeeds(
        stage,
        &run(stage, false, "read", &unrelated),
        "CAD1935_UNRELATED",
    );
    let stage = "control-unconfined-private-byte-read";
    succeeds(
        stage,
        &run(stage, false, "read", &sentinel),
        "CAD1935_PRIVATE",
    );
    let stage = "control-confined-pinned-entry-byte-read";
    succeeds(stage, &run(stage, true, "read", &entry), "@cad1935/runtime");
    // Run refusal legs before the expected pre-repair import failure.
    let stage = "refuse-confined-unrelated-byte-read";
    denied(stage, &run(stage, true, "read", &unrelated));
    let stage = "refuse-confined-private-byte-read";
    denied(stage, &run(stage, true, "read", &sentinel));
    let stage = "refuse-confined-runtime-write-open";
    denied(stage, &run(stage, true, "open-write", &runtime));
    let stage = "refuse-confined-codec-write-open";
    denied(stage, &run(stage, true, "open-write", &codec));
    let stage = "refuse-confined-unrelated-import";
    let sibling_import = run(stage, true, "import", &unrelated);
    assert!(
        !sibling_import.status.success(),
        "stage={stage}: unrelated import succeeded: {sibling_import:?}"
    );
    assert!(
        !String::from_utf8_lossy(&sibling_import.stdout).contains("CAD1935_UNRELATED"),
        "stage={stage}: unrelated bytes leaked: {sibling_import:?}"
    );
    // Execute the PINNED ENTRY, not merely require.resolve or a copied policy.
    let stage = "required-confined-pinned-entry-import";
    succeeds(stage, &run(stage, true, "import", &entry), TOKEN);
    let stage = "required-confined-runtime-byte-read";
    succeeds(stage, &run(stage, true, "read", &runtime), "@cad1935/codec");
    let stage = "required-confined-codec-byte-read";
    succeeds(stage, &run(stage, true, "read", &codec), TOKEN);
}
