//! The board's endpoint table (CAD-1664; slices CAD-1670, CAD-1682).
//!
//! One row declares a route's method, path, admission class and the parameters
//! it binds, so the dispatcher and [`super::operator::route_class`] cannot
//! disagree. A group moves whole — never half a stem — so no path is ever owned
//! by two mechanisms.
//!
//! A row without a dispatch arm does not compile: `write_route` matches on
//! [`RouteId`] with an exhaustive `match`. Add the arm with the row.

use super::operator::RouteClass;
use super::serve::err_response;
use super::write_path::HttpResp;

/// Parameters a row can bind. Two is every parameterised route today
/// (`{id}` and `{tag}`); a row needing more extends this and its array.
const MAX_PARAMS: usize = 2;

/// One route the table owns. One variant per dispatch arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RouteId {
    // CAD-1670: the session paths.
    SessionOpen,
    SessionLogout,
    SessionDeviceCode,
    SessionDevicePoll,
    // CAD-1682: the app-installations write stem and the explorer's exact paths.
    WorkspaceInstall,
    WorkspaceCheck,
    WorkspaceUpload,
    WorkspaceUploadCheck,
    WorkspaceMigrate,
    CatalogRecover,
    WorkspaceRecover,
    WorkspaceUpgrade,
    WorkspaceUpgradeCheck,
    WorkspaceUpgradeRecover,
    Conversations,
    ExplorerUpdateCheck,
    ExplorerRemovePreview,
    ExplorerRemove,
    ExplorerRestore,
    Mount,
    CatalogInstall,
    CatalogGitCheck,
    CatalogRequest,
    FavoriteSet,
    FavoriteDefault,
    FavoriteOpened,
    RequestDismiss,
}

/// How one bound parameter is validated, and what a failure answers.
///
/// `invalid` is the pair the arm answered before the migration: the mount and
/// recovery families fell through to `404 no such write route`, while an empty
/// explorer id returned `400 missing installation id` from its handler.
pub(super) struct Binding {
    pub name: &'static str,
    /// Whether an empty segment binds at all. The explorer id must bind it so
    /// its validator can answer the 400; every other row rejects it as a miss.
    pub allow_empty: bool,
    pub validate: fn(&str) -> bool,
    pub invalid: (u16, &'static str),
}

/// One row: the method it answers, the path it owns, its class, its parameters.
pub(super) struct Endpoint {
    pub id: RouteId,
    pub method: &'static str,
    pub pattern: &'static str,
    pub class: RouteClass,
    pub bindings: &'static [Binding],
}

fn non_empty(value: &str) -> bool {
    !value.is_empty()
}

/// The mount route's installation id, as `mount_route` enforced it before
/// the table owned the path (CAD-1682).
fn valid_install_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The mount route's screen tag (`app_screen_pkg::valid_tag`).
fn valid_tag(value: &str) -> bool {
    crate::issue::app_screen_pkg::valid_tag(value)
}

const INSTALL_ID: &[Binding] = &[Binding {
    name: "id",
    allow_empty: false,
    validate: non_empty,
    invalid: (404, "no such write route"),
}];

/// The explorer's id: it binds an empty segment so the row can answer the
/// handler's `400 missing installation id`, exactly as before.
const EXPLORER_ID: &[Binding] = &[Binding {
    name: "id",
    allow_empty: true,
    validate: non_empty,
    invalid: (400, "missing installation id"),
}];

const MOUNT_BINDINGS: &[Binding] = &[
    Binding {
        name: "id",
        allow_empty: false,
        validate: valid_install_id,
        invalid: (404, "no such write route"),
    },
    Binding {
        name: "tag",
        allow_empty: false,
        validate: valid_tag,
        invalid: (404, "no such write route"),
    },
];

macro_rules! row {
    ($id:ident, $pattern:literal, $class:ident, $bindings:expr) => {
        Endpoint {
            id: RouteId::$id,
            method: "POST",
            pattern: $pattern,
            class: RouteClass::$class,
            bindings: $bindings,
        }
    };
}

/// Every route this table owns, in match order.
///
/// Order matters for exactly one pair: `/migrations/{id}/recover` must precede
/// `/{id}/upgrade/recover`, because `POST
/// /api/app-installations/migrations/upgrade/recover` satisfies both and the
/// pre-migration chain relayed catalog recovery with id `upgrade`.
pub(super) const ENDPOINTS: &[Endpoint] = &[
    row!(SessionOpen, "/api/session", Session, &[]),
    row!(SessionLogout, "/api/session/logout", Session, &[]),
    row!(SessionDeviceCode, "/api/session/device/code", Session, &[]),
    row!(SessionDevicePoll, "/api/session/device/poll", Session, &[]),
    row!(
        WorkspaceInstall,
        "/api/app-installations",
        OperatorOnly,
        &[]
    ),
    row!(
        WorkspaceCheck,
        "/api/app-installations/check",
        OperatorOnly,
        &[]
    ),
    row!(
        WorkspaceUpload,
        "/api/app-installations/upload",
        OperatorOnly,
        &[]
    ),
    row!(
        WorkspaceUploadCheck,
        "/api/app-installations/upload/check",
        OperatorOnly,
        &[]
    ),
    row!(
        WorkspaceMigrate,
        "/api/app-installations/migrate",
        OperatorOnly,
        &[]
    ),
    row!(
        CatalogRecover,
        "/api/app-installations/migrations/{id}/recover",
        OperatorOnly,
        INSTALL_ID
    ),
    row!(
        WorkspaceRecover,
        "/api/app-installations/{id}/recover",
        OperatorOnly,
        INSTALL_ID
    ),
    row!(
        WorkspaceUpgrade,
        "/api/app-installations/{id}/upgrade",
        OperatorOnly,
        INSTALL_ID
    ),
    row!(
        WorkspaceUpgradeCheck,
        "/api/app-installations/{id}/upgrade/check",
        OperatorOnly,
        INSTALL_ID
    ),
    row!(
        WorkspaceUpgradeRecover,
        "/api/app-installations/{id}/upgrade/recover",
        OperatorOnly,
        INSTALL_ID
    ),
    row!(
        Conversations,
        "/api/app-installations/{id}/conversations",
        OperatorOnly,
        INSTALL_ID
    ),
    row!(
        ExplorerUpdateCheck,
        "/api/app-installations/{id}/update-check",
        OperatorOnly,
        EXPLORER_ID
    ),
    row!(
        ExplorerRemovePreview,
        "/api/app-installations/{id}/remove-preview",
        OperatorOnly,
        EXPLORER_ID
    ),
    row!(
        ExplorerRemove,
        "/api/app-installations/{id}/remove",
        OperatorOnly,
        EXPLORER_ID
    ),
    row!(
        ExplorerRestore,
        "/api/app-installations/{id}/restore",
        OperatorOnly,
        EXPLORER_ID
    ),
    row!(
        Mount,
        "/api/app-installations/{id}/screens/{tag}/mount",
        ScreenRead,
        MOUNT_BINDINGS
    ),
    row!(
        CatalogInstall,
        "/api/app-catalog/install",
        OperatorOnly,
        &[]
    ),
    row!(
        CatalogGitCheck,
        "/api/app-catalog/git-check",
        OperatorOnly,
        &[]
    ),
    row!(
        CatalogRequest,
        "/api/app-catalog/request",
        AgentAllowed,
        &[]
    ),
    row!(FavoriteSet, "/api/app-favorites", AgentAllowed, &[]),
    row!(
        FavoriteDefault,
        "/api/app-favorites/default",
        OperatorOnly,
        &[]
    ),
    row!(
        FavoriteOpened,
        "/api/app-favorites/opened",
        AgentAllowed,
        &[]
    ),
    row!(
        RequestDismiss,
        "/api/app-requests/dismiss",
        OperatorOnly,
        &[]
    ),
];

impl Endpoint {
    fn binding(&self, name: &str) -> Option<&'static Binding> {
        self.bindings.iter().find(|binding| binding.name == name)
    }
}

/// A row plus the parameters it bound from one path.
pub(super) struct Bound<'p> {
    pub endpoint: &'static Endpoint,
    params: [Option<(&'static str, &'p str)>; MAX_PARAMS],
}

impl<'p> Bound<'p> {
    pub fn get(&self, name: &str) -> Option<&'p str> {
        self.params
            .iter()
            .flatten()
            .find(|(bound, _)| *bound == name)
            .map(|(_, value)| *value)
    }

    /// The row's refusal when a bound value fails its validator, or `None`.
    pub fn invalid(&self) -> Option<HttpResp> {
        for (name, value) in self.params.iter().flatten() {
            let binding = self.endpoint.binding(name)?;
            if !(binding.validate)(value) {
                return Some(err_response(binding.invalid.0, binding.invalid.1));
            }
        }
        None
    }
}

/// The row that owns `path`, with its bound parameters. Method is not
/// considered here: a matched row with the wrong method answers 405, and a
/// path no row owns falls through to the remaining chain.
pub(super) fn resolve(path: &str) -> Option<Bound<'_>> {
    ENDPOINTS.iter().find_map(|endpoint| bind(endpoint, path))
}

fn bind<'p>(endpoint: &'static Endpoint, path: &'p str) -> Option<Bound<'p>> {
    let mut want = endpoint.pattern.split('/');
    let mut got = path.split('/');
    let mut params = [None; MAX_PARAMS];
    let mut bound = 0;
    loop {
        match (want.next(), got.next()) {
            (None, None) => break,
            (Some(slot), Some(segment)) => {
                // A slot is recognised by its own spelling, never by the
                // request segment: a segment that literally reads `{tag}`
                // must still bind the parameter and face its validator.
                match slot
                    .strip_prefix('{')
                    .and_then(|slot| slot.strip_suffix('}'))
                {
                    Some(name) => {
                        let binding = endpoint.binding(name)?;
                        if segment.is_empty() && !binding.allow_empty {
                            return None;
                        }
                        if bound == MAX_PARAMS {
                            return None;
                        }
                        params[bound] = Some((binding.name, segment));
                        bound += 1;
                    }
                    None if slot == segment => {}
                    None => return None,
                }
            }
            _ => return None,
        }
    }
    Some(Bound { endpoint, params })
}

/// The class of `(method, path)`, or `None` when no row declares that method
/// for the path. `route_class` falls through to the older table on `None`, so a
/// migrated path keeps the class it had for every method the row does not name.
pub(super) fn class_of(method: &str, path: &str) -> Option<RouteClass> {
    resolve(path)
        .filter(|bound| bound.endpoint.method == method)
        .map(|bound| bound.endpoint.class)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_row_is_declared_twice() {
        for (index, row) in ENDPOINTS.iter().enumerate() {
            assert!(
                !ENDPOINTS[index + 1..]
                    .iter()
                    .any(|other| other.pattern == row.pattern && other.method == row.method),
                "{} is declared once",
                row.pattern
            );
        }
    }

    #[test]
    fn every_row_binds_its_own_pattern() {
        for row in ENDPOINTS {
            let probe = row
                .pattern
                .replace("{id}", "inst-1")
                .replace("{tag}", "screen");
            let bound = resolve(&probe).expect("a declared pattern resolves");
            assert_eq!(bound.endpoint.id, row.id, "{probe}");
            assert!(bound.invalid().is_none(), "{probe}");
        }
    }

    #[test]
    fn catalog_recovery_wins_the_real_overlap() {
        // The one pair where declaration order decides.
        let bound = resolve("/api/app-installations/migrations/upgrade/recover")
            .expect("the overlap resolves");
        assert_eq!(bound.endpoint.id, RouteId::CatalogRecover);
        assert_eq!(bound.get("id"), Some("upgrade"));
    }

    #[test]
    fn the_explorer_keeps_its_empty_id_refusal() {
        let bound = resolve("/api/app-installations//remove").expect("an empty id binds");
        assert_eq!(bound.endpoint.id, RouteId::ExplorerRemove);
        let response = bound.invalid().expect("an empty id is invalid");
        assert_eq!(response.status_code().0, 400);
    }

    #[test]
    fn a_segment_spelling_a_slot_still_binds_and_validates() {
        // `%7Btag%7D` decodes to this at ingress; the slot must bind it and
        // the tag validator must refuse it, not skip validation.
        let bound =
            resolve("/api/app-installations/i/screens/{tag}/mount").expect("the mount row binds");
        assert_eq!(bound.get("tag"), Some("{tag}"));
        assert_eq!(bound.invalid().expect("invalid tag").status_code().0, 404);
        let bound = resolve("/api/app-installations/{id}/remove").expect("the row binds");
        assert_eq!(bound.get("id"), Some("{id}"));
    }

    #[test]
    fn the_mount_refuses_a_bad_id_and_tag() {
        for path in [
            "/api/app-installations/bad.id/screens/screen/mount",
            "/api/app-installations/inst-1/screens/Bad_Tag/mount",
        ] {
            let bound = resolve(path).expect("the mount row binds");
            let response = bound.invalid().expect("the value is invalid");
            assert_eq!(response.status_code().0, 404, "{path}");
        }
    }

    #[test]
    fn an_extra_segment_is_no_row_at_all() {
        assert!(resolve("/api/app-installations/inst-1/update-check/update-check").is_none());
        assert!(resolve("/api/app-installations/inst-1/restore/restore").is_none());
    }

    #[test]
    fn the_class_is_method_sensitive_and_session_is_post_only() {
        for (path, class) in [
            ("/api/session", RouteClass::Session),
            (
                "/api/app-installations/inst-1/remove",
                RouteClass::OperatorOnly,
            ),
            ("/api/app-favorites", RouteClass::AgentAllowed),
            (
                "/api/app-installations/inst-1/screens/screen/mount",
                RouteClass::ScreenRead,
            ),
        ] {
            assert_eq!(class_of("POST", path), Some(class), "{path}");
            assert_eq!(class_of("PUT", path), None, "{path}");
        }
    }
}
