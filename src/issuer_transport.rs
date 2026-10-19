//! CAD-1434 / AOS-243: the single constructor for the hosted issuer and board
//! HTTP agents. Production builds construct exactly the agent the call site
//! built before: the caller's config, WebPki roots and the default resolver.
//! There is no environment, config or public selector for anything else.
//!
//! The only differences exist outside production builds: under `cfg(test)` a
//! fixture may trust one ephemeral test CA and resolve the synthetic board
//! hostnames to loopback through the same `ureq` TLS/resolver path; and in a
//! non-test binary built with `test-seam` (debug-only — `compile_error!`
//! keeps the feature out of every release) the explicit env pair
//! `CADENCE_TEST_TLS_CA` + `CADENCE_TEST_TLS_CONNECT` arms the same shape for
//! the CAD-1568 packaged-CLI acceptance. Certificate verification stays on
//! in both. Neither disables verification, trusts a host globally nor
//! changes DNS for anything but the fixture's named hosts.

use std::time::Duration;

/// The canonical production board alias the native chain uses unchanged.
/// Fixtures map it to loopback only; the production endpoint rule and
/// parser are never touched.
#[cfg(any(test, feature = "test-seam"))]
pub(crate) const CANONICAL_BOARD_HOST: &str = "lc.cadencecloud.app";

/// Build the agent from the caller's finished `ureq` config builder.
pub(crate) fn agent(
    builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
) -> ureq::Agent {
    build(builder)
}

#[cfg(test)]
fn build(builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::Agent {
    test_fixture::agent(builder)
}

#[cfg(all(not(test), not(feature = "test-seam")))]
fn build(builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::Agent {
    ureq::Agent::new_with_config(builder.build())
}

#[cfg(all(not(test), feature = "test-seam"))]
fn build(builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::Agent {
    env_fixture::agent(builder)
}

/// The JWKS fetch agent, production shape unchanged.
pub(crate) fn jwks_agent(timeout: Duration) -> ureq::Agent {
    agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false),
    )
}

/// CAD-1568: the packaged-CLI acceptance runs the real `cadence` executable
/// as a child process, so the thread-local `cfg(test)` fixture cannot reach
/// it. In a non-test `test-seam` debug binary only, the explicit env pair
/// arms an equivalent fixture transport: `CADENCE_TEST_TLS_CA` names a
/// private 0600 PEM file holding the single fixture CA certificate and
/// `CADENCE_TEST_TLS_CONNECT` is the `127.0.0.1:P` bridge socket. The pair
/// is all-or-nothing: exactly one of the two is a loud failure, never a
/// silent fall back to production DNS or trust. While armed, only the
/// fixture's own two hosts (`lc.cadencecloud.app` and the exact
/// `issuer.board.localhost` authority the controlled TLS bridge serves)
/// resolve — every other host fails closed — and full certificate-chain
/// and hostname verification still run against that private CA.
#[cfg(all(not(test), feature = "test-seam"))]
mod env_fixture {
    use std::net::SocketAddr;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use ureq::config::ConfigBuilder;
    use ureq::tls::{Certificate, RootCerts, TlsConfig};
    use ureq::typestate::AgentScope;
    use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
    use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

    /// The env pair a seam-built binary's fixture transport is armed with.
    const CA_ENV: &str = "CADENCE_TEST_TLS_CA";
    const CONNECT_ENV: &str = "CADENCE_TEST_TLS_CONNECT";
    /// The exact issuer host the controlled fixture bridge serves.
    const ISSUER_HOST: &str = "issuer.board.localhost";

    /// Resolves only the fixture's own two hosts to the armed loopback;
    /// every other host is refused outright, so an armed agent can never
    /// reach a real network destination.
    #[derive(Debug)]
    struct EnvResolver {
        loopback: SocketAddr,
    }

    impl Resolver for EnvResolver {
        fn resolve(
            &self,
            uri: &ureq::http::Uri,
            _config: &ureq::config::Config,
            _timeout: NextTimeout,
        ) -> Result<ResolvedSocketAddrs, ureq::Error> {
            let host = uri.host().unwrap_or_default();
            if host != super::CANONICAL_BOARD_HOST && host != ISSUER_HOST {
                return Err(ureq::Error::HostNotFound);
            }
            let mut addrs = ResolvedSocketAddrs::from_fn(|_| self.loopback);
            addrs.push(self.loopback);
            Ok(addrs)
        }
    }

    /// The private fixture CA file → DER. Mirrors the TLS fixture's own
    /// guards: an absolute path, a regular owned 0600 file, one bounded PEM
    /// certificate block. Every deviation is a loud panic in the seam
    /// binary, not a fallback.
    fn fixture_ca_der(path: &Path) -> Vec<u8> {
        assert!(path.is_absolute(), "{CA_ENV} must be an absolute path");
        let meta = std::fs::symlink_metadata(path)
            .unwrap_or_else(|_| panic!("{CA_ENV} file cannot be inspected"));
        assert!(
            meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && (meta.mode() & 0o777) == 0o600
                && meta.len() <= 16384,
            "{CA_ENV} must be a private owned regular PEM file"
        );
        let text = String::from_utf8(
            std::fs::read(path).unwrap_or_else(|_| panic!("{CA_ENV} file cannot be read")),
        )
        .unwrap_or_else(|_| panic!("{CA_ENV} must be PEM text"));
        let text = text.trim();
        let body = text
            .strip_prefix("-----BEGIN CERTIFICATE-----")
            .and_then(|rest| rest.strip_suffix("-----END CERTIFICATE-----"))
            .unwrap_or_else(|| panic!("{CA_ENV} must contain exactly one PEM certificate"));
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(body.split_ascii_whitespace().collect::<String>())
            .unwrap_or_else(|_| panic!("{CA_ENV} certificate is not base64 DER"))
    }

    pub(crate) fn agent(builder: ConfigBuilder<AgentScope>) -> ureq::Agent {
        let ca = std::env::var_os(CA_ENV);
        let connect = std::env::var_os(CONNECT_ENV);
        let (ca, connect) = match (ca, connect) {
            (None, None) => return ureq::Agent::new_with_config(builder.build()),
            (Some(ca), Some(connect)) => (ca, connect),
            _ => panic!(
                "{CA_ENV} and {CONNECT_ENV} arm the fixture transport together or not at all"
            ),
        };
        let connect: SocketAddr = connect
            .to_str()
            .and_then(|value| value.parse().ok())
            .filter(|addr: &SocketAddr| addr.ip().is_loopback())
            .unwrap_or_else(|| panic!("{CONNECT_ENV} must be a 127.0.0.1 socket address"));
        let der = fixture_ca_der(Path::new(&ca));
        let roots = RootCerts::new_with_certs(&[Certificate::from_der(&der).to_owned()]);
        let tls = TlsConfig::builder().root_certs(roots).build();
        ureq::Agent::with_parts(
            builder.tls_config(tls).proxy(None).max_redirects(0).build(),
            DefaultConnector::default(),
            EnvResolver { loopback: connect },
        )
    }
}

#[cfg(test)]
pub(crate) mod test_fixture {
    use std::cell::RefCell;
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use ureq::config::ConfigBuilder;
    use ureq::tls::{Certificate, RootCerts, TlsConfig};
    use ureq::typestate::AgentScope;
    use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
    use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

    thread_local! {
        /// Fixture state scoped to the installing test thread.
        static FIXTURE: RefCell<Option<Fixture>> = const { RefCell::new(None) };
    }

    /// Optional plain-HTTP host routed to its own loopback, keyed by the
    /// TLS fixture address. Unregistered fixtures keep the original host map.
    static PLAIN_HTTP: Mutex<Vec<(SocketAddr, String, SocketAddr)>> = Mutex::new(Vec::new());
    // Only one test thread at a time installs a fixture; `install` restores
    // the previous value on drop so nothing leaks to a later test.

    /// The trusted test CA (DER) and the loopback address synthetic hosts resolve to.
    #[derive(Clone)]
    pub(crate) struct Fixture {
        pub(crate) ca_der: Vec<u8>,
        pub(crate) loopback: SocketAddr,
    }

    /// Route `host` to `plain` only while a fixture installed for `tls_loopback`
    /// is resolving. Other hosts stay on that fixture's TLS loopback. Drop
    /// removes the route. Existing fixtures that never call this are unchanged.
    /// The only caller is the test-seam membership acceptance.
    #[cfg(feature = "test-seam")]
    pub(crate) fn route_plain_http(
        tls_loopback: SocketAddr,
        host: &str,
        plain: SocketAddr,
    ) -> PlainRoute {
        PLAIN_HTTP
            .lock()
            .unwrap()
            .push((tls_loopback, host.to_string(), plain));
        PlainRoute {
            tls_loopback,
            host: host.to_string(),
        }
    }

    #[cfg(feature = "test-seam")]
    pub(crate) struct PlainRoute {
        tls_loopback: SocketAddr,
        host: String,
    }

    #[cfg(feature = "test-seam")]
    impl Drop for PlainRoute {
        fn drop(&mut self) {
            PLAIN_HTTP
                .lock()
                .unwrap()
                .retain(|(tls, host, _)| !(*tls == self.tls_loopback && host == &self.host));
        }
    }

    fn plain_target(tls_loopback: SocketAddr, host: &str) -> Option<SocketAddr> {
        PLAIN_HTTP
            .lock()
            .unwrap()
            .iter()
            .find(|(tls, name, _)| *tls == tls_loopback && name == host)
            .map(|(_, _, plain)| *plain)
    }

    /// A registered plain host, ignoring which TLS fixture registered it.
    /// Used only when the calling thread has no fixture of its own, so a
    /// daemon or board worker still reaches the test's stub HTTP listener.
    fn plain_target_any(host: &str) -> Option<SocketAddr> {
        PLAIN_HTTP
            .lock()
            .unwrap()
            .iter()
            .find(|(_, name, _)| name == host)
            .map(|(_, _, plain)| *plain)
    }

    fn plain_routes_present() -> bool {
        !PLAIN_HTTP.lock().unwrap().is_empty()
    }

    /// Install `fixture` on this thread until the guard drops.
    pub(crate) fn install(fixture: Fixture) -> Guard {
        FIXTURE.with(|slot| *slot.borrow_mut() = Some(fixture));
        Guard
    }

    pub(crate) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            FIXTURE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    /// A logical host the fixture resolves to loopback: the canonical board
    /// alias, or a synthetic `*.board.localhost` name used by the TLS slice.
    /// Certificate names are still verified against the presented SAN.
    fn fixture_maps(host: &str) -> bool {
        host == super::CANONICAL_BOARD_HOST || host.ends_with(".board.localhost")
    }

    #[derive(Debug)]
    struct FixtureResolver {
        loopback: SocketAddr,
    }

    impl Resolver for FixtureResolver {
        fn resolve(
            &self,
            uri: &ureq::http::Uri,
            _config: &ureq::config::Config,
            _timeout: NextTimeout,
        ) -> Result<ResolvedSocketAddrs, ureq::Error> {
            // While a fixture is installed the agent never performs DNS. A
            // logical host the fixture does not own is refused outright, so no
            // external connect can happen from a test agent.
            let host = uri.host().unwrap_or_default();
            let target = plain_target(self.loopback, host).unwrap_or(self.loopback);
            if target == self.loopback && !fixture_maps(host) {
                return Err(ureq::Error::HostNotFound);
            }
            let mut addrs = ResolvedSocketAddrs::from_fn(|_| target);
            addrs.push(target);
            Ok(addrs)
        }
    }

    /// Resolves only hosts registered with [`route_plain_http`]. Every other
    /// host stays unrouted. Installed only while at least one route exists
    /// and the calling thread has no TLS fixture.
    #[derive(Debug)]
    struct AnyPlainResolver;

    impl Resolver for AnyPlainResolver {
        fn resolve(
            &self,
            uri: &ureq::http::Uri,
            _config: &ureq::config::Config,
            _timeout: NextTimeout,
        ) -> Result<ResolvedSocketAddrs, ureq::Error> {
            let Some(target) = plain_target_any(uri.host().unwrap_or_default()) else {
                return Err(ureq::Error::HostNotFound);
            };
            let mut addrs = ResolvedSocketAddrs::from_fn(|_| target);
            addrs.push(target);
            Ok(addrs)
        }
    }

    pub(crate) fn agent(builder: ConfigBuilder<AgentScope>) -> ureq::Agent {
        let fixture = FIXTURE.with(|slot| slot.borrow().clone());
        let Some(fixture) = fixture else {
            if plain_routes_present() {
                return ureq::Agent::with_parts(
                    builder.proxy(None).max_redirects(0).build(),
                    DefaultConnector::default(),
                    AnyPlainResolver,
                );
            }
            return crate::test_web_issuer_tls::agent(builder);
        };
        let roots = RootCerts::new_with_certs(&[Certificate::from_der(&fixture.ca_der).to_owned()]);
        let tls = TlsConfig::builder().root_certs(roots).build();
        ureq::Agent::with_parts(
            builder.tls_config(tls).proxy(None).max_redirects(0).build(),
            DefaultConnector::default(),
            FixtureResolver {
                loopback: fixture.loopback,
            },
        )
    }
}
