//! CAD-535: `cadence restore` — moved verbatim from src/main.rs.
//! CAD-1163: `--portable <SOURCE>` restores a `cadence.portable/1` package
//! as an INACTIVE store at `--state-dir`; the legacy path is unchanged.
//! `--portable --replace` forwards to the live controller's
//! `portable_replace` RPC, which authorizes by its own operator connection.
//! `--portable --activate` forwards to `portable_activate` the same way.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

mod flags;

use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use serde_json::json;

use super::print_json;
use flags::check_flags;
pub(super) use flags::{Activation, Options, Replace};

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
