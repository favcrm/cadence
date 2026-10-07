//! CAD-832 acceptance for exact ownership and conditional Tailscale
//! LocalAPI serve-config mutations. The fixture implements the pinned
//! Tailscale HTTP contract over a private Unix socket; it does not emulate
//! Cadence's authorization decisions.

use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const PORT: u16 = 3117;
const BIND_TEST_PORT: u16 = 3199;
const SHARE_PORT: u16 = 19450;
const TARGET: &str = "http://127.0.0.1:3117";
const FOREIGN_TARGET: &str = "http://127.0.0.1:3118";
const FORGED_TARGET: &str = "http://127.0.0.1:3010";
const HOSTPORT: &str = "acceptance.example.ts.net:19450";

#[derive(Clone)]
struct ApiState {
    config: Value,
    generation: u64,
    race_on_port: Option<u16>,
    raced: bool,
    requests: Vec<(String, String, String)>,
    responses: Vec<(u16, Option<String>)>,
}

struct LocalApi {
    socket: PathBuf,
    state: Arc<Mutex<ApiState>>,
    map_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LocalApi {
    fn new(socket: PathBuf) -> Self {
        let listener = UnixListener::bind(&socket).expect("bind private LocalAPI socket");
        listener
            .set_nonblocking(true)
            .expect("nonblocking private LocalAPI listener");
        let state = Arc::new(Mutex::new(ApiState {
            config: Value::Null,
            generation: 1,
            race_on_port: None,
            raced: false,
            requests: Vec::new(),
            responses: Vec::new(),
        }));
        let worker_state = Arc::clone(&state);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let map_path = socket.with_extension("json");
        fs::write(&map_path, "null").expect("initialize canonical fixture config");
        let worker_map_path = map_path.clone();
        let thread = thread::spawn(move || {
            const MAX_REQUESTS: usize = 256;
            let mut served_requests = 0;
            while !worker_shutdown.load(Ordering::Acquire) && served_requests < MAX_REQUESTS {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if !worker_shutdown.load(Ordering::Acquire) {
                            serve_request(stream, &worker_state, &worker_map_path);
                            served_requests += 1;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            socket,
            state,
            map_path,
            shutdown,
            thread: Some(thread),
        }
    }

    fn seed(&self, config: Value) {
        let mut state = self.state.lock().unwrap();
        state.config = config;
        state.generation += 1;
        fs::write(&self.map_path, state.config.to_string())
            .expect("write canonical fixture config");
    }

    fn race_next_post(&self, port: u16) {
        let mut state = self.state.lock().unwrap();
        state.race_on_port = Some(port);
        state.raced = false;
    }

    fn config(&self) -> Value {
        self.state.lock().unwrap().config.clone()
    }

    fn requests(&self) -> Vec<(String, String, String)> {
        self.state.lock().unwrap().requests.clone()
    }

    fn responses(&self) -> Vec<(u16, Option<String>)> {
        self.state.lock().unwrap().responses.clone()
    }
}

impl Drop for LocalApi {
    fn drop(&mut self) {
        // Signal shutdown explicitly, then wake accept. Active requests have
        // one total I/O deadline, so joining this owned thread is bounded.
        self.shutdown.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.socket);
    }
}

fn serve_request(mut stream: UnixStream, shared: &Arc<Mutex<ApiState>>, map_path: &Path) {
    const REQUEST_DEADLINE: Duration = Duration::from_secs(2);
    const MAX_HEADER_BYTES: usize = 16 * 1024;
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    if i + 4 > MAX_HEADER_BYTES {
                        return;
                    }
                    break i + 4;
                }
                if bytes.len() > MAX_HEADER_BYTES {
                    return;
                }
            }
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = headers.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    let mut if_match = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("if-match") {
                if_match = Some(value.trim().to_string());
            }
        }
    }
    if content_length > 1024 * 1024 {
        return;
    }
    while bytes.len().saturating_sub(header_end) < content_length {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
        }
    }
    let body = String::from_utf8_lossy(&bytes[header_end..header_end + content_length]).to_string();
    let method = request_line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    let mut state = shared.lock().unwrap();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    state.requests.push((
        method.clone(),
        path.clone(),
        if_match.clone().unwrap_or_default(),
    ));

    let (status, reason, response_body, etag) =
        if method == "GET" && path == "/localapi/v0/serve-config" {
            let etag = (state.generation != 0).then(|| format!("\"{}\"", state.generation));
            (200, "OK", state.config.to_string(), etag)
        } else if method == "POST" && path == "/localapi/v0/serve-config" {
            // Faithful to the pinned backend: a concurrent writer wins before
            // the If-Match check; a stale conditional update gets 412. Upstream
            // accepts an absent If-Match, so this fixture intentionally does too.
            if !state.raced && state.race_on_port.is_some() {
                let race_port = state.race_on_port.unwrap();
                state.config = config_with_mapping(&state.config, race_port, FOREIGN_TARGET);
                state.generation += 1;
                state.raced = true;
                let _ = fs::write(map_path, state.config.to_string());
            }
            let current = format!("\"{}\"", state.generation);
            if if_match
                .as_deref()
                .is_some_and(|candidate| candidate != current)
            {
                (412, "Precondition Failed", String::new(), Some(current))
            } else if let Ok(config) = serde_json::from_str::<Value>(&body) {
                state.config = config;
                state.generation += 1;
                let _ = fs::write(map_path, state.config.to_string());
                (
                    200,
                    "OK",
                    String::new(),
                    Some(format!("\"{}\"", state.generation)),
                )
            } else {
                (400, "Bad Request", String::new(), Some(current))
            }
        } else {
            (404, "Not Found", String::new(), None)
        };
    state.responses.push((status, etag.clone()));
    drop(state);

    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response_body.len()
    );
    if let Some(etag) = etag {
        response.push_str(&format!("ETag: {etag}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(&response_body);
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() && stream.set_write_timeout(Some(remaining)).is_ok() {
        let _ = stream.write_all(response.as_bytes());
    }
}

fn config_with_mapping(base: &Value, port: u16, target: &str) -> Value {
    let mut config = if base.is_object() {
        base.clone()
    } else {
        json!({})
    };
    let web = config
        .as_object_mut()
        .expect("object config")
        .entry("Web")
        .or_insert_with(|| json!({}));
    let web = web.as_object_mut().expect("Web config map");
    web.insert(
        format!("acceptance.example.ts.net:{port}"),
        json!({"Handlers":{"/":{"Proxy":target}}}),
    );
    // Tailscale's Web handler is linked to its HTTPS TCP listener by port.
    config
        .as_object_mut()
        .expect("object config")
        .entry("TCP")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("TCP config map")
        .insert(port.to_string(), json!({"HTTPS":true}));
    config
}

fn mapping(port: u16, target: &str) -> Value {
    config_with_mapping(&json!({}), port, target)
}

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    state: PathBuf,
    fake_bin: PathBuf,
    log: PathBuf,
    map: PathBuf,
    api: LocalApi,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = Builder::new()
            .prefix(&format!("c832-{label}-"))
            .tempdir_in("/tmp")
            .expect("short private fixture root");
        let home = root.path().join("h");
        let state = root.path().join("s");
        let fake_bin = root.path().join("bin");
        let log = root.path().join("tailscale.log");
        let socket = root.path().join("ts.sock");
        let map = socket.with_extension("json");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::write(fake_bin.join("tailscale"), FAKE_TAILSCALE).unwrap();
        fs::set_permissions(
            fake_bin.join("tailscale"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let api = LocalApi::new(socket);
        Self {
            _root: root,
            home,
            state,
            fake_bin,
            log,
            map,
            api,
        }
    }

    fn write_opts(&self, target: &str) -> Vec<u8> {
        let bytes = format!(
            "{{\n  \"port\": {PORT},\n  \"tailscale\": {{\n    \"dns_name\": \"acceptance.example.ts.net\",\n    \"https_port\": {SHARE_PORT},\n    \"target\": \"{target}\"\n  }}\n}}\n"
        )
        .into_bytes();
        fs::write(self.state.join("ui.json"), &bytes).unwrap();
        bytes
    }

    fn write_unshared_opts(&self) -> Vec<u8> {
        let bytes = format!("{{\n  \"port\": {PORT}\n}}\n").into_bytes();
        fs::write(self.state.join("ui.json"), &bytes).unwrap();
        bytes
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BINARY);
        cmd.args(["--state-dir"])
            .arg(&self.state)
            .args(args)
            .current_dir(self._root.path())
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", self._root.path().join("xdg-state"))
            .env("XDG_CONFIG_HOME", self._root.path().join("xdg-config"))
            .env("XDG_DATA_HOME", self._root.path().join("xdg-data"))
            .env("XDG_CACHE_HOME", self._root.path().join("xdg-cache"))
            .env("CADENCE_PROFILE", "sandbox:cad832-acceptance")
            .env("CADENCE_SANDBOX_ALLOW_GLOBAL", "1")
            .env("CADENCE_PM_DIR", self._root.path().join("pm"))
            .env("CADENCE_SANDBOX_ROOT", self._root.path().join("sandboxes"))
            .env("CADENCE_TAILSCALE_SOCKET", &self.api.socket)
            .env("TS_LOG", &self.log)
            .env("TS_MAP", &self.map)
            .env("PATH", prefixed_path(&self.fake_bin))
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        cadence_agent::reaper::output(&mut cmd).expect("invoke real cadence CLI")
    }

    fn cli_without_home_or_pm(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BINARY);
        cmd.args(["--state-dir"])
            .arg(&self.state)
            .args(args)
            .current_dir(self._root.path())
            .env_remove("HOME")
            .env("XDG_STATE_HOME", self._root.path().join("xdg-state"))
            .env("XDG_CONFIG_HOME", self._root.path().join("xdg-config"))
            .env("XDG_DATA_HOME", self._root.path().join("xdg-data"))
            .env("XDG_CACHE_HOME", self._root.path().join("xdg-cache"))
            .env("CADENCE_PROFILE", "sandbox:cad832-acceptance")
            .env("CADENCE_SANDBOX_ALLOW_GLOBAL", "1")
            .env_remove("CADENCE_PM_DIR")
            .env("CADENCE_SANDBOX_ROOT", self._root.path().join("sandboxes"))
            .env("CADENCE_TAILSCALE_SOCKET", &self.api.socket)
            .env("TS_LOG", &self.log)
            .env("TS_MAP", &self.map)
            .env("PATH", prefixed_path(&self.fake_bin))
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        cadence_agent::reaper::output(&mut cmd).expect("invoke real cadence CLI")
    }

    fn tailscale_calls(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn no_cli_mutation(&self) -> bool {
        !self.tailscale_calls().lines().any(|line| {
            line.starts_with("serve --bg") || line.contains("serve ") && line.ends_with(" off")
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best-effort cleanup of only processes started by this fixture.
        let _ = self.cli(&["ui", "stop"]);
        let _ = self.cli(&["ui", "tailscale", "stop"]);
    }
}

fn prefixed_path(bin: &Path) -> String {
    format!(
        "{}:{}",
        bin.display(),
        std::env::var_os("PATH")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    )
}

fn output_text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn has_http_post(f: &Fixture, if_match: &str) -> bool {
    f.api.requests().iter().any(|(method, path, tag)| {
        method == "POST" && path == "/localapi/v0/serve-config" && tag == if_match
    })
}

fn snapshot_response(f: &Fixture) -> Option<(u16, Option<String>)> {
    f.api
        .requests()
        .iter()
        .zip(f.api.responses())
        .find(|((method, path, _), _)| method == "GET" && path == "/localapi/v0/serve-config")
        .map(|(_, response)| response)
}

fn snapshot_etag(f: &Fixture) -> Option<String> {
    snapshot_response(f).and_then(|(_, etag)| etag)
}

#[test]
fn persisted_sharing_refuses_foreign_changes_and_uses_conditional_owned_mutations() {
    // The established guards still run before any LocalAPI operation and
    // preserve the persisted record verbatim.
    let forged = Fixture::new("forged");
    let original = forged.write_opts(FORGED_TARGET);
    let rejected = forged.cli(&["ui", "start"]);
    assert!(
        !rejected.status.success(),
        "forged target accepted: {}",
        output_text(&rejected)
    );
    let forged_diagnostic = output_text(&rejected).to_ascii_lowercase();
    assert!(
        forged_diagnostic.contains("persisted tailnet target")
            && forged_diagnostic.contains("effective bind"),
        "forged-target refusal diagnostic regressed: {forged_diagnostic}"
    );
    assert_eq!(fs::read(forged.state.join("ui.json")).unwrap(), original);
    assert!(
        forged.api.requests().is_empty(),
        "forged target reached LocalAPI: {:?}",
        forged.api.requests()
    );
    assert!(
        forged.tailscale_calls().is_empty(),
        "forged target reached the Tailscale CLI: {}",
        forged.tailscale_calls()
    );

    for (label, args) in [
        (
            "start-flag-forged",
            vec!["ui", "start", "--tailscale", "19450"],
        ),
        (
            "tailscale-start-forged",
            vec!["ui", "tailscale", "start", "--port", "19450"],
        ),
    ] {
        let route = Fixture::new(label);
        let before = route.write_opts(FORGED_TARGET);
        let out = route.cli(&args);
        assert!(
            !out.status.success(),
            "{label} accepted forged target: {}",
            output_text(&out)
        );
        let diagnostic = output_text(&out).to_ascii_lowercase();
        assert!(
            diagnostic.contains("persisted tailnet target")
                && diagnostic.contains("effective bind"),
            "{label} did not report the persisted-target guard: {diagnostic}"
        );
        assert_eq!(
            fs::read(route.state.join("ui.json")).unwrap(),
            before,
            "{label} changed options"
        );
        assert!(
            route.api.requests().is_empty(),
            "{label} reached LocalAPI: {:?}",
            route.api.requests()
        );
        assert!(
            route.tailscale_calls().is_empty(),
            "{label} reached the Tailscale CLI: {}",
            route.tailscale_calls()
        );
    }

    let partial = Fixture::new("partial-device-login");
    let partial_original = partial.write_unshared_opts();
    let partial_out = partial.cli(&[
        "ui",
        "start",
        "--tailscale",
        "19450",
        "--device-login-issuer",
        "https://issuer.example",
    ]);
    assert!(!partial_out.status.success());
    assert!(output_text(&partial_out)
        .to_ascii_lowercase()
        .contains("device login needs issuer, org and at least one subject"));
    assert_eq!(
        fs::read(partial.state.join("ui.json")).unwrap(),
        partial_original
    );
    assert!(
        partial.api.requests().is_empty(),
        "partial device login reached LocalAPI"
    );
    assert!(
        partial.no_cli_mutation(),
        "partial device-login refusal attempted legacy mapping CLI write: {}",
        partial.tailscale_calls()
    );

    let foreground = Fixture::new("foreground-sandbox");
    let foreground_original = foreground.write_unshared_opts();
    let foreground_out = foreground.cli_without_home_or_pm(&["ui", "run", "--tailscale", "19450"]);
    assert!(!foreground_out.status.success());
    assert_eq!(
        fs::read(foreground.state.join("ui.json")).unwrap(),
        foreground_original
    );
    assert!(
        foreground.api.requests().is_empty(),
        "foreground route reached LocalAPI"
    );
    assert!(
        foreground.tailscale_calls().is_empty(),
        "foreground refusal invoked Tailscale: {}",
        foreground.tailscale_calls()
    );

    // Honest create: exact owned port, ETag from the GET snapshot, and
    // unrelated service settings survive the replacement.
    let honest = Fixture::new("honest-create");
    let seed = json!({
        "Web": {"acceptance.example.ts.net:19449":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:9042","AcceptAppCaps":["peer-cap:opaque-one","peer-cap:opaque-two"]},"/text":{"Text":"existing"}}}},
        "TCP":{"19451":{"TCPForward":"127.0.0.1:9040","TerminateTLS":"service.example.com","ProxyProtocol":1},"19449":{"HTTPS":true}},
        "AllowFunnel":{"acceptance.example.ts.net:19447":true},
        "Foreground":{"session":{"TCP":{"19448":{"HTTPS":true}},"Web":{"acceptance.example.ts.net:19448":{"Handlers":{"/":{"Text":"keep"}}}}}}
    });
    honest.api.seed(seed.clone());
    honest.write_opts(TARGET);
    let created = honest.cli(&["ui", "start"]);
    assert!(
        created.status.success(),
        "honest start failed: {}",
        output_text(&created)
    );
    let reqs = honest.api.requests();
    let get_tag = snapshot_etag(&honest).unwrap_or_default();
    assert!(
        !get_tag.is_empty(),
        "GET response omitted ETag: {:?}",
        honest.api.responses()
    );
    assert!(
        reqs.iter()
            .any(|(m, p, tag)| m == "GET" && p == "/localapi/v0/serve-config" && tag.is_empty()),
        "GET request unexpectedly supplied an If-Match: {reqs:?}"
    );
    assert!(
        has_http_post(&honest, &get_tag),
        "mapping POST was not conditional on snapshot ETag {get_tag:?}: {reqs:?}"
    );
    assert!(
        honest.no_cli_mutation(),
        "legacy CLI mapping write used: {}",
        honest.tailscale_calls()
    );
    let after = honest.api.config();
    assert_eq!(after["TCP"]["19451"], seed["TCP"]["19451"]);
    assert_eq!(
        after["Web"]["acceptance.example.ts.net:19449"],
        seed["Web"]["acceptance.example.ts.net:19449"]
    );
    assert_eq!(after["TCP"]["19449"], seed["TCP"]["19449"]);
    assert_eq!(after["TCP"]["19450"], json!({"HTTPS":true}));
    assert_eq!(after["AllowFunnel"], seed["AllowFunnel"]);
    assert_eq!(after["Foreground"], seed["Foreground"]);
    assert_eq!(
        after["Web"][HOSTPORT]["Handlers"]["/"],
        json!({"Proxy": TARGET})
    );
    assert_eq!(
        after["Web"]["acceptance.example.ts.net:19449"]["Handlers"]["/text"]["Text"],
        "existing"
    );
    let honest_stopped = honest.cli(&["ui", "stop"]);
    assert!(
        honest_stopped.status.success(),
        "could not stop honest fixture board: {}",
        output_text(&honest_stopped)
    );

    for (label, args) in [
        (
            "start-flag-honest",
            vec!["ui", "start", "--tailscale", "19450"],
        ),
        (
            "tailscale-start-honest",
            vec!["ui", "tailscale", "start", "--port", "19450"],
        ),
    ] {
        let route = Fixture::new(label);
        route.api.seed(json!({
            "Web":{"acceptance.example.ts.net:19449":{"Handlers":{"/":{"Text":"keep"}}}},
            "TCP":{"19449":{"HTTPS":true}}
        }));
        route.write_opts(TARGET);
        let out = route.cli(&args);
        assert!(
            out.status.success(),
            "{label} rejected honest sharing: {}",
            output_text(&out)
        );
        let requests = route.api.requests();
        let tag = snapshot_etag(&route).unwrap_or_default();
        assert!(
            !tag.is_empty() && has_http_post(&route, &tag),
            "{label} lacked conditional LocalAPI mutation: {requests:?}"
        );
        let route_config = route.api.config();
        assert_eq!(
            route_config["Web"][HOSTPORT]["Handlers"]["/"]["Proxy"],
            TARGET
        );
        assert_eq!(route_config["TCP"]["19450"], json!({"HTTPS":true}));
        assert!(
            route.no_cli_mutation(),
            "{label} used a CLI mutation: {}",
            route.tailscale_calls()
        );
    }

    let honest_remove = Fixture::new("honest-remove");
    let remove_seed = json!({
        "Web": {HOSTPORT:{"Handlers":{"/":{"Proxy":TARGET}}},"acceptance.example.ts.net:19449":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:9042","AcceptAppCaps":["peer-cap:opaque-one","peer-cap:opaque-two"]},"/text":{"Text":"keep"}}}},
        "TCP":{"19450":{"HTTPS":true},"19451":{"TCPForward":"127.0.0.1:9040","TerminateTLS":"service.example.com","ProxyProtocol":1},"19449":{"HTTPS":true}},
        "Services":{"svc:acceptance":{"TCP":{"19452":{"TCPForward":"127.0.0.1:9041"}}}},
        "AllowFunnel":{"acceptance.example.ts.net:19447":true},
        "Foreground":{"session":{"TCP":{"19448":{"HTTPS":true}},"Web":{"acceptance.example.ts.net:19448":{"Handlers":{"/":{"Text":"keep"}}}}}}
    });
    honest_remove.api.seed(remove_seed.clone());
    honest_remove.write_opts(TARGET);
    let remove_out = honest_remove.cli(&["ui", "tailscale", "stop"]);
    assert!(
        remove_out.status.success(),
        "honest remove failed: {}",
        output_text(&remove_out)
    );
    let remove_requests = honest_remove.api.requests();
    let remove_tag = snapshot_etag(&honest_remove).unwrap_or_default();
    assert!(
        !remove_tag.is_empty() && has_http_post(&honest_remove, &remove_tag),
        "remove did not use GET ETag: {remove_requests:?}"
    );
    assert!(
        honest_remove.no_cli_mutation(),
        "honest removal used legacy CLI write: {}",
        honest_remove.tailscale_calls()
    );
    let remove_after = honest_remove.api.config();
    assert!(
        remove_after["Web"].get(HOSTPORT).is_none(),
        "owned Web mapping was not removed: {remove_after}"
    );
    assert!(
        remove_after["TCP"].get("19450").is_none(),
        "owned HTTPS listener was not removed: {remove_after}"
    );
    assert_eq!(
        remove_after["Web"]["acceptance.example.ts.net:19449"],
        remove_seed["Web"]["acceptance.example.ts.net:19449"]
    );
    assert_eq!(remove_after["TCP"]["19451"], remove_seed["TCP"]["19451"]);
    assert_eq!(
        remove_after["Web"]["acceptance.example.ts.net:19449"],
        remove_seed["Web"]["acceptance.example.ts.net:19449"]
    );
    assert_eq!(remove_after["TCP"]["19449"], remove_seed["TCP"]["19449"]);
    assert_eq!(remove_after["Services"], remove_seed["Services"]);
    assert_eq!(remove_after["AllowFunnel"], remove_seed["AllowFunnel"]);
    assert_eq!(remove_after["Foreground"], remove_seed["Foreground"]);

    // Deterministic lost-update race: a foreign mapping is installed after
    // the client's GET but before its POST is compared. The backend returns
    // 412 and retains the foreign winner; no CLI fallback may overwrite it.
    let race_create = Fixture::new("race-create");
    race_create.api.seed(json!({
        "Web":{"acceptance.example.ts.net:19449":{"Handlers":{"/":{"Text":"keep"}}}},
        "TCP":{"19451":{"TCPForward":"127.0.0.1:9040"},"19449":{"HTTPS":true}},
        "Services":{"svc:acceptance":{"TCP":{"19452":{"TCPForward":"127.0.0.1:9041"}}}},
        "AllowFunnel":{"acceptance.example.ts.net:19447":true},
        "Foreground":{"session":{"TCP":{"19448":{"HTTPS":true}},"Web":{"acceptance.example.ts.net:19448":{"Handlers":{"/":{"Text":"keep"}}}}}}
    }));
    race_create.api.race_next_post(SHARE_PORT);
    race_create.write_opts(TARGET);
    let create_out = race_create.cli(&["ui", "start"]);
    assert!(
        !create_out.status.success(),
        "stale create unexpectedly succeeded: {}",
        output_text(&create_out)
    );
    let after_race = race_create.api.config();
    assert_eq!(
        after_race["Web"][HOSTPORT]["Handlers"]["/"]["Proxy"], FOREIGN_TARGET,
        "foreign concurrent winner was lost"
    );
    assert_eq!(
        after_race["Web"]["acceptance.example.ts.net:19449"]["Handlers"]["/"]["Text"],
        "keep"
    );
    assert_eq!(after_race["TCP"]["19450"], json!({"HTTPS":true}));
    assert_eq!(after_race["TCP"]["19451"]["TCPForward"], "127.0.0.1:9040");
    assert_eq!(
        after_race["Services"]["svc:acceptance"]["TCP"]["19452"]["TCPForward"],
        "127.0.0.1:9041"
    );
    assert_eq!(
        after_race["AllowFunnel"]["acceptance.example.ts.net:19447"],
        true
    );
    assert_eq!(
        after_race["Foreground"]["session"]["Web"]["acceptance.example.ts.net:19448"],
        json!({"Handlers":{"/":{"Text":"keep"}}})
    );
    assert!(
        race_create.no_cli_mutation(),
        "stale create fell back to CLI mutation: {}",
        race_create.tailscale_calls()
    );
    assert!(
        race_create
            .api
            .requests()
            .iter()
            .any(|(m, p, _)| m == "POST" && p == "/localapi/v0/serve-config"),
        "race did not reach conditional POST"
    );
    let create_snapshot_tag = snapshot_etag(&race_create).expect("create GET response ETag");
    assert!(
        has_http_post(&race_create, &create_snapshot_tag),
        "create POST did not use GET response ETag: {:?}",
        race_create.api.requests()
    );
    assert!(
        race_create
            .api
            .responses()
            .iter()
            .any(|(status, _)| *status == 412),
        "create race did not receive backend 412: {:?}",
        race_create.api.responses()
    );

    // Missing ETag is a fail-closed client condition. This server can omit
    // the header for GET but permits unconditional POST exactly as upstream
    // does; therefore a client that omits If-Match would visibly mutate.
    let no_etag = Fixture::new("missing-etag");
    let no_etag_config = json!({
        "Web":{"acceptance.example.ts.net:19449":{"Handlers":{"/":{"Text":"keep"}}}},
        "TCP":{"19449":{"HTTPS":true}},
        "AllowFunnel":{"acceptance.example.ts.net:19447":true}
    });
    no_etag.api.seed(no_etag_config.clone());
    no_etag.api.state.lock().unwrap().generation = 0;
    no_etag.write_opts(TARGET);
    let no_etag_out = no_etag.cli(&["ui", "start"]);
    assert!(
        !no_etag_out.status.success(),
        "missing ETag did not refuse: {}",
        output_text(&no_etag_out)
    );
    assert_eq!(no_etag.api.config(), no_etag_config);
    let no_etag_requests = no_etag.api.requests();
    assert!(
        no_etag_requests
            .iter()
            .any(|(m, p, _)| m == "GET" && p == "/localapi/v0/serve-config"),
        "missing-ETag case did not reach GET: {no_etag_requests:?}"
    );
    assert_eq!(
        snapshot_response(&no_etag),
        Some((200, None)),
        "missing-ETag GET response audit mismatch"
    );
    assert!(
        output_text(&no_etag_out)
            .to_ascii_lowercase()
            .contains("get response omitted a usable etag"),
        "refusal was not caused by the missing ETag: {}",
        output_text(&no_etag_out)
    );
    assert!(
        !no_etag_requests
            .iter()
            .any(|(m, p, _)| m == "POST" && p == "/localapi/v0/serve-config"),
        "missing ETag must refuse before POST: {no_etag_requests:?}"
    );
    assert!(no_etag.no_cli_mutation(), "missing ETag used CLI write");

    // Remove through the real `ui tailscale stop` route after the exact
    // mapping has been seeded. A foreign change in the GET/POST window must
    // remain, and failure must retain both the durable record and sandbox.
    let race_remove = Fixture::new("race-remove");
    let record = race_remove.write_opts(TARGET);
    race_remove.api.seed(config_with_mapping(
        &json!({
            "Web":{"acceptance.example.ts.net:19449":{"Handlers":{"/":{"Text":"keep"}}}},
            "TCP":{"19451":{"TCPForward":"127.0.0.1:9040"},"19449":{"HTTPS":true}},
            "Services":{"svc:acceptance":{"TCP":{"19452":{"TCPForward":"127.0.0.1:9041"}}}},
            "AllowFunnel":{"acceptance.example.ts.net:19447":true},
            "Foreground":{"session":{"TCP":{"19448":{"HTTPS":true}},"Web":{"acceptance.example.ts.net:19448":{"Handlers":{"/":{"Text":"keep"}}}}}}
        }),
        SHARE_PORT,
        TARGET,
    ));
    race_remove.api.race_next_post(SHARE_PORT);
    let removed = race_remove.cli(&["ui", "tailscale", "stop"]);
    assert!(
        !removed.status.success(),
        "stale removal unexpectedly succeeded: {}",
        output_text(&removed)
    );
    let remove_race_config = race_remove.api.config();
    assert_eq!(
        remove_race_config["Web"][HOSTPORT]["Handlers"]["/"]["Proxy"], FOREIGN_TARGET,
        "removal race deleted foreign mapping"
    );
    assert_eq!(remove_race_config["TCP"]["19450"], json!({"HTTPS":true}));
    assert_eq!(
        remove_race_config["TCP"]["19451"]["TCPForward"],
        "127.0.0.1:9040"
    );
    assert_eq!(
        remove_race_config["Services"]["svc:acceptance"]["TCP"]["19452"]["TCPForward"],
        "127.0.0.1:9041"
    );
    assert_eq!(
        remove_race_config["AllowFunnel"]["acceptance.example.ts.net:19447"],
        true
    );
    assert_eq!(
        remove_race_config["Foreground"]["session"]["Web"]["acceptance.example.ts.net:19448"],
        json!({"Handlers":{"/":{"Text":"keep"}}})
    );
    assert_eq!(
        fs::read(race_remove.state.join("ui.json")).unwrap(),
        record,
        "failed remove deleted the durable record"
    );
    assert!(
        race_remove.no_cli_mutation(),
        "remove used legacy CLI write: {}",
        race_remove.tailscale_calls()
    );
    assert!(
        has_http_post(&race_remove, "\"2\""),
        "remove did not use GET's stale ETag: {:?}",
        race_remove.api.requests()
    );
    let remove_snapshot_tag = snapshot_etag(&race_remove).expect("remove GET response ETag");
    assert!(
        has_http_post(&race_remove, &remove_snapshot_tag),
        "remove POST did not use GET response ETag: {:?}",
        race_remove.api.requests()
    );
    assert!(
        race_remove
            .api
            .responses()
            .iter()
            .any(|(status, _)| *status == 412),
        "remove race did not receive backend 412: {:?}",
        race_remove.api.responses()
    );

    // Foreign existing owner: reject before POST; do not clear/add unrelated
    // ports or alter funnel/foreground settings. This checks ownership is an
    // exact snapshot fact, not merely a desired destination assertion.
    let foreign_owner = Fixture::new("foreign-owner");
    foreign_owner.write_opts(TARGET);
    let mut foreign_config = mapping(SHARE_PORT, FOREIGN_TARGET);
    foreign_config["TCP"]["19451"] = json!({"TCPForward":"127.0.0.1:9040"});
    foreign_config["TCP"]["19448"] = json!({"HTTPS":true});
    foreign_config["AllowFunnel"] = json!({"acceptance.example.ts.net:19447":true});
    foreign_config["Foreground"] = json!({"session":{"TCP":{"19448":{"HTTPS":true}},"Web":{"acceptance.example.ts.net:19448":{"Handlers":{"/":{"Text":"keep"}}}}}});
    foreign_owner.api.seed(foreign_config.clone());
    let foreign_out = foreign_owner.cli(&["ui", "start"]);
    assert!(
        !foreign_out.status.success(),
        "foreign mapping owner was accepted: {}",
        output_text(&foreign_out)
    );
    assert!(
        !foreign_owner
            .api
            .requests()
            .iter()
            .any(|(m, p, _)| m == "POST" && p == "/localapi/v0/serve-config"),
        "foreign ownership must refuse before POST"
    );
    assert_eq!(foreign_owner.api.config(), foreign_config);

    let ambiguous = Fixture::new("ambiguous-owned-port");
    ambiguous.write_opts(TARGET);
    let ambiguous_config = json!({
        "Web": {HOSTPORT:{"Handlers":{"/":{"Proxy":TARGET},"/extra":{"Text":"unexpected second handler"}}}},
        "TCP":{"19450":{"HTTPS":true},"19451":{"TCPForward":"127.0.0.1:9040"}}
    });
    ambiguous.api.seed(ambiguous_config.clone());
    let ambiguous_out = ambiguous.cli(&["ui", "start"]);
    assert!(
        !ambiguous_out.status.success(),
        "ambiguous port ownership was accepted: {}",
        output_text(&ambiguous_out)
    );
    assert!(
        !ambiguous
            .api
            .requests()
            .iter()
            .any(|(m, p, _)| m == "POST" && p == "/localapi/v0/serve-config"),
        "ambiguous ownership must refuse before POST"
    );
    assert_eq!(ambiguous.api.config(), ambiguous_config);

    let second_host = Fixture::new("second-host-shares-owned-port");
    let second_host_record = second_host.write_opts(TARGET);
    let second_host_config = json!({
        "TCP":{"19450":{"HTTPS":true}},
        "Web":{
            HOSTPORT:{"Handlers":{"/":{"Proxy":TARGET}}},
            "second.example.ts.net:19450":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3119"}}}
        }
    });
    second_host.api.seed(second_host_config.clone());
    let second_host_out = second_host.cli(&["ui", "tailscale", "stop"]);
    assert!(
        !second_host_out.status.success(),
        "removal accepted a second host sharing the owned port: {}",
        output_text(&second_host_out)
    );
    assert!(
        !second_host
            .api
            .requests()
            .iter()
            .any(|(m, p, _)| m == "POST" && p == "/localapi/v0/serve-config"),
        "ambiguous second-host ownership must refuse before POST"
    );
    assert_eq!(
        second_host.api.config(),
        second_host_config,
        "second-host refusal changed config"
    );
    assert_eq!(
        fs::read(second_host.state.join("ui.json")).unwrap(),
        second_host_record,
        "second-host refusal deleted durable share record"
    );
    assert!(
        second_host.no_cli_mutation(),
        "second-host refusal attempted legacy CLI write: {}",
        second_host.tailscale_calls()
    );

    let reset_foreign = Fixture::new("reset-foreign-owner");
    let base = reset_foreign._root.path().join("sandboxes");
    let root = base.join("refusal");
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        root.join(".cadence-sandbox"),
        r#"{"name":"refusal","allow_global":true}"#,
    )
    .unwrap();
    let record = format!(
        r#"{{"port":{PORT},"tailscale":{{"dns_name":"acceptance.example.ts.net","https_port":{SHARE_PORT},"target":"{TARGET}"}}}}"#
    );
    fs::write(state.join("ui.json"), &record).unwrap();
    let reset_config = config_with_mapping(
        &json!({
            "TCP":{"19451":{"TCPForward":"127.0.0.1:9040"}},
            "AllowFunnel":{"acceptance.example.ts.net:19447":true}
        }),
        SHARE_PORT,
        FOREIGN_TARGET,
    );
    reset_foreign.api.seed(reset_config.clone());
    let reset_out = reset_foreign.cli(&["sandbox", "reset", "refusal"]);
    assert!(
        !reset_out.status.success(),
        "reset removed foreign mapping: {}",
        output_text(&reset_out)
    );
    assert!(root.is_dir(), "failed reset deleted sandbox root");
    assert_eq!(
        fs::read_to_string(state.join("ui.json")).unwrap(),
        record,
        "failed reset deleted share record"
    );
    assert_eq!(
        reset_foreign.api.config(),
        reset_config,
        "failed reset changed foreign/unrelated config"
    );
    assert!(
        reset_foreign.no_cli_mutation(),
        "reset invoked legacy CLI mapping write: {}",
        reset_foreign.tailscale_calls()
    );

    // Existing safety contract: reset under the actual native up lock must
    // wait and then clean up. Every child receives this fixture's private
    // socket, including reset and Fixture::drop cleanup.
    let serialized = Fixture::new("reset-up-serialization");
    let sandbox_base = serialized._root.path().join("sandboxes");
    let sandbox_name = "serialized";
    let sandbox_root = sandbox_base.join(sandbox_name);
    fs::create_dir_all(sandbox_root.join("state")).unwrap();
    fs::write(
        sandbox_root.join(".cadence-sandbox"),
        r#"{"name":"serialized","allow_global":true}"#,
    )
    .unwrap();
    let marker = fs::read(sandbox_root.join(".cadence-sandbox")).unwrap();
    let lock_path = sandbox_base.join(format!(".up-{sandbox_name}.lock"));
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    use std::os::fd::AsRawFd;
    assert_eq!(
        unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) },
        0
    );
    let mut reset_cmd = Command::new(BINARY);
    reset_cmd
        .args(["--state-dir"])
        .arg(&serialized.state)
        .args(["sandbox", "reset", sandbox_name])
        .current_dir(serialized._root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", &serialized.home)
        .env("XDG_STATE_HOME", serialized._root.path().join("xdg-state"))
        .env(
            "XDG_CONFIG_HOME",
            serialized._root.path().join("xdg-config"),
        )
        .env("XDG_DATA_HOME", serialized._root.path().join("xdg-data"))
        .env("XDG_CACHE_HOME", serialized._root.path().join("xdg-cache"))
        .env("CADENCE_PROFILE", "sandbox:cad832-acceptance")
        .env("CADENCE_SANDBOX_ALLOW_GLOBAL", "1")
        .env("CADENCE_PM_DIR", serialized._root.path().join("pm"))
        .env("CADENCE_SANDBOX_ROOT", &sandbox_base)
        .env("CADENCE_TAILSCALE_SOCKET", &serialized.api.socket)
        .env("TS_LOG", &serialized.log)
        .env("TS_MAP", &serialized.map)
        .env("PATH", prefixed_path(&serialized.fake_bin))
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    let mut child = cadence_agent::reaper::spawn(&mut reset_cmd).expect("start reset child");
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut exited_held = false;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            exited_held = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let preserved_held = sandbox_root.is_dir()
        && fs::read(sandbox_root.join(".cadence-sandbox")).unwrap_or_default() == marker;
    assert_eq!(
        unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN) },
        0
    );
    let release_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < release_deadline {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let timed_out = child.try_wait().unwrap().is_none();
    if timed_out {
        child.kill().expect("stop only child started here");
        let _ = child.wait();
    }
    let reset_output = child.wait_with_output().expect("collect reset output");
    assert!(
        !exited_held
            && preserved_held
            && !timed_out
            && reset_output.status.success()
            && !sandbox_root.exists(),
        "reset serialization failed (exited={exited_held}, preserved={preserved_held}, timeout={timed_out}): {}",
        output_text(&reset_output)
    );
}

#[test]
fn sharing_requires_literal_ipv4_loopback_and_a_matching_bind() {
    // Persisted host mismatch: only the literal 127.0.0.1 proxy target is
    // safe, and the listener must actually bind that same host and port.
    let persisted = Fixture::new("persisted-ipv6-bind");
    let original = format!(
        "{{\n  \"host\": \"::1\",\n  \"port\": {PORT},\n  \"tailscale\": {{\n    \"dns_name\": \"acceptance.example.ts.net\",\n    \"https_port\": {SHARE_PORT},\n    \"target\": \"{TARGET}\"\n  }}\n}}\n"
    );
    fs::write(persisted.state.join("ui.json"), &original).unwrap();
    let config = mapping(SHARE_PORT, TARGET);
    persisted.api.seed(config.clone());
    let mut failures = Vec::new();
    let out = persisted.cli(&["ui", "start"]);
    let after = fs::read(persisted.state.join("ui.json")).unwrap();
    let after_config = persisted.api.config();
    let requests = persisted.api.requests();
    if out.status.success()
        || after != original.as_bytes()
        || after_config != config
        || requests.iter().any(|(m, _, _)| m == "POST")
    {
        failures.push(format!(
            "persisted IPv6 bind: status={} output={} options_unchanged={} api_unchanged={} requests={requests:?}",
            out.status,
            output_text(&out),
            after == original.as_bytes(),
            after_config == config,
        ));
    }

    // A start-time bind override must be checked too, not only persisted
    // options. Try ::1 and localhost separately so aliases never become
    // an accidental IPv4 assumption.
    for (label, host) in [
        ("flag-ipv6-bind", "::1"),
        ("flag-localhost-bind", "localhost"),
    ] {
        let fixture = Fixture::new(label);
        let original = format!("{{\n  \"port\": {BIND_TEST_PORT}\n}}\n").into_bytes();
        fs::write(fixture.state.join("ui.json"), &original).unwrap();
        let out = fixture.cli(&["ui", "start", "--host", host, "--tailscale", "19450"]);
        let after = fs::read(fixture.state.join("ui.json")).unwrap();
        let after_config = fixture.api.config();
        let requests = fixture.api.requests();
        if out.status.success()
            || after != original
            || after_config != Value::Null
            || requests.iter().any(|(m, _, _)| m == "POST")
        {
            failures.push(format!(
                "{label}: status={} output={} options_unchanged={} api_unchanged={} requests={requests:?}",
                out.status,
                output_text(&out),
                after == original,
                after_config == Value::Null,
            ));
        }
    }

    // Explicit Tailscale start derives the bind from persisted UI options;
    // reject aliases there before either API mutation or options persistence.
    for (label, host) in [
        ("tailscale-start-ipv6-bind", "::1"),
        ("tailscale-start-localhost-bind", "localhost"),
    ] {
        let fixture = Fixture::new(label);
        let original =
            format!("{{\n  \"host\": \"{host}\",\n  \"port\": {BIND_TEST_PORT}\n}}\n").into_bytes();
        fs::write(fixture.state.join("ui.json"), &original).unwrap();
        let out = fixture.cli(&["ui", "tailscale", "start", "--port", "19450"]);
        let after = fs::read(fixture.state.join("ui.json")).unwrap();
        let after_config = fixture.api.config();
        let requests = fixture.api.requests();
        if out.status.success()
            || after != original
            || after_config != Value::Null
            || requests.iter().any(|(m, _, _)| m == "POST")
        {
            failures.push(format!(
                "{label}: status={} output={} options_unchanged={} api_unchanged={} requests={requests:?}",
                out.status,
                output_text(&out),
                after == original,
                after_config == Value::Null,
            ));
        }
    }

    // Preserve the positive control: explicitly bound IPv4 loopback can
    // create a new share and persist exactly the API-owned target.
    let ipv4 = Fixture::new("flag-ipv4-bind-control");
    fs::write(
        ipv4.state.join("ui.json"),
        format!("{{\n  \"port\": {BIND_TEST_PORT}\n}}\n"),
    )
    .unwrap();
    let out = ipv4.cli(&["ui", "start", "--host", "127.0.0.1", "--tailscale", "19450"]);
    assert!(
        out.status.success(),
        "IPv4 loopback control failed: {}",
        output_text(&out)
    );
    assert_eq!(
        ipv4.api.config()["Web"][HOSTPORT]["Handlers"]["/"]["Proxy"],
        format!("http://127.0.0.1:{BIND_TEST_PORT}")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(ipv4.state.join("ui.json")).unwrap()).unwrap()
            ["tailscale"]["target"],
        format!("http://127.0.0.1:{BIND_TEST_PORT}")
    );
    assert!(
        failures.is_empty(),
        "unsafe sharing cases failed: {failures:#?}"
    );
}

#[test]
fn stop_refuses_forged_or_production_target_before_api_mutation() {
    let mut failures = Vec::new();
    for (label, args, target, port, ui_port) in [
        (
            "tailscale-stop-forged",
            vec!["ui", "tailscale", "stop"],
            FORGED_TARGET,
            SHARE_PORT,
            PORT,
        ),
        (
            "stop-off-forged",
            vec!["ui", "stop", "--tailscale-off"],
            FORGED_TARGET,
            SHARE_PORT,
            PORT,
        ),
        (
            "tailscale-stop-production-port",
            vec!["ui", "tailscale", "stop"],
            "http://127.0.0.1:3010",
            SHARE_PORT,
            3010,
        ),
        (
            "stop-off-production-port",
            vec!["ui", "stop", "--tailscale-off"],
            "http://127.0.0.1:3010",
            SHARE_PORT,
            3010,
        ),
    ] {
        let fixture = Fixture::new(label);
        let record = format!(
            "{{\n  \"port\": {ui_port},\n  \"tailscale\": {{\n    \"dns_name\": \"acceptance.example.ts.net\",\n    \"https_port\": {port},\n    \"target\": \"{target}\"\n  }}\n}}\n"
        );
        fs::write(fixture.state.join("ui.json"), &record).unwrap();
        let config = mapping(port, target);
        fixture.api.seed(config.clone());
        let out = fixture.cli(&args);
        let after = fs::read(fixture.state.join("ui.json")).unwrap();
        let after_config = fixture.api.config();
        let requests = fixture.api.requests();
        if out.status.success()
            || after != record.as_bytes()
            || after_config != config
            || requests.iter().any(|(m, _, _)| m == "POST")
        {
            failures.push(format!(
                "{label}: status={} output={} record_unchanged={} api_unchanged={} requests={requests:?}",
                out.status,
                output_text(&out),
                after == record.as_bytes(),
                after_config == config,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "unsafe stop cases failed: {failures:#?}"
    );
}

#[test]
fn reset_refuses_known_ui_options_decode_failure_without_mutation() {
    let cases = [
        (
            "reset-malformed-known-option-with-share",
            format!(
                "{{\n  \"dist\": false,\n  \"port\": {PORT},\n  \"tailscale\": {{\n    \"dns_name\": \"acceptance.example.ts.net\",\n    \"https_port\": {SHARE_PORT},\n    \"target\": \"{TARGET}\"\n  }}\n}}\n"
            )
            .into_bytes(),
        ),
        ("reset-array-root", b"[]\n".to_vec()),
        ("reset-null-root", b"null\n".to_vec()),
        ("reset-scalar-root", b"17\n".to_vec()),
        (
            "reset-malformed-options-without-share",
            format!("{{\n  \"dist\": false,\n  \"port\": {PORT}\n}}\n").into_bytes(),
        ),
    ];
    let mut failures = Vec::new();
    for (label, record) in cases {
        let fixture = Fixture::new(label);
        let base = fixture._root.path().join("sandboxes");
        let root = base.join(label);
        let state = root.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            root.join(".cadence-sandbox"),
            format!(r#"{{"name":"{label}","allow_global":true}}"#),
        )
        .unwrap();
        fs::write(state.join("ui.json"), &record).unwrap();
        let config = mapping(SHARE_PORT, TARGET);
        fixture.api.seed(config.clone());
        let out = fixture.cli(&["sandbox", "reset", label]);
        let root_exists = root.is_dir();
        let after = fs::read(state.join("ui.json")).unwrap_or_default();
        let after_config = fixture.api.config();
        let requests = fixture.api.requests();
        if out.status.success()
            || !root_exists
            || after != record
            || after_config != config
            || requests.iter().any(|(m, _, _)| m == "POST")
        {
            failures.push(format!(
                "{label}: status={} output={} root_exists={root_exists} record_unchanged={} api_unchanged={} requests={requests:?}",
                out.status,
                output_text(&out),
                after == record,
                after_config == config,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "malformed reset ownership cases failed: {failures:#?}"
    );
}

#[test]
fn unsupported_localapi_serve_schema_refuses_without_post_or_data_loss() {
    let known_free_config = || {
        json!({
            "Web": {
                "known.example.ts.net:19449": {"Handlers": {"/": {"Text": "keep"}}}
            },
            "TCP": {
                "19449": {"HTTPS": true},
                "19451": {"TCPForward": "127.0.0.1:9040"}
            },
            "AllowFunnel": {"known.example.ts.net:19447": true}
        })
    };
    let mut cases = Vec::new();

    let mut future_root = known_free_config();
    future_root["FutureServe"] = json!({
        "TCP": {"19450": {"TCPForward": "127.0.0.1:3118"}}
    });
    cases.push(("future-root-field", future_root));

    let mut future_handler = known_free_config();
    future_handler["Web"]["known.example.ts.net:19449"]["Handlers"]["/"]["FutureHandler"] =
        json!({"mode": "preserve-me"});
    cases.push(("future-http-handler-field", future_handler));

    let mut failures = Vec::new();
    for (label, original_config) in cases {
        let fixture = Fixture::new(label);
        let target = format!("http://127.0.0.1:{BIND_TEST_PORT}");
        let record = format!(
            "{{\n  \"port\": {BIND_TEST_PORT},\n  \"tailscale\": {{\n    \"dns_name\": \"acceptance.example.ts.net\",\n    \"https_port\": {SHARE_PORT},\n    \"target\": \"{target}\"\n  }}\n}}\n"
        );
        fs::write(fixture.state.join("ui.json"), &record).unwrap();
        fixture.api.seed(original_config.clone());
        let out = fixture.cli(&["ui", "start"]);
        let after_record = fs::read(fixture.state.join("ui.json")).unwrap();
        let after_config = fixture.api.config();
        let requests = fixture.api.requests();
        let got_snapshot = requests
            .iter()
            .any(|(method, path, _)| method == "GET" && path == "/localapi/v0/serve-config");
        if out.status.success()
            || !got_snapshot
            || requests
                .iter()
                .any(|(method, path, _)| method == "POST" && path == "/localapi/v0/serve-config")
            || after_record != record.as_bytes()
            || after_config != original_config
        {
            failures.push(format!(
                "{label}: status={} output={} reached_snapshot={got_snapshot} record_unchanged={} config_unchanged={} requests={requests:?}",
                out.status,
                output_text(&out),
                after_record == record.as_bytes(),
                after_config == original_config,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "unsupported LocalAPI schema cases failed: {failures:#?}"
    );
}

const FAKE_TAILSCALE: &str = r##"#!/bin/sh
set -eu
# CADENCE_TAILSCALE_SOCKET is passed as the global Tailscale --socket flag;
# strip only that fixture-routing option and leave read-only CLI calls intact.
if [ "${1-}" = "--socket" ]; then shift 2; fi
printf '%s\n' "$*" >> "$TS_LOG"
if [ "${1-}" = "status" ] && [ "${2-}" = "--json" ]; then
  printf '%s\n' '{"BackendState":"Running","Self":{"DNSName":"acceptance.example.ts.net."},"CertDomains":["acceptance.example.ts.net"]}'
  exit 0
fi
if [ "${1-}" = "serve" ] && [ "${2-}" = "status" ] && [ "${3-}" = "--json" ]; then
  if [ -f "$TS_MAP" ]; then cat "$TS_MAP"; else printf '%s\n' '{"Web":{}}'; fi
  exit 0
fi
# Legacy `tailscale serve --bg` and `tailscale serve ... off` are forbidden:
# record the attempted command above, but never simulate a successful write.
if [ "${1-}" = "serve" ] && { [ "${2-}" = "--bg" ] || [ "${3-}" = "off" ]; }; then
  printf 'legacy serve mutation forbidden by acceptance fixture: %s\n' "$*" >&2
  exit 97
fi
printf 'unexpected fake tailscale invocation: %s\n' "$*" >&2
exit 2
"##;
