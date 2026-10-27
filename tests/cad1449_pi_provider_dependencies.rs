//! CAD-1449 independent acceptance: a confined Pi master's generated
//! policy must cover the declared npm runtime dependency closure of each
//! operator-pinned `[pi].providers` package — and nothing else.
//!
//! The ticket's proven bad case: `pi-cursor-sdk` is pinned and installed,
//! but under `cadence confine` its `import "@cursor/sdk"` fails
//! MODULE_NOT_FOUND because the sibling package dir was never granted.
//! These checks build a synthetic npm root with a pinned extension that
//! imports a declared direct dependency (which itself imports a declared
//! transitive sibling), then apply the REAL
//! `cadence_agent::adapter::pi::pi_master_confinement` policy through the
//! REAL `cadence confine` subprocess against the host's real `node`:
//! the required imports must actually load (byte-read + execute — a
//! bare `require.resolve` only stats, which Landlock does not gate),
//! while an unrelated sibling package and a private sentinel outside
//! the npm root must stay unreadable. No
//! mock sandbox, no mirrored policy, no live provider/model/network use.
//! Canonical policy is imported once into an isolated fixture SQLite
//! store.
//!
//! The same group proves the real resolver/policy path refuses to widen
//! authority on malformed dependency metadata: traversal and absolute
//! dependency names, a missing REQUIRED dependency, and an installed
//! dependency dir that is a symlink out of the npm root must each leave
//! the npm tree ungranted (a fail-closed resolver may instead refuse the
//! launch outright — either way no unrelated path becomes readable). A
//! missing OPTIONAL dependency and a dependency cycle must not fail or
//! broaden the grant. The implementer must never edit this file.
//!
//! Node runtime assumption: the confined probes execute `node` resolved
//! from the fixture's PATH plus the policy's system reads (/usr, /bin…);
//! the fixture PATH is <fixture>/bin plus the real directories of `node`
//! and the test binary (cadence). On a host without Landlock the
//! `cadence confine` wrapper emits SKIP-NO-GUARD and the leg is a no-op —
//! the same Linux-guard convention other confinement checks use.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock};

use cadence_agent::adapter::ProviderEnv;

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");

/// `ProviderEnv::var` falls through to the process environment, so
/// concurrent #[test]s would race on ambient HOME/PATH reads;
/// serialize the group (cargo runs integration tests in one process).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

/// Everything under one synthetic root: fake HOME (so the private
/// sentinel is never inside any granted home tree), fake operator Pi
/// dir (`PI_CODING_AGENT_DIR` → its `npm/node_modules` is the pinned
/// provider root), pm dir, state dir, fixture PATH dir.
struct Host {
    root: tempfile::TempDir,
    node_bin: PathBuf,
    store: Arc<cadence_agent::store::Store>,
}

/// One `node -e` snippet shared by every probe: actually require the
/// given module from inside the pinned extension's `dist/` dir the way
/// the extension's own `import "<spec>"` loads it — this reads and
/// executes the module bytes, unlike `resolve()` which needs only
/// metadata/stat Landlock does not restrict. Prints the module's
/// exported value on success; any read/import error exits nonzero.
const REQUIRE_PROBE: &str = r#"const {createRequire}=require('module');
const [from,spec]=process.argv.slice(1);
try{const v=createRequire(from)(spec);console.log(typeof v==='string'?v:'loaded')}
catch(e){console.error(String(e));process.exit(3)}"#;

impl Host {
    fn new() -> Option<Self> {
        let root = tempfile::Builder::new()
            .prefix("cad1449-pi-deps-")
            .tempdir_in("/tmp")
            .unwrap();
        let node_bin = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path).find_map(|dir| {
                let candidate = dir.join("node");
                let executable = std::fs::metadata(&candidate).ok().is_some_and(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.is_file() && m.permissions().mode() & 0o111 != 0
                });
                executable.then_some(candidate)
            })
        })?;
        for dir in ["home", "pm", "state", "bin", "pi-agent/npm/node_modules"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        let store = Arc::new(
            cadence_agent::store::Store::open(&root.path().join("state/cadence.sqlite3")).unwrap(),
        );
        Some(Self {
            root,
            node_bin,
            store,
        })
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn npm_root(&self) -> PathBuf {
        self.path("pi-agent/npm/node_modules")
    }

    /// The environment the master adapter would see: `pm.yaml [pi]`
    /// providers resolved under the fake operator Pi npm dir, HOME and
    /// CADENCE_PM_DIR pointed inside the fixture, a PATH that finds the
    /// fixture `pi` stub, the real `node` and the confine binary.
    fn env(&self) -> ProviderEnv {
        let env = ProviderEnv::default().with_settings_store(Arc::clone(&self.store));
        let mut path = self.path("bin").to_string_lossy().into_owned();
        for exe in [&self.node_bin, &PathBuf::from(BINARY)] {
            if let Some(dir) = exe.parent() {
                let dir = dir.to_string_lossy().into_owned();
                if !path.split(':').any(|p| p == dir) {
                    path.push(':');
                    path.push_str(&dir);
                }
            }
        }
        env.set("PATH", path);
        env.set("HOME", self.path("home").to_string_lossy().into_owned());
        env.set(
            "XDG_DATA_HOME",
            self.path("home/.local/share")
                .to_string_lossy()
                .into_owned(),
        );
        env.set(
            "PI_CODING_AGENT_DIR",
            self.path("pi-agent").to_string_lossy().into_owned(),
        );
        env.set(
            "CADENCE_PM_DIR",
            self.path("pm").to_string_lossy().into_owned(),
        );
        env.set(
            "CADENCE_PI_COMMAND",
            self.path("bin/pi").to_string_lossy().into_owned(),
        );
        env.set("CADENCE_CONFINE_COMMAND", BINARY);
        env
    }

    /// The `node` path the confined probe execs — the canonicalized
    /// real executable, matching what `with_interpreter` resolves the
    /// stub's `#!` into and what the Landlock grant must cover.
    fn confined_node_path(&self) -> PathBuf {
        self.node_bin
            .canonicalize()
            .unwrap_or_else(|_| self.node_bin.clone())
    }

    /// Install the synthetic provider pin `pi-ext@1.0.0` whose entry
    /// imports `dep-a`; `dep-a` declares and imports `dep-b`; an
    /// undeclared sibling `dep-x` and a private sentinel outside the
    /// npm root carry the tokens the refusal legs look for.
    fn install_pinned_extension(&self) {
        let root = self.npm_root();
        // A `pi` executable for `pi_command`/confinement's program set.
        // Its shebang names the REAL discovered node executable — the
        // way a real npm `pi` entrypoint starts `#!…/env node` and
        // `with_interpreter` resolves that interpreter off PATH into
        // the master's read set. `node` lives under
        // `~/.local/share/pi-node`, NOT /usr, so a bare `/bin/sh`
        // shebang would leave node ungranted and the confined probe
        // would fail on the interpreter, never reaching the dependency
        // require under test. The stub never executes.
        let pi = self.path("bin/pi");
        let node = self
            .node_bin
            .canonicalize()
            .unwrap_or_else(|_| self.node_bin.clone());
        std::fs::write(&pi, format!("#!{}\n// pi stub\n", node.display())).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        write_pkg(
            &root.join("pi-ext"),
            concat!(
                r#"{"name":"pi-ext","version":"1.0.0","#,
                r#""pi":{"extensions":["./dist/index.js"]},"#,
                r#""dependencies":{"dep-a":"1.0.0"}}"#,
            ),
            &[("dist/index.js", "module.exports=require('dep-a')\n")],
        );
        write_pkg(
            &root.join("dep-a"),
            r#"{"name":"dep-a","version":"1.0.0","dependencies":{"dep-b":"2.0.0"}}"#,
            &[("index.js", "module.exports=require('dep-b')\n")],
        );
        write_pkg(
            &root.join("dep-b"),
            r#"{"name":"dep-b","version":"2.0.0"}"#,
            &[(
                "index.js",
                "module.exports='declared transitive dependency reachable'\n",
            )],
        );
        write_pkg(
            &root.join("dep-x"),
            r#"{"name":"dep-x","version":"9.9.9"}"#,
            &[(
                "index.js",
                "module.exports='unrelated sibling must stay unreadable'\n",
            )],
        );
        // A private sentinel OUTSIDE the npm root — an escaping resolver
        // or a broadened grant is caught here.
        let private = self.path("private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(
            private.join("sentinel.txt"),
            "private sentinel must stay unreadable\n",
        )
        .unwrap();
        std::fs::write(
            self.path("pm/pm.yaml"),
            "pi:\n  providers: [\"pi-ext@1.0.0\"]\n",
        )
        .unwrap();
        self.store.import_legacy_settings(&self.path("pm")).unwrap();
    }

    /// The real generated master policy for this fixture.
    fn policy(&self) -> cadence_agent::confine::Policy {
        cadence_agent::adapter::pi::pi_master_confinement(&self.env(), &self.path("state")).1
    }

    /// `node -e REQUIRE_PROBE <from> <spec>` inside the real
    /// confinement of this fixture's generated policy — the probe
    /// actually loads the module, so a granted package prints its
    /// export and an ungranted one exits nonzero on EACCES.
    /// `Ok(out)` = node ran; `Err(())` = no guard on this host (the
    /// probe leg is skipped).
    fn confined_node(
        &self,
        policy: &cadence_agent::confine::Policy,
        spec: &str,
    ) -> Result<Output, ()> {
        let from = self.npm_root().join("pi-ext/dist/index.js");
        let out = self
            .confined(
                policy,
                &self.confined_node_path(),
                &["-e", REQUIRE_PROBE, &from.to_string_lossy(), spec],
            )
            .unwrap();
        if String::from_utf8_lossy(&out.stderr).contains("SKIP-NO-GUARD") {
            return Err(());
        }
        Ok(out)
    }

    /// `cadence confine <policy> -- <prog> <args>` — the same real
    /// launcher argv the master runs, but the spawned command is a
    /// probe, not the provider. The policy argv is generated inside a
    /// `/bin/sh -c` wrapper so a host without Landlock (the CLI's
    /// documented pre-exec refusal) is translated to a SKIP-NO-GUARD
    /// marker on stderr instead of a probe failure.
    fn confined(
        &self,
        policy: &cadence_agent::confine::Policy,
        prog: &Path,
        args: &[&str],
    ) -> std::io::Result<Output> {
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let policy_args = policy
            .to_args()
            .iter()
            .map(|a| quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        let probe = std::iter::once(prog.to_string_lossy().into_owned())
            .chain(args.iter().map(|s| s.to_string()))
            .map(|a| quote(&a))
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!(
            "out=$({} confine {} -- {} 2>&1); ec=$?; \
             printf '%s' \"$out\"; \
             if [ $ec -ne 0 ] && printf '%s' \"$out\" | grep -qE 'Landlock|filesystem sandbox'; then \
                 echo SKIP-NO-GUARD >&2; exit 0; fi; \
             printf '%s' \"$out\" >&2; exit $ec",
            quote(BINARY),
            policy_args,
            probe,
        );
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        cmd.env("PATH", self.env().var("PATH").unwrap())
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("state/master/tmp"));
        std::fs::create_dir_all(self.path("state/master/tmp")).unwrap();
        cadence_agent::reaper::output(&mut cmd)
    }
}

fn write_pkg(dir: &Path, manifest: &str, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("package.json"), manifest).unwrap();
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
}

/// The declared direct and transitive dependencies must resolve from the
/// pinned extension under the real Landlock policy; an undeclared sibling
/// and a private sentinel outside the npm root must stay refused.
#[test]
fn pinned_extension_dependency_closure_confined() {
    let _guard = env_lock();
    let Some(host) = Host::new() else {
        eprintln!("SKIP: no node on PATH");
        return;
    };
    host.install_pinned_extension();
    let policy = host.policy();

    // The grant set must name the exact dependency package dirs, never
    // the whole npm root, its parent, or the operator home.
    for dep in ["dep-a", "dep-b"] {
        assert!(
            policy.read.iter().any(|p| p == &host.npm_root().join(dep)),
            "declared dependency {dep} absent from generated policy: {policy:?}"
        );
    }
    for forbidden in [
        host.npm_root(),
        host.path("pi-agent/npm"),
        host.path("pi-agent"),
        host.path("home"),
        host.path("private"),
    ] {
        assert!(
            !policy
                .read
                .iter()
                .any(|p| p == &forbidden || forbidden.starts_with(p)),
            "policy grants a too-broad root {forbidden:?}: {policy:?}"
        );
        assert!(
            !policy
                .write
                .iter()
                .any(|p| p == &forbidden || forbidden.starts_with(p)),
            "policy grants write to {forbidden:?}: {policy:?}"
        );
    }
    assert!(
        !policy
            .read
            .iter()
            .any(|p| p == &host.npm_root().join("dep-x")),
        "undeclared sibling dep-x must not be granted: {policy:?}"
    );

    // The real guard: required imports actually load under
    // confinement — `dep-a`'s export is `dep-b`'s payload string, so a
    // successful require proves direct AND transitive byte reads.
    match host.confined_node(&policy, "dep-a") {
        Err(()) => eprintln!("SKIP-NO-GUARD: Landlock unavailable"),
        Ok(out) => {
            assert!(
                out.status.success(),
                "confined extension could not load declared dep-a: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                String::from_utf8_lossy(&out.stdout)
                    .contains("declared transitive dependency reachable"),
                "dep-a loaded without dep-b's bytes: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }
    if let Ok(out) = host.confined_node(&policy, "dep-b") {
        assert!(
            out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .contains("declared transitive dependency reachable"),
            "transitive dep-b unreadable under confinement: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // …and the same kernel policy still refuses the undeclared
        // sibling's bytes (a bare resolve can stat it; loading cannot).
        let denied = host.confined_node(&policy, "dep-x").unwrap();
        assert!(
            !denied.status.success(),
            "undeclared sibling dep-x loaded under confinement: {}",
            String::from_utf8_lossy(&denied.stdout)
        );
        assert!(
            !String::from_utf8_lossy(&denied.stdout)
                .contains("unrelated sibling must stay unreadable"),
            "private sibling bytes leaked through the guard"
        );
        // The private sentinel outside the npm root stays unreadable
        // under the real kernel guard — not merely absent from the list.
        let sentinel = host.path("private/sentinel.txt");
        let out = host
            .confined(
                &policy,
                Path::new("/bin/cat"),
                &[&sentinel.to_string_lossy()],
            )
            .unwrap();
        if !String::from_utf8_lossy(&out.stderr).contains("SKIP-NO-GUARD") {
            assert!(
                !out.status.success(),
                "confined process read the private sentinel: {}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("Permission denied"),
                "sentinel refusal must be the kernel guard, not a missing file: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

/// Malformed dependency metadata must never widen the grant: a traversal
/// or absolute dependency name, a missing REQUIRED dependency, and an
/// installed dep dir that is a symlink out of the npm root must each
/// leave the npm tree/sentinel ungranted — a fail-closed resolver may
/// refuse the launch outright, but no path outside the declared closure
/// may become readable. A missing OPTIONAL dependency and a dependency
/// cycle resolve without failing and without extra grants. An oversized
/// but otherwise VALID seed package.json (>1 MiB) must likewise refuse:
/// the pinned package's own manifest read is part of bounded dependency
/// metadata discovery, not exempt from it.
#[test]
fn malformed_dependency_metadata_never_widens() {
    let _guard = env_lock();
    let Some(host) = Host::new() else {
        eprintln!("SKIP: no node on PATH");
        return;
    };
    host.install_pinned_extension();
    let npm = host.npm_root();
    let sentinel = host.path("private/sentinel.txt");

    // 1) Traversal and absolute dep names: rewrite the pin's manifest to
    // declare hostile dep names; the generated policy must not grant the
    // sentinel or the npm root/any ancestor of it.
    for bad in [
        "../private",
        "../../..",
        "/abs/evil",
        "@scope/../evil",
        "@@scope/pkg",
    ] {
        std::fs::write(
            npm.join("pi-ext/package.json"),
            format!(
                r#"{{"name":"pi-ext","version":"1.0.0","pi":{{"extensions":["./dist/index.js"]}},"dependencies":{{"dep-a":"1.0.0","{bad}":"1.0.0"}}}}"#
            ),
        )
        .unwrap();
        let policy = host.policy();
        assert!(
            !policy.read.iter().chain(policy.write.iter()).any(|p| {
                // A grant covering the sentinel, or covering the whole
                // npm root/any ancestor of it, is a widened authority.
                sentinel.starts_with(p) || npm.starts_with(p)
            }),
            "malformed dep name {bad:?} widened the policy: {policy:?}"
        );
        // The declared deps still resolve or the whole launch refused —
        // either way no unrelated path is readable.
    }

    // 2) Missing REQUIRED dependency: declare a dep that is not
    // installed. Fail-closed or skipped — never a grant on the root.
    std::fs::write(
        npm.join("pi-ext/package.json"),
        r#"{"name":"pi-ext","version":"1.0.0","pi":{"extensions":["./dist/index.js"]},"dependencies":{"dep-a":"1.0.0","ghost-required":"1.0.0"}}"#,
    )
    .unwrap();
    let policy = host.policy();
    assert!(
        !policy
            .read
            .iter()
            .chain(policy.write.iter())
            .any(|p| npm.starts_with(p)),
        "missing required dep widened the policy: {policy:?}"
    );

    // 3) Symlink escape: first restore a VALID manifest (the leg above
    // left `ghost-required` declared, which would refuse before the
    // symlink is even examined), then replace the installed dep-a dir
    // with a link to the private dir — a resolver that follows it
    // would grant the sentinel. Restore afterwards.
    std::fs::write(
        npm.join("pi-ext/package.json"),
        r#"{"name":"pi-ext","version":"1.0.0","pi":{"extensions":["./dist/index.js"]},"dependencies":{"dep-a":"1.0.0"}}"#,
    )
    .unwrap();
    let dep_a = npm.join("dep-a");
    let staged = npm.join("dep-a.real");
    std::fs::rename(&dep_a, &staged).unwrap();
    std::os::unix::fs::symlink(host.path("private"), &dep_a).unwrap();
    let policy = host.policy();
    assert!(
        !policy
            .read
            .iter()
            .chain(policy.write.iter())
            .any(|p| sentinel.starts_with(p)),
        "symlinked dep dir granted the private tree: {policy:?}"
    );
    std::fs::remove_file(&dep_a).unwrap();
    std::fs::rename(&staged, &dep_a).unwrap();

    // 3b) Dep dir symlinked to the npm ROOT itself: a resolver that
    // only checks `canonical.starts_with(root)` accepts
    // canonical == root and would hand the confined master the entire
    // npm tree. A valid-looking package.json at the root makes the
    // link resolve, so only strict subdir containment stops it.
    std::fs::write(
        npm.join("package.json"),
        r#"{"name":"npm-root","version":"0.0.0"}"#,
    )
    .unwrap();
    std::fs::rename(&dep_a, &staged).unwrap();
    std::os::unix::fs::symlink(&npm, &dep_a).unwrap();
    let policy = host.policy();
    assert!(
        !policy
            .read
            .iter()
            .chain(policy.write.iter())
            .any(|p| npm.starts_with(p)),
        "dep symlinked to the npm root granted the whole tree: {policy:?}"
    );
    std::fs::remove_file(&dep_a).unwrap();
    std::fs::rename(&staged, &dep_a).unwrap();
    std::fs::remove_file(npm.join("package.json")).unwrap();

    // 3c) Dep dir symlinked to a scope PARENT: `<root>/@scope` holds
    // unrelated sibling packages; canonical == the scope dir still
    // passes a bare `starts_with(root)` check, so a scoped-relative
    // grant would expose every `@scope/*` package.
    let scope = npm.join("@scope");
    let scope_other = scope.join("unrelated-pkg");
    write_pkg(
        &scope_other,
        r#"{"name":"@scope/unrelated-pkg","version":"1.0.0"}"#,
        &[(
            "index.js",
            "module.exports='scoped sibling must stay unreadable'\n",
        )],
    );
    std::fs::write(
        scope.join("package.json"),
        r#"{"name":"scope-parent","version":"0.0.0"}"#,
    )
    .unwrap();
    std::fs::rename(&dep_a, &staged).unwrap();
    std::os::unix::fs::symlink(&scope, &dep_a).unwrap();
    let policy = host.policy();
    assert!(
        !policy
            .read
            .iter()
            .chain(policy.write.iter())
            .any(|p| npm.starts_with(p) || scope_other.starts_with(p) || sentinel.starts_with(p)),
        "dep symlinked to a scope parent granted unrelated packages: {policy:?}"
    );
    std::fs::remove_file(&dep_a).unwrap();
    std::fs::rename(&staged, &dep_a).unwrap();

    // 4) Missing OPTIONAL dependency must not fail or broaden: declare
    // an optional dep that is absent; the pinned set must still resolve
    // and grant only declared present deps.
    std::fs::write(
        npm.join("pi-ext/package.json"),
        r#"{"name":"pi-ext","version":"1.0.0","pi":{"extensions":["./dist/index.js"]},"dependencies":{"dep-a":"1.0.0"},"optionalDependencies":{"ghost-optional":"1.0.0"}}"#,
    )
    .unwrap();
    let policy = host.policy();
    assert!(
        policy.read.iter().any(|p| p == &npm.join("dep-a")),
        "declared dep-a must still be granted with an absent optional dep: {policy:?}"
    );
    assert!(
        !policy
            .read
            .iter()
            .any(|p| p == &npm || p == &host.path("home")),
        "absent optional dep must not widen the grant: {policy:?}"
    );

    // 5) Cycle: dep-a ↔ dep-cycle both declare each other; the closure
    // must terminate and stay bounded.
    write_pkg(
        &npm.join("dep-cycle"),
        r#"{"name":"dep-cycle","version":"1.0.0","dependencies":{"dep-a":"1.0.0"}}"#,
        &[("index.js", "module.exports=1\n")],
    );
    std::fs::write(
        npm.join("dep-a/package.json"),
        r#"{"name":"dep-a","version":"1.0.0","dependencies":{"dep-b":"2.0.0","dep-cycle":"1.0.0"}}"#,
    )
    .unwrap();
    let policy = host.policy();
    assert!(
        policy.read.iter().any(|p| p == &npm.join("dep-b")),
        "cyclic closure lost transitive dep-b: {policy:?}"
    );
    assert!(
        policy.read.iter().filter(|p| p.starts_with(&npm)).count() <= 8,
        "cyclic closure exploded the grant set: {policy:?}"
    );
    assert!(
        !policy.read.iter().any(|p| p == &npm),
        "cycle widened the grant to the whole npm root: {policy:?}"
    );
    // And the guarded negative still holds on the cyclic fixture —
    // the undeclared dep-x's module bytes cannot be loaded.
    if let Ok(out) = host.confined_node(&policy, "dep-x") {
        assert!(
            !out.status.success(),
            "undeclared dep-x loaded under the cyclic-closure policy: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout)
                .contains("unrelated sibling must stay unreadable"),
            "dep-x bytes leaked under the cyclic-closure policy"
        );
    }

    // 6) Oversized SEED manifest: the pinned package's own package.json
    // is syntactically valid JSON carrying the pinned version,
    // pi.extensions and a real dependency — padded past the 1 MiB
    // metadata bound by an ignored string field. The old uncapped seed
    // read accepted this file and fed it to the closure walk; the
    // bounded seed read must refuse it, so a fail-closed resolve leaves
    // the pinned provider dir, the npm root, the sentinel and unrelated
    // packages ungranted. Save and restore the valid manifest bytes so
    // the fixture is unchanged afterwards.
    let seed_manifest = npm.join("pi-ext/package.json");
    let saved_manifest = std::fs::read(&seed_manifest).unwrap();
    let padding = "x".repeat(2 * 1024 * 1024);
    std::fs::write(
        &seed_manifest,
        format!(
            r#"{{"name":"pi-ext","version":"1.0.0","pi":{{"extensions":["./dist/index.js"]}},"dependencies":{{"dep-a":"1.0.0"}},"pad":"{padding}"}}"#
        ),
    )
    .unwrap();
    let policy = host.policy();
    for protected in [
        npm.join("pi-ext"),
        npm.join("dep-x"),
        scope_other.clone(),
        npm.clone(),
        sentinel.clone(),
    ] {
        assert!(
            !policy
                .read
                .iter()
                .chain(policy.write.iter())
                .any(|p| protected.starts_with(p)),
            "oversized seed manifest granted {protected:?}: {policy:?}"
        );
    }
    std::fs::write(&seed_manifest, &saved_manifest).unwrap();
}
