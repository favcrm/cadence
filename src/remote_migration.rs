//! CAD-1593: remote portable migration — preflight, inactive import,
//! owner-consented rollback-preserving replacement, status and explicit
//! rollback for a `cadence.portable/1` package to a hosted workspace.
//!
//! Transport is the fixed issuer-origin path family
//! `/v1/hosted-cadence/portable-migrations` under
//! [`crate::remote_enrollment::migration_issuer_request`]: the renewed `hct_`
//! bearer, the registered workspace id in `X-Cadence-Workspace`, the pinned
//! issuer from the stored login, and the same Access-ingress discipline as
//! every issuer call. Nothing here follows a redirect, accepts a server-
//! supplied URL, echoes a response message, or prints a credential.
//!
//! Wire shape (settled with the hosted AOS-291 implementation, reusing the
//! AOS-245 packed-v1 ABI):
//! - POST `{base}/preflight` — read-only facts bound to the package digest.
//! - POST `{base}` — begin: `{requestId, packageManifestSha256,
//!   expectedDestinationDigest, expectedGeneration, mode, consent}`; 201 mints
//!   the operation, 200 replays the same requestId+body.
//! - POST `{base}/:operationId/manifest` — exact manifest bytes (≤ 4 MiB).
//! - PUT `{base}/:operationId/input-packs/:index` — raw concatenated member
//!   bytes of the deterministic packed-v1 plan (1,048,576-byte greedy whole-
//!   member packs; a member > 1 MiB streams alone; `manifest.json` never packs).
//! - POST `{base}/:operationId/commit` — `{requestId}`; 202 is pending, never
//!   completion.
//! - GET `{base}/:operationId` — durable status receipt.
//! - GET `{base}/:operationId/inputs` — read-only input reconciliation: the
//!   registered manifest and completed pack indices, bound to the sealed plan.
//! - POST `{base}/:operationId/rollback` — `{requestId,
//!   expectedDestinationDigest, expectedGeneration,
//!   consent:"confirm-rollback-retained-root"}`.
//!
//! A lost answer to a mutation is outcome-unknown, never retried. An upload
//! resumes only through GET `{base}/:operationId/inputs`: a registered manifest
//! or pack that is durably complete is skipped, untouched spans upload, and an
//! unknown or unrecognized effect stops the operation for `--status` recovery.
//! The caller otherwise reconciles through the operation id or replays the
//! identical request under the same `--request-id` after checking status first.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::backup::portable::{self, Member, VerifiedPackage};
use crate::error::{Error, Result};
use crate::remote_cli::{self, RemoteTarget};
use crate::remote_enrollment::{
    migration_issuer_request, MigrationBody, MigrationRequest, MigrationTimeout,
};

const BASE: &str = "/v1/hosted-cadence/portable-migrations";
const RECEIPT_FORMAT: &str = "cadence.portable-migration/1";
/// packed-v1: greedy whole-member packs target this many bytes (immutable ABI).
const PACK_BYTES: u64 = 1_048_576;
/// Seal text the packed-v1 input descriptor hash is computed over.
const SEAL_KIND: &str = "portable-restore/input-storage/1";
const SEAL_LAYOUT: &str = "packed-v1";
/// The owner's renewal shares a bounded wait with `remote_cli` conventions;
/// migration calls do not consume the wake budget.
const RENEWAL_BUDGET: Duration = Duration::from_secs(20);

/// A sealed input member: the inventory row the server rebuilds. `path` is
/// the package-relative member path; `manifest.json` is added by the plan.
#[derive(Debug)]
struct InventoryMember<'a> {
    path: &'a str,
    sha256: &'a str,
    bytes: u64,
}

/// One packed-v1 upload unit: the members concatenated verbatim, in order.
#[derive(Debug)]
struct Pack {
    /// `input-packs/<index>` path element.
    index: usize,
    /// Exact bytes the PUT body must produce.
    bytes: u64,
    /// Contiguous ordinal range in the shared sealed inventory.
    members: std::ops::Range<usize>,
}

/// The deterministic packed-v1 input plan: sealed inventory plus packs.
struct PackPlan<'a> {
    inventory: Vec<InventoryMember<'a>>,
    packs: Vec<Pack>,
}

/// `cadence restore --portable` on a remote org. Every variant that touches
/// bytes verifies the package first; `Status` and `Rollback` never read the
/// source, so they work after the package is gone.
pub enum MigrationCommand {
    /// Read-only POST `/preflight`.
    Preflight { source: PathBuf },
    /// POST `/` `mode:"inactive"`, manifest, packs, commit.
    Import { source: PathBuf, request_id: String },
    /// POST `/` `mode:"replace"` bound to the preflight facts, upload, commit.
    Replace {
        source: PathBuf,
        request_id: String,
        expected_package_digest: String,
        expected_destination_digest: String,
        expected_generation: String,
    },
    /// GET `/:operationId` — durable status and reconciliation. An optional
    /// `--request-id` additionally binds the receipt to that caller id.
    Status {
        operation_id: String,
        request_id: Option<String>,
    },
    /// POST `/:operationId/rollback` — owner consent bound to live facts.
    Rollback {
        operation_id: String,
        request_id: String,
        expected_destination_digest: String,
        expected_generation: String,
    },
}

/// Run one remote migration action; returns the verified receipt `data` to
/// print. `note` carries progress lines (stderr at the call site).
pub fn run(target: &RemoteTarget, command: MigrationCommand, note: impl Fn(&str)) -> Result<Value> {
    let dir = crate::remote_auth::auth_dir(None).map_err(|_| {
        Error::rejected("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    match command {
        MigrationCommand::Preflight { source } => {
            let package = verify_source(&source)?;
            let data = call(
                &dir,
                target,
                "remote preflight",
                "POST",
                &format!("{BASE}/preflight"),
                MigrationBody::Json(&json!({
                    "packageManifestSha256": package.manifest_sha256,
                    "schemaVersion": package.manifest.schema_version,
                    "memberCount": package.manifest.members.len(),
                    "manifestBytes": package.manifest_bytes.len(),
                })),
                MigrationTimeout::Json,
            )?;
            preflight_data(&data, target, &package)
        }
        MigrationCommand::Import { source, request_id } => transfer(
            &dir,
            target,
            &source,
            &request_id,
            None,
            None,
            None,
            "inactive",
            "confirm-inactive-restore",
            &note,
        ),
        MigrationCommand::Replace {
            source,
            request_id,
            expected_package_digest,
            expected_destination_digest,
            expected_generation,
        } => transfer(
            &dir,
            target,
            &source,
            &request_id,
            Some(&expected_package_digest),
            Some(&expected_destination_digest),
            Some(&expected_generation),
            "replace",
            "confirm-replace-retain-rollback",
            &note,
        ),
        MigrationCommand::Status {
            operation_id,
            request_id,
        } => {
            let request_lookup = operation_id.strip_prefix("request:");
            let path = if let Some(id) = request_lookup {
                if !request_id_valid(id)
                    || request_id.as_deref().is_some_and(|expected| expected != id)
                {
                    return Err(Error::usage(
                        "status request selector does not match --request-id",
                    ));
                }
                format!("{BASE}/by-request/{id}")
            } else {
                operation_path(&operation_id, "")?
            };
            let data = call(
                &dir,
                target,
                "remote migration status",
                "GET",
                &path,
                MigrationBody::None,
                MigrationTimeout::Json,
            )?;
            let resolved_id = if request_lookup.is_some() {
                data.get("operationId")
                    .and_then(Value::as_str)
                    .filter(|id| id_valid(id))
                    .ok_or_else(|| receipt_error("status"))?
            } else {
                &operation_id
            };
            status_data(
                &data,
                target,
                resolved_id,
                request_lookup.or(request_id.as_deref()),
            )
        }
        MigrationCommand::Rollback {
            operation_id,
            request_id,
            expected_destination_digest,
            expected_generation,
        } => {
            let data = call(
                &dir,
                target,
                &format!("remote migration rollback for {operation_id}; reconcile with restore --portable --status {operation_id}"),
                "POST",
                &operation_path(&operation_id, "/rollback")?,
                MigrationBody::Json(&json!({
                    "requestId": request_id,
                    "expectedDestinationDigest": expected_destination_digest,
                    "expectedGeneration": expected_generation,
                    "consent": "confirm-rollback-retained-root",
                })),
                MigrationTimeout::Json,
            )?;
            status_data(&data, target, &operation_id, Some(&request_id))
        }
    }
}

/// The full import/replacement flow: begin (idempotent on requestId+body),
/// manifest POST bound to the package digest, every planned pack PUT verified
/// by its exact receipt, then commit. Every step is sent at most once; a lost
/// answer is outcome-unknown with the operation id named for reconciliation.
#[allow(clippy::too_many_arguments)]
fn transfer(
    dir: &Path,
    target: &RemoteTarget,
    source: &Path,
    request_id: &str,
    expected_package_digest: Option<&str>,
    expected_destination_digest: Option<&str>,
    expected_generation: Option<&str>,
    mode: &'static str,
    consent: &'static str,
    note: &dyn Fn(&str),
) -> Result<Value> {
    if !request_id_valid(request_id) {
        return Err(Error::usage(
            "--request-id must be 1–200 ASCII letters, digits, '_' or '-' — a stable id you mint once and reuse verbatim on replay",
        ));
    }
    let package = verify_source(source)?;
    if let Some(expected) = expected_package_digest {
        if expected != package.manifest_sha256.as_str() {
            return Err(Error::rejected(format!(
                "package manifest digest {sha} does not match --expected-package-digest; \
                 re-run `restore --portable --preflight` and pass the values it printed",
                sha = package.manifest_sha256
            )));
        }
    }
    let mut body = Map::new();
    body.insert("requestId".into(), json!(request_id));
    body.insert(
        "packageManifestSha256".into(),
        json!(package.manifest_sha256),
    );
    body.insert(
        "expectedDestinationDigest".into(),
        expected_destination_digest.map_or(Value::Null, |v| json!(v)),
    );
    body.insert(
        "expectedGeneration".into(),
        expected_generation.map_or(Value::Null, |v| json!(v)),
    );
    body.insert("mode".into(), json!(mode));
    body.insert("consent".into(), json!(consent));
    let data = call(
        dir,
        target,
        &format!("remote migration begin for request {request_id}; reconcile with restore --portable --status request:{request_id}"),
        "POST",
        BASE,
        MigrationBody::Json(&Value::Object(body)),
        MigrationTimeout::Json,
    )?;
    let operation_id = begin_data(
        &data,
        target,
        request_id,
        &package,
        mode,
        expected_destination_digest,
        expected_generation,
    )
    .map_err(|error| {
        Error::unknown(format!(
            "{error}; reconcile with restore --portable --status request:{request_id}"
        ))
    })?;
    let reconcile = |error: Error| {
        if error.kind() == "unknown" {
            Error::unknown(format!(
                "{error}; reconcile with restore --portable --status {operation_id}"
            ))
        } else {
            error
        }
    };

    // An explicit replay reconciles the existing operation before any upload.
    // In particular, never resend a manifest into a completed or applying job.
    let status = call(
        dir,
        target,
        "remote migration replay status",
        "GET",
        &operation_path(&operation_id, "")?,
        MigrationBody::None,
        MigrationTimeout::Json,
    )
    .map_err(reconcile)?;
    let status =
        status_data(&status, target, &operation_id, Some(request_id)).map_err(reconcile)?;
    if status["packageManifestSha256"].as_str() != Some(package.manifest_sha256.as_str())
        || status["mode"].as_str() != Some(mode)
    {
        return Err(reconcile(receipt_error("replay status")));
    }
    if status["phase"].as_str() != Some("consented") {
        return Ok(status);
    }

    let plan = plan_input_packs(&package)?;
    // Reconcile before any admission. A registered manifest or pack is never
    // re-sent: untouched pending spans upload, durably completed spans are
    // skipped, and an unknown or unrecognized effect stops with no mutation.
    let inputs = call(
        dir,
        target,
        "remote migration input reconciliation",
        "GET",
        &operation_path(&operation_id, "/inputs")?,
        MigrationBody::None,
        MigrationTimeout::Json,
    )
    .map_err(reconcile)?;
    let (descriptor, completed_packs) =
        match inputs_data(&inputs, target, &operation_id, &package, &plan).map_err(reconcile)? {
            InputProgress::Unregistered => (
                upload_manifest(dir, target, &operation_id, &package, &plan).map_err(reconcile)?,
                Vec::new(),
            ),
            InputProgress::Ready {
                descriptor,
                manifest_completed,
                completed_packs,
            } => {
                if !manifest_completed {
                    upload_manifest(dir, target, &operation_id, &package, &plan)
                        .map_err(reconcile)?;
                }
                (descriptor, completed_packs)
            }
        };
    if !completed_packs.is_empty() {
        note(&format!(
            "remote migration: {} of {} pack(s) already durably verified; skipping them",
            completed_packs.len(),
            plan.packs.len()
        ));
    }

    for pack in plan
        .packs
        .iter()
        .filter(|pack| completed_packs.binary_search(&pack.index).is_err())
    {
        note(&format!(
            "remote migration: sending pack {} of {} ({} bytes)",
            pack.index + 1,
            plan.packs.len(),
            pack.bytes
        ));
        let mut reader = PackReader::open(source, pack, &plan.inventory)?;
        let data = call(
            dir,
            target,
            "remote migration pack",
            "PUT",
            &operation_path(&operation_id, &format!("/input-packs/{}", pack.index))?,
            MigrationBody::Stream(&mut reader),
            MigrationTimeout::Pack,
        )
        .map_err(reconcile)?;
        pack_data(
            &data,
            target,
            &operation_id,
            pack,
            &descriptor,
            &plan.inventory,
        )
        .map_err(reconcile)?;
    }

    note("remote migration: all packs verified; committing");
    let data = call(
        dir,
        target,
        "remote migration commit",
        "POST",
        &operation_path(&operation_id, "/commit")?,
        MigrationBody::Json(&json!({"requestId": request_id})),
        MigrationTimeout::Commit,
    )
    .map_err(reconcile)?;
    let receipt = status_data(&data, target, &operation_id, Some(request_id)).map_err(reconcile)?;
    if receipt["packageManifestSha256"].as_str() != Some(package.manifest_sha256.as_str())
        || receipt["mode"].as_str() != Some(mode)
    {
        return Err(reconcile(receipt_error("commit")));
    }
    Ok(receipt)
}

/// The operation id from a begin receipt, with every bound field verified:
/// the workspace is the registered target, the request id is ours, and the
/// package digest the server bound is the package we verified.
fn begin_data(
    data: &Value,
    target: &RemoteTarget,
    request_id: &str,
    package: &VerifiedPackage,
    mode: &str,
    expected_destination_digest: Option<&str>,
    expected_generation: Option<&str>,
) -> Result<String> {
    let object = strict_data(
        data,
        &[
            "format",
            "workspace",
            "requestId",
            "operationId",
            "phase",
            "packageManifestSha256",
            "destinationDigest",
            "destinationGeneration",
        ],
    )?;
    common_binding(object, target)?;
    if object["requestId"].as_str() != Some(request_id)
        || object["packageManifestSha256"].as_str() != Some(package.manifest_sha256.as_str())
        || object["phase"].as_str().is_none_or(|p| !phase_name(p))
        || !digest_or_null(&object["destinationDigest"])
        || !generation_or_null(&object["destinationGeneration"])
        || expected_destination_digest
            .is_some_and(|expected| object["destinationDigest"].as_str() != Some(expected))
        || expected_generation
            .is_some_and(|expected| object["destinationGeneration"].as_str() != Some(expected))
    {
        return Err(receipt_error("begin"));
    }
    let operation_id = object["operationId"]
        .as_str()
        .filter(|id| id_valid(id))
        .ok_or_else(|| receipt_error("begin"))?
        .to_owned();
    if mode == "replace" {
        // The server must have bound the facts we consented to, not silently
        // switched to a fresh read.
        if object["destinationDigest"].as_str().is_none()
            || object["destinationGeneration"].as_str().is_none()
        {
            return Err(receipt_error("begin"));
        }
    }
    Ok(operation_id)
}

/// Manifest receipt: the server re-sealed the inventory we planned — digest,
/// member count and pack count must match exactly.
fn manifest_data(
    data: &Value,
    target: &RemoteTarget,
    operation_id: &str,
    package: &VerifiedPackage,
    plan: &PackPlan<'_>,
) -> Result<String> {
    let object = strict_data(
        data,
        &[
            "format",
            "workspace",
            "operationId",
            "manifestSha256",
            "members",
            "inputStorage",
            "descriptorSha256",
            "packCount",
        ],
    )?;
    common_binding(object, target)?;
    if object["operationId"].as_str() != Some(operation_id)
        || object["manifestSha256"].as_str() != Some(package.manifest_sha256.as_str())
        || object["members"].as_u64() != Some(plan.inventory.len() as u64)
        || object["inputStorage"].as_str() != Some(SEAL_LAYOUT)
        || object["packCount"].as_u64() != Some(plan.packs.len() as u64)
    {
        return Err(receipt_error("manifest"));
    }
    let descriptor = object["descriptorSha256"]
        .as_str()
        .filter(|v| sha256_shape(v))
        .ok_or_else(|| receipt_error("manifest"))?
        .to_owned();
    if descriptor != seal_descriptor(target, operation_id, plan) {
        return Err(Error::rejected(
            "remote migration: the server's input seal does not match this package's plan — aborting, nothing retried",
        ));
    }
    Ok(descriptor)
}

/// Input reconciliation receipt. `Unregistered` means nothing was admitted;
/// `Ready` means every registered effect is either durably completed (listed
/// in `completed_packs`) or untouched and uploadable. Anything else stops.
enum InputProgress {
    Unregistered,
    Ready {
        descriptor: String,
        manifest_completed: bool,
        completed_packs: Vec<usize>,
    },
}

/// GET `/:operationId/inputs`: the exact server catalog, bound to this
/// workspace, operation, package digest and deterministic local plan. Pack
/// indices must be in range, sorted and unique; the descriptor must equal
/// the seal recomputed locally from the plan.
fn inputs_data(
    data: &Value,
    target: &RemoteTarget,
    operation_id: &str,
    package: &VerifiedPackage,
    plan: &PackPlan<'_>,
) -> Result<InputProgress> {
    let object = strict_data(
        data,
        &[
            "format",
            "workspace",
            "operationId",
            "state",
            "manifestSha256",
            "descriptorSha256",
            "packCount",
            "manifestCompleted",
            "completedPacks",
        ],
    )?;
    if object["format"].as_str() != Some("cadence.portable-migration-inputs/1")
        || object["workspace"].as_str() != Some(target.org_id.as_str())
    {
        return Err(receipt_error("inputs"));
    }
    if object["operationId"].as_str() != Some(operation_id)
        || object["manifestSha256"].as_str() != Some(package.manifest_sha256.as_str())
    {
        return Err(receipt_error("inputs"));
    }
    let pack_count = object["packCount"]
        .as_u64()
        .ok_or_else(|| receipt_error("inputs"))?;
    let manifest_completed = object["manifestCompleted"]
        .as_bool()
        .ok_or_else(|| receipt_error("inputs"))?;
    let completed_packs = object["completedPacks"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .map(|v| v.as_u64().and_then(|n| usize::try_from(n).ok()))
                .collect::<Option<Vec<usize>>>()
        })
        .ok_or_else(|| receipt_error("inputs"))?;
    match object["state"].as_str() {
        Some("unknown") => Err(input_unknown_error(operation_id)),
        Some("unregistered") => {
            if !object["descriptorSha256"].is_null()
                || pack_count != 0
                || manifest_completed
                || !completed_packs.is_empty()
            {
                return Err(receipt_error("inputs"));
            }
            Ok(InputProgress::Unregistered)
        }
        Some("ready") => {
            let descriptor = object["descriptorSha256"]
                .as_str()
                .filter(|v| sha256_shape(v))
                .ok_or_else(|| receipt_error("inputs"))?;
            if pack_count != plan.packs.len() as u64
                || (!manifest_completed && !completed_packs.is_empty())
                || !completed_packs.windows(2).all(|pair| pair[0] < pair[1])
                || completed_packs.iter().any(|&i| i as u64 >= pack_count)
            {
                return Err(receipt_error("inputs"));
            }
            if descriptor != seal_descriptor(target, operation_id, plan) {
                return Err(Error::rejected(
                    "remote migration: the server's input seal does not match this package's plan — aborting, nothing retried",
                ));
            }
            Ok(InputProgress::Ready {
                descriptor: descriptor.to_owned(),
                manifest_completed,
                completed_packs,
            })
        }
        _ => Err(receipt_error("inputs")),
    }
}

/// Admit the pinned manifest bytes once. The receipt must re-seal the planned
/// inventory; the returned descriptor is that seal.
fn upload_manifest(
    dir: &Path,
    target: &RemoteTarget,
    operation_id: &str,
    package: &VerifiedPackage,
    plan: &PackPlan<'_>,
) -> Result<String> {
    // The manifest bytes are pinned to the bound digest again immediately
    // before send: a swapped manifest after verification is refused here.
    if sha256_hex(&package.manifest_bytes) != package.manifest_sha256 {
        return Err(Error::rejected("package manifest changed during transfer"));
    }
    let data = call(
        dir,
        target,
        "remote migration manifest",
        "POST",
        &operation_path(operation_id, "/manifest")?,
        MigrationBody::Bytes(package.manifest_bytes.as_slice()),
        MigrationTimeout::Manifest,
    )?;
    manifest_data(&data, target, operation_id, package, plan)
}

/// Uncertain input state: an earlier effect may be admitted or unrecognized.
/// Nothing is re-admitted or re-sent; the operation must be reconciled first.
fn input_unknown_error(operation_id: &str) -> Error {
    Error::unknown(format!(
        "remote migration upload cannot safely continue for operation {operation_id}: \
         an earlier input effect is unknown or unrecognized; nothing was re-sent — \
         reconcile with restore --portable --status {operation_id}"
    ))
}

/// Pack receipt: exact index, byte count, seal and per-member `(ordinal,
/// sha256, bytes)` rows — the same coverage the server verified.
fn pack_data(
    data: &Value,
    target: &RemoteTarget,
    operation_id: &str,
    pack: &Pack,
    descriptor: &str,
    inventory: &[InventoryMember<'_>],
) -> Result<()> {
    let object = strict_data(
        data,
        &[
            "format",
            "workspace",
            "operationId",
            "ok",
            "inputStorage",
            "descriptorSha256",
            "packIndex",
            "bytes",
            "generation",
            "members",
        ],
    )?;
    common_binding(object, target)?;
    let rows = object["members"]
        .as_array()
        .filter(|rows| rows.len() == pack.members.len())
        .ok_or_else(|| receipt_error("pack"))?;
    let covered = pack.members.clone().zip(rows).all(|(ordinal, row)| {
        let member = &inventory[ordinal];
        row.as_array().is_some_and(|r| {
            r.len() == 3
                && r[0].as_u64() == Some(ordinal as u64)
                && r[1].as_str() == Some(member.sha256)
                && r[2].as_u64() == Some(member.bytes)
        })
    });
    if object["ok"] != Value::Bool(true)
        || object["operationId"].as_str() != Some(operation_id)
        || object["inputStorage"].as_str() != Some(SEAL_LAYOUT)
        || object["descriptorSha256"].as_str() != Some(descriptor)
        || object["packIndex"].as_u64() != Some(pack.index as u64)
        || object["bytes"].as_u64() != Some(pack.bytes)
        || object["generation"].as_u64() != Some(0)
        || !covered
    {
        return Err(receipt_error("pack"));
    }
    Ok(())
}

/// Preflight receipt: the bound package digest and the destination facts a
/// `--replace` will need. The package must fit the server's advertised limits.
fn preflight_data(data: &Value, target: &RemoteTarget, package: &VerifiedPackage) -> Result<Value> {
    let object = strict_data(
        data,
        &[
            "format",
            "workspace",
            "packageManifestSha256",
            "destinationDigest",
            "destinationGeneration",
            "activeStoreRoot",
            "state",
            "limits",
            "replacementSupported",
        ],
    )?;
    common_binding(object, target)?;
    let limits = object["limits"]
        .as_object()
        .filter(|o| {
            o.len() == 3
                && ["manifestBytes", "members", "totalBytes"]
                    .iter()
                    .all(|k| o[*k].as_u64().is_some())
        })
        .ok_or_else(|| receipt_error("preflight"))?;
    let member_bytes = package
        .manifest
        .members
        .iter()
        .try_fold(0u64, |sum, m| sum.checked_add(m.bytes))
        .ok_or_else(|| receipt_error("preflight"))?;
    if object["packageManifestSha256"].as_str() != Some(package.manifest_sha256.as_str())
        || object["destinationDigest"]
            .as_str()
            .is_none_or(|v| !sha256_shape(v))
        || object["destinationGeneration"]
            .as_str()
            .is_none_or(|v| v.is_empty())
        || (!object["activeStoreRoot"].is_null()
            && object["activeStoreRoot"]
                .as_str()
                .is_none_or(|v| v.is_empty()))
        || object["state"].as_str().is_none_or(|v| !code_token(v))
        || object["replacementSupported"] != Value::Bool(true)
        || limits["manifestBytes"].as_u64().unwrap() < package.manifest_bytes.len() as u64
        || limits["members"].as_u64().unwrap() < package.manifest.members.len() as u64
        || limits["totalBytes"].as_u64().unwrap() < member_bytes
    {
        return Err(receipt_error("preflight"));
    }
    let mut receipt = object.clone();
    receipt.insert(
        "source".into(),
        json!({
            "scope": package.manifest.scope,
            "fullWorkspace": package.manifest.full_workspace,
            "schemaVersion": package.manifest.schema_version,
            "members": package.manifest.members.len(),
            "bytes": member_bytes,
            "exclusions": package.manifest.exclusions,
            "redactedMessageBodies": package.manifest.redacted_message_bodies,
            "sourceFidelityEstablished": false,
        }),
    );
    receipt.insert(
        "requiredConsent".into(),
        json!({
            "inactive": "confirm-inactive-restore",
            "replace": "confirm-replace-retain-rollback",
        }),
    );
    Ok(Value::Object(receipt))
}

/// Durable status receipt — the same fixed key set for status, commit and
/// rollback answers. `request_id` is bound when the caller supplied it.
fn status_data(
    data: &Value,
    target: &RemoteTarget,
    operation_id: &str,
    request_id: Option<&str>,
) -> Result<Value> {
    let object = strict_data(
        data,
        &[
            "format",
            "workspace",
            "operationId",
            "requestId",
            "phase",
            "mode",
            "packageManifestSha256",
            "destinationDigest",
            "destinationGeneration",
            "rollback",
            "result",
            "failure",
        ],
    )?;
    common_binding(object, target)?;
    if object["operationId"].as_str() != Some(operation_id)
        || object["phase"].as_str().is_none_or(|p| !phase_name(p))
        || object["mode"]
            .as_str()
            .is_none_or(|m| m != "inactive" && m != "replace")
        || !sha256_shape(object["packageManifestSha256"].as_str().unwrap_or_default())
        || !digest_or_null(&object["destinationDigest"])
        || !generation_or_null(&object["destinationGeneration"])
        || request_id.is_some_and(|id| object["requestId"].as_str() != Some(id))
        || !id_valid(object["requestId"].as_str().unwrap_or_default())
    {
        return Err(receipt_error("status"));
    }
    match &object["rollback"] {
        Value::Null => {}
        rollback => {
            let entry = strict_data(rollback, &["id", "root", "generation", "digest"])?;
            if entry["id"].as_str().is_none_or(|v| !id_valid(v))
                || entry["root"].as_str().is_none_or(|v| v.is_empty())
                || entry["generation"].as_str().is_none_or(|v| v.is_empty())
                || entry["digest"].as_str().is_none_or(|v| !sha256_shape(v))
            {
                return Err(receipt_error("status"));
            }
        }
    }
    match &object["result"] {
        Value::Null => {}
        result => {
            let entry = strict_data(
                result,
                &[
                    "state",
                    "root",
                    "generation",
                    "executionMode",
                    "actorsStarted",
                ],
            )?;
            if !matches!(
                entry["state"].as_str(),
                Some("committed_inactive" | "replaced_inactive" | "rolled_back")
            ) || entry["root"].as_str().is_none_or(|v| v.is_empty())
                || entry["generation"].as_str().is_none_or(|v| v.is_empty())
                || entry["executionMode"].as_str() != Some("disabled")
                || entry["actorsStarted"] != Value::Bool(false)
            {
                return Err(receipt_error("status"));
            }
        }
    }
    match &object["failure"] {
        Value::Null => {}
        failure => {
            let entry = strict_data(failure, &["code"])?;
            if entry["code"].as_str().is_none_or(|c| !code_token(c)) {
                return Err(receipt_error("status"));
            }
        }
    }
    let phase = object["phase"].as_str().unwrap_or_default();
    match phase {
        "committed_inactive" | "replaced_inactive" | "rolled_back" => {
            let expected_mode = if phase == "committed_inactive" {
                "inactive"
            } else {
                "replace"
            };
            if object["result"]["state"].as_str() != Some(phase)
                || object["mode"].as_str() != Some(expected_mode)
                || !object["failure"].is_null()
                || (phase != "committed_inactive"
                    && (object["rollback"].is_null()
                        || object["destinationDigest"].is_null()
                        || object["destinationGeneration"].is_null()))
            {
                return Err(receipt_error("status"));
            }
        }
        "failed" | "unknown" => {
            if object["failure"].is_null() || !object["result"].is_null() {
                return Err(receipt_error("status"));
            }
        }
        _ => {
            if !object["result"].is_null() || !object["failure"].is_null() {
                return Err(receipt_error("status"));
            }
        }
    }
    Ok(data.clone())
}

/// `format` + `workspace`: every receipt binds the registered target; a
/// foreign-workspace answer is a malformed receipt, not a result.
fn common_binding(object: &Map<String, Value>, target: &RemoteTarget) -> Result<()> {
    if object["format"].as_str() != Some(RECEIPT_FORMAT)
        || object["workspace"].as_str() != Some(target.org_id.as_str())
    {
        return Err(receipt_error("migration"));
    }
    Ok(())
}

/// Exactly these keys on `data`, all present — anything else is an
/// incomplete or foreign receipt, never silently trusted.
fn strict_data<'a>(data: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let object = data.as_object().ok_or_else(|| receipt_error("receipt"))?;
    if object.len() != keys.len() || !keys.iter().all(|k| object.contains_key(*k)) {
        return Err(receipt_error("receipt"));
    }
    Ok(object)
}

fn receipt_error(step: &str) -> Error {
    Error::unknown(format!(
        "remote migration {step} returned a malformed or incomplete receipt — \
         nothing is assumed written; reconcile before retrying"
    ))
}

/// Verify the source package end to end (real inspector: member hashes,
/// exact set, store catalog, tracker PM root) before any byte is sent.
fn verify_source(source: &Path) -> Result<VerifiedPackage> {
    portable::verify_transfer_package(source)
        .map_err(|e| Error::rejected(format!("remote migration source refused: {e}")))
}

/// Send one request under the migration contract: renewed credential (the
/// issuer pin and member/issuer identity are rechecked), fixed path, workspace
/// header, single attempt. A send failure is outcome-unknown (the bytes may
/// have landed) and carries the step name for reconciliation — nothing is
/// silently re-issued.
#[allow(clippy::too_many_arguments)]
fn call(
    dir: &Path,
    target: &RemoteTarget,
    step: &str,
    method: &str,
    path: &str,
    body: MigrationBody<'_>,
    timeout: MigrationTimeout,
) -> Result<Value> {
    let material = remote_cli::cli_material_until(dir, target, Instant::now() + RENEWAL_BUDGET)?;
    let issuer = material.issuer.as_deref().ok_or_else(|| {
        Error::rejected("the stored login has no pinned issuer — run `cadence login` again")
    })?;
    let outcome = migration_issuer_request(
        dir,
        issuer,
        MigrationRequest {
            method,
            path,
            workspace: &target.org_id,
            bearer: &material.access_token,
            timeout,
            body,
        },
        Instant::now() + timeout.cap() + Duration::from_secs(5),
    );
    let (status, body) = match outcome {
        Ok(pair) => pair,
        Err(error) => {
            if error.kind() == "unknown" {
                // The request may or may not have landed: name the step and
                // the reconciliation path; nothing is re-sent.
                return Err(Error::unknown(format!(
                    "{step} — the request may or may not have reached the remote: {error}"
                )));
            }
            return Err(error);
        }
    };
    let envelope = match body {
        Some(body) => body,
        None if (200..300).contains(&status) => {
            return Err(receipt_error(step));
        }
        None => return Err(refusal_error(status, None)),
    };
    let Some(object) = envelope.as_object() else {
        return Err(refusal_error(status, None));
    };
    if object.len() == 2 && object["ok"] == Value::Bool(true) && (200..300).contains(&status) {
        return Ok(object["data"].clone());
    }
    Err(refusal_error(status, Some(&envelope)))
}

/// `{ok:false,error:{code,…}}` → a refusal carrying only the status and the
/// allowlisted safe code; the server's `message` is never echoed.
fn refusal_error(status: u16, body: Option<&Value>) -> Error {
    let code = body
        .and_then(|b| b.get("error"))
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .filter(|c| code_token(c));
    let code = code.map_or_else(|| "unavailable".to_string(), str::to_string);
    match status {
        401 | 403 => Error::rejected(format!(
            "remote migration refused the credential or workspace authority \
             (HTTP {status}, {code}) — the operation needs the verified workspace \
             owner; check `cadence auth status` before retrying"
        )),
        409 => Error::rejected(format!(
            "remote migration conflicted with the recorded destination facts or a \
             different request body (HTTP {status}, {code}) — re-run --preflight for \
             current digests, and replay only with the identical --request-id"
        )),
        503 => Error::unknown(format!(
            "remote migration outcome is uncertain (HTTP {status}, {code}) — the \
             operation may be fenced; reconcile with `cadence restore --portable \
             --status <operation-id> --org <org>` before anything else is sent"
        )),
        _ => Error::rejected(format!("remote migration refused (HTTP {status}, {code})")),
    }
}

/// `operationId`/`requestId` shape: the shared hosted-id contract.
fn id_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn request_id_valid(value: &str) -> bool {
    id_valid(value)
}

/// A typed refusal/failure code: bounded lowercase token only.
fn code_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The durable phase vocabulary the hosted ledger emits.
fn phase_name(value: &str) -> bool {
    matches!(
        value,
        "consented"
            | "applying"
            | "finalization"
            | "committed_inactive"
            | "replaced_inactive"
            | "rolled_back"
            | "failed"
            | "unknown"
    )
}

fn sha256_shape(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn digest_or_null(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(sha256_shape)
}

fn generation_or_null(value: &Value) -> bool {
    value.is_null()
        || value
            .as_str()
            .is_some_and(|v| !v.is_empty() && v.len() <= 200)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Build the `/:operationId<suffix>` path; a malformed id is refused before
/// it can reach a URL.
fn operation_path(operation_id: &str, suffix: &str) -> Result<String> {
    if !id_valid(operation_id) {
        return Err(Error::rejected(
            "remote migration operation id is not valid",
        ));
    }
    Ok(format!("{BASE}/{operation_id}{suffix}"))
}

/// packed-v1 plan over the sealed input inventory: manifest members plus
/// `manifest.json` itself, sorted by UTF-8 path bytes, then greedy whole-
/// member concatenation into ≤ 1 MiB packs with the manifest segment boundary
/// excluded — the exact deterministic plan the server replans and seals.
fn plan_input_packs(package: &VerifiedPackage) -> Result<PackPlan<'_>> {
    let mut inventory = Vec::with_capacity(package.manifest.members.len() + 1);
    inventory.extend(package.manifest.members.iter().map(
        |Member {
             path,
             bytes,
             sha256,
         }| InventoryMember {
            path: path.as_str(),
            sha256: sha256.as_str(),
            bytes: *bytes,
        },
    ));
    inventory.push(InventoryMember {
        path: "manifest.json",
        sha256: &package.manifest_sha256,
        bytes: package.manifest_bytes.len() as u64,
    });
    inventory.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    if inventory
        .windows(2)
        .any(|pair| pair[0].path == pair[1].path)
    {
        return Err(Error::rejected("package inventory has duplicate paths"));
    }
    let ordinal = inventory
        .iter()
        .position(|m| m.path == "manifest.json")
        .ok_or_else(|| Error::internal("sealed inventory lost the manifest"))?;
    let mut packs: Vec<Pack> = Vec::new();
    for (start, end) in [(0, ordinal), (ordinal + 1, inventory.len())] {
        if start == end {
            continue;
        }
        // Index of the pack currently collecting members; `None` closes it.
        let mut current: Option<usize> = None;
        for (at, member) in inventory.iter().enumerate().take(end).skip(start) {
            let streamed = member.bytes > PACK_BYTES;
            if current
                .map(|at| packs[at].bytes + member.bytes > PACK_BYTES)
                .unwrap_or(true)
            {
                packs.push(Pack {
                    index: packs.len(),
                    bytes: 0,
                    members: at..at,
                });
                current = Some(packs.len() - 1);
            }
            let pack = &mut packs[current.expect("pack just opened")];
            pack.members.end = at + 1;
            pack.bytes += member.bytes;
            if streamed {
                current = None;
            }
        }
    }
    Ok(PackPlan { inventory, packs })
}

/// The input-storage descriptor seal, byte-exact with the server's
/// `JSON.stringify` construction: fixed key order `{path,sha256,bytes}` per
/// member and `{manifestSha256,members}` for the inventory object.
fn seal_descriptor(target: &RemoteTarget, operation_id: &str, plan: &PackPlan<'_>) -> String {
    let manifest = &plan.inventory[plan
        .inventory
        .iter()
        .position(|m| m.path == "manifest.json")
        .unwrap_or(0)];
    let mut text = String::with_capacity(
        manifest.bytes as usize + target.org_id.len() + operation_id.len() + 256,
    );
    text.push_str("[\"");
    text.push_str(SEAL_KIND);
    text.push_str("\",\"");
    text.push_str(SEAL_LAYOUT);
    text.push_str("\",");
    js_string(&mut text, &target.org_id);
    text.push(',');
    js_string(&mut text, operation_id);
    text.push_str(",0,{\"manifestSha256\":");
    js_string(&mut text, manifest.sha256);
    text.push_str(",\"members\":[");
    for (at, member) in plan.inventory.iter().enumerate() {
        if at > 0 {
            text.push(',');
        }
        text.push_str("{\"path\":");
        js_string(&mut text, member.path);
        text.push_str(",\"sha256\":");
        js_string(&mut text, member.sha256);
        text.push_str(",\"bytes\":");
        text.push_str(&member.bytes.to_string());
        text.push('}');
    }
    text.push_str("]}]");
    sha256_hex(text.as_bytes())
}

/// Escape one JSON string exactly as `JSON.stringify` does. Unicode scalar
/// values, including U+2028/U+2029, remain literal UTF-8; only JSON's quotes,
/// backslashes and C0 controls require escaping.
fn js_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Streams one pack body: each member file is opened privately on first read
/// (never through a link, never a non-regular file), hashed as it flows, and
/// refused mid-stream — as an I/O error that aborts the request — when its
/// bytes or digest differ from the sealed inventory.
struct PackReader<'a, 'p> {
    source: &'a Path,
    members: &'a [InventoryMember<'p>],
    at: usize,
    file: Option<File>,
    remain: u64,
    hasher: Sha256,
}

impl<'a, 'p> PackReader<'a, 'p> {
    fn open(source: &'a Path, pack: &Pack, inventory: &'a [InventoryMember<'p>]) -> Result<Self> {
        Ok(PackReader {
            source,
            members: &inventory[pack.members.clone()],
            at: 0,
            file: None,
            remain: 0,
            hasher: Sha256::new(),
        })
    }

    fn next_member(&mut self) -> io::Result<bool> {
        let Some(member) = self.members.get(self.at) else {
            return Ok(false);
        };
        let path = self.source.join(member.path);
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 || meta.size() != member.bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("package member {} changed during transfer", member.path),
            ));
        }
        self.file = Some(file);
        self.remain = member.bytes;
        self.hasher = Sha256::new();
        Ok(true)
    }

    fn finish_member(&mut self) -> io::Result<()> {
        let member = &self.members[self.at];
        if format!("{:x}", std::mem::take(&mut self.hasher).finalize()) != member.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "package member {} digest changed during transfer",
                    member.path
                ),
            ));
        }
        self.file = None;
        self.at += 1;
        Ok(())
    }
}

impl Read for PackReader<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.file.is_none() && !self.next_member()? {
                return Ok(0);
            }
            if self.remain == 0 {
                self.finish_member()?;
                continue;
            }
            let file = self.file.as_mut().expect("member open");
            let limit = buf.len().min(self.remain as usize);
            let read = file.read(&mut buf[..limit])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "package member {} truncated during transfer",
                        self.members[self.at].path
                    ),
                ));
            }
            self.hasher.update(&buf[..read]);
            self.remain -= read as u64;
            return Ok(read);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> RemoteTarget {
        RemoteTarget {
            org: "migration-test".into(),
            endpoint: "https://migration-test.cadencecloud.app".into(),
            org_id: "ws_11111111-1111-4111-8111-111111111111".into(),
            identity: None,
        }
    }

    fn pending_status() -> Value {
        json!({
            "format": RECEIPT_FORMAT,
            "workspace": target().org_id,
            "operationId": "migrate_test",
            "requestId": "request-test",
            "phase": "applying",
            "mode": "replace",
            "packageManifestSha256": "a".repeat(64),
            "destinationDigest": "b".repeat(64),
            "destinationGeneration": "4.2",
            "rollback": null,
            "result": null,
            "failure": null,
        })
    }

    #[test]
    fn descriptor_strings_preserve_json_unicode_separators() {
        let path = "tracker/line\u{2028}paragraph\u{2029}.md";
        let mut encoded = String::new();
        js_string(&mut encoded, path);
        // JSON.stringify emits these valid filename characters as literal UTF-8.
        assert_eq!(encoded.as_bytes(), format!("\"{path}\"").as_bytes());
    }

    #[test]
    fn malformed_nested_receipts_are_refused() {
        for (field, malformed) in [
            (
                "rollback",
                json!({"unexpected": 0, "root": "root", "generation": "4", "digest": "b".repeat(64)}),
            ),
            (
                "result",
                json!({"unexpected": 0, "root": "root", "generation": "4", "executionMode": "disabled", "actorsStarted": false}),
            ),
            ("failure", json!({"unexpected": "invalid_request"})),
        ] {
            let mut receipt = pending_status();
            receipt[field] = malformed;
            assert!(
                status_data(&receipt, &target(), "migrate_test", Some("request-test")).is_err(),
                "accepted malformed {field}"
            );
        }
    }

    #[test]
    fn completed_replacement_requires_matching_result_and_rollback() {
        let mut receipt = pending_status();
        receipt["phase"] = json!("replaced_inactive");
        assert!(status_data(&receipt, &target(), "migrate_test", None).is_err());
        receipt["result"] = json!({
            "state": "committed_inactive",
            "root": "c".repeat(32),
            "generation": "5.3",
            "executionMode": "disabled",
            "actorsStarted": false,
        });
        assert!(status_data(&receipt, &target(), "migrate_test", None).is_err());
        receipt["result"]["state"] = json!("replaced_inactive");
        receipt["rollback"] = json!({
            "id": "rollback-test",
            "root": "d".repeat(32),
            "generation": "4.2",
            "digest": "b".repeat(64),
        });
        assert_eq!(
            status_data(&receipt, &target(), "migrate_test", Some("request-test")).unwrap(),
            receipt
        );
    }
}
