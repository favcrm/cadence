use cadence_image::{prepare, validate_native, Format};
use image::{ExtendedColorType, ImageEncoder};

#[test]
fn forged_animation_control_is_refused_by_both_image_entry_points() {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
        .expect("encode a valid one-pixel PNG");

    assert!(validate_native(&png, "image/png").is_ok());
    assert!(prepare(&png, Format::Png).is_ok());

    // An APNG acTL chunk must contain two four-byte integers. This one has
    // only one byte, but has a correct PNG chunk CRC. Put it after IHDR.
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&1u32.to_be_bytes());
    chunk.extend_from_slice(b"acTL");
    chunk.push(0);
    let crc = crc32fast::hash(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    png.splice(33..33, chunk);

    let native_refused = validate_native(&png, "image/png").is_err();
    let hosted_refused = prepare(&png, Format::Png).is_err();
    assert!(
        native_refused && hosted_refused,
        "malformed APNG control was accepted: native_refused={native_refused}, hosted_refused={hosted_refused}"
    );
}
