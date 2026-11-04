//! CAD-1429 test-only TLS fixture for the Publish Web issuer calls.
//!
//! Compiled only under `cfg(test)`. Production builds never reference this
//! module: `remote_enrollment::issuer_agent` is `new_with_config(builder.build())`
//! outside tests. While nothing is installed, the test branch is that same
//! construction, so guard, TLS and webpki behavior are unchanged.
//!
//! `install` swaps in ONE virtual issuer, `https://web-publish-issuer.test`
//! (no explicit port). Its host resolves only to the caller's `127.0.0.1`
//! ephemeral socket, and the agent trusts only the caller's disposable CA. Every
//! other host or port fails to resolve. Redirects stay refused by the
//! production builder, and no certificate verification is disabled.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use ureq::config::ConfigBuilder;
use ureq::http::Uri;
use ureq::tls::{Certificate, RootCerts, TlsConfig};
use ureq::typestate::AgentScope;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// The only issuer origin the fixture ever maps.
pub(crate) const ISSUER: &str = "https://web-publish-issuer.test";
const HOST: &str = "web-publish-issuer.test";

struct Installed {
    loopback: SocketAddr,
    tls: TlsConfig,
}

static INSTALLED: Mutex<Option<Installed>> = Mutex::new(None);

fn slot() -> std::sync::MutexGuard<'static, Option<Installed>> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// Maps exactly the virtual issuer host to the fixture socket; everything
/// else is `HostNotFound`.
#[derive(Debug)]
struct VirtualIssuer(SocketAddr);

impl Resolver for VirtualIssuer {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &ureq::config::Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let exact = uri.scheme_str() == Some("https")
            && uri.host() == Some(HOST)
            && uri.port_u16().is_none();
        if !exact {
            return Err(ureq::Error::HostNotFound);
        }
        let mut addrs = self.empty();
        addrs.push(self.0);
        Ok(addrs)
    }
}

/// Keeps the fixture installed; dropping it restores the production path.
#[must_use]
pub(crate) struct TestWebIssuerTlsGuard;

impl Drop for TestWebIssuerTlsGuard {
    fn drop(&mut self) {
        *slot() = None;
    }
}

/// Test-only fixture entry point.
pub(crate) struct TestWebIssuerTls;

impl TestWebIssuerTls {
    /// Installs the virtual issuer. `issuer` must be exactly
    /// [`ISSUER`], `loopback` a non-zero `127.0.0.1` socket, and `ca_pem` a
    /// PEM certificate that becomes the only trusted root. Refused while another
    /// fixture is installed (tests that install must run serially).
    pub(crate) fn install(
        issuer: &str,
        loopback: SocketAddr,
        ca_pem: &[u8],
    ) -> Result<TestWebIssuerTlsGuard, String> {
        if issuer != ISSUER {
            return Err("test issuer must be the exact virtual origin".into());
        }
        if loopback.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || loopback.port() == 0 {
            return Err("test loopback must be a non-zero 127.0.0.1 socket".into());
        }
        let ca = Certificate::from_pem(ca_pem).map_err(|_| "test CA is not PEM".to_string())?;
        let tls = TlsConfig::builder()
            .root_certs(RootCerts::new_with_certs(&[ca]))
            .build();
        let mut installed = slot();
        if installed.is_some() {
            return Err("test issuer TLS fixture is already installed".into());
        }
        *installed = Some(Installed { loopback, tls });
        Ok(TestWebIssuerTlsGuard)
    }
}

/// The cfg(test) branch of `remote_enrollment::issuer_agent`. With no fixture
/// installed this is byte-for-byte the production construction.
pub(crate) fn agent(builder: ConfigBuilder<AgentScope>) -> ureq::Agent {
    match slot().as_ref() {
        None => ureq::Agent::new_with_config(builder.build()),
        Some(installed) => ureq::Agent::with_parts(
            builder.tls_config(installed.tls.clone()).build(),
            DefaultConnector::default(),
            VirtualIssuer(installed.loopback),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{BufReader, Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls::{ServerConfig, ServerConnection, StreamOwned};

    /// Installs mutate process-global state, so these tests never overlap.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Disposable PKI in a fresh 0700 directory outside the tree. Keys are
    /// chmod 600 and the directory is removed on drop; nothing is printed.
    struct Pki {
        dir: PathBuf,
    }

    impl Pki {
        fn generate() -> Pki {
            static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let dir = std::env::temp_dir()
                .join(format!("cad1429-web-issuer-tls-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            let pki = Pki { dir };
            pki.ca("ca", "CAD-1429 test CA");
            pki.ca("other", "CAD-1429 other test CA");
            pki.leaf("leaf", "web-publish-issuer.test", "ca");
            pki.leaf("wrong", "wrong-name.test", "ca");
            for key in ["ca.key", "other.key", "leaf.key", "wrong.key"] {
                fs::set_permissions(pki.path(key), fs::Permissions::from_mode(0o600)).unwrap();
            }
            pki
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }

        fn read(&self, name: &str) -> Vec<u8> {
            fs::read(self.path(name)).unwrap()
        }

        fn ca(&self, stem: &str, subject: &str) {
            let subject = format!("/CN={subject}");
            openssl(
                &self.dir,
                &[
                    "req",
                    "-x509",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-256",
                    "-nodes",
                    "-keyout",
                    &format!("{stem}.key"),
                    "-out",
                    &format!("{stem}.pem"),
                    "-days",
                    "2",
                    "-subj",
                    &subject,
                    "-addext",
                    "basicConstraints=critical,CA:TRUE",
                    "-addext",
                    "keyUsage=critical,keyCertSign,cRLSign",
                ],
            );
        }

        fn leaf(&self, stem: &str, san: &str, ca: &str) {
            fs::write(
                self.path(&format!("{stem}.ext")),
                format!(
                    "subjectAltName=DNS:{san}\nbasicConstraints=CA:FALSE\n\
                     keyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n"
                ),
            )
            .unwrap();
            openssl(
                &self.dir,
                &[
                    "req",
                    "-new",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-256",
                    "-nodes",
                    "-keyout",
                    &format!("{stem}.key"),
                    "-out",
                    &format!("{stem}.csr"),
                    "-subj",
                    "/CN=cad-1429-leaf",
                ],
            );
            openssl(
                &self.dir,
                &[
                    "x509",
                    "-req",
                    "-in",
                    &format!("{stem}.csr"),
                    "-CA",
                    &format!("{ca}.pem"),
                    "-CAkey",
                    &format!("{ca}.key"),
                    "-CAcreateserial",
                    "-days",
                    "2",
                    "-extfile",
                    &format!("{stem}.ext"),
                    "-out",
                    &format!("{stem}.pem"),
                ],
            );
        }

        /// Serves `stem`'s chain and key on a fresh loopback TLS listener that
        /// answers up to three connections with `response`.
        fn serve(&self, stem: &str, response: &'static str) -> SocketAddr {
            let mut chain_reader =
                BufReader::new(fs::File::open(self.path(&format!("{stem}.pem"))).unwrap());
            let chain: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut chain_reader)
                .collect::<Result<_, _>>()
                .unwrap();
            let mut key_reader =
                BufReader::new(fs::File::open(self.path(&format!("{stem}.key"))).unwrap());
            let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
                .unwrap()
                .unwrap();
            let config = Arc::new(
                ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .unwrap(),
            );
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            thread::spawn(move || {
                for stream in listener.incoming().take(3) {
                    let Ok(tcp) = stream else { continue };
                    let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
                    let Ok(conn) = ServerConnection::new(config.clone()) else {
                        continue;
                    };
                    let mut tls = StreamOwned::new(conn, tcp);
                    let mut seen = Vec::new();
                    let mut buf = [0_u8; 1024];
                    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tls.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                    let _ = tls.write_all(response.as_bytes());
                    let _ = tls.flush();
                }
            });
            addr
        }
    }

    impl Drop for Pki {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// Runs `openssl` with output discarded, so keys never reach the log.
    fn openssl(dir: &Path, args: &[&str]) {
        let status = Command::new("openssl")
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("openssl is required for the CAD-1429 TLS fixture");
        assert!(status.success(), "openssl fixture step failed");
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
        Content-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}";
    const REDIRECT: &str = "HTTP/1.1 302 Found\r\nLocation: https://evil.test/\r\n\
        Content-Length: 0\r\nConnection: close\r\n\r\n";

    fn client() -> ureq::Agent {
        agent(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(10)))
                .http_status_as_error(false)
                .max_redirects(0)
                .proxy(None),
        )
    }

    #[test]
    fn positive_real_chain_for_the_virtual_issuer_succeeds() {
        let _serial = serial();
        let pki = Pki::generate();
        let server = pki.serve("leaf", OK);
        let _guard = TestWebIssuerTls::install(ISSUER, server, &pki.read("ca.pem")).unwrap();
        let mut response = client()
            .get(format!("{ISSUER}/v1/web-sites/probe"))
            .call()
            .expect("valid SAN and trusted CA must connect");
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.body_mut().read_to_string().unwrap(),
            "{\"ok\":true}"
        );
    }

    #[test]
    fn wrong_ca_fails_closed() {
        let _serial = serial();
        let pki = Pki::generate();
        let server = pki.serve("leaf", OK);
        let _guard = TestWebIssuerTls::install(ISSUER, server, &pki.read("other.pem")).unwrap();
        assert!(client()
            .get(format!("{ISSUER}/v1/web-sites/probe"))
            .call()
            .is_err());
    }

    #[test]
    fn wrong_name_certificate_fails_closed() {
        let _serial = serial();
        let pki = Pki::generate();
        let server = pki.serve("wrong", OK);
        let _guard = TestWebIssuerTls::install(ISSUER, server, &pki.read("ca.pem")).unwrap();
        assert!(client()
            .get(format!("{ISSUER}/v1/web-sites/probe"))
            .call()
            .is_err());
    }

    #[test]
    fn other_origins_and_explicit_ports_are_not_mapped() {
        let _serial = serial();
        let pki = Pki::generate();
        let server = pki.serve("leaf", OK);
        let _guard = TestWebIssuerTls::install(ISSUER, server, &pki.read("ca.pem")).unwrap();
        for url in [
            "https://other-issuer.test/v1/web-sites/probe",
            "https://web-publish-issuer.test:8443/v1/web-sites/probe",
            "http://web-publish-issuer.test/v1/web-sites/probe",
        ] {
            assert!(client().get(url).call().is_err(), "{url} must not resolve");
        }
    }

    #[test]
    fn redirects_are_returned_not_followed() {
        let _serial = serial();
        let pki = Pki::generate();
        let server = pki.serve("leaf", REDIRECT);
        let _guard = TestWebIssuerTls::install(ISSUER, server, &pki.read("ca.pem")).unwrap();
        let response = client()
            .get(format!("{ISSUER}/v1/web-sites/probe"))
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 302);
    }

    #[test]
    fn install_refuses_anything_but_the_exact_fixture() {
        let _serial = serial();
        let pki = Pki::generate();
        let ca = pki.read("ca.pem");
        let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();
        assert!(TestWebIssuerTls::install("https://other.test", loopback, &ca).is_err());
        assert!(
            TestWebIssuerTls::install("https://web-publish-issuer.test:443", loopback, &ca)
                .is_err()
        );
        assert!(TestWebIssuerTls::install(ISSUER, "192.0.2.1:443".parse().unwrap(), &ca).is_err());
        assert!(TestWebIssuerTls::install(ISSUER, "127.0.0.1:0".parse().unwrap(), &ca).is_err());
        assert!(TestWebIssuerTls::install(ISSUER, loopback, b"not pem").is_err());
    }

    #[test]
    fn single_install_and_drop_restores_the_production_path() {
        let _serial = serial();
        let pki = Pki::generate();
        let ca = pki.read("ca.pem");
        let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let guard = TestWebIssuerTls::install(ISSUER, loopback, &ca).unwrap();
        assert!(TestWebIssuerTls::install(ISSUER, loopback, &ca).is_err());
        drop(guard);
        assert!(slot().is_none());
        let second = TestWebIssuerTls::install(ISSUER, loopback, &ca).unwrap();
        assert!(slot().is_some());
        drop(second);
        assert!(slot().is_none());
    }

    // CAD-2009: independently authored verified-TLS logout acceptance.
    #[cfg(unix)]
    mod cad2009_logout_uncertainty_acceptance {
        use super::*;
        use serde_json::{json, Value};
        use std::io::{BufRead, BufReader, Read, Write};
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixStream;

        const SESSION: &str = "hcs_00000000-0000-4000-8000-000000000009";
        const REFRESH: &str = "hcr_RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR";

        #[derive(Clone, Copy, Debug)]
        enum Answer {
            Lost,
            NonJson,
            Oversized,
            Missing,
            Foreign,
            Bare503,
            Refused,
            Revoked,
        }

        fn run(
            answer: Answer,
            trust_matches: bool,
        ) -> (crate::error::Result<i32>, Vec<Value>, bool) {
            let _serial = serial();
            let pki = Pki::generate();
            let mut certs = BufReader::new(fs::File::open(pki.path("leaf.pem")).unwrap());
            let chain = rustls_pemfile::certs(&mut certs)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let mut keys = BufReader::new(fs::File::open(pki.path("leaf.key")).unwrap());
            let key = rustls_pemfile::private_key(&mut keys).unwrap().unwrap();
            let config = Arc::new(
                ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .unwrap(),
            );
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let _tls = TestWebIssuerTls::install(
                ISSUER,
                listener.local_addr().unwrap(),
                &pki.read("ca.pem"),
            )
            .unwrap();
            let root = tempfile::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let path = root.path().join("cli-acme.json");
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let credential = json!({"version": "hosted-cadence-auth.v1", "organization_id": "ws_real",
            "organization_slug": "acme", "audience": "https://acme.cadencecloud.app", "issuer": ISSUER,
            "subject_id": "user_1", "capabilities": ["cli.read", "cli.write"],
            "access_token": "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", "expires_at": at + 300,
            "cli_session": {"session_id": SESSION, "refresh_token": REFRESH, "generation": 1,
                "device_label": "synthetic", "idle_expires_at": at + 3600, "absolute_expires_at": at + 7200}});
            fs::write(&path, serde_json::to_vec(&credential).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let before = fs::read(&path).unwrap();
            let pin = root.path().join("trusted-issuer");
            fs::write(
                &pin,
                if trust_matches {
                    ISSUER
                } else {
                    "https://different.example.test"
                },
            )
            .unwrap();
            fs::set_permissions(&pin, fs::Permissions::from_mode(0o600)).unwrap();
            let target = crate::remote_cli::RemoteTarget {
                identity: None,
                org: "acme".into(),
                org_id: "ws_real".into(),
                endpoint: "https://acme.cadencecloud.app".into(),
            };
            let (completion, completed) = UnixStream::pair().unwrap();
            let (result, requests) = std::thread::scope(|scope| {
                let server = scope.spawn(move || {
                let mut requests = Vec::new();
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    assert!(!remaining.is_zero(), "logout fixture did not settle");
                    let mut fds = [
                        libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                        libc::pollfd { fd: completed.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                    ];
                    // SAFETY: fds contains exactly two initialized pollfd values.
                    let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2,
                        remaining.as_millis().clamp(1, i32::MAX as u128) as i32) };
                    if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted { continue; }
                    assert!(ready > 0, "logout fixture deadline expired");
                    if fds[0].revents & libc::POLLIN != 0 {
                        let (tcp, _) = listener.accept().unwrap();
                        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                        tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                        let mut tls = StreamOwned::new(ServerConnection::new(config.clone()).unwrap(), tcp);
                        let mut reader = BufReader::new(&mut tls);
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        assert_eq!(line.trim_end(), "POST /v1/hosted-cadence/cli-session/revoke HTTP/1.1");
                        let mut length = None;
                        let mut bearer = None;
                        loop {
                            line.clear();
                            assert!(reader.read_line(&mut line).unwrap() > 0);
                            if line == "\r\n" { break; }
                            let (key, value) = line.split_once(':').unwrap();
                            if key.eq_ignore_ascii_case("content-length") { length = Some(value.trim().parse::<usize>().unwrap()); }
                            if key.eq_ignore_ascii_case("authorization") { bearer = Some(value.trim().to_string()); }
                            assert!(!key.eq_ignore_ascii_case("cookie"));
                        }
                        assert_eq!(bearer.as_deref(), Some(format!("Bearer {REFRESH}").as_str()));
                        let mut bytes = vec![0; length.unwrap()];
                        reader.read_exact(&mut bytes).unwrap();
                        drop(reader);
                        let request: Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(request, json!({"version": "hosted-cadence-cli-session.v1", "session_id": SESSION}));
                        requests.push(request);
                        if matches!(answer, Answer::Lost) && requests.len() == 1 { continue; }
                        let (status, body) = if requests.len() > 1 {
                            ("503 Service Unavailable", b"{}".to_vec())
                        } else { match answer {
                            Answer::Lost | Answer::Bare503 => ("503 Service Unavailable", b"{}".to_vec()),
                            Answer::NonJson => ("200 OK", b"not-json".to_vec()),
                            Answer::Oversized => ("200 OK", vec![b'x'; 64 * 1024 + 1]),
                            Answer::Missing => ("200 OK", b"{}".to_vec()),
                            Answer::Foreign => ("200 OK", serde_json::to_vec(&json!({"version": "hosted-cadence-cli-session.v1", "status": "revoked", "session_id": "hcs_00000000-0000-4000-8000-000000000099"})).unwrap()),
                            Answer::Refused => ("401 Unauthorized", b"{\"error\":\"invalid_token\"}".to_vec()),
                            Answer::Revoked => ("200 OK", serde_json::to_vec(&json!({"version": "hosted-cadence-cli-session.v1", "status": "revoked", "session_id": SESSION})).unwrap()),
                        }};
                        let _ = write!(tls, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                        let _ = tls.write_all(&body);
                        let _ = tls.flush();
                    } else if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 { return requests; }
                    else { panic!("unexpected logout fixture poll events"); }
                }
            });
                let result = crate::remote_cli::logout(root.path(), &target);
                drop(completion);
                (result, server.join().unwrap())
            });
            let retained = path.exists();
            if retained {
                assert_eq!(
                    fs::read(&path).unwrap(),
                    before,
                    "logout changed uncertain local authority"
                );
            }
            (result, requests, retained)
        }

        #[test]
        fn cad2009_unproved_logout_retains_exact_session_and_never_replays() {
            for answer in [
                Answer::Lost,
                Answer::NonJson,
                Answer::Oversized,
                Answer::Missing,
                Answer::Foreign,
                Answer::Bare503,
            ] {
                let (result, requests, retained) = run(answer, true);
                assert_eq!(
                    requests.len(),
                    1,
                    "{answer:?}: revocation was blindly replayed"
                );
                assert!(
                    retained,
                    "{answer:?}: uncertain revocation discarded local authority"
                );
                let error = result.unwrap_err();
                assert_eq!(error.kind(), "unknown", "{answer:?}: {error}");
                let text = error.to_string();
                assert!(text.contains(SESSION) && text.contains("acme"),
                "recovery must name the exact retained session and org, not an invented readback: {text}");
                assert!(text.contains("retain") || text.contains("kept"), "{text}");
                assert!(
                    !text.contains(REFRESH),
                    "logout recovery exposed its refresh bearer"
                );
            }
        }

        #[test]
        fn cad2009_logout_pin_and_issuer_auth_refusals_stay_definite() {
            let (result, requests, retained) = run(Answer::Revoked, false);
            assert!(
                requests.is_empty(),
                "a pin refusal transmitted the refresh bearer"
            );
            assert!(retained);
            assert_eq!(result.unwrap_err().kind(), "rejected");
            let (result, requests, retained) = run(Answer::Refused, true);
            assert_eq!(requests.len(), 1);
            assert!(retained);
            assert_eq!(result.unwrap_err().kind(), "rejected");
        }

        #[test]
        fn cad2009_correlated_logout_receipt_still_removes_local_authority() {
            let (result, requests, retained) = run(Answer::Revoked, true);
            assert_eq!(requests.len(), 1);
            assert_eq!(result.unwrap(), 0);
            assert!(!retained);
        }
    }
}
