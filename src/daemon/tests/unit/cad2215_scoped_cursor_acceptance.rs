//! CAD-2215: operator-provisioned scoped cursor keyring acceptance.
//!
//! The only setup operation is the RPC `scoped_cursor_initialize`, which takes
//! no parameters and therefore no caller-selected paths; the internal read
//! handle is `Shared::scoped_cursor_codec()`. Key material and config are
//! fixture-owned files that the daemon never creates, repairs or derives.
//!
//! This exercises the real dispatcher guard (`Shared::dispatch` ->
//! `operator_connection`) for agent and unproven callers, the real store
//! metadata that binds an initialized ring, and the codec's binding and
//! stable-row-identity contract. It does NOT establish OS isolation or
//! private-project availability.

use super::*;
use crate::daemon::scoped_cursor::{
    CursorBinding, CursorDirection, CursorError, CursorKeyId, CursorKeyring, ScopedCursorCodec,
    KEY_BYTES,
};
use crate::test_seam::Asserted;
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const IDENTITY_BYTE: u8 = 0xa1;
const KEY_A_BYTE: u8 = 0xb2;
const KEY_B_BYTE: u8 = 0xc3;
const KEY_C_BYTE: u8 = 0xd4;

const KEY_ID_A: u8 = 0x01;
const KEY_ID_B: u8 = 0x02;
const KEY_ID_C: u8 = 0x03;

fn key_bytes(byte: u8) -> [u8; KEY_BYTES] {
    [byte; KEY_BYTES]
}

/// A key id is 16 raw bytes; its persisted spelling is lowercase hex.
fn id_hex(byte: u8) -> String {
    format!("{byte:02x}").repeat(16)
}

fn key_id(byte: u8) -> CursorKeyId {
    CursorKeyId::from_bytes(&[byte; 16]).unwrap()
}

/// Build the same already-admitted ring the record names, for a test that
/// needs to prove which sealing key a token used.
fn ring_with(ids: &[u8]) -> CursorKeyring {
    let mut ring = CursorKeyring::new();
    for &id in ids {
        let key_byte = match id {
            KEY_ID_A => KEY_A_BYTE,
            KEY_ID_B => KEY_B_BYTE,
            KEY_ID_C => KEY_C_BYTE,
            _ => unreachable!("fixture key id"),
        };
        ring.insert_key(key_id(id), &key_bytes(key_byte)).unwrap();
    }
    ring
}

fn binding() -> CursorBinding<'static> {
    CursorBinding {
        domain: "messages",
        direction: CursorDirection::Forward,
        principal: "agent:reader",
        resource: "thread/1",
        filters: "kind=note,order=asc",
    }
}

fn private_dir(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    drop(file);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Two disjoint temp roots: the daemon state dir and the owner-provisioned key
/// files. Keys must never live inside the state dir or the tracker.
struct CursorFixture {
    state: tempfile::TempDir,
    keys: tempfile::TempDir,
}

impl CursorFixture {
    fn new() -> Self {
        let state = tempfile::tempdir().unwrap();
        private_dir(state.path());
        let keys = tempfile::tempdir().unwrap();
        private_dir(keys.path());
        for (name, byte) in [
            ("identity.key", IDENTITY_BYTE),
            ("cursor-a.key", KEY_A_BYTE),
            ("cursor-b.key", KEY_B_BYTE),
            ("cursor-c.key", KEY_C_BYTE),
        ] {
            write_private(&keys.path().join(name), &key_bytes(byte));
        }
        Self { state, keys }
    }

    fn key_file(&self, name: &str) -> PathBuf {
        self.keys.path().join(name)
    }

    fn config_path(&self) -> PathBuf {
        self.state.path().join("scoped-cursors.json")
    }

    fn shared(&self) -> Arc<Shared> {
        self.shared_at(self.state.path())
    }

    /// A fresh `Shared` over an arbitrary temp state root (the fixture key
    /// files stay outside it). Two such roots are two real store workspaces.
    fn shared_at(&self, state_dir: &Path) -> Arc<Shared> {
        let opts = ServeOptions::default();
        opts.provider_env.set(
            "CADENCE_PM_DIR",
            state_dir.join("tracker").to_str().unwrap(),
        );
        Shared::new(state_dir, &opts).unwrap()
    }

    fn config(&self, active: u8, keys: Vec<(u8, PathBuf)>) -> Value {
        json!({
            "version": 1,
            "identity_key_file": self.key_file("identity.key").to_str().unwrap(),
            "active_key_id": id_hex(active),
            "keys": keys
                .into_iter()
                .map(|(id, path)| json!({
                    "id": id_hex(id),
                    "key_file": path.to_str().unwrap(),
                }))
                .collect::<Vec<_>>(),
        })
    }

    fn good_config(&self, active: u8) -> Value {
        self.config(
            active,
            vec![
                (KEY_ID_A, self.key_file("cursor-a.key")),
                (KEY_ID_B, self.key_file("cursor-b.key")),
            ],
        )
    }

    fn write_config(&self, value: &Value) {
        self.write_config_at(self.state.path(), value);
    }

    fn write_config_at(&self, state_dir: &Path, value: &Value) {
        let path = state_dir.join("scoped-cursors.json");
        // Also clears a symlink left by the unsafe-config case.
        let _ = std::fs::remove_file(&path);
        write_private(&path, &serde_json::to_vec(value).unwrap());
    }

    fn write_good_config(&self, active: u8) {
        let value = self.good_config(active);
        self.write_config(&value);
    }
}

fn init_as(shared: &Arc<Shared>, who: Asserted, params: &Value) -> Result<Value> {
    crate::test_seam::scoped(who, || {
        shared.dispatch("scoped_cursor_initialize", params, std::process::id())
    })
}

fn operator_init(shared: &Arc<Shared>) -> Result<Value> {
    init_as(shared, Asserted::Operator, &json!({}))
}

/// A refusal must not carry token or key material. The checks are shared by
/// the structured and codec refusals so neither surface can start leaking.
fn assert_no_secret_material(rendered: &str, what: &str) {
    for needle in ["csc1_", "csfk_", "csrid_"] {
        assert!(
            !rendered.contains(needle),
            "{what}: refusal leaked {needle}: {rendered}"
        );
    }
    for byte in [IDENTITY_BYTE, KEY_A_BYTE, KEY_B_BYTE, KEY_C_BYTE] {
        let hex = format!("{byte:02x}").repeat(32);
        assert!(
            !rendered.contains(&hex),
            "{what}: refusal leaked key material: {rendered}"
        );
    }
}

/// Every config/material failure is the one structured refusal (the stable
/// consumer contract is the kind/code, not prose), and it names no key,
/// token, position or path.
fn assert_unavailable(error: Error) {
    assert_eq!(error.kind(), "rejected", "{error:?}");
    assert_eq!(error.code(), Some("scoped_cursor_unavailable"), "{error:?}");
    assert_no_secret_material(&format!("{error:?} {error}"), "scoped cursor");
}

/// A dispatcher/authority refusal: the class is the stable contract, and the
/// rendered error must not disclose key or token material.
fn assert_rejected_without_secrets(error: Error, what: &str) {
    assert_eq!(error.kind(), "rejected", "{what}: {error:?}");
    assert_no_secret_material(&format!("{error:?} {error}"), what);
}

fn assert_init_unavailable(shared: &Arc<Shared>) {
    let error = operator_init(shared).expect_err("initialize must fail closed");
    assert_unavailable(error);
}

fn assert_codec_unavailable(shared: &Arc<Shared>) {
    let error = shared
        .scoped_cursor_codec()
        .expect_err("no codec may be admitted");
    assert_unavailable(error);
}

/// One generic codec refusal: the error type is the contract, and no
/// position, token or key material is rendered.
fn assert_cursor_refusal<T: std::fmt::Debug>(
    result: std::result::Result<T, CursorError>,
    what: &str,
) {
    let error = result.expect_err(what);
    assert_no_secret_material(&format!("{error} {error:?}"), what);
}

fn altered(token: &str) -> String {
    let mut bytes = token.as_bytes().to_vec();
    let at = bytes.len() / 2;
    bytes[at] = if bytes[at] == b'A' { b'B' } else { b'A' };
    String::from_utf8(bytes).unwrap()
}

#[test]
fn operator_initializes_repeats_and_recovers_cursors_across_restart() {
    let fixture = CursorFixture::new();
    fixture.write_good_config(KEY_ID_A);
    let shared = fixture.shared();

    // Nothing is admitted before the operator initializes the workspace.
    assert_codec_unavailable(&shared);

    let first = operator_init(&shared).unwrap();
    assert_eq!(first["initialized"], json!(true));
    let fingerprint = first["fingerprint"].as_str().unwrap().to_string();

    let codec = shared.scoped_cursor_codec().unwrap();
    assert_eq!(codec.keyring_fingerprint(), fingerprint);
    assert_eq!(codec.verify_keyring_fingerprint(&fingerprint), Ok(()));
    assert_eq!(
        codec.verify_keyring_fingerprint("csfk_0000000000000000000000000000000000000000000"),
        Err(CursorError)
    );
    let scope = binding();
    let token = codec.seal(41, &scope).unwrap();
    assert_eq!(codec.open(&token, &scope).unwrap(), 41);

    // The same config is safe to initialize again: same ring, same fingerprint.
    let again = operator_init(&shared).unwrap();
    assert_eq!(again["initialized"], json!(false), "{again}");
    assert_eq!(again["fingerprint"].as_str(), Some(fingerprint.as_str()));
    assert_eq!(
        shared
            .scoped_cursor_codec()
            .unwrap()
            .open(&token, &scope)
            .unwrap(),
        41
    );

    // Cursors are not OS isolation; private projects stay disabled.
    assert!(!crate::issue::write::private_available());

    drop(codec);
    drop(shared);

    // A restarted Shared on the same workspace recovers the old token.
    let restarted = fixture.shared();
    let recovered = restarted.scoped_cursor_codec().unwrap();
    assert_eq!(recovered.keyring_fingerprint(), fingerprint);
    assert_eq!(recovered.open(&token, &scope).unwrap(), 41);
    let settled = operator_init(&restarted).unwrap();
    assert_eq!(settled["initialized"], json!(false), "{settled}");
    assert_eq!(settled["fingerprint"].as_str(), Some(fingerprint.as_str()));
}

#[test]
fn dispatcher_refuses_agents_and_unproven_callers_without_recording_initialization() {
    let fixture = CursorFixture::new();
    fixture.write_good_config(KEY_ID_A);
    let shared = fixture.shared();

    let agent = init_as(&shared, Asserted::Agent("worker".to_string()), &json!({}))
        .expect_err("an agent must never initialize the keyring");
    assert_rejected_without_secrets(agent, "agent caller");
    assert_codec_unavailable(&shared);

    let unproven = init_as(&shared, Asserted::Unproven, &json!({}))
        .expect_err("an unproven caller must never initialize the keyring");
    assert_rejected_without_secrets(unproven, "unproven caller");
    assert_codec_unavailable(&shared);

    // No caller-selected paths: the setup operation refuses a parameter bag
    // and records nothing.
    let key_path = fixture.key_file("cursor-a.key");
    let extra = init_as(
        &shared,
        Asserted::Operator,
        &json!({ "key_file": key_path.to_str().unwrap() }),
    )
    .expect_err("the setup operation must refuse a caller-selected path");
    assert_rejected_without_secrets(extra, "caller-selected path");
    assert_codec_unavailable(&shared);

    // Only now does the operator initialize: the refusals above recorded nothing.
    let first = operator_init(&shared).unwrap();
    assert_eq!(first["initialized"], json!(true), "{first}");
}

#[test]
fn absent_malformed_unsafe_missing_and_wrong_length_material_never_yield_a_codec_or_record_initialization(
) {
    let fixture = CursorFixture::new();
    let shared = fixture.shared();
    let config = fixture.good_config(KEY_ID_A);

    // Absent config.
    assert!(!fixture.config_path().exists());
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // Malformed JSON.
    write_private(&fixture.config_path(), b"{\"version\":1,");
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // Config is not owner-only.
    fixture.write_config(&config);
    std::fs::set_permissions(
        fixture.config_path(),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // Config is a symlink.
    let real_config = fixture.key_file("real-config.json");
    write_private(&real_config, &serde_json::to_vec(&config).unwrap());
    let _ = std::fs::remove_file(fixture.config_path());
    symlink(&real_config, fixture.config_path()).unwrap();
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // A key file the config names is absent.
    fixture
        .write_config(&fixture.config(KEY_ID_A, vec![(KEY_ID_A, fixture.key_file("absent.key"))]));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // A short key file.
    let short = fixture.key_file("short.key");
    write_private(&short, &[0x11u8; KEY_BYTES - 1]);
    fixture.write_config(&fixture.config(KEY_ID_A, vec![(KEY_ID_A, short)]));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // An oversized key file.
    let long = fixture.key_file("long.key");
    write_private(&long, &[0x11u8; KEY_BYTES + 1]);
    fixture.write_config(&fixture.config(KEY_ID_A, vec![(KEY_ID_A, long)]));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // A key file that is not owner-only.
    let open_mode = fixture.key_file("open-mode.key");
    write_private(&open_mode, &key_bytes(KEY_C_BYTE));
    std::fs::set_permissions(&open_mode, std::fs::Permissions::from_mode(0o644)).unwrap();
    fixture.write_config(&fixture.config(KEY_ID_A, vec![(KEY_ID_A, open_mode)]));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // A symlinked key file.
    let link = fixture.key_file("link.key");
    symlink(fixture.key_file("cursor-a.key"), &link).unwrap();
    fixture.write_config(&fixture.config(KEY_ID_A, vec![(KEY_ID_A, link)]));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // A key file inside the daemon state dir is refused by containment.
    let inside = fixture.state.path().join("inside.key");
    write_private(&inside, &key_bytes(KEY_C_BYTE));
    fixture.write_config(&fixture.config(KEY_ID_A, vec![(KEY_ID_A, inside)]));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // A key file under a reserved artifact component is refused even though
    // the file itself is owner-only and outside the state dir.
    let reserved_dir = fixture.keys.path().join("backups");
    std::fs::create_dir(&reserved_dir).unwrap();
    private_dir(&reserved_dir);
    let reserved_key = reserved_dir.join("cursor.key");
    write_private(&reserved_key, &key_bytes(KEY_C_BYTE));
    fixture.write_config(&fixture.config(KEY_ID_A, vec![(KEY_ID_A, reserved_key)]));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // The identity key path obeys the same reserved-component rule.
    let reserved_identity_dir = fixture.keys.path().join("exports");
    std::fs::create_dir(&reserved_identity_dir).unwrap();
    private_dir(&reserved_identity_dir);
    let reserved_identity = reserved_identity_dir.join("identity.key");
    write_private(&reserved_identity, &key_bytes(IDENTITY_BYTE));
    let mut reserved_identity_config = fixture.good_config(KEY_ID_A);
    reserved_identity_config["identity_key_file"] = json!(reserved_identity.to_str().unwrap());
    fixture.write_config(&reserved_identity_config);
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // An empty retained key set.
    fixture.write_config(&fixture.config(KEY_ID_A, Vec::new()));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // Duplicate key ids.
    fixture.write_config(&fixture.config(
        KEY_ID_A,
        vec![
            (KEY_ID_A, fixture.key_file("cursor-a.key")),
            (KEY_ID_A, fixture.key_file("cursor-b.key")),
        ],
    ));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // The identity key may not double as an AEAD key file.
    let mut reused = fixture.good_config(KEY_ID_A);
    reused["identity_key_file"] = json!(fixture.key_file("cursor-a.key").to_str().unwrap());
    fixture.write_config(&reused);
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // A short identity key file.
    let short_identity = fixture.key_file("short-identity.key");
    write_private(&short_identity, &[0x11u8; KEY_BYTES - 1]);
    let mut short_identity_config = fixture.good_config(KEY_ID_A);
    short_identity_config["identity_key_file"] = json!(short_identity.to_str().unwrap());
    fixture.write_config(&short_identity_config);
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // The active key id must be one of the retained keys.
    fixture.write_config(&fixture.config(
        KEY_ID_C,
        vec![
            (KEY_ID_A, fixture.key_file("cursor-a.key")),
            (KEY_ID_B, fixture.key_file("cursor-b.key")),
        ],
    ));
    assert_init_unavailable(&shared);
    assert_codec_unavailable(&shared);

    // None of the refusals recorded an initialization.
    fixture.write_good_config(KEY_ID_A);
    let first = operator_init(&shared).unwrap();
    assert_eq!(first["initialized"], json!(true), "{first}");
    let fingerprint = first["fingerprint"].as_str().unwrap().to_string();
    let codec = shared.scoped_cursor_codec().unwrap();
    assert_eq!(codec.keyring_fingerprint(), fingerprint);
    drop(codec);

    // Corrupting admitted material after initialization fails closed and does
    // not reset the persisted binding; restoring it confirms the same ring.
    let key_a = fixture.key_file("cursor-a.key");
    std::fs::set_permissions(&key_a, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_codec_unavailable(&shared);
    assert_init_unavailable(&shared);
    std::fs::set_permissions(&key_a, std::fs::Permissions::from_mode(0o600)).unwrap();
    let settled = operator_init(&shared).unwrap();
    assert_eq!(settled["initialized"], json!(false), "{settled}");
    assert_eq!(settled["fingerprint"].as_str(), Some(fingerprint.as_str()));
    assert_eq!(
        shared.scoped_cursor_codec().unwrap().keyring_fingerprint(),
        fingerprint
    );

    // An existing but unsafe tracker root fails closed before the key overlap
    // comparison, so it cannot be re-pointed at the key directory.
    let tracker = fixture.state.path().join("tracker");
    assert!(!tracker.exists(), "fixture tracker root must start absent");
    symlink(fixture.keys.path(), &tracker).unwrap();
    assert_codec_unavailable(&shared);
    assert_init_unavailable(&shared);
    std::fs::remove_file(&tracker).unwrap();

    // A group-writable tracker root is equally refused.
    std::fs::create_dir(&tracker).unwrap();
    std::fs::set_permissions(&tracker, std::fs::Permissions::from_mode(0o775)).unwrap();
    assert_codec_unavailable(&shared);
    assert_init_unavailable(&shared);
    std::fs::remove_dir(&tracker).unwrap();

    // With the tracker absent again, the same ring still confirms.
    let restored = operator_init(&shared).unwrap();
    assert_eq!(restored["initialized"], json!(false), "{restored}");
    assert_eq!(restored["fingerprint"].as_str(), Some(fingerprint.as_str()));
    assert_eq!(
        shared.scoped_cursor_codec().unwrap().keyring_fingerprint(),
        fingerprint
    );
}

#[test]
fn a_different_key_set_after_initialization_fails_closed_without_resetting_metadata() {
    let fixture = CursorFixture::new();
    fixture.write_good_config(KEY_ID_A);
    let shared = fixture.shared();
    let first = operator_init(&shared).unwrap();
    let fingerprint = first["fingerprint"].as_str().unwrap().to_string();
    let scope = binding();
    let token = {
        let codec = shared.scoped_cursor_codec().unwrap();
        let token = codec.seal(7, &scope).unwrap();
        assert_eq!(codec.open(&token, &scope).unwrap(), 7);
        token
    };
    drop(shared);

    // Same active id, different retained set: neither init nor the read handle
    // may accept it, and neither may rewrite the persisted fingerprint.
    fixture.write_config(&fixture.config(
        KEY_ID_A,
        vec![
            (KEY_ID_A, fixture.key_file("cursor-a.key")),
            (KEY_ID_C, fixture.key_file("cursor-c.key")),
        ],
    ));
    let mismatched = fixture.shared();
    assert_init_unavailable(&mismatched);
    assert_codec_unavailable(&mismatched);

    // The original set still confirms as already initialized with the same
    // fingerprint, so the failed attempt reset nothing.
    fixture.write_good_config(KEY_ID_A);
    let settled = operator_init(&mismatched).unwrap();
    assert_eq!(settled["initialized"], json!(false), "{settled}");
    assert_eq!(settled["fingerprint"].as_str(), Some(fingerprint.as_str()));
    let recovered = mismatched.scoped_cursor_codec().unwrap();
    assert_eq!(recovered.keyring_fingerprint(), fingerprint);
    assert_eq!(recovered.open(&token, &scope).unwrap(), 7);
}

#[test]
fn active_key_switches_among_the_retained_set_and_opens_old_cursors() {
    let fixture = CursorFixture::new();
    // Both keys are retained; B is active first.
    fixture.write_good_config(KEY_ID_B);
    let shared = fixture.shared();
    let first = operator_init(&shared).unwrap();
    let fingerprint = first["fingerprint"].as_str().unwrap().to_string();
    let scope = binding();
    let old_token = {
        let codec = shared.scoped_cursor_codec().unwrap();
        assert_eq!(codec.keyring_fingerprint(), fingerprint);
        codec.seal(99, &scope).unwrap()
    };
    drop(shared);

    // Same retained set, active key switched: the fingerprint excludes the
    // active id, so the persisted check still holds and old cursors open.
    fixture.write_good_config(KEY_ID_A);
    let switched = fixture.shared();
    let settled = operator_init(&switched).unwrap();
    assert_eq!(settled["initialized"], json!(false), "{settled}");
    assert_eq!(settled["fingerprint"].as_str(), Some(fingerprint.as_str()));
    let codec = switched.scoped_cursor_codec().unwrap();
    assert_eq!(codec.verify_keyring_fingerprint(&fingerprint), Ok(()));
    assert_eq!(codec.keyring_fingerprint(), fingerprint);

    let new_token = codec.seal(99, &scope).unwrap();
    assert_eq!(codec.open(&new_token, &scope).unwrap(), 99);
    assert_eq!(codec.open(&old_token, &scope).unwrap(), 99);

    // Consumer-level proof of sealing-key selection. The workspace comes from
    // the store's own metadata, never a codec getter: the old token is sealed
    // under B (B-only opens it, A-only refuses it) and the token sealed after
    // the switch is under A (A-only opens it, B-only refuses it), while the
    // full retained ring opens both.
    let (workspace, _) = switched.store.cursor_keyring_metadata().unwrap();
    let identity = key_bytes(IDENTITY_BYTE);
    let a_only = ScopedCursorCodec::new(
        &workspace,
        ring_with(&[KEY_ID_A]),
        key_id(KEY_ID_A),
        &identity,
    )
    .unwrap();
    let b_only = ScopedCursorCodec::new(
        &workspace,
        ring_with(&[KEY_ID_B]),
        key_id(KEY_ID_B),
        &identity,
    )
    .unwrap();
    let full = ScopedCursorCodec::new(
        &workspace,
        ring_with(&[KEY_ID_A, KEY_ID_B]),
        key_id(KEY_ID_A),
        &identity,
    )
    .unwrap();
    assert_eq!(a_only.open(&new_token, &scope).unwrap(), 99);
    assert_cursor_refusal(a_only.open(&old_token, &scope), "old token sealed under B");
    assert_eq!(b_only.open(&old_token, &scope).unwrap(), 99);
    assert_cursor_refusal(b_only.open(&new_token, &scope), "new token sealed under A");
    assert_eq!(full.open(&new_token, &scope).unwrap(), 99);
    assert_eq!(full.open(&old_token, &scope).unwrap(), 99);
    assert_eq!(full.verify_keyring_fingerprint(&fingerprint), Ok(()));
}

#[test]
fn admitted_codec_round_trips_positions_and_binds_tokens_to_their_scope() {
    let fixture = CursorFixture::new();
    fixture.write_good_config(KEY_ID_A);
    let shared = fixture.shared();
    let initialized = operator_init(&shared).unwrap();
    assert_eq!(initialized["initialized"], json!(true));
    let codec = shared.scoped_cursor_codec().unwrap();
    let scope = binding();

    for position in [0, 1, 4096, i64::MAX] {
        let token = codec.seal(position, &scope).unwrap();
        assert_eq!(codec.open(&token, &scope).unwrap(), position);
    }
    assert_cursor_refusal(codec.seal(-1, &scope), "negative position");
    assert_cursor_refusal(codec.seal(i64::MIN, &scope), "minimum position");

    let token = codec.seal(1234, &scope).unwrap();

    // Altered, truncated, oversized, padded, wrong-prefix and non-base64
    // tokens are all the same refusal.
    assert_cursor_refusal(codec.open(&altered(&token), &scope), "altered token");
    assert_cursor_refusal(
        codec.open(&token[..token.len() - 1], &scope),
        "truncated token",
    );
    assert_cursor_refusal(codec.open(&format!("{token}A"), &scope), "oversized token");
    assert_cursor_refusal(codec.open(&format!("{token}="), &scope), "padded token");
    assert_cursor_refusal(
        codec.open(&token.replacen("csc1_", "csc2_", 1), &scope),
        "wrong prefix",
    );
    assert_cursor_refusal(codec.open("", &scope), "empty token");
    assert_cursor_refusal(
        codec.open(&"!".repeat(token.len()), &scope),
        "invalid base64",
    );

    // Every bound field is authenticated: another principal, domain,
    // direction, resource or filter never opens this token.
    for (what, wrong) in [
        (
            "wrong principal",
            CursorBinding {
                principal: "agent:other",
                ..scope
            },
        ),
        (
            "wrong domain",
            CursorBinding {
                domain: "jobs",
                ..scope
            },
        ),
        (
            "wrong direction",
            CursorBinding {
                direction: CursorDirection::Backward,
                ..scope
            },
        ),
        (
            "wrong resource",
            CursorBinding {
                resource: "thread/2",
                ..scope
            },
        ),
        (
            "wrong filters",
            CursorBinding {
                filters: "kind=other,order=asc",
                ..scope
            },
        ),
        (
            "empty filters",
            CursorBinding {
                filters: "",
                ..scope
            },
        ),
    ] {
        assert_cursor_refusal(codec.open(&token, &wrong), what);
    }

    // A second real store workspace over the same owner-provisioned key files
    // cannot open this workspace's cursors, and its own cursors do not open
    // here. Its row identities are namespaced apart as well.
    let foreign_state = tempfile::tempdir().unwrap();
    private_dir(foreign_state.path());
    fixture.write_config_at(foreign_state.path(), &fixture.good_config(KEY_ID_A));
    let foreign = fixture.shared_at(foreign_state.path());
    assert_eq!(operator_init(&foreign).unwrap()["initialized"], json!(true));
    let (workspace, _) = shared.store.cursor_keyring_metadata().unwrap();
    let (foreign_workspace, _) = foreign.store.cursor_keyring_metadata().unwrap();
    assert_ne!(foreign_workspace, workspace);
    let foreign_codec = foreign.scoped_cursor_codec().unwrap();
    let foreign_token = foreign_codec.seal(1234, &scope).unwrap();
    assert_eq!(foreign_codec.open(&foreign_token, &scope).unwrap(), 1234);
    assert_cursor_refusal(
        codec.open(&foreign_token, &scope),
        "foreign workspace token",
    );
    assert_cursor_refusal(
        foreign_codec.open(&token, &scope),
        "token from the foreign workspace",
    );

    let row = 7i64.to_be_bytes();
    let row_id = codec.stable_row_id("messages", "thread/1", &row).unwrap();
    assert!(row_id.starts_with("csrid_"), "{row_id}");
    assert_eq!(
        codec.stable_row_id("messages", "thread/1", &row).unwrap(),
        row_id
    );
    assert_ne!(
        codec.stable_row_id("messages", "thread/2", &row).unwrap(),
        row_id
    );
    assert_ne!(
        codec.stable_row_id("jobs", "thread/1", &row).unwrap(),
        row_id
    );
    assert_ne!(
        codec
            .stable_row_id("messages", "thread/1", &8i64.to_be_bytes())
            .unwrap(),
        row_id
    );
    assert_ne!(
        foreign_codec
            .stable_row_id("messages", "thread/1", &row)
            .unwrap(),
        row_id
    );
    assert_cursor_refusal(
        codec.stable_row_id("messages", "thread/1", &[]),
        "empty row",
    );
}
