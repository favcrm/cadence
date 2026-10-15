//! CAD-1434 / AOS-243: the single constructor for the hosted issuer and board
//! HTTP agents. Production builds construct exactly the agent the call site
//! built before: the caller's config, WebPki roots and the default resolver.
//! There is no environment, config or public selector for anything else.
//!
//! The only difference exists under `cfg(test)`: a fixture may trust one
//! ephemeral test CA and resolve the synthetic board hostnames to loopback
//! through the same `ureq` TLS/resolver path. Certificate verification stays
//! on. This module never disables verification, never trusts a host globally
//! and never changes DNS for anything but the fixture's named hosts.

use std::time::Duration;

/// Build the agent from the caller's finished `ureq` config builder.
pub(crate) fn agent(
    builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
) -> ureq::Agent {
    #[cfg(not(test))]
    {
        ureq::Agent::new_with_config(builder.build())
    }
    #[cfg(test)]
    {
        test_fixture::agent(builder)
    }
}

/// The JWKS fetch agent, production shape unchanged.
pub(crate) fn jwks_agent(timeout: Duration) -> ureq::Agent {
    agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false),
    )
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

    /// The canonical production board alias the native chain uses unchanged.
    /// It maps to loopback only inside this fixture; the production endpoint
    /// rule and parser are not touched.
    pub(crate) const CANONICAL_BOARD_HOST: &str = "lc.cadencecloud.app";

    /// A logical host the fixture resolves to loopback: the canonical board
    /// alias, or a synthetic `*.board.localhost` name used by the TLS slice.
    /// Certificate names are still verified against the presented SAN.
    fn fixture_maps(host: &str) -> bool {
        host == CANONICAL_BOARD_HOST || host.ends_with(".board.localhost")
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
