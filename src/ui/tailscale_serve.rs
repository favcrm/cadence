//! Compare-and-swap updates for Tailscale's local serve configuration.
//!
//! The LocalAPI is intentionally used only for writes and matching checks:
//! CLI status output is lossy and cannot establish ownership of a config that
//! will be mutated. The upstream endpoint is unstable, so every unexpected
//! shape or missing ETag fails closed.

use std::fmt;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};
use ureq::{Agent, Error as UreqError};

use crate::error::{Error, Result};

const API_URL: &str = "http://local-tailscaled.sock/localapi/v0/serve-config";
const MAX_CONFIG: usize = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const SOCKET_ENV: &str = "CADENCE_TAILSCALE_SOCKET";

#[derive(Debug)]
struct LocalApiResolver;

impl Resolver for LocalApiResolver {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: NextTimeout,
    ) -> std::result::Result<ResolvedSocketAddrs, UreqError> {
        // The connector uses only the Unix socket. Return a placeholder address
        // without consulting DNS; it is never opened as a network socket.
        Ok(self.empty())
    }
}

#[derive(Debug)]
struct UnixApiConnector;

#[derive(Debug)]
struct UnixTransport {
    stream: UnixStream,
    buffers: LazyBuffers,
}

impl Connector for UnixApiConnector {
    type Out = UnixTransport;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> std::result::Result<Option<Self::Out>, UreqError> {
        if details.uri.scheme_str() != Some("http")
            || details.uri.host() != Some("local-tailscaled.sock")
        {
            return Err(UreqError::BadUri(
                "unexpected Tailscale LocalAPI URI".into(),
            ));
        }
        let path = socket_path().map_err(UreqError::Io)?;
        let stream = connect_unix(&path).map_err(UreqError::Io)?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(UreqError::Io)?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(UreqError::Io)?;
        Ok(Some(UnixTransport {
            stream,
            buffers: LazyBuffers::new(8192, 8192),
        }))
    }
}

impl Transport for UnixTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: NextTimeout,
    ) -> std::result::Result<(), UreqError> {
        self.stream
            .set_write_timeout(Some(
                (*timeout.after)
                    .min(IO_TIMEOUT)
                    .max(Duration::from_millis(1)),
            ))
            .map_err(UreqError::Io)?;
        self.stream
            .write_all(&self.buffers.output()[..amount])
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::TimedOut {
                    UreqError::Timeout(timeout.reason)
                } else {
                    UreqError::Io(e)
                }
            })
    }

    fn await_input(&mut self, timeout: NextTimeout) -> std::result::Result<bool, UreqError> {
        self.stream
            .set_read_timeout(Some(
                (*timeout.after)
                    .min(IO_TIMEOUT)
                    .max(Duration::from_millis(1)),
            ))
            .map_err(UreqError::Io)?;
        let buf = self.buffers.input_append_buf();
        match self.stream.read(buf) {
            Ok(0) => Ok(false),
            Ok(n) => {
                self.buffers.input_appended(n);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                Err(UreqError::Timeout(timeout.reason))
            }
            Err(e) => Err(UreqError::Io(e)),
        }
    }

    fn is_open(&mut self) -> bool {
        true
    }
}

pub(crate) fn validate_socket_override() -> Result<()> {
    if std::env::var_os(SOCKET_ENV).is_some() {
        socket_path().map_err(api_error)?;
    }
    Ok(())
}

fn socket_path() -> std::io::Result<PathBuf> {
    if let Some(_value) = std::env::var_os(SOCKET_ENV) {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "explicit Tailscale Unix socket paths are supported only on Linux/macOS; {SOCKET_ENV} cannot be used on this platform"
            ),
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let path = PathBuf::from(_value);
            let bytes = path.as_os_str().as_bytes();
            let max = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
                .sun_path
                .len();
            if !path.is_absolute() || bytes.is_empty() || bytes.len() >= max || bytes.contains(&0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "{SOCKET_ENV} must be a non-empty, absolute Unix socket path within the platform length limit"
                    ),
                ));
            }
            return Ok(path);
        }
    }
    #[cfg(target_os = "linux")]
    {
        Ok(PathBuf::from("/var/run/tailscale/tailscaled.sock"))
    }
    #[cfg(target_os = "macos")]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "Tailscale LocalAPI has no configured default socket path on macOS; set {SOCKET_ENV} to an explicit absolute Unix socket path"
            ),
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "Tailscale LocalAPI socket is unsupported on this platform; supported: Linux default socket, or Linux/macOS with {SOCKET_ENV} set to an explicit absolute Unix socket path"
            ),
        ))
    }
}

// Nonblocking connect avoids hanging indefinitely if a local socket backlog is
// saturated. EAGAIN is a bounded refusal; we never fall back to TCP or CLI writes.
fn connect_unix(path: &std::path::Path) -> std::io::Result<UnixStream> {
    use std::os::unix::io::IntoRawFd;
    let bytes = path.as_os_str().as_bytes();
    let max = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
        .sun_path
        .len();
    if bytes.is_empty() || bytes.len() >= max || bytes.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid Unix socket path length",
        ));
    }
    // Linux sets close-on-exec atomically, avoiding an inherited descriptor
    // if another thread spawns while the socket is being configured. macOS
    // lacks these socket flags, so the checked fcntl setup below is required.
    #[cfg(target_os = "linux")]
    let socket_type = libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
    #[cfg(not(target_os = "linux"))]
    let socket_type = libc::SOCK_STREAM;
    let fd = unsafe { libc::socket(libc::AF_UNIX, socket_type, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let fd_flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFD) };
    if fd_flags < 0
        || unsafe {
            libc::fcntl(
                owned.as_raw_fd(),
                libc::F_SETFD,
                fd_flags | libc::FD_CLOEXEC,
            )
        } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let status_flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFL) };
    if status_flags < 0
        || unsafe {
            libc::fcntl(
                owned.as_raw_fd(),
                libc::F_SETFL,
                status_flags | libc::O_NONBLOCK,
            )
        } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    let path_offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
    #[cfg(target_vendor = "apple")]
    let addr_len = path_offset + bytes.len();
    #[cfg(not(target_vendor = "apple"))]
    let addr_len = path_offset + bytes.len() + 1;
    let len = addr_len as libc::socklen_t;
    #[cfg(target_vendor = "apple")]
    {
        addr.sun_len = len as u8;
    }
    let rc = unsafe {
        libc::connect(
            owned.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            len,
        )
    };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(e);
        }
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut pollfd = libc::pollfd {
            fd: owned.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let ms = CONNECT_TIMEOUT.as_millis().min(i32::MAX as u128) as i32;
        let rc = unsafe { libc::poll(&mut pollfd, 1, ms) };
        if rc <= 0 || Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Tailscale LocalAPI socket connect timed out",
            ));
        }
        let mut err = 0;
        let mut n = std::mem::size_of_val(&err) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                owned.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut err as *mut _ as *mut libc::c_void,
                &mut n,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if err != 0 {
            return Err(std::io::Error::from_raw_os_error(err));
        }
    }
    let fd = owned.into_raw_fd();
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn agent() -> Agent {
    let config = ureq::config::Config::builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(IO_TIMEOUT))
        .build();
    Agent::with_parts(config, UnixApiConnector, LocalApiResolver)
}

fn api_error(e: impl fmt::Display) -> Error {
    Error::rejected(format!(
        "Tailscale LocalAPI serve-config failed closed: {e}"
    ))
}

fn request_error(e: UreqError) -> Error {
    let offline = match &e {
        UreqError::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::AddrNotAvailable
                | std::io::ErrorKind::BrokenPipe
        ),
        UreqError::Timeout(_) => true,
        _ => false,
    };
    if offline {
        Error::rejected(format!("tailscale LocalAPI offline: {e}"))
    } else {
        api_error(e)
    }
}

fn snapshot() -> Result<(Value, String)> {
    let mut response = agent().get(API_URL).call().map_err(request_error)?;
    if response.status().as_u16() != 200 {
        return Err(api_error(format!(
            "GET returned HTTP {}",
            response.status()
        )));
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .trim()
        .to_owned();
    if etag.is_empty() {
        return Err(api_error(
            "GET response omitted a usable ETag; no update was attempted",
        ));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_CONFIG as u64)
        .read_to_vec()
        .map_err(api_error)?;
    let value: Value = serde_json::from_slice(&body).map_err(api_error)?;
    if value.is_null() {
        return Ok((json!({}), etag));
    }
    validate_serve_config(&value, false)?;
    Ok((value, etag))
}

fn post(value: &Value, etag: &str) -> Result<()> {
    let body = serde_json::to_vec(value).map_err(api_error)?;
    if body.len() > MAX_CONFIG {
        return Err(api_error("updated serve-config exceeds 1 MiB limit"));
    }
    let response = agent()
        .post(API_URL)
        .header("If-Match", etag)
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|e| match e {
            UreqError::StatusCode(412) => api_error(
                "POST returned HTTP 412 (stale ETag); the server refused this update; inspect current serve config before retrying",
            ),
            other => api_error(format!(
                "POST result was not acknowledged ({other}); inspect current serve config before retrying"
            )),
        })?;
    if response.status().as_u16() != 200 {
        return Err(api_error(format!(
            "POST result was not acknowledged (HTTP {}); inspect current serve config before retrying",
            response.status()
        )));
    }
    Ok(())
}

fn host_key(dns: &str, port: u16) -> String {
    let dns = dns.trim_end_matches('.');
    format!("{dns}:{port}")
}

fn web_key_port(key: &str) -> Option<u16> {
    let (host, port) = key.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    if host.contains('[') || host.contains(']') {
        if !(host.starts_with('[') && host.ends_with(']')) {
            return None;
        }
        host.strip_prefix('[')?
            .strip_suffix(']')?
            .parse::<std::net::Ipv6Addr>()
            .ok()?;
    } else if host.contains(':') {
        return None;
    }
    port.parse().ok()
}

fn reject_unknown_fields(value: &Value, allowed: &[&str], context: &str) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| api_error(format!("serve-config {context} is malformed")))?;
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(api_error(format!(
            "serve-config {context} contains unsupported field {field:?}"
        )));
    }
    Ok(())
}

fn validate_tcp_map(value: &Value) -> Result<()> {
    let entries = value
        .as_object()
        .ok_or_else(|| api_error("serve-config TCP field is malformed"))?;
    for (port, handler) in entries {
        port.parse::<u16>()
            .map_err(|_| api_error("serve-config TCP contains a malformed port key"))?;
        if handler.is_null() {
            continue;
        }
        reject_unknown_fields(
            handler,
            &[
                "HTTPS",
                "HTTP",
                "TCPForward",
                "TerminateTLS",
                "ProxyProtocol",
            ],
            "TCP port handler",
        )?;
        let fields = handler.as_object().expect("validated object");
        for field in ["HTTPS", "HTTP"] {
            if fields.get(field).is_some_and(|value| !value.is_boolean()) {
                return Err(api_error(format!("serve-config TCP {field} is malformed")));
            }
        }
        for field in ["TCPForward", "TerminateTLS"] {
            if fields.get(field).is_some_and(|value| !value.is_string()) {
                return Err(api_error(format!("serve-config TCP {field} is malformed")));
            }
        }
        if fields
            .get("ProxyProtocol")
            .is_some_and(|value| value.as_i64().is_none())
        {
            return Err(api_error("serve-config TCP ProxyProtocol is malformed"));
        }
    }
    Ok(())
}

fn validate_web_map(value: &Value) -> Result<()> {
    let entries = value
        .as_object()
        .ok_or_else(|| api_error("serve-config Web field is malformed"))?;
    for (key, server) in entries {
        if web_key_port(key).is_none() {
            return Err(api_error(
                "serve-config Web contains a malformed HostPort key",
            ));
        }
        if server.is_null() {
            continue;
        }
        reject_unknown_fields(server, &["Handlers"], "web server config")?;
        if let Some(handlers) = server.get("Handlers") {
            let handlers = handlers
                .as_object()
                .ok_or_else(|| api_error("serve-config Web Handlers field is malformed"))?;
            for handler in handlers.values().filter(|handler| !handler.is_null()) {
                reject_unknown_fields(
                    handler,
                    &["Path", "Proxy", "Text", "AcceptAppCaps", "Redirect"],
                    "HTTP handler",
                )?;
                let fields = handler.as_object().expect("validated object");
                for field in ["Path", "Proxy", "Text", "Redirect"] {
                    if fields.get(field).is_some_and(|value| !value.is_string()) {
                        return Err(api_error(format!("serve-config HTTP {field} is malformed")));
                    }
                }
                if fields.get("AcceptAppCaps").is_some_and(|value| {
                    value
                        .as_array()
                        .is_none_or(|caps| caps.iter().any(|cap| !cap.is_string()))
                }) {
                    return Err(api_error("serve-config HTTP AcceptAppCaps is malformed"));
                }
            }
        }
    }
    Ok(())
}

fn validate_serve_config(cfg: &Value, foreground: bool) -> Result<()> {
    reject_unknown_fields(
        cfg,
        &["TCP", "Web", "Services", "AllowFunnel", "Foreground"],
        "root config",
    )?;
    if let Some(tcp) = cfg.get("TCP") {
        validate_tcp_map(tcp)?;
    }
    if let Some(web) = cfg.get("Web") {
        validate_web_map(web)?;
    }
    if let Some(funnel) = cfg.get("AllowFunnel") {
        let entries = funnel
            .as_object()
            .ok_or_else(|| api_error("serve-config AllowFunnel field is malformed"))?;
        for (key, enabled) in entries {
            if web_key_port(key).is_none() || !enabled.is_boolean() {
                return Err(api_error("serve-config AllowFunnel entries are malformed"));
            }
        }
    }
    if let Some(services) = cfg.get("Services") {
        let entries = services
            .as_object()
            .ok_or_else(|| api_error("serve-config Services field is malformed"))?;
        for service in entries.values().filter(|service| !service.is_null()) {
            reject_unknown_fields(service, &["TCP", "Web", "Tun"], "service config")?;
            if let Some(tcp) = service.get("TCP") {
                validate_tcp_map(tcp)?;
            }
            if let Some(web) = service.get("Web") {
                validate_web_map(web)?;
            }
            if service.get("Tun").is_some_and(|value| !value.is_boolean()) {
                return Err(api_error("serve-config service Tun field is malformed"));
            }
        }
    }
    if let Some(foreground_config) = cfg.get("Foreground") {
        if foreground {
            return Err(api_error("nested serve-config Foreground is unsupported"));
        }
        let entries = foreground_config
            .as_object()
            .ok_or_else(|| api_error("serve-config Foreground field is malformed"))?;
        for config in entries.values().filter(|config| !config.is_null()) {
            validate_serve_config(config, true)?;
        }
    }
    Ok(())
}

fn validate_host_port_keys(cfg: &Value) -> Result<()> {
    if cfg.get("TCP").is_some_and(|tcp| !tcp.is_object()) {
        return Err(api_error("serve-config TCP field is malformed"));
    }
    for field in ["Web", "AllowFunnel"] {
        if let Some(entries) = cfg.get(field) {
            let entries = entries
                .as_object()
                .ok_or_else(|| api_error(format!("serve-config {field} field is malformed")))?;
            if entries.keys().any(|key| web_key_port(key).is_none()) {
                return Err(api_error(format!(
                    "serve-config {field} contains a malformed HostPort key"
                )));
            }
            if field == "AllowFunnel" && entries.values().any(|value| !value.is_boolean()) {
                return Err(api_error("serve-config AllowFunnel values are malformed"));
            }
        }
    }
    for field in ["Services", "Foreground"] {
        if let Some(entries) = cfg.get(field) {
            let entries = entries
                .as_object()
                .ok_or_else(|| api_error(format!("serve-config {field} field is malformed")))?;
            for value in entries.values().filter(|value| !value.is_null()) {
                if !value.is_object() {
                    return Err(api_error(format!(
                        "serve-config {field} contains a malformed entry"
                    )));
                }
                validate_host_port_keys(value)?;
            }
        }
    }
    Ok(())
}

fn config_has_port(cfg: &Value, port: u16) -> bool {
    cfg.get("TCP")
        .and_then(Value::as_object)
        .is_some_and(|m| m.contains_key(&port.to_string()))
        || cfg
            .get("Web")
            .and_then(Value::as_object)
            .is_some_and(|m| m.keys().any(|key| web_key_port(key) == Some(port)))
        || cfg
            .get("Services")
            .and_then(Value::as_object)
            .is_some_and(|m| m.values().any(|v| config_has_port(v, port)))
        || cfg
            .get("Foreground")
            .and_then(Value::as_object)
            .is_some_and(|m| m.values().any(|v| config_has_port(v, port)))
}

fn own_state(cfg: &Value, dns: &str, port: u16, expected: &str) -> Result<bool> {
    validate_host_port_keys(cfg)?;
    let host = host_key(dns, port);
    let tcp_key = port.to_string();
    for key in ["TCP", "Web", "Services", "Foreground", "AllowFunnel"] {
        if cfg.get(key).is_some_and(|v| !v.is_object()) {
            return Err(api_error(format!("serve-config {key} field is malformed")));
        }
    }
    let funnel_port = cfg
        .get("AllowFunnel")
        .and_then(Value::as_object)
        .is_some_and(|m| m.keys().any(|k| web_key_port(k) == Some(port)));
    let services_port = cfg
        .get("Services")
        .and_then(Value::as_object)
        .is_some_and(|m| m.values().any(|v| config_has_port(v, port)));
    let foreground_port = cfg
        .get("Foreground")
        .and_then(Value::as_object)
        .is_some_and(|m| m.values().any(|v| config_has_port(v, port)));
    if funnel_port || services_port || foreground_port {
        return Err(api_error(format!(
            "port {port} collides with a Funnel, service, or foreground serve entry"
        )));
    }
    let tcp = cfg.get("TCP").and_then(Value::as_object);
    let web = cfg.get("Web").and_then(Value::as_object);
    let tcp_port_keys: Vec<_> = tcp
        .into_iter()
        .flat_map(|m| m.keys())
        .filter(|key| key.parse::<u16>().ok() == Some(port))
        .collect();
    let web_port_keys: Vec<_> = web
        .into_iter()
        .flat_map(|m| m.keys())
        .filter(|key| web_key_port(key) == Some(port))
        .collect();
    if tcp_port_keys.is_empty() && web_port_keys.is_empty() {
        return Ok(false);
    }
    if tcp_port_keys.len() != 1
        || tcp_port_keys[0].as_str() != tcp_key
        || web_port_keys.len() != 1
        || web_port_keys[0].as_str() != host
    {
        return Err(api_error(format!(
            "port {port} has multiple or noncanonical TCP/Web entries; refusing to alter shared serve config"
        )));
    }
    let tcp_entry = tcp.and_then(|m| m.get(&tcp_key));
    let web_entry = web.and_then(|m| m.get(&host));
    let canonical_tcp = tcp_entry.is_some_and(|v| {
        v.as_object()
            .is_some_and(|m| m.len() == 1 && m.get("HTTPS") == Some(&Value::Bool(true)))
    });
    let canonical_web = web_entry.is_some_and(|v| {
        v.as_object().is_some_and(|web_config| {
            web_config.len() == 1
                && web_config.get("Handlers").is_some_and(|handlers| {
                    handlers.as_object().is_some_and(|handlers| {
                        handlers.len() == 1
                            && handlers.get("/").is_some_and(|root| {
                                root.as_object().is_some_and(|root| {
                                    root.len() == 1
                                        && root.get("Proxy").and_then(Value::as_str)
                                            == Some(expected)
                                })
                            })
                    })
                })
        })
    });
    if canonical_tcp && canonical_web {
        return Ok(true);
    }
    Err(api_error(format!(
        "port {port} is occupied by a foreign, noncanonical, or incomplete serve-config entry; refusing to alter it"
    )))
}

pub(crate) fn matches(dns: &str, port: u16, target: &str) -> Result<bool> {
    let (cfg, _) = snapshot()?;
    own_state(&cfg, dns, port, target)
}

pub(crate) struct PreparedUpdate {
    value: Option<Value>,
    etag: String,
}

impl PreparedUpdate {
    pub(crate) fn apply(self) -> Result<bool> {
        let Some(value) = self.value else {
            return Ok(false);
        };
        post(&value, &self.etag)?;
        Ok(true)
    }
}

pub(crate) fn prepare_ensure(dns: &str, port: u16, target: &str) -> Result<PreparedUpdate> {
    let (mut cfg, etag) = snapshot()?;
    if own_state(&cfg, dns, port, target)? {
        return Ok(PreparedUpdate { value: None, etag });
    }
    if !target.starts_with("http://127.0.0.1:") {
        return Err(api_error(
            "proxy target is not the expected loopback UI target",
        ));
    }
    let port = port.to_string();
    let host = host_key(dns, port.parse().expect("u16"));
    let root = cfg.as_object_mut().expect("snapshot returns object");
    root.entry("TCP")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| api_error("TCP is not an object"))?
        .insert(port, json!({"HTTPS": true}));
    root.entry("Web")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| api_error("Web is not an object"))?
        .insert(host, json!({"Handlers": {"/": {"Proxy": target}}}));
    Ok(PreparedUpdate {
        value: Some(cfg),
        etag,
    })
}

pub(crate) fn remove(dns: &str, port: u16, expected: &str) -> Result<bool> {
    let (mut cfg, etag) = snapshot()?;
    if !own_state(&cfg, dns, port, expected)? {
        return Ok(false);
    }
    let root = cfg.as_object_mut().expect("snapshot returns object");
    if let Some(tcp) = root.get_mut("TCP").and_then(Value::as_object_mut) {
        tcp.remove(&port.to_string());
        if tcp.is_empty() {
            root.remove("TCP");
        }
    }
    if let Some(web) = root.get_mut("Web").and_then(Value::as_object_mut) {
        web.remove(&host_key(dns, port));
        if web.is_empty() {
            root.remove("Web");
        }
    }
    post(&cfg, &etag)?;
    Ok(true)
}
