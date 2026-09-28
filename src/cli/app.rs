//! CAD-535: `cadence app` — moved verbatim from src/main.rs.

use super::*;

/// `cadence app` verbs (CAD-547). `install`/`update`/`set`/`remove`
/// write the tracker directly — one commit each, `Actor:` recorded —
/// `ls`/`show` read it; `approve` is the operator's daemon gate, like
/// `workflow approve`.
#[derive(Subcommand)]
pub(crate) enum AppAction {
    /// Configure exact publication connections for an installation or context.
    Binding {
        #[command(subcommand)]
        action: BindingAction,
    },
    /// Stage and explicitly approve release of an independently reviewed artifact.
    Effect {
        #[command(subcommand)]
        action: EffectAction,
    },
    /// Optional app-owned content settings; creates no worker or provider authority.
    Context {
        #[command(subcommand)]
        action: ContextAction,
    },
    /// Stable workspace installation IDs; execution and approval are separate.
    Catalog {
        #[command(subcommand)]
        action: CatalogAction,
    },
    /// App-owned local runs with separate execution approval.
    Run {
        #[command(subcommand)]
        action: RunAction,
    },
    /// Run a fixture-only HMR preview from an explicitly trusted local
    /// Cadence source checkout. This executes that checkout's known
    /// development harness, not an installed app or its manifest.
    Dev {
        /// Preview directory name under app-previews/.
        name: String,
        /// Trusted source checkout containing scripts/app-dev.mjs.
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value_t = 3186, value_parser = clap::value_parser!(u16).range(3110..=3199))]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Existing private tailnet hostname; required when sharing.
        #[arg(long)]
        allow_host: Vec<String>,
    },
    /// Install an app folder into the project: `app.md` (frontmatter
    /// `app`, `title`, `version`, `needs.connections`) plus
    /// `workflows/*.md` — every one checked like `workflow check` —
    /// and optional flat `rubrics/`, `templates/` dirs. `<source>` is a
    /// local path or a git URL (cloned, pinned to the commit SHA in the
    /// install record). Nothing in the bundle executes, and nothing in
    /// it runs until `cadence app approve`.
    Install {
        /// App source — a folder path or a git URL.
        source: String,
        /// Project key the app installs into.
        #[arg(long)]
        project: String,
    },
    /// Every installed app: project, name, version, workflows, slots
    /// with bindings, digest and approval state.
    Ls {
        /// Project key; all projects when absent.
        #[arg(long)]
        project: Option<String>,
    },
    /// An installed app's manifest, guide, workflow summaries, slot
    /// bindings, install record, digest and approval state.
    Show {
        /// App name — the `apps/<name>/` folder.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Bind a declared `needs.connections` slot to a connection name —
    /// `<slot>=<connection>`, repeatable; `<slot>=` unbinds it
    /// explicitly. Slots default to `local`. A binding change is
    /// structural — it re-gates the app until `app approve`.
    Set {
        /// App name.
        name: String,
        /// `<slot>=<connection>` pairs — repeatable.
        bindings: Vec<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Replace an installed app from its recorded source (or the given
    /// one), after the same checks `install` runs — prints the diff.
    /// Any structural change re-gates the app until `app approve`.
    Update {
        /// App name.
        name: String,
        /// A folder path or git URL; absent re-reads the recorded
        /// source.
        source: Option<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Remove an installed app — the folder and its install record, one
    /// commit. Refuses while a plan proposed from the app is open.
    Remove {
        /// App name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Approve the app's current structure — `plan propose
    /// --workflow <app>/<wf>` refuses it until this matches the
    /// installed folder. The approval also derives the app's grants:
    /// exactly the scopes its workflow steps declare on their bound
    /// slots, to the agents the app's default team assigns those steps
    /// (CAD-577). Operator only, through the daemon.
    Approve {
        /// App name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Withdraw the app's approval (CAD-577) — `plan propose
    /// --workflow <app>/<wf>` refuses again, and every grant the
    /// approval derived is revoked (a waiting effect that loses a
    /// scope is closed). The counterpart to `approve`; operator only,
    /// through the daemon.
    Revoke {
        /// App name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Record the app's default team (CAD-577): one agent alias per
    /// workflow input role, `<input>=<agent>` repeatable; `<input>=`
    /// clears a role. The team lives with the install record and is
    /// not part of the gate digest, so setting it never re-requires
    /// approval. Operator only, through the daemon.
    SetTeam {
        /// App name.
        name: String,
        /// `<input>=<agent>` pairs — repeatable.
        #[arg(long = "role", required = true)]
        roles: Vec<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Join a new Devin worker for one of the app's team roles (CAD-577)
    /// — the board's "Add worker": a unique role-prefixed alias, under
    /// the operator (a group root), recorded in the app's default team.
    /// Operator only, through the daemon.
    AddWorker {
        /// App name.
        name: String,
        /// The team role the worker fills.
        #[arg(long)]
        role: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum ContextAction {
    /// Create an optional context using explicitly eligible content defaults.
    Create {
        install_id: String,
        #[arg(long)]
        label: String,
        /// Bounded JSON object of content defaults; empty when omitted.
        #[arg(long)]
        defaults: Option<PathBuf>,
        #[arg(long)]
        request_id: String,
    },
    Ls {
        install_id: String,
    },
    Show {
        install_id: String,
        context_id: String,
    },
    /// Replace label and defaults at the exact observed revision.
    Set {
        install_id: String,
        context_id: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        label: String,
        #[arg(long)]
        defaults: PathBuf,
    },
    /// Close unfinished eligibility while retaining historical audit.
    Archive {
        install_id: String,
        context_id: String,
        #[arg(long)]
        expected_revision: u64,
    },
}

#[derive(Subcommand)]
pub(crate) enum BindingAction {
    /// Bind one declared capability slot to an exact connection ID.
    Create {
        install_id: String,
        #[arg(long)]
        context_id: Option<String>,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        connection_id: String,
        #[arg(long)]
        request_id: String,
    },
    /// List bindings, optionally for an exact context.
    Ls {
        install_id: String,
        #[arg(long)]
        context_id: Option<String>,
    },
    /// Inspect one installation's exact binding.
    Show {
        install_id: String,
        binding_id: String,
    },
    /// Change the connection at the expected binding revision.
    Set {
        install_id: String,
        binding_id: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        connection_id: String,
    },
    /// Revoke a binding at its expected revision.
    Revoke {
        install_id: String,
        binding_id: String,
        #[arg(long)]
        expected_revision: u64,
    },
}
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum ResolutionChoice {
    Close,
    Acknowledge,
}
impl ResolutionChoice {
    fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Acknowledge => "acknowledge",
        }
    }
}
#[derive(Subcommand)]
pub(crate) enum EffectAction {
    /// Close an uncertain receipt or acknowledge a flagged terminal outcome.
    /// Resolution retains history and never executes a release.
    Resolve {
        effect_id: String,
        #[arg(long)]
        digest: String,
        #[arg(long, value_enum)]
        resolution: ResolutionChoice,
    },
    /// Stage stored accepted artifact bytes; no caller content or path is accepted.
    Stage {
        run_id: String,
        #[arg(long)]
        artifact_id: String,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        title: String,
    },
    /// Inspect the complete staged effect and approval digest.
    Show { effect_id: String },
    /// List release receipts; context filtering requires an installation.
    Ls {
        #[arg(long)]
        install_id: Option<String>,
        #[arg(long, requires = "install_id")]
        context_id: Option<String>,
    },
    /// Approve the exact staged digest for one outward release.
    Accept {
        effect_id: String,
        #[arg(long)]
        digest: String,
    },
    /// Decline the exact staged digest without publishing.
    Decline {
        effect_id: String,
        #[arg(long)]
        digest: String,
    },
}
fn release_scope(install: Option<&str>, context: Option<&str>) -> serde_json::Value {
    let mut params = json!({});
    if let Some(id) = install {
        params["install_id"] = json!(id);
    }
    if let Some(id) = context {
        params["context_id"] = json!(id);
    }
    params
}
fn binding_params(action: &BindingAction) -> (&'static str, serde_json::Value) {
    match action {
        BindingAction::Create {
            install_id,
            context_id,
            slot,
            connection_id,
            request_id,
        } => {
            let mut params = release_scope(Some(install_id), context_id.as_deref());
            params["slot"] = json!(slot);
            params["connection_id"] = json!(connection_id);
            params["request_id"] = json!(request_id);
            ("app_binding_create", params)
        }
        BindingAction::Ls {
            install_id,
            context_id,
        } => (
            "app_binding_list",
            release_scope(Some(install_id), context_id.as_deref()),
        ),
        BindingAction::Show {
            install_id,
            binding_id,
        } => (
            "app_binding_show",
            json!({"install_id":install_id,"binding_id":binding_id}),
        ),
        BindingAction::Set {
            install_id,
            binding_id,
            expected_revision,
            connection_id,
        } => (
            "app_binding_update",
            json!({"install_id":install_id,"binding_id":binding_id,"expected_revision":expected_revision,"connection_id":connection_id}),
        ),
        BindingAction::Revoke {
            install_id,
            binding_id,
            expected_revision,
        } => (
            "app_binding_revoke",
            json!({"install_id":install_id,"binding_id":binding_id,"expected_revision":expected_revision}),
        ),
    }
}
fn effect_params(action: &EffectAction) -> (&'static str, serde_json::Value) {
    match action {
        EffectAction::Resolve {
            effect_id,
            digest,
            resolution,
        } => (
            "app_effect_resolve",
            json!({"effect_id":effect_id,"digest":digest,"resolution":resolution.as_str()}),
        ),
        EffectAction::Stage {
            run_id,
            artifact_id,
            slot,
            request_id,
            title,
        } => (
            "app_effect_stage",
            json!({"run_id":run_id,"artifact_id":artifact_id,"slot":slot,"request_id":request_id,"title":title}),
        ),
        EffectAction::Show { effect_id } => ("app_effect_show", json!({"effect_id":effect_id})),
        EffectAction::Ls {
            install_id,
            context_id,
        } => (
            "app_effect_list",
            release_scope(install_id.as_deref(), context_id.as_deref()),
        ),
        EffectAction::Accept { effect_id, digest } => (
            "app_effect_decide",
            json!({"effect_id":effect_id,"digest":digest,"decision":"accept"}),
        ),
        EffectAction::Decline { effect_id, digest } => (
            "app_effect_decide",
            json!({"effect_id":effect_id,"digest":digest,"decision":"decline"}),
        ),
    }
}

#[derive(Subcommand)]
pub(crate) enum CatalogAction {
    /// Approve the exact installed digest for supported local artifact steps.
    Approve {
        install_id: String,
        #[arg(long)]
        digest: String,
    },
    /// Revoke local capabilities without changing legacy project approvals.
    Revoke {
        install_id: String,
        #[arg(long)]
        digest: String,
    },
    /// Explicitly resume or roll back a retained catalog migration journal.
    MigrationRecover {
        journal_id: String,
        #[arg(long)]
        rollback: bool,
    },
    /// Install a validated bundle without creating a project or grants.
    Install { source: String },
    /// List catalogued installations. Never migrates on read.
    Ls,
    /// Inspect an exact stable installation ID.
    Show { install_id: String },
    /// Explicitly catalogue/backfill existing legacy installations.
    Migrate,
    /// Resume a retained installation journal after a failed delivery.
    Recover { install_id: String },
}

#[derive(Subcommand)]
pub(crate) enum RunAction {
    /// Freeze a checked local workflow and explicit inputs; does not dispatch.
    Create {
        install_id: String,
        #[arg(long)]
        workflow: String,
        /// JSON object of string inputs, including declared worker roles.
        #[arg(long)]
        inputs: PathBuf,
        /// Idempotency key; reuse with different inputs is refused.
        #[arg(long)]
        request_id: String,
        /// Registered PM whose workers execute this run.
        #[arg(long)]
        owner_pm: String,
        /// Optional discovery link; confers no authority.
        #[arg(long)]
        project_link: Option<String>,
        /// Exact optional brand context; omission means a context-free run.
        #[arg(long)]
        context_id: Option<String>,
        /// Exact previously fetched provider result, if this run uses a source post.
        #[arg(long, requires = "selected_post_id")]
        source_receipt_id: Option<String>,
        #[arg(long, requires = "source_receipt_id")]
        selected_post_id: Option<String>,
    },
    /// Approve the exact frozen run snapshot; does not authorize outward release.
    Approve {
        run_id: String,
        #[arg(long)]
        digest: String,
    },
    /// Cancel a run and prevent further dispatch.
    Cancel { run_id: String },
    /// Dispatch eligible steps of an explicitly approved run.
    Dispatch { run_id: String },
    /// Inspect an exact app-owned run and its output receipts.
    Show { run_id: String },
    /// List app-owned runs, optionally for an exact installation.
    Ls {
        #[arg(long)]
        install_id: Option<String>,
        #[arg(long, requires = "install_id")]
        context_id: Option<String>,
    },
    /// Read a bounded text artifact as the operator or its assigned dependent turn.
    Artifact {
        artifact_id: String,
        /// Active dependent kickoff; requires its current turn token.
        #[arg(long, requires = "token")]
        message: Option<String>,
        #[arg(long, requires = "message")]
        token: Option<String>,
    },
    /// Invoke one frozen read/draft capability during the assigned app turn.
    CapabilityCall {
        #[arg(long)]
        message: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        input_json: String,
    },
    /// Operator view of durable capability receipts for one run.
    CapabilityResults { run_id: String },
    /// Operator or assigned turn view of one exact receipt.
    CapabilityResult {
        receipt_id: String,
        #[arg(long, requires = "token")]
        message: Option<String>,
        #[arg(long, requires = "message")]
        token: Option<String>,
    },
    /// Operator or assigned turn download of one exact bounded asset.
    CapabilityAsset {
        receipt_id: String,
        #[arg(long, requires = "token")]
        message: Option<String>,
        #[arg(long, requires = "message")]
        token: Option<String>,
    },
}

fn read_run_inputs(path: &Path) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_INPUT_BYTES: usize = 32 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_run_inputs", format!("cannot open inputs: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_run_inputs", format!("cannot read inputs: {e}")))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(Error::invalid(
            "app_run_inputs",
            "inputs JSON exceeds 32KiB",
        ));
    }
    let inputs: std::collections::BTreeMap<String, String> = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_run_inputs", "inputs must be a JSON object of strings"))?;
    Ok(json!(inputs))
}

fn run_params(action: &RunAction) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        RunAction::Create {
            install_id,
            workflow,
            inputs,
            request_id,
            owner_pm,
            project_link,
            context_id,
            source_receipt_id,
            selected_post_id,
        } => {
            let mut params = json!({
                "install_id": install_id,
                "workflow": workflow,
                "inputs": read_run_inputs(inputs)?,
                "request_id": request_id,
                "owner_pm": owner_pm,
            });
            if let Some(link) = project_link {
                params["project_link"] = json!(link);
            }
            if let Some(context) = context_id {
                params["context_id"] = json!(context);
            }
            if let (Some(receipt), Some(post)) = (source_receipt_id, selected_post_id) {
                params["source_receipt_id"] = json!(receipt);
                params["selected_post_id"] = json!(post);
            }
            ("app_run_create", params)
        }
        RunAction::Approve { run_id, digest } => {
            ("app_run_approve", json!({"run_id":run_id,"digest":digest}))
        }
        RunAction::Cancel { run_id } => ("app_run_cancel", json!({"run_id":run_id})),
        RunAction::Dispatch { run_id } => ("app_run_dispatch", json!({"run_id":run_id})),
        RunAction::Show { run_id } => ("app_run_show", json!({"run_id":run_id})),
        RunAction::Ls {
            install_id,
            context_id,
        } => {
            let mut params = match install_id {
                Some(id) => json!({"install_id":id}),
                None => json!({}),
            };
            if let Some(context) = context_id {
                params["context_id"] = json!(context);
            }
            ("app_run_list", params)
        }
        RunAction::Artifact {
            artifact_id,
            message,
            token,
        } => {
            let params = match (message, token) {
                (None, None) => json!({"artifact_id":artifact_id}),
                (Some(message), Some(token)) => {
                    json!({"artifact_id":artifact_id,"message":message,"token":token})
                }
                _ => {
                    return Err(Error::invalid(
                        "app_run_artifact",
                        "message and token must be supplied together",
                    ));
                }
            };
            ("app_run_artifact", params)
        }
        RunAction::CapabilityCall {
            message,
            token,
            slot,
            request_id,
            input_json,
        } => {
            let input: Value = serde_json::from_str(input_json).map_err(|_| {
                Error::invalid(
                    "app_run_capability_call",
                    "input-json must be a JSON object",
                )
            })?;
            if !input.is_object() || serde_json::to_vec(&input)?.len() > 64 * 1024 {
                return Err(Error::invalid(
                    "app_run_capability_call",
                    "input-json must be an object within 64 KiB",
                ));
            }
            (
                "app_run_capability_call",
                json!({"message":message,"token":token,"slot":slot,
                "request_id":request_id,"input":input}),
            )
        }
        RunAction::CapabilityResults { run_id } => {
            ("app_run_capability_results", json!({"run_id":run_id}))
        }
        RunAction::CapabilityResult {
            receipt_id,
            message,
            token,
        }
        | RunAction::CapabilityAsset {
            receipt_id,
            message,
            token,
        } => {
            let mut params = json!({"receipt_id":receipt_id});
            if let (Some(message), Some(token)) = (message, token) {
                params["message"] = json!(message);
                params["token"] = json!(token);
            }
            (
                if matches!(action, RunAction::CapabilityResult { .. }) {
                    "app_run_capability_result"
                } else {
                    "app_run_capability_asset"
                },
                params,
            )
        }
    })
}

/// `cadence app …` (CAD-547). `install`/`update`/`set`/`remove` write
/// the tracker directly — one commit each, `Actor:` recorded, all
/// unapproving-by-construction (a tracker write can only change the
/// digest, never the approval record) — `ls`/`show` read; `approve`
/// is the operator's daemon call, like `workflow approve`.
pub(super) fn run_app(state_dir: &Path, action: AppAction) -> Result<i32> {
    use cadence_agent::issue::app;
    let result = match &action {
        AppAction::Binding { action } => {
            let (method, params) = binding_params(action);
            client::rpc(state_dir, method, params)?
        }
        AppAction::Effect { action } => {
            let (method, params) = effect_params(action);
            client::rpc(state_dir, method, params)?
        }
        AppAction::Context { action } => {
            let (method, params) = match action {
                ContextAction::Create {
                    install_id,
                    label,
                    defaults,
                    request_id,
                } => (
                    "app_context_create",
                    json!({"install_id":install_id,"label":label,"input_defaults":match defaults {Some(path)=>read_run_inputs(path)?,None=>json!({})},"request_id":request_id}),
                ),
                ContextAction::Ls { install_id } => {
                    ("app_context_list", json!({"install_id":install_id}))
                }
                ContextAction::Show {
                    install_id,
                    context_id,
                } => (
                    "app_context_show",
                    json!({"install_id":install_id,"context_id":context_id}),
                ),
                ContextAction::Set {
                    install_id,
                    context_id,
                    expected_revision,
                    label,
                    defaults,
                } => (
                    "app_context_update",
                    json!({"install_id":install_id,"context_id":context_id,"expected_revision":expected_revision,"label":label,"input_defaults":read_run_inputs(defaults)?}),
                ),
                ContextAction::Archive {
                    install_id,
                    context_id,
                    expected_revision,
                } => (
                    "app_context_archive",
                    json!({"install_id":install_id,"context_id":context_id,"expected_revision":expected_revision}),
                ),
            };
            client::rpc(state_dir, method, params)?
        }
        AppAction::Catalog { action } => {
            let (method, params) = match action {
                CatalogAction::Approve { install_id, digest } => (
                    "app_local_install_approve",
                    json!({"install_id":install_id,"digest":digest}),
                ),
                CatalogAction::Revoke { install_id, digest } => (
                    "app_local_install_revoke",
                    json!({"install_id":install_id,"digest":digest}),
                ),
                CatalogAction::Install { source } => {
                    let source = if source.contains("://") || source.starts_with("git@") {
                        source.clone()
                    } else {
                        std::fs::canonicalize(source)
                            .map_err(|e| Error::rejected(format!("workspace app source: {e}")))?
                            .to_string_lossy()
                            .into_owned()
                    };
                    ("app_workspace_install", json!({"source":source}))
                }
                CatalogAction::MigrationRecover {
                    journal_id,
                    rollback,
                } => (
                    "app_workspace_migration_recover",
                    json!({"journal_id":journal_id,"rollback":rollback}),
                ),
                CatalogAction::Ls => ("app_workspace_list", json!({})),
                CatalogAction::Show { install_id } => {
                    ("app_workspace_show", json!({"install_id":install_id}))
                }
                CatalogAction::Migrate => ("app_workspace_migrate", json!({})),
                CatalogAction::Recover { install_id } => {
                    ("app_workspace_recover", json!({"install_id":install_id}))
                }
            };
            client::rpc(state_dir, method, params)?
        }
        AppAction::Run { action } => {
            let (method, params) = run_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Dev {
            name,
            source,
            port,
            host,
            allow_host,
        } => {
            return run_dev(name, source, *port, host, allow_host);
        }
        AppAction::Install { source, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::install(&pm, project, source, state_dir, "")?
        }
        AppAction::Ls { project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::ls(&pm, project.as_deref(), state_dir)?
        }
        AppAction::Show { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::show(&pm, project, name, state_dir)?
        }
        AppAction::Set {
            name,
            bindings,
            project,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::set(&pm, project, name, bindings, state_dir, "")?
        }
        AppAction::Update {
            name,
            source,
            project,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::update(&pm, project, name, source.as_deref(), state_dir, "")?
        }
        AppAction::Remove { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::remove(&pm, project, name, state_dir, "")?
        }
        AppAction::Approve { name, project } => client::rpc(
            state_dir,
            "app_approve",
            json!({"project": project, "name": name}),
        )?,
        AppAction::Revoke { name, project } => client::rpc(
            state_dir,
            "app_revoke",
            json!({"project": project, "name": name}),
        )?,
        AppAction::SetTeam {
            name,
            roles,
            project,
        } => client::rpc(
            state_dir,
            "app_set_team",
            json!({"project": project, "name": name, "team": roles}),
        )?,
        AppAction::AddWorker {
            name,
            role,
            project,
        } => client::rpc(
            state_dir,
            "app_add_worker",
            json!({"project": project, "name": name, "role": role}),
        )?,
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: AppAction) -> Result<i32> {
    run_app(&state_dir, action)
}

/// No production state, PM, socket or app approval is accessed here.
pub(super) fn run_dev(
    name: &str,
    source: &Path,
    port: u16,
    host: &str,
    allow_hosts: &[String],
) -> Result<i32> {
    let source = source
        .canonicalize()
        .map_err(|e| Error::invalid("app_dev_source", format!("trusted source checkout: {e}")))?;
    let harness = source.join("scripts/app-dev.mjs");
    if !harness.is_file() || !source.join("ui/package.json").is_file() {
        return Err(Error::invalid("app_dev_source", "source must be a trusted Cadence checkout with scripts/app-dev.mjs and UI dependencies"));
    }
    let mut command = std::process::Command::new("node");
    command
        .arg(&harness)
        .arg(name)
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg(host)
        .current_dir(&source);
    for host in allow_hosts {
        command.arg("--allow-host").arg(host);
    }
    // The preview needs tools, not a native identity, state dir, provider
    // keys, browser cookies or the operator's daemon/session environment.
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "LANG",
        "TERM",
        "PNPM_HOME",
        "XDG_CACHE_HOME",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let status = cadence_agent::reaper::status(&mut command)
        .map_err(|e| Error::internal(format!("app development harness: {e}")))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_run_inputs_reject_non_string_maps_without_echoing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inputs.json");
        for text in [
            r#"["private input"]"#,
            r#"{"source":123}"#,
            r#"{"source":{"private input":"value"}}"#,
            "private input",
        ] {
            std::fs::write(&path, text).unwrap();
            let error = read_run_inputs(&path).unwrap_err().to_string();
            assert!(error.contains("JSON object of strings"));
            assert!(!error.contains("private input"));
        }
        std::fs::write(&path, r#"{"source":"source text","writer":"op-writer"}"#).unwrap();
        assert_eq!(
            read_run_inputs(&path).unwrap(),
            json!({"source":"source text","writer":"op-writer"})
        );
    }

    #[test]
    fn app_run_inputs_bound_utf8_bytes_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inputs.json");
        let text = json!({"source":"界".repeat(11_000)}).to_string();
        std::fs::write(&path, text).unwrap();
        assert!(read_run_inputs(&path)
            .unwrap_err()
            .to_string()
            .contains("exceeds 32KiB"));
    }
}
