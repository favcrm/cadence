//! The board's endpoint table (CAD-1664 slice 1).
//!
//! One row declares a route's method, path and admission class, so the
//! dispatcher and [`super::operator::route_class`] cannot disagree. Rows are
//! exact paths today; the table grows group by group, and a group moves whole
//! (never half a stem) so no path is ever owned by two mechanisms.
//!
//! A row without a dispatch arm does not compile: `write_route` matches on
//! [`Endpoint::id`] with an exhaustive `match`, so the compiler demands the arm
//! with the row.

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
/// path it did not declare. Dispatch uses this before the method check, so a
/// wrong method still answers the route's own 405 instead of falling through.
pub(super) fn lookup(path: &str) -> Option<&'static Endpoint> {
    ENDPOINTS.iter().find(|endpoint| endpoint.pattern == path)
}

/// The class of `(method, path)`, or `None` when the method does not match the
/// row. `route_class` falls through to the older table on `None`, so a
/// migrated path keeps the class it had before the migration for every method
/// the row does not declare.
pub(super) fn class_of(method: &str, path: &str) -> Option<RouteClass> {
    lookup(path)
        .filter(|endpoint| endpoint.method == method)
        .map(|endpoint| endpoint.class)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Listed literally, not derived from `ENDPOINTS`: deleting or misclassing
    /// a row must fail this test, which iterating the table cannot do.
    const EXPECTED: &[(SessionRoute, &str, &str)] = &[
        (SessionRoute::Open, "POST", "/api/session"),
        (SessionRoute::Logout, "POST", "/api/session/logout"),
        (SessionRoute::DeviceCode, "POST", "/api/session/device/code"),
        (SessionRoute::DevicePoll, "POST", "/api/session/device/poll"),
    ];

    #[test]
    fn the_table_is_exactly_the_session_rows() {
        assert_eq!(ENDPOINTS.len(), EXPECTED.len(), "no row was added or lost");
        for (id, method, pattern) in EXPECTED {
            let endpoint = lookup(pattern).unwrap_or_else(|| panic!("{pattern} is declared"));
            assert_eq!(endpoint.id, *id, "{pattern}");
            assert_eq!(endpoint.method, *method, "{pattern}");
            assert_eq!(endpoint.class, RouteClass::Session, "{pattern}");
        }
    }

    #[test]
    fn the_class_is_method_sensitive() {
        for (_, method, pattern) in EXPECTED {
            assert_eq!(
                class_of(method, pattern),
                Some(RouteClass::Session),
                "{pattern}"
            );
            for other in ["PUT", "PATCH", "DELETE", "GET"] {
                assert_eq!(class_of(other, pattern), None, "{other} {pattern}");
            }
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
