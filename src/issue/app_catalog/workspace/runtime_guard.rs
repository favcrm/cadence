//! Runtime bundle snapshot guards, lock-free reads, and soft-remove/restore/update helpers.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeMap;
use std::path::Path;

use cadence_core::app_compat;
use serde_json::{json, Value};

use super::super::fs::Root;
use super::super::{
    builtin, catalogue, client, hash, no_pending, yaml, Catalog, InstallationId, Storage,
    ADMISSION_CHANGED,
};
use super::{bundle_digest, describe, install_resolved, read_catalog, snapshot};
use crate::error::{Error, Result};
use crate::issue::app::{self, Record};
use crate::issue::{write, Pm};
use crate::store::Store;

/// Evidence of a verified installation snapshot while its PM lock is held.
/// It cannot be constructed by callers or retained after the callback.
pub struct RuntimeSnapshot<'a> {
    install: &'a str,
    digest: &'a str,
    files: &'a BTreeMap<String, String>,
    #[cfg(any(test, all(debug_assertions, feature = "store-test-support")))]
    fixture: bool,
}

impl RuntimeSnapshot<'_> {
    pub fn install(&self) -> &str {
        self.install
    }
    pub fn digest(&self) -> &str {
        self.digest
    }
    #[cfg(any(test, all(debug_assertions, feature = "store-test-support")))]
    #[cfg_attr(not(test), doc(hidden))]
    pub fn store_fixture() -> RuntimeSnapshot<'static> {
        static FILES: std::sync::LazyLock<BTreeMap<String, String>> =
            std::sync::LazyLock::new(BTreeMap::new);
        RuntimeSnapshot {
            install: "",
            digest: "",
            files: &FILES,
            fixture: true,
        }
    }
    #[cfg(any(test, all(debug_assertions, feature = "store-test-support")))]
    #[cfg_attr(not(test), doc(hidden))]
    pub fn is_store_fixture(&self) -> bool {
        self.fixture
    }
    pub fn declares_read_or_draft(&self, slot: &str) -> bool {
        self.files
            .get("app.md")
            .and_then(|text| app::parse_manifest(text).ok())
            .is_some_and(|manifest| {
                manifest.capabilities.get(slot).is_some_and(|need| {
                    need.validate().is_ok() && matches!(need.effect.as_str(), "read" | "draft")
                })
            })
    }
}

pub fn with_runtime_snapshot_proof<T>(
    pm: &Pm,
    id: &str,
    callback: impl for<'a> FnOnce(&Value, &BTreeMap<String, String>, &RuntimeSnapshot<'a>) -> Result<T>,
) -> Result<T> {
    with_runtime_snapshot(pm, id, |description, files| {
        let digest = description["digest"]
            .as_str()
            .ok_or_else(|| Error::rejected("installation digest is missing"))?;
        let proof = RuntimeSnapshot {
            install: id,
            digest,
            files,
            #[cfg(any(test, all(debug_assertions, feature = "store-test-support")))]
            fixture: false,
        };
        callback(description, files, &proof)
    })
}

pub fn with_runtime_snapshot_pair_proof<T>(
    pm: &Pm,
    caller: &str,
    producer: &str,
    callback: impl for<'a> FnOnce(
        (&Value, &BTreeMap<String, String>, &RuntimeSnapshot<'a>),
        (&Value, &BTreeMap<String, String>),
    ) -> Result<T>,
) -> Result<T> {
    with_runtime_snapshot_pair(pm, caller, producer, |(description, files), producer| {
        let digest = description["digest"]
            .as_str()
            .ok_or_else(|| Error::rejected("installation digest is missing"))?;
        let proof = RuntimeSnapshot {
            install: caller,
            digest,
            files,
            #[cfg(any(test, all(debug_assertions, feature = "store-test-support")))]
            fixture: false,
        };
        callback((description, files, &proof), producer)
    })
}

/// Runtime admission uses the same descriptor-confined installation lookup.
/// The callback runs under PM -> SQLite lock ordering, never the reverse.
#[track_caller]
pub fn with_runtime_snapshot<T>(
    pm: &Pm,
    id: &str,
    callback: impl FnOnce(&Value, &BTreeMap<String, String>) -> Result<T>,
) -> Result<T> {
    let id = InstallationId::parse(id)?;
    let _lock = pm.lock()?;
    let (description, files) = snapshot_under_lock(pm, &id)?;
    callback(&description, &files)
}

/// CAD-1365: the caller's runtime snapshot AND a second install's, read
/// under ONE held PM lock. The callback receives `(caller, producer)`.
/// Both reads run the same checks as `with_runtime_snapshot`; the second
/// read never re-enters `read_catalog`, so an odd legacy generation cannot
/// refuse it while the caller already holds the lock.
pub fn with_runtime_snapshot_pair<T>(
    pm: &Pm,
    caller: &str,
    producer: &str,
    callback: impl FnOnce(
        (&Value, &BTreeMap<String, String>),
        (&Value, &BTreeMap<String, String>),
    ) -> Result<T>,
) -> Result<T> {
    let caller = InstallationId::parse(caller)?;
    let producer = InstallationId::parse(producer)?;
    let _lock = pm.lock()?;
    let (cd, cf) = snapshot_under_lock(pm, &caller)?;
    if producer == caller {
        return callback((&cd, &cf), (&cd, &cf));
    }
    let (pd, pf) = snapshot_under_lock(pm, &producer)?;
    callback((&cd, &cf), (&pd, &pf))
}

/// The checks of `with_runtime_snapshot`, run by the caller that already
/// holds the PM lock. Returns the install's description and bundle files.
fn snapshot_under_lock(pm: &Pm, id: &InstallationId) -> Result<(Value, BTreeMap<String, String>)> {
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    no_pending(&root)?;
    let description = describe(&root, &catalog, id)?;
    // CAD-1129 H5: a soft-removed install admits no runtime snapshot —
    // run creation, dispatch and consent checks all stop here.
    if description["removed"].as_i64().is_some() {
        return Err(Error::rejected(
            "installation is removed — restore it before any app action",
        ));
    }
    let (bundle, _) = catalog.installations[id].paths(id);
    let files = snapshot(&root, &bundle, false)?;
    if bundle_digest(&files) != description["digest"].as_str().unwrap_or("") {
        return Err(Error::invalid(
            ADMISSION_CHANGED,
            "installation changed during runtime admission",
        ));
    }
    Ok((description, files))
}

/// CAD-1212: test seam for `with_runtime_read`. A check installs a hook that
/// runs after `describe` and before the bundle `snapshot` (once per retry
/// attempt), so it can modify a bundle file at exactly the point the digest
/// compare guards. Compiled to nothing outside `cfg(test)`.
pub(crate) mod read_seam {
    #[cfg(test)]
    use std::cell::RefCell;

    #[cfg(test)]
    type Hook = Box<dyn FnMut()>;

    #[cfg(test)]
    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    #[inline]
    pub(super) fn between_describe_and_snapshot() {
        #[cfg(test)]
        HOOK.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f();
            }
        });
    }

    /// Install (or clear) the callback for this thread.
    #[cfg(test)]
    pub(crate) fn set_hook(hook: Option<Hook>) {
        HOOK.with(|h| *h.borrow_mut() = hook);
    }
}

/// CAD-1189: lock-free runtime read for callbacks that write nothing. It
/// makes the same checks as `with_runtime_snapshot` (pending journal,
/// catalog generation, record re-read, bundle-digest compare) under the
/// optimistic retry, but takes no PM lock. A callback that creates or
/// changes state must use `with_runtime_snapshot`, which the PM lock orders
/// against install, upgrade, remove and revoke.
pub fn with_runtime_read<T>(
    pm: &Pm,
    id: &str,
    callback: impl FnOnce(&Value, &BTreeMap<String, String>) -> Result<T>,
) -> Result<T> {
    let id = InstallationId::parse(id)?;
    let (description, files) = read_catalog(pm, || {
        let root = Root::open(&pm.dir)?;
        let catalog = Catalog::load(&pm.dir)?;
        no_pending(&root)?;
        let description = describe(&root, &catalog, &id)?;
        read_seam::between_describe_and_snapshot();
        // CAD-1129 H5: the lock-free read funnel refuses a soft-removed
        // install exactly like `with_runtime_snapshot`.
        if description["removed"].as_i64().is_some() {
            return Err(Error::rejected(
                "installation is removed — restore it before any app action",
            ));
        }
        let (bundle, _) = catalog.installations[&id].paths(&id);
        let files = snapshot(&root, &bundle, false)?;
        if bundle_digest(&files) != description["digest"].as_str().unwrap_or("") {
            return Err(Error::invalid(
                ADMISSION_CHANGED,
                "installation changed during runtime admission",
            ));
        }
        Ok((description, files))
    })?;
    callback(&description, &files)
}

/// Read an immutable installed revision for completed work. The current
/// catalog still proves the installation identity; only exact retained bundle
/// bytes may supply the old app contract. Active runs always use the current
/// runtime snapshot above.
#[track_caller]
pub fn with_completed_bundle_snapshot<T>(
    pm: &Pm,
    id: &str,
    digest: &str,
    callback: impl FnOnce(&Value, &BTreeMap<String, String>) -> Result<T>,
) -> Result<T> {
    let id = InstallationId::parse(id)?;
    let revision = digest
        .strip_prefix("sha256:")
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
        .ok_or_else(|| Error::rejected("historical bundle digest is invalid"))?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    no_pending(&root)?;
    let current = describe(&root, &catalog, &id)?;
    // A soft-removed install admits no historical snapshot either —
    // completed-work receipts stop with the live install.
    if current["removed"].as_i64().is_some() {
        return Err(Error::rejected(
            "installation is removed — restore it before any app action",
        ));
    }
    let entry = &catalog.installations[&id];
    if entry.storage != Storage::Workspace {
        return Err(Error::rejected(
            "historical bundle needs a workspace installation",
        ));
    }
    let bundle = if current["digest"] == digest {
        entry.paths(&id).0
    } else {
        let base = Path::new(".apps/installations").join(&*id);
        let original = base.join("bundle");
        let old_files = snapshot(&root, &original, false)?;
        if bundle_digest(&old_files) == digest {
            original
        } else {
            base.join("revisions").join(revision).join("bundle")
        }
    };
    let files = snapshot(&root, &bundle, false)?;
    if bundle_digest(&files) != digest {
        return Err(Error::rejected(
            "retained historical bundle differs from its frozen digest",
        ));
    }
    let manifest = app::parse_manifest(
        files
            .get("app.md")
            .ok_or_else(|| Error::rejected("historical bundle manifest is missing"))?,
    )?;
    if manifest.app != entry.app {
        return Err(Error::rejected(
            "historical bundle changes installation identity",
        ));
    }
    callback(&json!({"digest":digest}), &files)
}

/// CAD-1422: `builtin:<catalog_id>` is the retired embedded-catalog
/// source form. It is never resolved as a bundle: `builtin::refuse_retired`
/// is the explicit retired refusal, so an install or upgrade naming it fails
/// closed before the lock, the catalog read or any write.
pub(super) fn builtin_bundle(
    pm: &Pm,
    catalog_id: &str,
) -> Result<(BTreeMap<String, String>, app::Validated, app::Source)> {
    let _ = pm;
    builtin::refuse_retired(catalog_id)?;
    unreachable!("builtin::refuse_retired always returns the retired refusal")
}

/// [`install_builtin`] with a guard re-run under the PM lock before the first
/// write (CAD-1374). Retired (CAD-1422): the catalog no longer ships
/// distributions, so this always resolves to the retired refusal before
/// any lock or write — the guard never runs on a source that cannot
/// install.
pub fn install_builtin_guarded(
    pm: &Pm,
    catalog_id: &str,
    expected_digest: &str,
    guard: &dyn Fn() -> Result<()>,
) -> Result<Value> {
    let _ = guard;
    let (files, validated, provenance) = builtin_bundle(pm, catalog_id)?;
    install_resolved(
        pm,
        files,
        validated,
        provenance,
        Some(expected_digest),
        None,
        guard,
    )
}

/// CAD-1129 H4: re-check the install's recorded `source` upstream —
/// never a caller-chosen URL. For `Source::Git` the same URL + `dir`
/// re-resolves `HEAD` to a commit and re-fetches; `Source::Builtin`
/// answers "current" until a later built-in catalog version ships.
/// A `Source::Path` is a local checkout — no upstream to check.
pub fn update_check(pm: &Pm, install_id: &str) -> Result<Value> {
    let id = InstallationId::parse(install_id)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    if entry.storage != Storage::Workspace {
        return Err(Error::rejected(
            "update check needs a workspace installation",
        ));
    }
    let desc = describe(&root, &catalog, &id)?;
    let current_digest = desc["digest"].as_str().unwrap_or_default().to_string();
    let record: Record = serde_json::from_value(desc["record"].clone())
        .map_err(|e| Error::internal(format!("installation record is not readable: {e}")))?;
    if let Some(locator) = record.catalogue.as_ref() {
        let installed_version = desc["version"]
            .as_str()
            .ok_or_else(|| Error::rejected("installed version is not readable"))?;
        // Directory I/O must not hold the workspace's mutation lock.
        drop(_lock);
        let origin = client::catalog_origin()
            .ok_or_else(|| Error::rejected("catalogue metadata is unavailable"))?;
        let document = client::fetch_versions_at(&origin, &locator.id)?;
        let mut result = catalogue::update_check(
            &document,
            locator,
            &entry.app,
            installed_version,
            &current_digest,
        )?;
        result["install_id"] = json!(&*id);
        result["host_checked"] = json!(false);
        return Ok(result);
    }
    let source = record.source;
    match source {
        app::Source::Builtin { id: cat_id, digest } => {
            // CAD-1422: the built-in catalog is retired — there is no
            // embedded upstream to check against. Report the retired
            // source honestly, never a fake up-to-date answer.
            Ok(json!({"schema":1,"install_id":&*id,"source":"builtin",
                "catalog_id":cat_id,"has_update":false,"retired":true,
                "current_digest":digest,
                "note":"the built-in catalog was retired — update from the app's detached repository (a trusted path or Git source)"}))
        }
        app::Source::Git { url, sha, dir } => {
            let dir = dir.unwrap_or_default();
            let selected = crate::issue::app_source::SelectedGitSource::new(&url, &sha, &dir)?;
            let bundle = crate::issue::app_source::resolve(&selected)?;
            let validated = app::validate_texts(
                bundle
                    .files
                    .iter()
                    .map(|(n, t)| (n.clone(), t.clone()))
                    .collect(),
                &Default::default(),
                &[],
            )?;
            let new_digest = bundle_digest(&bundle.files);
            let has_update = new_digest != current_digest;
            Ok(json!({"schema":1,"install_id":&*id,"source":"git",
                "url":url,"sha":sha,"dir":dir,"has_update":has_update,
                "current_digest":current_digest,"digest":new_digest,
                "version":validated.manifest.version}))
        }
        app::Source::Path { path } => Ok(json!({"schema":1,"install_id":&*id,
            "source":"path","path":path,"has_update":false,
            "note":"a local checkout is not upstream-tracked — check its own files"})),
    }
}

/// CAD-1129 H5: preview a soft remove — the install's name, the
/// `personal_data` flag and the data it keeps. Per-kind counts live
/// in the store's own tables; the daemon's RPC reads those directly.
/// Writes nothing; the operator confirms before `remove` commits.
pub fn remove_preview(pm: &Pm, install_id: &str) -> Result<Value> {
    let id = InstallationId::parse(install_id)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    let desc = describe(&root, &catalog, &id)?;
    let _record: Record = serde_json::from_value(desc["record"].clone())
        .map_err(|e| Error::internal(format!("installation record is not readable: {e}")))?;
    // `listing` is a manifest field; `describe` doesn't carry it —
    // re-read the bundle's manifest for the personal-data flag.
    let app_md = {
        let (bundle, _) = catalog.installations[&id].paths(&id);
        let files = snapshot(&root, &bundle, false)?;
        files.get("app.md").cloned()
    };
    let listing = app_md
        .and_then(|md| app::parse_manifest(&md).ok())
        .and_then(|m| m.listing);
    let personal_data = listing
        .as_ref()
        .and_then(|l| l.value["data"]["personal"].as_bool())
        .unwrap_or(false)
        || listing
            .as_ref()
            .and_then(|l| l.value["data"]["contacts"].as_bool())
            .unwrap_or(false);
    Ok(json!({
        "schema":1,"install_id":&*id,"name":entry.app,
        "title":desc["title"],"personal_data":personal_data,
        "keeps":{"data":listing.as_ref().map(|l| l.value["data"]["keeps"].clone())},
        "generation":desc["catalog_generation"],"digest":desc["digest"],
    }))
}

/// [`remove`] with a guard re-run under the PM lock before the first write
/// (CAD-1374). A replay returns before the guard: it changes nothing.
pub fn remove_guarded(
    pm: &Pm,
    store: &Store,
    install_id: &str,
    expected_generation: &str,
    expected_digest: &str,
    request_id: &str,
    guard: &dyn Fn() -> Result<()>,
) -> Result<Value> {
    let id = InstallationId::parse(install_id)?;
    crate::proto::identifier(request_id, "remove request ID")?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    if hash(&yaml(&catalog)?) != expected_generation {
        return Err(Error::rejected("workspace catalog generation is stale"));
    }
    let desc = describe(&root, &catalog, &id)?;
    if desc["digest"].as_str() != Some(expected_digest) {
        return Err(Error::rejected("workspace installation digest is stale"));
    }
    let mut record: Record = serde_json::from_value(desc["record"].clone())
        .map_err(|e| Error::internal(format!("installation record is not readable: {e}")))?;
    if record.removed.is_some() {
        return Ok(json!({"schema":1,"install_id":&*id,"state":"removed",
            "removed":record.removed,"restore_after":record.restore_after,
            "request_id":request_id,"replayed":true,"digest":desc["digest"]}));
    }
    // CAD-1374: fresh authority re-proved under the PM lock, before the first write.
    guard()?;
    let now = crate::issue::time::now_epoch();
    record.removed = Some(now);
    record.restore_after = Some(now + 30 * 24 * 3600);
    // Consent revocation rides the same record write — the digest is
    // frozen, so revoke binds exactly these bytes.
    store.app_install_revoke(install_id, expected_digest, "remove")?;
    // Cancel queued publishes the store still holds for this install.
    let cancelled = store.social_publish_cancel_install(&id)?;
    let record_path = Path::new(".apps/installations")
        .join(&*id)
        .join("record.yaml");
    root.put(&record_path, &yaml(&record)?)?;
    let foreign = write::commit(
        pm,
        &[pm.dir.join(&record_path)],
        &format!("workspace app {} removed ({})", entry.app, &*id),
        &[],
        "operator",
    )?;
    let _ = store.event_public(
        "cadence",
        "app_workspace_removed",
        json!({"install_id":&*id,"app":entry.app,"request_id":request_id}),
    );
    Ok(json!({"schema":1,"install_id":&*id,"state":"removed",
        "removed":now,"restore_after":record.restore_after,
        "cancelled_publishes":cancelled,"request_id":request_id,
        "foreign_files":foreign,"digest":desc["digest"]}))
}

/// [`restore`] with a guard re-run under the PM lock before the first write
/// (CAD-1374). An already-live install returns before the guard: no effect.
pub fn restore_guarded(pm: &Pm, install_id: &str, guard: &dyn Fn() -> Result<()>) -> Result<Value> {
    let id = InstallationId::parse(install_id)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    let desc = describe(&root, &catalog, &id)?;
    let mut record: Record = serde_json::from_value(desc["record"].clone())
        .map_err(|e| Error::internal(format!("installation record is not readable: {e}")))?;
    if record.removed.is_none() {
        return Ok(json!({"schema":1,"install_id":&*id,"state":"live",
            "digest":desc["digest"],"restored":false}));
    }
    let now = crate::issue::time::now_epoch();
    if record.restore_after.is_some_and(|after| now > after) {
        return Err(Error::rejected(
            "the restore window closed — this install can only be removed",
        ));
    }
    let (bundle, _) = entry.paths(&id);
    let files = snapshot(&root, &bundle, false)?;
    let manifest = app::parse_manifest(
        files
            .get("app.md")
            .ok_or_else(|| Error::rejected("installed bundle has no manifest"))?,
    )?;
    manifest
        .requires
        .check(&app_compat::host(env!("CARGO_PKG_VERSION"))?)?;
    // CAD-1374: fresh authority re-proved under the PM lock, before the first write.
    guard()?;
    record.removed = None;
    record.restore_after = None;
    let record_path = Path::new(".apps/installations")
        .join(&*id)
        .join("record.yaml");
    root.put(&record_path, &yaml(&record)?)?;
    let foreign = write::commit(
        pm,
        &[pm.dir.join(&record_path)],
        &format!("workspace app {} restored ({})", entry.app, &*id),
        &[],
        "operator",
    )?;
    Ok(json!({"schema":1,"install_id":&*id,"state":"live",
        "digest":desc["digest"],"restored":true,"foreign_files":foreign}))
}
