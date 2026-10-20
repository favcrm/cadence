//! Scoped Cadence tools for a managed Codex endpoint.
//!
//! Codex starts local MCP servers outside its command sandbox. This process
//! remains a descendant of the enrolled provider, so the daemon derives the
//! caller from SO_PEERCRED and /proc ancestry. `self` and `app_run_artifact`
//! call those existing daemon RPCs in this process. `app_run_artifact` is
//! only that one guarded method and the tool's three strings; it is not a
//! general RPC tool, and it does not spawn a CLI child that would carry the
//! turn token in argv. The other tools still run the Cadence CLI as children
//! of this process, which keeps the same enrolled ancestry.

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
    // Discovery is best effort and happens only after `initialize` has been
    // answered: the managed client allows ten seconds to start the server,
    // and a stalled daemon must not cost it even the static tools.
    let mut discovered: Option<Vec<PlatformTool>> = None;
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
            Some("tools/list") => {
                let list = discovered.get_or_insert_with(|| platform_tools(state_dir));
                json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools(&*list)}})
            }
            Some("tools/call") => {
                let name = request
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                let list = discovered.get_or_insert_with(|| platform_tools(state_dir));
                let result = command_args(name, &args, &*list)
                    .and_then(|argv| invoke(state_dir, &alias, &argv));
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

fn tools(platform_tools: &[PlatformTool]) -> Value {
    let mut tools = json!([
        {"name":"self","description":"Show your Cadence identity and your own held turns. Each running entry is {id, turn_id, task} for this endpoint only. Use id as message and turn_id as token with app_run_artifact.",
         "inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"wiki_search","description":"Lexical search of wiki pages visible to your Cadence identity. Not semantic: every token in q must appear (AND); punctuation splits tokens; only the first 12 tokens are used; q is at most 256 bytes. A full question usually matches nothing, so pass scoped keywords instead: the vendor, product or entity, the operation or field name, and the exact identifier, version or number from the question (e.g. '<vendor> <operation>', '<product> <field> <version>'). Drop filler words. Use path to limit to a collection you are authorized for (an organization/global folder, a project folder such as projects/<key>, or a private folder your identity may read); do not narrow by default. Results are candidates, not evidence: wiki_read each source before answering, and an empty result is not proof of absence.",
         "inputSchema":{"type":"object","properties":{
             "q":{"type":"string","description":"Scoped keywords; all tokens must match; first 12 tokens only."},
             "path":{"type":"string","description":"Optional authorized wiki folder prefix, e.g. global, projects/<key>, or global/pms/eva."}},
             "required":["q"],"additionalProperties":false}},
        {"name":"wiki_read","description":"Read a visible wiki page with its canonical path and original revision. Cite that path and revision, plus the section or PDF page when known. A full page whose JSON line would exceed the native tool line bound (50000 bytes) must be read with offset and bytes. Pass the returned rev on every later range of that page; a different revision is refused and returns no body. offset is a UTF-8 byte index, bytes is the exclusive length, and the reply's range is the exact slice.",
         "inputSchema":{"type":"object","properties":{
             "path":{"type":"string"},
             "offset":{"type":"integer","minimum":0,"description":"UTF-8 byte offset into the original. Omit for the start, or for a full read when the page fits."},
             "bytes":{"type":"integer","minimum":0,"description":"Maximum original bytes. Omit to take the longest slice that fits the line bound."},
             "rev":{"type":"string","description":"Original revision from an earlier range of this page. Required on every range after the first."}},
             "required":["path"],"additionalProperties":false}},
        {"name":"issue_show","description":"Read one Cadence issue by ID.",
         "inputSchema":{"type":"object","properties":{"id":{"type":"string"}},
             "required":["id"],"additionalProperties":false}},
        {"name":"app_run_artifact","description":"Read one text dependency artifact for your current assigned app turn. Required strings: artifact_id from the kickoff, plus message and token equal to the id and turn_id from self. Alias, provider, path, and command are refused.",
         "inputSchema":{"type":"object","properties":{
             "artifact_id":{"type":"string"},
             "message":{"type":"string"},
             "token":{"type":"string"}},
             "required":["artifact_id","message","token"],"additionalProperties":false}}
    ]);
    if let Some(array) = tools.as_array_mut() {
        for tool in platform_tools {
            array.push(json!({
                "name": tool.name,
                "description": format!(
                    "{} (platform {}, effect {}). Runs through the Cadence platform gate; the caller is this connection.",
                    tool.description, tool.platform, tool.effect
                ),
                "inputSchema": {"type":"object","properties":{},"additionalProperties":true}
            }));
        }
    }
    tools
}

/// One reviewed adapter tool exposed to the agent. Only tools the adapter
/// can execute through `platform_call` are listed; an app-run-only tool is
/// not offered as a promise this surface cannot keep.
struct PlatformTool {
    name: String,
    platform: String,
    tool: String,
    description: String,
    effect: String,
}

const STATIC_TOOL_NAMES: [&str; 5] = [
    "self",
    "wiki_search",
    "wiki_read",
    "issue_show",
    "app_run_artifact",
];

/// MCP tool names allow only `[A-Za-z0-9_-]`; a platform slug such as
/// `smtp` and a tool slug such as `email.send` must survive the join.
fn mcp_name_part(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn platform_tools(state_dir: &Path) -> Vec<PlatformTool> {
    // Bounded end to end: `rpc_deadline` puts connect, the write and the
    // complete reply under one five-second deadline, so a stale daemon
    // cannot block the single MCP request loop.
    let Ok(listing) = crate::client::rpc_deadline(
        state_dir,
        "platform_tools",
        json!({}),
        Duration::from_secs(5),
    ) else {
        return Vec::new();
    };
    let mut rows: Vec<(&str, &str, bool, &Value)> = Vec::new();
    for platform in listing["platforms"].as_array().into_iter().flatten() {
        let Some(platform_name) = platform["platform"].as_str() else {
            continue;
        };
        for tool in platform["tools"].as_array().into_iter().flatten() {
            let Some(tool_name) = tool["tool"].as_str() else {
                continue;
            };
            rows.push((
                platform_name,
                tool_name,
                tool["direct"].as_bool() == Some(true),
                tool,
            ));
        }
    }
    let mut out = Vec::new();
    for (name, platform, tool) in generated_tool_names(rows.iter().map(|row| (row.0, row.1, row.2)))
    {
        let Some(row) = rows
            .iter()
            .find(|row| row.0 == platform && row.1 == tool)
            .map(|row| row.3)
        else {
            continue;
        };
        out.push(PlatformTool {
            name,
            platform,
            tool,
            description: row["label"]
                .as_str()
                .unwrap_or("Cadence platform tool")
                .to_string(),
            effect: row["effect"].as_str().unwrap_or("unknown").to_string(),
        });
    }
    out
}

pub(crate) fn static_tool_names() -> &'static [&'static str] {
    &STATIC_TOOL_NAMES
}

/// Generated MCP names for one reviewed inventory, in one deterministic
/// order for every consumer. A name that collides with a static tool or an
/// earlier generated name gets a stable digest suffix instead of silently
/// overwriting another tool.
pub(crate) fn generated_tool_names<'a>(
    entries: impl Iterator<Item = (&'a str, &'a str, bool)>,
) -> Vec<(String, String, String)> {
    let mut used: std::collections::HashSet<String> = STATIC_TOOL_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let mut out = Vec::new();
    for (platform, tool, direct) in entries {
        if !direct {
            continue;
        }
        let name = platform_tool_name(platform, tool, &mut used);
        out.push((name, platform.to_string(), tool.to_string()));
    }
    out
}

fn platform_tool_name(
    platform: &str,
    tool: &str,
    used: &mut std::collections::HashSet<String>,
) -> String {
    let base = format!("{}_{}", mcp_name_part(platform), mcp_name_part(tool));
    if used.insert(base.clone()) {
        return base;
    }
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(platform.as_bytes());
    digest.update(b"\0");
    digest.update(tool.as_bytes());
    let hex = format!("{:x}", digest.finalize());
    // Deterministic and unique: an earlier base may already occupy the
    // first suffixed spelling, so keep appending an attempt counter (and
    // stay within the MCP 64-character name bound).
    let mut attempt = 0u32;
    loop {
        let suffix = match attempt {
            0 => format!("_{}", &hex[..6]),
            attempt => format!("_{}_{attempt}", &hex[..6]),
        };
        let room = 64usize.saturating_sub(suffix.len());
        let stem: String = base.chars().take(room).collect();
        let candidate = format!("{stem}{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        attempt = attempt.saturating_add(1);
    }
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

fn command_args(name: &str, args: &Value, platform_tools: &[PlatformTool]) -> Result<Vec<String>> {
    if let Some(tool) = platform_tools.iter().find(|tool| tool.name == name) {
        if !args.is_object() {
            return Err(Error::rejected("tool arguments must be an object"));
        }
        return Ok(vec![
            "platform".into(),
            "call".into(),
            tool.tool.clone(),
            "--platform".into(),
            tool.platform.clone(),
            "--input".into(),
            serde_json::to_string(args)
                .map_err(|_| Error::internal("platform tool arguments do not serialize"))?,
        ]);
    }
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
        "wiki_read" => wiki_read_args(args),
        "issue_show" => {
            let args = fields(args, &["id"], &["id"])?;
            let id = args["id"].as_str().unwrap();
            if !id.starts_with("CAD-") || !id[4..].chars().all(|c| c.is_ascii_digit()) {
                return Err(Error::rejected("issue id must be CAD-<number>"));
            }
            Ok(vec!["issue".into(), "show".into(), id.into()])
        }
        "app_run_artifact" => {
            let args = fields(
                args,
                &["artifact_id", "message", "token"],
                &["artifact_id", "message", "token"],
            )?;
            Ok(vec![
                "app".into(),
                "run".into(),
                "artifact".into(),
                positional(args["artifact_id"].as_str().unwrap())?,
                "--message".into(),
                positional(args["message"].as_str().unwrap())?,
                "--token".into(),
                positional(args["token"].as_str().unwrap())?,
            ])
        }
        _ => Err(Error::rejected("unknown Cadence agent tool")),
    }
}

fn tool_u64(object: &Map<String, Value>, key: &str) -> Result<Option<u64>> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| {
                Error::rejected(format!(
                    "tool argument '{key}' must be a non-negative integer"
                ))
            })
            .map(Some),
        Some(_) => Err(Error::rejected(format!(
            "tool argument '{key}' must be a non-negative integer"
        ))),
    }
}

fn wiki_read_args(args: &Value) -> Result<Vec<String>> {
    let object = args
        .as_object()
        .ok_or_else(|| Error::rejected("tool arguments must be an object"))?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "path" | "offset" | "bytes" | "rev"))
    {
        return Err(Error::rejected(format!("unexpected tool argument '{key}'")));
    }
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| Error::rejected("required string argument 'path' is missing"))?;
    let offset = tool_u64(object, "offset")?;
    let bytes = tool_u64(object, "bytes")?;
    let rev = match object.get("rev") {
        None => None,
        Some(Value::String(rev)) if !rev.is_empty() => Some(positional(rev)?),
        Some(_) => return Err(Error::rejected("tool argument 'rev' must be a string")),
    };
    // `rev` is moved into argv below; the unbounded flag has to be known first.
    let unbounded = offset.is_none() && bytes.is_none() && rev.is_none();
    let mut argv = vec!["wiki".into(), "cat".into(), positional(path)?];
    if let Some(offset) = offset {
        argv.extend(["--offset".into(), offset.to_string()]);
    }
    if let Some(bytes) = bytes {
        argv.extend(["--bytes".into(), bytes.to_string()]);
    }
    if let Some(rev) = rev {
        argv.extend(["--rev".into(), rev]);
    }
    if unbounded {
        argv.push("--meta".into());
    }
    Ok(argv)
}

fn invoke(state_dir: &Path, alias: &str, argv: &[String]) -> Result<String> {
    if argv == ["self"] {
        // The ordinary CLI `self` command reads CADENCE_ALIAS for display;
        // an idle forged alias could therefore look valid. This RPC derives
        // identity only from the enrolled MCP process's peer ancestry.
        let identity = crate::client::rpc(state_dir, "agent_identity", json!({}))?;
        if identity["alias"].as_str() != Some(alias) {
            return Err(Error::rejected(
                "Cadence MCP identity differs from endpoint",
            ));
        }
        return Ok(identity.to_string());
    }
    if argv.len() == 8
        && argv[0] == "app"
        && argv[1] == "run"
        && argv[2] == "artifact"
        && argv[4] == "--message"
        && argv[6] == "--token"
    {
        // Same guarded route, without putting the turn token in child argv.
        let artifact = crate::client::rpc(
            state_dir,
            "app_run_artifact",
            json!({"artifact_id": argv[3], "message": argv[5], "token": argv[7]}),
        )?;
        return Ok(artifact.to_string());
    }
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
            (
                "app_run_artifact",
                json!({"artifact_id":"artifact-1","message":"m1","token":"t1","alias":"other"}),
            ),
            (
                "app_run_artifact",
                json!({"artifact_id":"artifact-1","message":"m1","token":"t1","provider":"codex"}),
            ),
            (
                "app_run_artifact",
                json!({"artifact_id":"artifact-1","message":"m1","token":"t1","path":"/tmp/x"}),
            ),
            (
                "app_run_artifact",
                json!({"artifact_id":"artifact-1","message":"m1","token":"t1","command":"sh"}),
            ),
            (
                "app_run_artifact",
                json!({"artifact_id":"artifact-1","message":"m1"}),
            ),
            (
                "app_run_artifact",
                json!({"artifact_id":"artifact-1","token":"t1"}),
            ),
            (
                "app_run_artifact",
                json!({"artifact_id":"artifact-1","message":"m1","token":""}),
            ),
            (
                "app_run_artifact",
                json!({"artifact_id":"--help","message":"m1","token":"t1"}),
            ),
        ] {
            assert!(command_args(name, &args, &[]).is_err(), "{name}: {args}");
        }
    }

    #[test]
    fn a_generated_platform_tool_marshals_its_arguments() {
        let generated = [PlatformTool {
            name: "agenticos_post_draft".into(),
            platform: "agenticos".into(),
            tool: "post_draft".into(),
            description: "Draft".into(),
            effect: "draft".into(),
        }];
        let argv = command_args(
            "agenticos_post_draft",
            &json!({"connection_id":"conn-1"}),
            &generated,
        )
        .unwrap();
        let expected: Vec<String> = [
            "platform",
            "call",
            "post_draft",
            "--platform",
            "agenticos",
            "--input",
            "{\"connection_id\":\"conn-1\"}",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(argv, expected);
        // The static five stay reachable, and the listing grows by one.
        assert_eq!(tools(&[]).as_array().unwrap().len(), 5);
        assert_eq!(tools(&generated).as_array().unwrap().len(), 6);
        // A dotted tool slug survives as an MCP-safe name.
        assert_eq!(mcp_name_part("email.send"), "email_send");
        assert_eq!(mcp_name_part("account-images"), "account-images");
    }

    #[test]
    fn generated_names_do_not_collide() {
        let names = generated_tool_names(
            [
                ("a.b", "c", true),
                ("a_b", "c", true),
                ("local", "hidden", false),
            ]
            .into_iter(),
        );
        assert_eq!(names.len(), 2, "a non-direct tool is absent");
        assert_ne!(names[0].0, names[1].0);
        assert_eq!(names[0].0, "a_b_c");
        assert!(names[1].0.starts_with("a_b_c_"), "{}", names[1].0);
    }
}
