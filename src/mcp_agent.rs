//! Scoped Cadence tools for a managed Codex endpoint.
//!
//! Codex starts local MCP servers outside its command sandbox. This process
//! and its CLI children remain descendants of the enrolled provider, so the
//! daemon still derives the caller from SO_PEERCRED and /proc ancestry.

use std::io::{BufRead, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};

const TOOL_DEADLINE: Duration = Duration::from_secs(35);
const OUTPUT_LIMIT: usize = 512 * 1024;

pub fn run(state_dir: &Path) -> Result<i32> {
    let alias = std::env::var("CADENCE_ALIAS")
        .map_err(|_| Error::rejected("Cadence agent MCP requires CADENCE_ALIAS"))?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let response = match request["method"].as_str() {
            Some("initialize") => json!({"jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":request.pointer("/params/protocolVersion")
                    .and_then(Value::as_str).unwrap_or("2025-11-25"),
                "capabilities":{"tools":{}},
                "serverInfo":{"name":"cadence-agent","version":env!("CARGO_PKG_VERSION")}
            }}),
            Some("tools/list") => json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools()}}),
            Some("tools/call") => {
                let name = request
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                let result =
                    command_args(name, &args).and_then(|argv| invoke(state_dir, &alias, &argv));
                match result {
                    Ok(text) => json!({"jsonrpc":"2.0","id":id,"result":{
                        "content":[{"type":"text","text":text}]
                    }}),
                    Err(error) => json!({"jsonrpc":"2.0","id":id,"result":{
                        "content":[{"type":"text","text":error.to_string()}],
                        "isError":true
                    }}),
                }
            }
            _ => json!({"jsonrpc":"2.0","id":id,"error":{
                "code":-32601,"message":"method not found"
            }}),
        };
        if writeln!(stdout, "{response}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
    }
    Ok(0)
}

fn tools() -> Value {
    json!([
        {"name":"self","description":"Show your Cadence identity and running work.",
         "inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"wiki_search","description":"Search wiki pages visible to your Cadence identity.",
         "inputSchema":{"type":"object","properties":{
             "q":{"type":"string"},"path":{"type":"string"}},
             "required":["q"],"additionalProperties":false}},
        {"name":"wiki_read","description":"Read a visible wiki page with source metadata.",
         "inputSchema":{"type":"object","properties":{"path":{"type":"string"}},
             "required":["path"],"additionalProperties":false}},
        {"name":"issue_show","description":"Read one Cadence issue by ID.",
         "inputSchema":{"type":"object","properties":{"id":{"type":"string"}},
             "required":["id"],"additionalProperties":false}}
    ])
}

fn fields<'a>(
    args: &'a Value,
    allowed: &[&str],
    required: &[&str],
) -> Result<&'a Map<String, Value>> {
    let object = args
        .as_object()
        .ok_or_else(|| Error::rejected("tool arguments must be an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::rejected(format!("unexpected tool argument '{key}'")));
    }
    for key in required {
        if object
            .get(*key)
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(Error::rejected(format!(
                "required string argument '{key}' is missing"
            )));
        }
    }
    if let Some(key) = object
        .iter()
        .find_map(|(key, value)| (!value.is_string()).then_some(key))
    {
        return Err(Error::rejected(format!(
            "tool argument '{key}' must be a string"
        )));
    }
    Ok(object)
}

fn positional(value: &str) -> Result<String> {
    if value.starts_with('-') {
        return Err(Error::rejected(
            "positional tool values cannot begin with '-'",
        ));
    }
    Ok(value.to_string())
}

fn command_args(name: &str, args: &Value) -> Result<Vec<String>> {
    match name {
        "self" => {
            fields(args, &[], &[])?;
            Ok(vec!["self".into()])
        }
        "wiki_search" => {
            let args = fields(args, &["q", "path"], &["q"])?;
            let mut out = vec![
                "wiki".into(),
                "search".into(),
                positional(args["q"].as_str().unwrap())?,
                "--json".into(),
            ];
            if let Some(path) = args.get("path").and_then(Value::as_str) {
                out.extend(["--path".into(), positional(path)?]);
            }
            Ok(out)
        }
        "wiki_read" => {
            let args = fields(args, &["path"], &["path"])?;
            Ok(vec![
                "wiki".into(),
                "cat".into(),
                positional(args["path"].as_str().unwrap())?,
                "--meta".into(),
            ])
        }
        "issue_show" => {
            let args = fields(args, &["id"], &["id"])?;
            let id = args["id"].as_str().unwrap();
            if !id.starts_with("CAD-") || !id[4..].chars().all(|c| c.is_ascii_digit()) {
                return Err(Error::rejected("issue id must be CAD-<number>"));
            }
            Ok(vec!["issue".into(), "show".into(), id.into()])
        }
        _ => Err(Error::rejected("unknown Cadence agent tool")),
    }
}

fn invoke(state_dir: &Path, alias: &str, argv: &[String]) -> Result<String> {
    let binary = std::env::current_exe()?;
    let mut command = Command::new(binary);
    command.arg("--state-dir").arg(state_dir).args(argv);
    command
        .env("CADENCE_STATE_DIR", state_dir)
        .env("CADENCE_ALIAS", alias);
    let (output, bounds) =
        crate::proc::run_bounded_limited(&mut command, TOOL_DEADLINE, OUTPUT_LIMIT)
            .map_err(|error| Error::provider(format!("Cadence agent tool failed: {error}")))?;
    if bounds.stdout_exceeded || bounds.stderr_exceeded {
        return Err(Error::rejected("Cadence tool output exceeded its limit"));
    }
    if !output.status.success() {
        return Err(Error::rejected(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_actions_and_forged_identity_are_not_bridge_tools() {
        for (name, args) in [
            ("wiki_index_status", json!({})),
            ("wiki_index_refresh", json!({})),
            ("wiki_search", json!({"q":"needle","alias":"operator"})),
            ("wiki_search", json!({"q":"needle","as":"operator"})),
            ("issue_show", json!({"id":"--state-dir"})),
        ] {
            assert!(command_args(name, &args).is_err(), "{name}: {args}");
        }
    }

    #[test]
    fn only_known_cli_argv_can_be_constructed() {
        assert_eq!(command_args("self", &json!({})).unwrap(), ["self"]);
        assert_eq!(
            command_args("wiki_search", &json!({"q":"needle"})).unwrap(),
            ["wiki", "search", "needle", "--json"]
        );
    }
}
