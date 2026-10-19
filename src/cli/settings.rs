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

/// Body cap for `--file`/`--payload-file` reads — a settings document or
/// profile job is small; 8 MiB keeps parity with the remote body bound.
const FILE_CAP: u64 = 8 * 1024 * 1024;

#[derive(Subcommand)]
pub(crate) enum SettingsAction {
    /// defaults, profiles, pins, history and settings-scoped grants.
    /// Every mutation takes `--expected-revision` from `show`.
    Agents {
        /// Settings-scoped grant id authorizing exactly this operation
        /// and payload (`settings agents grant list`) — accepted on reads
        /// and mutations alike; the daemon strips it before binding the
        /// payload bytes.
        #[arg(long, global = true, value_name = "ID")]
        grant: Option<String>,
        #[command(subcommand)]
        action: AgentsAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum AgentsAction {
    /// The full authoritative document: revision, config, observations,
    /// catalogs and history. Machine-readable JSON.
    Show,
    /// Every registered gated agent evaluated against this authority — who
    /// would be refused at launch and why, with the effective policy. Read
    /// only: nothing is launched, mutated or written. Use it before saving a
    /// policy edit, so a change cannot silently strand the fleet.
    Fleet,
    /// Replace the whole document atomically from a JSON file (`--file
    /// -` or a pipe reads stdin). The file is the same `config` object
    /// `show` prints — harnesses/providers/models/profiles/scope/
    /// roleDefaults/pins exactly. Binds the file's bytes: `--digest`
    /// must equal sha256 of the canonical payload, and a settings grant
    /// binds those bytes daemon-side, so a replaced file is refused.
    Apply {
        /// JSON document file; `-` (or a piped stdin) reads stdin.
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        #[command(flatten)]
        mutation: Mutation,
        /// sha256:<hex> of the canonical payload — refuse before send
        /// when the file's content does not match.
        #[arg(long, value_name = "sha256:HEX")]
        digest: Option<String>,
    },
    /// Manage launch harness rows (`id`, `label`, adapter `cli`,
    /// `note`). Installed/version/live are host observations, never
    /// writable flags.
    Harness {
        #[command(subcommand)]
        action: HarnessAction,
    },
    /// Manage inference provider rows (`id`, `label`, `kind`, `pin`,
    /// `gate`, `paid`, `note`) — settings entries, not Connections
    /// accounts.
    Provider {
        #[command(subcommand)]
        action: ProviderAction,
    },
    /// Manage model rows and their launch scope. `thinking`/`images`
    /// catalog facts stay unknown (null) unless explicitly set.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Provider model catalogs: refresh from the host catalog and
    /// import entries as scope `off`. A failed refresh stays visible;
    /// importing never authorizes a model.
    Catalog {
        #[command(subcommand)]
        action: CatalogAction,
    },
    /// Team-role default models (pm research architect dev qa devops
    /// worker) — selection defaults, not runtime identity.
    Role {
        #[command(subcommand)]
        action: RoleAction,
    },
    /// Saved launch profiles (`id`, `role`, `model`/`inherit`, `job`)
    /// plus export sync and launch preview.
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Pi extension pins (`pkg`, `pinned`). Editing a pin never
    /// installs a package.
    Pin {
        #[command(subcommand)]
        action: PinAction,
    },
    /// Available settings/profile history — recorded saves, not
    /// fabricated per-field diffs.
    History,
    /// Settings-scoped authority for a designated management agent:
    /// request an exact operation, the operator allows or rejects it,
    /// and the bound mutation carries `--grant`. Works while the master
    /// is down; ordinary workers and app-scoped callers are refused by
    /// the daemon.
    Grant {
        #[command(subcommand)]
        action: GrantAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum HarnessAction {
    /// Every harness row with its host observations (installed, version,
    /// live agents).
    #[command(visible_alias = "ls")]
    List,
    /// One harness row with its host observations.
    Show { id: String },
    /// Create or update a harness. On update, unpassed fields keep their
    /// stored values; `cli` must stay a registered adapter program.
    Put {
        id: String,
        #[arg(long)]
        label: Option<String>,
        /// Registered adapter program name, never a path.
        #[arg(long)]
        cli: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a harness. Refused while an active agent uses it.
    Remove {
        id: String,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum ProviderKindArg {
    /// Provider models ride the Pi allow/gate policy.
    PiExtension,
    /// Provider is a native agent CLI; its models do not use the Pi gate.
    NativeCli,
}

impl ProviderKindArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::PiExtension => "Pi extension",
            Self::NativeCli => "native CLI",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum ProviderAction {
    /// Every provider row.
    #[command(visible_alias = "ls")]
    List,
    /// One provider row.
    Show { id: String },
    /// Create or update a provider. `kind` is `pi-extension` or
    /// `native-cli`; `gate` follows the kind unless explicitly set.
    Put {
        id: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, value_enum)]
        kind: Option<ProviderKindArg>,
        /// Pin metadata for the provider (`name@version` or text).
        #[arg(long)]
        pin: Option<String>,
        /// Route this provider's models through the Pi allow gate
        /// (default on for `pi-extension`, off for `native-cli`).
        #[arg(long, conflicts_with = "no_gate")]
        gate: bool,
        #[arg(long)]
        no_gate: bool,
        /// Mark the provider as paid/billable.
        #[arg(long, conflicts_with = "free")]
        paid: bool,
        /// Mark the provider as free.
        #[arg(long)]
        free: bool,
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a provider. Refused while its models are referenced or an
    /// active agent uses it.
    Remove {
        id: String,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Subcommand)]
pub(crate) enum ModelAction {
    /// Every model row with its launch scope, live count and the role
    /// defaults selecting it.
    #[command(visible_alias = "ls")]
    List,
    /// One model row: id, provider, ctx/out, thinking/images facts
    /// (null = unknown, never false), scope, live count, role defaults.
    Show { id: String },
    /// Create or update a model. Unknown capability facts stay unknown:
    /// pass neither `--thinking`/`--no-thinking` nor
    /// `--images`/`--no-images` to leave a fact unset.
    Put {
        id: String,
        /// Provider id the model belongs to.
        #[arg(long)]
        provider: Option<String>,
        /// Context window label (for example `200k`).
        #[arg(long)]
        ctx: Option<String>,
        /// Output token label.
        #[arg(long)]
        out: Option<String>,
        /// The model is known to support thinking/reasoning.
        #[arg(long, conflicts_with = "no_thinking")]
        thinking: bool,
        /// The model is known NOT to support thinking.
        #[arg(long)]
        no_thinking: bool,
        /// The model is known to accept image input.
        #[arg(long, conflicts_with = "no_images")]
        images: bool,
        /// The model is known NOT to accept image input.
        #[arg(long)]
        no_images: bool,
        /// Also set the launch scope in the same transaction (gated
        /// providers only) — a convenience for one-shot provisioning;
        /// `model scope` does the same alone.
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Rename a model atomically — scope, profile and role-default
    /// references move with it in the same transaction.
    Rename {
        id: String,
        to: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a model. Refused while a profile or role default
    /// references it or an active agent runs it.
    Remove {
        id: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Set a gated provider model's launch scope: `off`, `allow` (any
    /// use), `master`, or `worker`. Native-CLI provider models have no
    /// gate and refuse.
    Scope {
        id: String,
        #[arg(value_enum)]
        scope: ScopeArg,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum ScopeArg {
    Off,
    Allow,
    Master,
    Worker,
}

impl ScopeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Allow => "allow",
            Self::Master => "master",
            Self::Worker => "worker",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum CatalogAction {
    /// Refresh one provider's model catalog from the host (`omp
    /// models`). The result reports source, fetch time and any error —
    /// a failed fetch is never an empty successful catalog.
    Refresh { provider: String },
    /// Import catalog entries for a provider as scope `off` — discovery
    /// does not authorize a model. Nothing imports without an explicit
    /// selection: repeat `--model <id>` or pass `--all-new` for every
    /// catalog entry the document does not already have. The daemon
    /// recomputes the catalog inside the same call, so a stale client
    /// copy cannot import.
    Import {
        provider: String,
        /// Catalog model id to import; repeatable.
        #[arg(
            long = "model",
            conflicts_with = "all_new",
            required_unless_present = "all_new"
        )]
        models: Vec<String>,
        /// Import every catalog entry not already in the document.
        #[arg(long)]
        all_new: bool,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum RoleArg {
    Pm,
    Research,
    Architect,
    Dev,
    Qa,
    Devops,
    Worker,
}

impl RoleArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pm => "pm",
            Self::Research => "research",
            Self::Architect => "architect",
            Self::Dev => "dev",
            Self::Qa => "qa",
            Self::Devops => "devops",
            Self::Worker => "worker",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum RoleAction {
    /// Configured and effective model selection per team role.
    #[command(visible_alias = "ls")]
    List,
    /// Set one team role's default model.
    Set {
        #[arg(value_enum)]
        role: RoleArg,
        model: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Clear one team role's default (falls back to the provider
    /// default).
    Clear {
        #[arg(value_enum)]
        role: RoleArg,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Subcommand)]
pub(crate) enum ProfileAction {
    /// Every saved profile with version, usedBy, history and generated
    /// export-file state.
    #[command(visible_alias = "ls")]
    List,
    /// One profile: fields plus version, usedBy, history, resolved model
    /// and per-harness export-file state.
    Show { id: String },
    /// Create or update a profile. `--model inherit` selects at launch;
    /// the job document comes from `--job` or `--job-file` (no shell
    /// quoting of a multiline file). `--digest` binds the payload bytes.
    Put {
        id: String,
        #[arg(long, value_enum)]
        role: Option<RoleArg>,
        /// Model id, or `inherit` (the default for a new profile).
        #[arg(long)]
        model: Option<String>,
        /// Inline job text.
        #[arg(long, conflicts_with = "job_file")]
        job: Option<String>,
        /// Job document file; `-` (or a piped stdin) reads stdin.
        #[arg(long, value_name = "FILE")]
        job_file: Option<PathBuf>,
        /// sha256:<hex> of the canonical payload — refuse before send
        /// when the assembled payload does not match.
        #[arg(long, value_name = "sha256:HEX")]
        digest: Option<String>,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Rename a profile atomically — references move with it.
    Rename {
        id: String,
        to: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a profile. Refused while an active agent references it.
    Remove {
        id: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Regenerate Pi/Claude export files from the saved revision. One
    /// profile id or `--all` (the default); unmanaged files are
    /// reported, never overwritten. Saving does not sync.
    Sync {
        /// Sync only this profile.
        profile: Option<String>,
        /// Sync every profile (the default when no id is given).
        #[arg(long, conflicts_with = "profile")]
        all: bool,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Preview a saved profile on a harness: the command, resolved model
    /// and its source, `allowed` and every refusal reason — without
    /// starting anything. Preview is not authentication or launch proof.
    Preview {
        profile: String,
        /// Harness id the profile would launch under.
        #[arg(long)]
        harness: String,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Subcommand)]
pub(crate) enum PinAction {
    /// Every extension pin, including provider pin metadata.
    #[command(visible_alias = "ls")]
    List,
    /// Pin a package to a version (`name@version` validation is
    /// daemon-side). Never installs anything.
    Put {
        pkg: String,
        /// Pinned version spec.
        #[arg(long)]
        pinned: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a pin.
    Remove {
        pkg: String,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Subcommand)]
pub(crate) enum GrantAction {
    /// Request operator approval for one exact settings operation.
    /// `--payload`/`--payload-file` is the ENTIRE intended operation
    /// payload verbatim (for example `{"id":"<model>","scope":"allow"}`)
    /// — the operator's allow binds those exact bytes, so a different
    /// field, a replaced file's content or a moved revision refuses on
    /// use. Prints the request id to hand the operator.
    Request {
        /// Operation to run once granted (for example `model.scope`).
        operation: String,
        /// Intended payload as a JSON object.
        #[arg(long, conflicts_with = "payload_file")]
        payload: Option<String>,
        /// Intended payload JSON from a file; `-` reads stdin.
        #[arg(long, value_name = "FILE")]
        payload_file: Option<PathBuf>,
        /// Revision the eventual mutation will require (defaults to the
        /// revision current when the operator decides).
        #[arg(long, value_name = "REV")]
        expected_revision: Option<String>,
        /// Why the change is needed — shown to the operator.
        #[arg(long)]
        reason: String,
    },
    /// Pending requests and live settings-scoped grants (an agent sees
    /// its own rows; the operator sees all).
    #[command(visible_alias = "ls")]
    List,
    /// Approve one pending request (operator only) — binds the exact
    /// stored payload, its revision, the requester identity and org.
    Allow {
        request_id: String,
        /// Current revision token from `show`.
        #[arg(long, value_name = "REV")]
        expected_revision: String,
    },
    /// Reject one pending request (operator only).
    Reject {
        request_id: String,
        /// Current revision token from `show`.
        #[arg(long, value_name = "REV")]
        expected_revision: String,
    },
    /// Revoke one live grant by id (operator only); it stops matching
    /// immediately.
    Revoke { grant_id: String },
}

/// Shared flag of every document mutation: the observed revision token.
/// Grant authority is the parent's global `--grant`, not a per-mutation
/// flag — a grant binds an operation, so it belongs on every verb.
#[derive(clap::Args, Clone)]
pub(crate) struct Mutation {
    /// Revision token from `show` (`.revision`, `N:hex`). Refused with a
    /// conflict when the document moved — no silent retry.
    #[arg(long, value_name = "REV")]
    expected_revision: String,
}

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

/// Assemble `{operation, expected_revision, payload}` for one action.
/// `state_dir` is `None` on the remote path (file reads are still local).
/// `put` payloads carry only the fields the command line actually gave —
/// the daemon merges them over the stored row inside the settings
/// transaction, so a granted mutation never needs a snapshot read first.
fn request(
    state_dir: Option<&Path>,
    action: &AgentsAction,
    grant: &Option<String>,
) -> Result<(String, Option<String>, Map<String, Value>)> {
    let params = |operation: &str,
                  expected: Option<String>,
                  payload: Map<String, Value>|
     -> Result<(String, Option<String>, Map<String, Value>)> {
        Ok((operation.to_string(), expected, payload))
    };
    let mut planned = match action {
        AgentsAction::Show => params("snapshot", None, Map::new())?,
        AgentsAction::Fleet => params("fleet.check", None, Map::new())?,
        AgentsAction::Apply {
            file,
            mutation,
            digest,
        } => {
            let text = read_payload_file(state_dir, file.as_deref())?;
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
            bind_digest(&mut payload, digest.as_deref())?;
            params("apply", Some(mutation.expected_revision.clone()), payload)?
        }
        AgentsAction::Harness { action } => match action {
            HarnessAction::List => params("harness.list", None, Map::new())?,
            HarnessAction::Show { id } => params("harness.show", None, id_payload(id))?,
            HarnessAction::Put {
                id,
                label,
                cli,
                note,
                mutation,
            } => params(
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
            )?,
            HarnessAction::Remove { id, mutation } => params(
                "harness.remove",
                Some(mutation.expected_revision.clone()),
                id_payload(id),
            )?,
        },
        AgentsAction::Provider { action } => match action {
            ProviderAction::List => params("provider.list", None, Map::new())?,
            ProviderAction::Show { id } => params("provider.show", None, id_payload(id))?,
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
            } => params(
                "provider.put",
                Some(mutation.expected_revision.clone()),
                put_payload(
                    id,
                    &[
                        ("label", label.clone().map(Value::String)),
                        ("kind", kind.map(ProviderKindArg::as_str).map(|k| json!(k))),
                        ("pin", pin.clone().map(Value::String)),
                        (
                            "gate",
                            if *gate {
                                Some(json!(true))
                            } else if *no_gate {
                                Some(json!(false))
                            } else {
                                None
                            },
                        ),
                        (
                            "paid",
                            if *paid {
                                Some(json!(true))
                            } else if *free {
                                Some(json!(false))
                            } else {
                                None
                            },
                        ),
                        ("note", note.clone().map(Value::String)),
                    ],
                ),
            )?,
            ProviderAction::Remove { id, mutation } => params(
                "provider.remove",
                Some(mutation.expected_revision.clone()),
                id_payload(id),
            )?,
        },
        AgentsAction::Model { action } => match action {
            ModelAction::List => params("model.list", None, Map::new())?,
            ModelAction::Show { id } => params("model.show", None, id_payload(id))?,
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
            } => params(
                "model.put",
                Some(mutation.expected_revision.clone()),
                put_payload(
                    id,
                    &[
                        ("provider", provider.clone().map(Value::String)),
                        ("ctx", ctx.clone().map(Value::String)),
                        ("out", out.clone().map(Value::String)),
                        (
                            "thinking",
                            if *thinking {
                                Some(json!(true))
                            } else if *no_thinking {
                                Some(json!(false))
                            } else {
                                None
                            },
                        ),
                        (
                            "images",
                            if *images {
                                Some(json!(true))
                            } else if *no_images {
                                Some(json!(false))
                            } else {
                                None
                            },
                        ),
                        ("scope", scope.map(|s| json!(s.as_str()))),
                    ],
                ),
            )?,
            ModelAction::Rename { id, to, mutation } => {
                let mut payload = id_payload(id);
                payload.insert("to".to_string(), json!(to));
                params(
                    "model.rename",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
            ModelAction::Remove { id, mutation } => params(
                "model.remove",
                Some(mutation.expected_revision.clone()),
                id_payload(id),
            )?,
            ModelAction::Scope {
                id,
                scope,
                mutation,
            } => {
                let mut payload = id_payload(id);
                payload.insert("scope".to_string(), json!(scope.as_str()));
                params(
                    "model.scope",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
        },
        AgentsAction::Catalog { action } => match action {
            CatalogAction::Refresh { provider } => {
                let mut payload = Map::new();
                payload.insert("provider".to_string(), json!(provider));
                params("catalog.refresh", None, payload)?
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
                params(
                    "catalog.import",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
        },
        AgentsAction::Role { action } => match action {
            RoleAction::List => params("role.list", None, Map::new())?,
            RoleAction::Set {
                role,
                model,
                mutation,
            } => {
                let mut payload = Map::new();
                payload.insert("role".to_string(), json!(role.as_str()));
                payload.insert("model".to_string(), json!(model));
                params(
                    "role.set",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
            RoleAction::Clear { role, mutation } => {
                let mut payload = Map::new();
                payload.insert("role".to_string(), json!(role.as_str()));
                params(
                    "role.clear",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
        },
        AgentsAction::Profile { action } => match action {
            ProfileAction::List => params("profile.list", None, Map::new())?,
            ProfileAction::Show { id } => params("profile.show", None, id_payload(id))?,
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
                params(
                    "profile.put",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
            ProfileAction::Rename { id, to, mutation } => {
                let mut payload = id_payload(id);
                payload.insert("to".to_string(), json!(to));
                params(
                    "profile.rename",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
            ProfileAction::Remove { id, mutation } => params(
                "profile.remove",
                Some(mutation.expected_revision.clone()),
                id_payload(id),
            )?,
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
                params(
                    "profile.sync",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
            ProfileAction::Preview {
                profile,
                harness,
                mutation,
            } => {
                let mut payload = Map::new();
                payload.insert("profile".to_string(), json!(profile));
                payload.insert("harness".to_string(), json!(harness));
                params(
                    "profile.preview",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
        },
        AgentsAction::Pin { action } => match action {
            PinAction::List => params("pin.list", None, Map::new())?,
            PinAction::Put {
                pkg,
                pinned,
                mutation,
            } => {
                let mut payload = Map::new();
                payload.insert("pkg".to_string(), json!(pkg));
                payload.insert("pinned".to_string(), json!(pinned));
                params("pin.put", Some(mutation.expected_revision.clone()), payload)?
            }
            PinAction::Remove { pkg, mutation } => {
                let mut payload = Map::new();
                payload.insert("pkg".to_string(), json!(pkg));
                params(
                    "pin.remove",
                    Some(mutation.expected_revision.clone()),
                    payload,
                )?
            }
        },
        AgentsAction::History => params("history", None, Map::new())?,
        AgentsAction::Grant { action } => match action {
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
                params("grant.request", None, p)?
            }
            GrantAction::List => params("grant.list", None, Map::new())?,
            GrantAction::Allow {
                request_id,
                expected_revision,
            } => {
                let mut payload = Map::new();
                payload.insert("request_id".to_string(), json!(request_id));
                params("grant.allow", Some(expected_revision.clone()), payload)?
            }
            GrantAction::Reject {
                request_id,
                expected_revision,
            } => {
                let mut payload = Map::new();
                payload.insert("request_id".to_string(), json!(request_id));
                params("grant.reject", Some(expected_revision.clone()), payload)?
            }
            GrantAction::Revoke { grant_id } => {
                let mut payload = Map::new();
                payload.insert("grant_id".to_string(), json!(grant_id));
                params("grant.revoke", None, payload)?
            }
        },
    };
    // A grant binds an operation — never the grant surface itself.
    if !planned.0.starts_with("grant.") {
        bind_grant(&mut planned.2, grant);
    }
    Ok(planned)
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
        |line| eprintln!("{line}"),
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
