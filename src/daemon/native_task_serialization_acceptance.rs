//! CAD-1203 D-9: independently authored allocation-bound acceptance on the
//! real `emit` / `minimal_terminal` path. This module owns only refusal checks;
//! production code must not add test-only seams to satisfy it.
use super::{emit, StreamState};
use serde_json::{json, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Generous fixed allowance for bounded serialization, framing and datagram
/// work. Input Values are constructed before the window starts.
const MAX_EMIT_ALLOCATED: usize = 128 * 1024;
const TASK: &str = "0123456789abcdef0123456789abcdef";

thread_local! {
    // Const initialization means consulting the counter from the allocator
    // does not itself allocate or recursively initialize TLS.
    static TRACKED_BYTES: Cell<*const AtomicUsize> = const { Cell::new(std::ptr::null()) };
}

struct CountingAllocator;

#[global_allocator]
static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

fn record_allocation(size: usize) {
    let _ = TRACKED_BYTES.try_with(|slot| {
        let counter = slot.get();
        if !counter.is_null() {
            // The measured window keeps its stack counter alive until it clears
            // this thread-local pointer, including while unwinding.
            unsafe { &*counter }.fetch_add(size, Ordering::Relaxed);
        }
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            record_allocation(new_size);
        }
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

struct AllocationWindow;

impl Drop for AllocationWindow {
    fn drop(&mut self) {
        TRACKED_BYTES.with(|slot| slot.set(std::ptr::null()));
    }
}

fn allocated_by(operation: impl FnOnce()) -> usize {
    let allocated = AtomicUsize::new(0);
    TRACKED_BYTES.with(|slot| slot.set(&allocated));
    let window = AllocationWindow;
    operation();
    drop(window);
    allocated.load(Ordering::Relaxed)
}

fn assert_bounded(label: &str, allocated: usize) {
    assert!(
        allocated <= MAX_EMIT_ALLOCATED,
        "{label} allocated {allocated} bytes during emit; limit is {MAX_EMIT_ALLOCATED}"
    );
}

#[test]
fn emit_bounds_serialization_and_skips_closed_done_or_dropped_data() {
    let (control, _peer) = UnixDatagram::pair().unwrap();
    // JSON escaping expands each input byte sixfold. Construct these values
    // outside the measured windows so the assertion measures production work,
    // not the test's deliberately large guest-derived input.
    let payload = "\u{0001}".repeat(1024 * 1024);
    let oversized_data: Value = json!({"type":"event","payload":payload.clone()});
    let oversized_status: Value = json!({"type":"result","status":payload});

    for (label, closed, done, truncated) in [
        ("closed stream", true, false, false),
        ("completed stream", false, true, false),
        ("truncated nonterminal", false, false, true),
    ] {
        let mut state = StreamState::new();
        state.closed = closed;
        state.done = done;
        state.truncated = truncated;
        let allocated = allocated_by(|| {
            emit(&control, TASK, &Mutex::new(state), &oversized_data).unwrap();
        });
        assert_bounded(label, allocated);
    }

    // Exercise the active, not-yet-truncated event path at its tightest data
    // budget: it must serialize only enough to distinguish overflow, not build
    // the complete guest event before emitting the marker.
    let mut state = StreamState::new();
    state.bytes = 65_536 - super::TASK_OUTPUT_TRUNCATED_MARKER.len() - 4_096;
    state.next = 4_094;
    let allocated = allocated_by(|| {
        emit(&control, TASK, &Mutex::new(state), &oversized_data).unwrap();
    });
    assert_bounded("oversized active event at exhausted data budget", allocated);

    let mut state = StreamState::new();
    state.truncated = true;
    let allocated = allocated_by(|| {
        emit(&control, TASK, &Mutex::new(state), &oversized_status).unwrap();
    });
    assert_bounded("oversized terminal status fallback", allocated);
}
