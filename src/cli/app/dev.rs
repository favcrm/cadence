//! Offline `cadence app dev` package-contract validation and preview launcher.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use cadence_agent::error::{Error, Result};
use cadence_agent::output::eprint_line;
use serde_json::Value;

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

/// A command Cadence can prefix with `exec` without truncating a shell list.
/// Quoted metacharacters are ordinary arguments; unquoted separators,
/// redirects, subshells, command substitution and newlines are refused.
pub(super) struct DevCommand {
    pub(super) envs: Vec<(String, String)>,
    pub(super) argv: Vec<String>,
}

pub(super) fn parse_dev_command(text: &str) -> Result<DevCommand> {
    let words = split_shell_words(text)?;
    let mut envs = Vec::new();
    let mut index = 0;
    while let Some((key, value)) = words.get(index).and_then(|word| assignment(word)) {
        envs.push((key.to_owned(), value.to_owned()));
        index += 1;
    }
    if index == words.len() {
        return Err(Error::invalid(
            "app_dev_package",
            "cadence.dev needs a command after its assignments",
        ));
    }
    Ok(DevCommand {
        envs,
        argv: words[index..].to_vec(),
    })
}

fn assignment(word: &str) -> Option<(&str, &str)> {
    let (key, value) = word.split_once('=')?;
    let mut chars = key.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some((key, value))
}

/// Split one command into words without invoking a shell. Quoted separators
/// stay in the argument; unquoted separators, redirects, subshells, command
/// substitution and newlines refuse. Expansions are not performed; an app
/// that needs a shell belongs in a script.
fn split_shell_words(text: &str) -> Result<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\n' || ch == '\r' {
            return Err(Error::invalid(
                "app_dev_package",
                "cadence.dev must be one command; newlines are refused",
            ));
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else if active == '"' && ch == '\\' {
                let next = chars.get(index + 1).ok_or_else(|| {
                    Error::invalid("app_dev_package", "cadence.dev ends with a backslash")
                })?;
                if *next == '\n' || *next == '\r' {
                    return Err(Error::invalid(
                        "app_dev_package",
                        "cadence.dev must be one command; newlines are refused",
                    ));
                }
                if matches!(*next, '$' | '`' | '"' | '\\') {
                    current.push(*next);
                    index += 1;
                } else {
                    current.push('\\');
                }
            } else if ch == '`' || (ch == '$' && chars.get(index + 1) == Some(&'(')) {
                return Err(Error::invalid(
                    "app_dev_package",
                    "cadence.dev does not allow command substitution",
                ));
            } else {
                current.push(ch);
            }
            started = true;
            index += 1;
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                started = true;
            }
            '\\' => {
                let next = chars.get(index + 1).ok_or_else(|| {
                    Error::invalid("app_dev_package", "cadence.dev ends with a backslash")
                })?;
                current.push(*next);
                started = true;
                index += 1;
            }
            ' ' | '\t' => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            ';' | '&' | '|' | '<' | '>' | '`' | '(' | ')' => {
                return Err(Error::invalid(
                    "app_dev_package",
                    "cadence.dev must be one command; put multiple steps in a script",
                ));
            }
            '$' if chars.get(index + 1) == Some(&'(') => {
                return Err(Error::invalid(
                    "app_dev_package",
                    "cadence.dev does not allow command substitution",
                ));
            }
            _ => {
                current.push(ch);
                started = true;
            }
        }
        index += 1;
    }
    if quote.is_some() {
        return Err(Error::invalid(
            "app_dev_package",
            "cadence.dev has an unterminated quote",
        ));
    }
    if started {
        words.push(current);
    }
    Ok(words)
}

fn configure_app_env(
    child: &mut std::process::Command,
    root: &Path,
    app: &str,
    port: u16,
    host: &str,
) {
    child
        .current_dir(root)
        .env_clear()
        .env("CADENCE_APP_ROOT", root)
        .env("CADENCE_APP_NAME", app)
        .env("CADENCE_APP_PORT", port.to_string())
        .env("CADENCE_APP_HOST", host)
        .env("CADENCE_APP_CONTRACT", "1");
    copy_preview_env(child);
}

fn shell_base(root: &Path, app: &str, port: u16, host: &str) -> std::process::Command {
    let mut child = std::process::Command::new("sh");
    configure_app_env(&mut child, root, app, port, host);
    child
}

/// Run the one parsed `dev` command directly, with leading `NAME=value`
/// assignments applied to its environment. No shell means `exec` replaces
/// Cadence cleanly and signals reach the app's own trap.
fn dev_command(
    command: &str,
    root: &Path,
    app: &str,
    port: u16,
    host: &str,
) -> Result<std::process::Command> {
    let DevCommand { envs, argv } = parse_dev_command(command)?;
    let mut child = std::process::Command::new(&argv[0]);
    child.args(&argv[1..]);
    configure_app_env(&mut child, root, app, port, host);
    for (key, value) in envs {
        child.env(key, value);
    }
    Ok(child)
}

fn shell_command(
    command: &str,
    root: &Path,
    app: &str,
    port: u16,
    host: &str,
) -> std::process::Command {
    let mut child = shell_base(root, app, port, host);
    child.arg("-c").arg(command);
    child
}

fn shell_status(
    command: &str,
    root: &Path,
    app: &str,
    port: u16,
    host: &str,
) -> Result<std::process::ExitStatus> {
    let mut child = shell_command(command, root, app, port, host);
    cadence_agent::reaper::status(&mut child)
        .map_err(|e| Error::internal(format!("app dev command: {e}")))
}

fn copy_preview_env(command: &mut std::process::Command) {
    for name in [
        "PATH",
        "HOME",
        "LANG",
        "TERM",
        "PNPM_HOME",
        "XDG_CACHE_HOME",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}
