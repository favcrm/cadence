//! The AgenticOS media door in its REAL job shape, for host tests (CAD-1315).
//!
//! Derived from agenticos-v2 @ 583c9101, not from the host's own parser:
//! - routes (`POST /image`, `GET /jobs/:id`, `GET /artifacts/:ref`,
//!   `GET /price/image`): `apps/api/src/media/routes.ts:1383-1401` (hosted)
//!   and `:1360-1381` (`handleMedia`);
//! - a fresh submit is 201 `submitted` (`routes.ts:895-901`); a replay of the
//!   same key and body is 200 with `repeated:true` (`:807-848`), 409
//!   `key_conflict` for another body (`:808-810`) and 409 `uncertain` for an
//!   `uncertain` job (`:812-818`);
//! - a poll moves `submitted -> running -> succeeded` and imports the
//!   artifact (`pollMediaJob`, `:1073-1180`); `failed` keeps the hold open;
//! - the body is the strict `imageInferenceRequestSchema`
//!   (`packages/contracts/src/inference.ts:90-103`);
//! - a job is `mediaJobViewSchema` (`packages/contracts/src/media.ts:118-142`),
//!   built by `jobView` (`apps/api/src/media/jobs.ts:390-420`): provider is
//!   always `"kie"`, artifacts are `{ref,digest,bytes,mime}`;
//! - the artifact is served with `content-type` and `x-artifact-digest`
//!   (`routes.ts:1346-1357`); AgenticOS imports up to 10 MiB
//!   (`jobs.ts:31`).
//!
//! The artifact is a 1024x1024 noise PNG (~3 MiB): above the host's 2 MiB
//! custody bound, inside AgenticOS's 10 MiB cap.
#![allow(dead_code)]

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering::SeqCst};
use std::sync::{Mutex, OnceLock};

pub const IMAGE_MODEL: &str = "openai/gpt-image-2.5";
const PRICE_VERSION: &str = "2026-09-29T00:00:00.000Z";
const AT: &str = "2026-10-09T17:00:00.000Z";

/// What a poll does to a job (read at poll time, so a test can flip it).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// submitted -> running -> succeeded.
    Normal = 0,
    /// Stays `running` forever.
    Hold = 1,
    /// running -> `failed` (AgenticOS keeps the hold open for reconciliation).
    Fail = 2,
    /// The job becomes `uncertain`: polls and replays answer 409 `uncertain`.
    Uncertain = 3,
    /// Every request answers 503 before it is recorded.
    Down = 4,
}

struct Job {
    id: String,
    key: String,
    body: Value,
    status: &'static str,
    polls: u32,
    artifact: Option<(String, String, usize)>,
}

#[derive(Default)]
struct State {
    jobs: Vec<Job>,
    artifacts: HashMap<String, Vec<u8>>,
    log: Vec<String>,
}

pub struct MediaDoor {
    mode: AtomicU8,
    state: Mutex<State>,
}

/// A deterministic 1024x1024 RGB noise PNG, one per `n % 4`.
pub fn noise_png(n: u8) -> &'static Vec<u8> {
    static CACHE: [OnceLock<Vec<u8>>; 4] = [
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
    ];
    CACHE[(n % 4) as usize].get_or_init(|| {
        use image::ImageEncoder as _;
        let mut seed =
            0x9E37_79B9_7F4A_7C15u64 ^ (n as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03);
        let mut pixels = vec![0u8; 1024 * 1024 * 3];
        for byte in pixels.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = (seed >> 24) as u8;
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, 1024, 1024, image::ExtendedColorType::Rgb8)
            .unwrap();
        png
    })
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn failure(status: u16, code: &str) -> (u16, Vec<u8>) {
    (
        status,
        json!({"ok":false,"error":{"code":code,"message":code}})
            .to_string()
            .into_bytes(),
    )
}

impl Default for MediaDoor {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaDoor {
    pub fn new() -> Self {
        Self {
            mode: AtomicU8::new(Scenario::Normal as u8),
            state: Mutex::default(),
        }
    }

    pub fn set(&self, scenario: Scenario) {
        self.mode.store(scenario as u8, SeqCst);
    }

    fn scenario(&self) -> Scenario {
        match self.mode.load(SeqCst) {
            1 => Scenario::Hold,
            2 => Scenario::Fail,
            3 => Scenario::Uncertain,
            4 => Scenario::Down,
            _ => Scenario::Normal,
        }
    }

    /// Every media request in order: `POST image key=<k>` / `GET job <id>`.
    pub fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    /// The idempotency key of every submit, in order.
    pub fn submit_keys(&self) -> Vec<String> {
        self.log()
            .iter()
            .filter_map(|l| l.strip_prefix("POST image key=").map(str::to_owned))
            .collect()
    }

    /// Jobs AgenticOS created, one per distinct caller key.
    pub fn job_count(&self) -> usize {
        self.state.lock().unwrap().jobs.len()
    }

    fn view(job: &Job, repeated: bool, cached: bool) -> Value {
        let artifacts: Vec<Value> = job
            .artifact
            .iter()
            .map(|(r, d, n)| json!({"ref":r,"digest":d,"bytes":n,"mime":"image/png"}))
            .collect();
        let settled = job.status == "succeeded";
        json!({
            "id": job.id, "kind": "image", "status": job.status, "model": IMAGE_MODEL,
            "provider": "kie",
            "providerTaskId": if job.status == "admitting" { Value::Null } else { json!(format!("kie_{}", &job.id[4..])) },
            "artifacts": artifacts, "artifactError": null,
            "usage": {"providerCredits": if settled { json!(6.0) } else { Value::Null }},
            "price": {"slug":"generate_image","chargeMinor":31500,"currency":"USD","version":PRICE_VERSION},
            "billing": if settled { "settled" } else if job.status == "failed" { "uncertain" } else { "reserved" },
            "repeated": repeated, "cached": cached, "stale": false,
            "createdAt": AT, "updatedAt": AT,
        })
    }

    /// Answer one media request, or `None` when the route is not a media one.
    /// `(status, content-type, body)`.
    pub fn reply(
        &self,
        method: &str,
        route: &str,
        key: Option<&str>,
        body: &[u8],
    ) -> Option<(u16, &'static str, Vec<u8>)> {
        let json_reply =
            |(status, bytes): (u16, Vec<u8>)| Some((status, "application/json", bytes));
        let ok = |status: u16, data: Value| {
            (
                status,
                json!({"ok": true, "data": data}).to_string().into_bytes(),
            )
        };
        if !route.starts_with("/v1/runtime/media/") {
            return None;
        }
        if self.scenario() == Scenario::Down {
            return json_reply(failure(503, "capability_unavailable"));
        }
        let mut state = self.state.lock().unwrap();
        match (method, route) {
            ("GET", "/v1/runtime/media/price/image") => json_reply(ok(
                200,
                json!({"kind":"image","model":IMAGE_MODEL,
                    "price":{"slug":"generate_image","chargeMinor":31500,"currency":"USD","version":PRICE_VERSION}}),
            )),
            ("POST", "/v1/runtime/media/image") => {
                let key = key.unwrap_or_default().to_owned();
                state.log.push(format!("POST image key={key}"));
                let key_ok = (8..=128).contains(&key.len())
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
                let parsed: Option<Value> = serde_json::from_slice(body).ok();
                let strict = parsed.as_ref().is_some_and(|v| {
                    v.as_object().is_some_and(|o| {
                        o.keys().all(|k| {
                            matches!(
                                k.as_str(),
                                "model" | "prompt" | "aspectRatio" | "resolution" | "references"
                            )
                        }) && o
                            .get("prompt")
                            .is_some_and(|p| p.as_str().is_some_and(|p| !p.trim().is_empty()))
                    })
                });
                if !key_ok || !strict {
                    return json_reply(failure(400, "invalid_request"));
                }
                let body = parsed.unwrap();
                if let Some(job) = state.jobs.iter().find(|j| j.key == key) {
                    if job.body != body {
                        return json_reply(failure(409, "key_conflict"));
                    }
                    if job.status == "uncertain" {
                        return json_reply(failure(409, "uncertain"));
                    }
                    return json_reply(ok(200, json!({"job": Self::view(job, true, true)})));
                }
                let id = format!("med_{:06}", state.jobs.len() + 1);
                let job = Job {
                    id,
                    key,
                    body,
                    status: "submitted",
                    polls: 0,
                    artifact: None,
                };
                let view = Self::view(&job, false, false);
                state.jobs.push(job);
                json_reply(ok(201, json!({"job": view})))
            }
            ("GET", job) if job.starts_with("/v1/runtime/media/jobs/") => {
                let id = &job["/v1/runtime/media/jobs/".len()..];
                state.log.push(format!("GET job {id}"));
                let mode = self.scenario();
                let State {
                    jobs, artifacts, ..
                } = &mut *state;
                let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
                    return json_reply(failure(404, "not_found"));
                };
                match job.status {
                    "uncertain" => return json_reply(failure(409, "uncertain")),
                    "succeeded" | "failed" => {
                        return json_reply(ok(200, json!({"job": Self::view(job, true, true)})))
                    }
                    _ => {}
                }
                job.polls += 1;
                match mode {
                    Scenario::Hold => job.status = "running",
                    Scenario::Fail => {
                        job.status = if job.polls >= 2 { "failed" } else { "running" }
                    }
                    Scenario::Uncertain => {
                        job.status = "uncertain";
                        return json_reply(failure(409, "uncertain"));
                    }
                    _ if job.polls < 2 => job.status = "running",
                    _ => {
                        let bytes = noise_png(jobs_len_hint(&job.id)).clone();
                        let digest = hex(&bytes);
                        let reference = format!("{}.{digest}", job.id);
                        job.artifact = Some((reference.clone(), digest, bytes.len()));
                        job.status = "succeeded";
                        artifacts.insert(reference, bytes);
                    }
                }
                json_reply(ok(200, json!({"job": Self::view(job, true, false)})))
            }
            ("GET", artifact) if artifact.starts_with("/v1/runtime/media/artifacts/") => {
                let reference = &artifact["/v1/runtime/media/artifacts/".len()..];
                match state.artifacts.get(reference) {
                    Some(bytes) => Some((200, "image/png", bytes.clone())),
                    None => json_reply(failure(404, "not_found")),
                }
            }
            _ => None,
        }
    }
}

fn jobs_len_hint(id: &str) -> u8 {
    id.bytes().last().map(|b| b % 4).unwrap_or(0)
}
