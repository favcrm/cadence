//! Independent CAD-1085 acceptance for the real runtime eligibility epoch guard.
//!
//! Child of protected_connections. Uses a real temporary Shared and migrated
//! Store. Fixture evidence is input to the post-authentication guard only:
//! this test does not mint or mock an authenticated root-owner capability.

use std::sync::Arc;

use cadence_core::connection_dispatch::dto::{FenceStateV1, RuntimeProfileV1};
use cadence_core::connection_dispatch::{Digest, Ident, Revision, UuidV1};
use tempfile::TempDir;

use super::{
    ManagementEvidence, OwnerEligibility, ProfileRegistryManifest, ProtectedConnections,
    ProtectedOwnerClient,
};
use crate::daemon::{ServeOptions, Shared};
use crate::error::Result;
use crate::protected_runtime::ops::RuntimeCaller;

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

struct StubProfileOwner {
    eligibility: OwnerEligibility,
    profile_manifest: ProfileRegistryManifest,
}

impl ProtectedOwnerClient for StubProfileOwner {
    fn current_eligibility(&self, _subject: &RuntimeCaller) -> Result<OwnerEligibility> {
        Ok(self.eligibility.clone())
    }

    fn management_evidence(&self) -> Result<ManagementEvidence> {
        Ok(ManagementEvidence {
            workspace_id: self.eligibility.workspace_id.clone(),
            custody_domain: "local".to_string(),
            database_epoch: self.eligibility.custody_epoch,
            owner_epoch: self.eligibility.owner_epoch,
            profile_digest: self.profile_manifest.manifest_digest.clone(),
            supervisor_generation: "sup-test".to_string(),
            fenced: false,
        })
    }

    fn profile_registry_manifest(&self) -> Result<Option<ProfileRegistryManifest>> {
        Ok(Some(self.profile_manifest.clone()))
    }
}

#[test]
fn cad2576_manifest_is_current_validates_profile_snapshot_and_invalidates_on_policy() {
    let dir = TempDir::new().expect("isolated Shared fixture directory");
    let opts = ServeOptions::default();
    let absent_pm = dir.path().join("no-pm");
    opts.provider_env.set(
        "CADENCE_PM_DIR",
        absent_pm.to_str().expect("fixture path encoding"),
    );
    let shared = Shared::new(dir.path(), &opts).expect("temporary Shared fixture");
    shared
        .store
        .write_tx(|tx| {
            tx.execute(
                "UPDATE connection_dispatch_config SET enabled=1,ever_enabled=1,fenced=0 WHERE singleton=1",
                [],
            )?;
            Ok(())
        })
        .expect("enable dispatch config");
    let authority = shared.store.protected_authority().expect("authority");
    let sha = |c: char| Digest::try_from(format!("sha256:{}", c.to_string().repeat(64))).unwrap();
    let agent_uid = format!("agt-{}", "b".repeat(32));
    let owner = Arc::new(StubProfileOwner {
        eligibility: OwnerEligibility {
            workspace_id: authority.workspace_id.clone(),
            custody_domain: "local".to_string(),
            custody_epoch: authority.database_epoch,
            owner_epoch: Revision::new(2).unwrap(),
            runtime_generation: Revision::new(1).unwrap(),
            enrollment_id: UuidV1::try_from(uuid::Uuid::new_v4().to_string()).unwrap(),
            observed_registry_digest: sha('1'),
            fence: FenceStateV1::Clear,
            runtime_profile: RuntimeProfileV1::NativeOciAgent1,
        },
        profile_manifest: ProfileRegistryManifest {
            manifest_id: Ident::try_from("protected-local-1".to_string()).unwrap(),
            manifest_revision: Revision::new(1).unwrap(),
            manifest_digest: sha('4'),
            policy_digest: sha('5'),
        },
    });
    let backend = ProtectedConnections::new(
        &shared,
        Some(Arc::clone(&owner) as Arc<dyn ProtectedOwnerClient>),
    );
    let caller = RuntimeCaller::registered(
        uuid::Uuid::new_v4().to_string(),
        agent_uid.clone(),
        uuid::Uuid::new_v4().to_string(),
        1,
        21001,
        1234,
        5678,
    );

    let (first_snap, first_receipt) = backend
        .current_material(&caller)
        .expect("profile snapshot issued when zero policies exist");
    assert!(first_snap.connection_bindings.is_empty());
    assert!(first_snap.capabilities.is_empty());

    // Subsequent calls reuse the same snapshot and `current_material_for_agent` validates it.
    let (reused_snap, reused_receipt) = backend.current_material(&caller).unwrap();
    assert_eq!(reused_snap.snapshot_id, first_snap.snapshot_id);
    assert_eq!(
        reused_receipt.loaded_receipt_id,
        first_receipt.loaded_receipt_id
    );
    let (agent_snap, agent_receipt) = backend
        .current_material_for_agent(&agent_uid)
        .expect("agent validation accepts current profile snapshot");
    assert_eq!(agent_snap.snapshot_id, first_snap.snapshot_id);
    let loaded = crate::store::connection_dispatch::LoadedSnapshot {
        snapshot: agent_snap,
        receipt: agent_receipt,
        authorization_revision: authority.authorization_revision,
    };
    assert!(backend.manifest_is_current(&shared, &loaded).unwrap());

    // Changing the profile registry manifest invalidates with loaded_state_mismatch.
    let changed_owner = Arc::new(StubProfileOwner {
        eligibility: owner.eligibility.clone(),
        profile_manifest: ProfileRegistryManifest {
            manifest_revision: Revision::new(2).unwrap(),
            ..owner.profile_manifest.clone()
        },
    });
    let changed_backend = ProtectedConnections::new(&shared, Some(changed_owner));
    assert!(!changed_backend
        .manifest_is_current(&shared, &loaded)
        .unwrap());
    let mismatch = changed_backend
        .current_material_for_agent(&agent_uid)
        .expect_err("changed profile manifest must refuse with loaded_state_mismatch");
    assert_eq!(mismatch.code(), Some("loaded_state_mismatch"));

    // Adding an active policy invalidates the profile snapshot and prevents fallback.
    let policy_digest = format!("sha256:{}", "9".repeat(64));
    shared
        .store
        .write_tx(|tx| {
            tx.execute(
                "INSERT INTO connection_operation_policies(\
                 connection_id,credential_revision,policy_revision,policy_digest,policy_json,state) \
                 VALUES('conn-fixture',1,1,?1,'{}','active')",
                [&policy_digest],
            )?;
            Ok(())
        })
        .expect("insert active policy");
    assert!(!backend.manifest_is_current(&shared, &loaded).unwrap());
    let err = backend
        .current_material_for_agent(&agent_uid)
        .expect_err("active policy must invalidate profile snapshot");
    assert_eq!(err.code(), Some("stale_revision"));
    let err2 = backend
        .current_material(&caller)
        .expect_err("current_material must refuse instead of falling back when policy is active");
    assert_eq!(err2.code(), Some("capability_unavailable"));
}
