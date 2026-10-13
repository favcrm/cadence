//! CAD-1416: hosted intake and export through the AgenticOS document
//! pipeline. Selected by the daemon only when a real hosted lease is held
//! (`Shared::lease`). The origin is fixed; no body, env or caller field
//! supplies a URL. Every failure refuses. Nothing falls back to a local
//! process or to the pinned-Docker runner.
//!
//! Wire (AOS documents routes): POST `/sources` admits one original,
//! POST `/intake` and POST `/render-template` admit a job, GET `/jobs/<id>`
//! polls it, and GET `/artifacts/<ref>` returns bytes that are checked
//! against the job's artifact record before they are returned. Company and
//! identity come from the host's outbound binding, never from this client.

use std::time::{Duration, Instant};

use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{check_reference_name, Exported, Intake};
use crate::error::{Error, Result};

const ORIGIN: &str = "http://api.internal/v1/runtime/documents";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Total budget for one hosted intake or export, polling included.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL_EVERY: Duration = Duration::from_secs(1);
/// Refusal when the whole-operation budget is spent.
const EXPIRED: &str =
    "hosted document operation exceeded its 120 s deadline and was not treated as success";
/// One admitted original, and the aggregate of template + context inputs
/// and of downloaded artifacts (AOS staged-input and artifact bounds).
const BYTES_CAP: usize = 8 * 1024 * 1024;
/// Upper bound on any JSON response envelope.
const JSON_CAP: u64 = 512 * 1024;

const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const PDF_MIME: &str = "application/pdf";
const JSON_MIME: &str = "application/json";

/// Hosted structured extraction of one reference file.
pub fn intake(file_name: &str, bytes: &[u8]) -> Result<Intake> {
    // Refuse an unsafe name before any upload, exactly as native intake does.
    check_reference_name(file_name)?;
    if bytes.len() > BYTES_CAP {
        return Err(Error::rejected(
            "reference exceeds the hosted admitted size bound",
        ));
    }
    let (format, mime) = if file_name.to_ascii_lowercase().ends_with(".docx") {
        ("docx", DOCX_MIME)
    } else {
        ("pdf", PDF_MIME)
    };
    let client = Client::new();
    let source_ref = client.admit_source(file_name, format, mime, bytes)?;
    let key = idempotency_key("intake", &[&source_ref]);
    let admitted: JobData = client.post("/intake", &json!({ "source_ref": source_ref }), &key)?;
    let job = client.await_success(admitted.job, "intake", &[&source_ref])?;
    let mut budget = BYTES_CAP;
    let provenance = client
        .artifact(&job, "source.provenance.json", JSON_MIME, &mut budget)?
        .ok_or_else(|| Error::rejected("reference extraction produced no provenance"))?;
    let reconstruction =
        client.artifact(&job, "source.reconstruction.docx", DOCX_MIME, &mut budget)?;
    Ok(Intake {
        provenance,
        reconstruction,
    })
}

/// Hosted render of a retained draft template with its draft context.
pub fn export(template_docx: &[u8], context: &Value) -> Result<Exported> {
    render(template_docx, context, false)
}

/// Hosted labelled sample render of an approved version.
pub fn export_sample(template_docx: &[u8], context: &Value) -> Result<Exported> {
    render(template_docx, context, true)
}

fn render(template_docx: &[u8], context: &Value, sample: bool) -> Result<Exported> {
    let context_bytes = serde_json::to_vec(context)
        .map_err(|e| Error::internal(format!("cannot encode the template context: {e}")))?;
    if template_docx.len() + context_bytes.len() > BYTES_CAP {
        return Err(Error::rejected(
            "template and context exceed the hosted admitted size bound",
        ));
    }
    let client = Client::new();
    let template_ref = client.admit_source("template.docx", "docx", DOCX_MIME, template_docx)?;
    let context_ref = client.admit_source("context.json", "json", JSON_MIME, &context_bytes)?;
    let key = idempotency_key(
        "render-template",
        &[
            &template_ref,
            &context_ref,
            if sample { "sample" } else { "final" },
        ],
    );
    let admitted: JobData = client.post(
        "/render-template",
        &json!({ "template_ref": template_ref, "context_ref": context_ref, "sample": sample }),
        &key,
    )?;
    let job = client.await_success(
        admitted.job,
        "render-template",
        &[&template_ref, &context_ref],
    )?;
    let mut budget = BYTES_CAP;
    let docx = client
        .artifact(&job, "document.docx", DOCX_MIME, &mut budget)?
        .ok_or_else(|| Error::rejected("document export produced no DOCX"))?;
    let pdf = client
        .artifact(&job, "document.pdf", PDF_MIME, &mut budget)?
        .ok_or_else(|| Error::rejected("document export produced no PDF"))?;
    Ok(Exported { docx, pdf })
}

struct Client {
    http: ureq::Agent,
    deadline: Instant,
}

impl Client {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build();
        Self {
            http: ureq::Agent::new_with_config(config),
            deadline: Instant::now() + DEADLINE,
        }
    }

    /// Admit one original and verify the returned record names exactly the
    /// bytes uploaded. A repeated admission carries the original row's name,
    /// so the name is not compared.
    fn admit_source(&self, name: &str, format: &str, mime: &str, bytes: &[u8]) -> Result<String> {
        if bytes.len() > BYTES_CAP {
            return Err(Error::rejected(
                "source exceeds the hosted admitted size bound",
            ));
        }
        let body = json!({
            "name": name,
            "format": format,
            "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        let data: SourceData = self.post_unkeyed("/sources", &body)?;
        let source = data.source;
        let exact = valid_token(&source.id, "dcs_")
            && is_sha256_hex(&source.digest)
            && source.reference == format!("{}.{}", source.id, source.digest)
            && source.digest == sha256_hex(bytes)
            && source.bytes == bytes.len() as u64
            && source.format == format
            && source.mime == mime;
        if !exact {
            return Err(Error::rejected(
                "hosted source admission did not match the uploaded bytes",
            ));
        }
        Ok(source.reference)
    }

    /// Time left in the whole operation. An expired budget refuses, so no
    /// request starts after the deadline.
    fn remaining(&self) -> Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| Error::rejected(EXPIRED))
    }

    /// Per-request bound: the smaller of the request timeout and what is left.
    fn timeout(&self) -> Result<Duration> {
        Ok(self.remaining()?.min(REQUEST_TIMEOUT))
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: &Value, key: &str) -> Result<T> {
        let request = self
            .http
            .post(format!("{ORIGIN}{path}"))
            .header("idempotency-key", key);
        let mut response = bounded(request, self.timeout()?)
            .send_json(body)
            .map_err(unavailable)?;
        read(&mut response)
    }

    fn post_unkeyed<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let request = self.http.post(format!("{ORIGIN}{path}"));
        let mut response = bounded(request, self.timeout()?)
            .send_json(body)
            .map_err(unavailable)?;
        read(&mut response)
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let request = self.http.get(format!("{ORIGIN}{path}"));
        let mut response = bounded(request, self.timeout()?)
            .call()
            .map_err(unavailable)?;
        read(&mut response)
    }

    /// Poll a known job id until it succeeds. `failed`, `refused`, an
    /// unknown status or the total deadline refuse; nothing is inferred.
    fn await_success(&self, mut job: JobWire, kind: &str, sources: &[&str]) -> Result<JobWire> {
        verify_job(&job, None, kind, sources)?;
        loop {
            match job.status.as_str() {
                "succeeded" => return Ok(job),
                "failed" | "refused" => {
                    return Err(Error::rejected(format!(
                        "hosted document job {}: {}",
                        job.status,
                        error_code(job.error.as_deref())
                    )))
                }
                "admitted" | "running" => {}
                _ => return Err(malformed()),
            }
            // Sleep no longer than the budget left. The next `get` re-checks
            // the deadline before it sends, and refuses once it has passed.
            std::thread::sleep(POLL_EVERY.min(self.remaining()?));
            let id = job.id.clone();
            job = self.get::<JobData>(&format!("/jobs/{id}"))?.job;
            verify_job(&job, Some(&id), kind, sources)?;
        }
    }

    /// Download the one artifact named `name`. `Ok(None)` when the job did
    /// not publish it; a duplicate, a record that does not bind to this job
    /// and digest, or bytes that do not match refuse.
    fn artifact(
        &self,
        job: &JobWire,
        name: &str,
        mime: &str,
        budget: &mut usize,
    ) -> Result<Option<Vec<u8>>> {
        let mut found = job.artifacts.iter().filter(|a| a.name == name);
        let Some(record) = found.next() else {
            return Ok(None);
        };
        if found.next().is_some() {
            return Err(malformed());
        }
        let exact = is_sha256_hex(&record.digest)
            && record.reference == format!("{}.{}", job.id, record.digest)
            && record.mime == mime;
        let declared = usize::try_from(record.bytes).map_err(|_| malformed())?;
        if !exact || declared > *budget {
            return Err(Error::rejected(
                "hosted document artifact record does not match the expected output",
            ));
        }
        *budget -= declared;

        let request = self
            .http
            .get(format!("{ORIGIN}/artifacts/{}", record.reference));
        let mut response = bounded(request, self.timeout()?)
            .call()
            .map_err(unavailable)?;
        if response.status().as_u16() != 200 {
            return Err(Error::rejected("hosted document artifact was not served"));
        }
        let header_digest = response
            .headers()
            .get("x-artifact-digest")
            .and_then(|v| v.to_str().ok());
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::trim);
        if header_digest != Some(record.digest.as_str()) || content_type != Some(mime) {
            return Err(Error::rejected(
                "hosted document artifact headers do not match its record",
            ));
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(declared as u64)
            .read_to_vec()
            .map_err(|_| Error::rejected("hosted document artifact exceeded its declared size"))?;
        if bytes.len() != declared || sha256_hex(&bytes) != record.digest {
            return Err(Error::rejected(
                "hosted document artifact bytes do not match its digest",
            ));
        }
        Ok(Some(bytes))
    }
}

/// Apply the per-request timeout. ureq's global timeout covers DNS, connect,
/// send and the body read, so the request ends within its budget. Other
/// config (no redirects, no proxy, status handling) is inherited from the agent.
fn bounded<B>(request: ureq::RequestBuilder<B>, timeout: Duration) -> ureq::RequestBuilder<B> {
    request.config().timeout_global(Some(timeout)).build()
}

/// Parse one response. Only `200`/`201` with `{ok:true,data}` succeed. A
/// refusal keeps only its sanitized error code.
fn read<T: DeserializeOwned>(response: &mut ureq::http::Response<ureq::Body>) -> Result<T> {
    let status = response.status().as_u16();
    let bytes = response
        .body_mut()
        .with_config()
        .limit(JSON_CAP)
        .read_to_vec()
        .map_err(unavailable)?;
    if status != 200 && status != 201 {
        let code = serde_json::from_slice::<Failure>(&bytes)
            .ok()
            .map(|f| f.error.code)
            .filter(|code| valid_code(code))
            .unwrap_or_else(|| "error".to_string());
        return Err(Error::rejected(format!(
            "hosted document request refused: {code} (HTTP {status})"
        )));
    }
    let envelope: Envelope<T> = serde_json::from_slice(&bytes).map_err(|_| malformed())?;
    if !envelope.ok {
        return Err(malformed());
    }
    Ok(envelope.data)
}

fn verify_job(job: &JobWire, id: Option<&str>, kind: &str, sources: &[&str]) -> Result<()> {
    let mut want: Vec<&str> = sources.to_vec();
    want.sort_unstable();
    let mut got: Vec<&str> = job.sources.iter().map(String::as_str).collect();
    got.sort_unstable();
    let exact = valid_token(&job.id, "doc_")
        && id.is_none_or(|expected| expected == job.id)
        && job.kind == kind
        && got == want;
    if exact {
        Ok(())
    } else {
        Err(Error::rejected(
            "hosted document job does not match the admitted request",
        ))
    }
}

/// Deterministic key over the operation and its bound inputs, so a retry of
/// the same snapshot replays one job and a different input cannot collide.
fn idempotency_key(op: &str, parts: &[&str]) -> String {
    let mut material = op.to_string();
    for part in parts {
        material.push('\n');
        material.push_str(part);
    }
    sha256_hex(material.as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `<prefix><1..80 of [A-Za-z0-9_-]>`, the AOS job and source id shape.
fn valid_token(text: &str, prefix: &str) -> bool {
    text.strip_prefix(prefix).is_some_and(|rest| {
        (1..=80).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn valid_code(code: &str) -> bool {
    code.len() <= 80 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn error_code(raw: Option<&str>) -> &str {
    raw.filter(|code| valid_code(code)).unwrap_or("error")
}

fn malformed() -> Error {
    Error::rejected("hosted document response is malformed")
}

fn unavailable(error: impl std::fmt::Display) -> Error {
    Error::rejected(format!("hosted document converter is unavailable: {error}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    ok: bool,
    data: T,
}

#[derive(Deserialize)]
struct Failure {
    error: FailureBody,
}

#[derive(Deserialize)]
struct FailureBody {
    code: String,
}

#[derive(Deserialize)]
struct SourceData {
    source: SourceWire,
}

#[derive(Deserialize)]
struct SourceWire {
    #[serde(rename = "ref")]
    reference: String,
    id: String,
    digest: String,
    bytes: u64,
    mime: String,
    format: String,
}

#[derive(Deserialize)]
struct JobData {
    job: JobWire,
}

#[derive(Deserialize)]
struct JobWire {
    id: String,
    kind: String,
    status: String,
    sources: Vec<String>,
    artifacts: Vec<ArtifactWire>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ArtifactWire {
    #[serde(rename = "ref")]
    reference: String,
    digest: String,
    bytes: u64,
    mime: String,
    name: String,
}
