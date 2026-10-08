//! CAD-1159 / spec finding M2: independently authored acceptance for the
//! per-task output budget on the REAL `emit` path toward AOS.
//! Body ownership: T2 acceptance author; implementers register only.
//!
//! AOS (agenticos-v2 PR349 @ 5bd81074, apps/api/src/runtime/
//! native-runtime-task-wire.ts `event()`, reached from executor-admission.ts
//! "task-event") accepts at most 65,536 decoded bytes and 4,096 parts per task,
//! and otherwise throws "native task stream unavailable". CAD must therefore
//! never emit beyond either limit and must end the output with
//! `TASK_OUTPUT_TRUNCATED_MARKER`, which itself fits inside the budget.
//!
//! Contract the implementer must provide in `native_task.rs` (names asserted):
//!   * `const TASK_OUTPUT_TRUNCATED_MARKER: &[u8]` - non-empty, <= 1024 bytes.
//!   * `StreamState::new()` - fresh per-task state (next = 0, not closed, no
//!     bytes counted). The budget state lives inside `StreamState`.
//!   * `emit(control, task, &Mutex<StreamState>, &Value)` keeps its signature
//!     and still returns Ok(()) once truncated (late data is dropped silently;
//!     the stream must not error, which would `_exit(125)` the worker).
use super::{emit, StreamState, TASK_OUTPUT_TRUNCATED_MARKER};
use crate::installer_bundle::constructor::private_wire::{self, Packet};
use base64::Engine;
use serde_json::{json, Value};
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const AOS_MAX_BYTES: usize = 65_536;
const AOS_MAX_PARTS: usize = 4_096;
/// Data budget: the marker's bytes and one part are reserved for it.
const DATA_MAX_PARTS: usize = AOS_MAX_PARTS - 1;
fn data_max_bytes() -> usize {
    AOS_MAX_BYTES - TASK_OUTPUT_TRUNCATED_MARKER.len()
}
const TASK: &str = "0123456789abcdef0123456789abcdef";

/// Emit every value through the production `emit` over a real datagram pair
/// and return the (part index, decoded bytes) packets the peer received.
fn run(values: &[Value]) -> Vec<(u64, Vec<u8>)> {
    let (control, peer) = UnixDatagram::pair().unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let reader = std::thread::spawn(move || {
        let mut got = Vec::new();
        loop {
            match private_wire::receive_child(&peer).unwrap() {
                Some(Packet::TaskEvent {
                    version: 1,
                    task,
                    part,
                    bytes,
                }) => {
                    assert_eq!(task, TASK);
                    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(bytes)
                        .unwrap();
                    got.push((part, raw));
                }
                Some(_) => panic!("unexpected packet"),
                None if flag.load(Ordering::SeqCst) => return got,
                None => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
    });
    let state = Mutex::new(StreamState::new());
    for value in values {
        emit(&control, TASK, &state, value).expect("emit must stay Ok at and past the budget");
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    done.store(true, Ordering::SeqCst);
    reader.join().unwrap()
}

fn total(parts: &[(u64, Vec<u8>)]) -> Vec<u8> {
    parts.iter().flat_map(|(_, b)| b.clone()).collect()
}

fn wire(values: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        out.extend(serde_json::to_vec(v).unwrap());
        out.push(b'\n');
    }
    out
}

fn assert_in_aos_budget(parts: &[(u64, Vec<u8>)]) {
    assert!(parts.len() <= AOS_MAX_PARTS, "parts {}", parts.len());
    assert!(
        total(parts).len() <= AOS_MAX_BYTES,
        "bytes {}",
        total(parts).len()
    );
    for (i, (part, _)) in parts.iter().enumerate() {
        assert_eq!(*part, i as u64, "parts stay consecutive from 0");
    }
}

/// One event whose newline-terminated wire form is exactly `len` bytes.
fn event_of_len(len: usize) -> Value {
    let overhead = serde_json::to_vec(&json!({"t": ""})).unwrap().len() + 1;
    json!({"t": "x".repeat(len - overhead)})
}

#[test]
fn marker_is_a_small_nonempty_constant() {
    assert!(!TASK_OUTPUT_TRUNCATED_MARKER.is_empty());
    assert!(TASK_OUTPUT_TRUNCATED_MARKER.len() <= 1024);
    assert!(TASK_OUTPUT_TRUNCATED_MARKER.starts_with(b"\n"));
    assert!(TASK_OUTPUT_TRUNCATED_MARKER.ends_with(b"\n"));
}

#[test]
fn a_task_under_budget_is_byte_for_byte_unchanged() {
    let values = vec![
        json!({"type":"opened","model":"a/b"}),
        event_of_len(40_000),
        json!({"type":"result","text":"héllo 😀"}),
    ];
    let parts = run(&values);
    let expected = wire(&values);
    assert_eq!(total(&parts), expected);
    // Chunking is per event (16 KiB parts), exactly as before the budget.
    let per_event: usize = values
        .iter()
        .map(|v| wire(std::slice::from_ref(v)).chunks(16_384).count())
        .sum();
    assert_eq!(parts.len(), per_event);
    assert_eq!(parts.len(), 5);
    assert!(!total(&parts)
        .windows(TASK_OUTPUT_TRUNCATED_MARKER.len())
        .any(|w| w == TASK_OUTPUT_TRUNCATED_MARKER));
    assert_in_aos_budget(&parts);
}

fn has_marker(parts: &[(u64, Vec<u8>)]) -> bool {
    total(parts).ends_with(TASK_OUTPUT_TRUNCATED_MARKER)
}

#[test]
fn exactly_the_data_budget_bytes_is_unchanged_without_marker() {
    let values = vec![event_of_len(data_max_bytes())];
    let parts = run(&values);
    assert_eq!(total(&parts), wire(&values));
    assert!(!has_marker(&parts));
    assert_in_aos_budget(&parts);
}

#[test]
fn one_byte_over_the_data_budget_is_truncated_with_the_marker() {
    let values = vec![event_of_len(data_max_bytes() + 1)];
    let parts = run(&values);
    assert_in_aos_budget(&parts);
    assert!(has_marker(&parts), "marker missing");
}

#[test]
fn exactly_the_data_budget_parts_is_unchanged_without_marker() {
    let values: Vec<Value> = (0..DATA_MAX_PARTS).map(|_| json!({})).collect();
    let parts = run(&values);
    assert_eq!(parts.len(), DATA_MAX_PARTS);
    assert_eq!(total(&parts), wire(&values));
    assert!(!has_marker(&parts));
}

#[test]
fn one_part_over_the_data_budget_is_truncated_with_the_marker_last() {
    let values: Vec<Value> = (0..AOS_MAX_PARTS).map(|_| json!({})).collect();
    let parts = run(&values);
    assert_in_aos_budget(&parts);
    assert!(parts
        .last()
        .unwrap()
        .1
        .ends_with(TASK_OUTPUT_TRUNCATED_MARKER));
}

#[test]
fn over_the_byte_budget_is_cut_and_ends_with_the_marker() {
    let values = vec![
        json!({"type":"opened"}),
        event_of_len(70_000),
        json!({"type":"result","text":"late"}),
    ];
    let parts = run(&values);
    let out = total(&parts);
    assert_in_aos_budget(&parts);
    assert!(
        out.ends_with(TASK_OUTPUT_TRUNCATED_MARKER),
        "marker missing"
    );
    // The cut keeps a real prefix of the original output.
    let marker = TASK_OUTPUT_TRUNCATED_MARKER.len();
    let original = wire(&values);
    assert_eq!(&out[..out.len() - marker], &original[..out.len() - marker]);
    assert!(
        !out.windows(4).any(|w| w == b"late"),
        "nothing after the cut"
    );
}

#[test]
fn data_after_truncation_is_dropped_without_error() {
    let (control, peer) = UnixDatagram::pair().unwrap();
    let state = Mutex::new(StreamState::new());
    emit(&control, TASK, &state, &event_of_len(80_000)).unwrap();
    while private_wire::receive_child(&peer).unwrap().is_some() {}
    emit(&control, TASK, &state, &json!({"type":"result"})).unwrap();
    assert!(private_wire::receive_child(&peer).unwrap().is_none());
}

#[test]
fn many_tiny_parts_over_4096_are_capped_with_the_marker_in_the_last() {
    let values: Vec<Value> = (0..AOS_MAX_PARTS + 904).map(|_| json!({})).collect();
    let parts = run(&values);
    assert_in_aos_budget(&parts);
    let last = &parts.last().unwrap().1;
    assert!(
        last.ends_with(TASK_OUTPUT_TRUNCATED_MARKER),
        "last part lacks marker"
    );
    assert!(total(&parts).ends_with(TASK_OUTPUT_TRUNCATED_MARKER));
}

#[test]
fn a_cut_never_splits_a_multibyte_codepoint() {
    // 4-byte emoji, shifted by 0..4 ASCII bytes so one shift lands the cut
    // mid-codepoint wherever the marker/budget boundary falls.
    for shift in 0..4 {
        let text = format!("{}{}", "a".repeat(shift), "😀".repeat(20_000));
        let values = vec![json!({"type":"result","text":text})];
        let parts = run(&values);
        let out = total(&parts);
        assert_in_aos_budget(&parts);
        assert!(out.ends_with(TASK_OUTPUT_TRUNCATED_MARKER), "shift {shift}");
        let body = &out[..out.len() - TASK_OUTPUT_TRUNCATED_MARKER.len()];
        assert!(
            std::str::from_utf8(body).is_ok(),
            "split codepoint, shift {shift}"
        );
    }
}
