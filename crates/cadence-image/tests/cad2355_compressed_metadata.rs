use cadence_image::{prepare, validate_native, Format};
use image::{ExtendedColorType, ImageEncoder};

fn assert_metadata_removed(bytes: &[u8], payload: &[u8], entry_point: &str) {
    assert!(
        !bytes.windows(4).any(|window| window == b"zTXt"),
        "{entry_point} returned the malformed zTXt chunk"
    );
    assert!(
        !bytes.windows(payload.len()).any(|window| window == payload),
        "{entry_point} returned the malformed compressed payload"
    );
}

#[test]
fn malformed_compressed_metadata_never_reaches_the_model() {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
        .expect("encode a benign one-pixel PNG");

    // A normal image without ancillary metadata works through both real
    // public entry points, so a blanket refusal cannot satisfy this check.
    assert!(validate_native(&png, "image/png").is_ok());
    assert!(prepare(&png, Format::Png).is_ok());

    // zTXt consists of a keyword, NUL, compression method and a zlib
    // stream. 0xff is not a valid zlib stream. The outer PNG chunk CRC is
    // correct, so a CRC-only check cannot detect the malformed payload.
    let payload = [b'x', 0, 0, 0xff];
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"zTXt");
    chunk.extend_from_slice(&payload);
    let crc = crc32fast::hash(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    // The encoder emits the standard eight-byte signature and 25-byte IHDR.
    png.splice(33..33, chunk);

    // Either refusal or a sanitized returned image is acceptable. Native
    // consumers must use these returned bytes rather than the original.
    if let Ok(native_bytes) = validate_native(&png, "image/png") {
        assert_metadata_removed(native_bytes.as_ref(), &payload, "native guard");
    }
    if let Ok(prepared) = prepare(&png, Format::Png) {
        assert_metadata_removed(&prepared.bytes, &payload, "hosted preparation");
    }
}
