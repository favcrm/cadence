//! Context-scoped social draft operations through a live, server-minted screen action.

#[cfg(all(test, feature = "test-seam"))]
#[path = "app_social_discard_gate_test.rs"]
mod app_social_discard_gate_test;
// SOC-55: independently authored acceptance for the real source-receipt guard
// (owned by the acceptance author; this lane only wires it in).
#[cfg(all(test, feature = "test-seam"))]
#[path = "cad2026_source_guard_test.rs"]
mod cad2026_source_guard_test;
use super::{app_bindings_rpc::strict_fields, app_tools_rpc::ToolContext, required_str, Shared};
use crate::issue::app_catalog::workspace;
use crate::{
    error::{Error, Result},
    operator_auth::Origin,
    store::{
        app_records::RecordStore,
        app_social_drafts::{DraftSource, SourceCandidate, SourceCursor},
    },
};
use serde_json::{json, Value};

fn origin(params: &Value) -> Result<Origin> {
    match params["origin"].as_str() {
        Some("loopback") => Ok(Origin::Loopback),
        Some("tailnet") => Ok(Origin::Tailnet),
        Some("public") => Ok(Origin::Public),
        _ => Err(Error::rejected(
            "social draft action needs a classified session origin",
        )),
    }
}
fn local_social_draft_write(method: &str) -> Result<bool> {
    match method {
        "app_social_draft_list"
        | "app_social_draft_show"
        | "app_social_sources_show"
        | "app_social_sources_page"
        | "app_social_sources_get"
        | "app_social_upload_asset" => Ok(false),
        "app_social_draft_asset"
        | "app_social_image_upload"
        | "app_social_draft_create"
        | "app_social_draft_update"
        | "app_social_draft_discard"
        | "app_social_sources_save" => Ok(true),
        _ => Err(Error::rejected("unknown social draft action")),
    }
}

#[derive(Clone, Copy)]
enum BindingRequirement {
    Required,
    LocalPersistence,
}

/// SOC-55: one page of the retained source library. Count- and byte-bounded
/// so a page is a bounded read of the store, never the whole history; the
/// client accumulates pages until `next_cursor` is null. The byte bound is
/// 60 KiB, leaving room under the 64 KiB screen reply for the cursor and
/// envelope: a valid provider caption is at most 8 KiB UTF-8, and JSON
/// escaping of non-NUL control characters can grow it to ~48 KiB, so a
/// 32 KiB page would wrongly omit a legitimate post. Only an item larger
/// than a whole page — corrupt or historical, never provider-valid — is
/// reported partial and skipped without looping.
const SOURCE_PAGE_ITEMS: usize = 24;
const SOURCE_PAGE_BYTES: usize = 60 * 1024;
/// Receipts examined to fill one page. The cursor advances past every
/// examined row, so a scan that spends its budget on non-source legacy rows
/// still resumes exactly where it stopped instead of truncating the library.
const SOURCE_PAGE_SCAN: usize = 64;

/// SOC-55: what one of this context's retained receipt ids resolves to. The
/// page reports an unverifiable row as `partial`, the exact lookup answers
/// `item: null`, and a source-backed draft is refused — none of them invents
/// ownership or a post.
enum RetainedSource {
    /// This install's own `social.source.posts` read, retained by this context.
    Read(Value),
    /// A retained row of another kind (an upload, a caption): never a source.
    NotSource,
    /// Retained here, but the receipt is missing, corrupt or another install's.
    Unverifiable,
}

/// Which context link a retained receipt must prove.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceLink {
    Attachment,
    AttachmentOrFreshness,
}

/// The one post-id grammar the page and the exact lookup share: a retained
/// identity is opaque, non-empty and at most 100 bytes. It is never a URL,
/// path, pattern or escaped form, so control characters are legitimate
/// members of the exact identity.
fn source_post_id_valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100
}

fn source_lookup_valid(receipt_id: &str, post_id: &str) -> bool {
    crate::store::app_social_drafts::source_receipt_id_valid(receipt_id)
        && source_post_id_valid(post_id)
}
/// The receipt's handle, normalized exactly as the source settings store it.
/// A receipt without an attributable handle cannot name a source item.
fn source_handle(receipt: &Value) -> Option<String> {
    let handle = receipt["result"]["handle"]
        .as_str()?
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase();
    (!handle.is_empty()
        && handle.len() <= 30
        && handle.as_bytes()[0].is_ascii_alphanumeric()
        && handle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.')))
    .then_some(handle)
}

/// The one post `post_id` names inside a retained receipt. A repeated id is
/// ambiguous, so it fails closed rather than picking a copy.
fn source_receipt_post<'a>(receipt: &'a Value, post_id: &str) -> Option<&'a Value> {
    let mut found = receipt["result"]["posts"]
        .as_array()?
        .iter()
        .filter(|post| post["id"].as_str() == Some(post_id));
    let post = found.next()?;
    found.next().is_none().then_some(post)
}

fn https_uri(value: &str, max: usize) -> Option<ureq::http::Uri> {
    if value.is_empty() || value.len() > max {
        return None;
    }
    let uri: ureq::http::Uri = value.parse().ok()?;
    let authority = uri.authority()?;
    if authority.as_str().contains('@') || authority.as_str().contains(':') {
        return None;
    }
    (uri.scheme_str() == Some("https")).then_some(uri)
}

/// A source permalink: https on instagram.com only, as the frame's own link
/// policy admits it.
fn source_permalink(value: &str) -> Option<String> {
    let uri = https_uri(value, 512)?;
    matches!(
        uri.host()?.to_ascii_lowercase().as_str(),
        "instagram.com" | "www.instagram.com"
    )
    .then(|| value.to_owned())
}

/// A source thumbnail: https on the Instagram CDN hosts the frame CSP can
/// admit. It is an optional display hint, never source material.
fn source_thumb(value: &str) -> Option<String> {
    let uri = https_uri(value, 512)?;
    let host = uri.host()?.to_ascii_lowercase();
    (host.ends_with(".cdninstagram.com") || host.ends_with(".fbcdn.net")).then(|| value.to_owned())
}

/// One `{handle, post}` item in the screen's source shape. `None` means the
/// retained post cannot be attributed (no id, permalink or publish time), so
/// the page reports the material as partial instead of inventing a row.
fn source_post_item(
    handle: &str,
    receipt_id: &str,
    run_id: Option<&str>,
    post: &Value,
) -> Option<Value> {
    let id = post["id"].as_str().filter(|id| source_post_id_valid(id))?;
    let published = post["published_at_unix"].as_i64().or_else(|| {
        post["published_at"]
            .as_str()
            .and_then(crate::issue::time::parse_iso)
    })?;
    let permalink = source_permalink(post["permalink"].as_str()?)?;
    let media_kind = match post.get("media_kind") {
        None => "unknown",
        Some(Value::String(kind)) if !kind.is_empty() && kind.len() <= 32 => kind.as_str(),
        Some(_) => return None,
    };
    let caption = match post.get("caption") {
        None => "",
        Some(Value::String(caption)) => caption.as_str(),
        Some(_) => return None,
    };
    let thumb = post["preview_url"].as_str().and_then(source_thumb);
    let mut row = json!({
        "id": id,
        "caption": caption,
        "published_at": published,
        "permalink": permalink,
        "media_kind": media_kind,
        "receipt_id": receipt_id,
    });
    if let Some(url) = thumb {
        row["thumb_url"] = json!(url);
    }
    let mut item = json!({ "handle": handle, "post": row });
    if let Some(run_id) = run_id {
        item["run_id"] = json!(run_id);
    }
    Some(item)
}

/// The wire reply of one source page. `next_cursor` is the opaque position of
/// the next item (null at the end of the retained scan); `availability` is
/// `partial` only when retained material was found but could not be verified
/// or delivered — never merely because another page follows.
fn source_page(
    install: &str,
    context: &str,
    items: Vec<Value>,
    next: Option<SourceCursor>,
    partial: bool,
) -> Value {
    json!({
        "items": items,
        "next_cursor": next.map(|at| {
            crate::store::app_social_drafts::source_cursor_encode(install, context, &at)
        }),
        "availability": if partial { "partial" } else { "available" },
    })
}

/// Serialize one page only if the standalone snapshot is still the fence the
/// scan began with. The recheck closes the check/read gap: a concurrent source
/// attachment or freshness write bumps the revision and the page is refused as
/// stale instead of being returned under an old fence. The run watermark is a
/// separate immutable rowid bound and is not re-read.
fn source_page_checked(
    records: &RecordStore,
    install: &str,
    context: &str,
    fence: u64,
    items: Vec<Value>,
    next: Option<SourceCursor>,
    partial: bool,
) -> Result<Value> {
    if records.app_social_source_revision(context)? != fence {
        return Err(Error::invalid(
            "stale_cursor",
            "the retained source library changed during the scan — restart it",
        ));
    }
    Ok(source_page(install, context, items, next, partial))
}

/// Append one retained read's posts from `start` under the page bounds.
/// `Full` names the index the next page resumes at; `partial` records a post
/// that cannot be attributed or delivered under the bound.
enum Emit {
    Complete,
    Full(usize),
}

#[allow(clippy::too_many_arguments)]
fn emit_source_posts(
    items: &mut Vec<Value>,
    bytes: &mut usize,
    partial: &mut bool,
    handle: &str,
    receipt_id: &str,
    run_id: Option<&str>,
    posts: &[Value],
    start: usize,
) -> Result<Emit> {
    for (index, post) in posts.iter().enumerate().skip(start) {
        let item = source_post_item(handle, receipt_id, run_id, post);
        let Some(item) = item else {
            *partial = true;
            continue;
        };
        let size = serde_json::to_vec(&item)
            .map_err(|e| Error::internal(e.to_string()))?
            .len();
        if size + 3 > SOURCE_PAGE_BYTES {
            // Larger than a whole page: it cannot be delivered under the
            // bound, so it is reported partial rather than shipped unbounded
            // or dropped silently.
            *partial = true;
            continue;
        }
        if items.len() == SOURCE_PAGE_ITEMS || *bytes + size + 1 > SOURCE_PAGE_BYTES {
            return Ok(Emit::Full(index));
        }
        *bytes += size + 1;
        items.push(item);
    }
    Ok(Emit::Complete)
}

/// SOC-55: a retained source read from either provenance. A standalone tool
/// receipt names its own receipt; a run-bound read also names the run, so the
/// app can build the existing `run` draft source. `Unverifiable` is reported
/// as partial by the page and as `item: null` by the exact lookup.
enum SourceRead {
    Standalone(Value),
    Run { receipt: Value, run_id: String },
    NotSource,
    Unverifiable,
}

impl Shared {
    /// SOC-55: resolve one retained receipt id for the read verbs. A run-bound
    /// receipt is proven by the same run/context/recoverability checks a
    /// run-backed draft uses; nothing is inferred from an id's shape.
    fn retained_source_read(
        &self,
        records: &RecordStore,
        install: &str,
        context: &str,
        receipt_id: &str,
    ) -> Result<SourceRead> {
        match self.retained_source(
            records,
            install,
            context,
            receipt_id,
            SourceLink::AttachmentOrFreshness,
        )? {
            RetainedSource::Read(receipt) => Ok(SourceRead::Standalone(receipt)),
            RetainedSource::NotSource => Ok(SourceRead::NotSource),
            RetainedSource::Unverifiable => Ok(
                match self
                    .store
                    .app_source_run_receipt(install, context, receipt_id)
                {
                    Some((receipt, run_id)) => SourceRead::Run { receipt, run_id },
                    None => SourceRead::Unverifiable,
                },
            ),
        }
    }
}

fn source(params: &Value) -> Result<DraftSource> {
    let s = params
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::rejected("social draft source must be an object"))?;
    let kind = s.get("kind").and_then(Value::as_str).unwrap_or("");
    if s.keys()
        .any(|k| !matches!(k.as_str(), "kind" | "receipt_id" | "run_id" | "post_id"))
    {
        return Err(Error::rejected(
            "social draft source has unsupported fields",
        ));
    }
    let post_id = s.get("post_id").and_then(Value::as_str).map(str::to_owned);
    match kind {
        "tool_receipt"
            if s.get("receipt_id").and_then(Value::as_str).is_some()
                && !s.contains_key("run_id") =>
        {
            Ok(DraftSource::ToolReceipt {
                receipt_id: s["receipt_id"].as_str().unwrap().to_owned(),
                post_id,
            })
        }
        "run"
            if s.get("run_id").and_then(Value::as_str).is_some()
                && s.get("receipt_id").and_then(Value::as_str).is_some() =>
        {
            Ok(DraftSource::Run {
                run_id: s["run_id"].as_str().unwrap().to_owned(),
                receipt_id: s["receipt_id"].as_str().unwrap().to_owned(),
                post_id,
            })
        }
        "manual" if s.keys().all(|k| k == "kind") => Ok(DraftSource::Manual),
        _ => Err(Error::rejected(
            "social draft source must name one real receipt or run, or be manual",
        )),
    }
}

impl Shared {
    pub(super) fn social_draft_action(
        &self,
        params: &Value,
        peer_pid: u32,
        write: bool,
    ) -> Result<(ToolContext, String)> {
        self.social_draft_action_with_binding(params, peer_pid, write, BindingRequirement::Required)
    }

    fn social_draft_action_with_binding(
        &self,
        params: &Value,
        peer_pid: u32,
        write: bool,
        binding_requirement: BindingRequirement,
    ) -> Result<(ToolContext, String)> {
        self.operator_connection("social draft action", params, peer_pid)?;
        let o = origin(params)?;
        let (session, view) = self
            .tool_session(
                required_str(params, "token")?,
                params.get("key").and_then(Value::as_str).unwrap_or(""),
                o,
            )
            .ok_or_else(|| {
                Error::rejected("social draft action needs a live operator session — sign in again")
            })?;
        if o == Origin::Public
            && !view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator)
        {
            return Err(Error::rejected(
                "social draft action needs an operator-role public session",
            ));
        }
        let ctx = self.tool_context_proven(required_str(params, "action_token")?, &session)?;
        let context = ctx
            .context_id
            .clone()
            .ok_or_else(|| Error::rejected("social draft action needs a mounted active context"))?;
        let alias = required_str(params, "tool_alias")?;
        let slot = ctx.tools.get(alias).ok_or_else(|| {
            Error::rejected("draft action alias is not declared by the live screen")
        })?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, &ctx.install_id, |bundle, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if required_str(bundle, "digest")? != ctx.digest {
                return Err(Error::rejected("installation digest changed since mount"));
            }
            let status = self
                .store
                .app_capability_status(&ctx.install_id, &ctx.digest)?;
            if status["state"].as_str() != Some("approved") {
                return Err(Error::rejected(
                    "installation is not approved at its current digest",
                ));
            }
            if status["epoch"].as_i64() != Some(ctx.approval_epoch) {
                return Err(Error::rejected(
                    "installation was re-approved or revoked since mount — tool call refused",
                ));
            }
            let manifest = crate::issue::app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?;
            let declaration = manifest.capabilities.get(slot).ok_or_else(|| {
                Error::rejected("draft slot is not declared by this installation")
            })?;
            declaration.validate()?;
            if (write && declaration.effect != "draft")
                || (!write && !matches!(declaration.effect.as_str(), "read" | "draft"))
            {
                return Err(Error::rejected(
                    "social draft write requires a declared draft-effect slot",
                ));
            }
            if matches!(binding_requirement, BindingRequirement::Required) {
                let binding = self
                    .app_binding_live(&ctx.install_id, Some(&context), slot, bundle, files)?
                    .ok_or_else(|| Error::rejected("social draft capability binding is absent"))?;
                if binding.config["mapping"]["effect"] != declaration.effect {
                    return Err(Error::rejected(
                        "social draft binding effect differs from its declaration",
                    ));
                }
            }
            Ok(())
        })?;
        self.store.app_context_proof(&ctx.install_id, &context)?;
        Ok((ctx, context))
    }

    /// SOC-55: the one authority for "this receipt id is this context's own
    /// retained source read". The page, the exact lookup and a source-backed
    /// draft all resolve through it, so they cannot disagree about install,
    /// context or kind. `NotSource` is a verified non-source receipt; every
    /// case that cannot be proven is `Unverifiable`, never assumed.
    fn retained_source(
        &self,
        records: &RecordStore,
        install: &str,
        context: &str,
        receipt_id: &str,
        link: SourceLink,
    ) -> Result<RetainedSource> {
        let attached = records.app_social_tool_receipt_contains(context, receipt_id)?;
        let linked = attached
            || (link == SourceLink::AttachmentOrFreshness
                && records.app_social_freshness_contains(context, receipt_id)?);
        if !linked {
            return Ok(RetainedSource::Unverifiable);
        }
        match self.store.app_tool_result(receipt_id) {
            Ok(receipt) if receipt["install_id"].as_str() == Some(install) => {
                if receipt["result"]["kind"].as_str() == Some("social.source.posts") {
                    Ok(RetainedSource::Read(receipt))
                } else {
                    Ok(RetainedSource::NotSource)
                }
            }
            // Missing, corrupt (the store re-checks its digest) or foreign.
            Ok(_) | Err(_) => Ok(RetainedSource::Unverifiable),
        }
    }

    pub(super) fn verify_source(
        &self,
        install: &str,
        context: &str,
        source: &DraftSource,
        asset_id: Option<&str>,
    ) -> Result<()> {
        match source {
            DraftSource::ToolReceipt {
                receipt_id,
                post_id,
            } => {
                let records = RecordStore::open(&self.state_dir, install)?;
                // SOC-54/SOC-55: the library keeps posts an earlier fetch
                // proved, so any retained read attached to this context backs a
                // draft — not only the handle's newest one. The context link is
                // still the attachment table, exactly as before: the page may
                // show a legacy freshness-only read, but that never widens
                // draft authorization. The receipt must be this install's
                // `social.source.posts` read, its handle must still be a saved
                // source, and the selected post must be inside it.
                if !records.app_social_tool_receipt_contains(context, receipt_id)? {
                    return Err(Error::rejected(
                        "source receipt is not retained in this context",
                    ));
                }
                let receipt = match self.retained_source(
                    &records,
                    install,
                    context,
                    receipt_id,
                    SourceLink::Attachment,
                )? {
                    RetainedSource::Read(receipt) => receipt,
                    RetainedSource::NotSource => {
                        return Err(Error::rejected(
                            "source receipt is not retained for this installation",
                        ));
                    }
                    RetainedSource::Unverifiable => {
                        return Err(Error::rejected(
                            "source receipt is not retained in this context",
                        ));
                    }
                };
                let handle = source_handle(&receipt).unwrap_or_default();
                let saved = records.app_social_sources_show(context)?;
                if handle.is_empty()
                    || !saved["handles"].as_array().is_some_and(|handles| {
                        handles.iter().any(|h| h.as_str() == Some(handle.as_str()))
                    })
                {
                    // Removed from Settings after the post was fetched.
                    return Err(Error::invalid(
                        "source_not_saved",
                        "that profile is no longer a saved source",
                    ));
                }
                let post = post_id
                    .as_deref()
                    .ok_or_else(|| Error::rejected("selected social source needs a post ID"))?;
                if source_receipt_post(&receipt, post).is_none() {
                    return Err(Error::rejected(
                        "source receipt or post is not retained for this installation",
                    ));
                }
            }
            DraftSource::Manual => {}
            DraftSource::Run {
                run_id,
                receipt_id,
                post_id,
            } => {
                let post = post_id
                    .as_deref()
                    .ok_or_else(|| Error::rejected("selected social source needs a post ID"))?;
                let run = self.store.app_run_show(run_id)?;
                if run["install_id"] != install || run["context_id"] != context {
                    return Err(Error::rejected(
                        "source run does not belong to this installation and context",
                    ));
                }
                let _caption = self.store.app_selected_source_input(
                    install,
                    Some(context),
                    receipt_id,
                    post,
                )?;
                let receipt = self.store.app_capability_result(receipt_id)?;
                if receipt["run_id"].as_str() != Some(run_id.as_str()) {
                    return Err(Error::rejected(
                        "selected source receipt does not belong to the named run",
                    ));
                }
            }
        }
        if let Some(asset) = asset_id {
            let records = RecordStore::open(&self.state_dir, install)?;
            if !records.app_social_tool_receipt_contains(context, asset)? {
                return Err(Error::rejected(
                    "image receipt is not retained in this context",
                ));
            }
            let (header, digest, bytes) = self.store.app_tool_asset_bytes(install, asset)?;
            let mime =
                crate::platform::agenticos_external::image::uploaded_image_mime(&bytes, &header)
                    .map_err(|(_, message)| Error::rejected(message))?;
            if !matches!(mime, "image/jpeg" | "image/png")
                || bytes.len() > 2 * 1024 * 1024
                || crate::store::app_runs::artifact_digest(&bytes) != digest
            {
                return Err(Error::rejected(
                    "social draft attachment must be an intact owned JPEG or PNG",
                ));
            }
        }
        Ok(())
    }

    /// SOC-55: one bounded page of this context's retained source library.
    /// The scan is a keyset walk on `(completed_at DESC, receipt_id DESC)`,
    /// so concurrent new receipts land before the cursor and can never shift
    /// or skip an older row; equal timestamps tie-break on the receipt id.
    /// The cursor names the next item, including a position inside a receipt,
    /// so a page boundary never drops the rest of that receipt's posts.
    fn app_social_sources_page(
        &self,
        params: &Value,
        ctx: &ToolContext,
        context: &str,
        records: &RecordStore,
    ) -> Result<Value> {
        // Read the fence BEFORE any candidate query: a receipt attached after
        // this point bumps the revision, so the next page refuses rather than
        // letting a concurrent row join a scan that already began.
        let fence = records.app_social_source_revision(context)?;
        let run_fence = self.store.app_source_run_watermark()?;
        let cursor = match params.get("cursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                Some(crate::store::app_social_drafts::source_cursor_decode(
                    &ctx.install_id,
                    context,
                    raw,
                )?)
            }
            Some(_) => {
                return Err(Error::invalid(
                    "invalid_cursor",
                    "social source cursor is not valid for this scan",
                ))
            }
        };
        if cursor.as_ref().is_some_and(|at| at.revision != fence) {
            return Err(Error::invalid(
                "stale_cursor",
                "the retained source library changed during the scan — restart it",
            ));
        }
        // The run-bound snapshot is the cursor's rowid bound, not the current
        // one: a receipt inserted after the scan began is excluded on every
        // page, so equal/backdated timestamps cannot join it.
        let run_fence = cursor
            .as_ref()
            .and_then(|at| at.run_fence)
            .unwrap_or(run_fence);
        let mut items: Vec<Value> = Vec::new();
        let mut bytes = 2usize; // the encoded `[]`
        let mut partial = false;
        let mut next: Option<SourceCursor> = None;
        // The cursor's own receipt is always re-examined from its recorded
        // post index; the rest of the scan continues strictly after its key.
        let mut candidates = Vec::with_capacity(SOURCE_PAGE_SCAN + 2);
        if let Some(at) = &cursor {
            candidates.push(SourceCandidate {
                receipt_id: at.receipt_id.clone(),
                completed_at: at.completed_at,
            });
        }
        let after = cursor
            .as_ref()
            .map(|at| (at.completed_at, at.receipt_id.as_str()));
        let probe = SOURCE_PAGE_SCAN + 1;
        let mut rest = records.app_social_source_candidates(context, after, probe)?;
        rest.extend(records.app_social_freshness_candidates(context, after, probe)?);
        rest.extend(
            self.store
                .app_source_run_candidates(&ctx.install_id, context, after, run_fence, probe)?
                .into_iter()
                .map(|(receipt_id, created, _)| SourceCandidate {
                    receipt_id,
                    completed_at: created,
                }),
        );
        rest.sort_by(|a, b| {
            b.completed_at
                .total_cmp(&a.completed_at)
                .then_with(|| b.receipt_id.cmp(&a.receipt_id))
        });
        rest.dedup_by(|a, b| a.receipt_id == b.receipt_id);
        candidates.extend(rest.into_iter().take(probe));
        for (examined, candidate) in candidates.iter().enumerate() {
            if examined == SOURCE_PAGE_SCAN {
                // The probe row is the exact position the next page resumes
                // at: no older candidate is skipped by the scan budget.
                next = Some(SourceCursor {
                    revision: fence,
                    completed_at: candidate.completed_at,
                    receipt_id: candidate.receipt_id.clone(),
                    post: 0,
                    run_fence: Some(run_fence),
                });
                break;
            }
            let start = cursor
                .as_ref()
                .filter(|at| {
                    at.receipt_id == candidate.receipt_id
                        && at.completed_at == candidate.completed_at
                })
                .map_or(0, |at| at.post);
            let (receipt, run_id) = match self.retained_source_read(
                records,
                &ctx.install_id,
                context,
                &candidate.receipt_id,
            )? {
                SourceRead::Standalone(receipt) => (receipt, None),
                SourceRead::Run { receipt, run_id } => (receipt, Some(run_id)),
                SourceRead::NotSource => continue,
                SourceRead::Unverifiable => {
                    partial = true;
                    continue;
                }
            };
            let Some(handle) = source_handle(&receipt) else {
                partial = true;
                continue;
            };
            let Some(posts) = receipt["result"]["posts"].as_array() else {
                partial = true;
                continue;
            };
            match emit_source_posts(
                &mut items,
                &mut bytes,
                &mut partial,
                &handle,
                &candidate.receipt_id,
                run_id.as_deref(),
                posts,
                start,
            )? {
                Emit::Complete => {}
                Emit::Full(index) => {
                    next = Some(SourceCursor {
                        revision: fence,
                        completed_at: candidate.completed_at,
                        receipt_id: candidate.receipt_id.clone(),
                        post: index,
                        run_fence: Some(run_fence),
                    });
                    return source_page_checked(
                        records,
                        &ctx.install_id,
                        context,
                        fence,
                        items,
                        next,
                        partial,
                    );
                }
            }
        }
        source_page_checked(
            records,
            &ctx.install_id,
            context,
            fence,
            items,
            next,
            partial,
        )
    }

    /// SOC-55: one exact retained source item, so an older draft resolves its
    /// own receipt/post pair without paging the whole library. Missing,
    /// foreign, wrong-context, wrong-kind and mismatched-post receipts answer
    /// `item: null` — never another receipt's contents and never a substitute.
    fn app_social_sources_get(
        &self,
        params: &Value,
        ctx: &ToolContext,
        context: &str,
        records: &RecordStore,
    ) -> Result<Value> {
        let receipt_id = required_str(params, "receipt_id")?;
        let post_id = required_str(params, "post_id")?;
        if !source_lookup_valid(receipt_id, post_id) {
            return Err(Error::rejected(
                "social source lookup names no retained post",
            ));
        }
        let item = match self.retained_source_read(records, &ctx.install_id, context, receipt_id)? {
            SourceRead::Standalone(receipt) => {
                match (
                    source_handle(&receipt),
                    source_receipt_post(&receipt, post_id),
                ) {
                    (Some(handle), Some(post)) => source_post_item(&handle, receipt_id, None, post),
                    _ => None,
                }
            }
            SourceRead::Run { receipt, run_id } => {
                match (
                    source_handle(&receipt),
                    source_receipt_post(&receipt, post_id),
                ) {
                    (Some(handle), Some(post)) => {
                        source_post_item(&handle, receipt_id, Some(&run_id), post)
                    }
                    _ => None,
                }
            }
            SourceRead::NotSource | SourceRead::Unverifiable => None,
        };
        Ok(json!({ "item": item }))
    }

    /// Retain one operator-picked image as a context-scoped receipt. The
    /// board's multipart route stages the bytes under the shared upload dir;
    /// this RPC re-proves the live mount and draft slot, normalises the image
    /// (JPEG/PNG, 320 px floor, 4:5–1.91:1, max side 1440, EXIF dropped by
    /// re-encode, <=2 MiB), records the standalone receipt, and attaches it
    /// to the screen's context. The response carries a small display preview;
    /// the draft/publish paths read the retained bytes, never this preview.
    fn app_social_image_upload(
        &self,
        params: &Value,
        ctx: &ToolContext,
        context: &str,
        records: &RecordStore,
    ) -> Result<Value> {
        let alias = required_str(params, "tool_alias")?;
        let slot = ctx.tools.get(alias).ok_or_else(|| {
            Error::rejected("image upload alias is not declared by the live screen")
        })?;
        let tmp = required_str(params, "tmp")?;
        let root = self
            .state_dir
            .join(crate::wiki::UPLOAD_DIR)
            .canonicalize()
            .map_err(|_| Error::rejected("image upload staging is unavailable"))?;
        let path = std::path::PathBuf::from(tmp)
            .canonicalize()
            .map_err(|_| Error::rejected("uploaded image is no longer available"))?;
        if !path.starts_with(&root) || path.parent() != Some(root.as_path()) {
            return Err(Error::rejected(
                "uploaded image is outside the staging directory",
            ));
        }
        use std::io::Read as _;
        let file = std::fs::File::open(&path)
            .map_err(|_| Error::rejected("uploaded image is no longer available"))?;
        let meta = file
            .metadata()
            .map_err(|_| Error::rejected("uploaded image is no longer available"))?;
        if !meta.file_type().is_file()
            || meta.len() == 0
            || meta.len() > 2 * 1024 * 1024 + 64 * 1024
        {
            return Err(Error::rejected("uploaded image is outside the size bound"));
        }
        let mut raw = Vec::with_capacity(meta.len() as usize);
        file.take(2 * 1024 * 1024 + 64 * 1024 + 1)
            .read_to_end(&mut raw)
            .map_err(|_| Error::rejected("uploaded image could not be read"))?;
        if raw.len() as u64 > 2 * 1024 * 1024 + 64 * 1024 {
            return Err(Error::rejected("uploaded image is outside the size bound"));
        }
        let (bytes, mime, width, height) =
            crate::platform::agenticos_external::image::normalize_uploaded_image(&raw)
                .map_err(|(_, message)| Error::rejected(message))?;
        let digest = crate::store::app_runs::artifact_digest(&bytes);
        let raw_name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("image.jpg");
        let name: String = raw_name
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        let name = if name.trim().is_empty() {
            "image.jpg".to_owned()
        } else {
            name
        };
        let uuid = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("up-{uuid}");
        let request = format!("upload-{uuid}");
        let result = json!({
            "kind": "social.uploaded_image",
            "media_type": mime,
            "width": width,
            "height": height,
            "name": name,
            "digest": digest,
        });
        self.store
            .app_tool_upload_record(crate::store::app_tools::AppToolUpload {
                id: &id,
                request: &request,
                install: &ctx.install_id,
                alias,
                slot,
                result: &result,
                media_type: mime,
                bytes: &bytes,
            })?;
        records.app_social_tool_receipt_attach(
            context,
            &id,
            crate::store::now(),
            result["kind"].as_str().unwrap_or(""),
        )?;
        Ok(json!({
            "asset_id": id,
            "mime": mime,
            "width": width,
            "height": height,
            "size_bytes": bytes.len(),
            "digest": digest,
        }))
    }

    pub(super) fn rpc_app_social_draft(
        &self,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let allowed = match method {
            "app_social_draft_create" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "request_id",
                "caption",
                "source",
                "asset_id",
            ][..],
            "app_social_draft_list" => {
                &["action_token", "token", "key", "origin", "tool_alias"][..]
            }
            "app_social_draft_show" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "draft_id",
            ][..],
            "app_social_draft_update" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "request_id",
                "draft_id",
                "expected_revision",
                "caption",
                "asset_id",
            ][..],
            "app_social_draft_discard" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "draft_id",
                "revision",
            ][..],
            "app_social_draft_asset" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "draft_id",
            ][..],
            "app_social_image_upload" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "name",
                "tmp",
            ][..],
            "app_social_upload_asset" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "asset_id",
            ][..],
            "app_social_sources_show" => {
                &["action_token", "token", "key", "origin", "tool_alias"][..]
            }
            "app_social_sources_page" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "cursor",
            ][..],
            "app_social_sources_get" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "receipt_id",
                "post_id",
            ][..],
            "app_social_sources_save" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "request_id",
                "expected_revision",
                "handles",
            ][..],
            _ => return Err(Error::rejected("unknown social draft action")),
        };
        strict_fields(params, allowed)?;
        // This flag means the slot must declare draft effect, including for
        // asset reads; it is not merely an I/O-write classification.
        let write = local_social_draft_write(method)?;
        // The action context/session/mount and declared live capability are
        // proved before the RecordStore is opened. Local persistence does not
        // dispatch or bill; effect execution still requires its live binding.
        let (ctx, context) = self.social_draft_action_with_binding(
            params,
            peer_pid,
            write,
            BindingRequirement::LocalPersistence,
        )?;
        let _draft_release = if matches!(
            method,
            "app_social_draft_create"
                | "app_social_image_upload"
                | "app_social_draft_update"
                | "app_social_draft_discard"
                | "app_social_sources_save"
        ) {
            Some(
                self.app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
            )
        } else {
            None
        };
        let records = RecordStore::open(&self.state_dir, &ctx.install_id)?;
        match method {
            "app_social_draft_list" => {
                let mut result = records.app_social_draft_list(&context)?;
                let mut intents = records.app_social_generation_intents(&context)?;
                for intent in &mut intents {
                    let request = intent["request_id"].as_str().unwrap_or("").to_owned();
                    let state = intent["state"].as_str().unwrap_or("").to_owned();
                    let receipt = if matches!(state.as_str(), "pending" | "uncertain") {
                        self.store.app_tool_result_for_request(&request)?
                    } else if state == "completed" {
                        intent["receipt_id"]
                            .as_str()
                            .map(|id| self.store.app_tool_result(id))
                            .transpose()?
                    } else {
                        None
                    };
                    if let Some(receipt) = receipt {
                        records
                            .app_social_generation_validate_receipt(&context, &request, &receipt)?;
                        let id = required_str(&receipt, "id")?;
                        let completed = receipt["created_at"].as_f64().unwrap_or(0.0);
                        if matches!(state.as_str(), "pending" | "uncertain") {
                            records.app_social_tool_receipt_attach(
                                &context,
                                id,
                                completed,
                                receipt["result"]["kind"].as_str().unwrap_or(""),
                            )?;
                            records.app_social_generation_complete_request(&request, id)?;
                            intent["state"] = json!("completed");
                            intent["receipt_id"] = json!(id);
                        }
                        intent["receipt"] = receipt;
                    }
                }
                result["generation_intents"] = json!(intents);
                result["effects"] = records.app_social_effect_list(&context)?["effects"].clone();
                Ok(result)
            }
            "app_social_sources_show" => records.app_social_sources_show(&context),
            "app_social_sources_page" => {
                self.app_social_sources_page(params, &ctx, &context, &records)
            }
            "app_social_sources_get" => {
                self.app_social_sources_get(params, &ctx, &context, &records)
            }
            "app_social_sources_save" => {
                let handles = params
                    .get("handles")
                    .and_then(Value::as_array)
                    .ok_or_else(|| Error::rejected("social source handles must be an array"))?;
                let handles = handles
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| Error::rejected("social source handle must be a string"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                records.app_social_sources_save(
                    &context,
                    params
                        .get("expected_revision")
                        .and_then(Value::as_i64)
                        .ok_or_else(|| Error::rejected("expected_revision must be an integer"))?,
                    &handles,
                    required_str(params, "request_id")?,
                )
            }
            "app_social_draft_show" => {
                records.app_social_draft_show(&context, required_str(params, "draft_id")?)
            }
            "app_social_draft_asset" => {
                let draft =
                    records.app_social_draft_show(&context, required_str(params, "draft_id")?)?;
                let asset = draft["asset_id"]
                    .as_str()
                    .ok_or_else(|| Error::rejected("social draft has no attached image"))?;
                let (header, digest, bytes) =
                    self.store.app_tool_asset_bytes(&ctx.install_id, asset)?;
                let mime = crate::platform::agenticos_external::image::uploaded_image_mime(
                    &bytes, &header,
                )
                .map_err(|(_, message)| Error::rejected(message))?;
                if !matches!(mime, "image/jpeg" | "image/png") {
                    return Err(Error::rejected("publish image must be JPEG or PNG"));
                }
                if crate::store::app_runs::artifact_digest(&bytes) != digest {
                    return Err(Error::rejected("social image custody digest changed"));
                }
                Ok(
                    json!({"install_id":ctx.install_id,"draft_id":draft["draft_id"],"revision":draft["revision"],"asset_id":asset,"mime":mime,"digest":digest,"size_bytes":bytes.len()}),
                )
            }
            "app_social_image_upload" => {
                self.app_social_image_upload(params, &ctx, &context, &records)
            }
            "app_social_upload_asset" => {
                let asset = required_str(params, "asset_id")?;
                if !records.app_social_tool_receipt_contains(&context, asset)? {
                    return Err(Error::rejected(
                        "uploaded image is not retained in this context",
                    ));
                }
                let (header, digest, bytes) =
                    self.store.app_tool_asset_bytes(&ctx.install_id, asset)?;
                let mime = crate::platform::agenticos_external::image::uploaded_image_mime(
                    &bytes, &header,
                )
                .map_err(|(_, message)| Error::rejected(message))?;
                if !matches!(mime, "image/jpeg" | "image/png")
                    || bytes.len() > 2 * 1024 * 1024
                    || crate::store::app_runs::artifact_digest(&bytes) != digest
                {
                    return Err(Error::rejected(
                        "uploaded image must be an intact owned JPEG or PNG",
                    ));
                }
                Ok(json!({
                    "install_id": ctx.install_id,
                    "asset_id": asset,
                    "mime": mime,
                    "digest": digest,
                    "size_bytes": bytes.len(),
                }))
            }
            "app_social_draft_create" => {
                let source = source(params)?;
                let asset = params.get("asset_id").and_then(Value::as_str);
                self.verify_source(&ctx.install_id, &context, &source, asset)?;
                let actor = format!("session:{}", &ctx.session[..12]);
                records.app_social_draft_create(
                    &context,
                    required_str(params, "caption")?,
                    &source,
                    asset,
                    required_str(params, "request_id")?,
                    &actor,
                )
            }
            "app_social_draft_discard" => records.app_social_draft_discard(
                &context,
                required_str(params, "draft_id")?,
                params["revision"]
                    .as_i64()
                    .ok_or_else(|| Error::rejected("revision must be an integer"))?,
            ),
            "app_social_draft_update" => {
                let asset = match params.get("asset_id") {
                    None => None,
                    Some(Value::Null) => Some(None),
                    Some(Value::String(s)) => Some(Some(s.as_str())),
                    _ => return Err(Error::rejected("asset_id must be a string or null")),
                };
                let old =
                    records.app_social_draft_show(&context, required_str(params, "draft_id")?)?;
                let src: DraftSource = serde_json::from_value(old["source"].clone())
                    .map_err(|_| Error::rejected("social draft provenance is corrupt"))?;
                let new_asset = asset.unwrap_or(old["asset_id"].as_str());
                self.verify_source(&ctx.install_id, &context, &src, new_asset)?;
                let actor = format!("session:{}", &ctx.session[..12]);
                records.app_social_draft_update(
                    &context,
                    required_str(params, "draft_id")?,
                    &crate::store::app_social_drafts::SocialDraftEdit {
                        expected: params["expected_revision"].as_i64().ok_or_else(|| {
                            Error::rejected("expected_revision must be an integer")
                        })?,
                        caption: required_str(params, "caption")?,
                        asset_id: asset,
                        request_id: required_str(params, "request_id")?,
                        actor: &actor,
                    },
                )
            }
            _ => Err(Error::rejected("unknown social draft action")),
        }
    }
}

#[cfg(test)]
mod manual_source_tests {
    use super::source;
    use serde_json::json;

    #[test]
    fn manual_source_is_closed() {
        assert!(matches!(
            source(&json!({"source": {"kind": "manual"}})).unwrap(),
            crate::store::app_social_drafts::DraftSource::Manual
        ));
        for extra in ["post_id", "receipt_id", "run_id"] {
            let mut fields = serde_json::Map::new();
            fields.insert("kind".into(), json!("manual"));
            fields.insert(extra.into(), json!("x"));
            assert!(source(&json!({"source": fields})).is_err());
        }
        assert!(source(
            &json!({"source": {"kind": "tool_receipt", "receipt_id": "r", "post_id": "p"}})
        )
        .is_ok());
        assert!(source(
            &json!({"source": {"kind": "run", "run_id": "r", "receipt_id": "c", "post_id": "p"}})
        )
        .is_ok());
    }
}

#[cfg(test)]
mod source_page_fence_tests {
    use super::*;

    /// SOC-55: the page serializes only while the standalone candidate
    /// revision is still the fence the scan began with. A source attachment
    /// committed during materialization must turn the response into an
    /// explicit stale_cursor, including on a terminal page.
    #[test]
    fn soc55_a_page_is_refused_when_the_standalone_fence_moved() {
        let root = tempfile::tempdir().unwrap();
        let records = RecordStore::open(root.path(), "install-page").unwrap();
        let fence = records.app_social_source_revision("ctx-page").unwrap();
        assert!(
            source_page_checked(
                &records,
                "install-page",
                "ctx-page",
                fence,
                Vec::new(),
                None,
                false
            )
            .is_ok(),
            "an unchanged fence serializes the page"
        );
        records
            .app_social_tool_receipt_attach("ctx-page", "app-tool-race", 1.0, "social.source.posts")
            .unwrap();
        let stale = source_page_checked(
            &records,
            "install-page",
            "ctx-page",
            fence,
            Vec::new(),
            None,
            false,
        )
        .expect_err("a moved candidate set must refuse the page");
        assert_eq!(stale.code(), Some("stale_cursor"), "{stale}");
    }
}

#[cfg(test)]
mod source_page_size_tests {
    use super::*;

    /// SOC-55: a provider-valid 8 KiB caption made of non-NUL control
    /// characters escapes to ~48 KiB and must still be returned under the
    /// 60 KiB page bound with its exact receipt. Only an item that cannot fit
    /// a whole page is partial, and it is skipped without looping.
    #[test]
    fn soc55_worst_case_valid_caption_fits_and_only_oversize_is_partial() {
        let caption = "\u{1}".repeat(8 * 1024);
        let post = json!({
            "id": "post-1",
            "caption": caption,
            "published_at_unix": 1_790_467_200i64,
            "permalink": "https://www.instagram.com/p/AbCd123/",
            "media_kind": "image",
        });
        let mut items = Vec::new();
        let mut bytes = 2usize;
        let mut partial = false;
        let outcome = emit_source_posts(
            &mut items,
            &mut bytes,
            &mut partial,
            "brandhk",
            "app-tool-1",
            None,
            &[post],
            0,
        )
        .unwrap();
        assert!(matches!(outcome, Emit::Complete));
        assert!(!partial, "a valid worst-case caption is not partial");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["post"]["caption"], json!(caption));
        assert_eq!(items[0]["post"]["receipt_id"], "app-tool-1");
        assert!(bytes <= SOURCE_PAGE_BYTES, "the page stays under its bound");

        let oversize = json!({
            "id": "post-2",
            "caption": "x".repeat(SOURCE_PAGE_BYTES),
            "published_at_unix": 1_790_467_200i64,
            "permalink": "https://www.instagram.com/p/AbCd123/",
            "media_kind": "image",
        });
        let mut items = Vec::new();
        let mut bytes = 2usize;
        let mut partial = false;
        let outcome = emit_source_posts(
            &mut items,
            &mut bytes,
            &mut partial,
            "brandhk",
            "app-tool-1",
            None,
            &[oversize],
            0,
        )
        .unwrap();
        assert!(matches!(outcome, Emit::Complete));
        assert!(
            partial && items.is_empty(),
            "an unrepresentable item is partial, not emitted"
        );
    }
}

#[cfg(test)]
mod source_post_id_tests {
    use super::*;

    /// SOC-55: the page and the exact lookup share one opaque identity
    /// grammar. A control character is a legitimate member of a retained
    /// provider id, so the page emits it and `sources.get` must accept the
    /// same bounded string for exact membership — never a URL/path rule.
    #[test]
    fn soc55_page_and_get_share_the_exact_post_id_grammar() {
        let id = "post\u{1}id";
        assert!(source_post_id_valid(id));
        assert!(source_lookup_valid("app-tool-1", id));
        assert!(!source_post_id_valid(""));
        assert!(!source_post_id_valid(&"x".repeat(101)));
        assert!(source_post_id_valid(&"x".repeat(100)));
        let receipt = json!({
            "result": {
                "handle": "brandhk",
                "posts": [{
                    "id": id,
                    "caption": "Same",
                    "published_at_unix": 1_790_467_200i64,
                    "permalink": "https://www.instagram.com/p/AbCd123/",
                    "media_kind": "image",
                }],
            },
        });
        let post = source_receipt_post(&receipt, id)
            .expect("exact membership finds the control-character id");
        let item = source_post_item("brandhk", "app-tool-1", None, post)
            .expect("the page emits the same identity");
        assert_eq!(item["post"]["id"], json!(id));
        assert_eq!(item["post"]["receipt_id"], "app-tool-1");
    }
}
