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

use cadence_core::board_api::AppOrigin;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use super::*;
use crate::error::{Error, Result};
use crate::issue::app_access;
use crate::issue::app_catalog::{self, workspace};

/// CAD-2215: one exact app conversation a verified member may read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GrantedConversation {
    pub install_id: String,
    /// The conversation's exact context captured when the grant was written;
    /// `None` is the installation's canonical contextless General
    /// conversation. This is a fact, never a selector.
    pub context_id: Option<String>,
    /// Fences a later grant to the same conversation from this snapshot.
    pub authorization_epoch: String,
}

/// CAD-2215: the exact conversations a live member may read, re-proved from
/// active grants at the moment of the read. Local agents, `read_as` relays
/// and CLI-envelope principals never construct a non-empty value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct MemberAppAccess {
    admitted: BTreeMap<String, GrantedConversation>,
}

impl MemberAppAccess {
    /// The exact conversation ids for the scoped SQL. Deterministic order.
    pub(super) fn conversations(&self) -> Vec<String> {
        self.admitted.keys().cloned().collect()
    }

    pub(super) fn conversation(&self, id: &str) -> bool {
        self.admitted.contains_key(id)
    }

    /// Is this derived app origin exactly one the grant covers? Compares the
    /// conversation, its installation and its context; nothing is inferred.
    pub(super) fn allows_origin(&self, origin: &AppOrigin) -> bool {
        self.admits(
            &origin.conversation_id,
            &origin.install_id,
            origin.context_id.as_str(),
        )
    }

    /// CAD-2215: the same exact admission as [`Self::allows_origin`] from
    /// borrowed parts, so a read path that already holds a thread row never
    /// builds an [`AppOrigin`] (or copies its strings) to prove the grant.
    /// `context_id` is the installation's empty string when the conversation
    /// is contextless, exactly as `AppOrigin` spells it.
    pub(super) fn admits(&self, conversation_id: &str, install_id: &str, context_id: &str) -> bool {
        self.admitted.get(conversation_id).is_some_and(|admitted| {
            admitted.install_id.as_str() == install_id
                && admitted.context_id.as_deref().unwrap_or("") == context_id
        })
    }
}

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
            "app_automatic_updates_get" => &["install_id"],
            "app_automatic_updates_set" => &[
                "install_id",
                "automatic",
                "public_session",
                "session_token",
                "session_key",
                "session_origin",
            ],
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
            "app_member_access_list" => &["install_id", "public_session"],
            "app_member_access_grant" => &[
                "install_id",
                "member_sub",
                "conversation_id",
                "expected_digest",
                "public_session",
            ],
            "app_member_access_revoke" => &[
                "install_id",
                "member_sub",
                "conversation_id",
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
            "app_automatic_updates_get" => {
                let install_id = required_str(params, "install_id")?;
                self.require_workspace_install(install_id)?;
                self.store.app_automatic_updates_get(install_id)
            }
            "app_automatic_updates_set" => {
                let install_id = required_str(params, "install_id")?;
                let automatic = match params.get("automatic") {
                    Some(Value::Bool(value)) => *value,
                    _ => return Err(Error::rejected("automatic must be a boolean")),
                };
                self.require_board_person(params)?;
                self.require_workspace_install(install_id)?;
                self.store.app_automatic_updates_set(install_id, automatic)
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
            "app_member_access_list" => self.rpc_app_member_access_list(params, peer_pid),
            "app_member_access_grant" => self.rpc_app_member_access_grant(params, peer_pid),
            "app_member_access_revoke" => self.rpc_app_member_access_revoke(params),
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
    /// The preference belongs to one workspace install. A missing or legacy
    /// row is not a place to store it.
    fn require_workspace_install(&self, install_id: &str) -> Result<()> {
        let id = app_catalog::InstallationId::parse(install_id)?;
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
        let entry = catalog
            .entries()
            .get(&id)
            .ok_or_else(|| Error::rejected("unknown installation ID"))?;
        if entry.storage != app_catalog::Storage::Workspace {
            return Err(Error::rejected(
                "automatic updates need a workspace installation",
            ));
        }
        Ok(())
    }

    /// A person at the board, not an operator-shell process. A public
    /// session names that person. A local or tailnet board relays the
    /// operator session it read from the cookie. Neither is optional.
    fn require_board_person(&self, params: &Value) -> Result<()> {
        if self.explorer_human(params, None)?.is_some() {
            return Ok(());
        }
        let token = params
            .get("session_token")
            .and_then(Value::as_str)
            .unwrap_or("");
        if token.is_empty() {
            return Err(Error::rejected(
                "automatic updates need a person at the board",
            ));
        }
        let checked = self.rpc_operator_session_check(&json!({
            "token": token,
            "key": params.get("session_key").and_then(Value::as_str).unwrap_or(""),
            "origin": params.get("session_origin").and_then(Value::as_str).unwrap_or(""),
        }))?;
        if checked.get("valid") == Some(&Value::Bool(true)) {
            return Ok(());
        }
        Err(Error::rejected(
            "automatic updates need a person at the board",
        ))
    }

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

    /// CAD-2215: the exact conversations this live member may read, resolved
    /// from active grants and re-proved against live state. Fail closed: any
    /// grant whose installation, digest, approval, exact conversation or
    /// pinned context cannot be proved NOW is skipped, never widened.
    pub(super) fn member_app_access(
        &self,
        principal: &super::identity::PublicReadPrincipal,
    ) -> Result<MemberAppAccess> {
        let mut access = MemberAppAccess::default();
        // An operator-role session reads the unscoped document; a grant is
        // only ever a narrowing for a member. A local agent, `read_as` relay
        // or CLI principal never reaches here with a principal.
        if principal.operator {
            return Ok(access);
        }
        let grants = self
            .store
            .app_member_grants_active(&principal.company, &principal.subject)?;
        if grants.is_empty() {
            return Ok(access);
        }
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
        for grant in grants {
            if !grant.has_valid_authorization_epoch() {
                return Err(Error::rejected("app access grant identity is unavailable"));
            }
            let Ok(id) = app_catalog::InstallationId::parse(&grant.install_id) else {
                continue;
            };
            let Some(entry) = catalog.entries().get(&id) else {
                continue;
            };
            if entry.storage != app_catalog::Storage::Workspace {
                continue;
            }
            let Ok(description) = workspace::describe_id(&pm, &catalog, &id) else {
                continue;
            };
            if description["removed"].as_i64().is_some()
                || description["digest"].as_str() != Some(grant.install_digest.as_str())
            {
                continue;
            }
            if self
                .store
                .app_capability_status(&grant.install_id, &grant.install_digest)?
                .get("state")
                .and_then(Value::as_str)
                != Some("approved")
            {
                continue;
            }
            if crate::proto::identifier(&grant.conversation_id, "conversation ID").is_err() {
                continue;
            }
            let Some(thread) = self.store.thread_by_id(&grant.conversation_id)? else {
                continue;
            };
            if thread.id != grant.conversation_id
                || thread.alias != crate::master::ALIAS
                || thread.archived
                || thread.runs_as_home()
                || thread.subject.is_some()
                || thread.install_id.as_deref() != Some(grant.install_id.as_str())
            {
                continue;
            }
            match (grant.context_id.as_deref(), thread.context_id.as_deref()) {
                (Some(pinned), Some(current)) if pinned == current => {
                    if self
                        .store
                        .app_context_proof(&grant.install_id, pinned)
                        .is_err()
                    {
                        continue;
                    }
                }
                // The only contextless admission is the installation's
                // canonical General conversation, bound by its exact id.
                (None, None) if thread.is_general => {}
                _ => continue,
            }
            access.admitted.insert(
                thread.id,
                GrantedConversation {
                    install_id: grant.install_id,
                    context_id: thread.context_id,
                    authorization_epoch: grant.authorization_epoch,
                },
            );
        }
        Ok(access)
    }

    /// This board's configured company. The grant key's `org` is always this
    /// value, never a request field.
    fn member_grant_company(&self) -> Result<String> {
        crate::board_identity::read_config(&self.state_dir)
            .map(|config| config.company)
            .map_err(|_| {
                Error::invalid(
                    "capability_unavailable",
                    "member app access needs this board's configured identity",
                )
            })
    }

    /// CAD-2215: prove the target subject is a CURRENT member of this exact
    /// organization before writing any grant. The authoritative source is the
    /// live platform roster read with the operator's verified board session
    /// (`human_current_actor`), which checks the roster's issuer and
    /// organization against the server config. No roster means no grant: a
    /// later session check cannot undo a latent grant to a future member.
    fn verified_grant_member(&self, params: &Value, peer_pid: u32, member_sub: &str) -> Result<()> {
        let token = match params.get("public_session") {
            Some(Value::String(token)) if !token.is_empty() => token.as_str(),
            _ => {
                return Err(Error::invalid(
                    "capability_unavailable",
                    "a member app access grant needs a verified operator board session so the live roster can prove the target member",
                ))
            }
        };
        let current = self.human_current_actor(token, params, peer_pid)?;
        let config = crate::board_identity::read_config(&self.state_dir)?;
        if current.scope.organization_id != config.company
            || current.roster.organization_id != config.company
        {
            return Err(Error::invalid(
                "tenancy_mismatch",
                "the live roster names a different organization",
            ));
        }
        if current.self_role != "owner" {
            return Err(Error::invalid(
                "not_an_owner",
                "the live roster does not name this operator as an owner",
            ));
        }
        match current
            .roster
            .members
            .iter()
            .find(|member| member.subject_id == member_sub)
        {
            Some(member) if matches!(member.role.as_str(), "owner" | "member") => Ok(()),
            _ => Err(Error::invalid(
                "not_a_member",
                "the named subject is not a current member of this organization",
            )),
        }
    }

    /// Why an operator-visible grant is or is not usable right now. Every
    /// reason is one the operator can act on; a row is never silently omitted.
    fn app_member_grant_state(
        &self,
        pm: &crate::issue::Pm,
        catalog: &app_catalog::Catalog,
        grant: &store::app_member_grants::MemberGrant,
    ) -> (bool, &'static str) {
        if !grant.active() {
            return (false, "revoked");
        }
        if !grant.has_valid_authorization_epoch() {
            return (false, "grant_identity_unavailable");
        }
        let Ok(id) = app_catalog::InstallationId::parse(&grant.install_id) else {
            return (false, "install_mismatch");
        };
        let Ok(description) = workspace::describe_id(pm, catalog, &id) else {
            return (false, "install_unavailable");
        };
        if description["removed"].as_i64().is_some() {
            return (false, "install_removed");
        }
        if description["digest"].as_str() != Some(grant.install_digest.as_str()) {
            return (false, "digest_changed");
        }
        let Ok(status) = self
            .store
            .app_capability_status(&grant.install_id, &grant.install_digest)
        else {
            return (false, "capability_unavailable");
        };
        if status.get("state").and_then(Value::as_str) != Some("approved") {
            return (false, "capability_revoked");
        }
        let Ok(Some(thread)) = self.store.thread_by_id(&grant.conversation_id) else {
            return (false, "conversation_unavailable");
        };
        if thread.alias != crate::master::ALIAS
            || thread.archived
            || thread.runs_as_home()
            || thread.subject.is_some()
            || thread.install_id.as_deref() != Some(grant.install_id.as_str())
        {
            return (false, "conversation_unavailable");
        }
        match (grant.context_id.as_deref(), thread.context_id.as_deref()) {
            (Some(pinned), Some(current)) if pinned == current => {
                if self
                    .store
                    .app_context_proof(&grant.install_id, pinned)
                    .is_err()
                {
                    return (false, "context_archived");
                }
            }
            (None, None) if thread.is_general => {}
            _ => return (false, "conversation_changed"),
        }
        (true, "")
    }

    /// CAD-2215: one grant row as every response carries it — the exact
    /// binding plus the daemon's CURRENT verdict. The list and the grant
    /// receipt both use this shape, so a reader never has to guess whether
    /// `usable`/`reason` are present.
    fn app_member_grant_row(
        &self,
        pm: &crate::issue::Pm,
        catalog: &app_catalog::Catalog,
        grant: &store::app_member_grants::MemberGrant,
    ) -> Value {
        let (usable, reason) = self.app_member_grant_state(pm, catalog, grant);
        let mut row = grant.to_json();
        row["usable"] = json!(usable);
        row["reason"] = json!(reason);
        row
    }

    /// The row a revoke just wrote. Every row a revoke answers with is the
    /// tombstone the store read back after its UPDATE, and a non-active row is
    /// never usable — `app_member_grant_state`'s own first rule — so the
    /// verdict needs no installation read and a revoke still answers when the
    /// installation, its tracker or the member's session is gone.
    fn revoked_grant_row(grant: &store::app_member_grants::MemberGrant) -> Value {
        let mut row = grant.to_json();
        row["usable"] = json!(false);
        row["reason"] = json!("revoked");
        row
    }

    /// `app_member_access_list` — operator-only. Lists every grant (active and
    /// revoked), the installation's live conversations, and the live roster a
    /// grant may name, so the operator can choose an exact conversation and an
    /// exact member for the next grant.
    fn rpc_app_member_access_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let human = self.explorer_human(params, None)?;
        let install_id = required_str(params, "install_id")?;
        let id = app_catalog::InstallationId::parse(install_id)?;
        let org = self.member_grant_company()?;
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
        let (digest, install_reason) = match workspace::describe_id(&pm, &catalog, &id) {
            Ok(description) if description["removed"].as_i64().is_some() => (
                description["digest"].as_str().map(str::to_string),
                "install_removed",
            ),
            Ok(description) => (description["digest"].as_str().map(str::to_string), ""),
            Err(_) => (None, "install_unavailable"),
        };
        let conversations = self
            .store
            .conversation_list(crate::master::ALIAS, install_id)?
            .into_iter()
            .filter(|thread| {
                thread.install_id.as_deref() == Some(install_id)
                    && !thread.archived
                    && !thread.runs_as_home()
                    && thread.subject.is_none()
            })
            .map(|thread| {
                json!({
                    "id": thread.id,
                    "context_id": thread.context_id,
                    "general": thread.is_general,
                    "title": thread.title,
                })
            })
            .collect::<Vec<_>>();
        let grants = self
            .store
            .app_member_grants_list(&org, install_id)?
            .into_iter()
            .map(|grant| self.app_member_grant_row(&pm, &catalog, &grant))
            .collect::<Vec<_>>();
        // CAD-2215: the members a grant can name are the live roster the grant
        // guard itself reads (`human_current_actor`), re-proved against this
        // board's organization and the caller's current owner role. A public
        // session whose live authority or roster cannot be re-proven fails the
        // read; it never becomes an empty "nobody to add". A local operator
        // has no public session at all, so it gets `null` and cannot grant.
        let members = match &human {
            Some(_) => {
                let token = params
                    .get("public_session")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let current = self.human_current_actor(token, params, peer_pid)?;
                if current.scope.organization_id != org || current.roster.organization_id != org {
                    return Err(Error::invalid(
                        "tenancy_mismatch",
                        "the live roster names a different organization",
                    ));
                }
                if current.self_role != "owner" {
                    return Err(Error::invalid(
                        "not_an_owner",
                        "the live roster does not name this operator as an owner",
                    ));
                }
                Some(
                    current
                        .roster
                        .members
                        .iter()
                        .map(|member| {
                            json!({
                                "subject_id": member.subject_id,
                                "name": member.name,
                                "role": member.role,
                            })
                        })
                        .collect::<Vec<_>>(),
                )
            }
            None => None,
        };
        let grant_reason = self.app_member_grant_readiness(
            install_id,
            digest.as_deref(),
            install_reason,
            conversations.len(),
            members.is_some(),
        );
        Ok(json!({
            "install_id": install_id,
            "digest": digest,
            "install_reason": install_reason,
            "conversations": conversations,
            "grants": grants,
            "members": members,
            "grant_reason": grant_reason,
        }))
    }

    /// CAD-2215: the one machine reason a grant cannot be written right now,
    /// or `""` when one could. The order is the grant guard's own — install,
    /// digest, approval, a live conversation, then the live roster — so the
    /// panel never offers a grant the daemon would refuse. An unreadable
    /// approval is a reason, never a reason to lose the whole list.
    fn app_member_grant_readiness(
        &self,
        install_id: &str,
        digest: Option<&str>,
        install_reason: &'static str,
        conversations: usize,
        roster: bool,
    ) -> &'static str {
        if !install_reason.is_empty() {
            return install_reason;
        }
        let Some(digest) = digest else {
            return "digest_unavailable";
        };
        match self.store.app_capability_status(install_id, digest) {
            Ok(status) if status.get("state").and_then(Value::as_str) == Some("approved") => {}
            Ok(_) => return "not_approved",
            Err(_) => return "capability_unavailable",
        }
        if conversations == 0 {
            return "no_conversations";
        }
        if !roster {
            return "roster_unavailable";
        }
        ""
    }

    /// `app_member_access_grant` — operator-only. Binds one exact conversation
    /// of one live, approved installation to one `member_sub` that the live
    /// roster proves is currently in this organization.
    fn rpc_app_member_access_grant(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let human = self.explorer_human(params, None)?;
        let install_id = required_str(params, "install_id")?;
        let id = app_catalog::InstallationId::parse(install_id)?;
        let member_sub = member_grant_subject(params)?;
        let conversation_id = required_str(params, "conversation_id")?;
        crate::proto::identifier(conversation_id, "conversation ID")?;
        let expected = required_str(params, "expected_digest")?;
        if !is_sha256_digest(expected) {
            return Err(Error::rejected(
                "expected_digest must be the installation's bundle digest",
            ));
        }
        let org = self.member_grant_company()?;
        if let Some(human) = &human {
            if human.get("company").and_then(Value::as_str) != Some(org.as_str()) {
                return Err(Error::invalid(
                    "tenancy_mismatch",
                    "the operator session belongs to a different board",
                ));
            }
        }
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let catalog = app_catalog::Catalog::load(&pm.dir).unwrap_or_default();
        let description = workspace::describe_id(&pm, &catalog, &id)
            .map_err(|_| Error::rejected("unknown installation ID"))?;
        if description["removed"].as_i64().is_some() {
            return Err(Error::rejected(
                "this installation is removed - restore it before granting access",
            ));
        }
        let digest = description["digest"]
            .as_str()
            .ok_or_else(|| Error::rejected("the installation has no bundle digest"))?;
        if digest != expected {
            return Err(Error::rejected(
                "the installation changed - review the current version before granting access",
            ));
        }
        if self
            .store
            .app_capability_status(install_id, digest)?
            .get("state")
            .and_then(Value::as_str)
            != Some("approved")
        {
            return Err(Error::rejected(
                "the current version is not approved - approve it before granting access",
            ));
        }
        let thread = self
            .store
            .thread_by_id(conversation_id)?
            .filter(|thread| {
                thread.alias == crate::master::ALIAS
                    && thread.install_id.as_deref() == Some(install_id)
                    && !thread.archived
                    && !thread.runs_as_home()
                    && thread.subject.is_none()
                    && thread.id == conversation_id
            })
            .ok_or_else(|| Error::rejected("conversation is unavailable for this installation"))?;
        // Pin the conversation's exact existing context as a fact. A
        // contextless conversation is admitted only as the installation's
        // canonical General conversation.
        if let Some(context) = thread.context_id.as_deref() {
            self.store.app_context_proof(install_id, context)?;
        } else if !thread.is_general {
            return Err(Error::rejected(
                "conversation has no context and is not the installation's General conversation",
            ));
        }
        let by = human
            .as_ref()
            .and_then(|human| human.get("sub"))
            .and_then(Value::as_str)
            .unwrap_or("operator")
            .to_string();
        // The live owner/target roster proof runs HERE, after the
        // installation, digest, approval, conversation and context reads and
        // immediately before the write, so an owner demotion or target
        // removal in flight fails closed. The steps are sequential, not one
        // atomic transaction.
        self.verified_grant_member(params, peer_pid, &member_sub)?;
        let grant = self.store.app_member_grant_set(
            &org,
            &member_sub,
            install_id,
            conversation_id,
            thread.context_id.as_deref(),
            digest,
            &by,
        )?;
        self.store.event_public(
            DAEMON_ALIAS,
            "app_member_access_granted",
            json!({
                "install_id": install_id,
                "member_sub": member_sub,
                "conversation_id": conversation_id,
                "context_id": thread.context_id,
                "install_digest": digest,
                "company": org,
                "by": by,
                "by_kind": if human.is_some() { "board_operator" } else { "operator" },
            }),
        )?;
        Ok(json!({
            "install_id": install_id,
            "grant": self.app_member_grant_row(&pm, &catalog, &grant),
        }))
    }

    /// `app_member_access_revoke` — operator-only and idempotent. With a
    /// `conversation_id` it revokes that exact row; without one it tombstones
    /// every exact row this member holds for the installation. It needs no
    /// live member session and no assistant binding.
    fn rpc_app_member_access_revoke(&self, params: &Value) -> Result<Value> {
        let human = self.explorer_human(params, None)?;
        let install_id = required_str(params, "install_id")?;
        app_catalog::InstallationId::parse(install_id)?;
        let member_sub = member_grant_subject(params)?;
        let org = self.member_grant_company()?;
        if let Some(human) = &human {
            if human.get("company").and_then(Value::as_str) != Some(org.as_str()) {
                return Err(Error::invalid(
                    "tenancy_mismatch",
                    "the operator session belongs to a different board",
                ));
            }
        }
        let by = human
            .as_ref()
            .and_then(|human| human.get("sub"))
            .and_then(Value::as_str)
            .unwrap_or("operator")
            .to_string();
        let (grants, revoked, conversation_id) = match params.get("conversation_id") {
            Some(_) => {
                let conversation_id = required_str(params, "conversation_id")?;
                crate::proto::identifier(conversation_id, "conversation ID")?;
                let grant = self.store.app_member_grant_revoke(
                    &org,
                    &member_sub,
                    install_id,
                    conversation_id,
                    &by,
                )?;
                let revoked = grant.is_some();
                (
                    grant.into_iter().collect::<Vec<_>>(),
                    revoked,
                    json!(conversation_id),
                )
            }
            None => {
                let grants = self.store.app_member_grants_revoke_member_install(
                    &org,
                    &member_sub,
                    install_id,
                    &by,
                )?;
                let revoked = !grants.is_empty();
                (grants, revoked, Value::Null)
            }
        };
        self.store.event_public(
            DAEMON_ALIAS,
            "app_member_access_revoked",
            json!({
                "install_id": install_id,
                "member_sub": member_sub,
                "conversation_id": conversation_id,
                "company": org,
                "grants": grants.len(),
                "by": by,
                "by_kind": if human.is_some() { "board_operator" } else { "operator" },
            }),
        )?;
        Ok(json!({
            "install_id": install_id,
            "member_sub": member_sub,
            "revoked": revoked,
            "grants": grants.iter().map(Self::revoked_grant_row).collect::<Vec<_>>(),
        }))
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

/// CAD-2215: `member_sub` names the platform's exact member subject. It is
/// never a handle and never normalized: padding, control characters and
/// overlong values are refused rather than lossily matched.
fn member_grant_subject(params: &Value) -> Result<String> {
    let sub = required_str(params, "member_sub")?;
    if sub.is_empty() || sub.len() > 256 || sub.trim() != sub || sub.chars().any(char::is_control) {
        return Err(Error::rejected(
            "member_sub must be the platform's exact member subject",
        ));
    }
    Ok(sub.to_string())
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
    let tmp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
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
