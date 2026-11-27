//! Hosted `cadence template` and `cadence document` command handlers.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use cadence_agent::error::{Error, Result};
use cadence_agent::output::print_json;
use cadence_agent::remote_cli::RemoteTarget;
use serde_json::{json, Map, Value};

use super::super::business_templates::{self as bt, DocumentAction, TemplateAction};
use super::merge_gate::refuse_managed;

mod ctx;

use ctx::{obj_args, verify_sha256_hex, RemoteCtx, DOCX_MIME, TEMPLATE_BLOB_MAX_BYTES};

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
