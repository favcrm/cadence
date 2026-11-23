//! Offline `cadence app dev` package-contract validation and preview launcher.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

mod command;

use std::path::Path;

use cadence_agent::error::{Error, Result};
use cadence_agent::output::eprint_line;
use serde_json::Value;

pub(super) use command::parse_dev_command;
#[cfg(test)]
pub(super) use command::DevCommand;
use command::{copy_preview_env, dev_command, shell_status};

/// No production state, PM, socket or app approval is accessed here.
pub(in crate::cli) fn run_dev(
    name: Option<&str>,
    source: Option<&Path>,
    package: Option<&Path>,
    port: u16,
    host: &str,
    allow_hosts: &[String],
) -> Result<i32> {
    if let Some(package) = package {
        if !allow_hosts.is_empty() {
            return Err(Error::invalid(
                "app_dev_allow_host",
                "--allow-host is only available in legacy --source mode",
            ));
        }
        if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
            return Err(Error::invalid(
                "app_dev_host",
                "package previews must bind loopback",
            ));
        }
        return run_package_dev(package, port, host, true);
    }
    let name = name.ok_or_else(|| {
        Error::invalid(
            "app_dev_mode",
            "pass a preview name with --source, or --package <dir>",
        )
    })?;
    let source = source.ok_or_else(|| {
        Error::invalid(
            "app_dev_mode",
            "pass a preview name with --source, or --package <dir>",
        )
    })?;
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
    copy_preview_env(&mut command);
    let status = cadence_agent::reaper::status(&mut command)
        .map_err(|e| Error::internal(format!("app development harness: {e}")))?;
    Ok(status.code().unwrap_or(1))
}

struct AppPackageContract {
    app: String,
    dev: String,
    check: Option<String>,
}

/// An app-owned preview: the package's `package.json` declares the app
/// name and the commands; Cadence only validates the package and runs
/// them. No daemon, state dir, PM, socket or approval is read.
pub(super) fn run_package_dev(root: &Path, port: u16, host: &str, exec_child: bool) -> Result<i32> {
    let root = root
        .canonicalize()
        .map_err(|e| Error::invalid("app_dev_package", format!("package root: {e}")))?;
    let contract = package_contract(&root)?;
    if let Some(check) = contract.check.as_deref() {
        let status = shell_status(check, &root, &contract.app, port, host)?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }
    }
    let mut child = dev_command(&contract.dev, &root, &contract.app, port, host)?;
    if exec_child {
        use std::os::unix::process::CommandExt;
        let error = child.exec();
        eprint_line(&format!("app dev: {error}"));
        return Ok(launch_error_code(&error));
    }
    match cadence_agent::reaper::status(&mut child) {
        Ok(status) => Ok(status.code().unwrap_or(1)),
        Err(error) => {
            eprint_line(&format!("app dev: {error}"));
            Ok(launch_error_code(&error))
        }
    }
}

fn launch_error_code(error: &std::io::Error) -> i32 {
    if error.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

fn package_contract(root: &Path) -> Result<AppPackageContract> {
    let manifest = root.join("package.json");
    let bytes = std::fs::read(&manifest).map_err(|e| {
        Error::invalid(
            "app_dev_package",
            format!("read {}: {e}", manifest.display()),
        )
    })?;
    let json: Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::invalid("app_dev_package", format!("package.json: {e}")))?;
    let cadence = json
        .get("cadence")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::invalid(
                "app_dev_package",
                "package.json needs a cadence object with app and dev",
            )
        })?;
    let app = cadence
        .get("app")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("app_dev_package", "cadence.app must be a string"))?;
    cadence_agent::proto::identifier(app, "app name")?;
    let dev = package_command(cadence.get("dev"), "cadence.dev", true)?;
    let check = match cadence.get("check") {
        Some(value) => Some(package_command(Some(value), "cadence.check", false)?),
        None => None,
    };
    if !root.join("app/app.md").is_file() {
        return Err(Error::invalid(
            "app_dev_package",
            "package needs app/app.md",
        ));
    }
    Ok(AppPackageContract {
        app: app.to_owned(),
        dev,
        check,
    })
}

fn package_command(value: Option<&Value>, field: &str, one_command: bool) -> Result<String> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("app_dev_package", format!("{field} must be a string")))?;
    if text.trim().is_empty() || text.len() > 4096 {
        return Err(Error::invalid(
            "app_dev_package",
            format!("{field} must be 1..=4096 bytes"),
        ));
    }
    if one_command && parse_dev_command(text).is_err() {
        return Err(Error::invalid(
            "app_dev_package",
            "cadence.dev must be one command; put multiple steps in a script",
        ));
    }
    Ok(text.to_owned())
}
