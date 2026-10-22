//! Single-threaded Workers ABI. Buffer ownership crosses the seam explicitly.
use crate::command::{process, MAX_COMMAND_BYTES};

#[no_mangle]
pub extern "C" fn core_alloc(length: usize) -> *mut u8 {
    if length > MAX_COMMAND_BYTES {
        return std::ptr::null_mut();
    }
    Box::into_raw(vec![0_u8; length].into_boxed_slice()) as *mut u8
}

#[no_mangle]
pub unsafe extern "C" fn core_evaluate(ptr: *const u8, length: usize) -> u64 {
    let result = if ptr.is_null() || length > MAX_COMMAND_BYTES {
        serde_json::json!({"ok":false,"error":{"code":"invalid_command","message":"invalid input buffer"}})
    } else {
        process(std::slice::from_raw_parts(ptr, length))
    };
    let bytes = serde_json::to_vec(&result)
        .expect("domain result is JSON")
        .into_boxed_slice();
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
