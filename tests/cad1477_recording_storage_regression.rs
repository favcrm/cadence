use base64::Engine;
use cadence_agent::platform::AppCapabilityQuote;
use cadence_agent::store::app_recordings::{self, Consent, Failure, Recording, Segment, Summary};
use cadence_agent::store::app_records::RecordStore;
use serde_json::json;
use sha2::{Digest, Sha256};

fn recording(id: &str) -> Recording {
    Recording {
        summary: Summary {
            id: id.into(),
            title: Some("Operator-owned title".into()),
            title_owner: Some("manual".into()),
            participants: vec!["Alex".into()],
            state: "recording".into(),
            created_ms: 0,
            updated_ms: 0,
            duration_ms: 0,
            segment_count: 0,
            has_minutes: false,
        },
        consent: Consent {
            transcription_max_minor: 1_000_000,
            consolidation_max_minor: 100,
            currency: "USD".into(),
        },
        minutes: None,
        error: None,
        install_digest: "reviewed-bundle".into(),
        approval_epoch: 1,
        actor: json!({"kind": "operator"}),
        finish_sequence: None,
        final_job: None,
    }
}

#[test]
fn one_hour_transcript_preserves_all_original_sources_once_and_in_order() {
    let segments: Vec<Segment> = (0..450_u32)
        .map(|sequence| {
            let discussion = format!(
                "Item {sequence:03}: décision café 東京. Alex agreed to review this exact item. "
            )
            .repeat(7);
            Segment {
                sequence,
                start_ms: u64::from(sequence) * 8_000,
                end_ms: u64::from(sequence + 1) * 8_000,
                state: "confirmed".into(),
                text: Some(format!(
                    "BEGIN-ORIGINAL-{sequence:03}\n{discussion}\nEND-ORIGINAL-{sequence:03}"
                )),
            }
        })
        .collect();
    let mut record = recording("rec-hour");
    record.summary.segment_count = segments.len() as u32;
    record.summary.duration_ms = segments.last().unwrap().end_ms;
    record.finish_sequence = Some(449);
    assert_eq!(
        record.summary.duration_ms,
        app_recordings::MAX_SECONDS * 1_000
    );
    assert!(record.summary.segment_count <= app_recordings::MAX_SEGMENTS);

    let input = app_recordings::minutes_input(&record, &segments)
        .unwrap()
        .expect("nonempty confirmed speech must produce a consolidation request");
    let tokens = input["maxOutputTokens"].as_u64().unwrap();
    assert!((1..=8192).contains(&tokens));
    let messages = input["messages"].as_array().unwrap();
    assert!(!messages.is_empty());
    assert!(messages.len() <= 100);
    let mut joined = String::new();
    for message in messages {
        assert_eq!(message.as_object().unwrap().len(), 2);
        let role = message["role"].as_str().unwrap();
        assert!(matches!(role, "system" | "user"));
        let content = message["content"].as_str().unwrap();
        assert!(content.len() <= 64_000);
        if role == "user" {
            joined.push_str(content);
        }
    }
    let mut previous_end = 0;
    for segment in &segments {
        let original = segment.text.as_deref().unwrap();
        assert_eq!(
            joined.matches(original).count(),
            1,
            "source {} must survive complete and exactly once",
            segment.sequence,
        );
        let offset = joined.find(original).unwrap();
        assert!(
            offset >= previous_end,
            "source {} must follow the preceding original source",
            segment.sequence,
        );
        previous_end = offset + original.len();
    }
}

#[test]
fn paid_unpublished_audio_retains_receipt_and_source_across_reopen_without_retry() {
    let root = tempfile::tempdir().unwrap();
    let install = "install-regression";
    let context = "ctx-regression";
    let id = "rec-retained";
    let records = RecordStore::open(root.path(), install).unwrap();
    records.recording_schema().unwrap();
    records
        .recording_create(context, "create-regression", "create-digest", recording(id))
        .unwrap();

    let data_bytes = 128_000_u32 * 2;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&32_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    wav.resize(44 + data_bytes as usize, 0);
    let audio = base64::engine::general_purpose::STANDARD.encode(&wav);
    let digest = format!("{:x}", Sha256::digest(&wav));
    let frames = app_recordings::validate_audio(&audio, &digest).unwrap();
    assert_eq!(frames, 128_000);
    let (_, key) = records
        .recording_segment(
            context,
            id,
            Segment {
                sequence: 0,
                start_ms: 0,
                end_ms: 8_000,
                state: "queued".into(),
                text: None,
            },
            &digest,
            frames,
            &audio,
        )
        .unwrap();
    let mut job = records
        .recording_claim_job(context, id, &key, "boot-first")
        .unwrap()
        .unwrap();
    let quote = AppCapabilityQuote {
        schema: 1,
        currency: "USD".into(),
        unit_price_micros: 100,
        units: 1,
        total_price_micros: 100,
        price_revision: "reviewed-rate".into(),
    };
    let quote_value = serde_json::to_value(&quote).unwrap();
    let input = json!({"audio": {"format": "wav", "data": audio}});
    records
        .recording_reserve_job(
            context,
            id,
            &mut job,
            json!({"digest": "frozen-binding"}),
            quote,
            cadence_agent::store::app_runs::material_digest(&input),
        )
        .unwrap();
    let receipt = json!({
        "text": "Paid response must remain private",
        "providerReceipt": "receipt-one",
    });
    records
        .recording_retain_paid(
            context,
            id,
            &job,
            receipt.clone(),
            37,
            Failure::new(
                "unauthorized_scope",
                "Authority changed before publication",
                false,
            ),
        )
        .unwrap();
    assert!(records
        .recording_complete_job(context, id, &job, receipt.clone(), 37, None)
        .is_err());
    drop(records);

    let reopened = RecordStore::open(root.path(), install).unwrap();
    reopened.recording_schema().unwrap();
    let stored = reopened.recording_job(context, id, &key).unwrap();
    assert_eq!(stored.key, key);
    assert_eq!(stored.state, "paid_unpublished");
    assert_eq!(stored.reserved, 37);
    assert_eq!(stored.result, Some(receipt));
    assert_eq!(serde_json::to_value(stored.quote).unwrap(), quote_value);
    let (retained_audio, retained_frames) = reopened.recording_audio(context, id, 0).unwrap();
    assert_eq!(retained_audio, audio);
    assert_eq!(retained_frames, frames);
    let source = reopened.recording_transcript(context, id).unwrap();
    assert_eq!(source.len(), 1);
    assert_eq!(source[0].state, "failed");
    assert!(source[0].text.is_none());
    let record = reopened.recording_get(context, id).unwrap();
    assert_eq!(record.summary.state, "failed");
    assert_eq!(
        record.summary.title.as_deref(),
        Some("Operator-owned title")
    );
    assert_eq!(record.summary.title_owner.as_deref(), Some("manual"));
    assert!(!record.summary.has_minutes);
    assert!(record.minutes.is_none());
    assert!(!record.error.unwrap().retryable);
    assert!(reopened
        .recording_claim_job(context, id, &key, "boot-restarted")
        .unwrap()
        .is_none());
    assert!(reopened
        .recording_retry(context, id, "retry-regression")
        .is_err());
    let unchanged = reopened.recording_job(context, id, &key).unwrap();
    assert_eq!(unchanged.state, "paid_unpublished");
    assert_eq!(unchanged.reserved, 37);
    assert_eq!(unchanged.result, stored.result);
}
