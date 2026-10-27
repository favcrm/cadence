//! Independent CAD-1085 acceptance for the real runtime eligibility epoch guard.
//!
//! Child of protected_connections. Uses a real temporary Shared and migrated
//! Store. Fixture evidence is input to the post-authentication guard only:
//! this test does not mint or mock an authenticated root-owner capability.

use cadence_core::connection_dispatch::dto::{FenceStateV1, RuntimeProfileV1};
use cadence_core::connection_dispatch::{Digest, Revision, UuidV1};
use tempfile::TempDir;

use super::{OwnerEligibility, ProtectedConnections};
use crate::daemon::{ServeOptions, Shared};

#[test]
fn cad1085_runtime_eligibility_rejects_mismatched_database_epoch() {
    let dir = TempDir::new().expect("isolated Shared fixture directory");
    let opts = ServeOptions::default();
    let absent_pm = dir.path().join("no-pm");
    opts.provider_env.set(
        "CADENCE_PM_DIR",
        absent_pm.to_str().expect("fixture path encoding"),
    );
    let shared = Shared::new(dir.path(), &opts)
        .expect("ordinary temporary Shared fixture without a production runtime owner");

    // Real persisted guard preconditions, seeded through the repository's
    // bounded test-only Store facade. No fake checkpoint/owner is installed.
    shared
        .store
        .fixture_write(|tx| {
            let changed = tx.execute(
                "UPDATE connection_dispatch_config \
                 SET enabled=1,ever_enabled=1,fenced=0 WHERE singleton=1",
                [],
            )?;
            assert_eq!(changed, 1, "seed the actual singleton guard precondition");
            Ok(())
        })
        .expect("seed enabled and unfenced eligibility fixture");
    let authority = shared
        .store
        .protected_authority()
        .expect("read real core authority");
    assert!(authority.enabled && authority.ever_enabled && !authority.fenced);

    let core_epoch = authority.database_epoch.get();
    let distinct_owner_epoch = core_epoch.checked_add(17).expect("fixture epoch bound");
    let control = OwnerEligibility {
        workspace_id: authority.workspace_id.clone(),
        custody_domain: "local".to_string(),
        custody_epoch: Revision::new(core_epoch).expect("current database epoch"),
        owner_epoch: Revision::new(distinct_owner_epoch).expect("independent owner epoch"),
        runtime_generation: Revision::new(1).expect("fixture runtime generation"),
        enrollment_id: UuidV1::try_from(uuid::Uuid::new_v4().to_string())
            .expect("fixture enrollment selector"),
        observed_registry_digest: Digest::try_from(format!("sha256:{}", "1".repeat(64)))
            .expect("fixture registry digest"),
        fence: FenceStateV1::Clear,
        runtime_profile: RuntimeProfileV1::NativeOciAgent1,
    };
    assert_ne!(control.custody_epoch, control.owner_epoch);
    let backend = ProtectedConnections::new(&shared, None);
    let changes_before = shared.store.total_changes_for_test();

    // Positive control proves this is not refusal due to absent/enabled state,
    // workspace mismatch or accidentally comparing database with OWNER epoch.
    let accepted = backend
        .check_eligibility(control.clone())
        .expect("same database epoch must pass the actual eligibility guard");
    assert_eq!(accepted.custody_epoch.get(), core_epoch);
    assert_eq!(accepted.owner_epoch.get(), distinct_owner_epoch);

    // Hold workspace, owner epoch and every other fact constant. Only the
    // authenticated database epoch differs from the persisted core epoch.
    let mut wrong_epoch = control.clone();
    wrong_epoch.custody_epoch = Revision::new(
        core_epoch
            .checked_add(1)
            .expect("different fixture database epoch"),
    )
    .expect("positive mismatched database epoch");
    let error = backend
        .check_eligibility(wrong_epoch)
        .expect_err("different owner-reported database epoch must refuse before eligibility");
    assert_eq!(
        error.code(),
        Some("placement_changed"),
        "require the actual placement guard refusal, not an unrelated setup error"
    );

    assert_eq!(
        shared.store.total_changes_for_test(),
        changes_before,
        "neither acceptance nor refusal may issue or mutate protected authority"
    );
    let after = shared
        .store
        .protected_authority()
        .expect("read unchanged authority");
    assert_eq!(after.workspace_id, authority.workspace_id);
    assert_eq!(after.database_epoch, authority.database_epoch);
    assert_eq!(
        after.authorization_revision,
        authority.authorization_revision
    );
    assert_eq!(after.ledger_head.sequence, authority.ledger_head.sequence);
    assert_eq!(after.ledger_head.digest, authority.ledger_head.digest);
    backend
        .check_eligibility(control)
        .expect("the unchanged matching epoch remains eligible after refusal");
}
