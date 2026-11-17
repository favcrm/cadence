use std::io::Write;

use cadence_image::{prepare, validate_native, Format};
use flate2::{write::ZlibEncoder, Compression};
use image::{ExtendedColorType, ImageEncoder};

const PROFILE: &[u8] = include_bytes!("../src/display-p3.icc");

fn zlib(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).expect("write compressed profile");
    encoder.finish().expect("finish compressed profile")
}

fn png_with_iccp(compressed: &[u8]) -> Vec<u8> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
        .expect("encode control PNG");

    // iCCP: profile name, NUL, compression method 0, zlib payload.
    let mut payload = b"Display P3\0\0".to_vec();
    payload.extend_from_slice(compressed);
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"iCCP");
    chunk.extend_from_slice(&payload);
    let crc = crc32fast::hash(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    // Insert after the eight-byte PNG signature and 25-byte IHDR.
    png.splice(33..33, chunk);
    png
}

fn assert_refused(label: &str, compressed: &[u8]) {
    let png = png_with_iccp(compressed);
    let native_refused = validate_native(&png, "image/png").is_err();
    let hosted_refused = prepare(&png, Format::Png).is_err();
    assert!(
        native_refused && hosted_refused,
        "{label}: native_refused={native_refused}, hosted_refused={hosted_refused}"
    );
}

#[test]
fn malformed_or_over_limit_icc_profiles_are_refused() {
    let valid_stream = zlib(PROFILE);
    let valid = png_with_iccp(&valid_stream);
    assert!(
        validate_native(&valid, "image/png").is_ok(),
        "native valid control"
    );
    assert!(prepare(&valid, Format::Png).is_ok(), "hosted valid control");

    let mut truncated = valid_stream.clone();
    truncated.truncate(truncated.len() - 1);
    assert_refused("truncated zlib stream", &truncated);

    let invalid_icc = zlib(b"not an ICC profile");
    assert_refused("invalid ICC structure", &invalid_icc);

    let mut extra = valid_stream;
    extra.extend_from_slice(b"extra");
    assert_refused("bytes after zlib stream", &extra);

    let oversized = zlib(&vec![0u8; 256 * 1024 + 1]);
    assert_refused("ICC expansion exceeds 256 KiB", &oversized);
}
