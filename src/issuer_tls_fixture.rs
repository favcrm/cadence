//! CAD-1434 / AOS-243: test-only TLS loopback fixture for the hosted issuer.
//!
//! The real renewal and revoke POSTs run through the production guards and the
//! production `send_issuer_request` path. Only the agent's TLS roots and
//! resolver differ, and only under `cfg(test)`, via [`crate::issuer_transport`].
//! Nothing here disables certificate verification.
//!
//! The CA and the server certificate are created by host `openssl` in a private
//! 0700 temp dir. Key files are 0600. openssl's stdout/stderr are captured and
//! never printed, so no key or secret reaches a log. Every exit status is
//! checked. The temp dir is removed when the fixture drops.

#![cfg(test)]

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

/// The one hostname the fixture certificate is valid for.
pub(crate) const HOST: &str = "issuer.board.localhost";

/// Owns the temp PKI directory, the loopback TLS server and its thread.
pub(crate) struct TlsIssuer {
    _dir: tempfile::TempDir,
    ca_der: Vec<u8>,
    host: String,
    addr: SocketAddr,
    server: Option<JoinHandle<()>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    seen: Arc<std::sync::Mutex<Vec<Seen>>>,
}

/// One request the fixture observed before answering. Only headers that the
/// test asserts on are retained; bodies are not logged.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub(crate) path: String,
    pub(crate) header_names: Vec<String>,
}

fn run(cmd: &mut Command, what: &str) {
    let out = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("openssl {what}: spawn failed: {e}"));
    // Never echo stdout/stderr: they can contain key material.
    assert!(
        out.status.success(),
        "openssl {what} failed (status {:?})",
        out.status.code()
    );
}

impl TlsIssuer {
    /// Create an ephemeral CA and a server certificate for [`HOST`], then start
    /// a loopback TLS server that answers `answer(path)` for each request.
    pub(crate) fn start(answer: impl Fn(&str) -> (u16, String) + Send + 'static) -> Self {
        Self::start_with_headers(move |p| {
            let (s, b) = answer(p);
            (s, String::new(), b)
        })
    }

    /// Like `start`, but the answer also supplies extra response header lines
    /// (each already `Name: value\r\n`), sent before the body.
    pub(crate) fn start_with_headers(
        answer: impl Fn(&str) -> (u16, String, String) + Send + 'static,
    ) -> Self {
        Self::start_for_host(HOST, answer)
    }

    /// Like `start_with_headers`, but the server certificate is valid for
    /// `host` (the native chain's bridge host). Name verification stays on.
    pub(crate) fn start_for_host(
        host: &str,
        answer: impl Fn(&str) -> (u16, String, String) + Send + 'static,
    ) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("cad1434-tls-")
            .tempdir()
            .expect("temp dir");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let p = |name: &str| dir.path().join(name);
        let ca_key = p("ca.key");
        let ca_pem = p("ca.pem");
        let srv_key = p("srv.key");
        let srv_csr = p("srv.csr");
        let srv_pem = p("srv.pem");
        let ext = p("ext.cnf");

        // Ephemeral CA (valid 1 day).
        run(
            Command::new("openssl")
                .args(["req", "-x509", "-newkey", "ec", "-pkeyopt"])
                .args(["ec_paramgen_curve:prime256v1", "-nodes", "-days", "1"])
                .arg("-subj")
                .arg("/CN=cad1434-test-ca")
                .arg("-keyout")
                .arg(&ca_key)
                .arg("-out")
                .arg(&ca_pem),
            "ca",
        );
        // Server certificate for `host` only, signed by the CA.
        fs::write(
            &ext,
            format!("subjectAltName=DNS:{host}\nbasicConstraints=CA:FALSE\n"),
        )
        .unwrap();
        run(
            Command::new("openssl")
                .args(["req", "-newkey", "ec", "-pkeyopt"])
                .args(["ec_paramgen_curve:prime256v1", "-nodes"])
                .arg("-subj")
                .arg(format!("/CN={host}"))
                .arg("-keyout")
                .arg(&srv_key)
                .arg("-out")
                .arg(&srv_csr),
            "server csr",
        );
        run(
            Command::new("openssl")
                .args(["x509", "-req", "-days", "1"])
                .arg("-in")
                .arg(&srv_csr)
                .arg("-CA")
                .arg(&ca_pem)
                .arg("-CAkey")
                .arg(&ca_key)
                .args(["-CAcreateserial"])
                .arg("-extfile")
                .arg(&ext)
                .arg("-out")
                .arg(&srv_pem),
            "server sign",
        );
        for key in [&ca_key, &srv_key] {
            fs::set_permissions(key, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let ca_der = pem_to_der(&ca_pem, "CERTIFICATE");
        let server_chain = vec![CertificateDer::from(pem_to_der(&srv_pem, "CERTIFICATE"))];
        let server_key: PrivateKeyDer<'static> = {
            let mut reader = std::io::BufReader::new(fs::File::open(&srv_key).unwrap());
            rustls_pemfile::private_key(&mut reader)
                .expect("server key parse")
                .expect("server key present")
        };
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(server_chain, server_key)
            .expect("server config");
        let config = Arc::new(config);

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback bind");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let server = {
            let (stop, seen) = (stop.clone(), seen.clone());
            thread::spawn(move || serve(listener, config, answer, stop, seen))
        };
        Self {
            _dir: dir,
            ca_der,
            host: host.to_string(),
            addr,
            server: Some(server),
            stop,
            seen,
        }
    }

    /// The fixture CA, DER, for the agent trust store.
    pub(crate) fn ca_der(&self) -> &[u8] {
        &self.ca_der
    }

    /// The loopback socket the synthetic `HOST` resolves to.
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The `https://<host>:port` origin the CLI must be pinned to.
    pub(crate) fn origin(&self) -> String {
        format!("https://{}:{}", self.host, self.addr.port())
    }

    /// Requests the fixture has answered, in order.
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for TlsIssuer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.server.take() {
            let _ = handle.join();
        }
    }
}

fn pem_to_der(path: &Path, label: &str) -> Vec<u8> {
    let text = fs::read_to_string(path).expect("pem read");
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let body: String = text
        .lines()
        .skip_while(|l| *l != begin)
        .skip(1)
        .take_while(|l| *l != end)
        .collect();
    base64_decode(&body)
}

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .expect("valid base64 PEM body")
}

fn serve(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    answer: impl Fn(&str) -> (u16, String, String),
    stop: Arc<std::sync::atomic::AtomicBool>,
    seen: Arc<std::sync::Mutex<Vec<Seen>>>,
) {
    use std::sync::atomic::Ordering::SeqCst;
    while !stop.load(SeqCst) {
        match listener.accept() {
            Ok((socket, _)) => {
                let _ = socket.set_nonblocking(false);
                handle(socket, &config, &answer, &seen);
            }
            Err(_) => thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

fn handle(
    mut socket: TcpStream,
    config: &Arc<ServerConfig>,
    answer: &impl Fn(&str) -> (u16, String, String),
    seen: &Arc<std::sync::Mutex<Vec<Seen>>>,
) {
    let _ = socket.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let Ok(mut conn) = rustls::ServerConnection::new(config.clone()) else {
        return;
    };
    let mut tls = rustls::Stream::new(&mut conn, &mut socket);
    // Read the request head, then the declared body, all inside TLS.
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        match tls.read(&mut byte) {
            Ok(0) | Err(_) => return,
            Ok(_) => buf.push(byte[0]),
        }
        if buf.len() > 16 * 1024 {
            return;
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let header_names: Vec<String> = head
        .lines()
        .skip(1)
        .filter_map(|l| {
            l.split_once(':')
                .map(|(n, _)| n.trim().to_ascii_lowercase())
        })
        .collect();
    let len: usize = head
        .lines()
        .find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0u8; len.min(64 * 1024)];
    let _ = tls.read_exact(&mut body);
    seen.lock().unwrap().push(Seen {
        path: path.clone(),
        header_names,
    });
    let (status, extra, payload) = answer(&path);
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = tls.write_all(response.as_bytes());
    let _ = tls.flush();
    conn.send_close_notify();
    let _ = conn.complete_io(&mut socket);
}
