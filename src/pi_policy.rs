//! `pm.yaml [pi]` — the operator-owned Pi policy (CAD-559).
//!
//! Pi silently falls back: an unavailable model becomes another
//! provider's, and an unpinned extension loads whatever the operator's
//! npm dir happens to hold. The production master once fell back to a
//! paid OpenRouter model this way. The policy closes both holes:
//!
//! ```yaml
//! pi:
//!   providers: ["pi-devin@0.1.2"]           # npm package@version pins
//!   models:
//!     allow: ["openrouter/z-ai/glm-5.3-flash", "devin/swe-2-high"]
//!     master_allow: ["openrouter/z-ai/glm-5.3-flash"]   # optional (CAD-575)
//!     worker_allow: ["devin/swe-2-high"]              # optional (CAD-575)
//!     default: { master: "openrouter/z-ai/glm-5.3-flash", worker: "devin/swe-2-high" }
//! ```
//!
//! - `models.allow` is the vocabulary a managed pi agent may launch
//!   on — `master start`, `join`, `agent set --next-launch` and the
//!   adapter's own launch all check it. `models.master_allow` and
//!   `models.worker_allow` (CAD-575) narrow it per role — a confined
//!   master cannot run `devin/*` models the workers can (the pi-devin
//!   extension shells out to a Devin CLI the sandbox cannot
//!   credential). A role list that is present replaces `allow` for
//!   that role, including a present-but-empty one; a role without its
//!   own list falls back to `allow` unchanged. An absent `[pi]`
//!   allows nothing: every pi launch refuses until the operator pins
//!   a list.
//! - `models.default.{master,worker}` is the role fallback when no
//!   explicit `--model` (and no `model_defaults` role entry) names one.
//!   There is no "provider default" for pi — that is the silent
//!   fallback this module exists to remove.
//! - `providers` lists the only Pi extension packages that load, each
//!   pinned `name@version` and resolved under the operator's own Pi
//!   npm dir (`<PI_CODING_AGENT_DIR|~/.pi/agent>/npm/node_modules`). A
//!   version drift or a missing package refuses the launch rather than
//!   load what is there.
//!
//! The reader follows [`crate::doctor::host`]'s `pm.yaml` contract:
//! absent file or table is `None`; a present-but-unusable table is an
//! error naming the culprit key, never a silent default.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};

/// `[pi]` in pm.yaml — everything optional individually; the table's
/// absence means "no pi policy", which the model gate treats as an
/// empty allowlist.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiPolicy {
    /// `name@version` pins for the provider packages Pi may `-e`.
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub models: PiModels,
}

/// `[pi.models]` — the launch vocabulary and the per-role fallback.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiModels {
    /// The only `provider/id` values a pi agent may launch on — the
    /// fallback both role lists override.
    #[serde(default)]
    pub allow: Vec<String>,
    /// CAD-575: the master's own vocabulary — `Some` replaces `allow`
    /// for the master outright (an empty list allows it nothing);
    /// `None` falls back to `allow`. A confined master cannot run
    /// `devin/*` — the pi-devin extension shells out to a Devin CLI
    /// the sandbox cannot credential.
    pub master_allow: Option<Vec<String>>,
    /// CAD-575: the same for workers — every pi agent that is not the
    /// master (pm-role agents share `worker`, like the role defaults).
    pub worker_allow: Option<Vec<String>>,
    #[serde(default)]
    pub default: PiRoleDefaults,
}

impl PiModels {
    /// The list `role` launches under: its own `master_allow` /
    /// `worker_allow` when pm.yaml carries one, else `allow` —
    /// [`crate::pi_policy::PiRoleDefaults::for_role`] treats every
    /// non-master role as `worker`, and so does this.
    pub fn allow_for(&self, role: &str) -> &[String] {
        match role {
            "master" => self.master_allow.as_deref().unwrap_or(&self.allow),
            _ => self.worker_allow.as_deref().unwrap_or(&self.allow),
        }
    }

    /// The pm.yaml key `role`'s list came from — named in refusals so
    /// a rejected launch points at the list the operator must edit.
    pub fn allow_key(&self, role: &str) -> &'static str {
        match role {
            "master" if self.master_allow.is_some() => "master_allow",
            "master" => "allow",
            _ if self.worker_allow.is_some() => "worker_allow",
            _ => "allow",
        }
    }
}

/// `[pi.models.default]` — a fallback per cadence role. A pm-role pi
/// agent shares `worker`: the master is the only role of its own.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiRoleDefaults {
    pub master: Option<String>,
    pub worker: Option<String>,
}

impl PiRoleDefaults {
    pub fn for_role(&self, role: &str) -> Option<&str> {
        match role {
            "master" => self.master.as_deref(),
            _ => self.worker.as_deref(),
        }
    }
}

/// Read `[pi]` from `<pm_dir>/pm.yaml`: absent file or table →
/// `Ok(None)`; a malformed table → `Err` naming the culprit key (the
/// same per-key retry `read_host_overrides` uses — serde_yaml's error
/// does not name the offending field).
pub fn read(pm_dir: &Path) -> Result<Option<PiPolicy>> {
    let Ok(text) = std::fs::read_to_string(pm_dir.join("pm.yaml")) else {
        return Ok(None);
    };
    let yaml: serde_yaml::Value = serde_yaml::from_str(&text)
        .map_err(|e| Error::rejected(format!("pm.yaml is not valid YAML: {e}")))?;
    let Some(pi) = yaml.get("pi") else {
        return Ok(None);
    };
    let policy: PiPolicy = serde_yaml::from_value(pi.clone()).map_err(|e| {
        let culprit = pi.as_mapping().and_then(|m| {
            m.iter().find_map(|(k, v)| {
                let mut one = serde_yaml::Mapping::new();
                one.insert(k.clone(), v.clone());
                serde_yaml::from_value::<PiPolicy>(serde_yaml::Value::Mapping(one))
                    .is_err()
                    .then(|| k.as_str().unwrap_or("?").to_string())
            })
        });
        match culprit {
            Some(key) => Error::rejected(format!("pm.yaml [pi] {key}: {e}")),
            None => Error::rejected(format!("pm.yaml [pi]: {e}")),
        }
    })?;
    policy.validate()?;
    Ok(Some(policy))
}

impl PiPolicy {
    /// Every entry is checked at read time — a malformed id or pin is
    /// a config error the operator fixes once, not a per-launch miss.
    fn validate(&self) -> Result<()> {
        for model in self
            .models
            .allow
            .iter()
            .chain(self.models.master_allow.iter().flatten())
            .chain(self.models.worker_allow.iter().flatten())
            .chain(self.models.default.master.iter())
            .chain(self.models.default.worker.iter())
        {
            check_model_id(model)
                .map_err(|why| Error::rejected(format!("pm.yaml [pi] model '{model}': {why}")))?;
        }
        for spec in &self.providers {
            parse_package_pin(spec).map_err(|why| {
                Error::rejected(format!("pm.yaml [pi].providers '{spec}': {why}"))
            })?;
        }
        Ok(())
    }
}

/// A pi model spec is whatever `pi --model` accepts — `provider/id` or
/// a bare id — so the grammar is the daemon's own model-id rule
/// (`model_defaults::validate_model_id`), not a narrower one.
fn check_model_id(model: &str) -> Result<()> {
    // The allowlist is exact-match — an entry that needs a trim would
    // validate yet never match, so the typo is refused here.
    if model.trim() != model {
        return Err(Error::rejected(
            "model ids must not be padded with whitespace",
        ));
    }
    crate::model_defaults::validate_model_id(model)
        .map(|_| ())
        .map_err(|e| Error::rejected(format!("{e}")))
}

/// The model a pi `<role>` agent launches with (`role` is `master` or
/// `worker`). An explicit model wins; else the role's
/// `[pi].models.default`; else refuse — pi's own fallback chain is
/// never used. The winner must be on `models.allow`.
pub fn resolve_model(
    policy: Option<&PiPolicy>,
    role: &str,
    explicit: Option<&str>,
) -> Result<String> {
    let configured = explicit
        .map(str::to_string)
        .or_else(|| policy.and_then(|p| p.models.default.for_role(role).map(str::to_string)));
    let Some(model) = configured else {
        return Err(Error::rejected(format!(
            "a pi {role} has no model — pass --model <provider/id> or set \
             pi.models.default.{role} in pm.yaml; pi's own provider fallback \
             is never used (CAD-559)"
        )));
    };
    require_allowed(policy, role, &model)?;
    Ok(model)
}

/// `model` must be on the allowlist `role` launches under — the role's
/// own `master_allow`/`worker_allow` when pm.yaml carries one, else
/// `allow` (CAD-575). The refusal lists the pinned ids and names the
/// key so a rejected request names its fix; an absent `[pi]` (or an
/// empty applicable list) allows nothing.
pub fn require_allowed(policy: Option<&PiPolicy>, role: &str, model: &str) -> Result<()> {
    let key = policy.map(|p| p.models.allow_key(role)).unwrap_or("allow");
    let allow: &[String] = policy.map(|p| p.models.allow_for(role)).unwrap_or(&[]);
    if allow.iter().any(|m| m == model) {
        return Ok(());
    }
    let listed = if allow.is_empty() {
        format!("nothing — pm.yaml has no [pi].models.{key} entries")
    } else {
        allow.join(", ")
    };
    Err(Error::rejected(format!(
        "pi {role} model '{model}' is not on the operator's allowlist (allowed: \
         {listed}) — the operator pins the pi model set in pm.yaml \
         [pi].models.{key} (CAD-559, CAD-575)"
    )))
}

/// One operator-pinned provider package: its install dir (the confined
/// master's read grant) and the extension files `-e` receives.
#[derive(Debug)]
pub struct ProviderPackage {
    /// `<root>/<name>` — the whole package dir, read-only.
    pub dir: PathBuf,
    /// The package's `pi.extensions` entries, normalized inside `dir`.
    pub entries: Vec<PathBuf>,
}

/// `name@version` — the version pin is the part after the LAST `@`
/// (`@scope/name@1.2` parses too); npm package names never contain
/// `..`, an empty segment, or characters outside `[A-Za-z0-9._-]`
/// plus the one scoping `@`/`/`.
fn parse_package_pin(spec: &str) -> Result<(&str, &str)> {
    let Some((name, version)) = spec.rsplit_once('@') else {
        return Err(Error::rejected(
            "expected 'name@version' (a pin is required)",
        ));
    };
    let name_ok = {
        let segments: Vec<&str> = name.split('/').collect();
        let well_formed = if let Some(rest) = name.strip_prefix('@') {
            // scoped: exactly "@scope/name"
            !rest.is_empty() && segments.len() == 2
        } else {
            segments.len() == 1
        };
        let seg = |s: &str| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        };
        well_formed && segments.iter().all(|s| seg(s.trim_start_matches('@')))
    };
    let version_ok = !version.is_empty()
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    if !name_ok || !version_ok {
        return Err(Error::rejected(
            "expected 'name@version' or '@scope/name@version'",
        ));
    }
    Ok((name, version))
}

/// Resolve one `[pi].providers` pin under the operator's npm
/// `node_modules` root: the package must be installed, at exactly the
/// pinned version, and declare `pi.extensions` entry files inside its
/// own dir. Any drift refuses — nothing is loaded "close enough".
pub fn resolve_package(spec: &str, root: &Path) -> Result<ProviderPackage> {
    let (name, pin) = parse_package_pin(spec)?;
    let dir = root.join(name);
    let manifest = dir.join("package.json");
    let text = std::fs::read_to_string(&manifest).map_err(|_| {
        Error::rejected(format!(
            "pi provider package '{spec}' is not installed under {} — install \
             it as the operator first (`pi install {name}@{pin}`) (CAD-559)",
            root.display()
        ))
    })?;
    let pkg: Value = serde_json::from_str(&text).map_err(|e| {
        Error::rejected(format!(
            "pi provider package '{spec}': package.json is not valid JSON: {e}"
        ))
    })?;
    let installed = pkg.get("version").and_then(Value::as_str).unwrap_or("");
    if installed != pin {
        return Err(Error::rejected(format!(
            "pi provider package '{name}' is installed at version '{installed}', \
             not the pinned '{pin}' — align the install or the pin in pm.yaml \
             [pi].providers (CAD-559)"
        )));
    }
    let declared = pkg
        .get("pi")
        .and_then(|pi| pi.get("extensions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if declared.is_empty() {
        return Err(Error::rejected(format!(
            "pi provider package '{spec}' declares no pi.extensions — nothing \
             would load; fix the package or drop the pin"
        )));
    }
    let mut entries = Vec::with_capacity(declared.len());
    for entry in &declared {
        let Some(rel) = entry.as_str() else {
            return Err(Error::rejected(format!(
                "pi provider package '{spec}': pi.extensions entries must be strings"
            )));
        };
        entries.push(
            entry_file(&dir, rel)
                .map_err(|e| Error::rejected(format!("pi provider package '{spec}': {e}")))?,
        );
    }
    Ok(ProviderPackage { dir, entries })
}

/// A `pi.extensions` entry resolved inside its package dir: relative
/// only, `..`/absolute paths refuse, and the file must exist — an `-e`
/// that points nowhere would otherwise fail as an opaque Pi error.
fn entry_file(dir: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(Error::rejected(format!(
            "pi.extensions entry '{rel}' must be relative to the package dir"
        )));
    }
    let mut path = dir.to_path_buf();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => {
                return Err(Error::rejected(format!(
                    "pi.extensions entry '{rel}' escapes the package dir"
                )))
            }
        }
    }
    if !path.is_file() {
        return Err(Error::rejected(format!(
            "pi.extensions entry '{rel}' does not exist at {}",
            path.display()
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pm_with(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pm.yaml"), text).unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    #[test]
    fn absent_file_or_table_is_no_policy() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).unwrap().is_none());
        let (_d, pm) = pm_with("schema: 1\n");
        assert!(read(&pm).unwrap().is_none());
        let (_d, pm) = pm_with("host:\n  agent_gc_older_than_secs: 60\n");
        assert!(read(&pm).unwrap().is_none());
    }

    #[test]
    fn a_full_table_parses() {
        let (_d, pm) = pm_with(
            "pi:\n  providers: [\"pi-devin@0.1.2\", \"@acme/tool@2.0.0\"]\n  models:\n    allow: [\"openrouter/z-ai/glm-5.3-flash\", \"devin/swe-2-high\"]\n    default:\n      master: \"openrouter/z-ai/glm-5.3-flash\"\n      worker: \"devin/swe-2-high\"\n",
        );
        let policy = read(&pm).unwrap().unwrap();
        assert_eq!(policy.providers, vec!["pi-devin@0.1.2", "@acme/tool@2.0.0"]);
        assert_eq!(
            policy.models.allow,
            vec!["openrouter/z-ai/glm-5.3-flash", "devin/swe-2-high"]
        );
        assert_eq!(
            policy.models.default.master.as_deref(),
            Some("openrouter/z-ai/glm-5.3-flash")
        );
        assert_eq!(
            policy.models.default.worker.as_deref(),
            Some("devin/swe-2-high")
        );
    }

    #[test]
    fn a_bad_key_or_entry_names_the_culprit() {
        let (_d, pm) = pm_with("pi:\n  provdiders: [\"x@1\"]\n");
        let err = read(&pm).unwrap_err().to_string();
        assert!(err.contains("provdiders"), "{err}");

        let (_d, pm) = pm_with("pi:\n  models:\n    allow: [\" padded \"]\n");
        let err = read(&pm).unwrap_err().to_string();
        assert!(err.contains(" padded "), "{err}");

        let (_d, pm) = pm_with("pi:\n  providers: [\"unpinned\"]\n");
        let err = read(&pm).unwrap_err().to_string();
        assert!(err.contains("unpinned"), "{err}");

        let (_d, pm) = pm_with("pi: [\n");
        assert!(read(&pm).is_err());
    }

    #[test]
    fn resolve_model_is_explicit_then_role_default_then_refuse() {
        let (_d, pm) = pm_with(
            "pi:\n  models:\n    allow: [\"a/m1\", \"a/m2\"]\n    default:\n      master: \"a/m1\"\n      worker: \"a/m2\"\n",
        );
        let policy = read(&pm).unwrap();
        let p = policy.as_ref();
        assert_eq!(resolve_model(p, "worker", Some("a/m1")).unwrap(), "a/m1");
        // The role default fills an absent explicit choice; a pm-role
        // agent shares the worker default.
        assert_eq!(resolve_model(p, "worker", None).unwrap(), "a/m2");
        assert_eq!(resolve_model(p, "pm", None).unwrap(), "a/m2");
        assert_eq!(resolve_model(p, "master", None).unwrap(), "a/m1");
        // Explicit beats the default but still must be allowlisted.
        let err = resolve_model(p, "worker", Some("a/m3"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("a/m3") && err.contains("a/m1"), "{err}");
        // No explicit and no default for the role… worker has one here —
        // use a policy without defaults for the refuse leg.
        let (_d2, pm2) = pm_with("pi:\n  models:\n    allow: [\"a/m1\"]\n");
        let p2 = read(&pm2).unwrap();
        let err = resolve_model(p2.as_ref(), "worker", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("models.default.worker"), "{err}");
        // No [pi] at all: nothing is allowed — not even an explicit id.
        let err = resolve_model(None, "worker", Some("a/m1"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowlist"), "{err}");
    }

    /// CAD-575: `master_allow`/`worker_allow` parse, override `allow`
    /// per role (a present-but-empty role list allows that role
    /// nothing — it does NOT mean `allow`), and a role without its own
    /// list falls back to `allow` untouched.
    #[test]
    fn per_role_lists_override_and_fall_back() {
        let (_d, pm) = pm_with(
            "pi:\n  models:\n    allow: [\"a/shared-1\"]\n    master_allow: [\"a/master-only\"]\n    default:\n      master: \"a/master-only\"\n      worker: \"a/shared-1\"\n",
        );
        let policy = read(&pm).unwrap().unwrap();
        // The master's list replaces allow; the worker keeps it.
        assert_eq!(
            policy.models.allow_for("master"),
            &["a/master-only".to_string()]
        );
        for role in ["worker", "pm"] {
            assert_eq!(policy.models.allow_for(role), &["a/shared-1".to_string()]);
        }
        // The master may not launch on a model only `allow` offers,
        // and the worker may not launch on `master_allow`'s.
        let err = require_allowed(Some(&policy), "master", "a/shared-1")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("master") && err.contains("master_allow"),
            "{err}"
        );
        let err = require_allowed(Some(&policy), "worker", "a/master-only")
            .unwrap_err()
            .to_string();
        assert!(err.contains("worker") && err.contains("allow"), "{err}");
        // resolve_model gates the role default the same way — a master
        // default that is only on `allow` (not master_allow) refuses.
        let (_d2, pm2) = pm_with(
            "pi:\n  models:\n    allow: [\"a/m1\"]\n    master_allow: [\"a/m2\"]\n    default:\n      master: \"a/m1\"\n",
        );
        let p2 = read(&pm2).unwrap();
        let err = resolve_model(p2.as_ref(), "master", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("a/m1") && err.contains("master_allow"),
            "{err}"
        );
    }

    /// An explicitly empty role list is not "no role list": it allows
    /// that role nothing even though `allow` is populated — `None` vs
    /// `Some(vec![])` is the only difference and it must not collapse.
    #[test]
    fn an_empty_role_list_allows_nothing() {
        let (_d, pm) = pm_with("pi:\n  models:\n    allow: [\"a/m1\"]\n    master_allow: []\n");
        let policy = read(&pm).unwrap().unwrap();
        assert!(policy.models.allow_for("master").is_empty());
        let err = require_allowed(Some(&policy), "master", "a/m1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("master_allow"), "{err}");
        // The worker still falls back to `allow`.
        require_allowed(Some(&policy), "worker", "a/m1").unwrap();
        assert_eq!(policy.models.allow_key("worker"), "allow");
    }

    /// `worker_allow` narrows the worker side the same way.
    #[test]
    fn worker_allow_narrows_the_worker_side() {
        let (_d, pm) = pm_with(
            "pi:\n  models:\n    allow: [\"a/shared-1\", \"a/shared-2\"]\n    worker_allow: [\"a/shared-2\"]\n",
        );
        let policy = read(&pm).unwrap().unwrap();
        require_allowed(Some(&policy), "worker", "a/shared-2").unwrap();
        let err = require_allowed(Some(&policy), "worker", "a/shared-1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("worker_allow"), "{err}");
        // The master never reads `worker_allow` — it keeps `allow`.
        require_allowed(Some(&policy), "master", "a/shared-1").unwrap();
        assert_eq!(policy.models.allow_key("master"), "allow");
    }

    /// A malformed id inside a role list is caught at read, like an
    /// `allow` entry.
    #[test]
    fn role_lists_validate_at_read() {
        let (_d, pm) = pm_with("pi:\n  models:\n    master_allow: [\" bad \"]\n");
        let err = read(&pm).unwrap_err().to_string();
        assert!(err.contains(" bad "), "{err}");
    }

    fn install(root: &Path, name: &str, manifest: &str, files: &[&str]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), manifest).unwrap();
        for f in files {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "// ext").unwrap();
        }
    }

    #[test]
    fn a_pinned_package_resolves_to_its_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        install(
            &root,
            "pi-devin",
            r#"{"version":"0.1.2","pi":{"extensions":["./extensions/index.ts"]}}"#,
            &["extensions/index.ts"],
        );
        install(
            &root,
            "@acme/tool",
            r#"{"version":"2.0.0","pi":{"extensions":["a.js","b/c.js"]}}"#,
            &["a.js", "b/c.js"],
        );
        let pkg = resolve_package("pi-devin@0.1.2", &root).unwrap();
        assert_eq!(pkg.dir, root.join("pi-devin"));
        assert_eq!(pkg.entries, vec![root.join("pi-devin/extensions/index.ts")]);
        let scoped = resolve_package("@acme/tool@2.0.0", &root).unwrap();
        assert_eq!(
            scoped.entries,
            vec![root.join("@acme/tool/a.js"), root.join("@acme/tool/b/c.js")]
        );
    }

    #[test]
    fn package_drift_and_escape_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        install(
            &root,
            "pi-devin",
            r#"{"version":"0.2.0","pi":{"extensions":["./e.ts"]}}"#,
            &["e.ts"],
        );
        // Version drift — installed 0.2.0, pinned 0.1.2.
        let err = resolve_package("pi-devin@0.1.2", &root)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("0.2.0") && err.contains("0.1.2"), "{err}");
        // Not installed at all.
        assert!(resolve_package("ghost@1.0.0", &root).is_err());
        // No extensions declared.
        std::fs::write(root.join("pi-devin/package.json"), r#"{"version":"0.1.2"}"#).unwrap();
        assert!(resolve_package("pi-devin@0.1.2", &root).is_err());
        // An entry escaping the package dir refuses even when the file exists.
        install(
            &root,
            "evil",
            r#"{"version":"1.0.0","pi":{"extensions":["../outside.ts"]}}"#,
            &[],
        );
        let err = resolve_package("evil@1.0.0", &root)
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes"), "{err}");
        // A missing entry file refuses.
        install(
            &root,
            "missing",
            r#"{"version":"1.0.0","pi":{"extensions":["nope.ts"]}}"#,
            &[],
        );
        assert!(resolve_package("missing@1.0.0", &root).is_err());
        // Bad specs never reach the filesystem.
        for spec in ["", "pkg", "pkg@", "@1.0", "@/x@1", "../x@1", "a/b@1"] {
            assert!(resolve_package(spec, &root).is_err(), "{spec}");
        }
    }
}
