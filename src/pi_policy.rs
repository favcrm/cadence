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
            parse_package_pin(spec).map(|_| ()).map_err(|why| {
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
    /// `<root>/<name>` canonicalized — the real package dir, proven to
    /// sit under the canonical npm root; read-only.
    pub dir: PathBuf,
    /// The package's `pi.extensions` entries — canonical paths proven
    /// inside `dir`.
    pub entries: Vec<PathBuf>,
}

/// `name@version` — the version pin is the part after the LAST `@`
/// (`@scope/name@1.2` parses too); npm package names never contain
/// `..`, an empty segment, or characters outside `[A-Za-z0-9._-]`
/// plus the one scoping `@`/`/`. The optional `#sha256-<64 hex>`
/// fragment is the content pin (CAD-572): the digest every launch
/// verifies the installed files against.
fn parse_package_pin(spec: &str) -> Result<(&str, &str, Option<&str>)> {
    let Some((name, rest)) = spec.rsplit_once('@') else {
        return Err(Error::rejected(
            "expected 'name@version' (a pin is required)",
        ));
    };
    let (version, digest) = match rest.split_once('#') {
        Some((version, digest)) => (version, Some(digest)),
        None => (rest, None),
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
    if let Some(digest) = digest {
        let hex = digest.strip_prefix("sha256-").unwrap_or("");
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::rejected(
                "the content pin must be '#sha256-<64 lowercase hex>' (CAD-572)",
            ));
        }
    }
    Ok((name, version, digest))
}

/// The per-file bound [`package_digest`] reads — a package carrying a
/// bigger file refuses rather than making every launch hash it.
const DIGEST_FILE_CAP: u64 = 8 << 20;

/// Deterministic content digest of a package dir: `sha256-<64 hex>` —
/// the integrity half of a pin (CAD-572). The walk is
/// order-independent: relative paths sorted byte-wise; a file feeds
/// `b"f" <rel> NUL <len LE u64> <bytes>`, a symlink `b"l" <rel> NUL
/// <target>` (never followed — the target string is the content);
/// directories contribute nothing, so an empty dir is not content.
/// `node_modules/` is excluded by design: the pin covers the
/// package's own files, not its dependency tree.
pub fn package_digest(dir: &Path) -> Result<String> {
    use sha2::Digest as _;
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = std::fs::read_dir(&d).map_err(|e| {
            Error::rejected(format!("package digest: cannot read {}: {e}", d.display()))
        })?;
        for ent in rd {
            let path = ent
                .map_err(|e| {
                    Error::rejected(format!("package digest: cannot read {}: {e}", d.display()))
                })?
                .path();
            let rel = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if rel.split('/').any(|seg| seg == "node_modules") {
                continue;
            }
            let md = std::fs::symlink_metadata(&path).map_err(|e| {
                Error::rejected(format!(
                    "package digest: cannot stat {}: {e}",
                    path.display()
                ))
            })?;
            if md.is_dir() {
                stack.push(path);
            } else {
                entries.push((rel, path));
            }
        }
    }
    entries.sort();
    let mut h = sha2::Sha256::new();
    for (rel, path) in entries {
        let md = std::fs::symlink_metadata(&path).map_err(|e| {
            Error::rejected(format!(
                "package digest: cannot stat {}: {e}",
                path.display()
            ))
        })?;
        if md.file_type().is_symlink() {
            let target = std::fs::read_link(&path).map_err(|e| {
                Error::rejected(format!(
                    "package digest: cannot read link {}: {e}",
                    path.display()
                ))
            })?;
            h.update(b"l");
            h.update(rel.as_bytes());
            h.update([0]);
            h.update(target.to_string_lossy().as_bytes());
        } else if md.is_file() {
            if md.len() > DIGEST_FILE_CAP {
                return Err(Error::rejected(format!(
                    "package digest: {} exceeds the {DIGEST_FILE_CAP}-byte bound",
                    path.display()
                )));
            }
            let bytes = std::fs::read(&path).map_err(|e| {
                Error::rejected(format!(
                    "package digest: cannot read {}: {e}",
                    path.display()
                ))
            })?;
            h.update(b"f");
            h.update(rel.as_bytes());
            h.update([0]);
            h.update(md.len().to_le_bytes());
            h.update(&bytes);
        } else {
            return Err(Error::rejected(format!(
                "package digest: {} is not a regular file",
                path.display()
            )));
        }
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256-{hex}"))
}

/// Resolve one `[pi].providers` pin under the operator's npm
/// `node_modules` root: the package must be installed, at exactly the
/// pinned version, declare `pi.extensions` entry files inside its own
/// dir, and — when the pin carries a `#sha256-…` fragment (CAD-572) —
/// hash to that content digest. Any drift refuses — nothing is loaded
/// "close enough".
pub fn resolve_package(spec: &str, root: &Path) -> Result<ProviderPackage> {
    let (name, pin, digest) = parse_package_pin(spec)?;
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
    // The lexical path can hide a symlink escape: `dir` (or an ancestor)
    // linked outside the npm root resolves `package.json` and entries
    // through the link while the string path still names the root.
    // Canonicalize and require containment — defence in depth; today the
    // root is operator-chosen, so this is reachable only at install
    // time as the operator (CAD-568).
    let root_canon = std::fs::canonicalize(root).map_err(|_| {
        Error::rejected(format!(
            "pi npm root {} does not resolve — install the pinned package \
             under it as the operator first (CAD-568)",
            root.display()
        ))
    })?;
    let dir = std::fs::canonicalize(&dir).map_err(|_| {
        Error::rejected(format!(
            "pi provider package '{spec}': package dir {} does not resolve",
            dir.display()
        ))
    })?;
    if !dir.starts_with(&root_canon) {
        return Err(Error::rejected(format!(
            "pi provider package '{spec}': package dir escapes the npm root \
             {} via a symlink — refusing (CAD-568)",
            root_canon.display()
        )));
    }
    // The integrity half of the pin (CAD-572): a version string cannot
    // see a hand-edited file — the content digest can. Only a pin that
    // carries one is checked; the refusal names both digests so the
    // operator can re-pin deliberately after an intentional change.
    if let Some(expected) = digest {
        let found = package_digest(&dir)?;
        if found != expected {
            return Err(Error::rejected(format!(
                "pi provider package '{spec}': package content drifted from the \
                 pin (pinned {expected}, installed {found}) — reinstall the \
                 pinned package or, after an intentional change, update the \
                 digest in pm.yaml [pi].providers (CAD-572)"
            )));
        }
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

/// A `pi.extensions` entry resolved inside its (canonical) package
/// dir: relative only, `..`/absolute paths refuse, the file must
/// exist, and its canonical path must stay inside `dir` — a symlinked
/// entry or intermediate dir cannot escape the pin. The returned path
/// is the canonical target.
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
    // `is_file` follows links — a symlinked entry (or intermediate dir)
    // inside the package can point anywhere. Canonicalize and require
    // the target to stay inside the canonical package dir (CAD-568).
    let canon = std::fs::canonicalize(&path).map_err(|_| {
        Error::rejected(format!(
            "pi.extensions entry '{rel}' does not resolve at {}",
            path.display()
        ))
    })?;
    if !canon.starts_with(dir) {
        return Err(Error::rejected(format!(
            "pi.extensions entry '{rel}' escapes the package dir via a \
             symlink (resolves to {})",
            canon.display()
        )));
    }
    Ok(canon)
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

    #[cfg(unix)]
    #[test]
    fn a_symlinked_package_dir_cannot_escape_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        std::fs::create_dir_all(&root).unwrap();
        // A real, valid package — but installed OUTSIDE the pinned npm
        // root and linked in. Lexically `root/pi-devin` looks installed;
        // the resolved dir escapes, so the pin must refuse.
        let outside = tmp.path().join("elsewhere/pi-devin");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("package.json"),
            r#"{"version":"0.1.2","pi":{"extensions":["./e.ts"]}}"#,
        )
        .unwrap();
        std::fs::write(outside.join("e.ts"), "// ext").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("pi-devin")).unwrap();
        let err = resolve_package("pi-devin@0.1.2", &root)
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_entry_cannot_escape_the_package_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        // The package is installed at the pin; its declared entry is a
        // symlink to a file outside the package dir — `is_file` follows
        // it, so only canonicalization catches the escape.
        let outside = tmp.path().join("payload.ts");
        std::fs::write(&outside, "// not the pinned package's file").unwrap();
        let dir = root.join("evil");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"version":"1.0.0","pi":{"extensions":["./entry.ts"]}}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("entry.ts")).unwrap();
        let err = resolve_package("evil@1.0.0", &root)
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes"), "{err}");
        // A symlinked INTERMEDIATE dir is the same escape.
        let real_ext = tmp.path().join("real-ext");
        std::fs::create_dir_all(&real_ext).unwrap();
        std::fs::write(real_ext.join("x.ts"), "// ext").unwrap();
        let dir2 = root.join("evil2");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(
            dir2.join("package.json"),
            r#"{"version":"1.0.0","pi":{"extensions":["./sub/x.ts"]}}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&real_ext, dir2.join("sub")).unwrap();
        assert!(resolve_package("evil2@1.0.0", &root).is_err());
    }

    /// CAD-572: `name@version#sha256-<hex>` — the content pin. The
    /// hand-edit case is the ticket's own: a one-line change under
    /// `src/` that leaves package.json's version alone.
    #[test]
    fn a_content_pin_refuses_a_hand_edited_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        install(
            &root,
            "pi-devin",
            r#"{"version":"0.2.1","pi":{"extensions":["./extensions/index.ts"]}}"#,
            &["extensions/index.ts", "src/stream.ts"],
        );
        let dir = root.join("pi-devin");
        let digest = package_digest(&dir).unwrap();
        let spec = format!("pi-devin@0.2.1#{digest}");
        // A clean install at the pin resolves.
        assert!(resolve_package(&spec, &root).is_ok());
        // The hand-edit: same version, different bytes.
        std::fs::write(dir.join("src/stream.ts"), "// hand-edited in place").unwrap();
        let err = resolve_package(&spec, &root).unwrap_err().to_string();
        assert!(err.contains("drifted"), "{err}");
        assert!(
            err.contains(&digest),
            "the refusal names the pinned digest so the operator can compare: {err}"
        );
        // Restoring the byte-identical content passes again.
        std::fs::write(dir.join("src/stream.ts"), "// ext").unwrap();
        assert!(resolve_package(&spec, &root).is_ok());
    }

    #[test]
    fn added_or_removed_files_change_the_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        install(
            &root,
            "pi-devin",
            r#"{"version":"0.2.1","pi":{"extensions":["./e.ts"]}}"#,
            &["e.ts"],
        );
        let dir = root.join("pi-devin");
        let digest = package_digest(&dir).unwrap();
        let spec = format!("pi-devin@0.2.1#{digest}");
        assert!(resolve_package(&spec, &root).is_ok());
        // A planted file — a loader the version string cannot see.
        std::fs::write(dir.join("extra.ts"), "// planted").unwrap();
        assert!(resolve_package(&spec, &root).is_err());
        std::fs::remove_file(dir.join("extra.ts")).unwrap();
        assert!(resolve_package(&spec, &root).is_ok());
        // A removed file refuses too.
        std::fs::remove_file(dir.join("e.ts")).unwrap();
        assert!(resolve_package(&spec, &root).is_err());
    }

    #[test]
    fn the_content_pin_ignores_node_modules() {
        // The documented scope: the pin covers the package's own files,
        // not its dependency tree (pinning deps is a different pin).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        install(
            &root,
            "pi-devin",
            r#"{"version":"0.2.1","pi":{"extensions":["./e.ts"]}}"#,
            &["e.ts"],
        );
        let dir = root.join("pi-devin");
        std::fs::create_dir_all(dir.join("node_modules/dep")).unwrap();
        std::fs::write(dir.join("node_modules/dep/index.js"), "// dep").unwrap();
        let digest = package_digest(&dir).unwrap();
        let spec = format!("pi-devin@0.2.1#{digest}");
        assert!(resolve_package(&spec, &root).is_ok());
        std::fs::write(dir.join("node_modules/dep/index.js"), "// swapped").unwrap();
        assert!(
            resolve_package(&spec, &root).is_ok(),
            "node_modules is out of the pin's scope — documented, not silent"
        );
        // And a later recording excludes it too: the digest is stable
        // across the swap.
        assert_eq!(package_digest(&dir).unwrap(), digest);
    }

    #[test]
    fn a_malformed_content_pin_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        install(
            &root,
            "pi-devin",
            r#"{"version":"0.1.2","pi":{"extensions":["./e.ts"]}}"#,
            &["e.ts"],
        );
        for spec in [
            "pi-devin@0.1.2#",
            "pi-devin@0.1.2#sha256",
            "pi-devin@0.1.2#md5-0123456789abcdef0123456789abcdef",
            "pi-devin@0.1.2#sha256-XYZ",
            "pi-devin@0.1.2#sha256-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
            "pi-devin@0.1.2#sha256-0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(resolve_package(spec, &root).is_err(), "{spec}");
        }
        // The same grammar gates pm.yaml at read time.
        let good = format!(
            "pi:\n  providers: [\"pi-devin@0.1.2#sha256-{}\"]\n",
            "0".repeat(64)
        );
        let (_d, pm) = pm_with(&good);
        assert_eq!(read(&pm).unwrap().unwrap().providers.len(), 1);
        let (_d, pm) = pm_with("pi:\n  providers: [\"pi-devin@0.1.2#md5-abc\"]\n");
        let err = read(&pm).unwrap_err().to_string();
        assert!(err.contains("pi-devin@0.1.2#md5-abc"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn the_digest_covers_symlink_targets() {
        // A link inside the package is content: its target string is
        // hashed, not followed — a re-pointed link changes the digest.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        install(
            &root,
            "linked",
            r#"{"version":"1.0.0","pi":{"extensions":["./alias.ts","./real/inner.ts"]}}"#,
            &["real/index.ts", "real/inner.ts"],
        );
        let dir = root.join("linked");
        std::os::unix::fs::symlink("real/index.ts", dir.join("alias.ts")).unwrap();
        let digest = package_digest(&dir).unwrap();
        let spec = format!("linked@1.0.0#{digest}");
        assert!(resolve_package(&spec, &root).is_ok());
        // Re-point the link at another internal file — still contained,
        // still different content.
        std::fs::remove_file(dir.join("alias.ts")).unwrap();
        std::os::unix::fs::symlink("real/inner.ts", dir.join("alias.ts")).unwrap();
        assert!(resolve_package(&spec, &root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn an_internal_symlinked_entry_still_resolves() {
        // Defence in depth is containment, not a symlink ban: an entry
        // link whose target stays inside the package dir resolves.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules");
        let dir = root.join("linked");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"version":"1.0.0","pi":{"extensions":["./alias.ts","./real/inner.ts"]}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("real/index.ts"), "// ext").unwrap();
        std::fs::write(dir.join("real/inner.ts"), "// ext").unwrap();
        std::os::unix::fs::symlink("real/index.ts", dir.join("alias.ts")).unwrap();
        let pkg = resolve_package("linked@1.0.0", &root).unwrap();
        assert_eq!(pkg.entries.len(), 2);
        for entry in &pkg.entries {
            let canon = std::fs::canonicalize(&dir).unwrap();
            assert!(entry.starts_with(&canon), "{entry:?} escaped {canon:?}");
        }
    }
}
