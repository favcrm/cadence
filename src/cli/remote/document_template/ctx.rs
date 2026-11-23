//! Remote RPC context, bounded blob paging/upload, and digest helpers for hosted templates and documents.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cadence_agent::error::{Error, Result};
use cadence_agent::output::eprint_line;
use cadence_agent::remote_cli::{
    call_verb, upload_template, RemoteAnswer, RemoteTarget, TemplateUpload,
};
use serde_json::{json, Map, Value};

use crate::cli::business_templates as bt;

pub(super) const TEMPLATE_BLOB_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub(super) const DOCX_MIME: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

pub(super) fn obj_args(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Default::default(),
    }
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(super) fn verify_sha256_hex(bytes: &[u8], expected: &str, ctx: &str) -> Result<()> {
    if !is_hex64(expected) {
        return Err(Error::rejected(format!(
            "{ctx}: invalid sha256 digest '{expected}'. Nothing was written"
        )));
    }
    if !cadence_agent::business_templates::sha256_hex(bytes).eq_ignore_ascii_case(expected) {
        return Err(Error::rejected(format!(
            "{ctx} do not match its sha256 digest. Nothing was written"
        )));
    }
    Ok(())
}

pub(super) struct RemoteCtx<'a> {
    target: &'a RemoteTarget,
    deadline: Instant,
    wake: u64,
    fb: &'static str,
}

impl<'a> RemoteCtx<'a> {
    pub(super) fn new(target: &'a RemoteTarget, wake: u64, fb: &'static str) -> Result<Self> {
        let dur = Duration::from_secs(wake);
        let deadline = Instant::now()
            .checked_add(dur)
            .ok_or_else(|| Error::usage("invalid wake timeout"))?;
        Ok(Self {
            target,
            deadline,
            wake,
            fb,
        })
    }

    pub(super) fn call(&self, verb: &str, args: Map<String, Value>) -> Result<Value> {
        match call_verb(
            self.target,
            verb,
            args,
            self.deadline,
            self.wake,
            eprint_line,
        )? {
            RemoteAnswer::Ok(body) => Ok(body),
            RemoteAnswer::Conflict(b) => Err(Error::rejected(
                b.pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or(self.fb),
            )),
        }
    }

    pub(super) fn read_blob(&self, key: &str, expected: Option<&str>) -> Result<Vec<u8>> {
        let msg =
            "remote answered a malformed or inconsistent template blob page; nothing was written";
        let bad = || Error::rejected(msg);
        if expected.is_some_and(|e| !is_hex64(e)) {
            return Err(bad());
        }
        let mut bytes = Vec::new();
        let mut total_pin: Option<usize> = None;
        let mut sha_pin = expected.map(str::to_ascii_lowercase);
        loop {
            let a = obj_args(json!({"blob_key": key, "offset": bytes.len()}));
            let RemoteAnswer::Ok(b) = call_verb(
                self.target,
                "template_blob_read",
                a,
                self.deadline,
                self.wake,
                eprint_line,
            )?
            else {
                return Err(Error::rejected("remote template blob read conflicted"));
            };
            if b.get("blob_key").and_then(Value::as_str) != Some(key) {
                return Err(bad());
            }
            let r = b.get("range").and_then(Value::as_object).ok_or_else(bad)?;
            let gu = |k: &str| r.get(k).and_then(Value::as_u64);
            let start = gu("start")
                .filter(|v| *v == bytes.len() as u64)
                .ok_or_else(bad)?;
            let end = gu("end").ok_or_else(bad)?;
            let cap = |t: u64| {
                (t <= TEMPLATE_BLOB_MAX_BYTES)
                    .then(|| usize::try_from(t).ok())
                    .flatten()
            };
            let total = gu("total").and_then(cap).ok_or_else(bad)?;
            if start > end || end > total as u64 || total_pin.is_some_and(|t| t != total) {
                return Err(bad());
            }
            total_pin = Some(total);
            let ds = b
                .get("sha256")
                .and_then(Value::as_str)
                .filter(|d| is_hex64(d));
            let dig = ds.map(str::to_ascii_lowercase).ok_or_else(bad)?;
            if sha_pin.as_ref().is_some_and(|e| e != &dig) {
                return Err(bad());
            }
            sha_pin.get_or_insert(dig);
            let enc = b
                .get("data_base64")
                .and_then(Value::as_str)
                .ok_or_else(bad)?;
            if enc.len() > (total - bytes.len()).div_ceil(3) * 4 {
                return Err(bad());
            }
            let prev = bytes.len();
            B64.decode_vec(enc, &mut bytes).map_err(|_| bad())?;
            let stalled = bytes.len() == prev && bytes.len() < total;
            if bytes.len() != end as usize || stalled {
                return Err(bad());
            }
            if bytes.len() == total {
                break;
            }
        }
        let pin = sha_pin.as_deref().expect("sha256 pinned");
        verify_sha256_hex(&bytes, pin, "the template blob's bytes")?;
        Ok(bytes)
    }

    pub(super) fn upload(
        &self,
        path: &Path,
        mime: &str,
        cap: u64,
        lbl: &str,
        raw: Option<&[u8]>,
    ) -> Result<(String, String)> {
        let owned;
        let bytes = match raw {
            Some(b) => b,
            None => {
                owned = bt::read_bounded_template_file(path, cap, lbl)?;
                &owned
            }
        };
        let n_opt = path.file_name().and_then(|n| n.to_str());
        let name = n_opt
            .ok_or_else(|| Error::rejected("template file needs a file name"))?
            .to_string();
        let part = cadence_agent::wiki::upload::part_file_name(&name);
        let req = TemplateUpload {
            media_type: mime,
            file_name: &part,
            bytes,
        };
        let up = upload_template(self.target, &req, self.wake, eprint_line)?;
        let key = up["blob"]["blob_key"].as_str().filter(|k| !k.is_empty());
        let key = key.ok_or_else(|| Error::unknown("template upload did not return a blob_key"))?;
        Ok((name, key.to_string()))
    }

    pub(super) fn prepare_template(
        &self,
        name: &str,
        base: Option<i64>,
        html: Option<&PathBuf>,
        docx: Option<&PathBuf>,
        file: &PathBuf,
    ) -> Result<(&'static str, Map<String, Value>)> {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let spec = bt::read_json(file)?;
        let authoring = spec.get("kind").and_then(Value::as_str) == Some("authoring");
        let mut args = obj_args(json!({"name": name, "spec": spec}));
        if let Some(b) = base {
            args.insert("base".into(), json!(b));
        }
        if authoring {
            if html.is_some() || docx.is_some() {
                return Err(Error::rejected(
                    "an authoring pack does not accept --html or --docx",
                ));
            }
        } else if let Some(hp) = html {
            let s = bt::read_html_source(hp)?;
            let (_, key) = self.upload(hp, "text/html", 0, "", Some(s.as_bytes()))?;
            args.insert("source_blob_key".into(), json!(key));
        } else if let Some(dp) = docx {
            let b = bt::read_bounded_template_file(dp, TEMPLATE_BLOB_MAX_BYTES, "DOCX template")?;
            args.insert("docx_base64".into(), json!(B64.encode(b)));
        } else {
            return Err(Error::rejected(
                "a quotation or invoice template requires --html (or --docx on a local daemon)",
            ));
        }
        Ok((
            if base.is_some() {
                "template_revise"
            } else {
                "template_create"
            },
            args,
        ))
    }

    pub(super) fn write_outputs(
        &self,
        out: &Path,
        stem: &str,
        format: Option<&str>,
        reply: &Value,
        mode: &str,
    ) -> Result<Value> {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let fmt = format.unwrap_or("html");
        let mut summary = if mode == "sample" {
            obj_args(json!({"sample": true, "context": reply["context"]}))
        } else {
            let mut m = obj_args(json!({"id": reply["id"]}));
            if let Some(a) = reply.get("artifact") {
                m.insert("artifact".into(), a.clone());
            }
            m
        };
        let exp_sha = |tag: &str, k: &str| -> Option<&str> {
            (mode == "export" && (fmt == tag || fmt == "both"))
                .then(|| {
                    reply
                        .get(k)
                        .or_else(|| (fmt == tag).then(|| reply.get("sha256")).flatten())
                        .and_then(Value::as_str)
                })
                .flatten()
        };
        let resolve = |inl: Option<&str>,
                       b64: &str,
                       bk: &str,
                       sha: Option<&str>,
                       lbl: &str|
         -> Result<Vec<u8>> {
            if reply.get("requires_blob_read").and_then(Value::as_bool) != Some(true) {
                let inline = if let Some(e) = reply.get(b64).and_then(Value::as_str) {
                    Some(B64.decode(e).map_err(|_| {
                        Error::rejected(format!("remote returned invalid {lbl} base64"))
                    })?)
                } else {
                    inl.and_then(|k| reply.get(k))
                        .and_then(Value::as_str)
                        .map(|s| s.as_bytes().to_vec())
                };
                if let Some(bytes) = inline {
                    if let Some(exp) = sha {
                        verify_sha256_hex(&bytes, exp, &format!("the exported {lbl} bytes"))?;
                    }
                    return Ok(bytes);
                }
            }
            let key = reply
                .get(bk)
                .or_else(|| reply.get("blob_key"))
                .and_then(Value::as_str)
                .filter(|k| !k.is_empty())
                .ok_or_else(|| {
                    Error::rejected(format!(
                        "remote response included neither {lbl} bytes nor {bk}"
                    ))
                })?;
            self.read_blob(key, sha)
        };
        bt::write_rendered_outputs(
            out,
            stem,
            format,
            &mut summary,
            || {
                resolve(
                    Some("html"),
                    "html_base64",
                    "html_blob_key",
                    exp_sha("html", "html_sha256"),
                    "HTML",
                )
            },
            || {
                resolve(
                    None,
                    "pdf_base64",
                    "pdf_blob_key",
                    exp_sha("pdf", "pdf_sha256"),
                    "PDF",
                )
            },
        )?;
        Ok(Value::Object(summary))
    }
}
