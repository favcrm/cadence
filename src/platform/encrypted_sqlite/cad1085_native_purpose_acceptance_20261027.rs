//! Independent CAD-1085 acceptance for the native model's distinct vault purpose.
//!
//! Child module of encrypted_sqlite. Exercises the actual EncryptedVault and
//! actual manager connection; no copied guard, mocked AEAD, core authority,
//! provider traffic, production path, or pre-existing acceptance edits.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use tempfile::TempDir;
use zeroize::Zeroizing;

use super::{EncryptedCustodyConfig, EncryptedVault, ExpectedIdentity, TAG_PREFIX};

const NATIVE_PURPOSE: &str = "native-model-v1";
const PAYLOAD: &[u8] = b"cad1085-synthetic-native-source";

type CipherRow = (Vec<u8>, Vec<u8>, Vec<u8>);

#[derive(Debug, PartialEq, Eq)]
struct VaultSnapshot {
    total_changes: u64,
    header: (Vec<u8>, Vec<u8>),
    rows: Vec<CipherRow>,
}

fn snapshot(vault: &EncryptedVault) -> VaultSnapshot {
    let inner = vault.inner.lock().expect("fixture manager lock");
    let header = inner
        .conn
        .query_row(
            "SELECT nonce,tag FROM vault_header WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read real key-confirmation header without key bytes");
    let mut statement = inner
        .conn
        .prepare("SELECT locator,nonce,ciphertext FROM records ORDER BY locator")
        .expect("prepare read-only physical ciphertext observation");
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query real ciphertext rows")
        .collect::<rusqlite::Result<Vec<CipherRow>>>()
        .expect("collect real ciphertext rows");
    VaultSnapshot {
        total_changes: inner.conn.total_changes(),
        header,
        rows,
    }
}

fn fixture() -> (TempDir, EncryptedVault, ExpectedIdentity) {
    let dir = tempfile::tempdir().expect("isolated custody fixture directory");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("owner-only fixture traversal");
    let keys = dir.path().join("keys");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&keys)
        .expect("separate owner-only key directory");
    let key_path = keys.join("native-purpose.key");
    let mut key = Zeroizing::new([0u8; 32]);
    getrandom::fill(key.as_mut()).expect("fresh fixture-only key");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)
        .expect("provision fixture key without following/replacing a file");
    file.write_all(key.as_ref())
        .expect("write fixture-only key");
    drop(file);
    drop(key);

    let vault = EncryptedVault::new(EncryptedCustodyConfig {
        key_file: key_path,
        vault_dir: dir.path().join("vault"),
    })
    .expect("construct actual normal encrypted custody");
    let workspace = "ab".repeat(16);
    vault
        .initialize(&workspace, false)
        .expect("initialize a fresh isolated vault without claiming protected core authority");
    let identity = ExpectedIdentity {
        workspace_id: workspace,
        connection_id: "native-source-a".to_string(),
        provider: "devin".to_string(),
        account: "fixture-account".to_string(),
        credential_revision: 1,
        exchange: NATIVE_PURPOSE.to_string(),
        key_id: vault.key_id().expect("actual unlocked key identity"),
    };
    (dir, vault, identity)
}

fn assert_read_refused(
    vault: &EncryptedVault,
    reference: &str,
    expected: &ExpectedIdentity,
    code: &str,
) {
    let before = snapshot(vault);
    let error = vault
        .read_exact(reference, expected)
        .expect_err("the real custody read must refuse this substituted identity");
    assert_eq!(
        error.code(),
        Some(code),
        "require the intended real guard refusal"
    );
    assert_eq!(
        snapshot(vault),
        before,
        "refused read must not mutate custody"
    );
}

#[test]
fn cad1085_native_purpose_roundtrip_and_cross_purpose_refusal() {
    let (_dir, vault, identity) = fixture();
    assert!(snapshot(&vault).rows.is_empty());

    // RED on the old exact allowlist: this must fail as invalid_store before
    // the repair. A guard that refuses every purpose cannot pass this leaf.
    let reference = vault
        .write_row(&identity, PAYLOAD)
        .expect("valid native-model-v1 purpose must reach real AEAD and persist");
    assert!(reference.starts_with(TAG_PREFIX));
    assert_eq!(snapshot(&vault).rows.len(), 1);
    let plaintext = vault
        .read_exact(&reference, &identity)
        .expect("new native purpose must authenticate and decrypt its actual ciphertext");
    assert_eq!(plaintext.as_slice(), PAYLOAD);
    drop(plaintext);

    // Reopening reuses the persisted header/row and the provisioned fixture
    // key. This cannot pass on an in-memory echo instead of real custody.
    vault.lock().expect("lock actual fixture vault");
    vault
        .unlock(&identity.workspace_id)
        .expect("unlock persisted fixture vault");
    assert_eq!(
        vault
            .read_exact(&reference, &identity)
            .expect("persisted native row decrypts")
            .as_slice(),
        PAYLOAD
    );

    // These are admitted BUSINESS purposes, but cannot borrow this row.
    for business in ["token", "smtp", "consent"] {
        let mut wrong = identity.clone();
        wrong.exchange = business.to_string();
        assert_read_refused(&vault, &reference, &wrong, "integrity_failure");
    }

    // Every other AAD/core-identity dimension remains exact as well.
    let mut wrong_workspace = identity.clone();
    wrong_workspace.workspace_id = "cd".repeat(16);
    let mut wrong_source = identity.clone();
    wrong_source.connection_id = "native-source-b".to_string();
    let mut wrong_provider = identity.clone();
    wrong_provider.provider = "github".to_string();
    let mut wrong_account = identity.clone();
    wrong_account.account = "other-account".to_string();
    let mut wrong_revision = identity.clone();
    wrong_revision.credential_revision = 2;
    let mut wrong_key = identity.clone();
    wrong_key.key_id[0] ^= 1;
    for wrong in [
        wrong_workspace,
        wrong_source,
        wrong_provider,
        wrong_account,
        wrong_revision,
        wrong_key,
    ] {
        assert_read_refused(&vault, &reference, &wrong, "integrity_failure");
    }

    // Unknown strings fit the existing length bound, so these assertions
    // exercise the real finite purpose gate rather than an oversized field.
    for purpose in ["native-model-v2", "unreviewed"] {
        let mut wrong = identity.clone();
        wrong.exchange = purpose.to_string();
        let before = snapshot(&vault);
        let error = vault
            .write_row(&wrong, b"another-synthetic-fixture-value")
            .expect_err("unknown purpose must not create ciphertext");
        assert_eq!(error.code(), Some("invalid_store"));
        assert_eq!(
            snapshot(&vault),
            before,
            "unknown-purpose write mutated custody"
        );
        assert_read_refused(&vault, &reference, &wrong, "invalid_store");
    }

    assert_eq!(
        vault
            .read_exact(&reference, &identity)
            .expect("valid native identity remains usable")
            .as_slice(),
        PAYLOAD
    );
    assert_eq!(
        snapshot(&vault).rows.len(),
        1,
        "only the valid native write may publish a row"
    );
    assert_eq!(vault.status()["unlocked"], true);
    assert_eq!(
        vault.status()["protected"],
        false,
        "normal custody primitive must not fabricate isolation"
    );
}
