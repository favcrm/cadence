//! CAD-535: `cadence doctor` — moved verbatim from src/main.rs.
//! CAD-2111: [`run_remote`] is the same verb on a hosted org.

use super::*;
use cadence_agent::remote_cli::probe::{self, Check};
use cadence_agent::remote_cli::RemoteTarget;

pub(super) fn run(
    state_dir: PathBuf,
    host: bool,
    json: bool,
    reclaim_plan: bool,
    agent: Option<String>,
    skill: Option<String>,
) -> Result<i32> {
    if host {
        return cadence_agent::doctor::host::cli(&state_dir, json, reclaim_plan);
    }
    if let Some(alias) = agent {
        let name = skill.as_deref().unwrap_or("cadence");
        // Validate the bundle name without inspecting this caller's home as the target.
        cadence_agent::skill::guidance(None, name)?;
        let shown = client::rpc(
            &state_dir,
            "agent_show",
            json!({"alias": alias, "limit": 0}),
        )?;
        let disabled = native_discovery_disabled(&shown);
        print_json(&json!({
            "target": alias,
            "skill": name,
            "scope": "native agent",
            "provider": shown["agent"]["provider"],
            "endpoint_kind": shown["agent"]["endpoint_kind"],
            "status": if disabled { "disabled" } else { "unknown" },
            "remedy": if disabled {
                "Managed Pi/omp disables automatic skill discovery; explicitly provide the guidance through an authorized task route."
            } else {
                "Runtime guidance reachability is not established; inspect its supported configuration and provide guidance explicitly."
            },
            "live_session": "not checked"
        }));
        return Ok(1);
    }
    let report = cadence_agent::doctor::run(&state_dir)?;
    print_json(&report);
    let ok = report
        .pointer("/checks/storage/ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(if ok { 0 } else { 1 })
}

fn native_discovery_disabled(shown: &Value) -> bool {
    let provider = shown.pointer("/agent/provider").and_then(Value::as_str);
    let kind = shown
        .pointer("/agent/endpoint_kind")
        .and_then(Value::as_str);
    provider.zip(kind).is_some_and(|(provider, kind)| {
        cadence_agent::adapter::registry::pi_family_managed(provider, kind)
    })
}

/// `cadence doctor` with a hosted org selected. It opens no local state, so it
/// works on a client-only installation; the probe never falls back to local.
pub(super) fn run_remote(
    target: &RemoteTarget,
    json: bool,
    offline: bool,
    org_flag: bool,
    client_only: bool,
    guidance: Option<(&str, Option<&str>)>,
    wake_timeout: u64,
) -> i32 {
    if let Some((profile, skill)) = guidance {
        return remote_guidance(
            target,
            profile,
            skill.unwrap_or("cadence"),
            offline,
            org_flag,
            wake_timeout,
        );
    }
    let kind = if client_only { "client-only" } else { "native" };
    let mut checks = vec![
        Check::ok(
            "install",
            format!(
                "{kind} installation, version {}",
                cadence_agent::overview::BUILD_ID
            ),
        ),
        Check::ok(
            "selection",
            format!(
                "org {} at {}, selected by {}",
                target.org,
                target.endpoint,
                selected_by(target, org_flag)
            ),
        ),
    ];
    checks.extend(probe::run(target, offline));
    if json {
        print_json(&probe::report_json(target, &checks));
    } else {
        print!("{}", probe::report_text(target, &checks));
    }
    probe::exit_code(&checks)
}

/// Evaluate only the metadata needed for the selected profile and skill.
fn hosted_guidance(profile: &Value, catalog: &Value, id: &str, name: &str) -> Value {
    let unknown = || json!({"status": "unknown", "remedy": "Check profile and workspace skill access and board support."});
    let Some(p) = profile.pointer("/context/profile") else {
        return unknown();
    };
    if p.get("id").and_then(Value::as_str) != Some(id)
        || p.get("status").and_then(Value::as_str) != Some("active")
    {
        return unknown();
    }
    let Some(skills) = p.get("skills").and_then(Value::as_array) else {
        return unknown();
    };
    if skills.iter().any(|s| s.as_str().is_none_or(str::is_empty)) {
        return unknown();
    }
    if !skills.iter().any(|s| s.as_str() == Some(name)) {
        return json!({"status": "not_assigned", "remedy": "Ask the profile owner to assign this skill."});
    }
    let Some(entries) = catalog.get("skills").and_then(Value::as_array) else {
        return unknown();
    };
    if entries
        .iter()
        .any(|entry| entry["name"].as_str().is_none_or(str::is_empty))
    {
        return unknown();
    }
    let mut matching = entries
        .iter()
        .filter(|entry| entry["name"].as_str() == Some(name));
    let Some(entry) = matching.next() else {
        return json!({"status": "missing", "remedy": "Create the workspace skill and request owner approval."});
    };
    if matching.next().is_some() {
        return unknown();
    }
    let Some(approved) = entry.get("approved") else {
        return unknown();
    };
    if approved.is_null() {
        return json!({"status": "unapproved", "remedy": "Ask the workspace owner to review and approve a revision."});
    }
    let (Some(latest), Some(revision), Some(digest)) = (
        entry["latest_revision"].as_u64(),
        approved["revision"].as_u64(),
        approved["digest"].as_str(),
    ) else {
        return unknown();
    };
    if revision == 0
        || revision > latest
        || (revision == latest && entry["latest_digest"].as_str() != Some(digest))
        || !digest.strip_prefix("sha256:").is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        })
    {
        return unknown();
    }
    json!({"status": "available", "revision": revision, "digest": digest,
           "remedy": "Available for this profile's board dispatch; a running session is not checked."})
}

fn remote_guidance(
    target: &RemoteTarget,
    profile: &str,
    skill: &str,
    offline: bool,
    org_flag: bool,
    wake_timeout: u64,
) -> i32 {
    use cadence_agent::remote_cli::{call_verb, RemoteAnswer};
    use std::time::{Duration, Instant};
    let result = if offline {
        json!({"status": "unknown", "remedy": "Offline: no hosted requests were sent."})
    } else if !org_flag {
        // workspace-skill reads require a named org.
        json!({"status": "unknown", "remedy": "Name the workspace explicitly with --org; nothing was sent."})
    } else if let Some(deadline) = Instant::now().checked_add(Duration::from_secs(wake_timeout)) {
        let call = |verb: &str, args: serde_json::Map<String, Value>| match call_verb(
            target,
            verb,
            args,
            deadline,
            wake_timeout,
            |_| {},
        ) {
            Ok(RemoteAnswer::Ok(value)) => Some(value),
            _ => None,
        };
        let mut args = serde_json::Map::new();
        args.insert("id".into(), json!(profile));
        match (
            call("agent_profile_context", args),
            call("workspace_skill_list", serde_json::Map::new()),
        ) {
            (Some(profile_data), Some(catalog)) => {
                hosted_guidance(&profile_data, &catalog, profile, skill)
            }
            _ => {
                json!({"status": "unknown", "remedy": "A hosted read was refused or unavailable; check access and board support."})
            }
        }
    } else {
        json!({"status": "unknown", "remedy": "Invalid wake timeout."})
    };
    let ok = result["status"] == "available";
    print_json(&json!({
        "target": profile, "skill": skill, "org": target.org,
        "scope": "hosted profile board dispatch", "guidance": result,
        "live_session": "not checked"
    }));
    i32::from(!ok)
}

#[cfg(test)]
mod guidance_tests {
    use super::*;

    #[test]
    fn native_managed_pi_and_omp_never_infer_home_discovery() {
        for provider in ["pi", "omp"] {
            assert!(native_discovery_disabled(&json!({"agent": {
                "provider": provider, "endpoint_kind": "managed"
            }})));
        }
        assert!(!native_discovery_disabled(
            &json!({"agent": {"provider": "pi"}})
        ));
    }

    #[test]
    fn hosted_requires_profile_assignment_and_approved_catalog_metadata() {
        let profile = json!({"context": {"profile": {
            "id": "p", "status": "active", "skills": ["cadence"]
        }}});
        let approved = json!({"skills": [{
            "name": "cadence", "latest_revision": 2, "latest_digest": format!("sha256:{}", "a".repeat(64)),
            "approved": {"revision": 2, "digest": format!("sha256:{}", "a".repeat(64))}
        }]});
        let unapproved = json!({"skills": [{"name": "cadence", "approved": null}]});
        let absent = json!({"skills": []});
        let incomplete = json!({"skills": [{"name": "cadence", "approved": {"revision": 2}}]});
        for (catalog, name, status) in [
            (&approved, "cadence", "available"),
            (&approved, "other", "not_assigned"),
            (&absent, "cadence", "missing"),
            (&unapproved, "cadence", "unapproved"),
            (&incomplete, "cadence", "unknown"),
            (&Value::Null, "cadence", "unknown"),
        ] {
            assert_eq!(
                hosted_guidance(&profile, catalog, "p", name)["status"],
                status
            );
        }
        let unknown_profile = json!({"context": {"profile": {"id": "p", "status": "active"}}});
        assert_eq!(
            hosted_guidance(&unknown_profile, &approved, "p", "cadence")["status"],
            "unknown"
        );
    }
}

/// Why this org, by the precedence `org::settle` applies: a running assignment
/// binds a managed caller, then `--org`, then `CADENCE_ORG`, then the saved
/// default.
fn selected_by(target: &RemoteTarget, org_flag: bool) -> &'static str {
    if target.identity.is_some() {
        "this session's running assignment"
    } else if org_flag {
        "--org"
    } else if std::env::var_os("CADENCE_ORG").is_some_and(|value| !value.is_empty()) {
        "CADENCE_ORG"
    } else {
        "the saved default"
    }
}
