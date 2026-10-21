//! The board's endpoint table (CAD-1664 slice 1).
//!
//! One row declares a route's method, path and admission class, so the
//! dispatcher and [`super::operator::route_class`] cannot disagree. Rows are
//! exact paths today; the table grows group by group, and a group moves whole
//! (never half a stem) so no path is ever owned by two mechanisms.
//!
//! A row is not a claim that the handler is registered: `write_route` matches
//! on [`Endpoint::id`], so a row with no arm compiles but dispatches nothing.
//! Add the arm with the row.

use super::operator::RouteClass;

/// The session paths this slice owns. One variant per handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionRoute {
    /// `POST /api/session` — redeem the single-use login nonce.
    Open,
    /// `POST /api/session/logout` — drop the session.
    Logout,
    /// `POST /api/session/device/code` — start the device exchange.
    DeviceCode,
    /// `POST /api/session/device/poll` — finish it.
    DevicePoll,
}

/// One row: the method it answers, the path it owns, its class.
#[derive(Clone, Copy, Debug)]
pub(super) struct Endpoint {
    pub id: SessionRoute,
    pub method: &'static str,
    pub pattern: &'static str,
    pub class: RouteClass,
}

/// Every route this slice's table owns. The class is the one
/// `operator::route_class` answers, and `Session` is the class that skips
/// `admit` — the four paths return before it, as they always did.
pub(super) const ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        id: SessionRoute::Open,
        method: "POST",
        pattern: "/api/session",
        class: RouteClass::Session,
    },
    Endpoint {
        id: SessionRoute::Logout,
        method: "POST",
        pattern: "/api/session/logout",
        class: RouteClass::Session,
    },
    Endpoint {
        id: SessionRoute::DeviceCode,
        method: "POST",
        pattern: "/api/session/device/code",
        class: RouteClass::Session,
    },
    Endpoint {
        id: SessionRoute::DevicePoll,
        method: "POST",
        pattern: "/api/session/device/poll",
        class: RouteClass::Session,
    },
];

/// The row that owns `path`, if any. Exact match only: a row never claims a
/// path it did not declare.
pub(super) fn lookup(path: &str) -> Option<&'static Endpoint> {
    ENDPOINTS.iter().find(|endpoint| endpoint.pattern == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_resolves_and_no_pattern_repeats() {
        for endpoint in ENDPOINTS {
            let found = lookup(endpoint.pattern).expect("a declared row resolves");
            assert_eq!(found.id, endpoint.id);
            assert_eq!(
                ENDPOINTS
                    .iter()
                    .filter(|other| other.pattern == endpoint.pattern)
                    .count(),
                1,
                "{} is declared once",
                endpoint.pattern
            );
        }
    }

    #[test]
    fn the_session_rows_are_post_and_session_classed() {
        for endpoint in ENDPOINTS {
            assert_eq!(endpoint.method, "POST", "{}", endpoint.pattern);
            assert_eq!(endpoint.class, RouteClass::Session, "{}", endpoint.pattern);
        }
    }

    #[test]
    fn lookup_is_exact() {
        assert!(lookup("/api/session").is_some());
        assert!(lookup("/api/session/").is_none());
        assert!(lookup("/api/session/device").is_none());
        assert!(lookup("/api/Session").is_none());
        assert!(lookup("/api/launch").is_none());
    }
}
