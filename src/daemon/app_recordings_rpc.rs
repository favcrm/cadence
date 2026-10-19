//! Native recording service: operator-only, installation/context scoped, no agent team.
//! Durable work claims and cumulative budgets precede provider I/O outside locks.
use super::app_capabilities_rpc::CapabilityQuoteRequest;
use super::*;
use crate::issue::app::{Manifest, RecordingNeed};
use crate::issue::app_catalog::workspace;
use crate::store::app_recordings::{
    self as recordings, Consent, Failure, Job, Recording, Segment, Summary,
};
use crate::store::app_records::RecordStore;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::LazyLock;

struct Prepared {
    request: CapabilityQuoteRequest,
    binding: Value,
    slot: String,
    digest: String,
    epoch: i64,
}
fn boot() -> &'static str {
    static BOOT: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().simple().to_string());
    BOOT.as_str()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create<'a> {
    request_id: &'a str,
    title: Option<String>,
    #[serde(default)]
    participants: Vec<String>,
    consent: Consent,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload<'a> {
    sequence: u32,
    digest: &'a str,
    start_ms: u64,
    end_ms: u64,
    format: &'a str,
    audio_base64: &'a str,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish<'a> {
    request_id: &'a str,
    last_sequence: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Retry<'a> {
    request_id: &'a str,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rename<'a> {
    request_id: &'a str,
    title: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    limit: Option<u32>,
    cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Detail {
    segments_after: Option<i64>,
    segments_limit: Option<u32>,
}
fn body<'a, T: Deserialize<'a>>(params: &'a Value) -> Result<T> {
    T::deserialize(&params["body"]).map_err(|_| {
        Error::rejected("invalid_request: recording request has unsupported fields or types")
    })
}
fn request_id(value: &str) -> Result<()> {
    crate::proto::identifier(value, "recording request ID").map(|_| ())
}
fn title(value: Option<String>) -> Result<Option<String>> {
    let value = value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if value
        .as_ref()
        .is_some_and(|value| value.len() > 200 || value.chars().any(char::is_control))
    {
        return Err(Error::rejected(
            "invalid_request: title must be one bounded line",
        ));
    }
    Ok(value)
}
fn open(shared: &Shared, install: &str) -> Result<RecordStore> {
    let records = RecordStore::open(&shared.state_dir, install)?;
    records.recording_schema()?;
    Ok(records)
}
fn unavailable(error: &Error) -> String {
    let text = error.to_string();
    if text.contains("no_connection") {
        "No reviewed speech/text service connection is available. Connect the platform service and retry.".into()
    } else if text.contains("unpriced") {
        "A platform speech or minutes price is not published. An operator must publish the default service and its reviewed rate.".into()
    } else {
        "The reviewed recording services could not be verified. Check the platform connection, published default and prices, then retry.".into()
    }
}

impl Shared {
    fn recording_scope<R>(
        &self,
        install: &str,
        context: &str,
        action: impl FnOnce(
            &Value,
            &BTreeMap<String, String>,
            &Manifest,
            &RecordingNeed,
            i64,
        ) -> Result<R>,
    ) -> Result<R> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(context, "context ID")?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.store.app_context_proof(install, context)?;
            let status = self
                .store
                .app_capability_status(install, required_str(bundle, "digest")?)?;
            if status["state"] != "approved" {
                return Err(Error::rejected(
                    "unauthorized_scope: recording installation is not approved",
                ));
            }
            let epoch = status["epoch"].as_i64().ok_or_else(|| {
                Error::rejected("unauthorized_scope: approval receipt is unavailable")
            })?;
            let manifest =
                crate::issue::app::parse_manifest(files.get("app.md").ok_or_else(|| {
                    Error::rejected("unauthorized_scope: installation manifest unavailable")
                })?)?;
            let declaration = manifest.recording.as_ref().ok_or_else(|| {
                Error::rejected("unauthorized_scope: installation does not declare recording")
            })?;
            action(bundle, files, &manifest, declaration, epoch)
        })
    }

    fn recording_actor_live(&self, actor: &Value) -> Result<()> {
        self.account_image_actor_live(actor)?;
        if actor["session"]
            .as_str()
            .is_some_and(|hash| self.public_row_hash(hash, false) == Some(false))
        {
            return Err(Error::rejected(
                "unauthorized_scope: the recording operator is no longer authorized",
            ));
        }
        Ok(())
    }

    fn recording_prepare(
        &self,
        install: &str,
        context: &str,
        kind: &str,
        actor: &Value,
    ) -> Result<Prepared> {
        self.recording_actor_live(actor)?;
        self.recording_scope(install, context, |bundle, files, _, declaration, epoch| {
            let slot = if kind == "audio" {
                &declaration.transcription
            } else {
                &declaration.consolidation
            };
            let proof = match self.app_binding_live(install, Some(context), slot, bundle, files)? {
                Some(proof) => proof,
                None => {
                    if self
                        .store
                        .app_binding_slot_recorded(install, Some(context), slot)?
                    {
                        return Err(Error::rejected(
                            "no_connection: recording slot is revoked or unbound",
                        ));
                    }
                    // Exactly one eligible reviewed connection may supply the
                    // default. Never override a saved choice or tombstone.
                    let mut selected = None;
                    for connection in self.connection_list_locked()? {
                        let Some(id) = connection["id"].as_str() else {
                            continue;
                        };
                        if let Ok(config) =
                            self.app_binding_config(install, Some(context), slot, id, bundle, files)
                        {
                            if config["provider"] == crate::platform::account_media::PLATFORM {
                                let source =
                                    serde_json::from_value(config["account_source"].clone())
                                        .map_err(|_| {
                                            Error::rejected(
                                                "no_connection: account source is invalid",
                                            )
                                        })?;
                                if !Self::account_image_owned(actor, &source) {
                                    continue;
                                }
                            }
                            if selected.is_some() {
                                return Err(Error::rejected(
                                    "no_connection: choose the service connection for this app",
                                ));
                            }
                            selected = Some(config);
                        }
                    }
                    let config = selected.ok_or_else(|| {
                        Error::rejected("no_connection: no reviewed recording service connection")
                    })?;
                    self.store.app_binding_create(
                        install,
                        Some(context),
                        slot,
                        &config,
                        &format!("recording-bind-{}", uuid::Uuid::new_v4().simple()),
                    )?;
                    self.store
                        .app_binding_for_slot(
                            install,
                            Some(context),
                            slot,
                            required_str(bundle, "digest")?,
                        )?
                        .ok_or_else(|| {
                            Error::rejected("no_connection: recording service binding unavailable")
                        })?
                }
            };
            self.app_account_binding_owned(actor, &proof)?;
            let request = self.prepare_app_capability_quote(&proof)?;
            Ok(Prepared {
                request,
                binding: serde_json::to_value(proof)?,
                slot: slot.clone(),
                digest: required_str(bundle, "digest")?.into(),
                epoch,
            })
        })
    }

    fn recording_prices(
        &self,
        install: &str,
        context: &str,
        actor: &Value,
    ) -> Result<(Prepared, Consent)> {
        let speech = self.recording_prepare(install, context, "audio", actor)?;
        let text = self.recording_prepare(install, context, "text", actor)?;
        let speech_price = speech.request.fetch()?;
        let text_price = text.request.fetch()?;
        if speech.digest != text.digest || speech.epoch != text.epoch {
            return Err(Error::rejected(
                "quote_required: recording approval changed during discovery",
            ));
        }
        let total = speech_price
            .unit_price_micros
            .checked_mul(recordings::MAX_SECONDS)
            .and_then(|total| total.checked_add(u64::from(recordings::MAX_SEGMENTS)))
            .filter(|total| *total <= 1_000_000_000)
            .ok_or_else(|| {
                Error::rejected("quote_required: recording budget exceeds the supported bound")
            })?;
        Ok((
            speech,
            Consent {
                transcription_max_minor: total,
                consolidation_max_minor: text_price.total_price_micros,
                currency: "USD".into(),
            },
        ))
    }

    pub(super) fn rpc_app_recording(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app recording management", params, peer_pid)?;
        let fields = params.as_object().ok_or_else(|| {
            Error::rejected("invalid_request: recording envelope must be an object")
        })?;
        if fields.keys().any(|field| {
            !matches!(
                field.as_str(),
                "install_id" | "context_id" | "recording_id" | "body" | "token" | "key" | "origin"
            )
        }) {
            return Err(Error::rejected(
                "invalid_request: recording envelope has unsupported authority fields",
            ));
        }
        if params.get("recording_id").is_some()
            && matches!(
                method,
                "app_recording_quote" | "app_recording_list" | "app_recording_create"
            )
        {
            return Err(Error::rejected(
                "invalid_request: this recording operation has no record selector",
            ));
        }
        let install = required_str(params, "install_id")?;
        let context = required_str(params, "context_id")?;
        let actor = self.account_image_actor(params)?;
        // The scope is proven before opening any installation file or even
        // inspecting an idempotency receipt.
        self.recording_scope(install, context, |_, _, _, _, _| Ok(()))?;
        match method {
            "app_recording_quote" => {
                if !params["body"]
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty)
                {
                    return Err(Error::rejected(
                        "invalid_request: quote has no caller-controlled inputs",
                    ));
                }
                match self.recording_prices(install, context, &actor) {
                    Ok((_, consent)) => Ok(
                        json!({"transcription_max_minor":consent.transcription_max_minor,"consolidation_max_minor":consent.consolidation_max_minor,"currency":"USD","unavailable_reason":null}),
                    ),
                    Err(error) => Ok(
                        json!({"transcription_max_minor":null,"consolidation_max_minor":null,"currency":"USD","unavailable_reason":unavailable(&error)}),
                    ),
                }
            }
            "app_recording_list" => {
                let list: List = body(params)?;
                let limit = list.limit.unwrap_or(20);
                if !(1..=100).contains(&limit)
                    || list
                        .cursor
                        .as_ref()
                        .is_some_and(|cursor| cursor.len() > 200)
                {
                    return Err(Error::rejected(
                        "invalid_request: recording pagination exceeds its bound",
                    ));
                }
                self.recording_scope(install, context, |_, _, _, _, _| {
                    open(self, install)?.recording_list(context, list.cursor.as_deref(), limit)
                })
            }
            "app_recording_create" => {
                let create: Create<'_> = body(params)?;
                request_id(create.request_id)?;
                let digest = crate::store::app_runs::material_digest(&params["body"]);
                let prior = self.recording_scope(install, context, |_, _, _, _, _| {
                    open(self, install)?.recording_created(context, create.request_id, &digest)
                })?;
                if let Some(prior) = prior {
                    return self.recording_reply(install, context, &prior.summary.id, -1, 50);
                }
                let title = title(create.title)?;
                if create.participants.len() > 32
                    || create.participants.iter().any(|person| {
                        person.trim().is_empty()
                            || person.len() > 120
                            || person.chars().any(char::is_control)
                    })
                {
                    return Err(Error::rejected(
                        "invalid_request: participants must be bounded names",
                    ));
                }
                let (prepared, quote) = self.recording_prices(install, context, &actor)?;
                if create.consent.currency != "USD"
                    || create.consent.transcription_max_minor != quote.transcription_max_minor
                    || create.consent.consolidation_max_minor != quote.consolidation_max_minor
                {
                    return Err(Error::rejected("quote_required: recording price changed; refresh availability before recording"));
                }
                let id = format!("rec_{}", uuid::Uuid::new_v4().simple());
                let record = Recording {
                    summary: Summary {
                        id,
                        title_owner: title.as_ref().map(|_| "manual".into()),
                        title,
                        participants: create.participants,
                        state: "recording".into(),
                        created_ms: 0,
                        updated_ms: 0,
                        duration_ms: 0,
                        segment_count: 0,
                        has_minutes: false,
                    },
                    consent: create.consent,
                    minutes: None,
                    error: None,
                    install_digest: prepared.digest.clone(),
                    approval_epoch: prepared.epoch,
                    actor,
                    finish_sequence: None,
                    final_job: None,
                };
                let created =
                    self.recording_scope(install, context, |bundle, _, _, _, epoch| {
                        if bundle["digest"] != prepared.digest || epoch != prepared.epoch {
                            return Err(Error::rejected(
                                "unauthorized_scope: recording approval changed",
                            ));
                        }
                        open(self, install)?.recording_create(
                            context,
                            create.request_id,
                            &digest,
                            record,
                        )
                    })?;
                self.recording_reply(install, context, &created.summary.id, -1, 50)
            }
            "app_recording_detail" => {
                let detail: Detail = body(params)?;
                let after = detail.segments_after.unwrap_or(-1);
                let limit = detail.segments_limit.unwrap_or(50);
                if after < -1
                    || after >= i64::from(recordings::MAX_SEGMENTS)
                    || !(1..=50).contains(&limit)
                {
                    return Err(Error::rejected(
                        "invalid_request: transcript pagination exceeds its bound",
                    ));
                }
                let id = required_str(params, "recording_id")?;
                let reply = self.recording_reply(install, context, id, after, limit)?;
                // Reopening a saved meeting continues work a stopped daemon
                // left unfinished, under the SAME job keys. The worker
                // rechecks the authority before it spends anything, so this
                // never starts a replacement paid operation.
                self.resume_interrupted_recording(install, context, id);
                Ok(reply)
            }
            "app_recording_segment" => {
                let upload: Upload<'_> = body(params)?;
                if upload.format != "wav16k-mono-pcm16" {
                    return Err(Error::rejected(
                        "unsupported_audio_format: recording needs PCM WAV",
                    ));
                }
                let frames = recordings::validate_audio(upload.audio_base64, upload.digest)?;
                let id = required_str(params, "recording_id")?;
                let (segment, key) = self.recording_scope(install, context, |_, _, _, _, _| {
                    let records = open(self, install)?;
                    records.recording_refresh_actor(context, id, &actor)?;
                    records.recording_segment(
                        context,
                        id,
                        Segment {
                            sequence: upload.sequence,
                            start_ms: upload.start_ms,
                            end_ms: upload.end_ms,
                            state: "queued".into(),
                            text: None,
                        },
                        upload.digest,
                        frames,
                        upload.audio_base64,
                    )
                })?;
                self.spawn_recording_job(install, context, id, &key);
                Ok(json!({"segment":segment}))
            }
            "app_recording_finish" => {
                let finish: Finish<'_> = body(params)?;
                request_id(finish.request_id)?;
                let id = required_str(params, "recording_id")?;
                self.recording_scope(install, context, |_, _, _, _, _| {
                    let records = open(self, install)?;
                    records.recording_refresh_actor(context, id, &actor)?;
                    records.recording_finish(context, id, finish.request_id, finish.last_sequence)
                })?;
                self.schedule_recording_minutes(install, context, id)?;
                self.recording_reply(install, context, id, -1, 50)
            }
            "app_recording_retry" => {
                let retry: Retry<'_> = body(params)?;
                request_id(retry.request_id)?;
                let id = required_str(params, "recording_id")?;
                let keys = self.recording_scope(install, context, |_, _, _, _, _| {
                    let records = open(self, install)?;
                    records.recording_refresh_actor(context, id, &actor)?;
                    records.recording_retry(context, id, retry.request_id)
                })?;
                for key in keys {
                    self.spawn_recording_job(install, context, id, &key);
                }
                self.schedule_recording_minutes(install, context, id)?;
                self.recording_reply(install, context, id, -1, 50)
            }
            "app_recording_rename" => {
                let rename: Rename<'_> = body(params)?;
                request_id(rename.request_id)?;
                let id = required_str(params, "recording_id")?;
                let title = title(rename.title)?;
                self.recording_scope(install, context, |_, _, _, _, _| {
                    open(self, install)?.recording_rename(context, id, rename.request_id, title)
                })?;
                self.recording_reply(install, context, id, -1, 50)
            }
            _ => Err(Error::rejected("unknown recording operation")),
        }
    }

    fn recording_reply(
        &self,
        install: &str,
        context: &str,
        id: &str,
        after: i64,
        limit: u32,
    ) -> Result<Value> {
        crate::proto::identifier(id, "recording ID")?;
        self.recording_scope(install, context, |_, _, _, _, _| {
            Ok(json!({"recording":open(self,install)?.recording_detail(context,id,after,limit)?}))
        })
    }

    fn schedule_recording_minutes(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        id: &str,
    ) -> Result<()> {
        let key=self.recording_scope(install,context,|_,_,_,_,_| {
            let records=open(self,install)?;let record=records.recording_get(context,id)?;
            if record.finish_sequence.is_none() || record.summary.state=="ready" {return Ok(None);}
            let segments=records.recording_transcript(context,id)?;
            if segments.iter().any(|segment|segment.state!="confirmed") {return Ok(None);}
            let input=match recordings::minutes_input(&record,&segments) {
                Ok(input)=>input,
                Err(_)=>{records.recording_set_error(context,id,Failure::new("too_large","The full transcript is saved but exceeds the minutes service input limit.",false))?;return Ok(None);}
            };
            records.recording_begin_minutes(context,id,input)
        })?;
        if let Some(key) = key {
            self.spawn_recording_job(install, context, id, &key);
        }
        Ok(())
    }

    /// Continue recording work a stopped daemon left unfinished. Every
    /// persisted job of an EARLIER boot is re-claimed under its own key, so
    /// the paid operation is the same one, never a replacement. A record
    /// whose final minutes job was never created is scheduled here too.
    fn resume_interrupted_recording(self: &Arc<Self>, install: &str, context: &str, id: &str) {
        let records = match open(self, install) {
            Ok(records) => records,
            Err(_) => return,
        };
        let record = match records.recording_get(context, id) {
            Ok(record) => record,
            Err(_) => return,
        };
        if record.summary.state == "ready" {
            return;
        }
        if record.error.as_ref().is_some_and(|error| !error.retryable) {
            return;
        }
        let stranded = match records.recording_stranded_jobs(context, id, boot()) {
            Ok(stranded) => stranded,
            Err(_) => return,
        };
        for key in stranded {
            self.spawn_recording_job(install, context, id, &key);
        }
        if record.final_job.is_none() && record.finish_sequence.is_some() {
            let _ = self.schedule_recording_minutes(install, context, id);
        }
    }

    fn spawn_recording_job(self: &Arc<Self>, install: &str, context: &str, id: &str, key: &str) {
        let shared = Arc::clone(self);
        let install = install.to_owned();
        let context = context.to_owned();
        let id = id.to_owned();
        let key = key.to_owned();
        std::thread::spawn(move || {
            let records = match open(&shared, &install) {
                Ok(records) => records,
                Err(_) => return,
            };
            let mut job = match records.recording_claim_job(&context, &id, &key, boot()) {
                Ok(Some(job)) => job,
                _ => return,
            };
            let outcome = shared.run_recording_job(&records, &install, &context, &id, &mut job);
            if let Err((refused, error)) = outcome {
                let _ = records.recording_fail_job(&context, &id, &key, boot(), refused, error);
            } else if job.kind == "audio" {
                let _ = shared.schedule_recording_minutes(&install, &context, &id);
            }
        });
    }

    fn run_recording_job(
        self: &Arc<Self>,
        records: &RecordStore,
        install: &str,
        context: &str,
        id: &str,
        job: &mut Job,
    ) -> std::result::Result<(), (bool, Failure)> {
        let uncertain_prior = job.uncertain_prior;
        let prework = || {
            (!uncertain_prior,Failure::new("provider_unavailable","The recording service is unavailable. Source audio/transcript is saved; retry after checking the connection and published prices.",true))
        };
        let unknown = || {
            (false,Failure::new("uncertain_outcome","The provider outcome is uncertain. Retry this saved recording to reconcile the same operation, never a replacement paid call.",true))
        };
        let record = records.recording_get(context, id).map_err(|_| prework())?;
        let prepared = self
            .recording_prepare(install, context, &job.kind, &record.actor)
            .map_err(|_| prework())?;
        if prepared.digest != record.install_digest || prepared.epoch != record.approval_epoch {
            return Err((
                !uncertain_prior,
                Failure::new(
                    "unauthorized_scope",
                    "The installation approval changed. Saved recording data is retained.",
                    false,
                ),
            ));
        }
        let input = if let Some(sequence) = job.sequence {
            let (audio, frames) = records
                .recording_audio(context, id, sequence)
                .map_err(|_| unknown())?;
            let mut input = json!({"audio":{"format":"wav","data":null}});
            input["audio"]["data"] = Value::String(audio);
            if job.quote.is_none() {
                let base = prepared.request.fetch().map_err(|_| prework())?;
                let quote = recordings::audio_quote(&base, frames).map_err(|_| prework())?;
                let digest = crate::store::app_runs::material_digest(&input);
                self.recording_scope(install, context, |_, _, _, _, _| {
                    records.recording_reserve_job(
                        context,
                        id,
                        job,
                        prepared.binding.clone(),
                        quote,
                        digest,
                    )
                })
                .map_err(|_| prework())?;
            }
            input
        } else {
            if job.quote.is_none() {
                let quote = prepared.request.fetch().map_err(|_| prework())?;
                let digest = crate::store::app_runs::material_digest(
                    job.input.as_ref().ok_or_else(prework)?,
                );
                self.recording_scope(install, context, |_, _, _, _, _| {
                    records.recording_reserve_job(
                        context,
                        id,
                        job,
                        prepared.binding.clone(),
                        quote,
                        digest,
                    )
                })
                .map_err(|_| prework())?;
            }
            job.input.take().ok_or_else(prework)?
        };
        let current = self
            .recording_prepare(install, context, &job.kind, &record.actor)
            .map_err(|_| prework())?;
        if current.digest != record.install_digest
            || current.epoch != record.approval_epoch
            || job.binding.as_ref() != Some(&current.binding)
            || job.input_digest.as_deref()
                != Some(crate::store::app_runs::material_digest(&input).as_str())
        {
            return Err(prework());
        }
        self.account_image_actor_live(&record.actor)
            .map_err(|_| prework())?;
        let authority = json!({"schema":1,"install_id":install,"context_id":context,"slot":current.slot,"binding":current.binding,"quote":job.quote,"input_digest":job.input_digest,"source":{"kind":"recording","recording_id":id,"job_key":job.key},"call_id":job.key,"invocation_id":job.key});
        let mut output=current.request.execute(&authority,&input,&job.key).map_err(|error|match error {
            crate::platform::AppCapabilityError::Refused(_)=>prework(),
            crate::platform::AppCapabilityError::Released(_)=>(true,Failure::new("provider_rejected","The provider refused this operation and released its hold. Saved source can be retried.",true)),
            _=>unknown(),
        })?;
        let charge = output
            .result
            .as_object_mut()
            .and_then(|result| result.remove("_charge_minor"))
            .and_then(|charge| charge.as_u64())
            .ok_or_else(unknown)?;
        let generated = if job.kind == "text" {
            let source = records
                .recording_transcript(context, id)
                .map_err(|_| unknown())?;
            let text = output.result["text"].as_str().ok_or_else(unknown)?;
            match recordings::validate_minutes(text, &source) {
                Ok(minutes) => Some(minutes),
                Err(_) => {
                    records.recording_retain_paid(context,id,job,output.result,charge,Failure::new("invalid_minutes","The minutes response could not be validated. The full transcript and paid receipt are retained; no extra generation was started.",false)).map_err(|_|unknown())?;
                    return Ok(());
                }
            }
        } else {
            None
        };
        // The receipt belongs to already admitted work; publishing its content
        // still requires fresh membership and the exact current local fences.
        let mut result = Some(output.result);
        let mut generated = generated;
        let publish = self.recording_actor_live(&record.actor).and_then(|()| {
            self.recording_scope(install, context, |bundle, files, _, _, epoch| {
                if bundle["digest"] != record.install_digest || epoch != record.approval_epoch {
                    return Err(Error::rejected(
                        "recording approval changed before retention",
                    ));
                }
                let binding = self
                    .app_binding_live(install, Some(context), &current.slot, bundle, files)?
                    .ok_or_else(|| {
                        Error::rejected("recording binding was revoked before retention")
                    })?;
                self.app_account_binding_owned(&record.actor, &binding)?;
                self.recheck_app_capability_quote(
                    &binding,
                    job.quote
                        .as_ref()
                        .ok_or_else(|| Error::rejected("recording paid quote unavailable"))?,
                )?;
                if job.binding.as_ref() != Some(&serde_json::to_value(binding)?) {
                    return Err(Error::rejected(
                        "recording binding changed before retention",
                    ));
                }
                self.account_image_actor_live(&record.actor)?;
                records.recording_complete_job(
                    context,
                    id,
                    job,
                    result.take().expect("unpublished paid response"),
                    charge,
                    generated.take(),
                )
            })
        });
        match publish {
            Ok(())=>Ok(()),
            Err(_) if result.is_some()=>records.recording_retain_paid(context,id,job,result.take().expect("retained paid response"),charge,Failure::new("unauthorized_scope","The paid response is retained, but recording authority changed. Source was not replaced or published; no new paid attempt was started.",false))
                .map_err(|_|unknown()),
            Err(_)=>Err(unknown()),
        }
    }
}
