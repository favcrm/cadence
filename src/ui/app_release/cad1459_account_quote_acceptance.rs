//! CAD-1459 TASK-009: independently authored paired native HTTP refusal.
//! Calls the production Quote entry, not a matching guard or fake RPC server.
//! No daemon, board, TLS server or account/provider request is started. The
//! access-only fixture uses the normal private login schema and existing issuer
//! trust metadata, not a media grant. Run only after coordinator admission.

use super::{handle_with_relay, route};
use crate::remote_cli::{account_image_identity, RemoteTarget};
use crate::ui::{operator, ServeOpts};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Request, StatusCode, TestRequest};

const TEST: &str = "ui::app_release::cad1459_account_quote_acceptance::quote_requires_native_browser_proof_and_access_only_login_needs_no_extra_grant";
const CHILD_ROOT: &str = "CAD1459_NATIVE_ACCEPTANCE_ROOT";
const ORG: &str = "lc";
const WORKSPACE: &str = "ws_cad1459_access_only";
const SUBJECT: &str = "user_cad1459_access_only";
const BOARD: &str = "https://lc.cadencecloud.app";
const ISSUER: &str = "https://issuer.board.localhost";
const ACCESS: &str = "hct_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_write(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn quote_requires_native_browser_proof_and_access_only_login_needs_no_extra_grant() {
    // A child isolates filesystem resolution without mutating the test runner's
    // environment or clearing any inherited native caller identity. Its socket
    // is deliberately absent, so an accidental relay cannot reach a real daemon.
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        let root = tempfile::Builder::new()
            .prefix("c1459n-")
            .tempdir_in("/tmp")
            .unwrap();
        for name in ["home", "config", "data", "state", "tmp", "locks"] {
            private_dir(&root.path().join(name));
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--test-threads", "1"])
            .env(CHILD_ROOT, root.path())
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("TMPDIR", root.path().join("tmp"))
            .env("CADENCE_SOCKET", root.path().join("state/cadence.sock"))
            .env("CADENCE_SUITE_LOCK", root.path().join("locks/suite"))
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "isolated native Quote acceptance failed");
        return;
    };
    assert_eq!(root.parent(), Some(Path::new("/tmp")));
    assert!(root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("c1459n-"));
    for (key, relative) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("TMPDIR", "tmp"),
        ("CADENCE_SOCKET", "state/cadence.sock"),
    ] {
        assert_eq!(
            PathBuf::from(std::env::var_os(key).unwrap()),
            root.join(relative)
        );
    }

    let state = root.join("state");
    assert!(!state.join("cadence.sock").exists());
    let opts = ServeOpts {
        port: 3119,
        ..Default::default()
    };
    let path =
        "/api/app-installations/install-cad1459/contexts/context-cad1459/bindings/image/quote";
    // Model/frame assertions are not native browser credentials. There is no
    // Cookie or X-Cadence-Session header, even though the host is legitimate.
    let mut request: Request = TestRequest::new()
        .with_path(path)
        .with_header(Header::from_bytes("Host", operator::board_host(opts.port)).unwrap())
        .with_body(
            r#"{"operator":true,"actor":"operator","creator":"operator","subject_id":"user_cad1459_access_only","account_source":{"org":"lc"},"token":"frame-assertion","key":"frame-assertion","origin":"loopback"}"#,
        )
        .into();
    let quote = route(path).expect("the real context-scoped Quote route must exist");
    let response = handle_with_relay(
        &mut request,
        &state,
        quote,
        false,
        operator::PublicRelay::NotPublic,
        &opts,
    );
    // With no RPC endpoint an attempted relay is unavailable, not a permission
    // refusal. This must be the HTTP guard's 403 BEFORE RPC/binding/provider
    // effects, never authority borrowed from the HTTP server's process PID.
    assert_eq!(
        response.status_code(),
        StatusCode(403),
        "Quote without native browser proof must refuse locally, not reach RPC"
    );
    assert_eq!(fs::read_dir(&state).unwrap().count(), 0);

    let dir = root.join("config/cadence/remote-auth");
    private_dir(&dir);
    private_write(&dir.join("trusted-issuer"), ISSUER.as_bytes());
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    // Ordinary access-only login: no cli_session, hcr_ refresh token, persistent
    // consent, provider capability, platform credential or media enrollment.
    let record = json!({
        "version": "hosted-cadence-auth.v1",
        "organization_id": WORKSPACE,
        "organization_slug": ORG,
        "audience": BOARD,
        "issuer": ISSUER,
        "subject_id": SUBJECT,
        "capabilities": ["cli.read", "cli.write"],
        "access_token": ACCESS,
        "expires_at": expiry,
        "pending_renewal": null
    });
    let bytes = serde_json::to_vec(&record).unwrap();
    let login_path = dir.join(format!("cli-{ORG}.json"));
    private_write(&login_path, &bytes);
    let identity = account_image_identity(&RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: BOARD.into(),
        org_id: WORKSPACE.into(),
    })
    .expect("still-live identifying access-only login must need no extra CLI-session grant");
    assert_eq!(identity.org, ORG);
    assert_eq!(identity.organization_id, WORKSPACE);
    assert_eq!(identity.audience, BOARD);
    assert_eq!(identity.issuer, ISSUER);
    assert_eq!(identity.subject_id, SUBJECT);
    assert!(
        !identity.session_id.is_empty(),
        "source lineage must identify the existing login"
    );
    assert!(
        !serde_json::to_string(&identity).unwrap().contains(ACCESS),
        "custody identity must never expose the bearer"
    );
    assert!(
        fs::read(&login_path).unwrap() == bytes,
        "offline identity must not rewrite the login or add a refresh grant"
    );
    assert_eq!(fs::read_dir(&state).unwrap().count(), 0);
    // No account_image_request is called: this positive half qualifies genuine
    // offline custody metadata only, not funded/provider or installed behavior.
}
