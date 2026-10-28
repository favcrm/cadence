//! An automatic upgrade re-reads the consent it claimed to have seen.
//! The client's re-read finishes before the package fetch, and the fetch
//! does not hold the PM lock, so a revoke or a switch turned off in that
//! gap must refuse the commit instead of installing the bundle and
//! approving it again. An explicit Apply omits the claim and is still
//! the operator's consent.

#[cfg(test)]
mod tests {
    use super::super::{Result, ServeOptions, Shared};
    use crate::test_seam::{scoped, Asserted};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};

    const WORKFLOW: &str = "---\ntitle: Local\ngoal: Reviewed text\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite.\n\n### Acceptance\n- [ ] written\n";

    struct Fx {
        dir: tempfile::TempDir,
        shared: std::sync::Arc<Shared>,
    }

    struct Installed {
        id: String,
        digest: String,
        generation: String,
        source: String,
    }

    impl Fx {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("auto-upgrade-consent-")
                .tempdir()
                .unwrap();
            let pm = dir.path().join("pm");
            crate::issue::Pm::init(&pm).unwrap();
            let opts = ServeOptions::default();
            opts.provider_env
                .set("CADENCE_PM_DIR", pm.to_str().unwrap());
            let shared = Shared::new(dir.path(), &opts).unwrap();
            Self { dir, shared }
        }

        fn call(&self, method: &str, params: Value) -> Result<Value> {
            scoped(Asserted::Operator, || {
                self.shared.dispatch(method, &params, std::process::id())
            })
        }

        fn op(&self, method: &str, params: Value) -> Value {
            self.call(method, params)
                .unwrap_or_else(|error| panic!("{method} failed: {error}"))
        }

        fn install(&self) -> Installed {
            let source = self.dir.path().join("pkg");
            write_app(&source, "1");
            let source = source.to_str().unwrap().to_string();
            let check = self.op("app_workspace_install_check", json!({"source": source}));
            let installed = self.op(
                "app_workspace_install",
                json!({"source": source, "expected_digest": check["digest"]}),
            );
            assert_eq!(
                installed["consent"]["recorded"],
                json!(true),
                "install did not record consent: {installed}"
            );
            Installed {
                id: installed["install_id"].as_str().unwrap().to_string(),
                digest: installed["digest"].as_str().unwrap().to_string(),
                generation: installed["catalog_generation"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                source,
            }
        }

        fn arm(&self, id: &str) {
            self.shared
                .store
                .app_automatic_updates_set(id, true)
                .unwrap();
        }

        fn revoke(&self, installed: &Installed) {
            self.op(
                "app_local_install_revoke",
                json!({"install_id": installed.id, "digest": installed.digest}),
            );
        }

        fn show_digest(&self, id: &str) -> String {
            self.op("app_workspace_show", json!({"install_id": id}))["digest"]
                .as_str()
                .unwrap()
                .to_string()
        }

        fn consent(&self, id: &str, digest: &str) -> String {
            self.shared.store.app_capability_status(id, digest).unwrap()["state"]
                .as_str()
                .unwrap()
                .to_string()
        }
    }

    fn write_app(dir: &Path, version: &str) {
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        std::fs::write(
            dir.join("app.md"),
            format!(
                "---\napp: demo\ntitle: T\nversion: '{version}'\nneeds:\n  connections: []\n---\n\nGuide.\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
    }

    fn bump(installed: &Installed) {
        let manifest = PathBuf::from(&installed.source).join("app.md");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(&manifest, text.replace("version: '1'", "version: '2'")).unwrap();
    }

    fn proposal(fx: &Fx, installed: &Installed) -> Value {
        fx.op(
            "app_workspace_upgrade_check",
            json!({
                "install_id": installed.id,
                "source": installed.source,
                "expected_digest": installed.digest,
                "expected_generation": installed.generation,
            }),
        )
    }

    fn apply(installed: &Installed, proposed: &Value, request: &str, automatic: bool) -> Value {
        let mut body = json!({
            "install_id": installed.id,
            "source": installed.source,
            "expected_digest": installed.digest,
            "expected_generation": installed.generation,
            "expected_new_digest": proposed["digest"],
            "request_id": request,
        });
        if automatic {
            body["automatic"] = json!(true);
            body["observed_approval"] = json!("approved");
        }
        body
    }

    #[test]
    fn revoked_consent_refuses_the_automatic_upgrade_before_commit() {
        let fx = Fx::new();
        let installed = fx.install();
        fx.arm(&installed.id);
        fx.revoke(&installed);
        assert_eq!(fx.consent(&installed.id, &installed.digest), "revoked");
        bump(&installed);
        let proposed = proposal(&fx, &installed);
        let refused = fx.call(
            "app_workspace_upgrade",
            apply(&installed, &proposed, "auto-revoked", true),
        );
        let error = refused.expect_err("revoked install was upgraded");
        assert!(
            error.to_string().contains("install consent changed"),
            "{error}"
        );
        assert_eq!(fx.show_digest(&installed.id), installed.digest);
        assert_eq!(fx.consent(&installed.id, &installed.digest), "revoked");
        assert_eq!(
            fx.consent(&installed.id, proposed["digest"].as_str().unwrap()),
            "unapproved"
        );
        assert!(!fx.dir.path().join("pm/.apps/upgrade-pending.yaml").exists());
    }

    #[test]
    fn switching_automatic_updates_off_refuses_the_upgrade_before_commit() {
        let fx = Fx::new();
        let installed = fx.install();
        assert_eq!(fx.consent(&installed.id, &installed.digest), "approved");
        bump(&installed);
        let proposed = proposal(&fx, &installed);
        let refused = fx.call(
            "app_workspace_upgrade",
            apply(&installed, &proposed, "auto-off", true),
        );
        let error = refused.expect_err("an unarmed install was upgraded");
        assert!(
            error.to_string().contains("automatic updates are off"),
            "{error}"
        );
        assert_eq!(fx.show_digest(&installed.id), installed.digest);
        assert_eq!(fx.consent(&installed.id, &installed.digest), "approved");
        assert!(!fx.dir.path().join("pm/.apps/upgrade-pending.yaml").exists());
    }

    #[test]
    fn automatic_upgrade_commits_and_keeps_consent_while_approval_holds() {
        let fx = Fx::new();
        let installed = fx.install();
        fx.arm(&installed.id);
        bump(&installed);
        let proposed = proposal(&fx, &installed);
        let body = apply(&installed, &proposed, "auto-ok", true);
        let upgraded = fx.op("app_workspace_upgrade", body.clone());
        let new_digest = proposed["digest"].as_str().unwrap();
        assert_eq!(upgraded["digest"].as_str(), Some(new_digest));
        assert_eq!(upgraded["consent"]["recorded"], json!(true), "{upgraded}");
        assert_eq!(fx.consent(&installed.id, new_digest), "approved");
        assert_eq!(fx.show_digest(&installed.id), new_digest);
        // A lost reply retries the same request. The bundle is already
        // committed and already approved, so the retry must not refuse.
        let replayed = fx.op("app_workspace_upgrade", body);
        assert_eq!(replayed["digest"].as_str(), Some(new_digest));
        assert_eq!(fx.consent(&installed.id, new_digest), "approved");
    }

    #[test]
    fn an_explicit_apply_still_records_consent_after_revoke() {
        let fx = Fx::new();
        let installed = fx.install();
        fx.revoke(&installed);
        bump(&installed);
        let proposed = proposal(&fx, &installed);
        let upgraded = fx.op(
            "app_workspace_upgrade",
            apply(&installed, &proposed, "manual-after-revoke", false),
        );
        let new_digest = proposed["digest"].as_str().unwrap();
        assert_eq!(upgraded["digest"].as_str(), Some(new_digest));
        assert_eq!(upgraded["consent"]["recorded"], json!(true), "{upgraded}");
        assert_eq!(fx.consent(&installed.id, new_digest), "approved");
    }

    #[test]
    fn a_partial_automatic_claim_is_refused_before_fetch() {
        let fx = Fx::new();
        let installed = fx.install();
        let mut body = apply(
            &installed,
            &json!({"digest": installed.digest}),
            "partial",
            false,
        );
        body["automatic"] = json!(true);
        let error = fx
            .call("app_workspace_upgrade", body)
            .expect_err("partial claim was accepted");
        assert!(error.to_string().contains("observed_approval"), "{error}");
        assert_eq!(fx.show_digest(&installed.id), installed.digest);

        let check = fx.call(
            "app_workspace_upgrade_check",
            json!({
                "install_id": installed.id,
                "source": installed.source,
                "expected_digest": installed.digest,
                "expected_generation": installed.generation,
                "automatic": true,
                "observed_approval": "approved",
            }),
        );
        let error = check.expect_err("check accepted the consent pair");
        assert!(
            error
                .to_string()
                .contains("unknown workspace catalog parameter"),
            "{error}"
        );
    }
}
