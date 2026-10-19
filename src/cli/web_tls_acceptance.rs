//! CAD-1429 independent TLS acceptance driver (acceptance author, not the
//! implementer). Included as a sibling `tls_acceptance` module of `web` by
//! `web_cli_test_harness.rs`, so `super::web::run` is the SAME production
//! command function the binary runs. Compiled only under `cfg(test)`.
//!
//! What it exercises, against the NodeHTTPS bridge owned by
//! `apps/api/src/web-sites/integration.acceptance.test.ts`:
//!   * the real PKCE device flow through `remote_enrollment::login_portal_scoped`
//!     to the virtual issuer `https://web-publish-issuer.test`, over TLS that
//!     trusts only the disposable CA (`TestWebIssuerTls::install`);
//!   * `save_credential` into the directory the production code actually reads:
//!     `remote_auth::auth_dir(None)`, which the Node controller forces under an
//!     owned `XDG_CONFIG_HOME`. The driver asserts that equality before writing;
//!   * a `RemoteTarget` built ONLY from the verified `LoginGrant` fields
//!     (`organization_id`, `endpoint`) plus the operator-selected slug;
//!   * the production `super::web::run` for Prepare (HTML), Publish, Status and
//!     Releases, asserting on the values those calls return and on the durable
//!     non-secret pending record they persist under that directory.
//!
//! Not in this file: no fabricated credential or registry JSON, no hand-built
//! server ledger, no manual HTML upload, no direct write to the pending record.
//! Owner consent and publication approval are performed by the Node controller
//! as separate HTTP actors with the real owner cookie, never by this process.
//!
//! Runs only when explicitly selected: `#[ignore]`, invoked later by Node with
//! `--ignored --exact` against the owned bridge environment.

use super::web::{run, WebAction};
use crate::remote_cli::RemoteTarget;
use crate::remote_enrollment::{login_portal_scoped, LoginGrant};
use crate::test_web_issuer_tls::{TestWebIssuerTls, ISSUER};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Environment variables the Node controller sets before spawning the test
/// binary. Nothing here is a credential; the CA is public, the socket is local.
const ENV_CA_PEM: &str = "AOS241_TLS_CA_PEM";
const ENV_LOOPBACK: &str = "AOS241_TLS_LOOPBACK";
const ENV_SLUG: &str = "AOS241_WORKSPACE_SLUG";
const ENV_OUT_DIR: &str = "AOS241_TLS_OUT_DIR";

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set by the Node controller"))
}

/// The directory `remote_auth::auth_dir(None)` resolves to under the
/// controller-owned environment. Asserted, never assumed: the production code
/// reads its credentials and pending record from here.
fn production_auth_dir(out_root: &Path) -> PathBuf {
    let resolved = cadence_agent::remote_auth::auth_dir(None)
        .expect("auth_dir(None) must resolve under the owned XDG_CONFIG_HOME");
    assert!(
        resolved.starts_with(out_root),
        "auth_dir(None) escaped the owned output root: production would read another dir"
    );
    resolved
}

/// Create the production auth directory with mode 0700 and nothing else in it.
fn fresh_auth_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("create auth dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod auth dir");
    }
}

/// Write the operator's public issuer trust pin (`trusted-issuer`) before the
/// real login. This is the independent trust decision the production guard
/// requires. It contains only the public [`ISSUER`] string, not a credential,
/// grant, session, or permission fabrication. Created exclusively, mode 0600.
fn write_operator_trust_pin(auth_dir: &Path) {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = auth_dir.join("trusted-issuer");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .expect("create operator trusted-issuer pin exclusively");
    file.write_all(format!("{ISSUER}\n").as_bytes())
        .expect("write trusted-issuer pin");
    file.sync_all().expect("sync trusted-issuer pin");
    let mode = std::fs::metadata(&path)
        .expect("stat trusted-issuer pin")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "trusted-issuer pin must be mode 0600");
}

/// Install the virtual-issuer TLS fixture, or fail the case with a clear reason.
fn install_tls(
    loopback: SocketAddr,
    ca_pem: &[u8],
) -> crate::test_web_issuer_tls::TestWebIssuerTlsGuard {
    TestWebIssuerTls::install(ISSUER, loopback, ca_pem)
        .unwrap_or_else(|e| panic!("TLS fixture refused: {e}"))
}

/// Build the `RemoteTarget` from the verified grant only. The org name is the
/// operator-selected slug; the endpoint and organization id come from the
/// issuer-echoed grant (`LoginGrant`), never from a caller literal.
fn target_from_grant(grant: &LoginGrant, slug: &str) -> RemoteTarget {
    RemoteTarget {
        identity: None,
        org: slug.to_string(),
        endpoint: grant.endpoint.clone(),
        org_id: grant.organization_id.clone(),
    }
}

/// Real PKCE device login. The show-code closure receives the verification
/// address and user code from the real device/code response; it writes nothing
/// to disk and logs nothing. The Node controller performs owner consent.
fn real_login(auth_dir: &Path, slug: &str) -> LoginGrant {
    login_portal_scoped(
        ISSUER,
        Some(slug),
        auth_dir,
        |_verification, _user_code| Ok(()),
        true,
        false,
    )
    .expect("real device-flow login")
}

/// Read the non-secret release list the production code persisted after a
/// verified finalize. The pending file is named by the org (workspace) slug,
/// matching `cli/web.rs::pending_path`. Only public ids and the digest are read.
fn prepared_release(auth_dir: &Path, org: &str, slug: &str) -> (String, String) {
    let path = auth_dir.join(format!("web-{org}.json"));
    let raw = std::fs::read(&path).expect("pending record must exist after a ready prepare");
    let record: serde_json::Value =
        serde_json::from_slice(&raw).expect("pending record must be JSON");
    let release = record["releases"]
        .as_array()
        .and_then(|list| list.iter().find(|r| r["slug"] == slug))
        .expect("the verified prepare must record one release for this slug");
    let release_id = release["release_id"]
        .as_str()
        .expect("release_id must be a string")
        .to_string();
    let digest = release["manifest_digest"]
        .as_str()
        .expect("manifest_digest must be a string")
        .to_string();
    (release_id, digest)
}

#[test]
#[ignore = "requires owned API bridge: run by the Node controller with --ignored --exact"]
fn cli_publish_web_real_device_login_prepare_publish_status_releases() {
    let ca_pem = std::fs::read(required(ENV_CA_PEM)).expect("read public CA certificate");
    let loopback: SocketAddr = required(ENV_LOOPBACK)
        .parse()
        .expect("loopback socket must be ip:port");
    let slug = required(ENV_SLUG);
    let out_root = PathBuf::from(required(ENV_OUT_DIR));

    // The production credential and pending directory lives under the owned
    // root (the controller sets XDG_CONFIG_HOME there). Assert it before use.
    let auth_dir = production_auth_dir(&out_root);
    fresh_auth_dir(&auth_dir);
    write_operator_trust_pin(&auth_dir);
    let _tls = install_tls(loopback, &ca_pem);

    // 1. Real device login over TLS; the grant is verified by the client.
    let grant = real_login(&auth_dir, &slug);
    grant
        .save_credential(&auth_dir)
        .expect("save the verified grant under the production auth directory");
    let target = target_from_grant(&grant, &slug);
    assert_eq!(
        target.org_id, grant.organization_id,
        "target bound to the verified grant"
    );
    assert!(
        !target.endpoint.is_empty(),
        "verified endpoint must be present"
    );

    // 2. Prepare a plain HTML file through the production command.
    let site = out_root.join("site");
    std::fs::create_dir_all(&site).expect("create site dir");
    std::fs::write(site.join("index.html"), "<!doctype html><h1>cli-tls</h1>").expect("write html");
    let site_slug = format!("{slug}-site");
    let prepared = run(
        &WebAction::Prepare {
            input: site.join("index.html"),
            name: site_slug.clone(),
            spa: false,
        },
        &target,
        5,
    )
    .expect("prepare must complete through the real TLS bridge");
    assert_eq!(prepared, 0, "prepare exit code");

    // 3. Publish the release the verified prepare recorded. The owner
    //    approval happens on the server (Node controller); the CLI only
    //    requests and then reports the awaiting action. Production maps
    //    `awaiting_approval` to exit 0 (cli/web.rs report_action), not 3.
    let (release_id, _digest) = prepared_release(&auth_dir, &target.org, &site_slug);
    let published = run(
        &WebAction::Publish {
            release: release_id,
        },
        &target,
        5,
    )
    .expect("publish must complete through the real TLS bridge");
    assert_eq!(
        published, 0,
        "publish must report the awaiting-approval outcome"
    );
    let pending = std::fs::read(auth_dir.join(format!("web-{}.json", target.org)))
        .expect("pending record must exist after publish");
    let record: serde_json::Value =
        serde_json::from_slice(&pending).expect("pending record must be JSON");
    assert!(
        record["intents"].as_array().is_some_and(|list| list
            .iter()
            .any(|i| i["kind"] == "publish" && i["state"] == "open")),
        "the awaiting publish must remain an open durable intent"
    );

    // 4. Status and releases observe the real post-approval state. The Node
    //    controller approves the action after it verifies the D1 pending row,
    //    so these calls are expected to see the completed action.
    let status = run(
        &WebAction::Status {
            slug: site_slug.clone(),
        },
        &target,
        5,
    )
    .expect("status must complete");
    assert_eq!(status, 0, "status exit code");
    let releases =
        run(&WebAction::Releases { slug: site_slug }, &target, 5).expect("releases must complete");
    assert_eq!(releases, 0, "releases exit code");
}

/// Expect a production command to exit 0. A non-zero outcome writes the step
/// name and the production error's Display text (never a token, cookie, device
/// or user code: the error values are non-secret refusal messages) to a marker
/// file under the owned root, then panics. The Node controller never reads it.
fn expect_ok(out_root: &Path, outcome: crate::Result<i32>, step: &str) {
    let message = match outcome {
        Ok(0) => return,
        Ok(code) => format!("{step} exited {code}"),
        Err(error) => format!("{step} failed: {error}"),
    };
    let _ = std::fs::write(out_root.join("lifecycle-failure.txt"), &message);
    panic!("{message}");
}

/// A controller-built site directory: a real directory (not a symlink) that
/// resolves inside the owned root. Refused otherwise, before any freeze.
fn site_input(out_root: &Path, name: &str) -> PathBuf {
    let dir = out_root.join(name);
    let meta = std::fs::symlink_metadata(&dir).expect("controller-built site directory");
    assert!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "site input must be a real directory"
    );
    let canonical = std::fs::canonicalize(&dir).expect("canonical site directory");
    let root = std::fs::canonicalize(out_root).expect("canonical owned root");
    assert!(
        canonical.starts_with(&root),
        "site input escaped the owned root"
    );
    dir
}

fn pending_record(auth_dir: &Path, org: &str) -> serde_json::Value {
    let raw =
        std::fs::read(auth_dir.join(format!("web-{org}.json"))).expect("pending record must exist");
    serde_json::from_slice(&raw).expect("pending record must be JSON")
}

/// The release id the verified prepare recorded for this site and digest.
fn release_for_digest(auth_dir: &Path, org: &str, slug: &str, digest: &str) -> String {
    pending_record(auth_dir, org)["releases"]
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|r| r["slug"] == slug && r["manifest_digest"] == digest)
        })
        .and_then(|r| r["release_id"].as_str())
        .map(str::to_owned)
        .expect("the verified prepare must record a release for this digest")
}

/// Non-prepare intents for the site, oldest first: (kind, release id if any).
fn action_intents(auth_dir: &Path, org: &str, slug: &str) -> Vec<(String, Option<String>)> {
    let record = pending_record(auth_dir, org);
    record["intents"]
        .as_array()
        .expect("intents must be a list")
        .iter()
        .filter(|i| i["slug"] == slug && i["kind"] != "prepare")
        .map(|i| {
            assert_eq!(
                i["state"], "done",
                "completed known action intents are durably closed"
            );
            (
                i["kind"].as_str().expect("kind").to_owned(),
                i["release_id"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

/// Library lifecycle (CAD-1429 positive extension). The controller builds two
/// tiny React/Vite releases (`site` = v1, `site-v2` = v2) and copies their
/// `dist` into the owned root. The CLI then drives prepare v1, publish v1,
/// prepare v2, publish v2, rollback to v1 and unpublish. The Node bridge owns
/// each owner approval with the real owner cookie; this process never does.
#[test]
#[ignore = "requires owned API bridge: run by the Node controller with --ignored --exact"]
fn cli_publish_web_real_device_login_spa_library_lifecycle() {
    let ca_pem = std::fs::read(required(ENV_CA_PEM)).expect("read public CA certificate");
    let loopback: SocketAddr = required(ENV_LOOPBACK)
        .parse()
        .expect("loopback socket must be ip:port");
    let slug = required(ENV_SLUG);
    let out_root = PathBuf::from(required(ENV_OUT_DIR));

    let auth_dir = production_auth_dir(&out_root);
    fresh_auth_dir(&auth_dir);
    write_operator_trust_pin(&auth_dir);
    let _tls = install_tls(loopback, &ca_pem);

    let grant = real_login(&auth_dir, &slug);
    grant
        .save_credential(&auth_dir)
        .expect("save the verified grant under the production auth directory");
    let target = target_from_grant(&grant, &slug);
    let site_slug = format!("{slug}-lib");

    let v1_dir = site_input(&out_root, "site");
    let v2_dir = site_input(&out_root, "site-v2");
    let v1_digest = cadence_agent::web_publish::freeze(&v1_dir)
        .expect("v1 build output must freeze locally")
        .digest;
    let v2_digest = cadence_agent::web_publish::freeze(&v2_dir)
        .expect("v2 build output must freeze locally")
        .digest;
    assert_ne!(v1_digest, v2_digest, "v2 must be a different release");

    // 1. First version: prepare, then request publish (owner approves on the bridge).
    expect_ok(
        &out_root,
        run(
            &WebAction::Prepare {
                input: v1_dir.clone(),
                name: site_slug.clone(),
                spa: true,
            },
            &target,
            5,
        ),
        "prepare v1",
    );
    let r1 = release_for_digest(&auth_dir, &target.org, &site_slug, &v1_digest);
    expect_ok(
        &out_root,
        run(
            &WebAction::Publish {
                release: r1.clone(),
            },
            &target,
            5,
        ),
        "publish v1",
    );
    expect_ok(
        &out_root,
        run(
            &WebAction::Status {
                slug: site_slug.clone(),
            },
            &target,
            5,
        ),
        "status after publish v1",
    );
    expect_ok(
        &out_root,
        run(
            &WebAction::Releases {
                slug: site_slug.clone(),
            },
            &target,
            5,
        ),
        "releases after publish v1",
    );

    // 2. Changed second version: prepare the new digest, then publish it.
    expect_ok(
        &out_root,
        run(
            &WebAction::Prepare {
                input: v2_dir.clone(),
                name: site_slug.clone(),
                spa: true,
            },
            &target,
            5,
        ),
        "prepare v2",
    );
    let r2 = release_for_digest(&auth_dir, &target.org, &site_slug, &v2_digest);
    assert_ne!(r1, r2, "second prepare must mint a distinct release");
    expect_ok(
        &out_root,
        run(
            &WebAction::Publish {
                release: r2.clone(),
            },
            &target,
            5,
        ),
        "publish v2",
    );

    // 3. Rollback to the first release, then unpublish.
    expect_ok(
        &out_root,
        run(
            &WebAction::Rollback {
                slug: site_slug.clone(),
                release: r1.clone(),
            },
            &target,
            5,
        ),
        "rollback to v1",
    );
    expect_ok(
        &out_root,
        run(
            &WebAction::Unpublish {
                slug: site_slug.clone(),
            },
            &target,
            5,
        ),
        "unpublish",
    );

    // 4. Final non-secret state: the four requests, in order, one intent each.
    expect_ok(
        &out_root,
        run(
            &WebAction::Status {
                slug: site_slug.clone(),
            },
            &target,
            5,
        ),
        "status after unpublish",
    );
    expect_ok(
        &out_root,
        run(
            &WebAction::Releases {
                slug: site_slug.clone(),
            },
            &target,
            5,
        ),
        "releases after unpublish",
    );
    assert_eq!(
        action_intents(&auth_dir, &target.org, &site_slug),
        vec![
            ("publish".to_owned(), Some(r1.clone())),
            ("publish".to_owned(), Some(r2.clone())),
            ("rollback".to_owned(), Some(r1.clone())),
            ("unpublish".to_owned(), None),
        ],
        "exactly one durable intent per request, in order"
    );
}
