//! End-to-end CAD-2508 acceptance for protected native `agenticos-provider`
//! read-only execution (`agenticos.search_console_report@1`).
//!
//! Uses a real `Shared`, migrated `Store`, unlocked AEAD encrypted custody
//! vault, `ProtectedConnections`, stub `ProtectedOwnerClient` +
//! `ProtectedCheckpointWriter`, `DedicatedContext`, and a loopback `tiny_http`
//! synthetic AgenticOS `/v1/runtime/tools/read` + `/lookup` upstream.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cadence_core::connection_dispatch::dto::{
    AgenticosReadPolicyV1, ArtifactKindV1, ArtifactV1, CallableV1, CapabilityV1, DataScopeV1,
    EffectClassV1, FenceStateV1, LimitSetV1, ManifestV1, OperationKindV1, OperationPolicyV1,
    OwnerLimitsV1, RuntimeProfileV1,
};
use cadence_core::connection_dispatch::ledger::{
    CheckpointAckV1, CheckpointV1, LedgerRecordV1, LedgerWatermarkV1,
};
use cadence_core::connection_dispatch::{
    material_digest_for_domain, Digest, Ident, Revision, UuidV1,
};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::{ManagementEvidence, OwnerEligibility, ProtectedConnections, ProtectedOwnerClient};
use crate::daemon::public_membership::MembershipMode;
use crate::daemon::{ServeOptions, Shared};
use crate::error::{Error, Result};
use crate::platform::agenticos_provider::{
    MANIFEST_VERSION, PRODUCTION_ORIGIN, READ_ENDPOINT_URL, SCOPE_DEDICATED_READ,
    SLUG_SEARCH_CONSOLE_REPORT,
};
use crate::protected_runtime::ops::{OpsMethod, RuntimeCaller};
use crate::store::connection_dispatch::{
    set_config_in, set_manifest_in, set_policy_in, BootAuthority, ExternalFloorV1,
    ProtectedCheckpointContext, ProtectedCheckpointWriter,
};
use crate::store::seal::StoreConn;
use crate::store::{CredentialRecord, NewAgent};

const TOKEN_SECRET: &str = "fav_mcp_cad2508_synthetic_secretfrag_0123456789abcdef";
const TOKEN_FRAGMENT: &str = "secretfrag";
const WORKSPACE_SLUG: &str = "emw";
const DEPLOYMENT_ID: &str = "dep-emw-01";
const SITE_URL: &str = "sc-domain:example.com";
const UPSTREAM_CONN_ID: &str = "con_emw_gsc_1";

fn sha(c: char) -> Digest {
    Digest::try_from(format!("sha256:{}", c.to_string().repeat(64))).unwrap()
}

fn id(s: &str) -> Ident {
    Ident::try_from(s.to_string()).unwrap()
}

#[derive(Clone, Debug)]
struct CapturedReq {
    method: String,
    url: String,
    auth: Option<String>,
    idem: Option<String>,
    body: Value,
}

struct StubUpstream {
    base_url: String,
    stop: Arc<AtomicBool>,
    post_count: Arc<AtomicUsize>,
    get_count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<CapturedReq>>>,
    post_replies: Arc<Mutex<VecDeque<(u16, Value)>>>,
    get_replies: Arc<Mutex<VecDeque<(u16, Value)>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StubUpstream {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind stub upstream");
        let base_url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let post_count = Arc::new(AtomicUsize::new(0));
        let get_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let post_replies = Arc::new(Mutex::new(VecDeque::new()));
        let get_replies = Arc::new(Mutex::new(VecDeque::new()));
        let def_post = Arc::new(Mutex::new((
            200,
            json!({
                "ok": true,
                "data": {
                    "effect": "read",
                    "repeated": false,
                    "price": {"amount": "0.000003", "currency": "USD", "scale": 6},
                    "result": {
                        "site_url": SITE_URL,
                        "dimension": "query",
                        "rows": [{"key": "cadence", "clicks": 12, "impressions": 100}]
                    }
                }
            }),
        )));
        let def_get = Arc::new(Mutex::new((
            200,
            json!({
                "ok": true,
                "data": {
                    "outcome": "recorded",
                    "recorded": true,
                    "key": "k",
                    "tool": SLUG_SEARCH_CONSOLE_REPORT,
                    "status": "settled",
                    "billing": {"amount_minor": 3}
                }
            }),
        )));

        let stop_c = stop.clone();
        let post_c = post_count.clone();
        let get_c = get_count.clone();
        let reqs_c = requests.clone();
        let post_q = post_replies.clone();
        let get_q = get_replies.clone();

        let thread = std::thread::spawn(move || loop {
            if stop_c.load(Ordering::SeqCst) {
                break;
            }
            let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(50)) else {
                continue;
            };
            let mut text = String::new();
            let _ = req.as_reader().read_to_string(&mut text);
            let method = req.method().to_string();
            let url = req.url().to_string();
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("authorization"))
                .map(|h| h.value.as_str().to_string());
            let idem = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("idempotency-key"))
                .map(|h| h.value.as_str().to_string());
            let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            reqs_c.lock().unwrap().push(CapturedReq {
                method: method.clone(),
                url: url.clone(),
                auth,
                idem,
                body,
            });
            let (status, payload) = if method == "POST" {
                post_c.fetch_add(1, Ordering::SeqCst);
                post_q
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| def_post.lock().unwrap().clone())
            } else {
                get_c.fetch_add(1, Ordering::SeqCst);
                get_q
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| def_get.lock().unwrap().clone())
            };
            let resp = tiny_http::Response::from_string(payload.to_string())
                .with_status_code(status)
                .with_header(
                    tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                        .unwrap(),
                );
            let _ = req.respond(resp);
        });

        Self {
            base_url,
            stop,
            post_count,
            get_count,
            requests,
            post_replies,
            get_replies,
            thread: Some(thread),
        }
    }

    fn posts(&self) -> usize {
        self.post_count.load(Ordering::SeqCst)
    }

    fn gets(&self) -> usize {
        self.get_count.load(Ordering::SeqCst)
    }
}

impl Drop for StubUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct AcceptingOwner {
    evidence: ManagementEvidence,
    eligibility: Mutex<OwnerEligibility>,
    watermark: Mutex<LedgerWatermarkV1>,
    receipts: Mutex<HashMap<u64, Digest>>,
}

impl AcceptingOwner {
    fn new(workspace_id: String, database_epoch: Revision) -> Self {
        let owner_epoch = Revision::new(1).unwrap();
        let runtime_generation = Revision::new(1).unwrap();
        let registry_digest = sha('1');
        let watermark = LedgerWatermarkV1::new(database_epoch);
        Self {
            evidence: ManagementEvidence {
                workspace_id: workspace_id.clone(),
                custody_domain: "local".to_string(),
                database_epoch,
                owner_epoch,
                profile_digest: registry_digest.clone(),
                supervisor_generation: "sup-cad2508".to_string(),
                fenced: false,
            },
            eligibility: Mutex::new(OwnerEligibility {
                workspace_id,
                custody_domain: "local".to_string(),
                custody_epoch: database_epoch,
                owner_epoch,
                runtime_generation,
                enrollment_id: UuidV1::try_from(uuid::Uuid::new_v4().to_string()).unwrap(),
                observed_registry_digest: registry_digest,
                fence: FenceStateV1::Clear,
                runtime_profile: RuntimeProfileV1::NativeOciAgent1,
            }),
            watermark: Mutex::new(watermark),
            receipts: Mutex::new(HashMap::new()),
        }
    }

    fn ack(&self, checkpoint: &CheckpointV1) -> Result<CheckpointAckV1> {
        if checkpoint.workspace_id.as_str() != self.evidence.workspace_id
            || checkpoint.database_epoch != self.evidence.database_epoch
        {
            return Err(Error::rejected("checkpoint workspace/epoch mismatch"));
        }
        Ok(CheckpointAckV1 {
            v: 1,
            workspace_id: checkpoint.workspace_id.clone(),
            custody_domain: checkpoint.custody_domain,
            database_epoch: checkpoint.database_epoch,
            sequence: checkpoint.ledger_head.sequence,
            digest: checkpoint.ledger_head.digest.clone(),
        })
    }
}

impl ProtectedOwnerClient for AcceptingOwner {
    fn current_eligibility(&self, _subject: &RuntimeCaller) -> Result<OwnerEligibility> {
        Ok(self.eligibility.lock().unwrap().clone())
    }

    fn management_evidence(&self) -> Result<ManagementEvidence> {
        Ok(self.evidence.clone())
    }
}

impl ProtectedCheckpointWriter for AcceptingOwner {
    fn context(
        &self,
        workspace_id: &str,
        database_epoch: Revision,
    ) -> Result<ProtectedCheckpointContext> {
        if workspace_id != self.evidence.workspace_id
            || database_epoch != self.evidence.database_epoch
        {
            return Err(Error::rejected("checkpoint identity mismatch"));
        }
        let wm = self.watermark.lock().unwrap();
        Ok(ProtectedCheckpointContext {
            owner_epoch: self.evidence.owner_epoch,
            minimum_sequence: wm.minimum_sequence,
            minimum_digest: wm.minimum_digest.clone(),
        })
    }

    fn checkpoint(
        &self,
        checkpoint: &CheckpointV1,
        records: &[LedgerRecordV1],
    ) -> Result<CheckpointAckV1> {
        let ack = self.ack(checkpoint)?;
        let mut wm = self.watermark.lock().unwrap();
        let mut next = wm.clone();
        next.accept_checkpoint(checkpoint, records)
            .map_err(|_| Error::rejected("checkpoint chain mismatch"))?;
        *wm = next;
        self.receipts.lock().unwrap().insert(
            checkpoint.ledger_head.sequence.get(),
            checkpoint.ledger_head.digest.clone(),
        );
        Ok(ack)
    }

    fn acknowledged_head(&self, checkpoint: &CheckpointV1) -> Result<CheckpointAckV1> {
        let ack = self.ack(checkpoint)?;
        if self
            .receipts
            .lock()
            .unwrap()
            .get(&checkpoint.ledger_head.sequence.get())
            != Some(&checkpoint.ledger_head.digest)
        {
            return Err(Error::rejected("missing checkpoint receipt"));
        }
        Ok(ack)
    }
}

#[allow(dead_code)]
struct TestRig {
    _dir: TempDir,
    shared: Arc<Shared>,
    backend: Arc<ProtectedConnections>,
    owner: Arc<AcceptingOwner>,
    caller: RuntimeCaller,
    connection_id: String,
    manifest_id: String,
    upstream: StubUpstream,
}

impl TestRig {
    fn new(mode: MembershipMode) -> Self {
        Self::new_with_scope(mode, Some(WORKSPACE_SLUG), Some(DEPLOYMENT_ID))
    }

    fn new_with_scope(
        mode: MembershipMode,
        workspace: Option<&str>,
        deployment: Option<&str>,
    ) -> Self {
        let dir = TempDir::new().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = dir.path();
        let state = root.join("state");
        let keys = root.join("keys");
        let pm = root.join("pm");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&state)
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&keys)
            .unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(&pm).unwrap();

        let key_file = keys.join("key");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key_file)
            .unwrap();
        f.write_all(&[0x5au8; 32]).unwrap();
        f.sync_all().unwrap();
        drop(f);

        let custody_cfg = crate::platform::encrypted_sqlite::EncryptedCustodyConfig {
            key_file,
            vault_dir: state.join("vault"),
        };

        let upstream = StubUpstream::start();
        let opts = ServeOptions {
            test_seam: true,
            encrypted_custody: Some(custody_cfg),
            membership_mode_for_test: Some(mode),
            agenticos_provider_fixture_origin: Some(upstream.base_url.clone()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());

        let shared = Shared::new(&state, &opts).expect("Shared::new");
        let workspace_id = shared.store.connection_workspace_id().unwrap();
        let authority = shared.store.protected_authority().unwrap();
        let owner = Arc::new(AcceptingOwner::new(
            workspace_id.clone(),
            authority.database_epoch,
        ));
        shared
            .store
            .install_protected_checkpoint(owner.clone())
            .expect("install checkpoint");
        let floor = {
            let wm = owner.watermark.lock().unwrap();
            ExternalFloorV1 {
                workspace_id: workspace_id.clone(),
                min_sequence: wm.minimum_sequence.get(),
                min_digest: wm.minimum_digest.clone(),
            }
        };
        assert!(matches!(
            shared.store.protected_history_preflight(&floor).unwrap(),
            BootAuthority::Ready
        ));
        shared
            .install_protected(Some(owner.clone() as Arc<dyn ProtectedOwnerClient>))
            .expect("install protected");
        let backend = Arc::clone(shared.protected().unwrap());

        let vault = shared.platform_custody.encrypted().unwrap();
        vault.initialize(&workspace_id, false).unwrap();
        vault.unlock(&workspace_id).unwrap();

        shared
            .store
            .register_agent(&NewAgent {
                alias: "agent-a",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd: root.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let agent_uid = shared
            .store
            .agent_uid_for_alias("agent-a")
            .unwrap()
            .expect("registered agent uid");

        let connection_id = format!("conn-{}", uuid::Uuid::new_v4().simple());
        let key_id = vault.key_id().unwrap();
        let vault_ref = vault
            .write_row(
                &crate::platform::encrypted_sqlite::ExpectedIdentity {
                    workspace_id: workspace_id.clone(),
                    connection_id: connection_id.clone(),
                    provider: crate::platform::agenticos_provider::PLATFORM.to_string(),
                    account: "emw-dedicated".to_string(),
                    credential_revision: 1,
                    exchange: "token".to_string(),
                    key_id,
                },
                TOKEN_SECRET.as_bytes(),
            )
            .unwrap();
        shared
            .store
            .platform_enroll(
                &CredentialRecord {
                    connection_id: connection_id.clone(),
                    credential_revision: 1,
                    platform: crate::platform::agenticos_provider::PLATFORM.to_string(),
                    account: "emw-dedicated".to_string(),
                    scopes: vec![SCOPE_DEDICATED_READ.to_string()],
                    fingerprint: crate::secret::fingerprint(TOKEN_SECRET.as_bytes()),
                    custody: vault_ref,
                    exchange: "token".to_string(),
                    enrolled_at: crate::store::now(),
                    by: "human:enroll".to_string(),
                },
                false,
                Some("protected-encrypted"),
            )
            .unwrap();
        shared
            .store
            .platform_grant_add(
                "agent-a",
                crate::platform::agenticos_provider::PLATFORM,
                "emw-dedicated",
                &[SCOPE_DEDICATED_READ.to_string()],
                "human:grant",
            )
            .unwrap();

        let limits = OwnerLimitsV1 {
            principal: LimitSetV1 {
                requests_per_60s: cadence_core::connection_dispatch::Counter::new(50).unwrap(),
                unresolved: cadence_core::connection_dispatch::Counter::new(10).unwrap(),
                daily_micros: cadence_core::connection_dispatch::UsdMicros::new(1_000_000).unwrap(),
                lifetime_micros: cadence_core::connection_dispatch::UsdMicros::new(10_000_000)
                    .unwrap(),
                per_call_micros: cadence_core::connection_dispatch::UsdMicros::new(500_000)
                    .unwrap(),
            },
            account: LimitSetV1 {
                requests_per_60s: cadence_core::connection_dispatch::Counter::new(50).unwrap(),
                unresolved: cadence_core::connection_dispatch::Counter::new(10).unwrap(),
                daily_micros: cadence_core::connection_dispatch::UsdMicros::new(1_000_000).unwrap(),
                lifetime_micros: cadence_core::connection_dispatch::UsdMicros::new(10_000_000)
                    .unwrap(),
                per_call_micros: cadence_core::connection_dispatch::UsdMicros::new(500_000)
                    .unwrap(),
            },
            workspace: LimitSetV1 {
                requests_per_60s: cadence_core::connection_dispatch::Counter::new(50).unwrap(),
                unresolved: cadence_core::connection_dispatch::Counter::new(10).unwrap(),
                daily_micros: cadence_core::connection_dispatch::UsdMicros::new(1_000_000).unwrap(),
                lifetime_micros: cadence_core::connection_dispatch::UsdMicros::new(10_000_000)
                    .unwrap(),
                per_call_micros: cadence_core::connection_dispatch::UsdMicros::new(500_000)
                    .unwrap(),
            },
        };
        let policy_doc = json!({
            "connection_id": connection_id,
            "site_url": SITE_URL,
            "workspace": workspace,
            "deployment": deployment,
        });
        let policy_digest = Digest::try_from(material_digest_for_domain(
            "cadence-protected-policy-doc-v1",
            &policy_doc,
        ))
        .unwrap();
        let policy = OperationPolicyV1 {
            policy_revision: Revision::new(1).unwrap(),
            active: true,
            allowed_operations: vec![OperationKindV1::AgenticosSearchConsoleReport],
            allowed_human_operations: vec![],
            allowed_destinations: vec![id(READ_ENDPOINT_URL)],
            allowed_models: vec![],
            provider_profile: id("agenticos-provider"),
            data_scopes: Vec::<DataScopeV1>::new(),
            approved_document_digest: policy_digest.clone(),
            agenticos_read: Some(AgenticosReadPolicyV1 {
                adapter_manifest_pin: id(MANIFEST_VERSION),
                slug: id(SLUG_SEARCH_CONSOLE_REPORT),
                upstream_connection_id: id(UPSTREAM_CONN_ID),
                site_url: SITE_URL.to_string(),
                workspace: workspace.map(id),
                deployment: deployment.map(id),
            }),
        };
        let manifest_id = "manifest-agenticos-read".to_string();
        let manifest = ManifestV1 {
            v: 1,
            manifest_id: id(&manifest_id),
            revision: Revision::new(1).unwrap(),
            artifacts: vec![ArtifactV1 {
                artifact_id: id("art-agenticos-tools"),
                kind: ArtifactKindV1::Tool,
                version: id(MANIFEST_VERSION),
                content_digest: sha('2'),
                implementation_digest: sha('3'),
                resource_digest: sha('4'),
                input_schema_digest: sha('5'),
                output_schema_digest: sha('6'),
            }],
            capabilities: vec![CapabilityV1 {
                capability_id: id("cap-search-console-report"),
                callable: CallableV1 {
                    name: id(SLUG_SEARCH_CONSOLE_REPORT),
                    version: id("1"),
                },
                artifact_id: id("art-agenticos-tools"),
                operation: OperationKindV1::AgenticosSearchConsoleReport,
                effect: EffectClassV1::Read,
                connection_scope: id(&connection_id),
                policy_scope: id("provider.read.dedicated"),
            }],
            policy_digest: policy_digest.clone(),
            material_digest: sha('7'),
        };
        let binding_digest = Digest::try_from(material_digest_for_domain(
            "cadence-protected-management-binding-v1",
            &json!({"workspace_id": workspace_id}),
        ))
        .unwrap();
        shared
            .store
            .protected_management_commit(
                &workspace_id,
                owner.evidence.owner_epoch,
                binding_digest,
                |tx| {
                    set_config_in(tx, true, &limits)?;
                    set_policy_in(tx, &connection_id, 1, &policy, policy_digest.as_str())?;
                    set_manifest_in(tx, &manifest)?;
                    Ok(())
                },
            )
            .expect("commit management policy/manifest");

        let caller = RuntimeCaller::registered(
            uuid::Uuid::new_v4().to_string(),
            agent_uid,
            uuid::Uuid::new_v4().to_string(),
            1,
            21001,
            1234,
            5678,
        );

        Self {
            _dir: dir,
            shared,
            backend,
            owner,
            caller,
            connection_id,
            manifest_id,
            upstream,
        }
    }

    fn snapshot(&self) -> (Value, Value) {
        let out = self
            .backend
            .handle_runtime(
                &self.caller,
                OpsMethod::ConnectionSnapshotV1,
                &json!({"v": 1, "manifest_id": self.manifest_id}),
            )
            .expect("snapshot");
        (out["snapshot"].clone(), out["loaded_receipt"].clone())
    }

    fn invoke_payload(
        &self,
        snap: &Value,
        receipt: &Value,
        idem_key: &str,
        site_url: &str,
        start_date: &str,
        end_date: &str,
    ) -> Value {
        json!({
            "v": 1,
            "connection_id": self.connection_id,
            "expected_credential_revision": "1",
            "expected_authorization_revision": snap["connection_bindings"][0]["authorization_revision"],
            "snapshot_id": snap["snapshot_id"],
            "loaded_receipt_id": receipt["loaded_receipt_id"],
            "capability_id": "cap-search-console-report",
            "idempotency_key": idem_key,
            "operation": {
                "kind": "agenticos.search_console_report@1",
                "site_url": site_url,
                "start_date": start_date,
                "end_date": end_date,
                "dimensions": ["query"],
                "row_limit": 25
            }
        })
    }

    fn dispatch_dump(&self) -> String {
        self.shared
            .store
            .read_tx(|conn| {
                let rows: Vec<(String, Option<String>, Option<String>)> = conn.query_vec(
                    "SELECT commit_json, result_json, receipt_json FROM connection_dispatches",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                Ok(serde_json::to_string(&rows).unwrap())
            })
            .unwrap()
    }
}

fn dedicated_mode(company: &str, deployment: &str) -> MembershipMode {
    let verifier_dir = TempDir::new().expect("verifier temp dir");
    let verifier_path = verifier_dir.path().join("verifier");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&verifier_path)
        .unwrap();
    file.write_all("a".repeat(64).as_bytes()).unwrap();
    file.sync_all().unwrap();
    let vars = HashMap::from([
        (
            "AGENTICOS_DEDICATED_BOARD_DEPLOYMENT",
            deployment.to_string(),
        ),
        ("AGENTICOS_BOARD_HOST", "board.example.test".to_string()),
        (
            "AGENTICOS_BOARD_ISSUER",
            "https://issuer.example.test".to_string(),
        ),
        ("AGENTICOS_BOARD_COMPANY", company.to_string()),
        (
            "AGENTICOS_DEDICATED_BOARD_VERIFIER_FILE",
            verifier_path.to_str().unwrap().to_string(),
        ),
    ]);
    let mode = MembershipMode::capture(&crate::lease::Hosted::default(), &|name| {
        vars.get(name).cloned()
    });
    assert!(
        matches!(mode, MembershipMode::Dedicated(_)),
        "fixture must capture dedicated mode"
    );
    mode
}

fn native_mode(company: &str) -> MembershipMode {
    let vars = HashMap::from([
        ("AGENTICOS_COMPANY", company.to_string()),
        ("AGENTICOS_INSTANCE", "instemw01".to_string()),
        ("AGENTICOS_BOARD_HOST", "board.example.test".to_string()),
        (
            "AGENTICOS_BOARD_ISSUER",
            "https://issuer.example.test".to_string(),
        ),
        ("AGENTICOS_BOARD_COMPANY", company.to_string()),
    ]);
    let mode = MembershipMode::capture(&crate::lease::Hosted::default(), &|name| {
        vars.get(name).cloned()
    });
    assert!(
        matches!(mode, MembershipMode::Native(_)),
        "fixture must capture native mode"
    );
    mode
}

fn assert_no_secret_leak(haystack: &str) {
    assert!(
        !haystack.contains(TOKEN_SECRET) && !haystack.contains(TOKEN_FRAGMENT),
        "response or store leaked enrolled bearer credential"
    );
    let chars: Vec<char> = TOKEN_SECRET.chars().collect();
    for w in chars.windows(8) {
        let needle: String = w.iter().collect();
        assert!(
            !haystack.contains(&needle),
            "8-char secret fragment leaked into output/store"
        );
    }
}

#[test]
fn cad2508_happy_path_dedicated_search_console_read_and_no_secret_leak() {
    let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
    let (snap, receipt) = rig.snapshot();
    let idem_key = "cad2508-happy-0001";
    let args = rig.invoke_payload(
        &snap,
        &receipt,
        idem_key,
        SITE_URL,
        "2026-10-01",
        "2026-10-07",
    );
    let res = rig
        .backend
        .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
        .expect("happy path invoke succeeds");

    assert_eq!(res["state"], "done");
    assert_eq!(res["receipt"]["state"], "done");
    assert!(res["receipt"]["usage"].is_null());
    assert_eq!(res["receipt"]["cost"]["reserved_micros"], "0");
    assert_eq!(res["receipt"]["cost"]["actual_micros"], "0");
    assert_eq!(res["receipt"]["result"]["kind"], "agenticos.read@1");
    assert_eq!(res["receipt"]["result"]["slug"], SLUG_SEARCH_CONSOLE_REPORT);
    assert_eq!(res["receipt"]["result"]["price"]["amount"], "0.000003");
    assert_eq!(res["receipt"]["result"]["price"]["currency"], "USD");
    assert_eq!(res["receipt"]["result"]["price"]["scale"], 6);
    let inner_json = res["receipt"]["result"]["result_json"].as_str().unwrap();
    assert!(inner_json.contains("\"clicks\":12"), "{inner_json}");

    assert_eq!(rig.upstream.posts(), 1);
    let captured = rig.upstream.requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].method, "POST");
    assert_eq!(captured[0].url, "/v1/runtime/tools/read");
    assert_eq!(
        captured[0].auth.as_deref(),
        Some(format!("Bearer {TOKEN_SECRET}").as_str())
    );
    assert_eq!(captured[0].idem.as_deref(), Some(idem_key));
    assert_eq!(captured[0].body["slug"], SLUG_SEARCH_CONSOLE_REPORT);
    assert_eq!(captured[0].body["connection_id"], UPSTREAM_CONN_ID);
    assert!(captured[0].body.get("max_charge_minor").is_none());
    assert!(captured[0].body["input"].get("max_charge_minor").is_none());
    assert_eq!(captured[0].body["input"]["site_url"], SITE_URL);
    assert_eq!(captured[0].body["input"]["row_limit"], 25);

    assert_no_secret_leak(&res.to_string());
    assert_no_secret_leak(&rig.dispatch_dump());

    // Confirm production adapter builder constructs real PRODUCTION_ORIGIN without env override.
    let prod =
        crate::platform::agenticos_provider::AgenticosProviderAdapter::new(PRODUCTION_ORIGIN);
    assert!(prod.is_ok());
}

#[test]
fn cad2508_pre_dispatch_refusals_make_zero_upstream_calls_and_preserve_encrypted_binding_guard() {
    // 1. Wrong credential scope ("provider.read" and "model.infer").
    for bad_scope in ["provider.read", "model.infer"] {
        let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
        let json_scopes = serde_json::to_string(&vec![bad_scope]).unwrap();
        rig.shared
            .store
            .write_tx(|tx| {
                tx.execute(
                    "UPDATE platform_credentials SET scopes=?1 WHERE connection_id=?2",
                    rusqlite::params![json_scopes, rig.connection_id],
                )?;
                Ok(())
            })
            .unwrap();
        let (snap, receipt) = rig.snapshot();
        let args = rig.invoke_payload(
            &snap,
            &receipt,
            "cad2508-scope-01",
            SITE_URL,
            "2026-10-01",
            "2026-10-07",
        );
        let err = rig
            .backend
            .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
            .unwrap_err();
        assert_eq!(err.code(), Some("operation_not_granted"), "{err}");
        assert_eq!(rig.upstream.posts(), 0);

        // Generic app broker refuse_encrypted_binding still blocks encrypted agenticos-provider credentials.
        let generic_err = rig
            .shared
            .refuse_encrypted_binding(&json!({
                "provider": crate::platform::agenticos_provider::PLATFORM,
                "account": "emw-dedicated",
                "connection_id": rig.connection_id,
            }))
            .unwrap_err();
        assert_eq!(generic_err.code(), Some("unsupported_custody_use"));
    }

    // 2. Wrong workspace, wrong deployment, standalone mode.
    for mode in [
        dedicated_mode("wrong-workspace", DEPLOYMENT_ID),
        dedicated_mode(WORKSPACE_SLUG, "wrong-deployment"),
        MembershipMode::Standalone,
    ] {
        let rig = TestRig::new(mode);
        let (snap, receipt) = rig.snapshot();
        let args = rig.invoke_payload(
            &snap,
            &receipt,
            "cad2508-mode-01",
            SITE_URL,
            "2026-10-01",
            "2026-10-07",
        );
        let err = rig
            .backend
            .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
            .unwrap_err();
        assert!(
            matches!(
                err.code(),
                Some("operation_not_granted" | "capability_unavailable")
            ),
            "{err}"
        );
        assert_eq!(rig.upstream.posts(), 0);
    }

    // 3. Wrong site_url override -> data_policy_refused; SmtpSend attempt refused.
    {
        let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
        let (snap, receipt) = rig.snapshot();
        let args = rig.invoke_payload(
            &snap,
            &receipt,
            "cad2508-site-01",
            "sc-domain:evil.example",
            "2026-10-01",
            "2026-10-07",
        );
        let err = rig
            .backend
            .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
            .unwrap_err();
        assert_eq!(err.code(), Some("data_policy_refused"), "{err}");
        assert_eq!(rig.upstream.posts(), 0);

        let send_args = json!({
            "v": 1,
            "connection_id": rig.connection_id,
            "expected_credential_revision": "1",
            "expected_authorization_revision": snap["connection_bindings"][0]["authorization_revision"],
            "snapshot_id": snap["snapshot_id"],
            "loaded_receipt_id": receipt["loaded_receipt_id"],
            "capability_id": "cap-search-console-report",
            "idempotency_key": "cad2508-send-01",
            "operation": {
                "kind": "smtp.send@1",
                "approved_send_id": "apr-fake"
            }
        });
        let err = rig
            .backend
            .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &send_args)
            .unwrap_err();
        assert_eq!(err.code(), Some("operation_not_granted"), "{err}");
        assert_eq!(rig.upstream.posts(), 0);
    }

    // 4. Missing grant & wrong runtime generation & revoked credential.
    {
        let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
        rig.shared
            .store
            .platform_grant_revoke(
                "agent-a",
                crate::platform::agenticos_provider::PLATFORM,
                "emw-dedicated",
                None,
                "human:revoke",
            )
            .unwrap();
        let (snap, receipt) = rig.snapshot();
        let args = rig.invoke_payload(
            &snap,
            &receipt,
            "cad2508-grant-01",
            SITE_URL,
            "2026-10-01",
            "2026-10-07",
        );
        let err = rig
            .backend
            .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
            .unwrap_err();
        assert_eq!(err.code(), Some("operation_not_granted"), "{err}");
        assert_eq!(rig.upstream.posts(), 0);

        let wrong_gen_caller = RuntimeCaller::registered(
            rig.caller.runtime_id().to_string(),
            rig.caller.agent_uid().to_string(),
            rig.caller.session_binding().to_string(),
            99,
            21001,
            1234,
            5678,
        );
        let err = rig
            .backend
            .handle_runtime(&wrong_gen_caller, OpsMethod::ConnectionInvokeV1, &args)
            .unwrap_err();
        assert_eq!(err.code(), Some("placement_changed"), "{err}");
        assert_eq!(rig.upstream.posts(), 0);

        rig.shared
            .store
            .platform_revoke_encrypted(
                crate::platform::agenticos_provider::PLATFORM,
                "emw-dedicated",
                "human:revoke",
                &rig.connection_id,
                1,
            )
            .unwrap();
        let err = rig
            .backend
            .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
            .unwrap_err();
        assert!(
            matches!(err.code(), Some("stale_revision" | "not_found")),
            "{err}"
        );
        let snap_err = rig
            .backend
            .handle_runtime(
                &rig.caller,
                OpsMethod::ConnectionSnapshotV1,
                &json!({"v": 1, "manifest_id": rig.manifest_id}),
            )
            .unwrap_err();
        assert_eq!(
            snap_err.code(),
            Some("capability_unavailable"),
            "{snap_err}"
        );
        assert_eq!(rig.upstream.posts(), 0);
    }

    // 5. Revoked policy row refuses before upstream POST.
    {
        let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
        let (snap, receipt) = rig.snapshot();
        rig.shared
            .store
            .write_tx(|tx| {
                tx.execute(
                    "UPDATE connection_operation_policies SET state='revoked' WHERE connection_id=?1",
                    [&rig.connection_id],
                )?;
                Ok(())
            })
            .unwrap();
        let args = rig.invoke_payload(
            &snap,
            &receipt,
            "cad2508-pol-01",
            SITE_URL,
            "2026-10-01",
            "2026-10-07",
        );
        let err = rig
            .backend
            .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
            .unwrap_err();
        assert!(
            matches!(err.code(), Some("stale_revision" | "operation_not_granted")),
            "{err}"
        );
        let snap_err = rig
            .backend
            .handle_runtime(
                &rig.caller,
                OpsMethod::ConnectionSnapshotV1,
                &json!({"v": 1, "manifest_id": rig.manifest_id}),
            )
            .unwrap_err();
        assert_eq!(
            snap_err.code(),
            Some("capability_unavailable"),
            "{snap_err}"
        );
        assert_eq!(rig.upstream.posts(), 0);
    }
}

#[test]
fn cad2508_duplicate_replay_and_idempotency_conflict_make_no_second_post() {
    let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
    let (snap, receipt) = rig.snapshot();
    let idem_key = "cad2508-replay-001";
    let args = rig.invoke_payload(
        &snap,
        &receipt,
        idem_key,
        SITE_URL,
        "2026-10-01",
        "2026-10-07",
    );
    let first = rig
        .backend
        .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
        .unwrap();
    assert_eq!(first["state"], "done");
    assert_eq!(rig.upstream.posts(), 1);

    // Exact replay -> duplicate status/receipt, 0 extra POSTs.
    let second = rig
        .backend
        .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
        .unwrap();
    assert_eq!(second["state"], "done");
    assert_eq!(second["dispatch_id"], first["dispatch_id"]);
    assert_eq!(second["receipt"]["result"], first["receipt"]["result"]);
    assert_eq!(rig.upstream.posts(), 1);

    // Different dates under same idempotency_key -> idempotency_conflict, 0 extra POSTs.
    let conflicting = rig.invoke_payload(
        &snap,
        &receipt,
        idem_key,
        SITE_URL,
        "2026-10-02",
        "2026-10-08",
    );
    let err = rig
        .backend
        .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &conflicting)
        .unwrap_err();
    assert_eq!(err.code(), Some("idempotency_conflict"), "{err}");
    assert_eq!(rig.upstream.posts(), 1);
}

#[test]
fn cad2508_unknown_reconciles_via_readonly_lookup_without_second_post() {
    let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
    rig.upstream.post_replies.lock().unwrap().push_back((
        502,
        json!({"ok": false, "error": {"code": "bad_gateway", "message": "upstream blip"}}),
    ));

    let (snap, receipt) = rig.snapshot();
    let idem_key = "cad2508-unknown-001";
    let args = rig.invoke_payload(
        &snap,
        &receipt,
        idem_key,
        SITE_URL,
        "2026-10-01",
        "2026-10-07",
    );
    let invoked = rig
        .backend
        .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
        .unwrap();
    assert_eq!(invoked["state"], "unknown");
    assert_eq!(rig.upstream.posts(), 1);
    assert_eq!(rig.upstream.gets(), 0);

    let res_args = json!({
        "v": 1,
        "dispatch_id": invoked["dispatch_id"],
        "request_id": invoked["request_id"],
    });
    let first_lookup = rig
        .backend
        .handle_runtime(&rig.caller, OpsMethod::ConnectionResultV1, &res_args)
        .unwrap();
    assert_eq!(first_lookup["state"], "unknown");
    assert_eq!(first_lookup["platform_lookup"]["outcome"], "recorded");
    assert_eq!(first_lookup["platform_lookup"]["recorded"], true);
    assert_eq!(first_lookup["platform_lookup"]["status"], "settled");
    assert_eq!(rig.upstream.posts(), 1);
    assert_eq!(rig.upstream.gets(), 1);

    rig.upstream.get_replies.lock().unwrap().push_back((
        200,
        json!({"ok": true, "data": {"outcome": "unrecorded", "recorded": false}}),
    ));
    let second_lookup = rig
        .backend
        .handle_runtime(&rig.caller, OpsMethod::ConnectionResultV1, &res_args)
        .unwrap();
    assert_eq!(second_lookup["platform_lookup"]["outcome"], "unrecorded");
    assert_eq!(second_lookup["platform_lookup"]["recorded"], false);
    assert_eq!(rig.upstream.posts(), 1);
    assert_eq!(rig.upstream.gets(), 2);
}

#[test]
fn cad2508_upstream_bearer_leak_is_withheld_as_unknown() {
    let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
    rig.upstream.post_replies.lock().unwrap().push_back((
        200,
        json!({
            "ok": true,
            "data": {
                "effect": "read",
                "repeated": false,
                "price": {"amount": "0.000003", "currency": "USD", "scale": 6},
                "result": {"leaked": format!("prefix {TOKEN_FRAGMENT} suffix")}
            }
        }),
    ));
    let (snap, receipt) = rig.snapshot();
    let args = rig.invoke_payload(
        &snap,
        &receipt,
        "cad2508-leak-001",
        SITE_URL,
        "2026-10-01",
        "2026-10-07",
    );
    let res = rig
        .backend
        .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
        .unwrap();
    assert_eq!(res["state"], "unknown");
    assert!(res["receipt"]["result"].is_null());
    assert_no_secret_leak(&res.to_string());
    assert_no_secret_leak(&rig.dispatch_dump());
}

fn run_policy_rpc(
    rig: &TestRig,
    policy: &OperationPolicyV1,
    policy_doc_digest: &str,
) -> crate::error::Result<Value> {
    let authority = rig.shared.store.protected_authority().unwrap();
    let eligibility = rig.owner.management_evidence().unwrap();
    crate::daemon::protected_management_rpc::execute(
        &rig.shared,
        "policy",
        &json!({
            "connection_id": rig.connection_id,
            "credential_revision": 1,
            "policy": policy,
            "policy_digest": policy_doc_digest,
        }),
        &authority,
        &eligibility,
    )
}

fn agenticos_policy_doc_digest(rig: &TestRig, label: &str) -> String {
    cadence_core::connection_dispatch::material_digest_for_domain(
        "cadence-protected-policy-doc-v1",
        &json!({"connection_id": rig.connection_id, "test": label}),
    )
}

fn agenticos_read_policy_for_destinations(
    policy_revision: u64,
    allowed_destinations: Vec<Ident>,
    policy_doc_digest: &str,
) -> OperationPolicyV1 {
    OperationPolicyV1 {
        policy_revision: Revision::new(policy_revision).unwrap(),
        active: true,
        allowed_operations: vec![OperationKindV1::AgenticosSearchConsoleReport],
        allowed_human_operations: vec![],
        allowed_destinations,
        allowed_models: vec![],
        provider_profile: id("agenticos-provider"),
        data_scopes: vec![],
        approved_document_digest: Digest::try_from(policy_doc_digest.to_string()).unwrap(),
        agenticos_read: Some(AgenticosReadPolicyV1 {
            adapter_manifest_pin: id(MANIFEST_VERSION),
            slug: id(SLUG_SEARCH_CONSOLE_REPORT),
            upstream_connection_id: id(UPSTREAM_CONN_ID),
            site_url: SITE_URL.to_string(),
            workspace: Some(id(WORKSPACE_SLUG)),
            deployment: Some(id(DEPLOYMENT_ID)),
        }),
    }
}

#[test]
fn cad2508_management_policy_rpc_refuses_unapproved_destination() {
    let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
    let policy_doc_digest = agenticos_policy_doc_digest(&rig, "destination-refused");

    for destinations in [
        vec![id("https://evil.example/v1/runtime/tools/read")],
        vec![
            id(READ_ENDPOINT_URL),
            id("https://evil.example/v1/runtime/tools/read"),
        ],
    ] {
        let policy = agenticos_read_policy_for_destinations(2, destinations, &policy_doc_digest);
        let err = run_policy_rpc(&rig, &policy, &policy_doc_digest).unwrap_err();
        assert_eq!(err.code(), Some("destination_refused"), "{err}");
    }
}

#[test]
fn cad2508_management_policy_rpc_enforces_agenticos_provider_gate_even_when_read_omitted() {
    let rig = TestRig::new(dedicated_mode(WORKSPACE_SLUG, DEPLOYMENT_ID));
    let policy_doc_digest = agenticos_policy_doc_digest(&rig, "policy-gate");

    // 1. Omitting agenticos.search_console_report@1 and agenticos_read on an agenticos-provider
    // connection must STILL enter the gate and fail closed before commit.
    let omitted_read_policy = OperationPolicyV1 {
        policy_revision: Revision::new(2).unwrap(),
        active: true,
        allowed_operations: vec![OperationKindV1::SmtpSend],
        allowed_human_operations: vec![],
        allowed_destinations: vec![id(READ_ENDPOINT_URL)],
        allowed_models: vec![],
        provider_profile: id("agenticos-provider"),
        data_scopes: vec![],
        approved_document_digest: Digest::try_from(policy_doc_digest.clone()).unwrap(),
        agenticos_read: None,
    };
    let err = run_policy_rpc(&rig, &omitted_read_policy, &policy_doc_digest).unwrap_err();
    assert_eq!(err.code(), Some("invalid_request"), "{err}");

    // 2. Valid policy targeting READ_ENDPOINT_URL succeeds (`policy_revision: 2`).
    let mut valid_policy =
        agenticos_read_policy_for_destinations(2, vec![id(READ_ENDPOINT_URL)], &policy_doc_digest);
    let ok = run_policy_rpc(&rig, &valid_policy, &policy_doc_digest)
        .expect("valid agenticos-provider policy commits");
    assert_eq!(ok["action"], "policy");
    assert_eq!(ok["policy_revision"], "2");

    // 3. Connection with wrong scopes fails even if submitted policy omits the read.
    rig.shared
        .store
        .write_tx(|tx| {
            tx.execute(
                "UPDATE platform_credentials SET scopes=?1 WHERE connection_id=?2",
                rusqlite::params![r#"["provider.read"]"#, rig.connection_id],
            )?;
            Ok(())
        })
        .unwrap();
    valid_policy.policy_revision = Revision::new(3).unwrap();
    valid_policy.allowed_operations = vec![];
    valid_policy.agenticos_read = None;
    let err = run_policy_rpc(&rig, &valid_policy, &policy_doc_digest).unwrap_err();
    assert_eq!(err.code(), Some("operation_not_granted"), "{err}");
}

#[test]
fn cad2508_native_membership_mode_enforces_workspace_and_rejects_deployment() {
    // 1. Native mode with matching workspace and deployment=None succeeds cleanly.
    let ok_rig = TestRig::new_with_scope(native_mode(WORKSPACE_SLUG), Some(WORKSPACE_SLUG), None);
    let (snap, receipt) = ok_rig.snapshot();
    let args = ok_rig.invoke_payload(
        &snap,
        &receipt,
        "cad2508-native-ok",
        SITE_URL,
        "2026-10-01",
        "2026-10-07",
    );
    let res = ok_rig
        .backend
        .handle_runtime(&ok_rig.caller, OpsMethod::ConnectionInvokeV1, &args)
        .expect("matching native workspace succeeds");
    assert_eq!(res["state"], "done");
    assert_eq!(ok_rig.upstream.posts(), 1);
    assert_no_secret_leak(&res.to_string());

    // 2. Native mode refuses any policy carrying a deployment slug OR mismatched workspace.
    for (label, policy_ws, policy_dep) in [
        (
            "cad2508-native-dep",
            Some(WORKSPACE_SLUG),
            Some(DEPLOYMENT_ID),
        ),
        ("cad2508-native-ws", Some("other-workspace"), None),
    ] {
        let rig = TestRig::new_with_scope(native_mode(WORKSPACE_SLUG), policy_ws, policy_dep);
        let (snap, receipt) = rig.snapshot();
        let args = rig.invoke_payload(&snap, &receipt, label, SITE_URL, "2026-10-01", "2026-10-07");
        let err = rig
            .backend
            .handle_runtime(&rig.caller, OpsMethod::ConnectionInvokeV1, &args)
            .unwrap_err();
        assert_eq!(err.code(), Some("operation_not_granted"), "{err}");
        assert_eq!(rig.upstream.posts(), 0);
    }
}

#[cfg(feature = "test-seam")]
fn call_credential_entry_rpc(shared: &Arc<Shared>, method: &str, params: &Value) -> Result<Value> {
    crate::daemon::credential_entry_rpc::rpc_credential_entry(
        shared,
        method,
        params,
        std::process::id(),
        |attempt, input| shared.apply_human_credential_attempt(attempt, input),
    )
}

#[cfg(feature = "test-seam")]
fn mint_entry_proof_and_nonce(
    shared: &Arc<Shared>,
    entry_origin: &str,
    request_id: &str,
) -> (Value, String) {
    let scope = crate::operator_auth::LinkScope::Entry {
        origin: entry_origin.to_string(),
        handoff: crate::operator_auth::EntryHandoff {
            request_id: request_id.to_string(),
            public_owner_hash: None,
        },
    };
    let now = shared.operator_now();
    let opened = {
        let mut auth = shared.operator_auth();
        let link_nonce = auth
            .mint_for_account(
                crate::operator_auth::Origin::Loopback,
                scope.clone(),
                now,
                "owner-cad2508".to_string(),
            )
            .unwrap();
        let (opened, _) = auth
            .open_scoped(
                &link_nonce,
                crate::operator_auth::Origin::Loopback,
                scope,
                "",
                now,
            )
            .unwrap()
            .unwrap();
        opened
    };
    let proof = json!({
        "token": opened.token,
        "key": opened.key,
        "origin": "loopback",
        "entry_origin": entry_origin,
        "public_only": false,
    });
    let nonce_out = call_credential_entry_rpc(
        shared,
        "credential_entry_session",
        &json!({"request_id": request_id, "session": proof}),
    )
    .expect("issue entry nonce");
    let nonce = nonce_out["nonce"].as_str().unwrap().to_string();
    (proof, nonce)
}

#[cfg(feature = "test-seam")]
fn vault_record_count(vault_dir: &std::path::Path) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(
        vault_dir.join("custody.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open vault sqlite");
    conn.query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .expect("count vault records")
}

#[cfg(feature = "test-seam")]
fn credential_record_count(shared: &Shared) -> i64 {
    shared
        .store
        .read_tx(|tx| {
            tx.query_row("SELECT count(*) FROM platform_credentials", [], |row| {
                row.get(0)
            })
            .map_err(Error::from)
        })
        .unwrap()
}

#[cfg(feature = "test-seam")]
#[test]
fn cad2508_real_enroll_rpc_enforces_shape_and_scope_before_vault_write() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let entry_origin = "https://entry.cadence.test";
    let mut opts = ServeOptions::default();
    crate::daemon::configure_credential_entry_fixture(&mut opts, dir.path(), entry_origin)
        .expect("entry fixture");
    let pm = dir.path().join("pm");
    std::fs::DirBuilder::new().mode(0o700).create(&pm).unwrap();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    opts.membership_mode_for_test = Some(native_mode(WORKSPACE_SLUG));
    opts.platforms.insert(
        crate::platform::agenticos_provider::PLATFORM.into(),
        Arc::new(
            crate::platform::agenticos_provider::AgenticosProviderAdapter::for_dedicated_read()
                .unwrap(),
        ),
    );
    let state = dir.path().join("state");
    let vault_dir = state.join("vault");
    let shared = Shared::new(&state, &opts).expect("Shared::new");
    let workspace_id = shared.store.connection_workspace_id().unwrap();
    let vault = shared.platform_custody.encrypted().unwrap();
    vault.initialize(&workspace_id, false).unwrap();
    vault.unlock(&workspace_id).unwrap();

    // 1. credential_request_create refuses wrong shape ("smtp") and wrong scope ("provider.read").
    for bad_create in [
        json!({
            "provider": crate::platform::agenticos_provider::PLATFORM,
            "verb": "enroll",
            "shape": "smtp",
            "scopes": [SCOPE_DEDICATED_READ],
            "target_id": "emw-dedicated",
        }),
        json!({
            "provider": crate::platform::agenticos_provider::PLATFORM,
            "verb": "enroll",
            "shape": "token",
            "scopes": ["provider.read"],
            "target_id": "emw-dedicated",
        }),
    ] {
        let err = call_credential_entry_rpc(&shared, "credential_request_create", &bad_create)
            .unwrap_err();
        assert_eq!(err.code(), Some("invalid_request"), "{err}");
    }

    // 2. Real ProtectedConnections::enroll refuses wrong scopes BEFORE vault.write_row.
    let req_bad_scope = call_credential_entry_rpc(
        &shared,
        "credential_request_create",
        &json!({
            "provider": crate::platform::agenticos_provider::PLATFORM,
            "verb": "enroll",
            "shape": "token",
            "scopes": [SCOPE_DEDICATED_READ],
            "target_id": "emw-dedicated",
        }),
    )
    .expect("create request for tampered scope test")["request"]["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    shared
        .store
        .write_tx(|tx| {
            tx.execute(
                "UPDATE credential_requests SET scopes=?1 WHERE request_id=?2",
                rusqlite::params![r#"["provider.read"]"#, req_bad_scope],
            )?;
            Ok(())
        })
        .unwrap();
    let (proof, nonce) = mint_entry_proof_and_nonce(&shared, entry_origin, &req_bad_scope);
    let err = call_credential_entry_rpc(
        &shared,
        "credential_request_submit",
        &json!({
            "request_id": req_bad_scope,
            "nonce": nonce,
            "session": proof,
            "secret": TOKEN_SECRET,
        }),
    )
    .unwrap_err();
    assert_eq!(err.code(), Some("invalid_request"), "{err}");
    assert_eq!(vault_record_count(&vault_dir), 0);
    assert_eq!(credential_record_count(&shared), 0);

    // 3. credential_entry_session + credential_request_submit refuse tampered non-token shape ("smtp").
    let req_bad_shape = call_credential_entry_rpc(
        &shared,
        "credential_request_create",
        &json!({
            "provider": crate::platform::agenticos_provider::PLATFORM,
            "verb": "enroll",
            "shape": "token",
            "scopes": [SCOPE_DEDICATED_READ],
            "target_id": "emw-dedicated",
        }),
    )
    .expect("create request for tampered shape test")["request"]["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (proof, nonce) = mint_entry_proof_and_nonce(&shared, entry_origin, &req_bad_shape);
    shared
        .store
        .write_tx(|tx| {
            tx.execute(
                "UPDATE credential_requests SET enrollment_shape='smtp' WHERE request_id=?1",
                rusqlite::params![req_bad_shape],
            )?;
            Ok(())
        })
        .unwrap();
    let session_err = call_credential_entry_rpc(
        &shared,
        "credential_entry_session",
        &json!({"request_id": req_bad_shape, "session": proof}),
    )
    .unwrap_err();
    assert_eq!(session_err.code(), Some("request_changed"), "{session_err}");
    let err = call_credential_entry_rpc(
        &shared,
        "credential_request_submit",
        &json!({
            "request_id": req_bad_shape,
            "nonce": nonce,
            "session": proof,
            "secret": "smtp-secret-value",
            "transport": {
                "host": "smtp.example.test",
                "port": 587,
                "tls_mode": "starttls",
                "username": "ops",
                "sender": "ops@example.test",
            },
        }),
    )
    .unwrap_err();
    assert_eq!(err.code(), Some("request_changed"), "{err}");
    assert_eq!(vault_record_count(&vault_dir), 0);
    assert_eq!(credential_record_count(&shared), 0);

    // 4. Valid token + provider.read.dedicated submission succeeds through real RPC -> enroll.
    let req_ok = call_credential_entry_rpc(
        &shared,
        "credential_request_create",
        &json!({
            "provider": crate::platform::agenticos_provider::PLATFORM,
            "verb": "enroll",
            "shape": "token",
            "scopes": [SCOPE_DEDICATED_READ],
            "target_id": "emw-dedicated",
        }),
    )
    .expect("create valid request")["request"]["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (proof, nonce) = mint_entry_proof_and_nonce(&shared, entry_origin, &req_ok);
    let out = call_credential_entry_rpc(
        &shared,
        "credential_request_submit",
        &json!({
            "request_id": req_ok,
            "nonce": nonce,
            "session": proof,
            "secret": TOKEN_SECRET,
        }),
    )
    .expect("valid agenticos-provider enroll succeeds");
    assert_eq!(out["status"], "saved");
    assert_eq!(out["request"]["state"], "done");
    assert_eq!(out["request"]["status"], "saved");
    let conn_id = out["request"]["connection_id"].as_str().unwrap();
    let stored = shared
        .store
        .connection_credential(conn_id)
        .unwrap()
        .expect("enrolled credential record");
    assert_eq!(
        stored.platform,
        crate::platform::agenticos_provider::PLATFORM
    );
    assert_eq!(stored.exchange, "token");
    assert_eq!(stored.scopes, vec![SCOPE_DEDICATED_READ.to_string()]);
    assert!(stored
        .custody
        .starts_with(crate::platform::encrypted_sqlite::TAG_PREFIX));
    assert_eq!(vault_record_count(&vault_dir), 1);
    assert_eq!(credential_record_count(&shared), 1);
    assert_no_secret_leak(&out.to_string());
}
