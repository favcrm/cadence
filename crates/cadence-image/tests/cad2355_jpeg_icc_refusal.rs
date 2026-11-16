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

    // The valid control above may share tag data exactly. A second tag with
    // the same signature, however, makes the directory ambiguous.
    let tag_count = u32::from_be_bytes(PROFILE[128..132].try_into().unwrap()) as usize;
    assert!(tag_count >= 2, "P3 fixture needs at least two tags");
    let first_tag = 132;
    let second_tag = 144;
    assert_ne!(
        &PROFILE[first_tag..first_tag + 4],
        &PROFILE[second_tag..second_tag + 4],
        "P3 control must have distinct tag signatures"
    );
    let mut duplicate = PROFILE.to_vec();
    duplicate[second_tag..second_tag + 4]
        .copy_from_slice(&PROFILE[first_tag..first_tag + 4]);
    assert_refused("duplicate ICC tag signature", &jpeg_with_icc(&duplicate));

    // Point a different tag into the middle of an existing tag element.
    // Its range is aligned and in bounds, but overlaps only part of the
    // original element rather than sharing its exact offset and size.
    let source_index = (0..tag_count)
        .find(|index| {
            let at = 132 + index * 12;
            let length = u32::from_be_bytes(PROFILE[at + 8..at + 12].try_into().unwrap());
            length >= 12
        })
        .expect("P3 fixture needs a tag element of at least 12 bytes");
    let target_index = if source_index == 0 { 1 } else { 0 };
    let source = 132 + source_index * 12;
    let target = 132 + target_index * 12;
    let offset = u32::from_be_bytes(PROFILE[source + 4..source + 8].try_into().unwrap());
    let length = u32::from_be_bytes(PROFILE[source + 8..source + 12].try_into().unwrap());
    let mut overlap = PROFILE.to_vec();
    overlap[target + 4..target + 8].copy_from_slice(&(offset + 4).to_be_bytes());
    overlap[target + 8..target + 12].copy_from_slice(&(length - 4).to_be_bytes());
    assert_refused("partially overlapping ICC tag data", &jpeg_with_icc(&overlap));

    let invalid = jpeg_with_icc(b"not-an-ICC-profile");
    assert_refused("invalid ICC profile", &invalid);

    // The only APP2 ICC segment claims to be part one of two. The JPEG
    // raster remains valid, but the profile is incomplete.
    let mut incomplete = jpeg_with_icc(PROFILE);
    assert_eq!(&incomplete[6..18], b"ICC_PROFILE\0");
    assert_eq!(incomplete[18], 1);
    assert_eq!(incomplete[19], 1);
    incomplete[19] = 2;
    assert_refused("incomplete ICC segment sequence", &incomplete);

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
