//! Selected-remote app lifecycle. Native standalone commands never enter here.
use super::app::{AppAction, CatalogAction};
use super::{print_json, Error, Result};
use cadence_agent::remote_cli::RemoteTarget;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

fn checked_review(path: &Path, action: &Value) -> Result<Value> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(131_073)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 131_072 {
        return Err(Error::usage("release-check review exceeds 128 KiB"));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    let data = &value["data"];
    if value["ok"] != true
        || data["state"] != "proposed"
        || !data["review"].is_object()
        || [
            "listing_id",
            "version",
            "digest",
            "install_id",
            "expected_digest",
            "expected_generation",
        ]
        .iter()
        .any(|key| data[*key] != action[*key])
    {
        return Err(Error::usage("review must be the full successful release-check JSON for these exact release and installation pins; inspect it before consenting"));
    }
    Ok(data["review"].clone())
}

pub(super) fn run(target: &RemoteTarget, action: &AppAction) -> Result<i32> {
    let AppAction::Catalog { action } = action else {
        return Err(Error::usage("selected-remote app supports only bounded catalog published-release lifecycle; project, record and execution commands remain native and were not run locally"));
    };
    let mut operation_id: Option<&str> = None;
    let (method, suffix, body) = match action {
        CatalogAction::Ls | CatalogAction::Show { .. } => ("GET", String::new(), None),
        CatalogAction::Operation { operation_id: id } => ("GET", format!("/operations/{id}"), None),
        CatalogAction::Pending { app } => ("GET", format!("/pending/{app}"), None),
        CatalogAction::Reconcile { operation_id: id } => {
            operation_id = Some(id);
            ("POST", format!("/operations/{id}/reconcile"), Some(json!({})))
        }
        CatalogAction::ReleaseCheck { listing, version, digest, install_id, expected_digest, expected_generation } => {
            let mut action = json!({"listing_id":listing,"version":version,"digest":digest,"intent":"check"});
            if let Some(id) = install_id {
                action["install_id"] = json!(id);
                action["expected_digest"] = json!(expected_digest);
                action["expected_generation"] = json!(expected_generation);
            }
            ("POST", "/action".into(), Some(json!({"action":action})))
        }
        CatalogAction::ReleaseInstall { listing, version, digest, operation_id: id, review } => {
            operation_id = Some(id);
            let action = json!({"listing_id":listing,"version":version,"digest":digest,"intent":"apply","operation_id":id});
            let review = checked_review(review, &action)?;
            ("POST", "/action".into(), Some(json!({"action":action,"review":review})))
        }
        CatalogAction::ReleaseUpdate { install_id, listing, version, digest, expected_digest, expected_generation, operation_id: id, review } => {
            operation_id = Some(id);
            let action = json!({"listing_id":listing,"version":version,"digest":digest,"intent":"apply","operation_id":id,
                "install_id":install_id,"expected_digest":expected_digest,"expected_generation":expected_generation});
            let review = checked_review(review, &action)?;
            ("POST", "/action".into(), Some(json!({"action":action,"review":review})))
        }
        CatalogAction::ReleaseRevoke { install_id, digest, expected_generation, operation_id: id } => {
            operation_id = Some(id);
            ("POST", "/revoke".into(), Some(json!({"install_id":install_id,"expected_digest":digest,
                "expected_generation":expected_generation,"operation_id":id})))
        }
        _ => return Err(Error::usage("selected-remote catalog never uploads a local bundle or falls back locally. Use release-check/release-install/release-update/release-revoke with an immutable published listing, version, digest and reviewed check; operation/pending/reconcile recover receipts")),
    };
    // Printed before the network boundary so even a lost/non-JSON reply retains recovery identity.
    if let Some(id) = operation_id {
        eprintln!(
            "operation_id={id}; if uncertain, read/reconcile this ID, never redispatch blindly"
        );
    }
    let reply = cadence_agent::hosted_app_cli::request(target, method, &suffix, body.as_ref())?;
    if let Some(id) = &reply.operation_id {
        eprintln!("operation_id={id}");
    }
    if let CatalogAction::Show { install_id } = action {
        if reply.status == 200 && reply.body["ok"] == true && reply.body["data"]["state"] == "ready"
        {
            let rows = reply.body["data"]["installations"]
                .as_array()
                .ok_or_else(|| Error::rejected("hosted catalog response has no installations"))?;
            let mut matches = rows
                .iter()
                .filter(|row| row["installId"] == install_id.as_str());
            let row = matches
                .next()
                .ok_or_else(|| Error::rejected("no installation matches this ID"))?;
            if matches.next().is_some() {
                return Err(Error::rejected(
                    "more than one installation matches this ID",
                ));
            }
            print_json(&json!({"ok":true,"data":row}));
            return Ok(0);
        }
    }
    print_json(&reply.body);
    Ok(if reply.status == 202 {
        5
    } else if !(200..300).contains(&reply.status) || reply.body["ok"] != true {
        3
    } else if reply.body["data"]["state"] == "dispatched"
        || reply.body["data"]["state"] == "prepared"
    {
        5
    } else if reply.body["data"]["state"] == "unavailable"
        || reply.body["data"]["state"] == "refused"
    {
        3
    } else {
        0
    })
}
