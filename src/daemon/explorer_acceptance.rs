//! CAD-1129 / CAD-1194 acceptance checks (A1–A9), written by the
//! Spec/security reviewer (cc13-sonnet-spec794) from the tickets, not
//! from the implementation. The implementer may not edit or weaken
//! them ("Gates and security work", AGENTS.md).
//!
//! Every check calls the real `Shared::dispatch` guard with the test
//! caller-identity seam (an operator, an agent, an unproven detached
//! child). A refusal is only counted when it names the guard under test
//! and a whole-tracker byte snapshot did not move; each refusal case
//! also has a positive control so a blanket failure cannot satisfy it.
//!
//! - **A1** — catalog list/show rows; a member gets the same rows minus
//!   `digest`/`request_count`; a forged `member_as` never resolves; a
//!   member scope never widens an agent caller. CAD-1422: the built-in
//!   catalog is retired — list/show answer tombstones for the catalog
//!   ids a historical `Source::Builtin` install or an open request still
//!   names; the rows carry the retired reason, never the old listing.
//! - **A2** — `app_home` reads live installs only; a soft-removed
//!   install reads `removed`, is absent from the catalog "installed"
//!   state and from favourites.
//! - **A3** — the pinned install route needs the checked
//!   `expected_digest`; a second install of one app is refused. The
//!   source is a real fixture path — the recorded provenance is
//!   `Source::Path` — and the retired `builtin:` install-entry route is
//!   refused outright (CAD-1422).
//! - **A4** — the Git check refuses non-HTTPS, credential, query,
//!   fragment, IP-literal and local/private hosts before any network.
//! - **A5** — the update check reads the record's stored upstream only;
//!   a `Source::Builtin` record reports the retired catalog honestly.
//! - **A6** — soft remove revokes consent, refuses run creation and
//!   every runtime snapshot; a stale generation/digest refuses.
//! - **A7** — restore re-consents the same digest; after the window it
//!   refuses.
//! - **A8** — favourites bind to the re-proved `member_as`.
//! - **A9** — the manifest parser refuses price keys, an unknown
//!   category and a non-`assets/` asset at parse; the bundle validator
//!   refuses a non-flat or non-SVG `assets/` member.

#[cfg(test)]
mod tests {
    use crate::daemon::*;
    use crate::issue::app_catalog::workspace;
    use crate::operator_auth::BoardUser;
    use crate::test_seam::{scoped, Asserted};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\ninputs:\n  topic: { ask: \"About what?\" }\n---\n\nWhy.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n";

    struct Fx {
        dir: tempfile::TempDir,
        shared: Arc<Shared>,
    }

    impl Fx {
        fn new() -> Self {
            let dir = tempfile::Builder::new().prefix("c1129a").tempdir().unwrap();
            let pm = dir.path().join("pm");
            crate::issue::Pm::init(&pm).unwrap();
            crate::board_identity::write_config(
                dir.path(),
                &crate::board_identity::Config {
                    host: "cad1374.example.test".to_string(),
                    issuer: "https://platform.example.test".to_string(),
                    company: "cad1374-test-company".to_string(),
                },
            )
            .unwrap();
            let opts = ServeOptions::default();
            opts.provider_env
                .set("CADENCE_PM_DIR", pm.to_str().unwrap());
            let shared = Shared::new(dir.path(), &opts).unwrap();
            Self { dir, shared }
        }
        fn pm(&self) -> PathBuf {
            self.dir.path().join("pm")
        }
        fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
            scoped(who, || {
                self.shared.dispatch(method, &params, std::process::id())
            })
        }
        fn op(&self, method: &str, params: Value) -> Value {
            self.call(Asserted::Operator, method, params)
                .unwrap_or_else(|e| panic!("operator {method}: {e}"))
        }
        /// A live public session for a platform user of this role.
        fn session(&self, handle: &str, role: &str) {
            let now = self.shared.operator_now();
            let user = BoardUser {
                sub: format!("sub-{handle}"),
                email: format!("{handle}@example.test"),
                name: handle.to_string(),
                role: role.to_string(),
                handle: handle.to_string(),
            };
            let config = crate::board_identity::read_config(self.dir.path()).unwrap();
            let realm = crate::operator_auth::PublicRealm {
                issuer: config.issuer,
                company: config.company,
                host: config.host,
            };
            self.shared
                .operator_auth()
                .open_public_bound(user, &format!("jti-{handle}"), now + 60, "ua", realm, now)
                .unwrap()
                .expect("fresh jti");
        }
        fn member(&self, handle: &str) {
            self.session(handle, "member");
        }
        /// The bearer a live public session would send. `role` is the
        /// stored board role: `operator` or `member`.
        fn session_token(&self, handle: &str, role: &str) -> String {
            let now = self.shared.operator_now();
            let user = BoardUser {
                sub: format!("sub-{handle}"),
                email: format!("{handle}@example.test"),
                name: handle.to_string(),
                role: role.to_string(),
                handle: handle.to_string(),
            };
            let config = crate::board_identity::read_config(self.dir.path()).unwrap();
            let realm = crate::operator_auth::PublicRealm {
                issuer: config.issuer,
                company: config.company,
                host: config.host,
            };
            self.shared
                .operator_auth()
                .open_public_bound(
                    user,
                    &format!("jti-token-{handle}"),
                    now + 60,
                    "ua",
                    realm,
                    now,
                )
                .unwrap()
                .expect("fresh jti")
                .token
        }
        /// Every file under the tracker (outside .git), with its bytes.
        fn tree(&self) -> BTreeMap<String, Vec<u8>> {
            fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
                for e in std::fs::read_dir(dir).unwrap() {
                    let p = e.unwrap().path();
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                    if rel == ".git" {
                        continue;
                    }
                    if p.is_dir() {
                        out.insert(format!("{rel}/"), vec![]);
                        walk(base, &p, out);
                    } else {
                        out.insert(rel, std::fs::read(&p).unwrap());
                    }
                }
            }
            let mut out = BTreeMap::new();
            walk(&self.pm(), &self.pm(), &mut out);
            out
        }
        /// check -> install of `tests/fixtures/apps/<id>`, the
        /// byte-identical snapshot of the retired built-in — the same
        /// digest-pinned install the board route ran (CAD-1422).
        fn install_fixture(&self, id: &str) -> Value {
            let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/apps")
                .join(id);
            let check = self.op("app_workspace_install_check", json!({"source": source}));
            self.op(
                "app_workspace_install",
                json!({"source": source, "expected_digest": check["digest"]}),
            )
        }
        /// A historical `Source::Builtin` install (CAD-1422): the
        /// record's `kind`/`id`/`digest` are exactly what a pre-retire
        /// install wrote — catalog identity and tombstone resolution
        /// depend on them. The record file is rewritten as YAML, not
        /// edited textually.
        fn install_retired_builtin(&self, app: &str) -> String {
            let out = self.install_fixture(app);
            let install_id = out["install_id"].as_str().unwrap().to_string();
            let record = self
                .pm()
                .join(".apps/installations")
                .join(&install_id)
                .join("record.yaml");
            let mut value: Value =
                serde_yaml::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
            assert_eq!(
                value["source"]["kind"].as_str(),
                Some("path"),
                "fixture install is path-sourced"
            );
            value["source"] = json!({
                "kind": "builtin",
                "id": app,
                "digest": out["digest"].as_str().unwrap(),
            });
            std::fs::write(&record, serde_yaml::to_string(&value).unwrap()).unwrap();
            install_id
        }
        fn bundle(&self, name: &str, app: &str) -> PathBuf {
            let dir = self.dir.path().join("src").join(name);
            std::fs::create_dir_all(dir.join("workflows")).unwrap();
            std::fs::write(
                dir.join("app.md"),
                format!("---\napp: {app}\ntitle: T\nversion: '1'\nneeds:\n  connections: []\n---\n\nGuide.\n"),
            )
            .unwrap();
            std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
            dir
        }
        fn remove_params(&self, id: &str, request: &str) -> Value {
            let p = self.op("app_workspace_remove_preview", json!({"install_id": id}));
            json!({"install_id": id, "expected_generation": p["generation"],
                "expected_digest": p["digest"], "request_id": request})
        }
        fn consent(&self, id: &str) -> String {
            let digest = workspace::show(&self.shared.pm_at(&self.pm()).unwrap(), id).unwrap()
                ["digest"]
                .as_str()
                .unwrap()
                .to_string();
            self.shared
                .store
                .app_capability_status(id, &digest)
                .unwrap()["state"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        }
    }

    fn err_text(r: Result<Value>, case: &str) -> String {
        match r {
            Err(e) => e.to_string(),
            Ok(v) => panic!("{case} was accepted: {v}"),
        }
    }

    /// Every explorer verb with parameters that are valid on shape, so the
    /// caller gate - not the parameter allowlist - decides.
    fn verbs() -> Vec<(&'static str, Value)> {
        let digest = format!("sha256:{}", "0".repeat(64));
        vec![
            ("app_catalog_list", json!({})),
            ("app_catalog_show", json!({"id": "crm"})),
            (
                "app_catalog_git_check",
                json!({"url": "https://example.invalid/a/b"}),
            ),
            ("app_home", json!({})),
            ("app_favorites_get", json!({})),
            ("app_favorites_put", json!({"install_ids": []})),
            ("app_favorites_put_default", json!({"install_ids": []})),
            ("app_favorites_opened", json!({"install_id": "x"})),
            ("app_install_request", json!({"catalog_id": "crm"})),
            ("app_install_requests_list", json!({})),
            ("app_install_request_dismiss", json!({"id": "req-x"})),
            (
                "app_workspace_install_entry",
                json!({"catalog_id": "crm", "expected_digest": digest}),
            ),
            ("app_workspace_update_check", json!({"install_id": "x"})),
            ("app_automatic_updates_get", json!({"install_id": "x"})),
            (
                "app_automatic_updates_set",
                json!({"install_id": "x", "automatic": true}),
            ),
            ("app_workspace_remove_preview", json!({"install_id": "x"})),
            (
                "app_workspace_remove",
                json!({"install_id": "x", "expected_generation": "g",
                    "expected_digest": digest, "request_id": "r"}),
            ),
            ("app_workspace_restore", json!({"install_id": "x"})),
        ]
    }

    const MEMBER_METHODS: [&str; 7] = [
        "app_catalog_list",
        "app_catalog_show",
        "app_home",
        "app_favorites_get",
        "app_favorites_put",
        "app_favorites_opened",
        "app_install_request",
    ];

    // ----------------------------------------------------------------- A1

    /// The retired catalog answers tombstones for the ids history
    /// still names — a historical `Source::Builtin` install, an open
    /// request. A tombstone carries the retired reason and the real
    /// install state, never the old listing payloads; the member
    /// projection still drops `request_count` (and there is no `digest`
    /// — the host ships no bundle to digest).
    #[test]
    fn a1_catalog_rows_operator_and_member_projection() {
        let fx = Fx::new();
        fx.member("alice");
        // No history: the retired catalog is empty, not a listing.
        let list = fx.op("app_catalog_list", json!({}));
        assert_eq!(list["catalog"], json!([]));
        // A historical builtin install names its catalog id.
        let install_id = fx.install_retired_builtin("crm");
        let list = fx.op("app_catalog_list", json!({}));
        let rows = list["catalog"].as_array().unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["crm"]);
        for row in rows {
            assert_eq!(row["retired"], json!(true), "{row}");
            assert!(
                row["retired_reason"].as_str().unwrap().contains("retired"),
                "{row}"
            );
            assert!(
                row.get("digest").is_none(),
                "a tombstone has no digest: {row}"
            );
            assert!(
                row.get("listing").is_none(),
                "a tombstone invents no listing: {row}"
            );
            assert!(row["request_count"].is_number());
            assert_eq!(row["state"], "installed");
            assert_eq!(row["install_id"], json!(install_id));
        }
        let member = fx.op("app_catalog_list", json!({"member_as": "alice"}));
        for row in member["catalog"].as_array().unwrap() {
            assert!(row.get("digest").is_none(), "member sees digest: {row}");
            assert!(
                row.get("request_count").is_none(),
                "member sees count: {row}"
            );
            assert_eq!(row["retired"], json!(true), "{row}");
        }
        let show = fx.op("app_catalog_show", json!({"id": "crm"}));
        assert_eq!(show["retired"], json!(true));
        assert!(
            show["retired_reason"].as_str().unwrap().contains("retired"),
            "{show}"
        );
        assert_eq!(show["install_id"], json!(install_id));
        assert!(
            show.get("about").is_none(),
            "a tombstone invents no about: {show}"
        );
        let shown = fx.op(
            "app_catalog_show",
            json!({"id": "crm", "member_as": "alice"}),
        );
        assert!(shown.get("digest").is_none() && shown.get("request_count").is_none());
        assert_eq!(shown["retired"], json!(true));
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_catalog_show",
                json!({"id": "nope"}),
            ),
            "unknown catalog id",
        );
        assert!(e.contains("unknown catalog entry"), "{e}");
    }

    /// A forged `member_as` (no live session, an operator-role session, a
    /// malformed handle) never resolves to a member; nothing is written.
    #[test]
    fn a1_forged_member_as_never_resolves() {
        let fx = Fx::new();
        fx.member("alice");
        fx.session("olive", "operator");
        let before = fx.tree();
        for forged in ["ghost", "olive", "ALICE", "alice ", "al/ice", ""] {
            for method in MEMBER_METHODS {
                let mut params = verbs().into_iter().find(|(m, _)| *m == method).unwrap().1;
                params["member_as"] = json!(forged);
                let e = err_text(
                    fx.call(Asserted::Operator, method, params),
                    &format!("{method} member_as={forged:?}"),
                );
                assert!(
                    e.contains("member_as"),
                    "{method} member_as={forged:?} refused for another reason: {e}"
                );
            }
        }
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_catalog_list",
                json!({"member_as": 7}),
            ),
            "non-string member_as",
        );
        assert!(e.contains("member_as"), "{e}");
        assert_eq!(fx.tree(), before);
        // positive control: the live member resolves.
        fx.op("app_catalog_list", json!({"member_as": "alice"}));
        assert_eq!(
            fx.shared.store.app_install_requests_open().unwrap().len(),
            0
        );
    }

    /// `app_install_request` resolves only against a tombstone id —
    /// an id nothing historical names is refused before the write.
    #[test]
    fn a1_retired_catalog_member_request_resolves_a_tombstone_only() {
        let fx = Fx::new();
        fx.member("alice");
        let before = fx.tree();
        // an unknown id is refused — the retired catalog has no fresh
        // entries to request.
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_install_request",
                json!({"catalog_id": "crm", "member_as": "alice"}),
            ),
            "request for a retired unknown id",
        );
        assert!(e.contains("unknown catalog entry"), "{e}");
        assert_eq!(fx.tree(), before);
        // a tombstone id resolves: the historical install names it.
        let _install = fx.install_retired_builtin("crm");
        fx.op(
            "app_install_request",
            json!({"catalog_id": "crm", "member_as": "alice"}),
        );
        let open = fx.shared.store.app_install_requests_open().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["catalog_id"], "crm");
        assert_eq!(open[0]["requested_by"], "alice");
    }

    /// A member scope never widens an agent or an unproven caller, and
    /// no explorer verb answers an agent at all.
    #[test]
    fn a1_member_scope_never_widens_an_agent_or_detached_caller() {
        let fx = Fx::new();
        fx.member("alice");
        let before = fx.tree();
        for (method, params) in verbs() {
            let agent = fx.call(Asserted::Agent("writer".into()), method, params.clone());
            let e = err_text(agent, &format!("agent {method}"));
            assert!(
                e.contains("operator action"),
                "agent {method} refused for another reason: {e}"
            );
            let detached = fx.call(Asserted::Unproven, method, params.clone());
            err_text(detached, &format!("unproven {method}"));
            if MEMBER_METHODS.contains(&method) {
                let mut scoped_params = params.clone();
                scoped_params["member_as"] = json!("alice");
                let e = err_text(
                    fx.call(
                        Asserted::Agent("writer".into()),
                        method,
                        scoped_params.clone(),
                    ),
                    &format!("agent {method} + live member_as"),
                );
                assert!(
                    e.contains("operator action"),
                    "agent {method} + member_as refused for another reason: {e}"
                );
                err_text(
                    fx.call(Asserted::Unproven, method, scoped_params),
                    &format!("unproven {method} + live member_as"),
                );
            }
        }
        assert_eq!(fx.tree(), before);
        assert_eq!(
            fx.shared.store.app_install_requests_open().unwrap().len(),
            0,
            "an agent filed an install request"
        );
        let fav = fx.op("app_favorites_get", json!({"member_as": "alice"}));
        assert_eq!(fav["favorites"], json!([]));
        // positive control: the operator runs the same read.
        fx.op("app_catalog_list", json!({}));
    }

    /// `member_as` on a method that has no member scope is refused even
    /// for a live member, and an unknown parameter is refused.
    #[test]
    fn a1_member_scope_on_a_non_member_method_is_refused() {
        let fx = Fx::new();
        fx.member("alice");
        for (method, params) in verbs() {
            if MEMBER_METHODS.contains(&method) {
                continue;
            }
            let mut p = params.clone();
            p["member_as"] = json!("alice");
            err_text(
                fx.call(Asserted::Operator, method, p),
                &format!("{method} + member_as"),
            );
        }
        // `app_install_requests_list` accepts the key on shape, so only
        // the member-scope guard stands between a member and the list.
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_install_requests_list",
                json!({"member_as": "alice"}),
            ),
            "member lists install requests",
        );
        assert!(e.contains("member-scope claim"), "{e}");
        fx.op("app_install_requests_list", json!({}));
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_catalog_list",
                json!({"workspace": "x"}),
            ),
            "unknown parameter",
        );
        assert!(e.contains("unknown app explorer parameter"), "{e}");
    }

    /// A member files an install request as themselves — against a
    /// tombstone id a historical install names (CAD-1422). The install
    /// path itself never opens to a member, and the retired built-in
    /// install route refuses any caller.
    #[test]
    fn a1_member_request_is_attributed_and_cannot_install() {
        let fx = Fx::new();
        fx.member("alice");
        let _install = fx.install_retired_builtin("crm");
        fx.op(
            "app_install_request",
            json!({"catalog_id": "crm", "member_as": "alice"}),
        );
        let open = fx.shared.store.app_install_requests_open().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["requested_by"], "alice");
        // no requester at all -> refused.
        err_text(
            fx.call(
                Asserted::Operator,
                "app_install_request",
                json!({"catalog_id": "crm"}),
            ),
            "request without a requester",
        );
        // a member scope on the install entry is refused (member-scope
        // claim on a non-member method) — and installs nothing.
        let before = fx.tree();
        err_text(
            fx.call(
                Asserted::Operator,
                "app_workspace_install_entry",
                json!({"catalog_id": "crm", "expected_digest": format!("sha256:{}", "0".repeat(64)),
                    "member_as": "alice"}),
            ),
            "member installs",
        );
        // the operator's own call on the retired route refuses too —
        // `builtin::get` is the retired refusal.
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_workspace_install_entry",
                json!({"catalog_id": "crm", "expected_digest": format!("sha256:{}", "0".repeat(64))}),
            ),
            "operator installs from the retired catalog",
        );
        assert!(e.contains("built-in catalog"), "{e}");
        assert_eq!(fx.tree(), before);
    }

    /// `board_session_member` answers whether a member handle holds a live
    /// public session. It carries no credential, so it must not answer a
    /// caller that is not the operator's own connection: an agent could
    /// otherwise enumerate who is signed in to the board.
    #[test]
    fn a1_board_session_member_is_not_an_oracle_for_agents() {
        let fx = Fx::new();
        fx.member("alice");
        for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
            let r = fx.call(
                who.clone(),
                "board_session_member",
                json!({"handle": "alice"}),
            );
            assert!(
                r.is_err(),
                "{who:?} learned that 'alice' holds a live member session: {r:?}"
            );
            let r = fx.call(
                who.clone(),
                "board_session_member",
                json!({"handle": "ghost"}),
            );
            assert!(r.is_err(), "{who:?} probed a handle: {r:?}");
        }
        // positive control: the operator's own connection gets the answer.
        let r = fx.op("board_session_member", json!({"handle": "alice"}));
        assert_eq!(r["member"], true);
        let r = fx.op("board_session_member", json!({"handle": "ghost"}));
        assert_eq!(r["member"], false);
    }

    // ----------------------------------------------------------------- A2

    #[test]
    fn a2_home_reads_live_installs_and_removed_is_not_installed() {
        let fx = Fx::new();
        let installed = fx.install_fixture("crm");
        let id = installed["install_id"].as_str().unwrap().to_string();
        let home = fx.op("app_home", json!({}));
        let row = home["installations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["install_id"] == json!(id))
            .expect("live install on the home");
        assert_eq!(row["attention"]["state"], "ok", "{row}");
        fx.op("app_favorites_put", json!({"install_ids": [id]}));
        // soft remove
        fx.op("app_workspace_remove", fx.remove_params(&id, "rm-a2"));
        let home = fx.op("app_home", json!({}));
        let row = home["installations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["install_id"] == json!(id))
            .expect("recently removed row");
        assert_eq!(row["attention"]["state"], "removed", "{row}");
        assert_eq!(row["attention"]["action"], "restore");
        let favs = fx.op("app_favorites_get", json!({}));
        assert_eq!(
            favs["favorites"],
            json!([]),
            "removed app still a favourite"
        );
    }

    /// A2 variant for the retired catalog: a historical
    /// `Source::Builtin` install's tombstone tracks the live state —
    /// `installed`, then `removed`+`restorable` after a soft remove
    /// (CAD-1422).
    #[test]
    fn a2_retired_tombstone_tracks_the_install_state() {
        let fx = Fx::new();
        let id = fx.install_retired_builtin("crm");
        let cat = fx.op("app_catalog_list", json!({}));
        let crm = cat["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "crm")
            .unwrap()
            .clone();
        assert_eq!(crm["state"], "installed");
        assert_eq!(crm["install_id"], json!(id));
        fx.op("app_workspace_remove", fx.remove_params(&id, "rm-a2r"));
        let cat = fx.op("app_catalog_list", json!({}));
        let crm = cat["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "crm")
            .unwrap()
            .clone();
        assert_eq!(crm["state"], "available", "removed still reads installed");
        assert_eq!(crm["removed"], json!(true));
        assert_eq!(crm["restorable"], json!(true));
    }

    // ----------------------------------------------------------------- A3

    /// The pinned install needs the checked `expected_digest` — over
    /// the generic source install the board's review drives. Missing,
    /// stale, malformed, wrong-type and other-bundle digests refuse with
    /// nothing written; the positive control records path provenance
    /// (CAD-1422: the built-in entry route is retired, checked below).
    #[test]
    fn a3_builtin_install_needs_the_checked_digest_and_writes_nothing_on_refusal() {
        let fx = Fx::new();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/apps/crm")
            .display()
            .to_string();
        let other_source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/apps/social-content")
            .display()
            .to_string();
        let check = fx.op("app_workspace_install_check", json!({"source": source}));
        let digest = check["digest"].as_str().unwrap().to_string();
        let other_digest = fx.op(
            "app_workspace_install_check",
            json!({"source": other_source}),
        )["digest"]
            .as_str()
            .unwrap()
            .to_string();
        let before = fx.tree();
        // the check is read-only and never creates the catalog.
        assert!(!fx.pm().join(".apps/catalog.yaml").exists());
        let stale = format!("sha256:{}", "0".repeat(64));
        for params in [
            json!({"source": source, "expected_digest": ""}),
            json!({"source": source, "expected_digest": "sha256:"}),
            json!({"source": source, "expected_digest": stale}),
            json!({"source": source, "expected_digest": digest.to_uppercase()}),
            json!({"source": source, "expected_digest": 7}),
            json!({"source": source, "expected_digest": other_digest}),
            json!({"source": source, "expected_digest": digest, "actor": "operator"}),
        ] {
            let r = fx.call(Asserted::Operator, "app_workspace_install", params.clone());
            assert!(r.is_err(), "accepted {params}: {r:?}");
        }
        assert_eq!(fx.tree(), before, "a refused install wrote");
        assert!(!fx.pm().join(".apps/catalog.yaml").exists());
        // positive control + record provenance.
        let out = fx.op(
            "app_workspace_install",
            json!({"source": source, "expected_digest": digest}),
        );
        let shown = fx.op(
            "app_workspace_show",
            json!({"install_id": out["install_id"]}),
        );
        assert_eq!(shown["digest"], json!(digest));
        assert_eq!(shown["source"]["kind"], "path");
        // a second install of one app refuses and changes nothing.
        let after = fx.tree();
        let again = fx.call(
            Asserted::Operator,
            "app_workspace_install",
            json!({"source": source, "expected_digest": digest}),
        );
        assert!(again.is_err(), "second install accepted: {again:?}");
        assert_eq!(fx.tree(), after);
        // a bundle edited after the check is not what the digest names.
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_workspace_install",
                json!({"source": source, "expected_digest": other_digest}),
            ),
            "digest of another bundle",
        );
        assert!(e.to_lowercase().contains("digest"), "{e}");
    }

    /// CAD-1422: the retired built-in install-entry route refuses every
    /// caller and writes nothing — there is no embedded bundle to pin.
    #[test]
    fn a3_retired_install_entry_route_refuses_everything() {
        let fx = Fx::new();
        let before = fx.tree();
        let digest = format!("sha256:{}", "0".repeat(64));
        for params in [
            json!({"catalog_id": "crm"}),
            json!({"catalog_id": "crm", "expected_digest": ""}),
            json!({"catalog_id": "crm", "expected_digest": "sha256:"}),
            json!({"catalog_id": "crm", "expected_digest": digest}),
            json!({"catalog_id": "crm", "expected_digest": digest.to_uppercase()}),
            json!({"catalog_id": "crm", "expected_digest": 7}),
            json!({"catalog_id": "social-content", "expected_digest": digest}),
            json!({"catalog_id": "nope", "expected_digest": digest}),
            json!({"catalog_id": "crm", "expected_digest": digest, "source": "/tmp"}),
            json!({"catalog_id": "crm", "expected_digest": digest, "actor": "operator"}),
        ] {
            let r = fx.call(
                Asserted::Operator,
                "app_workspace_install_entry",
                params.clone(),
            );
            assert!(r.is_err(), "accepted {params}: {r:?}");
        }
        for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
            let r = fx.call(
                who.clone(),
                "app_workspace_install_entry",
                json!({"catalog_id": "crm", "expected_digest": digest}),
            );
            assert!(r.is_err(), "{who:?} accepted a retired install: {r:?}");
        }
        assert_eq!(fx.tree(), before, "a refused retired install wrote");
        assert!(!fx.pm().join(".apps/catalog.yaml").exists());
    }

    // ----------------------------------------------------------------- A4

    /// The URL vet refuses before any network step. The refusal must be
    /// the vet's own message: a call that reached `git ls-remote` would
    /// answer a different error.
    #[test]
    fn a4_git_check_refuses_unsafe_urls_before_any_network_step() {
        let fx = Fx::new();
        let before = fx.tree();
        for url in [
            "http://github.com/o/r",
            "git://github.com/o/r",
            "ssh://git@github.com/o/r",
            "git@github.com:o/r.git",
            "file:///etc",
            "/etc",
            "https://user:pw@github.com/o/r",
            "https://user@github.com/o/r",
            "https://github.com/o/r?x=1",
            "https://github.com/o/r#frag",
            "https://github.com/o/%72",
            "https://127.0.0.1/o/r",
            "https://10.0.0.5/o/r",
            "https://192.168.1.10/o/r",
            "https://172.16.0.1/o/r",
            "https://169.254.169.254/latest",
            "https://[::1]/o/r",
            "https://[fd00::1]/o/r",
            "https://localhost/o/r",
            "https://LOCALHOST/o/r",
            "https:///o/r",
        ] {
            let e = err_text(
                fx.call(
                    Asserted::Operator,
                    "app_catalog_git_check",
                    json!({"url": url}),
                ),
                url,
            );
            assert!(
                e.contains("git check") || e.contains("git source url"),
                "{url} was not refused by the URL vet: {e}"
            );
        }
        assert_eq!(fx.tree(), before);
        // positive control: a well-formed public name passes the vet and
        // fails later, at the network step.
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_catalog_git_check",
                json!({"url": "https://example.invalid/o/r"}),
            ),
            "unresolvable host",
        );
        assert!(
            !e.contains("git check"),
            "the vet refused a well-formed public URL: {e}"
        );
    }

    /// Numeric and loopback-alias spellings of a local host are not
    /// public hosts: `git` resolves them to loopback/private addresses.
    #[test]
    fn a4_git_check_refuses_numeric_and_loopback_alias_hosts() {
        let fx = Fx::new();
        let mut reached = Vec::new();
        for url in [
            "https://2130706433/o/r",
            "https://0x7f000001/o/r",
            "https://0x7f.0.0.1/o/r",
            "https://127.1/o/r",
            "https://0177.0.0.1/o/r",
            "https://foo.localhost/o/r",
            "https://localhost./o/r",
        ] {
            let e = err_text(
                fx.call(
                    Asserted::Operator,
                    "app_catalog_git_check",
                    json!({"url": url}),
                ),
                url,
            );
            if !(e.contains("git check") || e.contains("git source url")) {
                reached.push(format!("{url} -> {e}"));
            }
        }
        assert!(
            reached.is_empty(),
            "these hosts passed the URL vet and reached the network step:\n{}",
            reached.join("\n")
        );
    }

    // ----------------------------------------------------------------- A5

    #[test]
    fn a5_update_check_reads_the_stored_upstream_only() {
        let fx = Fx::new();
        let src = fx.bundle("a", "pin-a");
        let out = fx.op(
            "app_workspace_install",
            json!({"source": src.to_str().unwrap()}),
        );
        let id = out["install_id"].as_str().unwrap().to_string();
        for forged in [
            json!({"install_id": id, "source": "/tmp"}),
            json!({"install_id": id, "url": "https://example.invalid/o/r"}),
            json!({"install_id": id, "dir": "x"}),
            json!({"install_id": id, "expected_digest": "sha256:x"}),
        ] {
            let e = err_text(
                fx.call(
                    Asserted::Operator,
                    "app_workspace_update_check",
                    forged.clone(),
                ),
                &forged.to_string(),
            );
            assert!(e.contains("unknown app explorer parameter"), "{e}");
        }
        err_text(
            fx.call(
                Asserted::Operator,
                "app_workspace_update_check",
                json!({"install_id": "inst-none"}),
            ),
            "unknown install",
        );
        let ok = fx.op("app_workspace_update_check", json!({"install_id": id}));
        assert_eq!(ok["source"], "path");
        assert_eq!(ok["has_update"], false);
        // CAD-1422: a historical `Source::Builtin` install reports the
        // retired source — honestly, never a fake up-to-date digest.
        let b = fx.install_retired_builtin("crm");
        let ok = fx.op("app_workspace_update_check", json!({"install_id": b}));
        assert_eq!(ok["source"], "builtin");
        assert_eq!(ok["has_update"], false);
        assert_eq!(ok["retired"], json!(true));
        assert!(ok["digest"].is_null() || ok["digest"].is_string());
    }

    // ----------------------------------------------------------------- A6

    #[test]
    fn a6_remove_revokes_consent_refuses_runs_and_checks_staleness() {
        let fx = Fx::new();
        let id = fx.install_retired_builtin("crm");
        let pm = fx.shared.pm_at(&fx.pm()).unwrap();
        assert_eq!(fx.consent(&id), "approved", "install records consent");
        // positive control: a live install admits a runtime snapshot.
        workspace::with_runtime_snapshot(&pm, &id, |_, _| Ok(())).expect("live snapshot");
        let run = |rid: &str| {
            fx.call(
                Asserted::Operator,
                "app_run_create",
                json!({"install_id": id, "workflow": "email-brief",
                    "inputs": {"facts": "x"}, "request_id": rid}),
            )
        };
        // the fixture's `email-brief` workflow is the same one the
        // retired built-in bundle shipped.
        if let Err(e) = run("run-live") {
            assert!(!e.to_string().contains("removed"), "{e}");
        }
        // stale generation / digest refuse with no change.
        let good = fx.remove_params(&id, "rm-a6");
        let before = fx.tree();
        let mut stale_gen = good.clone();
        stale_gen["expected_generation"] = json!("0".repeat(64));
        let e = err_text(
            fx.call(Asserted::Operator, "app_workspace_remove", stale_gen),
            "stale generation",
        );
        assert!(e.contains("generation is stale"), "{e}");
        let mut stale_digest = good.clone();
        stale_digest["expected_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
        let e = err_text(
            fx.call(Asserted::Operator, "app_workspace_remove", stale_digest),
            "stale digest",
        );
        assert!(e.contains("digest is stale"), "{e}");
        let mut bad_request = good.clone();
        bad_request["request_id"] = json!("bad id/../x");
        err_text(
            fx.call(Asserted::Operator, "app_workspace_remove", bad_request),
            "malformed request id",
        );
        assert_eq!(fx.tree(), before, "a refused remove wrote");
        assert_eq!(fx.consent(&id), "approved", "a refused remove revoked");
        for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
            err_text(
                fx.call(who, "app_workspace_remove", good.clone()),
                "non-operator remove",
            );
        }
        assert_eq!(fx.tree(), before);
        assert_eq!(fx.consent(&id), "approved");
        // remove.
        let out = fx.op("app_workspace_remove", good.clone());
        assert_eq!(out["state"], "removed");
        assert_eq!(fx.consent(&id), "revoked", "remove left consent");
        for (what, r) in [
            (
                "runtime snapshot",
                workspace::with_runtime_snapshot(&pm, &id, |_, _| Ok(())).map(|_| Value::Null),
            ),
            (
                "completed snapshot",
                workspace::with_completed_bundle_snapshot(
                    &pm,
                    &id,
                    out["digest"]
                        .as_str()
                        .unwrap_or(&format!("sha256:{}", "0".repeat(64))),
                    |_, _| Ok(()),
                )
                .map(|_| Value::Null),
            ),
            ("app_run_create", run("run-removed")),
            (
                "app_binding_list",
                fx.call(
                    Asserted::Operator,
                    "app_binding_list",
                    json!({"install_id": id}),
                ),
            ),
        ] {
            let e = err_text(r, what);
            assert!(
                e.contains("removed"),
                "{what} refused for another reason: {e}"
            );
        }
        // replaying the same remove is idempotent and does not re-run effects.
        let again = fx.op("app_workspace_remove", good);
        assert_eq!(again["replayed"], true);
    }

    // ----------------------------------------------------------------- A7

    #[test]
    fn a7_restore_reconsents_the_same_digest_and_the_window_closes() {
        let fx = Fx::new();
        let id = fx.install_retired_builtin("crm");
        let pm = fx.shared.pm_at(&fx.pm()).unwrap();
        let digest = workspace::show(&pm, &id).unwrap()["digest"].clone();
        fx.op("app_workspace_remove", fx.remove_params(&id, "rm-1"));
        assert_eq!(fx.consent(&id), "revoked");
        // non-operators cannot restore.
        for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
            err_text(
                fx.call(who, "app_workspace_restore", json!({"install_id": id})),
                "non-operator restore",
            );
        }
        assert_eq!(fx.consent(&id), "revoked");
        let out = fx.op("app_workspace_restore", json!({"install_id": id}));
        assert_eq!(out["state"], "live");
        assert_eq!(out["digest"], digest, "restore changed the digest");
        assert_eq!(fx.consent(&id), "approved", "restore did not re-consent");
        workspace::with_runtime_snapshot(&pm, &id, |_, _| Ok(())).expect("restored snapshot");
        // remove again, then close the window.
        fx.op("app_workspace_remove", fx.remove_params(&id, "rm-2"));
        let record = fx
            .pm()
            .join(".apps/installations")
            .join(&id)
            .join("record.yaml");
        let text = std::fs::read_to_string(&record).unwrap();
        let mut closed = String::new();
        for line in text.lines() {
            if line.starts_with("restore_after:") {
                closed.push_str("restore_after: 1\n");
            } else {
                closed.push_str(line);
                closed.push('\n');
            }
        }
        assert_ne!(text, closed, "no restore_after in {text}");
        std::fs::write(&record, closed).unwrap();
        let before = fx.tree();
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_workspace_restore",
                json!({"install_id": id}),
            ),
            "restore after the window",
        );
        assert!(e.contains("restore window closed"), "{e}");
        assert_eq!(fx.tree(), before);
        assert_eq!(fx.consent(&id), "revoked", "a refused restore consented");
        workspace::with_runtime_snapshot(&pm, &id, |_, _| Ok(()))
            .expect_err("closed-window install runs");
    }

    // ----------------------------------------------------------------- A8

    #[test]
    fn a8_favorites_bind_to_the_reproved_member_never_a_client_field() {
        let fx = Fx::new();
        fx.member("alice");
        fx.member("bob");
        let a = fx.install_fixture("crm")["install_id"]
            .as_str()
            .unwrap()
            .to_string();
        let b = fx.install_fixture("social-content")["install_id"]
            .as_str()
            .unwrap()
            .to_string();
        fx.op(
            "app_favorites_put",
            json!({"install_ids": [a], "member_as": "alice"}),
        );
        fx.op(
            "app_favorites_put",
            json!({"install_ids": [b, a], "member_as": "bob"}),
        );
        let ids = |v: Value| -> Vec<String> {
            v["favorites"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["install_id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            ids(fx.op("app_favorites_get", json!({"member_as": "alice"}))),
            std::slice::from_ref(&a)
        );
        assert_eq!(
            ids(fx.op("app_favorites_get", json!({"member_as": "bob"}))),
            [b.clone(), a.clone()]
        );
        // the operator's list is its own; it is not alice's or bob's.
        assert!(ids(fx.op("app_favorites_get", json!({}))).is_empty());
        // a client-chosen owner never rides a request.
        for key in ["owner", "member", "user", "requested_by"] {
            let mut p = json!({"install_ids": [a]});
            p[key] = json!("alice");
            let e = err_text(fx.call(Asserted::Operator, "app_favorites_put", p), key);
            assert!(e.contains("unknown app explorer parameter"), "{e}");
        }
        // unknown and removed ids are refused and the list is untouched.
        for bad in [
            json!(["inst-none"]),
            json!(["../x"]),
            json!([7]),
            json!("x"),
        ] {
            err_text(
                fx.call(
                    Asserted::Operator,
                    "app_favorites_put",
                    json!({"install_ids": bad, "member_as": "alice"}),
                ),
                "bad favourite",
            );
        }
        assert_eq!(
            ids(fx.op("app_favorites_get", json!({"member_as": "alice"}))),
            std::slice::from_ref(&a)
        );
        fx.op("app_workspace_remove", fx.remove_params(&b, "rm-b"));
        let e = err_text(
            fx.call(
                Asserted::Operator,
                "app_favorites_put",
                json!({"install_ids": [b], "member_as": "alice"}),
            ),
            "removed favourite",
        );
        assert!(e.contains("unknown or removed"), "{e}");
        // bob's saved removed app is filtered from his read.
        assert_eq!(
            ids(fx.op("app_favorites_get", json!({"member_as": "bob"}))),
            std::slice::from_ref(&a)
        );
        // the workspace default is the operator's: a member scope is refused.
        err_text(
            fx.call(
                Asserted::Operator,
                "app_favorites_put_default",
                json!({"install_ids": [a], "member_as": "alice"}),
            ),
            "member sets the workspace default",
        );
        // the list is bounded.
        let many: Vec<String> = (0..25).map(|i| format!("inst-{i}")).collect();
        assert!(fx.shared.store.app_favorites_put("alice", &many).is_err());
        assert!(fx.shared.store.app_favorites_put_default(&many).is_err());
        assert_eq!(
            ids(fx.op("app_favorites_get", json!({"member_as": "alice"}))),
            [a]
        );
    }

    // ----------------------------------------------------------------- A9

    fn manifest(front: &str) -> String {
        format!("---\napp: lst\ntitle: T\nversion: '1'\nneeds:\n  connections: []\n{front}---\n\nGuide.\n")
    }

    fn files(app_md: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut out = vec![
            ("app.md".to_string(), app_md.to_string()),
            ("workflows/do.md".to_string(), WORKFLOW.to_string()),
        ];
        out.extend(extra.iter().map(|(a, b)| (a.to_string(), b.to_string())));
        out
    }

    const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"></svg>";

    #[test]
    fn a9_listing_is_refused_at_parse_for_price_keys_category_and_assets() {
        use crate::issue::app::parse_manifest;
        // positive control.
        let ok = manifest(
            "listing:\n  tagline: A tagline\n  category: marketing\n  icon: assets/i.svg\n",
        );
        assert!(parse_manifest(&ok).is_ok());
        for (case, front) in [
            ("listing.cost", "listing:\n  tagline: x\n  cost: 3\n"),
            ("listing.price", "listing:\n  tagline: x\n  price: 3\n"),
            ("listing.pricing", "listing:\n  pricing:\n    plan: pro\n"),
            (
                "nested price",
                "listing:\n  data:\n    stores:\n      - price: 3\n",
            ),
            (
                "deep cost",
                "listing:\n  data:\n    keeps:\n      a:\n        b:\n          cost: 1\n",
            ),
            ("top-level cost", "cost: 5\nlisting:\n  tagline: x\n"),
            ("top-level price", "price: 5\n"),
            ("top-level pricing", "pricing: { a: 1 }\n"),
            ("unknown category", "listing:\n  category: crypto\n"),
            ("category case", "listing:\n  category: Marketing\n"),
            (
                "icon outside assets",
                "listing:\n  icon: workflows/do.svg\n",
            ),
            ("icon not svg", "listing:\n  icon: assets/i.png\n"),
            ("icon traversal", "listing:\n  icon: assets/../app.svg\n"),
            ("icon nested", "listing:\n  icon: assets/a/b.svg\n"),
            ("icon absolute", "listing:\n  icon: /assets/i.svg\n"),
            ("icon url", "listing:\n  icon: https://x.test/i.svg\n"),
            (
                "shot outside assets",
                "listing:\n  screenshots:\n    - file: screens/x.svg\n",
            ),
            (
                "shot not svg",
                "listing:\n  screenshots:\n    - file: assets/x.jpg\n",
            ),
            (
                "unknown key",
                "listing:\n  tagline: x\n  buy_url: https://x.test\n",
            ),
            ("html tagline", "listing:\n  tagline: '<b>x</b>'\n"),
            ("link about", "listing:\n  about: '[x](https://x.test)'\n"),
            (
                "over-long tagline",
                &format!("listing:\n  tagline: {}\n", "x".repeat(90)),
            ),
        ] {
            let r = parse_manifest(&manifest(front));
            assert!(r.is_err(), "{case} was accepted at parse");
        }
        // the price refusal is the price guard, not a side effect.
        for front in [
            "listing:\n  tagline: x\n  cost: 3\n",
            "listing:\n  pricing:\n    plan: pro\n",
            "listing:\n  data:\n    keeps:\n      a:\n        b:\n          cost: 1\n",
        ] {
            let e = parse_manifest(&manifest(front)).unwrap_err().to_string();
            assert!(e.contains("prices are not shown"), "{front}: {e}");
        }
    }

    #[test]
    fn a9_bundle_assets_are_flat_svg_and_referenced_files_exist() {
        let agents = Default::default();
        let validate = |fs: Vec<(String, String)>| {
            crate::issue::app::validate_texts(fs, &agents, &[]).map(|_| ())
        };
        let with_icon = manifest("listing:\n  tagline: x\n  icon: assets/i.svg\n");
        // positive control.
        validate(files(&with_icon, &[("assets/i.svg", SVG)])).expect("valid bundle");
        // a referenced asset that is not in the bundle.
        assert!(validate(files(&with_icon, &[])).is_err());
        // The member grammar is enforced where a bundle enters: a directory
        // source, checked read-only (nothing is written by the check).
        let fx = Fx::new();
        let before = fx.tree();
        let dir = |name: &str, extra: &[(&str, &str)], app_md: &str| {
            let d = fx.dir.path().join("src").join(name);
            std::fs::create_dir_all(d.join("workflows")).unwrap();
            std::fs::write(d.join("app.md"), app_md).unwrap();
            std::fs::write(d.join("workflows/do.md"), WORKFLOW).unwrap();
            for (rel, body) in extra {
                let p = d.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, body).unwrap();
            }
            d
        };
        let check = |d: &Path| {
            fx.call(
                Asserted::Operator,
                "app_workspace_install_check",
                json!({"source": d.to_str().unwrap()}),
            )
        };
        let good = dir("good", &[("assets/i.svg", SVG)], &with_icon);
        check(&good).expect("flat svg asset bundle checks");
        let plain = manifest("");
        for (case, member) in [
            ("png", "assets/i.png"),
            ("nested", "assets/a/i.svg"),
            ("dotfile", "assets/.i.svg"),
            ("html", "assets/i.html"),
            ("js", "assets/i.js"),
        ] {
            let d = dir(&format!("bad-{case}"), &[(member, SVG)], &plain);
            let r = check(&d);
            assert!(r.is_err(), "{case} ({member}) was accepted into a bundle");
        }
        // a listing that names a file the bundle lacks.
        let d = dir("missing", &[], &with_icon);
        assert!(check(&d).is_err(), "listing icon missing from bundle");
        assert_eq!(fx.tree(), before, "install-check wrote");
    }

    // ------------------------------------------------------- CAD-1209

    /// Item 3: an update is pending, the operator's tombstone row says
    /// so and a member's row (list and show) carries no
    /// `update_available`. The install is a historical
    /// `Source::Builtin` (CAD-1422) so its tombstone still resolves.
    #[test]
    fn c1209_member_row_has_no_update_available() {
        let fx = Fx::new();
        fx.member("alice");
        let id = fx.install_retired_builtin("crm");
        fx.shared
            .store
            .app_update_check_save(&id, true, Some("9.9.9"), None, Some("sha256:x"), &json!({}))
            .unwrap();
        let row = |v: &Value| -> Value {
            v["catalog"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == "crm")
                .unwrap()
                .clone()
        };
        let op = row(&fx.op("app_catalog_list", json!({})));
        assert_eq!(op["update_available"], json!(true), "operator row: {op}");
        let mem = row(&fx.op("app_catalog_list", json!({"member_as": "alice"})));
        assert_eq!(mem["state"], "installed", "member row: {mem}");
        assert!(
            mem.get("update_available").is_none(),
            "member sees update state: {mem}"
        );
        let shown = fx.op(
            "app_catalog_show",
            json!({"id": "crm", "member_as": "alice"}),
        );
        assert!(shown.get("update_available").is_none(), "{shown}");
        assert!(!shown.to_string().contains("9.9.9"), "{shown}");
    }

    /// Item 1 (CAD-1422): the retired catalog ships no bundles, so a
    /// tombstone's detail carries no access rows — the manifest's
    /// `data` declaration lives in the detached repository, and the host
    /// invents nothing for a retired entry. The data-row logic itself is
    /// covered by `data_access_rows` above and by the `app_access`
    /// module's own checks.
    #[test]
    fn c1209_retired_show_carries_no_invented_access_rows() {
        let fx = Fx::new();
        let _install = fx.install_retired_builtin("crm");
        let shown = fx.op("app_catalog_show", json!({"id": "crm"}));
        assert_eq!(shown["retired"], json!(true));
        assert!(
            shown.get("access").is_none() || shown["access"] == json!([]),
            "a tombstone invents no access rows: {shown}"
        );
        assert!(shown.get("data").is_none(), "{shown}");
    }

    /// Setting the preference needs a person at the board. An operator
    /// shell with no session proof must not turn it on.
    #[test]
    fn automatic_updates_set_needs_a_person_at_the_board() {
        let fx = Fx::new();
        let err = err_text(
            fx.call(
                Asserted::Operator,
                "app_automatic_updates_set",
                json!({"install_id": "x", "automatic": true}),
            ),
            "operator shell without a board person",
        );
        assert!(
            err.contains("person at the board"),
            "shell was not refused as a missing person: {err}"
        );
        assert_eq!(
            fx.shared.store.app_automatic_updates_get("x").unwrap()["automatic"],
            false
        );

        let installed = fx.install_fixture("crm");
        let id = installed["install_id"].as_str().unwrap();
        let member = fx.session_token("ada", "member");
        let err = err_text(
            fx.call(
                Asserted::Operator,
                "app_automatic_updates_set",
                json!({"install_id": id, "automatic": true, "public_session": member}),
            ),
            "member session sets the preference",
        );
        assert!(
            err.contains("does not match"),
            "member session was not refused: {err}"
        );
        let operator = fx.session_token("ora", "operator");
        let set = fx.op(
            "app_automatic_updates_set",
            json!({"install_id": id, "automatic": true, "public_session": operator}),
        );
        assert_eq!(set["automatic"], true);
        let got = fx.op("app_automatic_updates_get", json!({"install_id": id}));
        assert_eq!(got["automatic"], true);
    }
}
