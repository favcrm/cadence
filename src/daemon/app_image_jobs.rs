//! CAD-1315: the host worker behind a standalone image intent.
//!
//! `app_tool_invoke` proves, quotes, reserves the claim and writes the job
//! row, then returns with the intent `pending`. This worker does the
//! provider I/O (submit, poll, artifact fetch) on its own thread holding NO
//! PM, custody or release lock, and settles the job and its intent together.
//!
//! Exactly-once: every attempt (first, backoff retry, restart, re-check)
//! replays the same idempotency key with the byte-identical frozen body, and
//! AgenticOS answers a replay with the one existing job, never a second
//! reserve. AgenticOS has no read-by-key route, so that idempotent replay is
//! how a restart reconciles a job by key. A new key is minted only by a new
//! request id after the old job is terminal (the intent gate's existing
//! restart path); this module never mints one and adds no RPC that settles.

use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::platform::{ImageFailure, ImageReason, ImageSettle};
use crate::store::app_records::RecordStore;
use crate::store::app_social_drafts::{ImageJob, ImageSettlement};
use crate::store::app_tools::AppToolRecord;

/// Per-job window before a still-unresolved job settles `uncertain`. An
/// explicit re-check opens a fresh window; a restart always replays the key
/// once, even past the stored deadline, before that deadline can settle it.
pub(super) const JOB_WINDOW_MS: u64 = 10 * 60 * 1000;
pub(super) const BACKOFF_MIN_MS: u64 = 5 * 1000;
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How one worker run ended: only `Settled` may be followed by a respawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunEnd {
    /// This run settled the job (the intent left `pending`).
    Settled,
    /// Left `active` on purpose: closing, a tripped lease fence, no spec, or
    /// no job. A later boot, re-check or invoke restarts it; never a loop.
    Held,
}

/// Test-seam probes: worker-start count, a pause between settle and registry
/// removal, and a stand-in for a tripped lease fence.
#[cfg(feature = "test-seam")]
pub mod test_probe {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
    static STARTS: AtomicUsize = AtomicUsize::new(0);
    static FENCE: AtomicBool = AtomicBool::new(false);
    static PAUSE_MS: AtomicU64 = AtomicU64::new(0);
    static FAULT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    pub fn starts() -> usize {
        STARTS.load(SeqCst)
    }
    pub fn trip_fence(on: bool) {
        FENCE.store(on, SeqCst);
    }
    pub fn pause_after_settle_ms(ms: u64) {
        PAUSE_MS.store(ms, SeqCst);
    }
    /// One-shot fault for the next worker run: 1 panics, 2 returns an error.
    pub fn fault_next(kind: u8) {
        FAULT.store(kind, SeqCst);
    }
    pub(super) fn take_fault() -> u8 {
        FAULT.swap(0, SeqCst)
    }
    pub(super) fn started() {
        STARTS.fetch_add(1, SeqCst);
    }
    pub(super) fn fenced() -> bool {
        FENCE.load(SeqCst)
    }
    pub(super) fn pause() {
        std::thread::sleep(std::time::Duration::from_millis(PAUSE_MS.load(SeqCst)));
    }
}

impl Shared {
    /// The deadline (epoch seconds) of a job window opened now.
    pub(super) fn image_job_deadline(&self) -> f64 {
        crate::store::now() + self.image_job_window.as_secs_f64()
    }

    /// One worker per call id: the registry refuses a second runner, so a
    /// restart, a re-check and an invoke replay can never run two submitters
    /// for one key.
    pub(super) fn spawn_image_job_worker(self: &Arc<Self>, install: &str, call_id: &str) {
        {
            let mut workers = self
                .image_job_workers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !workers.insert(call_id.to_owned()) {
                return;
            }
        }
        let shared = Arc::clone(self);
        let (install, call_id) = (install.to_owned(), call_id.to_owned());
        std::thread::spawn(move || {
            // Removes the registry entry on every exit, a panic included.
            struct Registered(Arc<Shared>, String);
            impl Drop for Registered {
                fn drop(&mut self) {
                    self.0
                        .image_job_workers
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&self.1);
                }
            }
            let guard = Registered(Arc::clone(&shared), call_id.clone());
            let end = shared
                .image_job_run(&install, &call_id)
                .unwrap_or_else(|error| {
                    eprintln!("image job {call_id} worker stopped: {error}");
                    RunEnd::Held
                });
            #[cfg(feature = "test-seam")]
            test_probe::pause();
            drop(guard);
            // Only a run that settled can have raced a re-check that landed
            // before the removal (it saw a live worker and spawned none): if
            // the job is active again, run it. Every held exit, an error and
            // a panic wait for a boot, a re-check or an invoke instead.
            if end != RunEnd::Settled || shared.closing.load(Ordering::SeqCst) {
                return;
            }
            let active = RecordStore::open(&shared.state_dir, &install)
                .and_then(|records| records.app_social_image_job(&call_id))
                .is_ok_and(|job| job.is_some_and(|job| job.state == "active"));
            if active {
                shared.spawn_image_job_worker(&install, &call_id);
            }
        });
    }

    /// Boot reconciliation: every image job still `active` when the last
    /// daemon stopped gets a worker, which replays its key against AgenticOS
    /// and settles it. Nothing is left `pending`.
    pub(super) fn reconcile_image_jobs(self: &Arc<Self>) {
        let Ok(installs) = self.store.app_tool_claim_installs() else {
            return;
        };
        for install in installs {
            let present = crate::store::app_records::record_db_path(&self.state_dir, &install)
                .is_ok_and(|path| path.is_file());
            if !present {
                continue;
            }
            let Ok(records) = RecordStore::open(&self.state_dir, &install) else {
                continue;
            };
            for call_id in records.app_social_image_jobs_active().unwrap_or_default() {
                self.spawn_image_job_worker(&install, &call_id);
            }
        }
    }

    fn image_job_run(&self, install: &str, call_id: &str) -> Result<RunEnd> {
        #[cfg(feature = "test-seam")]
        {
            test_probe::started();
            match test_probe::take_fault() {
                1 => panic!("injected image worker panic"),
                2 => return Err(Error::internal("injected image worker error")),
                _ => {}
            }
        }
        let records = RecordStore::open(&self.state_dir, install)?;
        let Some(job) = records.app_social_image_job(call_id)? else {
            return Ok(RunEnd::Held);
        };
        let Some(spec) = job.spec.clone().filter(|_| job.state == "active") else {
            return Ok(RunEnd::Held);
        };
        let settle = |settlement| records.app_social_image_job_settle(call_id, settlement);
        // A crash between the retained receipt and the settle: finish the
        // intent without another provider call.
        if let Some(receipt) = self.store.app_tool_result_for_request(&job.request_id)? {
            let id = receipt["id"].as_str().unwrap_or_default();
            settle(ImageSettlement::Succeeded { receipt: id })?;
            if let Some(created) = receipt["created_at"].as_f64() {
                records.app_social_tool_receipt_attach(&job.context_id, id, created)?;
            }
            return Ok(RunEnd::Settled);
        }
        let authority = &spec["authority"];
        let config = &authority["binding"]["config"];
        let mut wait = self.image_job_backoff;
        loop {
            #[cfg(feature = "test-seam")]
            let probe_fence = test_probe::fenced();
            #[cfg(not(feature = "test-seam"))]
            let probe_fence = false;
            if probe_fence
                || self.closing.load(Ordering::SeqCst)
                || self
                    .lease
                    .as_ref()
                    .and_then(|lease| lease.fence().check())
                    .is_some()
            {
                // Left `active`: the next daemon (or a re-check) resumes it.
                return Ok(RunEnd::Held);
            }
            // Resolved per attempt: at boot the adapter or credential may not
            // be ready yet, which is a retry, never a verdict on the job.
            let credential = {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.app_capability_credential(config)
            };
            let adapter = config["provider"]
                .as_str()
                .and_then(|provider| self.platforms.get(provider));
            let (outcome, credential) = match (adapter, credential) {
                (Some(adapter), Ok(credential)) => (
                    adapter.execute_app_image(&credential, authority, &json!({}), call_id),
                    credential,
                ),
                _ => (
                    Err(ImageFailure::uncertain(
                        ImageReason::SubmitError,
                        "image provider is not ready",
                    )),
                    Vec::new(),
                ),
            };
            let failure = match outcome {
                Ok(output) => {
                    self.image_job_succeed(&records, &job, &spec, &credential, output)?;
                    return Ok(RunEnd::Settled);
                }
                Err(failure) => failure,
            };
            let reason = failure.reason;
            match reason.settle() {
                ImageSettle::Retry if crate::store::now() < job.deadline => {
                    if reason != ImageReason::PollTimeout {
                        self.image_job_sleep(wait);
                        wait = (wait * 2).min(BACKOFF_MAX);
                    }
                }
                ImageSettle::Retry | ImageSettle::Unresolved => {
                    settle(ImageSettlement::Uncertain {
                        reason: reason.code(),
                    })?;
                    return Ok(RunEnd::Settled);
                }
                ImageSettle::Terminal => {
                    settle(ImageSettlement::Failed {
                        reason: reason.code(),
                    })?;
                    return Ok(RunEnd::Settled);
                }
            }
        }
    }

    fn image_job_succeed(
        &self,
        records: &RecordStore,
        job: &ImageJob,
        spec: &Value,
        credential: &[u8],
        output: crate::platform::AppCapabilityOutput,
    ) -> Result<()> {
        let leaked =
            crate::platform::refuse_leak("app tool result", &output.result.to_string(), credential)
                .is_err()
                || output.asset.as_ref().is_some_and(|asset| {
                    crate::platform::refuse_leak(
                        "app tool asset",
                        &String::from_utf8_lossy(&asset.bytes),
                        credential,
                    )
                    .is_err()
                });
        if leaked {
            records.app_social_image_job_settle(
                &job.call_id,
                ImageSettlement::Uncertain {
                    reason: ImageReason::JobMalformed.code(),
                },
            )?;
            return Ok(());
        }
        let text = |key: &str| spec[key].as_str().unwrap_or_default();
        let receipt = self.store.app_tool_record(AppToolRecord {
            id: &job.call_id,
            request: &job.request_id,
            install: records.install(),
            alias: text("alias"),
            slot: text("slot"),
            binding_digest: text("binding_digest"),
            input_digest: text("input_digest"),
            input: &spec["input"],
            result: &output.result,
            asset: output
                .asset
                .as_ref()
                .map(|asset| (asset.media_type.as_str(), asset.bytes.as_slice())),
        })?;
        let id = receipt["id"].as_str().unwrap_or_default();
        records.app_social_image_job_settle(
            &job.call_id,
            ImageSettlement::Succeeded { receipt: id },
        )?;
        if let Some(created) = receipt["created_at"].as_f64() {
            records.app_social_tool_receipt_attach(&job.context_id, id, created)?;
        }
        Ok(())
    }

    /// A stop lands within a fraction of a second, never after a whole wait.
    fn image_job_sleep(&self, wait: Duration) {
        let until = std::time::Instant::now() + wait;
        while !self.closing.load(Ordering::SeqCst) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100).min(wait));
        }
    }
}
