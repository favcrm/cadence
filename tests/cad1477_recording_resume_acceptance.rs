use base64::Engine;
use cadence_agent::platform::AppCapabilityQuote;
use cadence_agent::store::app_recordings::{
    self, Consent, Failure, Job, Recording, Segment, Summary,
};
use cadence_agent::store::app_records::RecordStore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const INSTALL: &str = "install-resume";
const CONTEXT: &str = "ctx-resume";
const RECORD: &str = "rec-resume";
const OLD_BOOT: &str = "boot-earlier";
const NEW_BOOT: &str = "boot-current";

fn fixture() -> (tempfile::TempDir, RecordStore) {
    let root = tempfile::tempdir().unwrap();
    let records = RecordStore::open(root.path(), INSTALL).unwrap();
    records.recording_schema().unwrap();
    records
        .recording_create(
            CONTEXT,
            "create-resume",
            "create-digest",
            Recording {
                summary: Summary {
                    id: RECORD.into(),
                    title: None,
                    title_owner: None,
                    participants: Vec::new(),
                    state: "recording".into(),
                    created_ms: 0,
                    updated_ms: 0,
                    duration_ms: 0,
                    segment_count: 0,
                    has_minutes: false,
                },
                consent: Consent {
                    transcription_max_minor: 1_000,
                    consolidation_max_minor: 1_000,
                    currency: "USD".into(),
                },
                minutes: None,
                error: None,
                install_digest: "reviewed-bundle".into(),
                approval_epoch: 1,
                actor: json!({"kind": "operator"}),
                finish_sequence: None,
                final_job: None,
            },
        )
        .unwrap();
    (root, records)
}

fn queued_audio(records: &RecordStore) -> String {
    let data_bytes = 320_u32;
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
    records
        .recording_segment(
            CONTEXT,
            RECORD,
            Segment {
                sequence: 0,
                start_ms: 0,
                end_ms: 10,
                state: "queued".into(),
                text: None,
            },
            &digest,
            frames,
            &audio,
        )
        .unwrap()
        .1
}

fn reserve(records: &RecordStore, job: &mut Job) {
    let input: Value = if let Some(sequence) = job.sequence {
        let (audio, _) = records.recording_audio(CONTEXT, RECORD, sequence).unwrap();
        json!({"audio": {"format": "wav", "data": audio}})
    } else {
        job.input.clone().unwrap()
    };
    records
        .recording_reserve_job(
            CONTEXT,
            RECORD,
            job,
            json!({"digest": "original-binding"}),
            AppCapabilityQuote {
                schema: 1,
                currency: "USD".into(),
                unit_price_micros: 100,
                units: 1,
                total_price_micros: 100,
                price_revision: "original-rate".into(),
            },
            cadence_agent::store::app_runs::material_digest(&input),
        )
        .unwrap();
}

#[test]
fn earlier_boot_preparing_and_dispatched_jobs_resume_their_original_key() {
    for dispatched in [false, true] {
        let (root, records) = fixture();
        let key = queued_audio(&records);
        let mut original = records
            .recording_claim_job(CONTEXT, RECORD, &key, OLD_BOOT)
            .unwrap()
            .unwrap();
        if dispatched {
            reserve(&records, &mut original);
        }
        let before = records.recording_job(CONTEXT, RECORD, &key).unwrap();
        assert_eq!(before.boot, OLD_BOOT);
        assert_eq!(
            before.state,
            if dispatched {
                "dispatched"
            } else {
                "preparing"
            }
        );
        drop(records);

        let reopened = RecordStore::open(root.path(), INSTALL).unwrap();
        reopened.recording_schema().unwrap();
        assert_eq!(
            reopened
                .recording_stranded_jobs(CONTEXT, RECORD, NEW_BOOT)
                .unwrap(),
            vec![key.clone()],
        );
        let reclaimed = reopened
            .recording_claim_job(CONTEXT, RECORD, &key, NEW_BOOT)
            .unwrap()
            .unwrap();
        assert_eq!(reclaimed.key, key);
        assert_eq!(reclaimed.boot, NEW_BOOT);
        assert_eq!(reclaimed.state, before.state);
        assert_eq!(reclaimed.sequence, before.sequence);
        assert_eq!(reclaimed.reserved, before.reserved);
        assert_eq!(reclaimed.binding, before.binding);
        assert_eq!(reclaimed.input, before.input);
        assert_eq!(reclaimed.input_digest, before.input_digest);
        assert_eq!(
            serde_json::to_value(&reclaimed.quote).unwrap(),
            serde_json::to_value(&before.quote).unwrap(),
        );
        assert_eq!(reclaimed.uncertain_prior, dispatched);
        assert!(reopened
            .recording_stranded_jobs(CONTEXT, RECORD, NEW_BOOT)
            .unwrap()
            .is_empty());
        assert!(reopened
            .recording_claim_job(CONTEXT, RECORD, &key, NEW_BOOT)
            .unwrap()
            .is_none());
    }
}

#[test]
fn current_boot_dispatched_work_is_not_selected_or_claimed_twice() {
    let (_root, records) = fixture();
    let key = queued_audio(&records);
    let mut job = records
        .recording_claim_job(CONTEXT, RECORD, &key, NEW_BOOT)
        .unwrap()
        .unwrap();
    reserve(&records, &mut job);
    let before =
        serde_json::to_value(records.recording_job(CONTEXT, RECORD, &key).unwrap()).unwrap();
    assert!(records
        .recording_stranded_jobs(CONTEXT, RECORD, NEW_BOOT)
        .unwrap()
        .is_empty());
    assert!(records
        .recording_claim_job(CONTEXT, RECORD, &key, NEW_BOOT)
        .unwrap()
        .is_none());
    let after =
        serde_json::to_value(records.recording_job(CONTEXT, RECORD, &key).unwrap()).unwrap();
    assert_eq!(after, before);
}

#[test]
fn terminal_jobs_are_never_returned_or_reclaimed_by_a_later_boot() {
    for terminal in ["succeeded", "paid_unpublished", "invalid_output"] {
        let (root, records) = fixture();
        let audio_key = queued_audio(&records);
        let mut job = records
            .recording_claim_job(CONTEXT, RECORD, &audio_key, OLD_BOOT)
            .unwrap()
            .unwrap();
        reserve(&records, &mut job);
        if terminal == "paid_unpublished" {
            records
                .recording_retain_paid(
                    CONTEXT,
                    RECORD,
                    &job,
                    json!({"text": "Private paid transcript"}),
                    37,
                    Failure::new("unauthorized_scope", "Authority changed", false),
                )
                .unwrap();
        } else {
            records
                .recording_complete_job(
                    CONTEXT,
                    RECORD,
                    &job,
                    json!({"text": "Original source evidence."}),
                    37,
                    None,
                )
                .unwrap();
            if terminal == "invalid_output" {
                records
                    .recording_finish(CONTEXT, RECORD, "finish-resume", 0)
                    .unwrap();
                let record = records.recording_get(CONTEXT, RECORD).unwrap();
                let segments = records.recording_transcript(CONTEXT, RECORD).unwrap();
                let input = app_recordings::minutes_input(&record, &segments).unwrap();
                let text_key = records
                    .recording_begin_minutes(CONTEXT, RECORD, input)
                    .unwrap()
                    .unwrap();
                job = records
                    .recording_claim_job(CONTEXT, RECORD, &text_key, OLD_BOOT)
                    .unwrap()
                    .unwrap();
                reserve(&records, &mut job);
                records
                    .recording_retain_paid(
                        CONTEXT,
                        RECORD,
                        &job,
                        json!({"text": "Not structured minutes"}),
                        41,
                        Failure::new("invalid_minutes", "Invalid generated content", false),
                    )
                    .unwrap();
            }
        }
        let key = job.key.clone();
        let before =
            serde_json::to_value(records.recording_job(CONTEXT, RECORD, &key).unwrap()).unwrap();
        assert_eq!(before["state"], terminal);
        drop(records);
        let reopened = RecordStore::open(root.path(), INSTALL).unwrap();
        reopened.recording_schema().unwrap();
        assert!(reopened
            .recording_stranded_jobs(CONTEXT, RECORD, NEW_BOOT)
            .unwrap()
            .is_empty());
        assert!(reopened
            .recording_claim_job(CONTEXT, RECORD, &key, NEW_BOOT)
            .unwrap()
            .is_none());
        let after =
            serde_json::to_value(reopened.recording_job(CONTEXT, RECORD, &key).unwrap()).unwrap();
        assert_eq!(after, before);
    }
}

#[test]
fn recording_without_jobs_has_no_stranded_work() {
    let (root, records) = fixture();
    drop(records);
    let reopened = RecordStore::open(root.path(), INSTALL).unwrap();
    reopened.recording_schema().unwrap();
    assert!(reopened
        .recording_stranded_jobs(CONTEXT, RECORD, NEW_BOOT)
        .unwrap()
        .is_empty());
    assert_eq!(
        reopened
            .recording_get(CONTEXT, RECORD)
            .unwrap()
            .summary
            .segment_count,
        0
    );
}
