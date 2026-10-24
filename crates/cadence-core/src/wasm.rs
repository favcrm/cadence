//! The only wasm ABI: single-threaded, buffer ownership crosses the seam
//! explicitly. The caller frees both buffers with their exact lengths.
use crate::evaluate::{evaluate, refusal_bytes, Code, MAX_INPUT_BYTES};

#[no_mangle]
pub extern "C" fn core_alloc(length: usize) -> *mut u8 {
    if length > MAX_INPUT_BYTES {
        return std::ptr::null_mut();
    }
    Box::into_raw(vec![0_u8; length].into_boxed_slice()) as *mut u8
}

/// Returns `(length << 32) | ptr` of the JSON response.
#[no_mangle]
pub unsafe extern "C" fn core_evaluate(ptr: *const u8, length: usize) -> u64 {
    let output = if ptr.is_null() || length > MAX_INPUT_BYTES {
        refusal_bytes(Code::InvalidCommand, "invalid input buffer")
    } else {
        evaluate(std::slice::from_raw_parts(ptr, length))
    };
    let bytes = output.into_boxed_slice();
    let length = bytes.len() as u64;
    let ptr = Box::into_raw(bytes) as *mut u8 as u64;
    (length << 32) | ptr
}

#[no_mangle]
pub unsafe extern "C" fn core_free(ptr: *mut u8, length: usize) {
    if !ptr.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            ptr, length,
        )));
    }
}
