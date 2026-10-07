//! CAD-1019 slice 2: the remote CLI transport — allowlisted verbs over the
//! AgenticOS `POST /__platform/cli/{authorize,call}` relay (AOS-128, contract
//! `docs/design/remote-cli.md`). A selected `Remote` org routes a command
//! here instead of the local daemon socket; there is no local fallback.
//!
//! Wire contract (pinned to `origin/cadence/aos-128-hosted-cadence-cli-allowlisted-p`,
//! `hosted-cadence-auth.ts` + `board/routes.ts` + `hosted-cadence/wiki.ts`):
//!
//! - `POST {endpoint}/__platform/cli/authorize` — bearer `hct_` → server-minted
//!   cli actor envelope. Body is exactly `{version:"hosted-cadence-cli.v1",
//!   organization_id, audience, requested_scopes:["cli.read","cli.write"],
//!   ttl_seconds}` (a strict object — an extra key is `invalid_request`).
//!   The envelope is **single-use** — the container (#744) consumes its `jti`.
//!   [`call_once`] mints a fresh one for every command invocation, and the wake
//!   loop re-mints on every retry: a `503 waking` is produced by the worker's
//!   `wakeBoardRuntime` *before* any container forward, so the just-minted
//!   envelope's `jti` was never consumed — but re-minting is still the only
//!   safe posture (a `503` that came later in the pipeline would have burned
//!   it). An envelope is never reused across calls or retries.
//! - `POST {endpoint}/__platform/cli/call` — same bearer + `{version,
//!   envelope, verb, arguments?}` — `verb` is one of
//!   [`REMOTE_VERBS`]; `arguments` is an object the container's
//!   `/api/cli/<verb>` dispatch consumes. Top-level authority-shaped keys
//!   (`actor`, `role`, `org`, `user`, `as`, `principal`, `wiki_as`) are
//!   refused server-side; this client never sends them.
//! - `401/403` — the bearer no longer resolves to a live principal: re-login.
//! - `503 {"state":"waking","retry_after_s":N}` + `Retry-After` — the company
//!   container is waking; wait and retry inside the wake budget. A retry is
//!   safe: the refusal came back before the verb ran. `wake_failed`
//!   (`runtime_stopped`/`not_found`/`unavailable`) and every other failure
//!   end the command — never a redirect chase, never another org, never local.

use serde_json::{json, Map, Value};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::remote_auth::auth_dir;

const CLI_VERSION: &str = "hosted-cadence-cli.v1";
/// `ttl_seconds` ceiling (`HOSTED_CADENCE_MAX_TTL_SECONDS`): the server also
/// clamps to the live credential's expiry, so asking the maximum is exact.
const MAX_TTL: u64 = 300;
/// Authorization/call bodies are small; answers can carry a whole tracker
/// board. Bounded either way (the relay caps upstream bodies at 16 MiB).
const MAX_BODY: u64 = 8 * 1024 * 1024;
/// No single call waits on a waking/loaded remote past this; the wake
/// budget (`--wake-timeout`, default 120 s) caps the whole retry loop.
const CALL_TIMEOUT: Duration = Duration::from_secs(90);
const AUTH_TIMEOUT: Duration = Duration::from_secs(20);
/// Default `--wake-timeout`.
pub const WAKE_TIMEOUT_DEFAULT: u64 = 120;
/// Upper bound on one wake wait, whatever the remote advertises.
const WAKE_WAIT_MAX: Duration = Duration::from_secs(30);

/// The allowlisted remote verbs this build can send — the CAD-1019 contract
/// ∩ AOS-128's `HOSTED_CADENCE_CLI_VERBS`, minus `team_list` (no local
/// `team` command exists to map to it) and `message_send` (an agent-to-agent
/// channel the remote milestone does not expose): a command not listed here
/// is refused *locally* and its bytes never leave the process.
///
/// The cli.write half (`issue_new`/`issue_comment`/`issue_set`) rides the
/// same envelope — the caller's authority never travels as a request field,
/// only inside the verified, single-use envelope. A failed or ambiguous
/// call is never retried by the client: the wake retry is the only loop,
/// and it fires only on a pre-forward `waking` verdict. Anything else — a
/// timeout, a dropped connection, a 5xx after forward — stays explicitly
/// unresolved: the mutation may or may not have landed, so reconcile with
/// `issue show`/`issue log` first rather than blindly re-issuing.
pub const REMOTE_VERBS: &[&str] = &[
    // cli.read
    "status",
    "agent_list",
    "agent_show",
    "issue_ls",
    "issue_show",
    "issue_history",
    "message_read",
    "message_inbox",
    // cli.write
    "issue_new",
    "issue_comment",
    "issue_set",
];

/// A resolved remote org destination — pinned once per invocation from the
/// registry (`cli::org::resolve`). `org` is the registry name (the
/// issuer-verified slug); `endpoint`/`org_id` came only from the verified
/// grant at login. Nothing here is caller-mutable mid-command.
#[derive(Clone, Debug)]
pub struct RemoteTarget {
    pub org: String,
    pub endpoint: String,
    pub org_id: String,
}

/// The `cli-<slug>.json` record `LoginGrant::save_credential` writes —
/// re-declared here so a field mismatch fails closed at load, not at POST.
struct Credential {
    organization_id: String,
    audience: String,
    access_token: String,
    expires_at: u64,
}

fn reject(message: impl Into<String>) -> Error {
    Error::rejected(message.into())
}

fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| reject("invalid system clock"))?
        .as_secs())
}

/// Read the stored `hct_` for `target` from `dir` (the remote-auth dir).
/// The file must still name exactly this org's workspace and endpoint — a
/// credential recorded for another org is never sent to this host (contract
/// I1). Expired means "run `cadence login` again", not retry.
fn load_credential(dir: &Path, target: &RemoteTarget) -> Result<Credential> {
    let path = dir.join(format!("cli-{}.json", target.org));
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| {
            reject(format!(
                "no remote credential for org '{}' — run `cadence login --issuer \
                 <issuer> --org {} --slug {}` first",
                target.org, target.org_id, target.org
            ))
        })?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > 8192 {
        return Err(reject("remote credential is not a small regular file"));
    }
    let mut raw = Vec::new();
    file.take(8193).read_to_end(&mut raw)?;
    let body: Value =
        serde_json::from_slice(&raw).map_err(|_| reject("invalid remote credential"))?;
    let cred = Credential {
        organization_id: body["organization_id"]
            .as_str()
            .ok_or_else(|| reject("invalid remote credential"))?
            .to_string(),
        audience: body["audience"]
            .as_str()
            .ok_or_else(|| reject("invalid remote credential"))?
            .to_string(),
        access_token: body["access_token"]
            .as_str()
            .ok_or_else(|| reject("invalid remote credential"))?
            .to_string(),
        expires_at: body["expires_at"]
            .as_u64()
            .ok_or_else(|| reject("invalid remote credential"))?,
    };
    if cred.organization_id != target.org_id || cred.audience != target.endpoint {
        return Err(reject(
            "the stored credential belongs to a different workspace or host; \
             it is never sent to another destination — run `cadence login` again",
        ));
    }
    if !cred.access_token.starts_with("hct_") {
        return Err(reject("invalid remote credential"));
    }
    if cred.expires_at <= now()? {
        return Err(reject(
            "the remote credential has expired — run `cadence login` again",
        ));
    }
    Ok(cred)
}

/// One bounded POST to `{endpoint}{path}` with the `hct_` bearer. Redirects
/// are never followed (a 3xx is a verdict, not a hint: contract refusal
/// cases). Returns `(status, retry_after_seconds, body)`; `body` is `None`
/// when the answer is not JSON.
fn post(
    endpoint: &str,
    path: &str,
    bearer: &str,
    body: &Value,
    timeout: Duration,
) -> Result<(u16, Option<u64>, Option<Value>)> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build();
    let response = ureq::Agent::new_with_config(config)
        .post(format!("{endpoint}{path}"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send_json(body)
        .map_err(|_| reject("remote request failed"))?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(reject("remote response too large"));
    }
    let value = serde_json::from_slice(&bytes).ok();
    Ok((status, retry_after, value))
}

/// `POST /__platform/cli/authorize` — exchange the live `hct_` for one
/// cli-scoped actor envelope. The request is the strict AOS-128 shape;
/// the answer must echo the version and carry a compact-JWS envelope.
fn authorize(target: &RemoteTarget, cred: &Credential) -> Result<String> {
    let (status, _, body) = post(
        &target.endpoint,
        "/__platform/cli/authorize",
        &cred.access_token,
        &json!({
            "version": CLI_VERSION,
            "organization_id": target.org_id,
            "audience": target.endpoint,
            "requested_scopes": ["cli.read", "cli.write"],
            "ttl_seconds": MAX_TTL,
        }),
        AUTH_TIMEOUT,
    )?;
    let body = body.ok_or_else(|| reject("remote authorize answered non-JSON"))?;
    match status {
        200 => {
            let envelope = body["envelope"].as_str().unwrap_or_default();
            let compact_jws = envelope.split('.').all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            }) && envelope.split('.').count() == 3;
            if body["version"].as_str() != Some(CLI_VERSION) || !compact_jws {
                return Err(reject("remote authorize returned an invalid envelope"));
            }
            Ok(envelope.to_string())
        }
        401 | 403 => Err(reject(
            "the remote refused the credential — run `cadence login` again",
        )),
        s if (300..400).contains(&s) => Err(reject("remote redirect refused")),
        _ => Err(reject(format!(
            "remote authorize failed ({})",
            body["code"].as_str().unwrap_or("unavailable")
        ))),
    }
}

/// One `POST /__platform/cli/call` attempt: fresh envelope, strict body,
/// never an authority-shaped key. `arguments` carries only the verb's own
/// payload (identity/org/destination live only in the verified envelope).
fn call_once(
    target: &RemoteTarget,
    cred: &Credential,
    verb: &str,
    arguments: &Map<String, Value>,
) -> Result<(u16, Option<u64>, Option<Value>)> {
    let envelope = authorize(target, cred)?;
    let mut body = json!({
        "version": CLI_VERSION,
        "envelope": envelope,
        "verb": verb,
    });
    if !arguments.is_empty() {
        body["arguments"] = Value::Object(arguments.clone());
    }
    post(
        &target.endpoint,
        "/__platform/cli/call",
        &cred.access_token,
        &body,
        CALL_TIMEOUT,
    )
}

/// Outcome of one call attempt: either the answer is final, the remote
/// answered a checked write conflict (HTTP 409 + `{conflict,…}` — the
/// caller resyncs, never a retry), or the remote is waking and `wait`
/// is how long to sleep before the next attempt.
enum Verdict {
    Done(Result<Value>),
    Conflict(Value),
    /// CAD-1180: known-applied write verdict — preserve its durability
    /// disposition and receipt, whether confirmed or unconfirmed.
    Applied(Value),
    Waking(Duration),
}

fn classify(status: u16, retry_after: Option<u64>, body: Option<Value>) -> Verdict {
    match status {
        200..=299 => Verdict::Done(body.ok_or_else(|| reject("remote answer was not JSON"))),
        503 => {
            // A received post-write verdict is authoritative whether the
            // host confirmed publication or preserved a known-applied
            // failure receipt. Keep it terminal and visible; only an
            // absent/invalid body is an unknown transport outcome.
            if let Some(b) = &body {
                if b["applied"].as_bool() == Some(true)
                    && matches!(b["durability"].as_str(), Some("confirmed" | "unconfirmed"))
                    && b["receipt"].is_object()
                {
                    return Verdict::Applied(b.clone());
                }
            }
            let state = body
                .as_ref()
                .and_then(|b| b["state"].as_str())
                .unwrap_or_default();
            let advertised = body
                .as_ref()
                .and_then(|b| b["retry_after_s"].as_u64())
                .or(retry_after)
                .unwrap_or(5);
            match state {
                // Only an explicit `waking` reply may retry — a bare 503
                // (`board_unreachable`, `capability_unavailable`,
                // `runtime_unavailable`) is terminal, like `wake_failed`.
                "waking" => Verdict::Waking(Duration::from_secs(advertised).min(WAKE_WAIT_MAX)),
                "wake_failed" => Verdict::Done(Err(reject(format!(
                    "remote org is not running ({})",
                    body.and_then(|b| b["code"].as_str().map(str::to_string))
                        .unwrap_or_else(|| "wake_failed".into())
                )))),
                _ => Verdict::Done(Err(reject("remote org is unavailable"))),
            }
        }
        401 | 403 => Verdict::Done(Err(reject(
            "the remote refused the credential — run `cadence login` again",
        ))),
        s if (300..400).contains(&s) => Verdict::Done(Err(reject("remote redirect refused"))),
        409 => {
            // A checked conflict (`{conflict, current_rev, card, error}`
            // — the payload `write_reply` maps a stale `if_rev` to) is a
            // definite "not written" verdict, never a refusal of the
            // call itself and never retried. The body rides up so the
            // CLI can print `current_rev`/`card` for the caller to
            // resync on the spot. A 409 without that shape is the same
            // generic refusal as any other unhandled status.
            match body {
                Some(b) if b.get("conflict").is_some() => Verdict::Conflict(b),
                _ => Verdict::Done(Err(reject("remote call refused"))),
            }
        }
        _ => Verdict::Done(Err(reject("remote call refused"))),
    }
}

/// Run one allowlisted remote verb. The verb must already be in
/// [`REMOTE_VERBS`] (a bug in the caller is a local refusal, never a
/// request). The credential is loaded and checked against `target` before
/// the first byte is sent; wake retries re-mint the envelope per attempt
/// and are bounded by `wake_timeout` seconds. Progress goes to `note`
/// (stderr at the call site); the returned Value is the remote's JSON body.
/// What one remote verb answered: `Ok` — the verb's JSON body (a read
/// result or a confirmed write), or `Conflict` — the checked write
/// conflict body (`{conflict, current_rev, card, error}`) the remote
/// answered instead of writing. A conflict is a verdict, not an
/// error: the call itself succeeded and proved nothing was written.
#[derive(Debug)]
pub enum RemoteAnswer {
    Ok(Value),
    Conflict(Value),
    /// CAD-1180: the write is known applied, with its host durability
    /// disposition and receipt. Preserve the evidence; never replay it.
    Applied(Value),
}

pub fn call_verb(
    target: &RemoteTarget,
    verb: &str,
    arguments: Map<String, Value>,
    wake_timeout: u64,
    note: impl Fn(&str),
) -> Result<RemoteAnswer> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    call_verb_in(target, verb, arguments, wake_timeout, &dir, note, |d| {
        std::thread::sleep(d)
    })
}

/// `call_verb` with the credential dir and the sleep injectable — tests
/// drive the wake loop without wall-clock cost and point the credential
/// dir at a fixture. The verb allowlist is checked *here*, on the shared
/// path, so no caller can bypass it.
fn call_verb_in(
    target: &RemoteTarget,
    verb: &str,
    arguments: Map<String, Value>,
    wake_timeout: u64,
    dir: &Path,
    note: impl Fn(&str),
    sleep: impl Fn(Duration),
) -> Result<RemoteAnswer> {
    // I2: an unlisted verb is refused before a credential is even loaded.
    if !REMOTE_VERBS.contains(&verb) {
        return Err(reject(format!(
            "verb '{verb}' is not in the remote allowlist — never sent"
        )));
    }
    let cred = load_credential(dir, target)?;
    let deadline = Instant::now() + Duration::from_secs(wake_timeout);
    loop {
        let (status, retry_after, body) = call_once(target, &cred, verb, &arguments)?;
        match classify(status, retry_after, body) {
            Verdict::Done(result) => return result.map(RemoteAnswer::Ok),
            Verdict::Conflict(body) => return Ok(RemoteAnswer::Conflict(body)),
            Verdict::Applied(body) => return Ok(RemoteAnswer::Applied(body)),
            Verdict::Waking(wait) => {
                let now = Instant::now();
                if now + wait > deadline {
                    // The remote was still waking when the budget ran out —
                    // retryable by the operator's own choice, so `busy` (75)
                    // with the `waking` code the contract pins.
                    return Err(Error::busy_coded(
                        "waking",
                        format!(
                            "org '{}' is still waking after {}s — retry, or pass a \
                             larger --wake-timeout",
                            target.org, wake_timeout
                        ),
                    ));
                }
                note(&format!("waking {}… ({}s)", target.org, wait.as_secs()));
                sleep(wait);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    const ORG: &str = "acme";
    const ORG_ID: &str = "ws_real";
    const BRIDGE: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    fn target(listener: &TcpListener) -> RemoteTarget {
        RemoteTarget {
            org: ORG.into(),
            endpoint: format!("http://{}", listener.local_addr().unwrap()),
            org_id: ORG_ID.into(),
        }
    }

    /// Write the credential the login slice writes — the same on-disk
    /// layout (`cli-<slug>.json`, the `hct_` bound to org id + audience).
    fn store_credential_at(dir: &Path, org: &str, org_id: &str, endpoint: &str, expires_at: u64) {
        let body = json!({"version": "hosted-cadence-auth.v1",
            "organization_id": org_id, "audience": endpoint,
            "access_token": BRIDGE, "expires_at": expires_at});
        std::fs::write(dir.join(format!("cli-{org}.json")), body.to_string()).unwrap();
    }
    fn store_credential(dir: &Path, endpoint: &str) {
        store_credential_at(dir, ORG, ORG_ID, endpoint, now().unwrap() + 300);
    }

    fn read_request(socket: std::net::TcpStream) -> (std::net::TcpStream, Request) {
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut parts = line.trim_end().split(' ');
        let (method, path) = (
            parts.next().unwrap().to_string(),
            parts.next().unwrap().to_string(),
        );
        assert_eq!(method, "POST");
        let mut bearer = String::new();
        let mut length = 0_usize;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.split_once(':').unwrap();
            if key.eq_ignore_ascii_case("authorization") {
                bearer = value.trim().trim_start_matches("Bearer ").to_string();
            }
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut raw = vec![0; length];
        reader.read_exact(&mut raw).unwrap();
        (
            socket,
            Request {
                path,
                bearer,
                body: serde_json::from_slice(&raw).unwrap(),
            },
        )
    }

    /// One request the fake accepted — path, bearer and parsed body.
    struct Request {
        path: String,
        bearer: String,
        body: Value,
    }

    fn answer(mut socket: std::net::TcpStream, status: &str, extra: &str, body: &Value) {
        let bytes = serde_json::to_vec(body).unwrap();
        write!(
            socket,
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
            bytes.len()
        )
        .unwrap();
        socket.write_all(&bytes).unwrap();
    }

    /// The strict `/__platform/cli/authorize` gate (mirrors
    /// `hostedCadenceCliAuthorizeRequestSchema`): every key required, no key
    /// extra, org and audience pinned to this board, scopes a subset of
    /// `cli.*`, ttl within 1..=300.
    fn check_authorize(req: &Request) -> bool {
        req.path == "/__platform/cli/authorize"
            && req.bearer == BRIDGE
            && req.body.as_object().map(Map::len) == Some(5)
            && req.body["version"] == json!(CLI_VERSION)
            && req.body["organization_id"] == json!(ORG_ID)
            && req.body["audience"].as_str().is_some()
            && req.body["requested_scopes"].as_array().is_some_and(|s| {
                !s.is_empty()
                    && s.iter()
                        .all(|v| v.as_str().is_some_and(|s| s.starts_with("cli.")))
            })
            && req.body["ttl_seconds"]
                .as_u64()
                .is_some_and(|t| (1..=300).contains(&t))
    }

    /// The strict `/__platform/cli/call` gate (mirrors
    /// `hostedCadenceCliCallRequestSchema` + the forbidden-key rule):
    /// version, a compact-JWS envelope, an allowlisted verb, optional
    /// arguments object — and no authority-shaped top-level key.
    fn check_call(req: &Request) -> bool {
        const FORBIDDEN: &[&str] = &["wiki_as", "actor", "role", "org", "user", "as", "principal"];
        let keys_ok = req
            .body
            .as_object()
            .is_some_and(|o| o.keys().all(|k| !FORBIDDEN.contains(&k.as_str())));
        let envelope_ok = req.body["envelope"].as_str().is_some_and(|e| {
            e.split('.').count() == 3
                && e.split('.').all(|p| {
                    !p.is_empty()
                        && p.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                })
        });
        req.path == "/__platform/cli/call"
            && req.bearer == BRIDGE
            && keys_ok
            && req.body["version"] == json!(CLI_VERSION)
            && envelope_ok
            && REMOTE_VERBS.contains(&req.body["verb"].as_str().unwrap_or_default())
            && req.body.get("arguments").is_none_or(|a| a.is_object())
    }

    /// A loopback fake worker: one authorize (strict schema → mint a fixed
    /// envelope shape), then `calls` call-attempts answered in order.
    /// Returns the bound listener plus a `Vec` the server records every
    /// request path + body into — so a test can prove nothing was sent.
    fn fake_worker(
        answers: Vec<(u16, &'static str, Value)>,
    ) -> (
        TcpListener,
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.try_clone().unwrap();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let requests = seen.clone();
        // The container (#744) consumes each cli envelope's `jti` once —
        // the fake does the same so a reused envelope is refused, not
        // silently accepted.
        let spent: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let consumed = spent.clone();
        let handle = thread::spawn(move || {
            loop {
                let Ok((socket, _)) = server.accept() else {
                    return;
                };
                let req = read_request(socket);
                let (socket, req) = (req.0, req.1);
                requests.lock().unwrap().push(json!({
                    "path": req.path, "body": req.body,
                }));
                if req.path == "/__platform/cli/authorize" {
                    if !check_authorize(&req) {
                        answer(
                            socket,
                            "400 Bad Request",
                            "",
                            &json!({"code": "invalid_request"}),
                        );
                        continue;
                    }
                    // Envelope shape only — the worker signs the real one.
                    // A distinct envelope per mint lets a test prove the
                    // client never reuses one across commands/retries (#744:
                    // the container treats each cli envelope as single-use).
                    let mint = requests
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|r| r["path"] == json!("/__platform/cli/authorize"))
                        .count();
                    let envelope = format!("h.e.{mint}");
                    answer(
                        socket,
                        "200 OK",
                        "",
                        &json!({"version": CLI_VERSION, "envelope": envelope,
                            "bearer": format!("wikienv_{envelope}"), "expires_at": 0,
                            "actor": "users/o", "scope": ["cli.read"], "issuer": "i"}),
                    );
                    continue;
                }
                if req.path == "/__platform/cli/call" {
                    let n = requests
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|r| r["path"] == json!("/__platform/cli/call"))
                        .count();
                    if !check_call(&req) {
                        answer(
                            socket,
                            "400 Bad Request",
                            "",
                            &json!({"code": "invalid_request"}),
                        );
                        continue;
                    }
                    // Single-use: a replayed envelope is unauthorized —
                    // the jti was already spent by an earlier call.
                    let env = req.body["envelope"].as_str().unwrap().to_string();
                    {
                        let mut spent = consumed.lock().unwrap();
                        if spent.contains(&env) {
                            answer(
                                socket,
                                "401 Unauthorized",
                                "",
                                &json!({"code": "envelope_reused"}),
                            );
                            continue;
                        }
                        spent.push(env);
                    }
                    let Some((status, extra, body)) = answers.get(n - 1) else {
                        // More attempts than answers — a test bug or an
                        // unbounded retry loop; refuse loudly.
                        answer(socket, "500", "", &json!({"code": "unexpected_call"}));
                        continue;
                    };
                    let line = match status {
                        200 => "200 OK",
                        302 => "302 Found",
                        401 => "401 Unauthorized",
                        403 => "403 Forbidden",
                        409 => "409 Conflict",
                        503 => "503 Service Unavailable",
                        _ => "400 Bad Request",
                    };
                    answer(socket, line, extra, &body.clone());
                    continue;
                }
                answer(socket, "404 Not Found", "", &json!({"code": "not_found"}));
            }
        });
        (listener, seen, handle)
    }

    /// Run one command against the strict fake: returns the call result
    /// and every request the worker saw (to count mints / reads).
    fn run(
        answers: Vec<(u16, &'static str, Value)>,
        verb: &str,
        args: Map<String, Value>,
        wake_timeout: u64,
    ) -> (Result<Value>, Vec<Value>) {
        let (listener, calls, _server) = fake_worker(answers);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        let out = call_verb_in(&target, verb, args, wake_timeout, &dir, |_| {}, |_| {});
        let calls_out = calls.lock().unwrap().clone();
        // Tests assert on the verb's JSON body — a conflict answer
        // still carries one, so unwrap it the same way.
        let out = out.map(|a| match a {
            RemoteAnswer::Ok(b) | RemoteAnswer::Conflict(b) | RemoteAnswer::Applied(b) => b,
        });
        (out, calls_out)
    }

    /// Number of requests the fake accepted at `path` — 0 proves no byte
    /// for that route ever left the client.
    fn hits(requests: &[Value], path: &str) -> usize {
        requests.iter().filter(|r| r["path"] == json!(path)).count()
    }

    #[test]
    fn credential_for_another_org_is_never_sent_to_this_host() {
        // The stored file names ws_real/acme's endpoint; the resolved org is
        // beta — the token must not ride to a foreign host (contract I1).
        let (listener, _c, _s) = fake_worker(vec![]);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, "https://acme.cadencecloud.app");
        let foreign = RemoteTarget {
            org: ORG.into(),
            endpoint: format!("http://{}", listener.local_addr().unwrap()),
            org_id: "ws_other".into(),
        };
        let out = call_verb_in(&foreign, "status", Map::new(), 10, &dir, |_| {}, |_| {});
        assert!(out.unwrap_err().to_string().contains("different workspace"));
    }

    /// Envelopes the calls presented, in order — the value the container
    /// would consume as single-use `jti`.
    fn envelopes(requests: &[Value]) -> Vec<String> {
        requests
            .iter()
            .filter(|r| r["path"] == json!("/__platform/cli/call"))
            .map(|r| r["body"]["envelope"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn every_command_mints_a_fresh_envelope() {
        // Single-use contract (#744): the container consumes the envelope's
        // `jti`, so two commands must present two different mints — an
        // envelope is never reused across calls.
        let (listener, requests, _s) =
            fake_worker(vec![(200, "", json!({"a": 1})), (200, "", json!({"b": 2}))]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        for verb in ["status", "agent_list"] {
            call_verb_in(&target, verb, Map::new(), 10, &dir, |_| {}, |_| {}).unwrap();
        }
        let envs = envelopes(&requests.lock().unwrap());
        assert_eq!(envs.len(), 2);
        assert_ne!(envs[0], envs[1], "two commands reused one envelope");
    }

    #[test]
    fn a_wake_retry_remints_the_envelope() {
        // The waking 503 came from the worker before any forward — the
        // envelope's jti was verified there but never consumed by a
        // container. Retrying still mints a fresh one: a 503 that fired
        // later in the pipeline would have burned the first.
        let (out, requests) = run(
            vec![
                (503, "", json!({"state": "waking", "retry_after_s": 1})),
                (200, "", json!({"ok": true})),
            ],
            "status",
            Map::new(),
            120,
        );
        assert!(out.is_ok());
        assert_eq!(hits(&requests, "/__platform/cli/authorize"), 2);
        let envs = envelopes(&requests);
        assert_eq!(envs.len(), 2);
        assert_ne!(envs[0], envs[1], "a wake retry reused the envelope");
    }

    #[test]
    fn a_checked_conflict_rides_up_as_the_conflict_answer() {
        // A 409 `{conflict, current_rev, card}` is the remote's checked
        // "not written" verdict — the body must reach the caller (the
        // CLI prints it and exits 5), not collapse into a generic
        // refusal. One attempt only: a conflict is never retried.
        let body = json!({"conflict": "if_rev",
            "current_rev": "fnv1a:829ba77079943a03",
            "card": {"id": "CAD-1"},
            "error": "if_rev does not match issue.md — re-read and retry"});
        let (listener, requests, _s) = fake_worker(vec![(409, "", body)]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        let out = call_verb_in(&target, "issue_set", Map::new(), 10, &dir, |_| {}, |_| {}).unwrap();
        let RemoteAnswer::Conflict(body) = out else {
            panic!("a 409 conflict answer must surface as Conflict, got {out:?}");
        };
        assert_eq!(body["current_rev"], json!("fnv1a:829ba77079943a03"));
        assert_eq!(body["card"]["id"], json!("CAD-1"));
        assert_eq!(
            hits(&requests.lock().unwrap(), "/__platform/cli/call"),
            1,
            "a conflict was retried"
        );
    }

    #[test]
    fn a_bare_409_without_the_conflict_shape_is_a_generic_refusal() {
        // Only the contract's `{conflict: …}` payload is a verdict; a
        // 409 carrying anything else stays the generic refusal.
        let (out, requests) = run(
            vec![(409, "", json!({"error": "midway collision"}))],
            "issue_set",
            Map::new(),
            10,
        );
        assert!(out.unwrap_err().to_string().contains("remote call refused"));
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }
}
