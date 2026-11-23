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
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::private_app_update::{
    valid_generation, valid_install_id, valid_package_ref, valid_sha256_prefixed,
};

const TOOL_DEADLINE: Duration = Duration::from_secs(35);
const PRIVATE_APP_WAKE_TIMEOUT_SECS: u64 = 60;
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
                let result = if is_private_app_tool(name) {
                    parse_private_app_call(name, &args)
                        .and_then(|call| execute_private_app_call(state_dir, &alias, call))
                } else if is_hosted_app_tool(name) {
                    parse_hosted_app_call(name, &args)
                        .and_then(|call| execute_hosted_app_call(state_dir, &alias, call))
                } else {
                    let list = discovered.get_or_insert_with(|| platform_tools(state_dir));
                    command_args(name, &args, &*list)
                        .and_then(|argv| invoke(state_dir, &alias, &argv))
                };
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
             "required":["artifact_id","message","token"],"additionalProperties":false}},
        {"name":"private_app_prepare","description":"Stage a private app ZIP package (using kickoff package_ref or a tagged public GitHub release .zip URL) and request owner review or routine-policy preparation without installing it directly. Re-call with the same request_id if the outcome is uncertain.",
         "inputSchema":{"type":"object","properties":{
             "package_ref":{"type":"string","description":"Turn-assigned private app package reference from your kickoff."},
             "url":{"type":"string","description":"Public tagged GitHub release .zip URL (https://github.com/...)."},
             "install_id":{"type":"string","description":"Target installed private app id (install_...) when updating."},
             "expected_digest":{"type":"string","description":"Expected current installed sha256:... digest when updating (requires install_id)."},
             "expected_generation":{"type":"string","description":"Expected current catalog generation when updating (requires install_id)."},
             "request_id":{"type":"string","description":"Optional idempotency key; deterministically derived when omitted."}},
             "additionalProperties":false}},
        {"name":"private_app_status","description":"Read the current state of your private app review request.",
         "inputSchema":{"type":"object","properties":{
             "id":{"type":"string","description":"Private app review ID (par_...)."}},
             "required":["id"],"additionalProperties":false}},
        {"name":"private_app_apply","description":"Apply a private app review under exact owner authorization or an active patch update policy. On an uncertain outcome, call private_app_reconcile with id before retrying.",
         "inputSchema":{"type":"object","properties":{
             "id":{"type":"string","description":"Private app review ID (par_...)."},
             "digest":{"type":"string","description":"Reviewed app package sha256:... digest."},
             "zip_sha256":{"type":"string","description":"Reviewed ZIP sha256:... digest."}},
             "required":["id","digest","zip_sha256"],"additionalProperties":false}},
        {"name":"private_app_reconcile","description":"Read the authoritative outcome of a private app review or apply without replaying it.",
         "inputSchema":{"type":"object","properties":{
             "id":{"type":"string","description":"Private app review ID (par_...)."}},
             "required":["id"],"additionalProperties":false}},
        {"name":"app_describe","description":"Describe an installed workspace app's callable operations and skills in the hosted workspace.",
         "inputSchema":{"type":"object","properties":{
             "app":{"type":"string","description":"Installed app slug (for example 'waba')."}},
             "required":["app"],"additionalProperties":false}},
        {"name":"app_skill","description":"Read one named skill guide packaged in an installed workspace app.",
         "inputSchema":{"type":"object","properties":{
             "app":{"type":"string","description":"Installed app slug (for example 'waba')."},
             "skill":{"type":"string","description":"Skill name listed by app_describe."}},
             "required":["app","skill"],"additionalProperties":false}},
        {"name":"app_call","description":"Invoke a callable operation on an installed hosted app (such as 'waba'). Read and draft verbs run directly when permitted; external sends and WhatsApp lifecycle mutations stage into the owner's Home queue (check board_pending_actions, and inspect status rather than retrying uncertain outcomes).",
         "inputSchema":{"type":"object","properties":{
             "app":{"type":"string","description":"Installed app slug (for example 'waba')."},
             "operation":{"type":"string","description":"Callable operation name from app_describe."},
             "args":{"type":"object","description":"Operation argument object (passed serialized to the app operation)."},
             "reason":{"type":"string","description":"Optional short owner-facing reason when staging a write or send."},
             "request_id":{"type":"string","description":"Optional idempotency key; deterministically derived when omitted."}},
             "required":["app","operation"],"additionalProperties":false}},
        {"name":"board_pending_actions","description":"List or inspect staged actions awaiting or decided by the owner in Home (read-only).",
         "inputSchema":{"type":"object","properties":{
             "id":{"type":"string","description":"Optional staged action id."},
             "status":{"type":"string","enum":["pending","approved","denied"],"description":"Optional status filter."}},
             "additionalProperties":false}}
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

const STATIC_TOOL_NAMES: [&str; 13] = [
    "self",
    "wiki_search",
    "wiki_read",
    "issue_show",
    "app_run_artifact",
    "private_app_prepare",
    "private_app_status",
    "private_app_apply",
    "private_app_reconcile",
    "app_describe",
    "app_skill",
    "app_call",
    "board_pending_actions",
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum PrivateAppCall {
    Prepare {
        package_ref: Option<String>,
        url: Option<String>,
        install_id: Option<String>,
        expected_digest: Option<String>,
        expected_generation: Option<String>,
        request_id: Option<String>,
    },
    Status {
        id: String,
    },
    Apply {
        id: String,
        digest: String,
        zip_sha256: String,
    },
    Reconcile {
        id: String,
    },
}

fn is_private_app_tool(name: &str) -> bool {
    matches!(
        name,
        "private_app_prepare"
            | "private_app_status"
            | "private_app_apply"
            | "private_app_reconcile"
    )
}

fn valid_github_zip_url(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 800
        && value.starts_with("https://github.com/")
        && value.ends_with(".zip")
        && !value.bytes().any(|b| b <= 0x20 || b == 0x7f)
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

fn valid_review_id(value: &str) -> bool {
    value.strip_prefix("par_").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn valid_artifact_id(value: &str) -> bool {
    value.strip_prefix("art-").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn optional_nonempty_str(object: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(Error::rejected(format!(
            "tool argument '{key}' must be a non-empty string"
        ))),
    }
}

fn required_nonempty_str<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    match object.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.as_str()),
        _ => Err(Error::rejected(format!(
            "required tool argument '{key}' must be a non-empty string"
        ))),
    }
}

fn parse_private_app_call(name: &str, args: &Value) -> Result<PrivateAppCall> {
    match name {
        "private_app_prepare" => {
            let object = fields(
                args,
                &[
                    "package_ref",
                    "url",
                    "install_id",
                    "expected_digest",
                    "expected_generation",
                    "request_id",
                ],
                &[],
            )?;
            let package_ref = optional_nonempty_str(object, "package_ref")?;
            let url = optional_nonempty_str(object, "url")?;
            if package_ref.is_some() == url.is_some() {
                return Err(Error::rejected(
                    "private_app_prepare requires exactly one of 'package_ref' or 'url'",
                ));
            }
            if let Some(ref value) = package_ref {
                if !valid_package_ref(value) {
                    return Err(Error::rejected("tool argument 'package_ref' is invalid"));
                }
            }
            if let Some(ref value) = url {
                if !valid_github_zip_url(value) {
                    return Err(Error::rejected(
                        "tool argument 'url' must be a public https://github.com/.../*.zip URL",
                    ));
                }
            }
            let install_id = optional_nonempty_str(object, "install_id")?;
            if let Some(ref value) = install_id {
                if !valid_install_id(value) {
                    return Err(Error::rejected("tool argument 'install_id' is invalid"));
                }
            }
            let expected_digest = optional_nonempty_str(object, "expected_digest")?;
            if let Some(ref value) = expected_digest {
                if !valid_sha256_prefixed(value) {
                    return Err(Error::rejected(
                        "tool argument 'expected_digest' must be sha256:<64 hex>",
                    ));
                }
            }
            let expected_generation = optional_nonempty_str(object, "expected_generation")?;
            if let Some(ref value) = expected_generation {
                if !valid_generation(value) {
                    return Err(Error::rejected(
                        "tool argument 'expected_generation' is invalid",
                    ));
                }
            }
            if (expected_digest.is_some() || expected_generation.is_some()) && install_id.is_none()
            {
                return Err(Error::rejected(
                    "expected_digest and expected_generation require install_id",
                ));
            }
            let request_id = optional_nonempty_str(object, "request_id")?;
            if let Some(ref value) = request_id {
                if !valid_request_id(value) {
                    return Err(Error::rejected("tool argument 'request_id' is invalid"));
                }
            }
            Ok(PrivateAppCall::Prepare {
                package_ref,
                url,
                install_id,
                expected_digest,
                expected_generation,
                request_id,
            })
        }
        "private_app_status" | "private_app_reconcile" => {
            let object = fields(args, &["id"], &["id"])?;
            let id = required_nonempty_str(object, "id")?;
            if !valid_review_id(id) {
                return Err(Error::rejected("tool argument 'id' must be par_<32 hex>"));
            }
            if name == "private_app_status" {
                Ok(PrivateAppCall::Status { id: id.into() })
            } else {
                Ok(PrivateAppCall::Reconcile { id: id.into() })
            }
        }
        "private_app_apply" => {
            let object = fields(
                args,
                &["id", "digest", "zip_sha256"],
                &["id", "digest", "zip_sha256"],
            )?;
            let id = required_nonempty_str(object, "id")?;
            let digest = required_nonempty_str(object, "digest")?;
            let zip_sha256 = required_nonempty_str(object, "zip_sha256")?;
            if !valid_review_id(id) {
                return Err(Error::rejected("tool argument 'id' must be par_<32 hex>"));
            }
            if !valid_sha256_prefixed(digest) {
                return Err(Error::rejected(
                    "tool argument 'digest' must be sha256:<64 hex>",
                ));
            }
            if !valid_sha256_prefixed(zip_sha256) {
                return Err(Error::rejected(
                    "tool argument 'zip_sha256' must be sha256:<64 hex>",
                ));
            }
            Ok(PrivateAppCall::Apply {
                id: id.into(),
                digest: digest.into(),
                zip_sha256: zip_sha256.into(),
            })
        }
        _ => Err(Error::rejected("unknown Cadence agent tool")),
    }
}

fn deterministic_prepare_request_id(
    source_kind: &str,
    message_id: &str,
    source_pin: &str,
    install_id: Option<&str>,
    expected_digest: Option<&str>,
    expected_generation: Option<&str>,
) -> String {
    use std::fmt::Write as _;
    let mut hasher = Sha256::new();
    for part in [
        "cadence-private-app-prepare.v1",
        source_kind,
        message_id,
        source_pin,
        install_id.unwrap_or(""),
        expected_digest.unwrap_or(""),
        expected_generation.unwrap_or(""),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(5 + 32);
    out.push_str("parq_");
    for byte in &digest[..16] {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn merge_custody_pin(
    arg: Option<String>,
    custody: Option<&str>,
    label: &str,
) -> Result<Option<String>> {
    match (arg, custody) {
        (Some(arg), Some(pinned)) if arg != pinned => Err(Error::rejected(format!(
            "tool argument '{label}' does not match the turn-assigned private app target"
        ))),
        (Some(arg), _) => Ok(Some(arg)),
        (None, Some(pinned)) => Ok(Some(pinned.to_string())),
        (None, None) => Ok(None),
    }
}

fn safe_error_token(value: Option<&str>) -> Option<&str> {
    value.filter(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    })
}

fn private_app_conflict_error(op: &str, body: &Value) -> Error {
    let code = safe_error_token(
        body.pointer("/error/code")
            .or_else(|| body.get("code"))
            .and_then(Value::as_str),
    );
    let request_id = safe_error_token(body.pointer("/error/request_id").and_then(Value::as_str));
    let msg = match (code, request_id) {
        (Some(code), Some(req)) => format!("{op} conflicted ({code}, request {req})"),
        (Some(code), None) => format!("{op} conflicted ({code})"),
        (None, Some(req)) => format!("{op} conflicted (HTTP 409, request {req})"),
        (None, None) => format!("{op} conflicted (HTTP 409)"),
    };
    Error::rejected(msg)
}

fn resolve_running_message_id(state_dir: &Path, alias: &str) -> Result<String> {
    let identity = crate::client::rpc(state_dir, "agent_identity", json!({}))?;
    if identity["alias"].as_str() != Some(alias) {
        return Err(Error::rejected(
            "Cadence MCP identity differs from endpoint",
        ));
    }
    let running = identity["running"]
        .as_array()
        .ok_or_else(|| Error::rejected("Cadence MCP identity did not report running turns"))?;
    if running.len() != 1 {
        return Err(Error::rejected(
            "private app operations require exactly one active held turn on this endpoint",
        ));
    }
    let message_id = running[0]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::rejected("running turn identity is missing message id"))?;
    Ok(message_id.to_string())
}

fn execute_private_app_call(state_dir: &Path, alias: &str, call: PrivateAppCall) -> Result<String> {
    let (auth_dir, message_id, target, slot) = resolve_hosted_turn_context(state_dir, alias)?;
    execute_private_app_call_with(
        &auth_dir,
        state_dir,
        &message_id,
        &target,
        &slot,
        call,
        PRIVATE_APP_WAKE_TIMEOUT_SECS,
    )
}

fn resolve_hosted_turn_context(
    state_dir: &Path,
    alias: &str,
) -> Result<(
    std::path::PathBuf,
    String,
    crate::remote_cli::RemoteTarget,
    crate::remote_cli::AgentSlot,
)> {
    let message_id = resolve_running_message_id(state_dir, alias)?;
    let binding =
        crate::local_assignment::reporting::hosted_binding_target(state_dir, &message_id)?
            .ok_or_else(|| Error::rejected("current turn has no active hosted kickoff binding"))?;
    let cred_path =
        crate::local_assignment::reporting::agent_credential_path(state_dir, &message_id)?;
    let target = crate::remote_cli::RemoteTarget {
        org: binding.org,
        endpoint: binding.endpoint,
        org_id: binding.organization_id,
        identity: Some(crate::remote_cli::CliIdentityPin {
            subject_id: binding.subject_id,
            issuer: binding.issuer,
        }),
    };
    let slot = crate::remote_cli::AgentSlot::new(
        cred_path,
        &target,
        binding.assignment_id,
        binding.claim_id,
        binding.generation,
    );
    let auth_dir = crate::remote_auth::auth_dir(None).map_err(|_| {
        Error::rejected("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    Ok((auth_dir, message_id, target, slot))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HostedAppToolCall {
    Describe {
        app: String,
    },
    Skill {
        app: String,
        skill: String,
    },
    Call {
        app: String,
        operation: String,
        args_json: String,
        reason: Option<String>,
        request_id: Option<String>,
    },
    PendingActions {
        id: Option<String>,
        status: Option<String>,
    },
}

fn is_hosted_app_tool(name: &str) -> bool {
    matches!(
        name,
        "app_describe" | "app_skill" | "app_call" | "board_pending_actions"
    )
}

fn valid_hosted_app_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(idx, b)| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || (idx > 0 && matches!(b, b'-' | b'_'))
        })
}

fn valid_hosted_app_operation(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(idx, b)| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || (idx > 0 && matches!(b, b'.' | b'-' | b'_'))
        })
}

fn valid_hosted_skill_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.bytes().any(|b| b < 0x20 || b == 0x7f)
}

fn valid_hosted_action_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn deterministic_app_call_request_id(
    message_id: &str,
    app: &str,
    operation: &str,
    args_json: &str,
    reason: Option<&str>,
) -> String {
    use std::fmt::Write as _;
    let mut hasher = Sha256::new();
    for part in [
        "cadence-mcp-app-call.v1",
        message_id,
        app,
        operation,
        args_json,
        reason.unwrap_or(""),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + 32);
    out.push_str("mcpapp_");
    for byte in &digest[..16] {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn parse_hosted_app_call(name: &str, args: &Value) -> Result<HostedAppToolCall> {
    match name {
        "app_describe" => {
            let object = fields(args, &["app"], &["app"])?;
            let app = required_nonempty_str(object, "app")?;
            if !valid_hosted_app_slug(app) {
                return Err(Error::rejected("tool argument 'app' is invalid"));
            }
            Ok(HostedAppToolCall::Describe { app: app.into() })
        }
        "app_skill" => {
            let object = fields(args, &["app", "skill"], &["app", "skill"])?;
            let app = required_nonempty_str(object, "app")?;
            let skill = required_nonempty_str(object, "skill")?;
            if !valid_hosted_app_slug(app) {
                return Err(Error::rejected("tool argument 'app' is invalid"));
            }
            if !valid_hosted_skill_name(skill) {
                return Err(Error::rejected("tool argument 'skill' is invalid"));
            }
            Ok(HostedAppToolCall::Skill {
                app: app.into(),
                skill: skill.into(),
            })
        }
        "app_call" => {
            let object = args
                .as_object()
                .ok_or_else(|| Error::rejected("tool arguments must be an object"))?;
            if let Some(key) = object.keys().find(|key| {
                !matches!(
                    key.as_str(),
                    "app" | "operation" | "args" | "reason" | "request_id"
                )
            }) {
                return Err(Error::rejected(format!("unexpected tool argument '{key}'")));
            }
            let app = required_nonempty_str(object, "app")?;
            let operation = required_nonempty_str(object, "operation")?;
            if !valid_hosted_app_slug(app) {
                return Err(Error::rejected("tool argument 'app' is invalid"));
            }
            if !valid_hosted_app_operation(operation) {
                return Err(Error::rejected("tool argument 'operation' is invalid"));
            }
            let op_args = match object.get("args") {
                None => json!({}),
                Some(Value::Object(map)) => Value::Object(map.clone()),
                Some(_) => return Err(Error::rejected("tool argument 'args' must be an object")),
            };
            let args_json = serde_json::to_string(&op_args)
                .map_err(|_| Error::rejected("tool argument 'args' could not be serialized"))?;
            let reason = optional_nonempty_str(object, "reason")?;
            if let Some(ref value) = reason {
                if value.len() > 1000
                    || value
                        .bytes()
                        .any(|b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r')) || b == 0x7f)
                {
                    return Err(Error::rejected("tool argument 'reason' is invalid"));
                }
            }
            let request_id = optional_nonempty_str(object, "request_id")?;
            if let Some(ref value) = request_id {
                if !valid_request_id(value) {
                    return Err(Error::rejected("tool argument 'request_id' is invalid"));
                }
            }
            Ok(HostedAppToolCall::Call {
                app: app.into(),
                operation: operation.into(),
                args_json,
                reason,
                request_id,
            })
        }
        "board_pending_actions" => {
            let object = fields(args, &["id", "status"], &[])?;
            let id = optional_nonempty_str(object, "id")?;
            if let Some(ref value) = id {
                if !valid_hosted_action_id(value) {
                    return Err(Error::rejected("tool argument 'id' is invalid"));
                }
            }
            let status = optional_nonempty_str(object, "status")?;
            if let Some(ref value) = status {
                if !matches!(value.as_str(), "pending" | "approved" | "denied") {
                    return Err(Error::rejected(
                        "tool argument 'status' must be pending, approved, or denied",
                    ));
                }
            }
            Ok(HostedAppToolCall::PendingActions { id, status })
        }
        _ => Err(Error::rejected("unknown Cadence agent tool")),
    }
}

fn execute_hosted_app_call(
    state_dir: &Path,
    alias: &str,
    call: HostedAppToolCall,
) -> Result<String> {
    let (auth_dir, message_id, target, slot) = resolve_hosted_turn_context(state_dir, alias)?;
    execute_hosted_app_call_with(
        &auth_dir,
        &message_id,
        &target,
        &slot,
        call,
        PRIVATE_APP_WAKE_TIMEOUT_SECS,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn invoke_hosted_app_tool_with(
    auth_dir: &Path,
    message_id: &str,
    target: &crate::remote_cli::RemoteTarget,
    slot: &crate::remote_cli::AgentSlot,
    name: &str,
    args: &Value,
    wake_timeout: u64,
) -> Result<String> {
    let call = parse_hosted_app_call(name, args)?;
    execute_hosted_app_call_with(auth_dir, message_id, target, slot, call, wake_timeout)
}

fn execute_hosted_app_call_with(
    auth_dir: &Path,
    message_id: &str,
    target: &crate::remote_cli::RemoteTarget,
    slot: &crate::remote_cli::AgentSlot,
    call: HostedAppToolCall,
    wake_timeout: u64,
) -> Result<String> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid hosted app wake timeout"))?;
    let mut arguments = Map::new();
    arguments.insert("operation".into(), json!("call"));
    let (tool_label, req_key) = match call {
        HostedAppToolCall::Describe { app } => {
            arguments.insert("tool".into(), json!("app_describe"));
            arguments.insert("body".into(), json!({ "app": app }));
            ("app_describe", None)
        }
        HostedAppToolCall::Skill { app, skill } => {
            arguments.insert("tool".into(), json!("app_skill"));
            arguments.insert("body".into(), json!({ "app": app, "skill": skill }));
            ("app_skill", None)
        }
        HostedAppToolCall::Call {
            app,
            operation,
            args_json,
            reason,
            request_id,
        } => {
            let key = request_id.unwrap_or_else(|| {
                deterministic_app_call_request_id(
                    message_id,
                    &app,
                    &operation,
                    &args_json,
                    reason.as_deref(),
                )
            });
            let mut body = Map::new();
            body.insert("app".into(), json!(app));
            body.insert("operation".into(), json!(operation));
            body.insert("args".into(), json!(args_json));
            if let Some(reason) = reason {
                body.insert("reason".into(), json!(reason));
            }
            arguments.insert("tool".into(), json!("app_call"));
            arguments.insert("key".into(), json!(key));
            arguments.insert("body".into(), Value::Object(body));
            ("app_call", Some(key))
        }
        HostedAppToolCall::PendingActions { id, status } => {
            let mut body = Map::new();
            if let Some(id) = id {
                body.insert("id".into(), json!(id));
            }
            if let Some(status) = status {
                body.insert("status".into(), json!(status));
            }
            arguments.insert("tool".into(), json!("board_pending_actions"));
            arguments.insert("body".into(), Value::Object(body));
            ("board_pending_actions", None)
        }
    };
    match crate::remote_cli::call_verb_for_slot(
        auth_dir,
        target,
        "platform_call",
        arguments,
        deadline,
        wake_timeout,
        slot,
        |_| {},
    ) {
        Ok(crate::remote_cli::RemoteAnswer::Ok(val)) => Ok(val.to_string()),
        Ok(crate::remote_cli::RemoteAnswer::Conflict(body)) => {
            Err(private_app_conflict_error(tool_label, &body))
        }
        Err(Error::OutcomeUnknown(_)) if tool_label == "app_call" => {
            let key = req_key.unwrap_or_default();
            Err(Error::unknown(format!(
                "app_call outcome is uncertain for request_id '{key}'; inspect board_pending_actions or a read-only status verb (for example whatsapp.status, whatsapp.reply.status, or whatsapp.template.status) before retrying"
            )))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn invoke_private_app_tool_with(
    auth_dir: &Path,
    state_dir: &Path,
    message_id: &str,
    target: &crate::remote_cli::RemoteTarget,
    slot: &crate::remote_cli::AgentSlot,
    name: &str,
    args: &Value,
    wake_timeout: u64,
) -> Result<String> {
    let call = parse_private_app_call(name, args)?;
    execute_private_app_call_with(
        auth_dir,
        state_dir,
        message_id,
        target,
        slot,
        call,
        wake_timeout,
    )
}

fn execute_private_app_call_with(
    auth_dir: &Path,
    state_dir: &Path,
    message_id: &str,
    target: &crate::remote_cli::RemoteTarget,
    slot: &crate::remote_cli::AgentSlot,
    call: PrivateAppCall,
    wake_timeout: u64,
) -> Result<String> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid private app wake timeout"))?;
    match call {
        PrivateAppCall::Prepare {
            package_ref,
            url,
            mut install_id,
            mut expected_digest,
            mut expected_generation,
            request_id,
        } => {
            let mut body = Map::new();
            let req_id = if let Some(pkg_ref) = package_ref {
                let (custody, bytes) =
                    crate::local_assignment::reporting::read_private_app_package(
                        state_dir, message_id, &pkg_ref,
                    )?;
                install_id =
                    merge_custody_pin(install_id, custody.install_id.as_deref(), "install_id")?;
                expected_digest = merge_custody_pin(
                    expected_digest,
                    custody.expected_digest.as_deref(),
                    "expected_digest",
                )?;
                expected_generation = merge_custody_pin(
                    expected_generation,
                    custody.expected_generation.as_deref(),
                    "expected_generation",
                )?;
                if (expected_digest.is_some() || expected_generation.is_some())
                    && install_id.is_none()
                {
                    return Err(Error::rejected(
                        "expected_digest and expected_generation require install_id",
                    ));
                }
                let upload = crate::remote_cli::ArtifactUpload {
                    kind: "private-app-zip",
                    title: &custody.file_name,
                    id: None,
                    issue: None,
                    label: None,
                    customer: None,
                    file_name: &custody.file_name,
                    bytes: &bytes,
                };
                let uploaded = crate::remote_cli::upload_artifact_for_slot(
                    auth_dir,
                    target,
                    &upload,
                    wake_timeout,
                    slot,
                    |_| {},
                )?;
                let artifact_id = uploaded
                    .pointer("/file/id")
                    .and_then(Value::as_str)
                    .filter(|id| valid_artifact_id(id))
                    .ok_or_else(|| {
                        Error::unknown(
                            "staged private app artifact receipt was malformed; inspect artifacts before retrying",
                        )
                    })?;
                let uploaded_hex = uploaded
                    .pointer("/file/digest")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Error::unknown(
                            "staged private app artifact receipt omitted digest; inspect artifacts before retrying",
                        )
                    })?;
                if format!("sha256:{uploaded_hex}") != custody.zip_sha256 {
                    return Err(Error::unknown(
                        "uploaded private app artifact digest did not match staged ZIP; inspect artifacts before retrying",
                    ));
                }
                let req_id = request_id.unwrap_or_else(|| {
                    deterministic_prepare_request_id(
                        "artifact",
                        message_id,
                        &custody.zip_sha256,
                        install_id.as_deref(),
                        expected_digest.as_deref(),
                        expected_generation.as_deref(),
                    )
                });
                body.insert("request_id".into(), json!(req_id));
                body.insert("artifact_id".into(), json!(artifact_id));
                body.insert("zip_sha256".into(), json!(custody.zip_sha256));
                req_id
            } else {
                let url = url.ok_or_else(|| {
                    Error::rejected("private_app_prepare requires package_ref or url")
                })?;
                if (expected_digest.is_some() || expected_generation.is_some())
                    && install_id.is_none()
                {
                    return Err(Error::rejected(
                        "expected_digest and expected_generation require install_id",
                    ));
                }
                let req_id = request_id.unwrap_or_else(|| {
                    deterministic_prepare_request_id(
                        "github",
                        message_id,
                        &url,
                        install_id.as_deref(),
                        expected_digest.as_deref(),
                        expected_generation.as_deref(),
                    )
                });
                body.insert("request_id".into(), json!(req_id));
                body.insert("url".into(), json!(url));
                req_id
            };
            if let Some(install_id) = install_id {
                body.insert("install_id".into(), json!(install_id));
            }
            if let Some(expected_digest) = expected_digest {
                body.insert("expected_digest".into(), json!(expected_digest));
            }
            if let Some(expected_generation) = expected_generation {
                body.insert("expected_generation".into(), json!(expected_generation));
            }
            let mut arguments = Map::new();
            arguments.insert("operation".into(), json!("call"));
            arguments.insert("tool".into(), json!("private_app_review_request"));
            arguments.insert("key".into(), json!(req_id));
            arguments.insert("body".into(), Value::Object(body));
            match crate::remote_cli::call_verb_for_slot(
                auth_dir,
                target,
                "platform_call",
                arguments,
                deadline,
                wake_timeout,
                slot,
                |_| {},
            ) {
                Ok(crate::remote_cli::RemoteAnswer::Ok(val)) => Ok(val.to_string()),
                Ok(crate::remote_cli::RemoteAnswer::Conflict(body)) => {
                    Err(private_app_conflict_error("private_app_prepare", &body))
                }
                Err(Error::OutcomeUnknown(_)) => Err(Error::unknown(format!(
                    "private_app_prepare outcome is uncertain; call private_app_prepare again with exact request_id '{req_id}' before creating a new request"
                ))),
                Err(error) => Err(error),
            }
        }
        PrivateAppCall::Status { id } => {
            let mut arguments = Map::new();
            arguments.insert("operation".into(), json!("call"));
            arguments.insert("tool".into(), json!("private_app_review_status"));
            arguments.insert("key".into(), json!(format!("status-{id}")));
            arguments.insert("body".into(), json!({ "id": id }));
            match crate::remote_cli::call_verb_for_slot(
                auth_dir,
                target,
                "platform_call",
                arguments,
                deadline,
                wake_timeout,
                slot,
                |_| {},
            )? {
                crate::remote_cli::RemoteAnswer::Ok(val) => Ok(val.to_string()),
                crate::remote_cli::RemoteAnswer::Conflict(body) => {
                    Err(private_app_conflict_error("private_app_status", &body))
                }
            }
        }
        PrivateAppCall::Reconcile { id } => {
            let mut arguments = Map::new();
            arguments.insert("operation".into(), json!("call"));
            arguments.insert("tool".into(), json!("private_app_review_reconcile"));
            arguments.insert("key".into(), json!(format!("reconcile-{id}")));
            arguments.insert("body".into(), json!({ "id": id }));
            match crate::remote_cli::call_verb_for_slot(
                auth_dir,
                target,
                "platform_call",
                arguments,
                deadline,
                wake_timeout,
                slot,
                |_| {},
            )? {
                crate::remote_cli::RemoteAnswer::Ok(val) => Ok(val.to_string()),
                crate::remote_cli::RemoteAnswer::Conflict(body) => {
                    Err(private_app_conflict_error("private_app_reconcile", &body))
                }
            }
        }
        PrivateAppCall::Apply {
            id,
            digest,
            zip_sha256,
        } => {
            let mut arguments = Map::new();
            arguments.insert("operation".into(), json!("call"));
            arguments.insert("tool".into(), json!("private_app_review_apply"));
            arguments.insert("key".into(), json!(format!("apply-{id}")));
            arguments.insert(
                "body".into(),
                json!({
                    "id": id,
                    "digest": digest,
                    "zip_sha256": zip_sha256,
                }),
            );
            match crate::remote_cli::call_verb_for_slot(
                auth_dir,
                target,
                "platform_call",
                arguments,
                deadline,
                wake_timeout,
                slot,
                |_| {},
            ) {
                Ok(crate::remote_cli::RemoteAnswer::Ok(val)) => Ok(val.to_string()),
                Ok(crate::remote_cli::RemoteAnswer::Conflict(body)) => {
                    Err(private_app_conflict_error("private_app_apply", &body))
                }
                Err(Error::OutcomeUnknown(_)) => Err(Error::unknown(format!(
                    "private_app_apply outcome is uncertain; call private_app_reconcile with id '{id}' before retrying"
                ))),
                Err(error) => Err(error),
            }
        }
    }
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

    #[test]
    fn private_app_tools_validate_arguments_and_exclude_owner_controls() {
        let valid_sha = format!("sha256:{}", "a".repeat(64));
        let valid_par = format!("par_{}", "b".repeat(32));
        for args in [
            json!({}),
            json!({"package_ref": "pkg_1", "url": "https://github.com/org/repo/releases/download/v1/app.zip"}),
            json!({"package_ref": "../escape"}),
            json!({"url": "http://github.com/org/repo/releases/download/v1/app.zip"}),
            json!({"url": "https://evil.example/org/repo/app.zip"}),
            json!({"url": "https://github.com/org/repo/archive/v1.tar.gz"}),
            json!({"package_ref": "pkg_1", "expected_digest": valid_sha}),
            json!({"package_ref": "pkg_1", "expected_generation": "gen_1"}),
            json!({"package_ref": "pkg_1", "install_id": "bad_id"}),
        ] {
            assert!(parse_private_app_call("private_app_prepare", &args).is_err());
            assert!(command_args("private_app_prepare", &args, &[]).is_err());
        }
        for key in ["path", "file", "org", "alias", "provider"] {
            let args = json!({"package_ref": "pkg_1", key: "val"});
            assert!(parse_private_app_call("private_app_prepare", &args).is_err());
            assert!(command_args("private_app_prepare", &args, &[]).is_err());
        }
        for (name, args) in [
            ("private_app_review_authorize", json!({"id": valid_par})),
            (
                "private_app_update_policy",
                json!({"operation": "read", "install_id": "install_demo"}),
            ),
            ("private_app_status", json!({"id": "par_short"})),
            ("private_app_status", json!({"id": 123})),
            ("private_app_reconcile", json!({"id": "par_short"})),
            ("private_app_reconcile", json!({"id": true})),
            (
                "private_app_apply",
                json!({"id": valid_par, "digest": valid_sha}),
            ),
            (
                "private_app_apply",
                json!({"id": 123, "digest": 456, "zip_sha256": 789}),
            ),
            (
                "private_app_apply",
                json!({"id": valid_par, "digest": "bad", "zip_sha256": valid_sha}),
            ),
        ] {
            assert!(
                parse_private_app_call(name, &args).is_err(),
                "expected refusal for {name}: {args}"
            );
            assert!(command_args(name, &args, &[]).is_err());
        }

        let r1 = deterministic_prepare_request_id(
            "artifact",
            "msg_1",
            &valid_sha,
            Some("install_demo"),
            Some(&valid_sha),
            Some("g1"),
        );
        let r2 = deterministic_prepare_request_id(
            "artifact",
            "msg_1",
            &valid_sha,
            Some("install_demo"),
            Some(&valid_sha),
            Some("g1"),
        );
        let r3 = deterministic_prepare_request_id(
            "artifact",
            "msg_2",
            &valid_sha,
            Some("install_demo"),
            Some(&valid_sha),
            Some("g1"),
        );
        assert!(r1.starts_with("parq_") && r1.len() == 37);
        assert_eq!(r1, r2);
        assert_ne!(r1, r3);
    }

    #[test]
    fn private_app_tools_loopback_end_to_end_and_uncertain_guidance() {
        use crate::local_assignment::reporting;
        use crate::private_app_update::test_helpers;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        };

        let root = std::env::temp_dir().join(format!("cad2584-mcp-e2e-{}", uuid::Uuid::new_v4()));
        let state_dir = root.join("state");
        let auth_dir = root.join("auth");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&auth_dir).unwrap();

        let zip_bytes = b"PK\x03\x04\x14\x00\x00\x00\x00\x00demo-private-app-zip-payload";
        let zip_hex = test_helpers::sha256_hex(zip_bytes);
        let zip_sha256 = format!("sha256:{zip_hex}");
        let pkg_digest = format!("sha256:{}", "d".repeat(64));
        let expected_installed_digest = format!("sha256:{}", "e".repeat(64));
        let review_id = format!("par_{}", "f".repeat(32));
        let artifact_id = format!("art-{}", "1".repeat(32));
        let message_id = "msg-cad2584-e2e";

        let fail_mode = Arc::new(AtomicUsize::new(0));
        let recorded = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let fail_mode_srv = Arc::clone(&fail_mode);
        let recorded_srv = Arc::clone(&recorded);
        let review_id_srv = review_id.clone();
        let pkg_digest_srv = pkg_digest.clone();
        let zip_sha_srv = zip_sha256.clone();

        let (endpoint, handle) = test_helpers::spawn_platform_loopback(
            32,
            artifact_id.clone(),
            zip_hex,
            move |path, req| match path {
                "/__platform/cli/call" => {
                    let args = req["arguments"].clone();
                    let tool = args["tool"].as_str().unwrap_or("").to_string();
                    recorded_srv
                        .lock()
                        .unwrap()
                        .push((tool.clone(), args.clone()));
                    let mode = fail_mode_srv.load(Ordering::SeqCst);
                    if (mode == 1 && tool == "private_app_review_request")
                        || (mode == 2 && tool == "private_app_review_apply")
                    {
                        (500, json!({"ok": false, "error": {"code": "internal"}}))
                    } else if mode == 3 {
                        (
                            409,
                            json!({"ok": false, "error": {"code": "installation_stale"}}),
                        )
                    } else {
                        (
                            200,
                            json!({"ok": true, "data": test_helpers::review_tool_data(&tool, &review_id_srv, &pkg_digest_srv, &zip_sha_srv)}),
                        )
                    }
                }
                _ => (404, json!({"error": {"code": "not_found"}})),
            },
        );

        let ko = test_helpers::kickoff(
            "updater", "CAD-2584", "/tmp/wt", message_id, &endpoint, None,
        );
        reporting::store_kickoff_binding(&state_dir, "acme", &endpoint, &ko, "update private app")
            .unwrap();
        reporting::store_private_app_package(
            &state_dir,
            message_id,
            "pkg_ref_1",
            "app-update.zip",
            zip_bytes,
            Some("install_app1"),
            Some(&expected_installed_digest),
            Some("c1:g1"),
        )
        .unwrap();

        let target = test_helpers::remote_target(&endpoint);
        let slot = test_helpers::write_agent_slot(&state_dir, message_id, &target);
        let expected_req_id = deterministic_prepare_request_id(
            "artifact",
            message_id,
            &zip_sha256,
            Some("install_app1"),
            Some(&expected_installed_digest),
            Some("c1:g1"),
        );

        let call = |name, args: Value| {
            execute_private_app_call_with(
                &auth_dir,
                &state_dir,
                message_id,
                &target,
                &slot,
                parse_private_app_call(name, &args).unwrap(),
                10,
            )
        };
        let call_json = |name, args: Value| -> Value {
            serde_json::from_str(&call(name, args).unwrap()).unwrap()
        };

        assert_eq!(
            call_json("private_app_prepare", json!({"package_ref": "pkg_ref_1"}))["data"]["id"],
            review_id
        );
        assert_eq!(
            call_json("private_app_status", json!({"id": review_id}))["data"]["outcome"],
            "authorized"
        );
        assert_eq!(
            call_json(
                "private_app_apply",
                json!({"id": review_id, "digest": pkg_digest, "zip_sha256": zip_sha256})
            )["data"]["outcome"],
            "applied"
        );

        // Uncertain prepare error names exact request_id for idempotent reconciliation.
        fail_mode.store(1, Ordering::SeqCst);
        let prep_err =
            call("private_app_prepare", json!({"package_ref": "pkg_ref_1"})).unwrap_err();
        assert!(matches!(prep_err, Error::OutcomeUnknown(_)), "{prep_err:?}");
        assert!(
            prep_err.to_string().contains(&expected_req_id),
            "{prep_err}"
        );

        // Uncertain apply error names private_app_reconcile + review id; reconcile succeeds as a read.
        fail_mode.store(2, Ordering::SeqCst);
        let apply_err = call(
            "private_app_apply",
            json!({"id": review_id, "digest": pkg_digest, "zip_sha256": zip_sha256}),
        )
        .unwrap_err();
        assert!(
            matches!(apply_err, Error::OutcomeUnknown(_)),
            "{apply_err:?}"
        );
        let apply_msg = apply_err.to_string();
        assert!(
            apply_msg.contains("private_app_reconcile") && apply_msg.contains(&review_id),
            "{apply_msg}"
        );
        assert_eq!(
            call_json("private_app_reconcile", json!({"id": review_id}))["data"]["outcome"],
            "applied"
        );

        // HTTP 409 conflicts fail closed as Rejected (never returned as Ok).
        fail_mode.store(3, Ordering::SeqCst);
        let conflict_err = call(
            "private_app_apply",
            json!({"id": review_id, "digest": pkg_digest, "zip_sha256": zip_sha256}),
        )
        .unwrap_err();
        assert!(
            matches!(conflict_err, Error::Rejected(_)),
            "{conflict_err:?}"
        );
        assert!(
            conflict_err.to_string().contains("installation_stale"),
            "{conflict_err}"
        );

        let log = recorded.lock().unwrap();
        let req_call = log
            .iter()
            .find(|(tool, _)| tool == "private_app_review_request")
            .unwrap();
        assert_eq!(req_call.1["body"]["artifact_id"], artifact_id);
        assert_eq!(req_call.1["body"]["zip_sha256"], zip_sha256);
        assert_eq!(req_call.1["body"]["install_id"], "install_app1");
        assert_eq!(
            req_call.1["body"]["expected_digest"],
            expected_installed_digest
        );
        assert_eq!(req_call.1["body"]["expected_generation"], "c1:g1");
        assert_eq!(req_call.1["body"]["request_id"], expected_req_id);

        let _ = std::fs::remove_dir_all(&root);
        drop(handle);
    }

    #[test]
    fn hosted_app_tools_advertise_strict_schemas_and_validate_inputs() {
        let listed = tools(&[]);
        let arr = listed.as_array().unwrap();
        for name in [
            "app_describe",
            "app_skill",
            "app_call",
            "board_pending_actions",
        ] {
            let entry = arr
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("missing tool {name}"));
            assert_eq!(entry["inputSchema"]["additionalProperties"], json!(false));
        }

        assert!(matches!(
            parse_hosted_app_call("app_describe", &json!({"app": "waba"})).unwrap(),
            HostedAppToolCall::Describe { .. }
        ));
        assert!(matches!(
            parse_hosted_app_call("app_skill", &json!({"app": "waba", "skill": "waba-ops"}))
                .unwrap(),
            HostedAppToolCall::Skill { .. }
        ));
        assert!(matches!(
            parse_hosted_app_call(
                "app_call",
                &json!({
                    "app": "waba",
                    "operation": "whatsapp.reply.stage",
                    "args": {"provider": "official", "conversation_id": "conv_1", "text": "hi"},
                    "reason": "reply to customer",
                    "request_id": "req_1"
                })
            )
            .unwrap(),
            HostedAppToolCall::Call { .. }
        ));
        assert!(matches!(
            parse_hosted_app_call("board_pending_actions", &json!({"status": "pending"})).unwrap(),
            HostedAppToolCall::PendingActions { .. }
        ));

        for (name, bad_args) in [
            ("app_describe", json!({})),
            ("app_describe", json!({"app": "Bad_App"})),
            ("app_skill", json!({"app": "waba"})),
            ("app_skill", json!({"app": "waba", "skill": ""})),
            ("app_call", json!({"app": "waba"})),
            (
                "app_call",
                json!({"app": "waba", "operation": "whatsapp.status", "args": "not_object"}),
            ),
            ("app_call", json!({"app": "waba", "operation": "BadOp"})),
            (
                "app_call",
                json!({"app": "waba", "operation": "whatsapp.status", "request_id": "bad id!"}),
            ),
            (
                "app_call",
                json!({"app": "waba", "operation": "whatsapp.status", "extra": true}),
            ),
            ("board_pending_actions", json!({"conversation": "home"})),
            ("board_pending_actions", json!({"status": "executed"})),
        ] {
            assert!(
                parse_hosted_app_call(name, &bad_args).is_err(),
                "expected refusal for {name}: {bad_args}"
            );
        }
    }

    #[test]
    fn hosted_app_tools_loopback_end_to_end_qr_guard_and_uncertain() {
        use crate::local_assignment::reporting;
        use crate::private_app_update::test_helpers;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        };

        let root = std::env::temp_dir().join(format!("cad2649-mcp-app-{}", uuid::Uuid::new_v4()));
        let state_dir = root.join("state");
        let auth_dir = root.join("auth");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&auth_dir).unwrap();

        let message_id = "msg-cad2649-mcp";
        let mode = Arc::new(AtomicUsize::new(0));
        let recorded = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let mode_srv = Arc::clone(&mode);
        let recorded_srv = Arc::clone(&recorded);
        let artifact_id = format!("art-{}", "1".repeat(32));
        let dummy_hex = "0".repeat(64);

        let (endpoint, handle) = test_helpers::spawn_platform_loopback(
            32,
            artifact_id,
            dummy_hex,
            move |path, req| match path {
                "/__platform/cli/call" => {
                    let args = req["arguments"].clone();
                    let tool = args["tool"].as_str().unwrap_or("").to_string();
                    recorded_srv
                        .lock()
                        .unwrap()
                        .push((tool.clone(), args.clone()));
                    match mode_srv.load(Ordering::SeqCst) {
                        1 => (500, json!({"ok": false, "error": {"code": "internal"}})),
                        2 => (
                            200,
                            json!({"ok": true, "data": {"state": "pairing", "qr": "SECRET-QR"}}),
                        ),
                        3 => (
                            200,
                            json!({"ok": true, "data": {"state": "pairing", "qrCode": "CAMEL-QR"}}),
                        ),
                        _ => (
                            200,
                            json!({
                                "ok": true,
                                "data": {
                                    "tool": tool,
                                    "staged": args["body"]["operation"] == "whatsapp.reply.stage",
                                    "action_id": "0123456789abcdef0123456789abcdef"
                                }
                            }),
                        ),
                    }
                }
                _ => (404, json!({"error": {"code": "not_found"}})),
            },
        );

        let ko = test_helpers::kickoff(
            "waba-agent",
            "CAD-2649",
            "/tmp/wt",
            message_id,
            &endpoint,
            None,
        );
        reporting::store_kickoff_binding(&state_dir, "acme", &endpoint, &ko, "handle waba")
            .unwrap();
        let target = test_helpers::remote_target(&endpoint);
        let slot = test_helpers::write_agent_slot(&state_dir, message_id, &target);

        let desc: Value = serde_json::from_str(
            &invoke_hosted_app_tool_with(
                &auth_dir,
                message_id,
                &target,
                &slot,
                "app_describe",
                &json!({"app": "waba"}),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(desc["data"]["tool"], "app_describe");

        let skill: Value = serde_json::from_str(
            &invoke_hosted_app_tool_with(
                &auth_dir,
                message_id,
                &target,
                &slot,
                "app_skill",
                &json!({"app": "waba", "skill": "waba-ops"}),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(skill["data"]["tool"], "app_skill");

        let staged: Value = serde_json::from_str(
            &invoke_hosted_app_tool_with(
                &auth_dir,
                message_id,
                &target,
                &slot,
                "app_call",
                &json!({
                    "app": "waba",
                    "operation": "whatsapp.reply.stage",
                    "args": {"provider": "official", "conversation_id": "conv_1", "text": "Hi"},
                    "reason": "customer asked",
                    "request_id": "req_stage_1"
                }),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(staged["data"]["staged"], true);

        let pending: Value = serde_json::from_str(
            &invoke_hosted_app_tool_with(
                &auth_dir,
                message_id,
                &target,
                &slot,
                "board_pending_actions",
                &json!({"id": "0123456789abcdef0123456789abcdef"}),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(pending["data"]["tool"], "board_pending_actions");

        // QR payloads (including camelCase variants) are refused even if returned from app_call.
        for qr_mode in [2, 3] {
            mode.store(qr_mode, Ordering::SeqCst);
            let qr_err = invoke_hosted_app_tool_with(
                &auth_dir,
                message_id,
                &target,
                &slot,
                "app_call",
                &json!({"app": "waba", "operation": "whatsapp.status", "args": {"provider": "linked"}}),
                10,
            )
            .unwrap_err();
            assert!(qr_err.to_string().contains("QR"), "{qr_err}");
        }

        // Uncertain app_call names exact request_id and board_pending_actions.
        mode.store(1, Ordering::SeqCst);
        let expected_req = deterministic_app_call_request_id(
            message_id,
            "waba",
            "whatsapp.reply.stage",
            r#"{"conversation_id":"conv_1","provider":"official","text":"Hi"}"#,
            None,
        );
        let unc_err = invoke_hosted_app_tool_with(
            &auth_dir,
            message_id,
            &target,
            &slot,
            "app_call",
            &json!({
                "app": "waba",
                "operation": "whatsapp.reply.stage",
                "args": {"provider": "official", "conversation_id": "conv_1", "text": "Hi"}
            }),
            10,
        )
        .unwrap_err();
        assert!(matches!(unc_err, Error::OutcomeUnknown(_)), "{unc_err:?}");
        let msg = unc_err.to_string();
        assert!(
            msg.contains(&expected_req) && msg.contains("board_pending_actions"),
            "{msg}"
        );

        let log = recorded.lock().unwrap();
        let app_call_req = log.iter().find(|(t, _)| t == "app_call").unwrap();
        assert_eq!(app_call_req.1["key"], "req_stage_1");
        assert!(app_call_req.1["body"]["args"].is_string());

        let _ = std::fs::remove_dir_all(&root);
        drop(handle);
    }
}
