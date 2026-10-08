//! CAD-1203: independently authored acceptance for the terminal event of a
//! truncated native task, on the REAL `emit` path toward AOS.
//! Body ownership: T3 acceptance author; implementers register only.
//!
//! Contract (PM decision): besides the marker, room for ONE terminal event
//! (`{"type":"result",..}` or `{"type":"failed",..}`, the only terminal shapes
//! `emit`'s callers produce, native_task.rs ~136 and ~149; discriminator is the
//! `"type"` key) is reserved. Within the data budget (65,536 - marker -
//! `TERMINAL_EVENT_MAX` bytes, 4,094 parts) output is byte-for-byte unchanged.
//! After truncation data events are dropped and the FIRST terminal event is
//! emitted after the marker: unchanged if its line fits `TERMINAL_EVENT_MAX`,
//! else the minimal same-kind line (`result` keeps `status` only if its
//! serialized value is <= 64 bytes; `failed` carries only the type), each with
//! `"truncated":true`. Nothing follows the terminal line.
use super::{emit, StreamState, TASK_OUTPUT_TRUNCATED_MARKER};
use crate::installer_bundle::constructor::private_wire::{self, Packet};
use base64::Engine;
use serde_json::{json, Value};
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Must equal the implementation's `TERMINAL_EVENT_MAX` (4,096, PM-accepted).
/// Local so this module compiles before the constant exists; the boundary
/// tests below pin the value behaviourally.
const TERMINAL_EVENT_MAX: usize = 4_096;
const STATUS_MAX: usize = 64;
const AOS_MAX_BYTES: usize = 65_536;
const AOS_MAX_PARTS: usize = 4_096;
const TASK: &str = "0123456789abcdef0123456789abcdef";

fn marker() -> &'static [u8] {
    TASK_OUTPUT_TRUNCATED_MARKER
}
fn data_max_bytes() -> usize {
    AOS_MAX_BYTES - marker().len() - TERMINAL_EVENT_MAX
}

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

fn assert_bytes(got: &[u8], want: &[u8], what: &str) {
    if got != want {
        let show = |b: &[u8]| {
            let head = String::from_utf8_lossy(&b[..b.len().min(100)]).into_owned();
            format!("{} bytes, starts {head:?}", b.len())
        };
        panic!("{what}: got {} / want {}", show(got), show(want));
    }
}

fn line(v: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec(v).unwrap();
    out.push(b'\n');
    out
}

fn wire(values: &[Value]) -> Vec<u8> {
    values.iter().flat_map(line).collect()
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

/// A non-terminal event whose wire line is exactly `len` bytes.
fn data_of_len(len: usize) -> Value {
    let overhead = line(&json!({"type":"event","t":""})).len();
    json!({"type":"event","t":"x".repeat(len - overhead)})
}

/// A `result` whose wire line is exactly `len` bytes.
fn result_of_len(len: usize) -> Value {
    let overhead = line(&json!({"type":"result","text":""})).len();
    json!({"type":"result","text":"r".repeat(len - overhead)})
}

fn failed_of_len(len: usize) -> Value {
    let overhead = line(&json!({"type":"failed","error":""})).len();
    json!({"type":"failed","error":"f".repeat(len - overhead)})
}

/// Split a truncated stream into (data prefix, terminal line); the marker must
/// separate them and the terminal line is whatever follows the LAST marker.
fn split(parts: &[(u64, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
    let out = total(parts);
    let at = out
        .windows(marker().len())
        .rposition(|w| w == marker())
        .expect("marker missing");
    (out[..at].to_vec(), out[at + marker().len()..].to_vec())
}

fn assert_terminal(tail: &[u8], expected: &Value) {
    assert!(tail.ends_with(b"\n"), "terminal line ends with newline");
    assert_eq!(
        tail.iter().filter(|b| **b == b'\n').count(),
        1,
        "exactly one terminal line, nothing after it"
    );
    assert!(
        tail.len() <= TERMINAL_EVENT_MAX,
        "terminal fits the reserve"
    );
    assert_eq!(&serde_json::from_slice::<Value>(tail).unwrap(), expected);
}

/// Opening event then an oversized data event: truncates the stream.
fn over_budget() -> Vec<Value> {
    vec![
        json!({"type":"opened"}),
        data_of_len(data_max_bytes() + 10_000),
    ]
}

#[test]
fn a_task_under_budget_keeps_its_terminal_event_unchanged() {
    // (a) includes a result larger than TERMINAL_EVENT_MAX that still fits the
    // data budget: a task that never truncates is byte-for-byte unchanged.
    for terminal in [
        json!({"type":"result","status":"completed","text":"héllo 😀"}),
        result_of_len(TERMINAL_EVENT_MAX + 5_000),
        json!({"type":"failed","error":"boom"}),
    ] {
        let values = vec![json!({"type":"opened"}), data_of_len(20_000), terminal];
        let parts = run(&values);
        assert_bytes(&total(&parts), &wire(&values), "stream");
        assert!(!total(&parts).windows(marker().len()).any(|w| w == marker()));
        assert_in_aos_budget(&parts);
    }
}

#[test]
fn a_result_then_a_failed_without_truncation_both_go_out_unchanged() {
    let values = vec![
        json!({"type":"opened"}),
        json!({"type":"result","status":"completed","text":"ok"}),
        json!({"type":"failed","error":"late failure"}),
    ];
    let parts = run(&values);
    assert_bytes(&total(&parts), &wire(&values), "stream");
    assert_in_aos_budget(&parts);
}

#[test]
fn over_budget_then_a_small_result_is_marker_then_that_exact_line_then_nothing() {
    // (b)
    let terminal = json!({"type":"result","turn":"t1","status":"completed","text":"done"});
    let mut values = over_budget();
    values.push(terminal.clone());
    let parts = run(&values);
    assert_in_aos_budget(&parts);
    let (data, tail) = split(&parts);
    assert_bytes(&tail, &line(&terminal), "unchanged terminal line");
    let original = wire(&values);
    assert_bytes(&data, &original[..data.len()], "data is a real prefix");
    assert!(
        data.len() > 60_000,
        "the prefix keeps most of the data budget"
    );
    assert!(total(&parts).ends_with(&line(&terminal)));
}

#[test]
fn a_terminal_line_of_exactly_the_cap_is_unchanged_and_one_over_is_minimal() {
    for kind in ["result", "failed"] {
        let make = |len| {
            if kind == "result" {
                result_of_len(len)
            } else {
                failed_of_len(len)
            }
        };
        let mut values = over_budget();
        values.push(make(TERMINAL_EVENT_MAX));
        let parts = run(&values);
        assert_in_aos_budget(&parts);
        let (_, tail) = split(&parts);
        assert_bytes(
            &tail,
            &line(&make(TERMINAL_EVENT_MAX)),
            &format!("{kind} at cap"),
        );

        let mut values = over_budget();
        values.push(make(TERMINAL_EVENT_MAX + 1));
        let parts = run(&values);
        assert_in_aos_budget(&parts);
        let (_, tail) = split(&parts);
        assert_terminal(&tail, &json!({"type":kind,"truncated":true}));
    }
}

#[test]
fn over_budget_then_an_oversized_result_is_marker_then_the_minimal_result() {
    // (c): status kept when its serialized value is <= 64 bytes, else omitted.
    let status = json!("completed");
    let mut values = over_budget();
    values.push(json!({"type":"result","status":status,"text":"t".repeat(9_000),"error":null}));
    let (_, tail) = split(&run(&values));
    assert_terminal(
        &tail,
        &json!({"type":"result","status":"completed","truncated":true}),
    );

    let at_limit = "s".repeat(STATUS_MAX - 2); // serialized with quotes: 64 bytes
    let mut values = over_budget();
    values.push(json!({"type":"result","status":at_limit,"text":"t".repeat(9_000)}));
    let (_, tail) = split(&run(&values));
    assert_terminal(
        &tail,
        &json!({"type":"result","status":at_limit,"truncated":true}),
    );

    let too_long = "s".repeat(STATUS_MAX - 1); // 65 bytes serialized
    let mut values = over_budget();
    values.push(json!({"type":"result","status":too_long,"text":"t".repeat(9_000)}));
    let (_, tail) = split(&run(&values));
    assert_terminal(&tail, &json!({"type":"result","truncated":true}));

    let mut values = over_budget();
    values.push(json!({"type":"result","text":"t".repeat(9_000)}));
    let (_, tail) = split(&run(&values));
    assert_terminal(&tail, &json!({"type":"result","truncated":true}));
}

#[test]
fn data_terminal_data_leaves_only_marker_and_terminal() {
    // (d)
    let terminal = json!({"type":"result","status":"completed","text":"fin"});
    let mut values = over_budget();
    values.push(json!({"type":"event","t":"DROP-AFTER-CUT-1"}));
    values.push(terminal.clone());
    values.push(json!({"type":"event","t":"DROP-AFTER-TERMINAL-2"}));
    values.push(json!({"type":"request","id":"DROP-3"}));
    let parts = run(&values);
    assert_in_aos_budget(&parts);
    let out = total(&parts);
    let (data, tail) = split(&parts);
    assert_bytes(&tail, &line(&terminal), "terminal");
    assert!(
        out.ends_with(&line(&terminal)),
        "nothing after the terminal"
    );
    assert!(!out.windows(4).any(|w| w == b"DROP"), "dropped data absent");
    assert_bytes(&data, &wire(&values)[..data.len()], "data");
}

#[test]
fn a_failed_terminal_behaves_like_result() {
    // (e)
    let small = json!({"type":"failed","error":"boom"});
    let mut values = over_budget();
    values.push(json!({"type":"event","t":"DROP"}));
    values.push(small.clone());
    values.push(json!({"type":"event","t":"DROP"}));
    let parts = run(&values);
    assert_in_aos_budget(&parts);
    let (_, tail) = split(&parts);
    assert_bytes(&tail, &line(&small), "terminal");
    assert!(!total(&parts).windows(4).any(|w| w == b"DROP"));

    let mut values = over_budget();
    values.push(json!({"type":"failed","error":"e".repeat(9_000)}));
    let parts = run(&values);
    assert_in_aos_budget(&parts);
    let (_, tail) = split(&parts);
    assert_terminal(&tail, &json!({"type":"failed","truncated":true}));
}

#[test]
fn the_fallback_lines_fit_the_reserve_with_worst_case_status() {
    let worst = "s".repeat(STATUS_MAX - 2);
    let fallback = line(&json!({"type":"result","status":worst,"truncated":true}));
    assert!(
        fallback.len() <= TERMINAL_EVENT_MAX / 8,
        "fallback is minimal"
    );
    assert!(line(&json!({"type":"failed","truncated":true})).len() <= fallback.len());
}

#[test]
fn totals_and_parts_stay_within_the_hard_limits_in_every_case() {
    // (f) bytes bound: the largest data prefix plus marker plus a terminal
    // exactly at the cap must still fit 65,536 bytes.
    let mut values = vec![data_of_len(data_max_bytes() + 1)];
    values.push(result_of_len(TERMINAL_EVENT_MAX));
    let parts = run(&values);
    assert_in_aos_budget(&parts);
    let (data, tail) = split(&parts);
    assert_bytes(&tail, &line(&values[1]), "terminal");
    assert!(data.len() <= data_max_bytes());
    assert!(total(&parts).len() <= AOS_MAX_BYTES);

    // parts bound: tiny events past the data parts, then the terminal.
    for terminal in [
        json!({"type":"result","text":"x"}),
        json!({"type":"failed","error":"x"}),
    ] {
        let mut values: Vec<Value> = (0..AOS_MAX_PARTS + 500).map(|_| json!({})).collect();
        values.push(terminal.clone());
        let parts = run(&values);
        assert_in_aos_budget(&parts);
        assert_eq!(
            parts.len(),
            AOS_MAX_PARTS,
            "marker part + terminal part fill it"
        );
        let (_, tail) = split(&parts);
        assert_bytes(&tail, &line(&terminal), "terminal");
    }
}

#[test]
fn only_the_first_terminal_event_is_emitted_after_truncation() {
    // (g)
    let result = json!({"type":"result","status":"completed","text":"r"});
    let failed = json!({"type":"failed","error":"f"});
    for (first, second) in [(&result, &failed), (&failed, &result), (&result, &result)] {
        let mut values = over_budget();
        values.extend([first.clone(), second.clone(), first.clone()]);
        let parts = run(&values);
        assert_in_aos_budget(&parts);
        let (_, tail) = split(&parts);
        assert_bytes(&tail, &line(first), "terminal");
    }
    // A minimal (fallback) first terminal also closes the stream.
    let mut values = over_budget();
    values.push(result_of_len(TERMINAL_EVENT_MAX + 1));
    values.push(failed.clone());
    let parts = run(&values);
    let (_, tail) = split(&parts);
    assert_terminal(&tail, &json!({"type":"result","truncated":true}));
}

#[test]
fn the_data_part_budget_is_4094_so_one_more_tiny_event_truncates() {
    // 4,094 tiny data events are unchanged; the 4,095th is cut to the marker,
    // and the terminal event takes the last of the 4,096 parts.
    let tiny = |n: usize| (0..n).map(|_| json!({})).collect::<Vec<Value>>();
    let parts = run(&tiny(4_094));
    assert_eq!(parts.len(), 4_094);
    assert!(!total(&parts).windows(marker().len()).any(|w| w == marker()));

    let terminal = json!({"type":"result","text":"x"});
    let mut values = tiny(4_095);
    values.push(terminal.clone());
    let parts = run(&values);
    assert_eq!(parts.len(), 4_096);
    assert_eq!(parts[4_094].1, marker());
    assert_bytes(&parts[4_095].1, &line(&terminal), "terminal part");
}
