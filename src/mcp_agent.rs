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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};

const TOOL_DEADLINE: Duration = Duration::from_secs(35);
const OUTPUT_LIMIT: usize = 512 * 1024;
const RESOURCE_POLL_INTERVAL: Duration = Duration::from_secs(2);

fn write_json_line(out: &Arc<Mutex<std::io::Stdout>>, message: &Value) -> bool {
    let Ok(mut guard) = out.lock() else {
        return false;
    };
    writeln!(guard, "{message}")
        .and_then(|()| guard.flush())
        .is_ok()
}

fn inbox_resource_uri(alias: &str) -> String {
    format!("cadence://inbox/{alias}")
}

fn validate_inbox_resource_uri(alias: &str, uri: &str) -> Result<()> {
    let expected = inbox_resource_uri(alias);
    if uri != expected {
        return Err(Error::rejected(format!(
            "resource URI '{uri}' is not accessible to this agent (only '{expected}' is visible)"
        )));
    }
    Ok(())
}

pub fn run(state_dir: &Path) -> Result<i32> {
    let alias = std::env::var("CADENCE_ALIAS")
        .map_err(|_| Error::rejected("Cadence agent MCP requires CADENCE_ALIAS"))?;
    // Discovery is best effort and happens only after `initialize` has been
    // answered: the managed client allows ten seconds to start the server,
    // and a stalled daemon must not cost it even the static tools.
    let mut discovered: Option<Vec<PlatformTool>> = None;
    let stdin = std::io::stdin();
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let subscribed = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut watcher: Option<std::thread::JoinHandle<()>> = None;

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
                "capabilities":{"tools":{},"resources":{"subscribe":true}},
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
                let result = call_tool(state_dir, &alias, name, &args, &*list);
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
            Some("resources/list") => json!({"jsonrpc":"2.0","id":id,"result":{
                "resources":[{
                    "uri": inbox_resource_uri(&alias),
                    "name": format!("Agent Inbox ({alias})"),
                    "description": "Durable Agent Inbox feed, unread watermark, and reachable workspace directory for this agent.",
                    "mimeType": "application/json"
                }]
            }}),
            Some("resources/read") => {
                let uri = request
                    .pointer("/params/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match validate_inbox_resource_uri(&alias, uri).and_then(|()| {
                    crate::client::rpc_deadline(
                        state_dir,
                        "agent_inbox_feed",
                        json!({"alias": alias, "limit": 50}),
                        TOOL_DEADLINE,
                    )
                }) {
                    Ok(feed) => json!({"jsonrpc":"2.0","id":id,"result":{
                        "contents":[{
                            "uri": inbox_resource_uri(&alias),
                            "mimeType": "application/json",
                            "text": feed.to_string()
                        }]
                    }}),
                    Err(error) => json!({"jsonrpc":"2.0","id":id,"error":{
                        "code": -32602,
                        "message": error.to_string()
                    }}),
                }
            }
            Some("resources/subscribe") => {
                let uri = request
                    .pointer("/params/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match validate_inbox_resource_uri(&alias, uri) {
                    Ok(()) => {
                        subscribed.store(true, Ordering::SeqCst);
                        if watcher.is_none() {
                            watcher = Some(spawn_inbox_watcher(
                                state_dir.to_path_buf(),
                                alias.clone(),
                                Arc::clone(&stdout),
                                Arc::clone(&subscribed),
                                Arc::clone(&shutdown),
                            ));
                        }
                        json!({"jsonrpc":"2.0","id":id,"result":{}})
                    }
                    Err(error) => json!({"jsonrpc":"2.0","id":id,"error":{
                        "code": -32602,
                        "message": error.to_string()
                    }}),
                }
            }
            Some("resources/unsubscribe") => {
                let uri = request
                    .pointer("/params/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match validate_inbox_resource_uri(&alias, uri) {
                    Ok(()) => {
                        subscribed.store(false, Ordering::SeqCst);
                        json!({"jsonrpc":"2.0","id":id,"result":{}})
                    }
                    Err(error) => json!({"jsonrpc":"2.0","id":id,"error":{
                        "code": -32602,
                        "message": error.to_string()
                    }}),
                }
            }
            _ => json!({"jsonrpc":"2.0","id":id,"error":{
                "code":-32601,"message":"method not found"
            }}),
        };
        if !write_json_line(&stdout, &response) {
            break;
        }
    }
    shutdown.store(true, Ordering::SeqCst);
    if let Some(handle) = watcher {
        let _ = handle.join();
    }
    Ok(0)
}

fn inbox_updated_notification(alias: &str, feed: &Value) -> Value {
    let uri = inbox_resource_uri(alias);
    let mut params = json!({
        "uri": uri,
        "seq": feed["tail_seq"].as_i64().unwrap_or(0),
        "unread": feed["unread"].as_i64().unwrap_or(0),
    });
    if let Some(latest) = feed["messages"].as_array().and_then(|rows| rows.first()) {
        if let Some(seq) = latest["seq"].as_i64() {
            params["seq"] = json!(seq);
        }
        if let Some(id) = latest["id"].as_str() {
            params["msgId"] = json!(id);
        }
        if let Some(kind) = latest["kind"].as_str() {
            params["kind"] = json!(kind);
        }
        if let Some(from) = latest["sender"].as_str() {
            params["from"] = json!(from);
        }
        if let Some(reply_to) = latest["reply_to"].as_str() {
            params["replyTo"] = json!(reply_to);
        }
        if latest["auto_answered"].as_bool() == Some(true) {
            params["autoAnswered"] = json!(true);
        }
    }
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/resources/updated",
        "params": params
    })
}

fn spawn_inbox_watcher(
    state_dir: PathBuf,
    alias: String,
    stdout: Arc<Mutex<std::io::Stdout>>,
    subscribed: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last_tail: Option<i64> = None;
        while !shutdown.load(Ordering::SeqCst) {
            if subscribed.load(Ordering::SeqCst) {
                if let Ok(feed) = crate::client::rpc_deadline(
                    &state_dir,
                    "agent_inbox_feed",
                    json!({"alias": alias, "limit": 1}),
                    Duration::from_secs(5),
                ) {
                    let tail = feed["tail_seq"].as_i64().unwrap_or(0);
                    if let Some(prev) = last_tail {
                        if tail > prev {
                            let notification = inbox_updated_notification(&alias, &feed);
                            if !write_json_line(&stdout, &notification) {
                                break;
                            }
                        }
                    }
                    last_tail = Some(tail);
                }
            }
            let mut waited = Duration::ZERO;
            while waited < RESOURCE_POLL_INTERVAL && !shutdown.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
                waited += Duration::from_millis(100);
            }
        }
    })
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
             "required":["artifact_id","message","token"],"additionalProperties":false}},
        {"name":"inbox_list","description":"Read your own Agent Inbox messages, unread count, read watermark, and reachable workspace directory. Scoped to your Cadence identity; caller identity cannot be overridden.",
         "inputSchema":{"type":"object","properties":{
             "after":{"type":"integer","minimum":0,"description":"Return messages strictly after this seq."},
             "limit":{"type":"integer","minimum":1,"maximum":100,"description":"Maximum messages to return (default 50)."},
             "unread_only":{"type":"boolean","description":"When true, return only messages above your read watermark."},
             "reader":{"type":"string","description":"Optional reader cursor name (default 'default')."}},
             "additionalProperties":false}},
        {"name":"inbox_ack","description":"Advance your own Agent Inbox read/acknowledged watermark through the given seq.",
         "inputSchema":{"type":"object","properties":{
             "through":{"type":"integer","minimum":0,"description":"Watermark seq to acknowledge through."},
             "reader":{"type":"string","description":"Optional reader cursor name (default 'default')."}},
             "required":["through"],"additionalProperties":false}},
        {"name":"message_send","description":"Send a durable Agent Inbox message (checkin, handoff, decision proposal, or reply) to a colleague's assistant or a specialist agent without creating a board ticket. Does not grant operator, steering, or approval authority; decision proposals require explicit human approval.",
         "inputSchema":{"type":"object","properties":{
             "to":{"type":"string","description":"Target assistant or specialist agent alias."},
             "text":{"type":"string","description":"Message body."},
             "kind":{"type":"string","enum":["checkin","handoff","decision","reply"],"description":"Lightweight message kind (default 'handoff' when artifact is set, otherwise 'checkin' or plain message)."},
             "reply_to":{"type":"string","description":"Optional alias to route the reply back to."},
             "artifact":{"type":"string","description":"Optional artifact or path reference attached to a handoff or decision proposal."}},
             "required":["to","text"],"additionalProperties":false}},
        {"name":"agent_directory","description":"List reachable colleagues' assistants and specialist agents in your workspace directory.",
         "inputSchema":{"type":"object","properties":{},"additionalProperties":false}}
    ]);
    if let Some(array) = tools.as_array_mut() {
        for tool in platform_tools {
            array.push(json!({
                "name": tool.name,
                "description": if tool.connected_read {
                    format!(
                        "{} (platform {}, effect {}). Runs through the Cadence platform gate; \
                         the caller is this connection. Requires `connection` (the provider's \
                         connection id) and `max_charge` (USD micro-units you accept for this \
                         read); every other property is the provider's own input.",
                        tool.description, tool.platform, tool.effect
                    )
                } else {
                    format!(
                        "{} (platform {}, effect {}). Runs through the Cadence platform gate; the caller is this connection.",
                        tool.description, tool.platform, tool.effect
                    )
                },
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
    /// CAD-1974: the call must state the connection it reads and the ceiling
    /// it accepts. They travel as control arguments, never inside the
    /// provider's own input object.
    connected_read: bool,
}

const STATIC_TOOL_NAMES: [&str; 9] = [
    "self",
    "wiki_search",
    "wiki_read",
    "issue_show",
    "app_run_artifact",
    "inbox_list",
    "inbox_ack",
    "message_send",
    "agent_directory",
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
    platform_tools_from(&listing)
}

/// The listing → advertised-tools projection, split out so the availability
/// rule is testable without a daemon.
fn platform_tools_from(listing: &Value) -> Vec<PlatformTool> {
    let mut rows: Vec<(&str, &str, bool, bool, &Value)> = Vec::new();
    for platform in listing["platforms"].as_array().into_iter().flatten() {
        let Some(platform_name) = platform["platform"].as_str() else {
            continue;
        };
        for tool in platform["tools"].as_array().into_iter().flatten() {
            let Some(tool_name) = tool["tool"].as_str() else {
                continue;
            };
            // CAD-1974: an unavailable tool is not advertised. This surface
            // cannot carry the enrollment step, so offering the name would
            // promise a call the gate refuses.
            if tool["available"].as_bool() == Some(false) {
                continue;
            }
            rows.push((
                platform_name,
                tool_name,
                tool["direct"].as_bool() == Some(true),
                tool["connected_read"].as_bool() == Some(true),
                tool,
            ));
        }
    }
    let mut out = Vec::new();
    for (name, platform, tool) in generated_tool_names(rows.iter().map(|row| (row.0, row.1, row.2)))
    {
        let Some(entry) = rows.iter().find(|row| row.0 == platform && row.1 == tool) else {
            continue;
        };
        let (_, connected_read, row) = (entry.2, entry.3, entry.4);
        out.push(PlatformTool {
            name,
            platform,
            tool,
            description: row["label"]
                .as_str()
                .unwrap_or("Cadence platform tool")
                .to_string(),
            effect: row["effect"].as_str().unwrap_or("unknown").to_string(),
            connected_read,
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
        let mut out = vec![
            "platform".into(),
            "call".into(),
            tool.tool.clone(),
            "--platform".into(),
            tool.platform.clone(),
        ];
        let mut body = args.clone();
        if tool.connected_read {
            // The two controls the gate requires, taken out of the provider's
            // own input: forwarding them inside it would be refused as unknown
            // request fields by the platform.
            let connection = body
                .get("connection")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::rejected("required string argument 'connection' is missing"))?
                .to_string();
            let max_charge = body
                .get("max_charge")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::usage("required integer argument 'max_charge' is missing"))?;
            if let Some(object) = body.as_object_mut() {
                object.remove("connection");
                object.remove("max_charge");
            }
            out.push("--connection".into());
            out.push(connection);
            out.push("--max-charge".into());
            out.push(max_charge.to_string());
        }
        out.push("--input".into());
        out.push(
            serde_json::to_string(&body)
                .map_err(|_| Error::internal("platform tool arguments do not serialize"))?,
        );
        return Ok(out);
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

fn call_tool(
    state_dir: &Path,
    alias: &str,
    name: &str,
    args: &Value,
    platform_tools: &[PlatformTool],
) -> Result<String> {
    match name {
        "inbox_list" => {
            let req = inbox_list_params(alias, args)?;
            let feed =
                crate::client::rpc_deadline(state_dir, "agent_inbox_feed", req, TOOL_DEADLINE)?;
            Ok(feed.to_string())
        }
        "inbox_ack" => {
            let req = inbox_ack_params(alias, args)?;
            let res =
                crate::client::rpc_deadline(state_dir, "agent_inbox_mark", req, TOOL_DEADLINE)?;
            Ok(res.to_string())
        }
        "message_send" => {
            let req = message_send_params(args)?;
            let res = crate::client::rpc_deadline(state_dir, "agent_send", req, TOOL_DEADLINE)?;
            Ok(res.to_string())
        }
        "agent_directory" => {
            fields(args, &[], &[])?;
            let feed = crate::client::rpc_deadline(
                state_dir,
                "agent_inbox_feed",
                json!({"alias": alias, "limit": 1}),
                TOOL_DEADLINE,
            )?;
            Ok(json!({
                "alias": alias,
                "directory": feed["directory"],
            })
            .to_string())
        }
        _ => command_args(name, args, platform_tools)
            .and_then(|argv| invoke(state_dir, alias, &argv)),
    }
}

fn inbox_list_params(alias: &str, args: &Value) -> Result<Value> {
    let object = args
        .as_object()
        .ok_or_else(|| Error::rejected("tool arguments must be an object"))?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "after" | "limit" | "unread_only" | "reader"))
    {
        return Err(Error::rejected(format!("unexpected tool argument '{key}'")));
    }
    let after = tool_u64(object, "after")?;
    let limit = tool_u64(object, "limit")?;
    if let Some(limit) = limit {
        if !(1..=100).contains(&limit) {
            return Err(Error::rejected("tool argument 'limit' must be 1..=100"));
        }
    }
    let unread_only = match object.get("unread_only") {
        None => false,
        Some(Value::Bool(v)) => *v,
        Some(_) => {
            return Err(Error::rejected(
                "tool argument 'unread_only' must be a boolean",
            ))
        }
    };
    let reader = match object.get("reader") {
        None => None,
        Some(Value::String(s)) if !s.is_empty() => Some(positional(s)?),
        Some(_) => {
            return Err(Error::rejected(
                "tool argument 'reader' must be a non-empty string",
            ))
        }
    };
    let mut req = json!({"alias": alias, "unread_only": unread_only});
    if let Some(after) = after {
        req["after"] = json!(after);
    }
    if let Some(limit) = limit {
        req["limit"] = json!(limit);
    }
    if let Some(reader) = reader {
        req["reader"] = json!(reader);
    }
    Ok(req)
}

fn inbox_ack_params(alias: &str, args: &Value) -> Result<Value> {
    let object = args
        .as_object()
        .ok_or_else(|| Error::rejected("tool arguments must be an object"))?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "through" | "reader"))
    {
        return Err(Error::rejected(format!("unexpected tool argument '{key}'")));
    }
    let through = tool_u64(object, "through")?
        .ok_or_else(|| Error::rejected("required integer argument 'through' is missing"))?;
    let reader = match object.get("reader") {
        None => None,
        Some(Value::String(s)) if !s.is_empty() => Some(positional(s)?),
        Some(_) => {
            return Err(Error::rejected(
                "tool argument 'reader' must be a non-empty string",
            ))
        }
    };
    let mut req = json!({"alias": alias, "through": through});
    if let Some(reader) = reader {
        req["reader"] = json!(reader);
    }
    Ok(req)
}

fn message_send_params(args: &Value) -> Result<Value> {
    let object = fields(
        args,
        &["to", "text", "kind", "reply_to", "artifact"],
        &["to", "text"],
    )?;
    let to = positional(object["to"].as_str().unwrap())?;
    let text = object["text"].as_str().unwrap();
    let kind = match object.get("kind").and_then(Value::as_str) {
        None => None,
        Some(raw) => match crate::inbox::InboxKind::parse(raw)? {
            crate::inbox::InboxKind::Checkin
            | crate::inbox::InboxKind::Handoff
            | crate::inbox::InboxKind::Decision
            | crate::inbox::InboxKind::Reply => Some(raw.trim().to_ascii_lowercase()),
            other => {
                return Err(Error::rejected(format!(
                    "MCP message_send kind '{}' is not permitted — use checkin, handoff, decision, or reply",
                    other.as_str()
                )))
            }
        },
    };
    let reply_to = match object.get("reply_to").and_then(Value::as_str) {
        None => None,
        Some("") => {
            return Err(Error::rejected(
                "tool argument 'reply_to' must be a non-empty string",
            ))
        }
        Some(s) => Some(positional(s)?),
    };
    let artifact = match object.get("artifact").and_then(Value::as_str) {
        None => None,
        Some(s) => Some(crate::inbox::validate_artifact(s)?),
    };
    let mut req = json!({"alias": to, "text": text});
    if let Some(kind) = kind {
        req["kind"] = json!(kind);
    }
    if let Some(reply_to) = reply_to {
        req["reply_to"] = json!(reply_to);
    }
    if let Some(artifact) = artifact {
        req["artifact"] = json!(artifact);
    }
    Ok(req)
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

    /// CAD-1974: the two control values leave the provider's input and become
    /// flags. Sending them inside `--input` would be refused by the platform as
    /// unknown request fields, and omitting them would be refused by the gate.
    #[test]
    fn a_connected_read_marshals_its_controls_as_flags() {
        let generated = [PlatformTool {
            name: "agenticos-provider_search_console_report".into(),
            platform: "agenticos-provider".into(),
            tool: "search_console_report".into(),
            description: "Report".into(),
            effect: "read".into(),
            connected_read: true,
        }];
        let argv = command_args(
            "agenticos-provider_search_console_report",
            &json!({"connection":"con_alpha","max_charge":25,"site_url":"sc-domain:x.test"}),
            &generated,
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "platform",
                "call",
                "search_console_report",
                "--platform",
                "agenticos-provider",
                "--connection",
                "con_alpha",
                "--max-charge",
                "25",
                "--input",
                r#"{"site_url":"sc-domain:x.test"}"#,
            ]
        );
        // Both controls are required: a call missing either never becomes argv.
        assert!(command_args(
            "agenticos-provider_search_console_report",
            &json!({"max_charge":25}),
            &generated
        )
        .is_err());
        assert!(command_args(
            "agenticos-provider_search_console_report",
            &json!({"connection":"con_alpha"}),
            &generated
        )
        .is_err());
        let error = command_args(
            "agenticos-provider_search_console_report",
            &json!({"connection":"con_alpha","max_charge":"25"}),
            &generated,
        )
        .unwrap_err();
        assert!(error.to_string().contains("max_charge"), "{error}");
    }

    /// CAD-1974: discovery never advertises a tool the gate would refuse for
    /// want of an enrollment this surface cannot perform.
    #[test]
    fn an_unavailable_platform_tool_is_not_advertised() {
        let listing = json!({"platforms": [
            {"platform": "agenticos-provider",
             "tools": [
                {"tool":"search_console_report","effect":"read","direct":true,
                 "connected_read":true,"available":true,"label":"Report"},
                {"tool":"analytics_report","effect":"read","direct":true,
                 "connected_read":true,"available":false,
                 "unavailable":"no credential is enrolled for agenticos-provider — run ..."}]},
            {"platform": "local",
             "tools": [{"tool":"publish","effect":"send","direct":true,"available":true}]}
        ]});
        let tools = platform_tools_from(&listing);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert!(
            names.contains(&"agenticos-provider_search_console_report"),
            "got {names:?}"
        );
        assert!(names.contains(&"local_publish"));
        assert!(
            !names.iter().any(|name| name.contains("analytics")),
            "an unavailable tool must not be offered: {names:?}"
        );
        assert!(tools
            .iter()
            .find(|tool| tool.tool == "search_console_report")
            .is_some_and(|tool| tool.connected_read));
    }

    #[test]
    fn a_generated_platform_tool_marshals_its_arguments() {
        let generated = [PlatformTool {
            name: "agenticos_post_draft".into(),
            platform: "agenticos".into(),
            tool: "post_draft".into(),
            description: "Draft".into(),
            effect: "draft".into(),
            connected_read: false,
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
        // The static tools stay reachable, and the listing grows by one.
        assert_eq!(
            tools(&[]).as_array().unwrap().len(),
            STATIC_TOOL_NAMES.len()
        );
        assert_eq!(
            tools(&generated).as_array().unwrap().len(),
            STATIC_TOOL_NAMES.len() + 1
        );
        // A dotted tool slug survives as an MCP-safe name.
        assert_eq!(mcp_name_part("email.send"), "email_send");
        assert_eq!(mcp_name_part("account-images"), "account-images");
    }

    #[test]
    fn inbox_tools_and_resources_enforce_caller_scope_and_no_steering() {
        // Forged identity and steering controls are refused on every inbox tool.
        assert!(inbox_list_params("assistant-a", &json!({"alias": "assistant-b"})).is_err());
        assert!(inbox_list_params("assistant-a", &json!({"as": "operator"})).is_err());
        assert!(inbox_list_params("assistant-a", &json!({"limit": 0})).is_err());
        assert!(inbox_ack_params("assistant-a", &json!({"through": 3, "alias": "other"})).is_err());
        assert!(inbox_ack_params("assistant-a", &json!({"through": 3, "reset": true})).is_err());
        assert!(inbox_ack_params("assistant-a", &json!({})).is_err());
        assert!(message_send_params(&json!({"to": "fin", "text": "hi", "nudge": true})).is_err());
        assert!(
            message_send_params(&json!({"to": "fin", "text": "hi", "priority": "urgent"})).is_err()
        );
        assert!(
            message_send_params(&json!({"to": "fin", "text": "hi", "kind": "message"})).is_err()
        );
        assert!(message_send_params(&json!({"to": "--flag", "text": "hi"})).is_err());

        // Specialist decision proposals are allowed as proposal-only envelopes (no write authority).
        let decision_req = message_send_params(&json!({
            "to": "master",
            "text": "Book supplier invoice #4821 ($2,400)?",
            "kind": "decision",
            "reply_to": "finance-agent"
        }))
        .unwrap();
        assert_eq!(decision_req["kind"], "decision");

        // MCP resource update notification includes seq, msgId, kind, from, replyTo.
        let notif = inbox_updated_notification(
            "assistant-a",
            &json!({
                "tail_seq": 9,
                "unread": 2,
                "messages": [{
                    "seq": 9,
                    "id": "msg-9",
                    "kind": "handoff",
                    "sender": "assistant-b",
                    "reply_to": "assistant-b",
                    "auto_answered": false
                }]
            }),
        );
        assert_eq!(notif["method"], "notifications/resources/updated");
        assert_eq!(notif["params"]["uri"], "cadence://inbox/assistant-a");
        assert_eq!(notif["params"]["seq"], 9);
        assert_eq!(notif["params"]["msgId"], "msg-9");
        assert_eq!(notif["params"]["kind"], "handoff");
        assert_eq!(notif["params"]["from"], "assistant-b");
        assert_eq!(notif["params"]["replyTo"], "assistant-b");

        // Valid calls produce exact daemon RPC payloads.
        let feed_req = inbox_list_params(
            "assistant-a",
            &json!({"after": 4, "limit": 20, "unread_only": true}),
        )
        .unwrap();
        assert_eq!(
            feed_req,
            json!({"alias": "assistant-a", "after": 4, "limit": 20, "unread_only": true})
        );
        let ack_req = inbox_ack_params("assistant-a", &json!({"through": 7})).unwrap();
        assert_eq!(ack_req, json!({"alias": "assistant-a", "through": 7}));
        let send_req = message_send_params(&json!({
            "to": "assistant-b",
            "text": "Q3 forecast attached",
            "kind": "handoff",
            "reply_to": "assistant-a",
            "artifact": "docs/q3-forecast.md"
        }))
        .unwrap();
        assert_eq!(
            send_req,
            json!({
                "alias": "assistant-b",
                "text": "Q3 forecast attached",
                "kind": "handoff",
                "reply_to": "assistant-a",
                "artifact": "docs/q3-forecast.md"
            })
        );

        // Resource URIs are strictly scoped to the agent's own alias.
        assert!(validate_inbox_resource_uri("assistant-a", "cadence://inbox/assistant-a").is_ok());
        assert!(validate_inbox_resource_uri("assistant-a", "cadence://inbox/assistant-b").is_err());
        assert!(validate_inbox_resource_uri("assistant-a", "file:///etc/passwd").is_err());
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
