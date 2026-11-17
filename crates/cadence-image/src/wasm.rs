//! Binary ABI for Worker callers. Allocations are freed with exact lengths.
use crate::{prepare, Format, ImageError, PHOTO_INPUT_MAX};

const HEADER: usize = 20;

#[no_mangle]
pub extern "C" fn photo_alloc(length: usize) -> *mut u8 {
    if length == 0 || length > PHOTO_INPUT_MAX {
        return core::ptr::null_mut();
    }
    Box::into_raw(vec![0u8; length].into_boxed_slice()) as *mut u8
}

#[no_mangle]
pub unsafe extern "C" fn photo_free(pointer: *mut u8, length: usize) {
    if !pointer.is_null() && length > 0 && length <= PHOTO_INPUT_MAX {
        core::ptr::write_bytes(pointer, 0, length);
        drop(Box::from_raw(core::ptr::slice_from_raw_parts_mut(
            pointer, length,
        )));
    }
}

/// Returns `(response_length << 32) | response_pointer`. The response starts
/// with five little-endian u32 values: status, MIME, width, height, flags.
#[no_mangle]
pub unsafe extern "C" fn photo_prepare(pointer: *const u8, length: usize, format: u32) -> u64 {
    let kind = match format {
        1 => Some(Format::Jpeg),
        2 => Some(Format::Png),
        3 => Some(Format::Webp),
        _ => None,
    };
    let result = if pointer.is_null() || length == 0 || length > PHOTO_INPUT_MAX {
        Err(ImageError::Invalid)
    } else if let Some(kind) = kind {
        prepare(core::slice::from_raw_parts(pointer, length), kind)
    } else {
        Err(ImageError::Unsupported)
    };
    let (mut bytes, fields) = match result {
        Ok(image) => (
            image.bytes,
            [
                0,
                image.format as u32,
                image.width,
                image.height,
                u32::from(image.converted),
            ],
        ),
        Err(error) => (Vec::new(), [error as u32, 0, 0, 0, 0]),
    };
    let payload = bytes.len();
    bytes.resize(payload + HEADER, 0);
    bytes.copy_within(..payload, HEADER);
    for (index, value) in fields.iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len() as u64;
    let ptr = Box::into_raw(boxed) as *mut u8 as u64;
    (len << 32) | ptr
}
