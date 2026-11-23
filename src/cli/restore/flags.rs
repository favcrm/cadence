//! Flag types and validation for `cadence restore`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::PathBuf;

use cadence_agent::error::{Error, Result};

/// The `--replace` flags. Nothing here is a proof: the controller decides.
pub(in super::super) struct Replace {
    pub enabled: bool,
    pub controller_state: Option<PathBuf>,
    pub expected_package_digest: Option<String>,
    pub expected_destination_digest: Option<String>,
    pub expected_generation: Option<String>,
}

/// The `--activate` flags. The live controller authorizes the request.
pub(in super::super) struct Activation {
    pub enabled: bool,
    pub reconciliation: Option<PathBuf>,
}

/// Everything `restore` takes besides the destination and the source.
pub(in super::super) struct Options {
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
pub(super) fn check_flags(o: &Options) -> Result<()> {
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
        if o.preflight
            || o.replace.enabled
            || o.activate.enabled
            || (o.activate_disabled && o.activate_preflight)
        {
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
