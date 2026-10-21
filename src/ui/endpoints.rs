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
    WorkspaceList,
    WorkspaceShow,
    ConversationsRead,
    AgentDetail,
    ExplorerUpdateCheck,
    ExplorerRemovePreview,
    ExplorerRemove,
    ExplorerRestore,
    Mount,
    UpdateStart,
    UpdateCheck,
    SettingsAgents,
    SettingsAgentsCatalog,
    SettingsAgentsSync,
    SettingsAgentsPreview,
    ModelDefaults,
    Locale,
    Reports,
    AgentRegister,
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

/// The show row's id keeps the arm's own predicate: a non-empty segment
/// without a slash (an empty id 404s in the chain today).
const SHOW_ID: &[Binding] = &[Binding {
    name: "id",
    allow_empty: false,
    validate: non_empty,
    invalid: (404, "not found"),
}];

/// The agent-detail alias: the arm's own grammar, with its own 400.
const ALIAS_BINDING: &[Binding] = &[Binding {
    name: "alias",
    allow_empty: true,
    validate: valid_alias,
    invalid: (400, "bad agent alias"),
}];

/// `[A-Za-z0-9._-]{1,80}`, as the read arm enforced before the table.
fn valid_alias(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

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

macro_rules! get {
    ($id:ident, $pattern:literal, $class:ident, $bindings:expr) => {
        Endpoint {
            id: RouteId::$id,
            method: "GET",
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
    get!(WorkspaceList, "/api/app-installations", OperatorOnly, &[]),
    get!(
        WorkspaceShow,
        "/api/app-installations/{id}",
        ScreenRead,
        SHOW_ID
    ),
    get!(
        ConversationsRead,
        "/api/app-installations/{id}/conversations",
        OperatorOnly,
        INSTALL_ID
    ),
    get!(
        AgentDetail,
        "/api/agents/{alias}",
        CallerScoped,
        ALIAS_BINDING
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
    row!(UpdateStart, "/api/update", OperatorOnly, &[]),
    row!(UpdateCheck, "/api/update/check", OperatorOnly, &[]),
    row!(SettingsAgents, "/api/settings/agents", OperatorOnly, &[]),
    row!(
        SettingsAgentsCatalog,
        "/api/settings/agents/catalog",
        OperatorOnly,
        &[]
    ),
    row!(
        SettingsAgentsSync,
        "/api/settings/agents/sync",
        OperatorOnly,
        &[]
    ),
    row!(
        SettingsAgentsPreview,
        "/api/settings/agents/preview",
        OperatorOnly,
        &[]
    ),
    row!(
        ModelDefaults,
        "/api/settings/model-defaults",
        OperatorOnly,
        &[]
    ),
    row!(Locale, "/api/locale", CallerScoped, &[]),
    row!(Reports, "/api/reports", AgentAllowed, &[]),
    row!(AgentRegister, "/api/agents", OperatorOnly, &[]),
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

/// The row that owns `(method, path)`, with its bound parameters.
///
/// The method is part of the match, never a filter applied afterwards: a
/// literal write row must not shadow a wildcard read row, so `GET
/// /api/app-installations/upload` finds the show row with id `upload` even
/// though a `POST /api/app-installations/upload` row exists. `HEAD` resolves
/// to the `GET` row: the read arms serve both and `send` strips the body.
pub(super) fn resolve<'p>(method: &str, path: &'p str) -> Option<Bound<'p>> {
    let wanted = if method == "HEAD" { "GET" } else { method };
    ENDPOINTS
        .iter()
        .filter(|endpoint| endpoint.method == wanted)
        .find_map(|endpoint| bind(endpoint, path))
}

/// [`resolve`], or the first row that owns the path when none names the
/// method. The write dispatcher uses this to keep a table-owned path
/// answering `405` (after the request method's own admission) instead of
/// falling through to the legacy `404`. The returned row's method is *not*
/// the request's; the caller must check.
pub(super) fn resolve_or_path<'p>(method: &str, path: &'p str) -> Option<Bound<'p>> {
    resolve(method, path).or_else(|| {
        ENDPOINTS
            .iter()
            .filter(|endpoint| endpoint.method != "GET")
            .find_map(|endpoint| bind(endpoint, path))
    })
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
    resolve(method, path).map(|bound| bound.endpoint.class)
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
                .replace("{tag}", "screen")
                .replace("{alias}", "swe-1");
            let bound = resolve(row.method, &probe).expect("a declared pattern resolves");
            if bound.endpoint.id != row.id {
                // `GET /api/app-installations/{id}` and the POST rows share a
                // prefix; the probe must resolve to the row that declares it.
                panic!("{probe} resolved to {:?}", bound.endpoint.id);
            }
            assert!(bound.invalid().is_none(), "{probe}");
        }
    }

    #[test]
    fn catalog_recovery_wins_the_real_overlap() {
        // The one pair where declaration order decides.
        let bound = resolve("POST", "/api/app-installations/migrations/upgrade/recover")
            .expect("the overlap resolves");
        assert_eq!(bound.endpoint.id, RouteId::CatalogRecover);
        assert_eq!(bound.get("id"), Some("upgrade"));
    }

    #[test]
    fn the_explorer_keeps_its_empty_id_refusal() {
        let bound = resolve("POST", "/api/app-installations//remove").expect("an empty id binds");
        assert_eq!(bound.endpoint.id, RouteId::ExplorerRemove);
        let response = bound.invalid().expect("an empty id is invalid");
        assert_eq!(response.status_code().0, 400);
    }

    #[test]
    fn a_segment_spelling_a_slot_still_binds_and_validates() {
        // `%7Btag%7D` decodes to this at ingress; the slot must bind it and
        // the tag validator must refuse it, not skip validation.
        let bound = resolve("POST", "/api/app-installations/i/screens/{tag}/mount")
            .expect("the mount row binds");
        assert_eq!(bound.get("tag"), Some("{tag}"));
        assert_eq!(bound.invalid().expect("invalid tag").status_code().0, 404);
        let bound = resolve("POST", "/api/app-installations/{id}/remove").expect("the row binds");
        assert_eq!(bound.get("id"), Some("{id}"));
    }

    #[test]
    fn the_mount_refuses_a_bad_id_and_tag() {
        for path in [
            "/api/app-installations/bad.id/screens/screen/mount",
            "/api/app-installations/inst-1/screens/Bad_Tag/mount",
        ] {
            let bound = resolve("POST", path).expect("the mount row binds");
            let response = bound.invalid().expect("the value is invalid");
            assert_eq!(response.status_code().0, 404, "{path}");
        }
    }

    #[test]
    fn an_extra_segment_is_no_row_at_all() {
        assert!(resolve(
            "POST",
            "/api/app-installations/inst-1/update-check/update-check"
        )
        .is_none());
        assert!(resolve("POST", "/api/app-installations/inst-1/restore/restore").is_none());
    }

    #[test]
    fn a_literal_write_row_never_shadows_the_show_read() {
        // `GET /api/app-installations/upload` is the show row with id
        // `upload` today; the POST upload row must not take it.
        let bound = resolve("GET", "/api/app-installations/upload").expect("a read row");
        assert_eq!(bound.endpoint.id, RouteId::WorkspaceShow);
        assert_eq!(bound.get("id"), Some("upload"));
        let bound = resolve("POST", "/api/app-installations/upload").expect("a write row");
        assert_eq!(bound.endpoint.id, RouteId::WorkspaceUpload);
    }

    #[test]
    fn head_resolves_to_the_get_row() {
        for path in [
            "/api/app-installations",
            "/api/app-installations/inst-1",
            "/api/app-installations/inst-1/conversations",
        ] {
            let get = resolve("GET", path).expect("a read row");
            let head = resolve("HEAD", path).expect("HEAD follows GET");
            assert_eq!(head.endpoint.id, get.endpoint.id, "{path}");
            assert_eq!(head.endpoint.class, get.endpoint.class, "{path}");
        }
    }

    #[test]
    fn a_wrong_write_method_still_resolves_the_path() {
        // A table-owned path with the wrong write method must reach the
        // dispatcher's mismatch answer, not fall through to a legacy 404.
        let bound = resolve_or_path("PATCH", "/api/app-installations/inst-1/remove")
            .expect("the path is owned");
        assert_eq!(bound.endpoint.method, "POST");
        assert_eq!(bound.endpoint.id, RouteId::ExplorerRemove);
        assert!(resolve("PATCH", "/api/app-installations/inst-1/remove").is_none());
        // A read-only pattern has no write row: the fallback must not borrow
        // the GET row, or a bare POST would answer 405 instead of the
        // admitted `404 no such write route`.
        assert!(resolve_or_path("POST", "/api/app-installations/inst-1").is_none());
    }

    #[test]
    fn the_agent_detail_keeps_its_alias_grammar() {
        let bound = resolve("GET", "/api/agents/swe-1.high").expect("a valid alias binds");
        assert_eq!(bound.endpoint.id, RouteId::AgentDetail);
        assert_eq!(bound.get("alias"), Some("swe-1.high"));
        assert!(bound.invalid().is_none());

        // Empty and illegal aliases bind and answer the arm's own 400.
        for path in [
            "/api/agents/",
            "/api/agents/bad alias",
            "/api/agents/with%20space",
        ] {
            let Some(bound) = resolve("GET", path) else {
                continue; // `%20` decodes to a space before this point.
            };
            let response = bound.invalid().expect("the alias is invalid");
            assert_eq!(response.status_code().0, 400, "{path}");
        }
        let bound = resolve("GET", "/api/agents/").expect("an empty alias binds");
        assert_eq!(bound.invalid().expect("invalid").status_code().0, 400);
        let long = format!("/api/agents/{}", "a".repeat(81));
        assert_eq!(
            resolve("GET", &long)
                .expect("binds")
                .invalid()
                .expect("invalid")
                .status_code()
                .0,
            400
        );
    }

    #[test]
    fn the_read_rows_keep_their_classes() {
        assert_eq!(
            resolve("GET", "/api/app-installations").map(|b| b.endpoint.class),
            Some(RouteClass::OperatorOnly)
        );
        assert_eq!(
            resolve("GET", "/api/app-installations/i").map(|b| b.endpoint.class),
            Some(RouteClass::ScreenRead)
        );
        assert_eq!(
            resolve("GET", "/api/app-installations/i/conversations").map(|b| b.endpoint.class),
            Some(RouteClass::OperatorOnly)
        );
        // The show pattern has no write row: a bare POST is not a row.
        assert!(resolve("POST", "/api/app-installations/i").is_none());
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
