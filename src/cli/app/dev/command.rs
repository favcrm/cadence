//! Shell command parsing and preview environment setup for `cadence app dev`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use cadence_agent::error::{Error, Result};

/// A command Cadence can prefix with `exec` without truncating a shell list.
/// Quoted metacharacters are ordinary arguments; unquoted separators,
/// redirects, subshells, command substitution and newlines are refused.
pub(in super::super) struct DevCommand {
    pub(in super::super) envs: Vec<(String, String)>,
    pub(in super::super) argv: Vec<String>,
}

pub(in super::super) fn parse_dev_command(text: &str) -> Result<DevCommand> {
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
pub(super) fn dev_command(
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

pub(super) fn shell_status(
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

pub(super) fn copy_preview_env(command: &mut std::process::Command) {
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
