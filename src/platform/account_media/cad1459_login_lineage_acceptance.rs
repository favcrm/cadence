//! CAD-1459 TASK-010, independently authored from the login-lifetime contract.
//! Ordinary same-account login recovers the durable connection, never an old
//! paid intent. Exercise the actual adapter execute hook and specific pre-I/O
//! quote/session refusal. This does not qualify native actor ownership, funding
//! or provider behavior. Source implementers must not edit/weaken this driver.
//! Execution requires source completion and coordinator native admission.

use super::{account_handle, AccountMediaAdapter, PLATFORM};
use crate::issuer_transport::test_fixture::{self, Fixture};
use crate::platform::{AppCapabilityQuote, ImageReason, PlatformAdapter};
use crate::remote_cli::{account_image_identity, AccountImageIdentity, RemoteTarget};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const TEST: &str = "platform::account_media::cad1459_login_lineage_acceptance::ordinary_relogin_keeps_connection_but_refuses_old_quoted_session";
const CHILD_ROOT: &str = "CAD1459_LINEAGE_ACCEPTANCE_ROOT";
const ORG: &str = "lc";
const WORKSPACE: &str = "ws_cad1459_lineage";
const SUBJECT: &str = "user_cad1459_lineage";
const BOARD: &str = "https://lc.cadencecloud.app";
const ISSUER: &str = "https://issuer.board.localhost";
const MAXIMUM: u64 = 43_219;

fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_write(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn stable_source(identity: &AccountImageIdentity) -> Value {
    json!({
        "org": identity.org,
        "organization_id": identity.organization_id,
        "audience": identity.audience,
        "issuer": identity.issuer,
        "subject_id": identity.subject_id
    })
}

fn digest(value: &impl serde::Serialize) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

#[test]
fn ordinary_relogin_keeps_connection_but_refuses_old_quoted_session() {
    // As in the frozen native driver: isolate file resolution in a child,
    // preserving all inherited native caller identity and toolchain variables.
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        let root = tempfile::Builder::new()
            .prefix("c1459l-")
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
        assert!(status.success(), "isolated login-lineage acceptance failed");
        return;
    };
    assert_eq!(root.parent(), Some(Path::new("/tmp")));
    assert!(root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("c1459l-"));
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

    // No TLS/provider server. The existing fixture routes even an accidental
    // regression to this owned socket, never DNS/an account. A disposable CA
    // keeps TLS validation real; no invalid-cert or test-mode guard bypass.
    let pki = root.join("tmp/pki");
    private_dir(&pki);
    let ca_status = Command::new("openssl")
        .current_dir(&pki)
        .args([
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "2",
            "-subj",
            "/CN=CAD-1459 lineage acceptance CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(ca_status.success(), "disposable CA creation failed");
    fs::set_permissions(pki.join("ca.key"), fs::Permissions::from_mode(0o600)).unwrap();
    let ca_der = rustls_pemfile::certs(&mut BufReader::new(
        fs::File::open(pki.join("ca.pem")).unwrap(),
    ))
    .next()
    .unwrap()
    .unwrap()
    .as_ref()
    .to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let _transport = test_fixture::install(Fixture {
        ca_der,
        loopback: listener.local_addr().unwrap(),
    });

    let dir = root.join("config/cadence/remote-auth");
    private_dir(&dir);
    private_write(&dir.join("trusted-issuer"), ISSUER.as_bytes());
    let registry = json!({"selected":{"org":"local"},"connections":[
        {"selection":{"org":ORG},"destination":{"mode":"remote","endpoint":BOARD,"org_id":WORKSPACE}},
        {"selection":{"org":"local"},"destination":{"mode":"local","state_dir":root.join("state"),"tracker_dir":root.join("pm")}}
    ]});
    private_write(
        &root.join("config/cadence/orgs.json"),
        &serde_json::to_vec(&registry).unwrap(),
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // The normal access-only schema: no refresh grant or provider enrollment.
    let login = json!({
        "version":"hosted-cadence-auth.v1","organization_id":WORKSPACE,
        "organization_slug":ORG,"audience":BOARD,"issuer":ISSUER,"subject_id":SUBJECT,
        "capabilities":["cli.read","cli.write"],
        "access_token":format!("hct_{}", "A".repeat(43)),
        "expires_at":now + 300,"pending_renewal":null
    });
    let login_path = dir.join(format!("cli-{ORG}.json"));
    private_write(&login_path, &serde_json::to_vec(&login).unwrap());
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: BOARD.into(),
        org_id: WORKSPACE.into(),
    };
    let before =
        account_image_identity(&target).expect("normal login must yield genuine custody identity");
    let handle = account_handle(&before);
    let adapter = AccountMediaAdapter::new().unwrap();
    let descriptor = adapter.connection_descriptor().unwrap();
    descriptor.validate(adapter.table()).unwrap();
    assert_eq!(descriptor.builtin_accounts, vec![handle.clone()]);
    assert!(adapter.app_credentialless_account(&handle));
    let mapping = descriptor
        .resolve_action("media.generate", 1, "generate_image", "connection_account")
        .unwrap();
    // Synthetic local namespace, not an operator/creator/payer claim. All
    // account authority and action metadata come from actual custody/descriptor.
    let connection = format!(
        "builtin-{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("cad1459-lineage-local:{PLATFORM}:{handle}").as_bytes()
        )
        .simple()
    );
    let config = json!({
        "schema":1,"install_id":"install-cad1459-lineage",
        "provider":descriptor.provider,"descriptor_revision":descriptor.revision,
        "connection_id":connection,"connection_kind":"builtin","account":handle,
        "account_source":stable_source(&before),"mapping":mapping,
        "sink_registration":adapter.connection_registration().unwrap()
    });
    assert_eq!(config["account_source"].as_object().unwrap().len(), 5);
    assert!(
        adapter.bound_adapter(&[], &config).is_ok(),
        "original stable binding must be valid before re-login"
    );

    // Freeze exactly the contract's two SHA256s, not raw session/source refs.
    let canonical_price = json!({"kind":"image","model":"openai/gpt-image-2.5","price":{
        "slug":"generate_image","chargeMinor":MAXIMUM,"currency":"USD","version":"cad1459-lineage-v1"
    }});
    let quote = AppCapabilityQuote {
        schema: 1,
        currency: "USD".into(),
        unit_price_micros: MAXIMUM,
        units: 1,
        total_price_micros: MAXIMUM,
        price_revision: format!("{}{}", digest(&before), digest(&canonical_price)),
    };
    assert!(quote.valid());
    assert_eq!(quote.price_revision.len(), 128);
    assert!(quote.price_revision.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(&quote.price_revision[64..], digest(&canonical_price));
    let mut authority = json!({
        "schema":1,"install_id":config["install_id"],"binding":{"config":config},
        "source":null,"inputs":{"subject":"Synthetic harbour illustration",
            "source":"The synthetic harbour has a red lighthouse.",
            "brand_voice":"Calm and clear","image_prompt":"Use an editorial palette"},
        "quote":quote
    });
    let input = json!({});
    let prompt = crate::platform::agenticos_external::frozen_image_prompt(&authority, &input)
        .expect("the frozen intent must pass the real image-plan validator");
    assert!(!prompt.is_empty());
    authority["frozen_prompt"] = json!(prompt);

    // A second ordinary login changes bearer, expiry and custody-derived
    // access-session lineage, not any stable principal/account field.
    let mut relogin = login.clone();
    relogin["access_token"] = json!(format!("hct_{}", "B".repeat(43)));
    relogin["expires_at"] = json!(now + 900);
    let current_bytes = serde_json::to_vec(&relogin).unwrap();
    private_write(&login_path, &current_bytes);
    let current = account_image_identity(&target).expect("ordinary re-login must remain eligible");
    assert_ne!(
        before, current,
        "full execution identity must track login lineage"
    );
    assert_ne!(before.session_id, current.session_id);
    assert_ne!(before.absolute_expires_at, current.absolute_expires_at);
    assert_eq!(stable_source(&before), stable_source(&current));
    assert_eq!(
        account_handle(&current),
        handle,
        "normal login must not require media/provider rebind"
    );
    assert_ne!(&quote.price_revision[..64], digest(&current));

    let mut other_principal = relogin.clone();
    other_principal["subject_id"] = json!("user_cad1459_different");
    private_write(&login_path, &serde_json::to_vec(&other_principal).unwrap());
    let other = account_image_identity(&target)
        .expect("synthetic replacement must be a valid distinct principal");
    assert_ne!(
        account_handle(&other),
        handle,
        "another principal must not inherit the durable account handle"
    );
    private_write(&login_path, &current_bytes);

    // Anti-false-pass: the ORIGINAL unchanged stable config resolves CURRENT
    // custody and a live credentialless connection. A stale binding, bad plan,
    // absent credential or generic transport failure cannot satisfy this case.
    assert_eq!(
        adapter.connection_descriptor().unwrap().builtin_accounts,
        vec![handle.clone()]
    );
    assert!(adapter.app_credentialless_account(&handle));
    assert!(
        adapter
            .bound_adapter(&[], &authority["binding"]["config"])
            .is_ok(),
        "same-account re-login must recover the original stable binding"
    );
    assert_eq!(
        crate::platform::agenticos_external::frozen_image_prompt(&authority, &input).unwrap(),
        prompt
    );
    let failure =
        match adapter.execute_app_image(&[], &authority, &input, "cad1459-frozen-lineage-key") {
            Err(failure) => failure,
            Ok(_) => panic!("ordinary re-login revived an old quoted image intent"),
        };
    assert_eq!(failure.reason, ImageReason::NotAuthorized);
    assert!(
        failure.not_executed,
        "stale session must be a known refusal, never uncertain"
    );
    let detail = failure.detail.to_ascii_lowercase();
    assert!(
        detail.contains("quote") && detail.contains("session") && detail.contains("stale"),
        "must reach the specific stale-quote SESSION guard, not binding/plan/transport refusal"
    );
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "stale quote must refuse before any renewal/lease/provider connection"
    );
    assert!(
        fs::read(&login_path).unwrap() == current_bytes,
        "stale execution must not renew or rewrite the normal login"
    );
}
