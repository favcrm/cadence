//! CAD-535: `cadence restore` — moved verbatim from src/main.rs.
//! CAD-1163: `--portable <SOURCE>` restores a `cadence.portable/1` package
//! as an INACTIVE store at `--state-dir`; the legacy path is unchanged.
//! `--portable --replace` forwards to the live controller's
//! `portable_replace` RPC, which authorizes by its own operator connection.
//! `--portable --activate` forwards to `portable_activate` the same way.

use super::*;

/// The `--replace` flags. Nothing here is a proof: the controller decides.
pub(super) struct Replace {
    pub enabled: bool,
    pub controller_state: Option<PathBuf>,
    pub expected_package_digest: Option<String>,
    pub expected_destination_digest: Option<String>,
    pub expected_generation: Option<String>,
}

/// The `--activate` flags. The live controller authorizes the request.
pub(super) struct Activation {
    pub enabled: bool,
    pub reconciliation: Option<PathBuf>,
}

/// Everything `restore` takes besides the destination and the source.
pub(super) struct Options {
    pub state_dir: PathBuf,
    pub source: Option<PathBuf>,
    pub repos: Vec<PathBuf>,
    pub force: bool,
    pub portable: bool,
    pub preflight: bool,
    pub replace: Replace,
    pub activate: Activation,
    /// Offline activation (AOS-339): restore into a new dir and activate, or
    /// plan it. Never touches a live controller.
    pub activate_disabled: bool,
    pub activate_preflight: bool,
    /// Remote-only (`--org`) flags; every local form refuses them.
    pub request_id: Option<String>,
    pub status: Option<String>,
    pub rollback: Option<String>,
}

/// Refuse every flag combination that does not belong to the chosen action.
/// Runs before any RPC, file read or write, so a misplaced knob fails as
/// usage instead of being ignored.
fn check_flags(o: &Options) -> Result<()> {
    if o.request_id.is_some() || o.status.is_some() || o.rollback.is_some() {
        return Err(Error::usage(
            "--request-id, --status and --rollback are remote `--portable` options; a local restore has no remote operation to run",
        ));
    }
    if o.source.is_none() {
        return Err(Error::usage(
            "restore needs a source — a backup manifest, an export bundle, or a portable package directory",
        ));
    }
    let digests = o.replace.expected_package_digest.is_some()
        || o.replace.expected_destination_digest.is_some()
        || o.replace.expected_generation.is_some();
    let controller = o.replace.controller_state.is_some();
    let reconciliation = o.activate.reconciliation.is_some();
    if !o.portable {
        if o.preflight
            || o.replace.enabled
            || o.activate.enabled
            || o.activate_disabled
            || o.activate_preflight
            || controller
            || digests
            || reconciliation
        {
            return Err(Error::usage(
                "--preflight, --replace, --activate, --activate-disabled, --activate-preflight, --controller-state, --expected-* and --reconciliation need --portable",
            ));
        }
        return Ok(());
    }
    if o.activate_disabled || o.activate_preflight {
        // Offline activation never reaches a controller.
        if o.preflight || o.replace.enabled || o.activate.enabled {
            return Err(Error::usage(
                "choose one of --preflight, --replace, --activate, --activate-disabled or --activate-preflight",
            ));
        }
        if o.force || !o.repos.is_empty() {
            return Err(Error::usage("--portable does not take --repo or --force"));
        }
        if controller || digests {
            return Err(Error::usage(
                "--activate-disabled and --activate-preflight run offline; they take no --controller-state or --expected-* digests",
            ));
        }
        if o.activate_preflight && reconciliation {
            return Err(Error::usage(
                "--activate-preflight takes no --reconciliation; read the required list from its output",
            ));
        }
        return Ok(());
    }
    if o.force || !o.repos.is_empty() {
        return Err(Error::usage(
            "--portable does not take --repo or --force; replacement and activation need --controller-state",
        ));
    }
    if o.replace.enabled && (o.preflight || o.activate.enabled) {
        return Err(Error::usage(
            "choose one of --preflight, --replace or --activate",
        ));
    }
    let live = o.preflight || o.replace.enabled || o.activate.enabled;
    if !live {
        if controller || digests || reconciliation {
            return Err(Error::usage(
                "a plain --portable restore takes no --controller-state, --expected-* or --reconciliation; use --preflight, --replace or --activate",
            ));
        }
        return Ok(());
    }
    if !controller {
        return Err(Error::usage(
            "needs --controller-state (the live controller's state dir)",
        ));
    }
    if o.preflight {
        if digests || reconciliation {
            return Err(Error::usage(
                "--preflight takes no --expected-* digests or --reconciliation; read them from its output",
            ));
        }
        return Ok(());
    }
    // --replace or --activate: both need the exact digests from preflight.
    let complete = o.replace.expected_package_digest.is_some()
        && o.replace.expected_destination_digest.is_some()
        && o.replace.expected_generation.is_some();
    if !complete {
        return Err(Error::usage(
            "--replace and --activate need --expected-package-digest, --expected-destination-digest and --expected-generation",
        ));
    }
    if reconciliation && !o.activate.enabled {
        return Err(Error::usage(
            "--reconciliation applies only to --activate or --activate-disabled",
        ));
    }
    Ok(())
}

pub(super) fn run(opts: Options) -> Result<i32> {
    check_flags(&opts)?;
    let Options {
        state_dir,
        source,
        repos,
        force,
        portable,
        preflight,
        replace,
        activate,
        activate_disabled,
        activate_preflight,
        request_id: _,
        status: _,
        rollback: _,
    } = opts;
    // `check_flags` refused `None`; a local restore always has a source.
    let source = source.expect("source required by check_flags");
    if !portable {
        let opts = cadence_agent::backup::RestoreOptions { force, repos };
        print_json(&cadence_agent::backup::restore(&source, &state_dir, &opts)?);
        return Ok(0);
    }
    if activate_preflight {
        // Read-only plan: the package digest and the exact list of non-terminal
        // rows the caller must name. Nothing is written anywhere.
        print_json(&cadence_agent::backup::portable::activate_preflight(
            &source,
        )?);
        return Ok(0);
    }
    if activate_disabled {
        // Offline restore + activation, manual mode, execution disabled. The
        // destination must not exist; the activation runs on the private stage
        // before publish, so a refusal leaves nothing behind.
        let reconciliation = match activate.reconciliation {
            Some(path) => {
                let text = std::fs::read_to_string(&path)?;
                let value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| Error::usage(format!("reconciliation file: {e}")))?;
                cadence_agent::backup::portable_activate::parse_reconciliation(
                    &value["reconciliation"],
                )
                .map_err(|e| Error::usage(format!("reconciliation file: {e}")))?
            }
            None => Vec::new(),
        };
        print_json(&cadence_agent::backup::portable::restore_activate_disabled(
            &source,
            &state_dir,
            &reconciliation,
        )?);
        return Ok(0);
    }
    if preflight && activate.enabled {
        let controller = replace.controller_state.expect("checked by check_flags");
        // The daemon's activation preflight reports the target digest,
        // generation and required inventory but not the package digest.
        // Compute it locally from the same manifest bytes the activation
        // request will bind; nothing is written.
        let package_digest = cadence_agent::backup::portable_replace::package_digest(&source)?;
        let mut result = client::rpc(
            &controller,
            "portable_activate_preflight",
            json!({"destination": state_dir, "controller_state": controller}),
        )?;
        if let Some(obj) = result.as_object_mut() {
            obj.insert("package_digest".into(), json!(package_digest));
        }
        print_json(&result);
        return Ok(0);
    }
    if preflight {
        // Read-only: forwarded to the live controller, which applies the
        // same operator connection and controller check as --replace.
        let controller = replace.controller_state.expect("checked by check_flags");
        let result = client::rpc(
            &controller,
            "portable_replace_preflight",
            json!({
                "source": source,
                "destination": state_dir,
                "controller_state": controller,
            }),
        )?;
        print_json(&result);
        return Ok(0);
    }
    if activate.enabled {
        let controller = replace.controller_state.expect("checked by check_flags");
        let (Some(pkg), Some(dest), Some(generation)) = (
            replace.expected_package_digest,
            replace.expected_destination_digest,
            replace.expected_generation,
        ) else {
            unreachable!("checked by check_flags");
        };
        // An absent reconciliation file is an empty list. The live
        // controller refuses any unlisted non-terminal row.
        let reconciliation = match activate.reconciliation {
            Some(path) => {
                let text = std::fs::read_to_string(&path)?;
                let value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| Error::usage(format!("reconciliation file: {e}")))?;
                value
            }
            None => json!({"reconciliation": []}),
        };
        let result = client::rpc(
            &controller,
            "portable_activate",
            json!({
                "source": source,
                "destination": state_dir,
                "controller_state": controller,
                "expected_package_digest": pkg,
                "expected_destination_digest": dest,
                "expected_generation": generation,
                "execution_mode": "disabled",
                "bindings": [],
                "reconciliation": reconciliation["reconciliation"],
            }),
        )?;
        print_json(&result);
        return Ok(0);
    }
    if replace.enabled {
        let controller = replace.controller_state.expect("checked by check_flags");
        let (Some(pkg), Some(dest), Some(generation)) = (
            replace.expected_package_digest,
            replace.expected_destination_digest,
            replace.expected_generation,
        ) else {
            unreachable!("checked by check_flags");
        };
        // The request goes to the live controller only. The target dir is
        // passed as data; this process never mutates or starts it.
        let result = client::rpc(
            &controller,
            "portable_replace",
            json!({
                "source": source,
                "destination": state_dir,
                "controller_state": controller,
                "expected_package_digest": pkg,
                "expected_destination_digest": dest,
                "expected_generation": generation,
            }),
        )?;
        print_json(&result);
        return Ok(0);
    }
    let opts = cadence_agent::backup::portable::PortableRestoreOptions { replace: false };
    print_json(&cadence_agent::backup::portable::restore_inactive(
        &source, &state_dir, &opts,
    )?);
    Ok(0)
}
