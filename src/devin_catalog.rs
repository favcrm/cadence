//! The Devin model catalog for `master_models` (CAD-575) — per-variant
//! `cost_tier`/`label` for the `devin/*` family, resolved without ever
//! blocking a read on the network:
//!
//! 1. The pi-devin extension's own cache
//!    (`$HOME/.cache/pi-devin/models.json`) — it is the artifact
//!    `pi-devin` refreshes itself, free to re-read, so it wins whenever
//!    it parses.
//! 2. `devin models list --format json` — spawned under
//!    `CADENCE_DEVIN_COMMAND` when set (the same override the devin
//!    adapter honors), scrubbed of the daemon's environment, on a hard
//!    timeout so a hung or credential-prompting CLI can never stall
//!    the RPC. Its outcome — success OR failure — is memoized briefly,
//!    so a board polling `/api/master/models` never re-spawns.
//!
//! Every miss answers `None` and the caller reports `"unknown"`.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::adapter::ProviderEnv;

/// `model_uid` → `(cost_tier, label)` for the `devin/*` family.
#[derive(Debug, Default)]
pub struct DevinCatalog {
    tiers: HashMap<String, (String, Option<String>)>,
}

impl DevinCatalog {
    /// `(cost_tier, label)` the catalog lists for `model_uid`.
    pub fn lookup(&self, uid: &str) -> Option<&(String, Option<String>)> {
        self.tiers.get(uid)
    }
}

/// `devin models list` may never outlive this — the catalog is a
/// nicety on a read path, not worth a stalled request.
const DEVIN_LIST_TIMEOUT: Duration = Duration::from_secs(3);
/// A spawned lookup's outcome is reused this long — repeated reads
/// (a polling board) never re-spawn.
const SPAWN_MEMO_TTL: Duration = Duration::from_secs(120);

/// `$HOME/.cache/pi-devin/models.json` — where pi-devin persists the
/// catalog it fetched.
fn cache_file(env: &ProviderEnv) -> Option<PathBuf> {
    env.var("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".cache/pi-devin/models.json"))
}

/// `families[].variants[]` → the lookup. Both the bare document and a
/// `{"catalog": {...}}` wrapper parse; a variant without `cost_tier`
/// still enters the map so its `label` is usable.
fn parse(doc: &Value) -> DevinCatalog {
    let root = doc.get("catalog").unwrap_or(doc);
    let mut tiers = HashMap::new();
    for family in root
        .get("families")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for variant in family
            .get("variants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(uid) = variant.get("model_uid").and_then(Value::as_str) {
                let tier = variant
                    .get("cost_tier")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let label = variant
                    .get("label")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                tiers.insert(uid.to_string(), (tier, label));
            }
        }
    }
    DevinCatalog { tiers }
}

fn read_cache_file(env: &ProviderEnv) -> Option<DevinCatalog> {
    let text = std::fs::read_to_string(cache_file(env)?).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .map(|doc| parse(&doc))
}

/// `devin models list --format json` — spawned on a scrubbed
/// environment (PATH/HOME/TMPDIR only), drained concurrently so a
/// large catalog cannot deadlock the pipe, and killed at the timeout.
/// A command that cannot be resolved, exits badly, or runs out of
/// time answers `None`.
fn spawn_devin_models(env: &ProviderEnv) -> Option<DevinCatalog> {
    let argv: Vec<String> = env
        .var("CADENCE_DEVIN_COMMAND")
        .filter(|c| !c.trim().is_empty())
        .map(|c| c.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|| vec!["devin".to_string()]);
    // A bare program name resolves against the daemon's PATH now —
    // the child runs on the scrubbed environment above.
    let program = argv.first()?;
    let program = if program.contains('/') {
        program.clone()
    } else {
        crate::adapter::pty::resolve_on_path(program).ok()?
    };
    let mut cmd = Command::new(program);
    cmd.args(&argv[1..])
        .args(["models", "list", "--format", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();
    for name in ["PATH", "HOME", "TMPDIR"] {
        if let Some(v) = env.var(name) {
            cmd.env(name, v);
        }
    }
    let mut child = crate::reaper::spawn(&mut cmd).ok()?;
    // The catalog outweighs a pipe buffer — a blocked writer is
    // indistinguishable from a hang, so drain on a thread. A killed
    // child's grandchildren can keep the pipe open, so the read joins
    // on a channel under the same deadline, never on `join` alone.
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if out.read_to_end(&mut buf).is_ok() {
            let _ = tx.send(buf);
        }
    });
    let deadline = Instant::now() + DEVIN_LIST_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Err(_) => break None,
        }
    };
    if status.map(|s| s.success()) != Some(true) {
        return None;
    }
    let grace = deadline + Duration::from_millis(500);
    let stdout = rx
        .recv_timeout(grace.saturating_duration_since(Instant::now()))
        .ok()?;
    serde_json::from_slice::<Value>(&stdout)
        .ok()
        .map(|doc| parse(&doc))
}

/// The daemon's memoized catalog. The pi-devin cache file wins on
/// every call — it is free and always fresh; only the spawned CLI
/// path memoizes, failure included, so a missing or wedged `devin`
/// binary costs one 3s spawn per [`SPAWN_MEMO_TTL`], never per read.
#[derive(Default)]
pub struct CatalogCache(Mutex<Option<(Instant, Option<Arc<DevinCatalog>>)>>);

impl CatalogCache {
    /// The catalog if any source answers — `None` degrades the caller
    /// to `"unknown"` tiers, never to an error.
    pub fn catalog(&self, env: &ProviderEnv) -> Option<Arc<DevinCatalog>> {
        if let Some(catalog) = read_cache_file(env) {
            let catalog = Arc::new(catalog);
            *self.0.lock().unwrap() = Some((Instant::now(), Some(Arc::clone(&catalog))));
            return Some(catalog);
        }
        let mut memo = self.0.lock().unwrap();
        if let Some((at, catalog)) = &*memo {
            if at.elapsed() < SPAWN_MEMO_TTL {
                return catalog.clone();
            }
        }
        let catalog = spawn_devin_models(env).map(Arc::new);
        *memo = Some((Instant::now(), catalog.clone()));
        catalog
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(pairs: &[(&str, &str)]) -> ProviderEnv {
        let env = ProviderEnv::default();
        for (k, v) in pairs {
            env.set(k, v.to_string());
        }
        env
    }

    fn write_cache(home: &std::path::Path, doc: &str) {
        let dir = home.join(".cache/pi-devin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("models.json"), doc).unwrap();
    }

    const CATALOG: &str = r#"{"families":[{"name":"swe","variants":[
        {"model_uid":"swe-2-high","label":"SWE 2 High","cost_tier":"Free"},
        {"model_uid":"swe-2","label":"SWE 2","cost_tier":"Paid"},
        {"model_uid":"notier"}
    ]}]}"#;

    #[test]
    fn the_pi_devin_cache_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        write_cache(dir.path(), CATALOG);
        let env = env_with(&[("HOME", &dir.path().to_string_lossy())]);
        let cache = CatalogCache::default();
        let catalog = cache.catalog(&env).unwrap();
        assert_eq!(
            catalog.lookup("swe-2-high").map(|(t, _)| t.as_str()),
            Some("Free")
        );
        assert_eq!(
            catalog.lookup("swe-2").map(|(_, l)| l.as_deref()),
            Some(Some("SWE 2"))
        );
        assert_eq!(
            catalog.lookup("notier").map(|(t, _)| t.as_str()),
            Some("unknown")
        );
        assert!(catalog.lookup("ghost").is_none());
    }

    /// A `{"catalog": …}` wrapper parses the same as the bare doc.
    #[test]
    fn a_wrapped_catalog_parses() {
        let dir = tempfile::tempdir().unwrap();
        write_cache(dir.path(), &format!(r#"{{"catalog":{CATALOG}}}"#));
        let env = env_with(&[("HOME", &dir.path().to_string_lossy())]);
        let catalog = CatalogCache::default().catalog(&env).unwrap();
        assert_eq!(
            catalog.lookup("swe-2-high").map(|(t, _)| t.as_str()),
            Some("Free")
        );
    }

    /// No cache file: `CADENCE_DEVIN_COMMAND` is spawned, and its
    /// outcome memoized — repeated reads never re-spawn (the child
    /// gets a scrubbed env, so the stub embeds its own counter path).
    #[test]
    fn the_spawned_lookup_memoizes() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let stub = dir.path().join("devin-stub.sh");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho x >> \"{}\"\nprintf '%s' '{{\"families\":[]}}'\n",
                calls.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = env_with(&[
            ("HOME", &dir.path().to_string_lossy()),
            ("CADENCE_DEVIN_COMMAND", &format!("sh {}", stub.display())),
        ]);
        let cache = CatalogCache::default();
        assert!(cache.catalog(&env).is_some());
        assert!(cache.catalog(&env).is_some());
        assert!(cache.catalog(&env).is_some());
        let ran = std::fs::read_to_string(&calls).unwrap_or_default();
        assert_eq!(ran.lines().count(), 1, "each read must not re-spawn");
        // A command that cannot run at all degrades, does not error.
        let env = env_with(&[
            ("HOME", &dir.path().to_string_lossy()),
            ("CADENCE_DEVIN_COMMAND", "/definitely/not/devin"),
        ]);
        assert!(CatalogCache::default().catalog(&env).is_none());
    }

    /// A wedged `devin` is killed at the timeout and answers `None` —
    /// a read can never stall on it.
    #[test]
    fn a_hung_cli_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("devin-hang.sh");
        std::fs::write(&stub, "#!/bin/sh\nsleep 60\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = env_with(&[
            ("HOME", &dir.path().to_string_lossy()),
            ("CADENCE_DEVIN_COMMAND", &format!("sh {}", stub.display())),
        ]);
        let start = Instant::now();
        assert!(CatalogCache::default().catalog(&env).is_none());
        assert!(
            start.elapsed() < DEVIN_LIST_TIMEOUT + Duration::from_secs(2),
            "the timeout must bound the wait"
        );
    }
}
