//! CAD-1586: `cadence settings agents` — CLI parity for the Settings >
//! Agents editor (harnesses, providers, models, catalogs, role defaults,
//! profiles, extension pins, history) plus the settings-scoped grant
//! surface for a designated management agent.
//!
//! Every operation goes through one daemon resource command,
//! `agent_settings_command`, which runs the same validation and CAS
//! transaction as the five UI RPCs — the CLI never writes settings
//! storage or generated files itself. Mutations carry the observed
//! revision token (`show` → `.revision`); a stale token is a `conflict`
//! (exit 5), never a silent retry. Payloads that came from a file are
//! digest-bound: `--digest sha256:<hex>` is checked against the
//! canonical payload before anything is sent, and a settings grant
//! binds the exact payload bytes daemon-side — a file replaced after
//! approval is refused before any mutation.
//!
use serde_json::Map;
use sha2::{Digest, Sha256};

use super::*;

mod args;

pub(crate) use args::{AgentsAction, SettingsAction};
use args::{
    CatalogAction, GrantAction, HarnessAction, ModelAction, PinAction, ProfileAction,
    ProviderAction, ProviderKindArg, RoleAction, RoleArg,
};

/// Body cap for `--file`/`--payload-file` reads — a settings document or
/// profile job is small; 8 MiB keeps parity with the remote body bound.
const FILE_CAP: u64 = 8 * 1024 * 1024;

/// `canonical(payload)` exactly as the daemon computes it for digest and
/// grant binding — sorted keys, minimal separators — minus the
/// `grant_id`/`payload_sha256` self-referential fields.
fn canonical(value: &Value) -> String {
    fn write(out: &mut String, value: &Value) {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => {
                out.push('"');
                for ch in s.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(out, item);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(out, &Value::String((*key).clone()));
                    out.push(':');
                    write(out, &map[*key]);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    write(&mut out, value);
    out
}

/// sha256 hex over the canonical payload minus self-referential fields —
/// the same bytes the daemon digests for grant binding.
fn payload_digest(payload: &Map<String, Value>) -> String {
    let mut bound = payload.clone();
    bound.remove("grant_id");
    bound.remove("payload_sha256");
    let mut hasher = Sha256::new();
    hasher.update(canonical(&Value::Object(bound)).as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Verify a `--digest sha256:<hex>` flag against the payload; on match the
/// digest rides in the payload so the daemon re-verifies it.
fn bind_digest(payload: &mut Map<String, Value>, flag: Option<&str>) -> Result<()> {
    let Some(flag) = flag else {
        return Ok(());
    };
    let expected = flag.strip_prefix("sha256:").unwrap_or(flag).to_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::usage(
            "--digest must be sha256:<64 hex> of the canonical payload",
        ));
    }
    let actual = payload_digest(payload);
    if actual != expected {
        return Err(Error::rejected(format!(
            "--digest does not match the payload bytes (computed sha256:{actual}) — \
             the file changed since the digest was taken; nothing was sent"
        )));
    }
    payload.insert("payload_sha256".to_string(), json!(actual));
    Ok(())
}

/// Read a `--file` argument for a settings command: a master caller is
/// confined to `master/tmp` with no stdin (`read_master_command_file`);
/// other callers keep the `--file -`/stdin pipe. Remote runs have no
/// daemon state dir, but a managed caller can never reach remote, so the
/// plain reader applies.
fn read_payload_file(state_dir: Option<&Path>, file: Option<&Path>) -> Result<String> {
    match state_dir {
        Some(dir) => read_master_command_file(dir, file, FILE_CAP),
        None => {
            let text = read_body_capped(None, file.map(Path::to_path_buf), FILE_CAP)?;
            if text.len() as u64 > FILE_CAP {
                return Err(Error::rejected("settings file is over 8 MiB"));
            }
            Ok(text)
        }
    }
}

/// Attach `grant_id` to a payload — the global `--grant` flag binds any
/// operation (the daemon strips it before digesting/merging).
fn bind_grant(payload: &mut Map<String, Value>, grant: &Option<String>) {
    if let Some(id) = grant {
        payload.insert("grant_id".to_string(), json!(id));
    }
}

fn id_payload(id: &str) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("id".to_string(), json!(id));
    payload
}

/// Optional `--put` fields ride as `Some(value)` only — the daemon
/// merges them over the stored row (a missing field is "keep", never
/// "clear"), so a granted mutation needs no snapshot read first
/// (CAD-1586 manager ruling). An entirely empty put is left to the
/// daemon's own no-op/new-row validation.
fn put_payload(id: &str, fields: &[(&str, Option<Value>)]) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("id".to_string(), json!(id));
    for (name, value) in fields {
        if let Some(value) = value {
            payload.insert((*name).to_string(), value.clone());
        }
    }
    payload
}

type PlannedRequest = (String, Option<String>, Map<String, Value>);

fn planned(
    operation: &str,
    expected: Option<String>,
    payload: Map<String, Value>,
) -> PlannedRequest {
    (operation.to_string(), expected, payload)
}

fn tri_bool(enable: bool, disable: bool) -> Option<Value> {
    if enable {
        Some(json!(true))
    } else if disable {
        Some(json!(false))
    } else {
        None
    }
}

fn plan_apply(
    state_dir: Option<&Path>,
    file: Option<&Path>,
    expected_revision: &str,
    digest: Option<&str>,
) -> Result<PlannedRequest> {
    let text = read_payload_file(state_dir, file)?;
    let config: Value = serde_json::from_str(&text).map_err(|err| {
        Error::invalid(
            "invalid_config",
            format!("settings file is not JSON: {err}"),
        )
    })?;
    if !config.is_object() {
        return Err(Error::invalid(
            "invalid_config",
            "settings file must be a JSON object — the `config` object `show` prints",
        ));
    }
    let mut payload = Map::new();
    payload.insert("config".to_string(), config);
    bind_digest(&mut payload, digest)?;
    Ok(planned(
        "apply",
        Some(expected_revision.to_string()),
        payload,
    ))
}

fn plan_harness(action: &HarnessAction) -> PlannedRequest {
    match action {
        HarnessAction::List => planned("harness.list", None, Map::new()),
        HarnessAction::Show { id } => planned("harness.show", None, id_payload(id)),
        HarnessAction::Put {
            id,
            label,
            cli,
            note,
            mutation,
        } => planned(
            "harness.put",
            Some(mutation.expected_revision.clone()),
            put_payload(
                id,
                &[
                    ("label", label.clone().map(Value::String)),
                    ("cli", cli.clone().map(Value::String)),
                    ("note", note.clone().map(Value::String)),
                ],
            ),
        ),
        HarnessAction::Remove { id, mutation } => planned(
            "harness.remove",
            Some(mutation.expected_revision.clone()),
            id_payload(id),
        ),
    }
}

fn plan_provider(action: &ProviderAction) -> PlannedRequest {
    match action {
        ProviderAction::List => planned("provider.list", None, Map::new()),
        ProviderAction::Show { id } => planned("provider.show", None, id_payload(id)),
        ProviderAction::Put {
            id,
            label,
            kind,
            pin,
            gate,
            no_gate,
            paid,
            free,
            note,
            mutation,
        } => planned(
            "provider.put",
            Some(mutation.expected_revision.clone()),
            put_payload(
                id,
                &[
                    ("label", label.clone().map(Value::String)),
                    ("kind", kind.map(ProviderKindArg::as_str).map(|k| json!(k))),
                    ("pin", pin.clone().map(Value::String)),
                    ("gate", tri_bool(*gate, *no_gate)),
                    ("paid", tri_bool(*paid, *free)),
                    ("note", note.clone().map(Value::String)),
                ],
            ),
        ),
        ProviderAction::Remove { id, mutation } => planned(
            "provider.remove",
            Some(mutation.expected_revision.clone()),
            id_payload(id),
        ),
    }
}

fn plan_model(action: &ModelAction) -> PlannedRequest {
    match action {
        ModelAction::List => planned("model.list", None, Map::new()),
        ModelAction::Show { id } => planned("model.show", None, id_payload(id)),
        ModelAction::Put {
            id,
            provider,
            ctx,
            out,
            thinking,
            no_thinking,
            images,
            no_images,
            scope,
            mutation,
        } => planned(
            "model.put",
            Some(mutation.expected_revision.clone()),
            put_payload(
                id,
                &[
                    ("provider", provider.clone().map(Value::String)),
                    ("ctx", ctx.clone().map(Value::String)),
                    ("out", out.clone().map(Value::String)),
                    ("thinking", tri_bool(*thinking, *no_thinking)),
                    ("images", tri_bool(*images, *no_images)),
                    ("scope", scope.map(|s| json!(s.as_str()))),
                ],
            ),
        ),
        ModelAction::Rename { id, to, mutation } => {
            let mut payload = id_payload(id);
            payload.insert("to".to_string(), json!(to));
            planned(
                "model.rename",
                Some(mutation.expected_revision.clone()),
                payload,
            )
        }
        ModelAction::Remove { id, mutation } => planned(
            "model.remove",
            Some(mutation.expected_revision.clone()),
            id_payload(id),
        ),
        ModelAction::Scope {
            id,
            scope,
            mutation,
        } => {
            let mut payload = id_payload(id);
            payload.insert("scope".to_string(), json!(scope.as_str()));
            planned(
                "model.scope",
                Some(mutation.expected_revision.clone()),
                payload,
            )
        }
    }
}

fn plan_catalog(action: &CatalogAction) -> PlannedRequest {
    match action {
        CatalogAction::Refresh { provider } => {
            let mut payload = Map::new();
            payload.insert("provider".to_string(), json!(provider));
            planned("catalog.refresh", None, payload)
        }
        CatalogAction::Import {
            provider,
            models,
            all_new,
            mutation,
        } => {
            let mut payload = Map::new();
            payload.insert("provider".to_string(), json!(provider));
            payload.insert(
                "models".to_string(),
                if *all_new {
                    json!("all-new")
                } else {
                    json!(models)
                },
            );
            planned(
                "catalog.import",
                Some(mutation.expected_revision.clone()),
                payload,
            )
        }
    }
}

fn plan_role(action: &RoleAction) -> PlannedRequest {
    match action {
        RoleAction::List => planned("role.list", None, Map::new()),
        RoleAction::Set {
            role,
            model,
            mutation,
        } => {
            let mut payload = Map::new();
            payload.insert("role".to_string(), json!(role.as_str()));
            payload.insert("model".to_string(), json!(model));
            planned(
                "role.set",
                Some(mutation.expected_revision.clone()),
                payload,
            )
        }
        RoleAction::Clear { role, mutation } => {
            let mut payload = Map::new();
            payload.insert("role".to_string(), json!(role.as_str()));
            planned(
                "role.clear",
                Some(mutation.expected_revision.clone()),
                payload,
            )
        }
    }
}

fn plan_profile(state_dir: Option<&Path>, action: &ProfileAction) -> Result<PlannedRequest> {
    match action {
        ProfileAction::List => Ok(planned("profile.list", None, Map::new())),
        ProfileAction::Show { id } => Ok(planned("profile.show", None, id_payload(id))),
        ProfileAction::Put {
            id,
            role,
            model,
            job,
            job_file,
            digest,
            mutation,
        } => {
            // `--job` wins; `--job-file` reads only when given — an
            // omitted job is "keep" on an edit, never a stdin read.
            let job_text = match (job, job_file) {
                (Some(text), _) => Some(text.clone()),
                (None, file) => file
                    .as_deref()
                    .map(|f| read_payload_file(state_dir, Some(f)))
                    .transpose()?,
            };
            let mut payload = put_payload(
                id,
                &[
                    ("role", role.map(RoleArg::as_str).map(|r| json!(r))),
                    ("model", model.clone().map(Value::String)),
                    ("job", job_text.map(Value::String)),
                ],
            );
            bind_digest(&mut payload, digest.as_deref())?;
            Ok(planned(
                "profile.put",
                Some(mutation.expected_revision.clone()),
                payload,
            ))
        }
        ProfileAction::Rename { id, to, mutation } => {
            let mut payload = id_payload(id);
            payload.insert("to".to_string(), json!(to));
            Ok(planned(
                "profile.rename",
                Some(mutation.expected_revision.clone()),
                payload,
            ))
        }
        ProfileAction::Remove { id, mutation } => Ok(planned(
            "profile.remove",
            Some(mutation.expected_revision.clone()),
            id_payload(id),
        )),
        ProfileAction::Sync {
            profile,
            all: _,
            mutation,
        } => {
            let mut payload = Map::new();
            payload.insert(
                "profile".to_string(),
                profile.clone().map(Value::String).unwrap_or(Value::Null),
            );
            Ok(planned(
                "profile.sync",
                Some(mutation.expected_revision.clone()),
                payload,
            ))
        }
        ProfileAction::Preview {
            profile,
            harness,
            mutation,
        } => {
            let mut payload = Map::new();
            payload.insert("profile".to_string(), json!(profile));
            payload.insert("harness".to_string(), json!(harness));
            Ok(planned(
                "profile.preview",
                Some(mutation.expected_revision.clone()),
                payload,
            ))
        }
    }
}

fn plan_pin(action: &PinAction) -> PlannedRequest {
    match action {
        PinAction::List => planned("pin.list", None, Map::new()),
        PinAction::Put {
            pkg,
            pinned,
            mutation,
        } => {
            let mut payload = Map::new();
            payload.insert("pkg".to_string(), json!(pkg));
            payload.insert("pinned".to_string(), json!(pinned));
            planned("pin.put", Some(mutation.expected_revision.clone()), payload)
        }
        PinAction::Remove { pkg, mutation } => {
            let mut payload = Map::new();
            payload.insert("pkg".to_string(), json!(pkg));
            planned(
                "pin.remove",
                Some(mutation.expected_revision.clone()),
                payload,
            )
        }
    }
}

fn plan_grant(state_dir: Option<&Path>, action: &GrantAction) -> Result<PlannedRequest> {
    match action {
        GrantAction::Request {
            operation,
            payload,
            payload_file,
            expected_revision,
            reason,
        } => {
            let text = match (payload, payload_file) {
                (Some(text), _) => text.clone(),
                (None, file) => file
                    .as_deref()
                    .map(|f| read_payload_file(state_dir, Some(f)))
                    .transpose()?
                    .ok_or_else(|| {
                        Error::usage(
                            "grant request needs --payload or --payload-file — the exact operation payload the grant must bind",
                        )
                    })?,
            };
            let intended: Value = serde_json::from_str(&text).map_err(|err| {
                Error::invalid("invalid_request", format!("--payload is not JSON: {err}"))
            })?;
            if !intended.is_object() {
                return Err(Error::invalid(
                    "invalid_request",
                    "--payload must be a JSON object — the exact operation payload",
                ));
            }
            let mut p = Map::new();
            p.insert("operation".to_string(), json!(operation));
            p.insert(
                "expected_revision".to_string(),
                expected_revision
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            p.insert("payload".to_string(), intended);
            p.insert("reason".to_string(), json!(reason));
            Ok(planned("grant.request", None, p))
        }
        GrantAction::List => Ok(planned("grant.list", None, Map::new())),
        GrantAction::Allow {
            request_id,
            expected_revision,
        } => {
            let mut payload = Map::new();
            payload.insert("request_id".to_string(), json!(request_id));
            Ok(planned(
                "grant.allow",
                Some(expected_revision.clone()),
                payload,
            ))
        }
        GrantAction::Reject {
            request_id,
            expected_revision,
        } => {
            let mut payload = Map::new();
            payload.insert("request_id".to_string(), json!(request_id));
            Ok(planned(
                "grant.reject",
                Some(expected_revision.clone()),
                payload,
            ))
        }
        GrantAction::Revoke { grant_id } => {
            let mut payload = Map::new();
            payload.insert("grant_id".to_string(), json!(grant_id));
            Ok(planned("grant.revoke", None, payload))
        }
    }
}

/// Assemble `{operation, expected_revision, payload}` for one action.
/// `state_dir` is `None` on the remote path (file reads are still local).
/// `put` payloads carry only the fields the command line actually gave —
/// the daemon merges them over the stored row inside the settings
/// transaction, so a granted mutation never needs a snapshot read first.
fn request(
    state_dir: Option<&Path>,
    action: &AgentsAction,
    grant: &Option<String>,
) -> Result<PlannedRequest> {
    let mut planned_req = match action {
        AgentsAction::Show => planned("snapshot", None, Map::new()),
        AgentsAction::Fleet => planned("fleet.check", None, Map::new()),
        AgentsAction::Apply {
            file,
            mutation,
            digest,
        } => plan_apply(
            state_dir,
            file.as_deref(),
            &mutation.expected_revision,
            digest.as_deref(),
        )?,
        AgentsAction::Harness { action } => plan_harness(action),
        AgentsAction::Provider { action } => plan_provider(action),
        AgentsAction::Model { action } => plan_model(action),
        AgentsAction::Catalog { action } => plan_catalog(action),
        AgentsAction::Role { action } => plan_role(action),
        AgentsAction::Profile { action } => plan_profile(state_dir, action)?,
        AgentsAction::Pin { action } => plan_pin(action),
        AgentsAction::History => planned("history", None, Map::new()),
        AgentsAction::Grant { action } => plan_grant(state_dir, action)?,
    };
    // A grant binds an operation — never the grant surface itself.
    if !planned_req.0.starts_with("grant.") {
        bind_grant(&mut planned_req.2, grant);
    }
    Ok(planned_req)
}

/// Wrap one planned request into the `agent_settings_command` params.
fn params_of(operation: &str, expected: Option<String>, payload: Map<String, Value>) -> Value {
    json!({
        "operation": operation,
        "expected_revision": expected.map(Value::String).unwrap_or(Value::Null),
        "payload": Value::Object(payload),
    })
}

/// Local dispatch: the daemon's `agent_settings_command` runs the shared
/// validation + CAS transaction; the reply prints verbatim.
pub(super) fn run(state_dir: &Path, action: SettingsAction) -> Result<i32> {
    let SettingsAction::Agents { action, grant } = action;
    let (operation, expected, payload) = request(Some(state_dir), &action, &grant)?;
    print_json(&client::rpc(
        state_dir,
        "agent_settings_command",
        params_of(&operation, expected, payload),
    )?);
    Ok(0)
}

/// Remote dispatch on a selected hosted org: the identical
/// `{operation, expected_revision, payload}` goes through the explicit
/// `settings_agents` verb — never a raw tunnel, never a local fallback.
/// `local_state` exists only for `--file` reads; a managed caller cannot
/// reach this path at all.
pub(super) fn run_remote(
    target: &cadence_agent::remote_cli::RemoteTarget,
    action: &AgentsAction,
    grant: &Option<String>,
    wake_timeout: u64,
    local_state: Option<&Path>,
) -> Result<i32> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid wake timeout"))?;
    let (operation, expected, payload) = request(local_state, action, grant)?;
    let mut args = serde_json::Map::new();
    args.insert("operation".to_string(), json!(operation));
    args.insert(
        "expected_revision".to_string(),
        expected.map(|e| json!(e)).unwrap_or(Value::Null),
    );
    args.insert("payload".to_string(), Value::Object(payload));
    match cadence_agent::remote_cli::call_verb(
        target,
        cadence_agent::remote_cli::REMOTE_SETTINGS_VERB,
        args,
        deadline,
        wake_timeout,
        eprint_line,
    ) {
        Ok(cadence_agent::remote_cli::RemoteAnswer::Ok(body)) => {
            print_json(&body);
            Ok(0)
        }
        Ok(cadence_agent::remote_cli::RemoteAnswer::Conflict(body)) => {
            // Same verdict the local CAS returns: print the body and
            // exit 5 — the caller resyncs and re-issues, nothing was
            // written.
            print_json(&body);
            Err(Error::conflict(0, "settings revision moved"))
        }
        Err(error) => remote::remote_failure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_sorts_keys_and_minimizes() {
        let value = json!({"b": [1, {"z": null, "a": true}], "a": "x"});
        assert_eq!(
            canonical(&value),
            r#"{"a":"x","b":[1,{"a":true,"z":null}]}"#
        );
    }

    #[test]
    fn digest_ignores_self_fields() {
        let mut payload = Map::new();
        payload.insert("id".to_string(), json!("m1"));
        let base = payload_digest(&payload);
        payload.insert("grant_id".to_string(), json!("g1"));
        payload.insert("payload_sha256".to_string(), json!("0".repeat(64)));
        assert_eq!(payload_digest(&payload), base);
    }

    #[test]
    fn bind_digest_accepts_matching_and_refuses_stale() {
        let mut payload = Map::new();
        payload.insert("id".to_string(), json!("m1"));
        let hex = payload_digest(&payload);
        bind_digest(&mut payload, Some(&format!("sha256:{hex}"))).unwrap();
        let mut other = Map::new();
        other.insert("id".to_string(), json!("m2"));
        assert!(bind_digest(&mut other, Some(&format!("sha256:{hex}"))).is_err());
    }

    #[test]
    fn remote_classification_is_fail_closed() {
        for op in [
            "apply",
            "model.scope",
            "profile.put",
            "catalog.refresh",
            "catalog.import",
            "grant.request",
            "unknown.op",
            // The hosted contract (AgenticOS) does not carry this read yet, so
            // a remote call fails closed to the write scope rather than
            // claiming a settings.read the gateway would reject.
            "fleet.check",
        ] {
            assert_eq!(
                cadence_agent::remote_cli::settings_op_scope(op),
                "settings.write",
                "{op}"
            );
        }
        for op in [
            "snapshot",
            "model.list",
            "grant.list",
            "history",
            "profile.preview",
        ] {
            assert_eq!(
                cadence_agent::remote_cli::settings_op_scope(op),
                "settings.read",
                "{op}"
            );
        }
    }
}
