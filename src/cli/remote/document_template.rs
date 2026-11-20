//! Hosted `cadence template` and `cadence document` command handlers.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cadence_agent::error::{Error, Result};
use cadence_agent::output::{eprint_line, print_json};
use cadence_agent::remote_cli::{
    call_verb, upload_template, RemoteAnswer, RemoteTarget, TemplateUpload,
};
use serde_json::{json, Map, Value};

use super::super::business_templates::{self as bt, DocumentAction, TemplateAction};
use super::merge_gate::refuse_managed;

const TEMPLATE_BLOB_MAX_BYTES: u64 = 10 * 1024 * 1024;
const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

fn obj_args(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Default::default(),
    }
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn verify_sha256_hex(bytes: &[u8], expected: &str, ctx: &str) -> Result<()> {
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

struct RemoteCtx<'a> {
    target: &'a RemoteTarget,
    deadline: Instant,
    wake: u64,
    fb: &'static str,
}

impl<'a> RemoteCtx<'a> {
    fn new(target: &'a RemoteTarget, wake: u64, fb: &'static str) -> Result<Self> {
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

    fn call(&self, verb: &str, args: Map<String, Value>) -> Result<Value> {
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

    fn read_blob(&self, key: &str, expected: Option<&str>) -> Result<Vec<u8>> {
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

    fn upload(
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

    fn prepare_template(
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

    fn write_outputs(
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

fn push_save_artifact_args(args: &mut Map<String, Value>, a: &bt::ArtifactSaveArgs) {
    if a.save_artifact {
        args.insert("save_artifact".into(), json!(true));
        for (k, v) in [
            ("title", &a.title),
            ("label", &a.label),
            ("customer", &a.customer),
            ("issue", &a.issue),
        ] {
            if let Some(val) = v {
                args.insert(k.into(), json!(val));
            }
        }
    }
}

pub(super) fn remote_template(
    target: &RemoteTarget,
    action: &bt::TemplateAction,
    wake: u64,
) -> Result<i32> {
    let cx = RemoteCtx::new(target, wake, "remote template operation conflicted")?;
    match action {
        TemplateAction::List => print_json(&cx.call("template_list", Default::default())?),
        TemplateAction::Create {
            name,
            html,
            docx,
            file,
        } => {
            let (v, a) = cx.prepare_template(name, None, html.as_ref(), docx.as_ref(), file)?;
            print_json(&cx.call(v, a)?);
        }
        TemplateAction::Revise {
            name,
            base,
            html,
            docx,
            file,
        } => {
            let (v, a) =
                cx.prepare_template(name, Some(*base), html.as_ref(), docx.as_ref(), file)?;
            print_json(&cx.call(v, a)?);
        }
        TemplateAction::Show { name, version, out } => {
            if out.is_some() && version.is_none() {
                return Err(Error::rejected("template show --out requires --version"));
            }
            let mut args = obj_args(json!({"name": name}));
            if let Some(v) = version {
                args.insert("version".into(), json!(v));
            }
            let found = cx.call("template_show", args)?;
            if let Some(out_path) = out {
                if found["spec"]["kind"].as_str() == Some("authoring") {
                    print_json(&bt::materialise_authoring_pack(out_path, found)?);
                } else if found["template"]["source_format"].as_str() == Some("html")
                    || found.get("html").is_some()
                {
                    let sha = found["template"]["source_sha256"].as_str();
                    let bytes = if let Some(inl) = found.get("html").and_then(Value::as_str) {
                        let b = inl.as_bytes().to_vec();
                        if let Some(s) = sha {
                            verify_sha256_hex(&b, s, "the template's inline HTML")?;
                        }
                        b
                    } else {
                        let key = found["source_blob_key"].as_str().filter(|k| !k.is_empty()).ok_or_else(|| Error::rejected("template show returned neither inline html nor source_blob_key"))?;
                        cx.read_blob(key, sha)?
                    };
                    bt::write_new_file(out_path, &bytes)?;
                    print_json(
                        &json!({"template": found["template"], "spec": found["spec"], "written": out_path}),
                    );
                } else {
                    return Err(Error::rejected(
                        "template show --out is supported for authoring packs and HTML templates",
                    ));
                }
            } else {
                print_json(&found);
            }
        }
        TemplateAction::Intake { file, out } => {
            if out.is_some() {
                return Err(Error::rejected("hosted template intake returns suggested_html in JSON and does not write a DOCX reconstruction (--out is local-only)"));
            }
            if !file
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("docx"))
            {
                return Err(Error::rejected(
                    "hosted template intake supports .docx files only; pass a .docx reference file",
                ));
            }
            let (fname, bkey) = cx.upload(
                file,
                DOCX_MIME,
                TEMPLATE_BLOB_MAX_BYTES,
                "template intake",
                None,
            )?;
            print_json(&cx.call(
                "template_intake",
                obj_args(json!({"file": fname, "source_blob_key": bkey})),
            )?);
        }
        TemplateAction::Sample {
            name,
            version,
            out,
            format,
        } => {
            let format = bt::normalize_sample_or_export_format(format.as_deref())?;
            if format.as_deref() == Some("docx") {
                return Err(Error::rejected(
                    "remote `template sample` supports html, pdf, or both (not docx)",
                ));
            }
            let mut args = obj_args(json!({"name": name}));
            if let Some(v) = version {
                args.insert("version".into(), json!(v));
            }
            if let Some(f) = &format {
                args.insert("format".into(), json!(f));
            }
            let reply = cx.call("template_sample", args)?;
            let stem = format!(
                "{name}-v{}-sample",
                reply["version"].as_i64().or(*version).unwrap_or(1)
            );
            print_json(&cx.write_outputs(out, &stem, format.as_deref(), &reply, "sample")?);
        }
        TemplateAction::Approve {
            name,
            version,
            sha256,
            source,
        } => {
            refuse_managed(target, "template approve")?;
            print_json(&cx.call(
                "template_approve",
                obj_args(
                    json!({"name": name, "version": version, "digest": sha256, "source": source}),
                ),
            )?);
        }
    }
    Ok(0)
}

pub(super) fn remote_document(
    target: &RemoteTarget,
    action: &bt::DocumentAction,
    wake: u64,
) -> Result<i32> {
    let cx = RemoteCtx::new(target, wake, "remote document operation conflicted")?;
    match action {
        DocumentAction::Create {
            template,
            version,
            file,
            allow_partial,
            artifact,
        } => {
            bt::validate_artifact_meta_flags(artifact)?;
            let mut args = obj_args(json!({"template": template, "data": bt::read_json(file)?}));
            if let Some(v) = version {
                args.insert("version".into(), json!(v));
            }
            if *allow_partial {
                args.insert("allow_partial".into(), json!(true));
            }
            push_save_artifact_args(&mut args, artifact);
            print_json(&cx.call("document_create", args)?);
        }
        DocumentAction::Show { id } => {
            print_json(&cx.call("document_show", obj_args(json!({"id": id})))?)
        }
        DocumentAction::Export {
            id,
            out,
            format,
            artifact,
        } => {
            bt::validate_artifact_meta_flags(artifact)?;
            bt::validate_export_target(out, artifact.save_artifact)?;
            let format = bt::normalize_sample_or_export_format(format.as_deref())?;
            if format.as_deref() == Some("docx") {
                return Err(Error::rejected(
                    "remote `document export` supports html, pdf, or both (not docx)",
                ));
            }
            let mut args = obj_args(json!({"id": id}));
            if let Some(f) = &format {
                args.insert("format".into(), json!(f));
            }
            push_save_artifact_args(&mut args, artifact);
            let reply = cx.call("document_export", args)?;
            if let Some(out_path) = out {
                print_json(&cx.write_outputs(out_path, id, format.as_deref(), &reply, "export")?);
            } else {
                print_json(&reply);
            }
        }
    }
    Ok(0)
}
