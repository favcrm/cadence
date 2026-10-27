//! CAD-1129 daemon RPCs for the apps Explorer: catalog (H1), board
//! install (H2), home projection (H3), stored-upstream update check
//! (H4), soft remove/restore (H5), install requests (H7) and
//! favourites (H8).
//!
//! Caller rule: every method goes through `operator_connection` except
//! the explicitly member-capable reads and the member verbs the plan
//! opens — `app_home`, `app_catalog_list`, `app_catalog_show`,
//! `app_favorites_get`, `app_favorites_put`, `app_favorites_opened`,
//! `app_install_request`. A member gets the same rows minus
//! source/digest/update/request_count (the member projection); writes
//! stay the operator's. The board relays a `member_as` claim the
//! daemon re-checks against the session, exactly like `wiki_as` — a
//! relayed claim can only ever shrink a caller.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::*;
use crate::error::{Error, Result};
use crate::issue::app_access;
use crate::issue::app_catalog::{self, workspace};

/// One workspace install of a catalog app, as the Explorer sees it.
struct Installed {
    install_id: String,
    /// Soft-removed: the card offers Restore, never Install.
    removed: bool,
    /// Removed and still inside the restore window.
    restorable: bool,
}

/// app-name → its workspace install. A live install wins over a
/// soft-removed one of the same app; a removed one is still reported
/// (with `removed`) so the card returns Restore instead of Install.
fn installed_by_app(
    pm: &crate::issue::Pm,
    catalog: &app_catalog::Catalog,
) -> BTreeMap<String, Installed> {
    let now = crate::issue::time::now_epoch();
    let mut by_app: BTreeMap<String, Installed> = BTreeMap::new();
    for (id, entry) in catalog.entries() {
        if entry.storage != app_catalog::Storage::Workspace {
            continue;
        }
        let Ok(desc) = workspace::describe_id(pm, catalog, id) else {
            continue;
        };
        let removed = desc["removed"].as_i64().is_some();
        let restorable = removed
            && desc["restore_after"]
                .as_i64()
                .is_none_or(|after| now <= after);
        if by_app.get(&entry.app).is_some_and(|prev| !prev.removed) {
            continue;
        }
        if removed && by_app.get(&entry.app).is_some_and(|prev| prev.restorable) {
            continue;
        }
        by_app.insert(
            entry.app.clone(),
            Installed {
                install_id: id.to_string(),
                removed,
                restorable,
            },
        );
    }
    by_app
}

impl Shared {
    /// The retired host catalog (CAD-1422) ships no distributions:
    /// discovery and marketplace installs live on the AgenticOS Apps
    /// page, and a local install keeps working from Apps. Every entry
    /// id that still resolves — a catalog id named by a live or
    /// soft-removed `Source::Builtin` install's record, or by an open
    /// install request — gets a tombstone row: the retired reason plus
    /// the install's real state, so an old detail link explains why its
    /// source is gone instead of refusing as unknown or faking an
    /// up-to-date listing. Anything else is `unknown catalog entry`.
    fn retired_catalog_row(
        &self,
        id: &str,
        installed: Option<&Installed>,
        member: bool,
    ) -> Result<Value> {
        let mut row = json!({
            "id": id,
            "source_kind": "builtin",
            "retired": true,
            "retired_reason": "The built-in app catalog was retired (CAD-1422) — this app now lives in its own repository and the AgenticOS marketplace.",
            "name": id,
            "title": id,
            "trust": "cadence",
            "featured": false,
            "state": "available",
        });
        if let Some(installed) = installed {
            row["install_id"] = json!(installed.install_id);
            if installed.removed {
                row["removed"] = json!(true);
                row["restorable"] = json!(installed.restorable);
            } else {
                row["state"] = json!("installed");
            }
        }
        if !member {
            row["request_count"] = self.store.app_install_request_count(id).unwrap_or(0).into();
        }
        Ok(row)
    }

    /// Every catalog id the retired surface still resolves: the
    /// `source.id` of each `Source::Builtin` workspace install, plus
    /// the catalog ids open install requests name.
    fn retired_catalog_ids(&self, pm: &crate::issue::Pm) -> Vec<String> {
        let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
        let mut ids = Vec::new();
        for (id, entry) in catalog.entries() {
            if entry.storage != app_catalog::Storage::Workspace {
                continue;
            }
            let Ok(desc) = workspace::describe_id(pm, &catalog, id) else {
                continue;
            };
            if desc["record"]["source"]["kind"].as_str() == Some("builtin") {
                if let Some(cat_id) = desc["record"]["source"]["id"].as_str() {
                    ids.push(cat_id.to_string());
                }
            }
        }
        for request in self.store.app_install_requests_open().unwrap_or_default() {
            if let Some(cat_id) = request["catalog_id"].as_str() {
                ids.push(cat_id.to_string());
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    /// The tombstone's install: the `Source::Builtin` install whose
    /// record names this catalog id (the app name alone no longer
    /// resolves — there is no embedded manifest to parse).
    fn retired_install(
        installed: &BTreeMap<String, Installed>,
        pm: &crate::issue::Pm,
        catalog: &app_catalog::Catalog,
        id: &str,
    ) -> Option<Installed> {
        for (install_id, entry) in catalog.entries() {
            if entry.storage != app_catalog::Storage::Workspace {
                continue;
            }
            let Ok(desc) = workspace::describe_id(pm, catalog, install_id) else {
                continue;
            };
            if desc["record"]["source"]["kind"].as_str() == Some("builtin")
                && desc["record"]["source"]["id"].as_str() == Some(id)
            {
                return installed.get(&entry.app).map(|i| Installed {
                    install_id: i.install_id.clone(),
                    removed: i.removed,
                    restorable: i.restorable,
                });
            }
        }
        None
    }

    /// `app_catalog_list` — the retired catalog's tombstones, with the
    /// member/operator projection intact (members never see
    /// `request_count` or update state).
    fn rpc_app_catalog_list(&self, member: bool, member_name: Option<&str>) -> Result<Value> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
        let installed = installed_by_app(&pm, &catalog);
        let pending = self.store.app_updates_pending().unwrap_or_default();
        let member_requested: Vec<String> = if member {
            member_name
                .map(|who| self.store.app_install_requests_of(who).unwrap_or_default())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut rows = Vec::new();
        for id in self.retired_catalog_ids(&pm) {
            let inst = Self::retired_install(&installed, &pm, &catalog, &id);
            let mut row = self.retired_catalog_row(&id, inst.as_ref(), member)?;
            if member && member_requested.contains(&id) {
                row["requested_by_me"] = json!(true);
            }
            if !member
                && inst
                    .as_ref()
                    .is_some_and(|i| !i.removed && pending.contains(&i.install_id))
            {
                row["update_available"] = json!(true);
            }
            rows.push(row);
        }
        Ok(json!({"catalog": rows}))
    }

    /// `app_catalog_show` — the retired entry's tombstone (CAD-1422):
    /// the retired reason and its install state. The retired catalog
    /// ships no bundles, so the listing payloads (`about`, `can`,
    /// `screenshots`, `setup`, `data`) are absent, never invented. An
    /// id nothing historical names is `unknown catalog entry`.
    fn rpc_app_catalog_show(
        &self,
        id: &str,
        member: bool,
        member_name: Option<&str>,
    ) -> Result<Value> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
        if !self.retired_catalog_ids(&pm).iter().any(|i| i == id) {
            return Err(Error::rejected("unknown catalog entry"));
        }
        let installed = installed_by_app(&pm, &catalog);
        let inst = Self::retired_install(&installed, &pm, &catalog, id);
        let mut row = self.retired_catalog_row(id, inst.as_ref(), member)?;
        if member
            && member_name
                .map(|who| {
                    self.store
                        .app_install_requests_of(who)
                        .unwrap_or_default()
                        .iter()
                        .any(|r| r == id)
                })
                .unwrap_or(false)
        {
            row["requested_by_me"] = json!(true);
        }
        Ok(row)
    }

    /// The board install of a built-in — `app_workspace_install_entry`.
    /// The daemon materialises the embedded bundle itself; the request
    /// carries `catalog_id` and the `expected_digest` the
    /// `builtin:<id>` install-check returned (CAD-1194), so the bytes
    /// consented to are the bytes checked. This is `app_workspace_install`'s
    /// journaled path with `source` pre-resolved.
    fn rpc_app_workspace_install_entry(
        self: &Arc<Self>,
        catalog_id: &str,
        expected_digest: &str,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let result = workspace::install_builtin_guarded(&pm, catalog_id, expected_digest, guard)?;
        // Install = consent (CAD-1119): record the approval for the
        // digest this install committed, like the CLI install.
        if let (Some(id), Some(digest)) = (
            result["install_id"].as_str().map(str::to_owned),
            result["digest"].as_str().map(str::to_owned),
        ) {
            let mut row = result;
            let consent = self.record_install_consent_guarded(&pm, &id, &digest, "install", guard);
            // The install is committed: its request closes whatever the consent says.
            self.store.app_install_requests_close(catalog_id)?;
            row["consent"] = consent?.0;
            return Ok(row);
        }
        self.store.app_install_requests_close(catalog_id)?;
        Ok(result)
    }

    /// `app_home` — identity, installed version/listing, catalogue locator
    /// and attention for each install. The locator is navigation, not trust.
    /// Members get `update` folded to `ok` and member wording.
    fn rpc_app_home(&self, member: bool) -> Result<Value> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
        let pending_updates = self.store.app_updates_pending().unwrap_or_default();
        let mut rows = Vec::new();
        for (id, entry) in catalog.entries() {
            if entry.storage != app_catalog::Storage::Workspace {
                continue;
            }
            let install_id = id.to_string();
            let desc = match workspace::describe_id(&pm, &catalog, id) {
                Ok(desc) => desc,
                Err(_) => {
                    // A failed describe is shown, never skipped: a
                    // missing row would read as "not installed".
                    rows.push(json!({
                        "install_id": install_id,
                        "name": entry.app,
                        "title": entry.app,
                        "tagline": null,
                        "icon": null,
                        "attention": {"state":"attention","message":"This app could not be read. Check the workspace and try again.","action":null,"count":1},
                    }));
                    continue;
                }
            };
            // A soft-removed install reads `removed` on its own row —
            // hidden from Open, listed under Recently removed.
            let removed = desc["removed"].as_i64().is_some();
            let mut attention = json!({"state":"ok","message":null,"action":null,"count":0});
            if removed {
                let restorable = desc["restore_after"]
                    .as_i64()
                    .is_none_or(|after| crate::issue::time::now_epoch() <= after);
                attention = if restorable {
                    json!({"state":"removed","message":"Removed — restore within 30 days.","action":"restore","count":0})
                } else {
                    json!({"state":"removed","message":"Removed — the restore window has closed.","action":null,"count":0})
                };
                let listing = desc["listing"].clone();
                rows.push(json!({
                    "install_id": install_id,
                    "name": desc["name"],
                    "title": desc["title"],
                    "tagline": listing["tagline"],
                    "icon": listing["icon"],
                    "version": desc["version"],
                    "listing": listing,
                    "catalogue": desc["catalogue"],
                    "attention": attention,
                }));
                continue;
            }
            // Consent state: revoked installs read "off".
            let digest = desc["digest"].as_str().unwrap_or_default();
            let status = self
                .store
                .app_capability_status(&install_id, digest)
                .unwrap_or(json!({"state":"unapproved"}));
            if status["state"] == "revoked" {
                attention = json!({"state":"off","message":"Access is off. The app can't run or use its connections.","action":"turn_on","count":1});
            } else {
                // Unbound slots and binding drift → finish_setup.
                let bindings = self
                    .store
                    .app_binding_list(&install_id, None)
                    .unwrap_or(json!({"bindings":[]}));
                let drift = bindings["bindings"]
                    .as_array()
                    .is_some_and(|bs| bs.iter().any(|b| b["drift"]["state"] == "needs_confirm"));
                let unbound = desc["connection_slots"]
                    .as_array()
                    .map(|slots| {
                        slots.iter().any(|slot| {
                            !bindings["bindings"].as_array().is_some_and(|bs| {
                                bs.iter().any(|b| {
                                    b["slot"].as_str() == slot.as_str()
                                        && b["state"].as_str() == Some("configured")
                                })
                            })
                        })
                    })
                    .unwrap_or(false);
                if drift || unbound {
                    attention = json!({"state":"setup","message":"Setup isn't finished.","action":"finish_setup","count":1});
                } else if !member && pending_updates.contains(&install_id) {
                    attention = json!({"state":"update","message":"An update is ready.","action":"review_update","count":1});
                } else if member && pending_updates.contains(&install_id) {
                    attention = json!({"state":"ok","message":null,"action":null,"count":0});
                }
            }
            let listing = desc["listing"].clone();
            rows.push(json!({
                "install_id": install_id,
                "name": desc["name"],
                "title": desc["title"],
                // The legacy project association — two same-name
                // installs keep it so a reader can tell them apart
                // (the old `/apps` list's `Project:` chip).
                "project": desc["project"],
                "tagline": listing["tagline"],
                "icon": listing["icon"],
                "version": desc["version"],
                "listing": listing,
                "catalogue": desc["catalogue"],
                "attention": attention,
            }));
        }
        rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({"installations": rows}))
    }

    /// `app_workspace_update_check` — the stored-upstream check: the
    /// body carries no `source`; the record's `upstream` resolves
    /// server-side. Returns the proposal plus `access_change`.
    fn rpc_app_workspace_update_check(&self, install_id: &str) -> Result<Value> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let out = workspace::update_check(&pm, install_id)?;
        // Cache the outcome for the home badge.
        let has_update = out["has_update"].as_bool().unwrap_or(false);
        let version = out["version"].as_str();
        let access_change = out.get("access_change");
        let digest = out["digest"].as_str();
        self.store.app_update_check_save(
            install_id,
            has_update,
            version,
            access_change,
            digest,
            &out,
        )?;
        Ok(out)
    }

    /// `app_workspace_remove_preview` — counts per kind, `personal_data`
    /// and `keeps`; writes nothing.
    fn rpc_app_workspace_remove_preview(&self, install_id: &str) -> Result<Value> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        workspace::remove_preview(&pm, install_id)
    }

    /// `app_workspace_remove` — journaled soft remove: marks the
    /// record, revokes consent, refuses runs, cancels publishes.
    fn rpc_app_workspace_remove(
        self: &Arc<Self>,
        params: &Value,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let install_id = required_str(params, "install_id")?;
        let expected_generation = required_str(params, "expected_generation")?;
        let expected_digest = required_str(params, "expected_digest")?;
        let request_id = required_str(params, "request_id")?;
        workspace::remove_guarded(
            &pm,
            &self.store,
            install_id,
            expected_generation,
            expected_digest,
            request_id,
            guard,
        )
    }

    /// `app_workspace_restore` — clear the mark and re-consent the
    /// same digest; refused after `purge_after`.
    fn rpc_app_workspace_restore(
        self: &Arc<Self>,
        install_id: &str,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let mut out = workspace::restore_guarded(&pm, install_id, guard)?;
        if let Some(digest) = out["digest"].as_str().map(str::to_owned) {
            // CAD-1374: the consent write re-proves the human before its own
            // mutation, so a revoked session cannot re-approve the digest.
            let (_, changed) =
                self.record_install_consent_guarded(&pm, install_id, &digest, "restore", guard)?;
            out["consent_changed"] = json!(changed);
        }
        Ok(out)
    }

    /// CAD-1374: the owner effect's durable intent and outcome. The sealed
    /// `prepared` frame is written before anything else. `effect` receives a
    /// guard the workspace owner re-runs under its PM lock before its first
    /// write: it re-reads the live session and requires the same verified
    /// human as the prepared intent. A stale session refuses before any write.
    /// A failed effect is `uncertain`, never a claimed rollback, and nothing
    /// retries. The original error is always returned.
    fn audited_workspace_effect(
        &self,
        params: &Value,
        action: &str,
        resource: &str,
        effect: impl FnOnce(&dyn Fn() -> Result<()>) -> Result<Value>,
    ) -> Result<Value> {
        let human = self.explorer_human(params, None)?;
        let request = uuid::Uuid::new_v4().to_string();
        let prepared = workspace_frame(
            &request,
            human.as_ref(),
            action,
            resource,
            "prepared",
            Value::Null,
            "not_available_before_effect",
        );
        self.store
            .event_public(DAEMON_ALIAS, "app_workspace_mutation_prepared", prepared)?;
        // `passed`: a guard has accepted the human, so a write may have begun.
        // `refused`: a guard has refused. Only a refusal before any pass is a
        // no-effect outcome; a later refusal is uncertain.
        let passed = std::cell::Cell::new(false);
        let refused = std::cell::Cell::new(false);
        let guard = || -> Result<()> {
            match self.explorer_human(params, None) {
                Ok(live) if live == human => {
                    passed.set(true);
                    Ok(())
                }
                Ok(_) => {
                    refused.set(true);
                    Err(Error::rejected(
                        "the board session changed; the workspace change was not made",
                    ))
                }
                Err(error) => {
                    refused.set(true);
                    Err(error)
                }
            }
        };
        match effect(&guard) {
            Ok(out) => {
                let (outcome, digest, reason) = workspace_outcome(action, &out);
                let mut finished = workspace_frame(
                    &request,
                    human.as_ref(),
                    action,
                    resource,
                    outcome,
                    digest,
                    reason,
                );
                if let Some(id) = out["install_id"].as_str() {
                    finished["install_id"] = json!(id);
                }
                self.store
                    .event_public(DAEMON_ALIAS, "app_workspace_mutation_finished", finished)
                    .map_err(|_| {
                        Error::internal(
                            "the workspace change completed but its outcome could not be recorded; it was not retried",
                        )
                    })?;
                Ok(out)
            }
            Err(error) => {
                let outcome = if refused.get() && !passed.get() {
                    "refused"
                } else {
                    "uncertain"
                };
                let finished = workspace_frame(
                    &request,
                    human.as_ref(),
                    action,
                    resource,
                    outcome,
                    Value::Null,
                    "outcome_not_confirmed",
                );
                if self
                    .store
                    .event_public(DAEMON_ALIAS, "app_workspace_mutation_finished", finished)
                    .is_err()
                {
                    return Err(Error::internal(
                        "workspace operation returned an error; its final outcome is unavailable; prepared intent retained; it was not retried",
                    ));
                }
                Err(error)
            }
        }
    }

    /// The Explorer RPC family. `member_as` is the board's named-member
    /// claim — the daemon re-checks it against the session before the
    /// row ever leaves. Direct RPC callers are always `member=false`.
    pub(super) fn rpc_app_explorer(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let member = member_claim(params)?;
        let allowed: &[&str] = match method {
            "app_catalog_list" => &["member_as"],
            "app_catalog_show" => &["id", "member_as"],
            "app_catalog_git_check" => &["url", "git_ref", "dir"],
            "app_home" => &["member_as"],
            "app_favorites_get" => &["member_as"],
            "app_favorites_put" => &["install_ids", "member_as", "public_session"],
            "app_favorites_put_default" => &["install_ids", "public_session"],
            "app_favorites_opened" => &["install_id", "member_as"],
            "app_install_request" => &["catalog_id", "member_as", "public_session"],
            "app_install_requests_list" => &["member_as"],
            "app_install_request_dismiss" => &["id", "public_session"],
            "app_workspace_install_entry" => &["catalog_id", "expected_digest", "public_session"],
            "app_workspace_update_check" => &["install_id"],
            "app_workspace_remove_preview" => &[
                "install_id",
                "expected_generation",
                "expected_digest",
                "request_id",
            ],
            "app_workspace_remove" | "app_workspace_restore" => &[
                "install_id",
                "expected_generation",
                "expected_digest",
                "request_id",
                "public_session",
            ],
            _ => return Err(Error::rejected("unknown app explorer method")),
        };
        let object = params
            .as_object()
            .ok_or_else(|| Error::rejected("explorer parameters must be an object"))?;
        if object.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::rejected("unknown app explorer parameter"));
        }
        // The operator gate: every method except the member-capable
        // reads and member verbs.
        let member_method = matches!(
            method,
            "app_catalog_list"
                | "app_catalog_show"
                | "app_home"
                | "app_favorites_get"
                | "app_favorites_put"
                | "app_favorites_opened"
                | "app_install_request"
        );
        // `app_install_requests_list` is operator-only: not a member method.
        // Every explorer call rides the operator connection — the board
        // relays over its own. A `member_as` claim additionally proves
        // the named member's session is live; it only ever narrows the
        // rows the board returns, never widens who may call.
        // A registered agent never carries a human's proof: the human in an
        // attributed write must come from a board session, not an agent slot.
        if object.contains_key("public_session") && self.slot_identity(peer_pid)?.is_some() {
            return Err(Error::rejected(
                "a board session proof is refused from an agent connection",
            ));
        }
        self.operator_connection("workspace app explorer", params, peer_pid)?;
        if member.is_some() {
            if !member_method {
                return Err(Error::rejected(
                    "member_as is a member-scope claim — this method has none",
                ));
            }
            self.member_connection(member.as_deref().unwrap_or(""), peer_pid)?;
        }
        match method {
            "app_catalog_list" => self.rpc_app_catalog_list(member.is_some(), member.as_deref()),
            "app_catalog_show" => {
                let id = required_str(params, "id")?;
                self.rpc_app_catalog_show(id, member.is_some(), member.as_deref())
            }
            "app_catalog_git_check" => self.rpc_app_catalog_git_check(params),
            "app_home" => self.rpc_app_home(member.is_some()),
            "app_favorites_get" => {
                let pm_dir = self.pm_dir()?;
                let pm = self.pm_at(&pm_dir)?;
                let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
                let live: Vec<String> = catalog
                    .entries()
                    .iter()
                    .filter(|(_, e)| e.storage == app_catalog::Storage::Workspace)
                    .filter(|(i, _)| !self.is_removed(&pm, &catalog, i).unwrap_or(false))
                    .map(|(i, _)| i.to_string())
                    .collect();
                let owner = member.as_deref().unwrap_or("operator");
                self.store
                    .app_favorites_get(owner, &|id| live.iter().any(|l| l == id))
            }
            "app_favorites_put" => {
                let ids: Vec<String> = params["install_ids"]
                    .as_array()
                    .ok_or_else(|| Error::rejected("install_ids must be a list"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| Error::rejected("install_ids holds strings"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let pm_dir = self.pm_dir()?;
                let pm = self.pm_at(&pm_dir)?;
                let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
                for id in &ids {
                    let iid = app_catalog::InstallationId::parse(id)?;
                    let entry = catalog
                        .entries()
                        .get(&iid)
                        .ok_or_else(|| Error::rejected("unknown or removed install id"))?;
                    if entry.storage != app_catalog::Storage::Workspace
                        || self.is_removed(&pm, &catalog, &iid)?
                    {
                        return Err(Error::rejected("unknown or removed install id"));
                    }
                }
                let owner = member.as_deref().unwrap_or("operator");
                let human = self.explorer_human(params, member.as_deref())?;
                self.store.app_favorites_put_as(owner, &ids, human.as_ref())
            }
            "app_favorites_put_default" => {
                let ids: Vec<String> = params["install_ids"]
                    .as_array()
                    .ok_or_else(|| Error::rejected("install_ids must be a list"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| Error::rejected("install_ids holds strings"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let human = self.explorer_human(params, None)?;
                self.store
                    .app_favorites_put_default_as(&ids, human.as_ref())
            }
            "app_favorites_opened" => {
                let id = required_str(params, "install_id")?;
                let owner = member.as_deref().unwrap_or("operator");
                self.store.app_favorites_opened(owner, id)?;
                Ok(json!({"ok": true}))
            }
            "app_install_request" => {
                let who = member
                    .as_deref()
                    .ok_or_else(|| Error::rejected("a request needs a named requester"))?;
                let catalog_id = required_str(params, "catalog_id")?;
                // CAD-1422: the built-in catalog is retired — a request
                // resolves only against an id a historical install or an
                // open request still names (a tombstone), never a fresh
                // catalog lookup.
                let pm_dir = self.pm_dir()?;
                let pm = self.pm_at(&pm_dir)?;
                if !self
                    .retired_catalog_ids(&pm)
                    .iter()
                    .any(|i| i == catalog_id)
                {
                    return Err(Error::rejected("unknown catalog entry"));
                }
                let human = self.explorer_human(params, Some(who))?;
                self.store
                    .app_install_request_as(catalog_id, who, human.as_ref())
            }
            "app_install_requests_list" => {
                Ok(json!({"requests": self.store.app_install_requests_open()?}))
            }
            "app_install_request_dismiss" => {
                let human = self.explorer_human(params, None)?;
                self.store
                    .app_install_request_dismiss_as(required_str(params, "id")?, human.as_ref())?;
                Ok(json!({"ok": true}))
            }
            "app_workspace_install_entry" => {
                let catalog_id = required_str(params, "catalog_id")?;
                let expected = required_str(params, "expected_digest")?;
                // CAD-1422: `builtin::refuse_retired` is the retired refusal —
                // catalog install always refuses, before any effect.
                app_catalog::builtin::refuse_retired(catalog_id)?;
                self.audited_workspace_effect(
                    params,
                    "app_workspace_installed",
                    &format!("catalog:{catalog_id}"),
                    |guard| self.rpc_app_workspace_install_entry(catalog_id, expected, guard),
                )
            }
            "app_workspace_update_check" => {
                self.rpc_app_workspace_update_check(required_str(params, "install_id")?)
            }
            "app_workspace_remove_preview" => {
                self.rpc_app_workspace_remove_preview(required_str(params, "install_id")?)
            }
            "app_workspace_remove" => {
                let install_id = required_str(params, "install_id")?;
                app_catalog::InstallationId::parse(install_id)?;
                self.audited_workspace_effect(
                    params,
                    "app_workspace_removed",
                    &format!("install:{install_id}"),
                    |guard| self.rpc_app_workspace_remove(params, guard),
                )
            }
            "app_workspace_restore" => {
                let install_id = required_str(params, "install_id")?;
                app_catalog::InstallationId::parse(install_id)?;
                self.audited_workspace_effect(
                    params,
                    "app_workspace_restored",
                    &format!("install:{install_id}"),
                    |guard| self.rpc_app_workspace_restore(install_id, guard),
                )
            }
            _ => Err(Error::rejected("unknown app explorer method")),
        }
    }

    /// The Git check: HTTPS-only URL + ref→commit resolution +
    /// `app_source::resolve` + a found card or a plain error.
    /// Operator-only, admitted by `operator_connection` above.
    fn rpc_app_catalog_git_check(&self, params: &Value) -> Result<Value> {
        let url = required_str(params, "url")?;
        let dir = params.get("dir").and_then(Value::as_str).unwrap_or("");
        let git_ref = params
            .get("git_ref")
            .and_then(Value::as_str)
            .unwrap_or("HEAD");
        // Refuse credentials, queries, fragments, IP literals, loopback,
        // link-local and private hosts — `app_source::SelectedGitSource`
        // re-checks the URL shape; this layer rejects the network cases.
        check_git_url_public(url)?;
        vet_resolved_host(url)?;
        // A network-class failure never echoes git's stderr (it names
        // the internal service certificate or address that answered).
        let commit = resolve_git_commit(url, git_ref)
            .map_err(|_| Error::rejected("the repository or ref could not be read"))?;
        let selected = crate::issue::app_source::SelectedGitSource::new(url, &commit, dir)?;
        let bundle = crate::issue::app_source::resolve(&selected)?;
        let app_md = bundle
            .files
            .get("app.md")
            .ok_or_else(|| Error::rejected("the repo holds no app.md — not a Cadence app"))?;
        let manifest = crate::issue::app::parse_manifest(app_md)?;
        let digest = workspace::bundle_digest(&bundle.files);
        let (holds_records, personal) = data_flags(&manifest);
        let access = app_access::access_rows(&manifest, None, holds_records, personal);
        Ok(json!({
            "found": true,
            "name": manifest.app,
            "title": manifest.title,
            "version": manifest.version,
            "commit": commit,
            "source_url": url,
            "dir": dir,
            "digest": digest,
            "check_digest": digest,
            "trust": "unreviewed",
            "listing": manifest.listing.as_ref().map(|l| l.value.clone()),
            "access": access["access"],
            "never": access["never"],
        }))
    }

    /// Is this install soft-removed? Reads the live record's mark
    /// via `describe` — never the catalog row, which has none.
    fn is_removed(
        &self,
        pm: &crate::issue::Pm,
        catalog: &app_catalog::Catalog,
        id: &app_catalog::InstallationId,
    ) -> Result<bool> {
        Ok(workspace::describe_id(pm, catalog, id)?["removed"]
            .as_i64()
            .is_some())
    }

    /// A member claim check: `member_as` names a named member whose
    /// public session is live. The board relays it only when
    /// `board_caller` resolved `Named`; a forged value never resolves.
    fn member_connection(&self, member: &str, _peer_pid: u32) -> Result<()> {
        if self.handle_has_current_member(member)? {
            Ok(())
        } else {
            Err(Error::rejected(
                "member_as does not name a live named member",
            ))
        }
    }

    /// CAD-1374: the verified human behind an attributed explorer write. The
    /// board's `public_session` proof is re-checked against the live row; it
    /// attributes the write and never grants it, so the connection guards
    /// above still decide. An absent proof is the legacy local path and names
    /// no human. A present proof must be a live granted session whose role
    /// fits the write: the named member's `member_as`, else the owner.
    fn explorer_human(&self, params: &Value, member: Option<&str>) -> Result<Option<Value>> {
        let token = match params.get("public_session") {
            None => return Ok(None),
            Some(Value::String(token)) if !token.is_empty() => token.as_str(),
            Some(_) => return Err(Error::rejected("the board session proof is malformed")),
        };
        let granted = match self.current_public_session(token)? {
            crate::daemon::public_membership::Authority::Granted {
                sub,
                role,
                realm,
                display,
                view,
                ..
            } => (sub, role, realm, display, view),
            crate::daemon::public_membership::Authority::Denied => {
                return Err(Error::rejected("the board session is not live"))
            }
        };
        let (sub, role, realm, display, view) = granted;
        let fits = match member {
            Some(who) => {
                role == crate::board_identity::Role::Member
                    && view.user.as_ref().is_some_and(|u| u.handle == who)
            }
            None => role.is_operator(),
        };
        if !fits {
            return Err(Error::rejected(
                "the board session does not match this write's member scope",
            ));
        }
        Ok(Some(json!({
            "sub": sub,
            "company": realm.company,
            "session": view.id,
            "display": display,
            "role": role.as_str(),
        })))
    }
}

/// CAD-1374: one canonical workspace-effect frame. `human` is the verified
/// board human (or `null` for the legacy local path, which names no human).
/// `reason` is the validated action; `outcome` names the real state.
fn workspace_frame(
    request: &str,
    human: Option<&Value>,
    action: &str,
    resource: &str,
    outcome: &str,
    digest: Value,
    digest_unavailable_reason: &str,
) -> Value {
    let digest_unavailable = if digest.is_null() {
        json!(digest_unavailable_reason)
    } else {
        Value::Null
    };
    json!({
        "schema_version": 1,
        "request_id": request,
        "correlation_id": request,
        "causation_id": null,
        "at": crate::issue::time::now_epoch(),
        "company": human.map_or(Value::Null, |h| h["company"].clone()),
        "human": human.cloned().unwrap_or(Value::Null),
        "action": action,
        "resource": resource,
        "outcome": outcome,
        "reason": action,
        "revision_or_digest": digest,
        "revision_or_digest_unavailable_reason": digest_unavailable,
    })
}

/// The terminal outcome of a completed workspace effect, read from its real
/// result. A replayed remove or an already-live restore changed nothing new,
/// and says so. The digest is the validated operation value, or an explicit
/// unavailable reason; it is never a caller value or a later PM HEAD.
fn workspace_outcome(action: &str, out: &Value) -> (&'static str, Value, &'static str) {
    let outcome = match action {
        "app_workspace_installed" => "installed",
        "app_workspace_removed" if out["replayed"] == true => "already_removed",
        "app_workspace_removed" => "removed",
        _ if out["restored"] == true => "restored",
        _ if out["consent_changed"] == true => "already_live_consent_recorded",
        _ => "already_live",
    };
    match out["digest"].as_str().filter(|d| is_sha256_digest(d)) {
        Some(d) => (outcome, json!(d), ""),
        None => (outcome, Value::Null, "operation_result_has_no_digest"),
    }
}

/// `sha256:` then exactly 64 lowercase hex digits, the form `bundle_digest` writes.
fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// What the manifest's `listing.data` declares: `(holds_records,
/// personal)`. `stores` names what the app keeps (records it holds);
/// `personal` marks personal data. The two are independent, so an app
/// that keeps records but no personal data still shows "its own data".
fn data_flags(manifest: &crate::issue::app::Manifest) -> (bool, bool) {
    let data = manifest.listing.as_ref().map(|l| &l.value["data"]);
    let holds_records = data
        .and_then(|d| d["stores"].as_array())
        .is_some_and(|s| !s.is_empty());
    let personal = data.and_then(|d| d["personal"].as_bool()).unwrap_or(false);
    (holds_records, personal)
}

/// `member_as` param → `Some(handle)` when present and tag-shaped.
fn member_claim(params: &Value) -> Result<Option<String>> {
    match params.get("member_as") {
        None => Ok(None),
        Some(v) => {
            let who = v
                .as_str()
                .ok_or_else(|| Error::rejected("member_as must be a member handle"))?;
            if who.is_empty()
                || who.len() > 128
                || !who
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err(Error::rejected("member_as is not a member handle"));
            }
            Ok(Some(who.to_string()))
        }
    }
}

/// The public-URL gate for the Git check, an allowlist: `https://`, no
/// credentials, query, fragment, escape or port, and a host that is a
/// registered-shaped DNS name — lowercase letters, digits and hyphens in
/// 1..=63-byte labels, at least two labels, no empty label (so no trailing
/// dot), and a final label of letters only. That last rule is what makes
/// every numeric spelling of an address (decimal, hex, octal, short
/// dotted) a refusal: no such spelling ends in a letters-only label. Local
/// and private suffixes are refused by name. The text of every refusal is
/// fixed and echoes nothing the caller sent or the network said.
/// `app_source::SelectedGitSource::check_url` re-proves the shape; the
/// resolution check in [`vet_resolved_host`] is the network-class half.
pub(super) fn check_git_url_public(url: &str) -> Result<()> {
    const REFUSED: &str = "git check refuses this URL; name a public https repository";
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::rejected("git check admits https:// URLs only"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty()
        || authority
            .bytes()
            .any(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-')))
    {
        return Err(Error::rejected(REFUSED));
    }
    // Anything else in the URL that is not a plain path is refused.
    if rest.contains(['?', '#', '@', '%', '\\']) {
        return Err(Error::rejected(REFUSED));
    }
    let labels: Vec<&str> = authority.split('.').collect();
    let tld = labels.last().copied().unwrap_or_default();
    let label_ok =
        |l: &&str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-');
    if labels.len() < 2
        || !labels.iter().all(label_ok)
        || tld.len() < 2
        || !tld.bytes().all(|b| b.is_ascii_lowercase())
        || matches!(
            tld,
            "localhost"
                | "local"
                | "localdomain"
                | "internal"
                | "lan"
                | "home"
                | "arpa"
                | "corp"
                | "intranet"
                | "private"
        )
    {
        return Err(Error::rejected(REFUSED));
    }
    Ok(())
}

/// The one vet every route that fetches a Git source applies before git
/// runs: the Explorer's check and the CLI-facing install, install-check,
/// upgrade and upgrade-check. An absolute local path or `builtin:` id is not a network
/// source and passes through to its own checks; anything else must pass
/// the same public-https allowlist and resolved-host vet as the check.
pub(super) fn vet_remote_source(source: &str) -> Result<()> {
    // `builtin:<id>` is the embedded bundle, resolved without a network.
    if std::path::Path::new(source).is_absolute() || source.starts_with("builtin:") {
        return Ok(());
    }
    check_git_url_public(source)?;
    vet_resolved_host(source)
}

/// Resolve the vetted host now and refuse any non-public address, so a
/// public-looking name that points inside is refused before git runs.
/// This narrows but cannot close DNS rebinding (git resolves again when
/// it connects); the check is operator-only and read-only, and the
/// install itself is pinned by the digest the operator reviews.
pub(super) fn vet_resolved_host(url: &str) -> Result<()> {
    use std::net::ToSocketAddrs;
    let host = url
        .strip_prefix("https://")
        .and_then(|r| r.split('/').next())
        .unwrap_or_default();
    let addrs: Vec<_> = (host, 443u16)
        .to_socket_addrs()
        .map_err(|_| Error::rejected("the repository host could not be resolved"))?
        .collect();
    if addrs.is_empty() || addrs.iter().any(|a| !public_ip(a.ip())) {
        return Err(Error::rejected(
            "git check refuses this URL; name a public https repository",
        ));
    }
    Ok(())
}

/// Globally routable unicast only: not loopback, private, link-local,
/// CGNAT, documentation, benchmarking, multicast, reserved or unspecified.
fn public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_unspecified()
                || v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_broadcast()
                || v.is_multicast()
                || v.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 198 && (18..=19).contains(&o[1]))
                || o[0] >= 240)
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let s = v.segments();
            !(v.is_unspecified()
                || v.is_loopback()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || (s[0] == 0x0064 && s[1] == 0xff9b))
        }
    }
}

/// Resolve `ref` to a 40-hex commit through `git ls-remote` — the one
/// place a caller-chosen ref touches the network, bounded by
/// `app_source`'s env-scrubbed `git_step`.
fn resolve_git_commit(url: &str, git_ref: &str) -> Result<String> {
    if git_ref.len() > 128
        || !git_ref
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/' | b'.'))
    {
        return Err(Error::rejected(
            "git_ref must be a branch, tag or HEAD name",
        ));
    }
    // ls-remote resolves refs/tags/*, refs/heads/* and HEAD. A 40-hex
    // commit answers itself under HEAD only when it is the tip.
    let tmp = tempfile::tempdir()?;
    let out = crate::issue::app_source::ls_remote(tmp.path(), url, git_ref)?;
    Ok(out)
}

#[cfg(test)]
mod data_access_rows {
    //! CAD-1209: the detail page's "Its own data" row comes from the
    //! manifest's `data` declaration — `stores` (holds records) and
    //! `personal` are independent.
    use super::*;

    fn manifest(data: &str) -> crate::issue::app::Manifest {
        crate::issue::app::parse_manifest(&format!(
            "---\napp: demo\ntitle: Demo\nversion: '0.1.0'\nneeds:\n  connections: []\nlisting:\n  tagline: t\n{data}---\n\nbody\n"
        ))
        .unwrap()
    }

    #[test]
    fn records_without_personal_data_still_show_the_row() {
        let m = manifest("  data:\n    stores:\n      - Your orders\n    personal: false\n");
        assert_eq!(data_flags(&m), (true, false));
        let rows = app_access::access_rows(&m, None, true, false);
        let text = rows["access"].to_string();
        assert!(text.contains("Its own data"), "{text}");
        assert!(!text.contains("Personal data"), "{text}");
    }

    #[test]
    fn no_declaration_shows_no_data_row() {
        assert_eq!(data_flags(&manifest("")), (false, false));
        assert_eq!(
            data_flags(&manifest("  data:\n    personal: true\n")),
            (false, true)
        );
    }
}
