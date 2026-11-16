use cadence_image::{prepare, validate_native, Format};
use image::ExtendedColorType;

const PROFILE: &[u8] = include_bytes!("../src/display-p3.icc");
const ICC_LIMIT: usize = 256 * 1024;
const MAX_ICC_CHUNK: usize = 65_519;

fn ordinary_jpeg() -> Vec<u8> {
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
        .expect("encode control JPEG");
    jpeg
}

fn jpeg_with_icc(profile: &[u8]) -> Vec<u8> {
    assert!(!profile.is_empty());
    let count = profile.len().div_ceil(MAX_ICC_CHUNK);
    assert!(count <= u8::MAX as usize);
    let mut markers = Vec::new();
    for (index, part) in profile.chunks(MAX_ICC_CHUNK).enumerate() {
        let length = u16::try_from(2 + 14 + part.len()).expect("APP2 segment length");
        markers.extend_from_slice(&[0xff, 0xe2]);
        markers.extend_from_slice(&length.to_be_bytes());
        markers.extend_from_slice(b"ICC_PROFILE\0");
        markers.push((index + 1) as u8);
        markers.push(count as u8);
        markers.extend_from_slice(part);
    }
    let mut jpeg = ordinary_jpeg();
    assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    jpeg.splice(2..2, markers);
    jpeg
}

fn assert_accepted(label: &str, jpeg: &[u8]) {
    let native_accepted = validate_native(jpeg, "image/jpeg").is_ok();
    let hosted_accepted = prepare(jpeg, Format::Jpeg).is_ok();
    assert!(
        native_accepted && hosted_accepted,
        "{label}: native_accepted={native_accepted}, hosted_accepted={hosted_accepted}"
    );
}

fn assert_refused(label: &str, jpeg: &[u8]) {
    let native_refused = validate_native(jpeg, "image/jpeg").is_err();
    let hosted_refused = prepare(jpeg, Format::Jpeg).is_err();
    assert!(
        native_refused && hosted_refused,
        "{label}: native_refused={native_refused}, hosted_refused={hosted_refused}"
    );
}

#[test]
fn invalid_and_oversized_jpeg_icc_profiles_are_refused() {
    assert_accepted("ordinary JPEG", &ordinary_jpeg());
    assert_accepted("valid ICC profile", &jpeg_with_icc(PROFILE));

    let invalid = jpeg_with_icc(b"not-an-ICC-profile");
    assert_refused("invalid ICC profile", &invalid);

    // Keep the known profile's tag table and contents intact while extending
    // its declared profile size beyond the 256 KiB metadata limit.
    let mut oversized_profile = PROFILE.to_vec();
    oversized_profile.resize(ICC_LIMIT + 4, 0);
    let length = u32::try_from(oversized_profile.len()).expect("profile size");
    oversized_profile[..4].copy_from_slice(&length.to_be_bytes());
    let oversized = jpeg_with_icc(&oversized_profile);
    assert!(oversized.len() < 4 * 1024 * 1024);
    assert_refused("oversized ICC profile", &oversized);
}
