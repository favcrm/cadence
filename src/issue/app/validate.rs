//! Bundle validation helpers for `issue::app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use cadence_core::app_compat::{self, Compat};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::issue::{app_view, workflow};

use super::grammar::{file_cap, MAX_APP_BYTES, MAX_FILES};
use super::{
    bundle_files, parse_manifest, skill_package, skill_packages, workflow_slots, Manifest, MANIFEST,
};

/// Integrity-checked package bytes and observations, not execution authority.
/// Native validation additionally enforces the actual host requirements.
pub(crate) struct Validated {
    pub(crate) manifest: Manifest,
    pub(crate) files: Vec<(String, String)>,
    pub(crate) secret_warnings: Vec<crate::secret::Finding>,
    pub(crate) notes: Vec<String>,
    pub(crate) compatibility: Value,
}

/// Read a bundle's files as UTF-8 text and validate them: the manifest
/// parses, every workflow passes the CAD-487 `workflow check` (same
/// `check_text`), every `uses:` names a declared slot, and the secret
/// guard runs over each file — an install refuses on any error.
pub(crate) fn validate(
    root: &Path,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Validated> {
    let paths = bundle_files(root)?;
    let mut files = Vec::with_capacity(paths.len());
    let mut secret_warnings = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (rel, path) in paths {
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::rejected(format!("cannot read app file {rel}: {e}")))?;
        if bytes.len() as u64 > file_cap(&rel) {
            return Err(Error::rejected(format!(
                "{rel} is {} bytes — a file is at most {}",
                bytes.len(),
                file_cap(&rel)
            )));
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                errors.push(format!(
                    "{rel}: not UTF-8 text — an A1 app carries text only"
                ));
                continue;
            }
        };
        match crate::secret::guard(&format!("app file {rel}"), &text) {
            Ok(findings) => secret_warnings.extend(findings),
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
        files.push((rel, text));
    }
    validate_contents(
        files,
        secret_warnings,
        errors,
        agents,
        agent_sources,
        CompatibilityCheck::RequireNative,
    )
}

/// Validate an immutable descriptor-read snapshot before workspace publication.
pub(crate) fn validate_texts(
    files: Vec<(String, String)>,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Validated> {
    validate_snapshot(
        files,
        agents,
        agent_sources,
        CompatibilityCheck::RequireNative,
    )
}

/// Inspect immutable bytes without claiming that their target contracts are
/// implemented by Native. Unknown contract payload semantics remain target-owned.
pub(crate) fn inspect_texts(
    files: Vec<(String, String)>,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Validated> {
    validate_snapshot(
        files,
        agents,
        agent_sources,
        CompatibilityCheck::ReportNative,
    )
}

enum CompatibilityCheck {
    RequireNative,
    ReportNative,
}

fn validate_snapshot(
    files: Vec<(String, String)>,
    agents: &HashSet<String>,
    agent_sources: &[String],
    compatibility: CompatibilityCheck,
) -> Result<Validated> {
    if files.len() > MAX_FILES {
        return Err(Error::rejected("app bundle exceeds its file limit"));
    }
    if files.iter().map(|(_, text)| text.len() as u64).sum::<u64>() > MAX_APP_BYTES {
        return Err(Error::rejected(format!(
            "app bundle exceeds its {MAX_APP_BYTES}-byte aggregate limit"
        )));
    }
    let mut warnings = Vec::new();
    // Portable inspection must not consult a machine's state or inherit its
    // operator exceptions. Native validation retains its existing allowlist.
    let no_host_exceptions = crate::secret::Allowlist::default();
    for (name, text) in &files {
        let cap = file_cap(name);
        if text.len() as u64 > cap {
            return Err(Error::rejected(format!(
                "app bundle file {name:?} exceeds its {cap}-byte limit"
            )));
        }
        let findings = match &compatibility {
            CompatibilityCheck::RequireNative => {
                crate::secret::guard(&format!("app file {name}"), text)?
            }
            CompatibilityCheck::ReportNative => {
                crate::secret::guard_with(&format!("app file {name}"), text, &no_host_exceptions)?
            }
        };
        warnings.extend(findings);
    }
    validate_contents(
        files,
        warnings,
        Vec::new(),
        agents,
        agent_sources,
        compatibility,
    )
}

/// Storage members and their requirement are one package contract, including
/// when already-installed bytes are explicitly approved again.
pub fn validate_storage_members<'a>(
    manifest: &Manifest,
    paths: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let declared = manifest.requires.contracts.get("app-storage");
    let mut root = false;
    let mut members = false;
    for path in paths {
        root |= path == "app-storage.json";
        members |= path.starts_with("storage/");
    }
    if (root || members) && declared.is_none_or(|majors| majors.as_slice() != [1]) {
        return Err(Error::rejected(
            "app storage members require needs.requires.contracts.app-storage: [1]",
        ));
    }
    if declared.is_some() && !root {
        return Err(Error::rejected("app-storage requires app-storage.json"));
    }
    Ok(())
}

fn validate_contents(
    files: Vec<(String, String)>,
    secret_warnings: Vec<crate::secret::Finding>,
    mut errors: Vec<String>,
    agents: &HashSet<String>,
    agent_sources: &[String],
    compatibility: CompatibilityCheck,
) -> Result<Validated> {
    let manifest = match files.iter().find(|(rel, _)| rel == MANIFEST) {
        Some((_, text)) => match parse_manifest(text) {
            Ok(manifest) => manifest,
            Err(e) => {
                errors.push(e.to_string());
                // Without a manifest the slot check cannot run; report
                // the rest anyway.
                Manifest {
                    app: String::new(),
                    title: String::new(),
                    version: String::new(),
                    connections: vec![],
                    capabilities: BTreeMap::new(),
                    summary: None,
                    view_contract: None,
                    requires: Compat::legacy(),
                    listing: None,
                    i18n: None,
                    guide: String::new(),
                }
            }
        },
        None => Manifest {
            app: String::new(),
            title: String::new(),
            version: String::new(),
            connections: vec![],
            capabilities: BTreeMap::new(),
            summary: None,
            view_contract: None,
            requires: Compat::legacy(),
            listing: None,
            i18n: None,
            guide: String::new(),
        },
    };
    if let Err(error) =
        validate_storage_members(&manifest, files.iter().map(|(path, _)| path.as_str()))
    {
        errors.push(error.to_string());
    }
    // This bundle's own bytes as one `rel-path → text` map. The skill and
    // screen member groups are both validated over it, and run creation
    // resolves a skill package from the same shape of map.
    let bundle_map: BTreeMap<String, String> = files.iter().cloned().collect();
    // CAD-1129 §3: every asset `listing` references must be in this
    // bundle's own inventory — `assets/` flat *.svg, nothing outside.
    if let Some(listing) = &manifest.listing {
        for asset in &listing.assets {
            if !files.iter().any(|(rel, _)| rel == asset) {
                errors.push(format!(
                    "listing asset '{asset}' is not in the bundle's assets/"
                ));
            }
        }
    }
    // CAD-1110: a package whose chat descriptor breaks the app-chat/v1
    // grammar is refused at install and update, not at first use.
    if let Some((_, text)) = files
        .iter()
        .find(|(rel, _)| rel == crate::issue::app_chat::FILE)
    {
        if let Err(e) = crate::issue::app_chat::validate(text, &manifest.app) {
            errors.push(e.to_string());
        }
    }
    if let Some((_, text)) = files
        .iter()
        .find(|(rel, _)| rel == crate::issue::app_action::FILE)
    {
        if let Err(e) = crate::issue::app_action::validate_customer_bundle(text, &manifest.app) {
            errors.push(e.to_string());
        }
    }
    if let Some((_, text)) = files
        .iter()
        .find(|(rel, _)| rel == crate::issue::app_assistant::FILE)
    {
        match crate::issue::app_assistant::validate(text) {
            Ok(descriptor)
                if descriptor
                    .app
                    .as_deref()
                    .is_none_or(|app| app == manifest.app) => {}
            Ok(_) => errors.push(format!(
                "{} declares a different app name",
                crate::issue::app_assistant::FILE
            )),
            Err(e) => errors.push(format!("{}: {e}", crate::issue::app_assistant::FILE)),
        }
    }
    // CAD-864: the descriptor and its manifest declaration pair up —
    // either alone refuses. A declared `needs.views` with no
    // `views/app-views-v1.json` (or the reverse) is a bundle that
    // misstates what it ships. When both exist the descriptor parses
    // under the app-views/v1 gate and its `app` is this manifest's app:
    // the descriptor was reviewed *with* this bundle, and its
    // provenance can never borrow another app's views.
    let descriptor_file = files
        .iter()
        .find(|(rel, _)| rel == app_view::REL_PATH)
        .map(|(_, text)| text.as_str());
    match (manifest.view_contract.as_deref(), descriptor_file) {
        (Some(app_view::CONTRACT), Some(text)) => match app_view::parse_descriptor(text) {
            Ok(descriptor) if descriptor.app == manifest.app => {}
            Ok(descriptor) => errors.push(format!(
                "{}: descriptor `app` is '{}' — it must be this bundle's '{}'",
                app_view::REL_PATH,
                descriptor.app,
                manifest.app
            )),
            Err(e) => errors.push(format!("{}: {e}", app_view::REL_PATH)),
        },
        (Some(_), None) => errors.push(format!(
            "app.md declares `needs.views` but the bundle carries no {} — the \
             declaration and the descriptor file install together or not at all",
            app_view::REL_PATH
        )),
        (None, Some(_)) => errors.push(format!(
            "{} is present but app.md never declares `needs.views.contract` — an \
             undeclared descriptor can never install",
            app_view::REL_PATH
        )),
        (None, None) => {}
        // An unknown contract value already failed `parse_manifest`.
        (Some(_), Some(_)) => {}
    }
    let mut notes = Vec::new();
    let mut workflow_count = 0usize;
    for (rel, text) in &files {
        // `bundle_files` admits only `workflows/<tag>.md` under that top.
        if !rel.starts_with("workflows/") {
            continue;
        }
        workflow_count += 1;
        // CAD-1867: an installed bundle's workflow MAY declare `skill:` —
        // the tag is resolved against this bundle's own packages below, and
        // run creation re-proves it through the same `skill_package`.
        let (errs, ns, _) = workflow::check_installed_text(text, agents, agent_sources);
        errors.extend(errs.into_iter().map(|e| format!("{rel}: {e}")));
        notes.extend(ns.into_iter().map(|n| format!("{rel}: {n}")));
        if let Ok(template) = workflow::parse_template(text) {
            if let Some(slot) = template.publication_slot {
                if !manifest.capabilities.contains_key(&slot) {
                    errors.push(format!(
                        "{rel}: publication_slot '{slot}' is not declared in needs.capabilities"
                    ));
                }
            }
        }
        match workflow_slots(text) {
            Ok(slots) => {
                for slot in slots {
                    if !manifest.connections.iter().any(|s| s == &slot) {
                        errors.push(format!(
                            "{rel}: `uses: {slot}` — the app does not declare that slot; \
                             add it to `needs.connections` in app.md so the binding is \
                             the operator's choice, not the bundle's"
                        ));
                    }
                }
            }
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
        match workflow::skill_tags(text) {
            Ok(tags) => {
                for tag in tags.into_iter().flatten() {
                    if let Err(e) = skill_package(&bundle_map, &tag) {
                        errors.push(format!("{rel}: `skill: {tag}` — {e}"));
                    }
                }
            }
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
    }
    // CAD-1867: every `skills/<tag>/` package the bundle carries must be
    // complete on its own terms — its manifest, the declared set equal to the
    // present set, the caps, and every reference its declared text files make
    // to package material. One validator, `skill_package`, so install and run
    // creation can never disagree.
    match skill_packages(&bundle_map) {
        Ok(_) => {}
        Err(e) => errors.push(e.to_string()),
    }
    // CAD-1270 item 3: every `screens/<tag>/` member group is a screen
    // package — integrity-check each through the same
    // `app_screen_pkg::extract` → `app_screen_decl::validate_map` the
    // mount RPC re-runs, so a malformed declaration, an
    // undeclared/tampered asset or a tag dir without `screens.json`
    // refuses at install, not first mount. `tags_in` additionally
    // enforces the ≤8 declared-package bound; the member-prefix scan
    // also collects an orphan tag dir (no screens.json), which
    // `tags_in` cannot see. This shared check is what install,
    // install-check and the offline `app check` all inherit — no second
    // parser.
    {
        let screen_map = &bundle_map;
        if let Err(e) = crate::issue::app_screen_pkg::tags_in(screen_map) {
            errors.push(format!("screens: {e}"));
        }
        let mut candidates: Vec<&str> = Vec::new();
        for rel in screen_map.keys() {
            if let Some(rest) = rel.strip_prefix("screens/") {
                if let Some((tag, _)) = rest.split_once('/') {
                    if !candidates.contains(&tag) {
                        candidates.push(tag);
                    }
                }
            }
        }
        for tag in candidates {
            match crate::issue::app_screen_pkg::extract(screen_map, tag) {
                // Same binding the mount RPC enforces: the package's
                // declared `app` must be this bundle's manifest app —
                // a package cannot claim a different app.
                Ok(pkg) if pkg.app != manifest.app => errors.push(format!(
                    "screens/{tag}: screen package declares an app that is not the installation"
                )),
                Err(e) => errors.push(format!("screens/{tag}: {e}")),
                Ok(_) => {}
            }
        }
    }
    // CAD-1177: an app may ship no workflows only when it is a genuine
    // standalone-tools plugin — it carries at least one integrity-checked
    // `app-screens/v2` screen whose declared `tools` map resolves every
    // alias to a real `needs.capabilities` slot. That is the v2
    // no-workflow-file path: the tool, not a run, is the unit of work.
    // An empty app, a screen that fails integrity, or a tool whose slot
    // the manifest never declares cannot substitute for workflows and
    // still refuses.
    if workflow_count == 0 && !has_tools_only_screen(&files, &manifest) {
        errors.push("workflows/ holds no *.md workflow".to_string());
    }
    let host = app_compat::host(env!("CARGO_PKG_VERSION"))?;
    let compatibility = match compatibility {
        CompatibilityCheck::ReportNative => manifest.requires.report(&host),
        CompatibilityCheck::RequireNative => match manifest.requires.check(&host) {
            Ok(receipt) => receipt,
            Err(error) => {
                errors.push(error.to_string());
                Value::Null
            }
        },
    };
    if !errors.is_empty() {
        return Err(Error::rejected(format!(
            "app is not installable — {}",
            errors.join("; ")
        )));
    }
    Ok(Validated {
        manifest,
        files,
        secret_warnings,
        notes,
        compatibility,
    })
}

/// CAD-1177: whether the bundle carries a genuine standalone-tools
/// screen — an integrity-checked `app-screens/v2` package whose
/// declared `tools` map is non-empty AND resolves every alias to a
/// `needs.capabilities` slot with a `read`/`draft` effect (the only
/// effects a standalone, approval-free invoke may run). Its `may` is
/// `tools.invoke`, optionally plus the exact `media.read` permission a
/// `screen-media.v1` screen declares (CAD-1365). This is the
/// install-time proof that lets a workflow-free plugin install; it is
/// deliberately strict so an empty or malformed bundle cannot borrow
/// the exception. Returns false on any integrity/parse failure — a
/// broken screen never satisfies the exception (its own error is
/// reported by the screen validators elsewhere).
fn has_tools_only_screen(files: &[(String, String)], manifest: &Manifest) -> bool {
    let map: BTreeMap<String, String> = files.iter().cloned().collect();
    let Ok(tags) = crate::issue::app_screen_pkg::tags_in(&map) else {
        return false;
    };
    tags.iter().any(|tag| {
        let Ok(pkg) = crate::issue::app_screen_pkg::extract(&map, tag) else {
            return false;
        };
        pkg.contract == "app-screens/v2"
            // The screen must be FOR this app — a package declaring a
            // different app can never satisfy this bundle's exception.
            && pkg.app == manifest.app
            && !pkg.tools.is_empty()
            // `tools.invoke` alone, plus — only under the media host
            // contract — the exact `media.read`/`media.publish`
            // declarations. No other `may` member and no media right on
            // an actions-contract screen qualifies.
            && pkg.may.iter().all(|m| {
                m == crate::issue::app_screen_decl::MAY_TOOLS_INVOKE
                    || ((m == crate::issue::app_screen_decl::MAY_MEDIA_READ
                        || m == crate::issue::app_screen_decl::MAY_MEDIA_PUBLISH
                        || m == crate::issue::app_screen_decl::MAY_MEDIA_IMPORT)
                        && pkg.host_contract.as_deref()
                            == Some(crate::issue::app_screen_decl::HOST_CONTRACT_MEDIA))
            })
            && pkg.tools.values().all(|slot| {
                manifest
                    .capabilities
                    .get(slot)
                    .is_some_and(|decl| matches!(decl.effect.as_str(), "read" | "draft"))
            })
    })
}
