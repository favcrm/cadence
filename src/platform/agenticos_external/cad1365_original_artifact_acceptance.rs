//! CAD-1365 acceptance: when the verified downloaded ORIGINAL image bytes
//! exceed the 2 MiB custody bound, `AppCapabilityOutput.result` must retain
//! an `original_artifact` provenance descriptor —
//! `{schema:1, ref:<exact opaque artifact ref>, sha256:"sha256:<descriptor
//! digest>", bytes:<exact original byte count>, media_type:<original
//! validated MIME before any JPEG re-encode>}` — beside the unchanged
//! derivative `asset_sha256`/`asset_media_type`/`asset.bytes`. The contract
//! is frozen: the descriptor carries no URL, credential or key material,
//! and is a receipt, not a frame-asset reply.
//!
//! Expectations are derived ONLY from the synthetic fixture's original
//! bytes and the trusted provider descriptor, never from the returned
//! output. The current adapter stores just the derivative digest, so the
//! `original_artifact` assertions fail (RED) until the implementation
//! lands; the refusal case reuses the existing digest guard unchanged.

use image::ImageEncoder as _;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::image::ASSET_LIMIT;
use super::tests::{
    artifact_entry, artifact_response, image_adapter, image_proof, job_view, json_response,
    media_door, media_quote,
};
use crate::platform::adapter::PlatformAdapter;

/// A valid in-bounds square PNG whose encoded length exceeds the custody
/// bound, so `fit_to_custody` must run: noise covers two horizontal thirds
/// (incompressible under PNG, so the original tops 2 MiB) while the flat
/// third lets every JPEG quality step land inside ASSET_LIMIT. 1280² is
/// inside the side/pixel/allocation decode limits.
fn noisy_square_png() -> Vec<u8> {
    let side = 1280u32;
    let noise_until = side * 2 / 3;
    let mut pixels = vec![0u8; (side * side * 3) as usize];
    // SplitMix64 stream: deterministic, test-local, incompressible.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for y in 0..side {
        for x in 0..side {
            let offset = ((y * side + x) * 3) as usize;
            if x < noise_until {
                for channel in 0..3 {
                    state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                    let mut z = state;
                    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                    pixels[offset + channel] = (z ^ (z >> 31)) as u8;
                }
            } else {
                pixels[offset] = 0x4a;
                pixels[offset + 1] = 0x52;
                pixels[offset + 2] = 0x6b;
            }
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, side, side, image::ExtendedColorType::Rgb8)
        .unwrap();
    assert!(
        png.len() > ASSET_LIMIT,
        "fixture must force the custody re-encode path ({} bytes)",
        png.len()
    );
    png
}

#[test]
fn cad1365_original_artifact_descriptor_survives_the_custody_downsize() {
    let original = noisy_square_png();
    // Derived from the fixture BEFORE the call, never from the output.
    let original_len = original.len() as u64;
    let original_sha256 = format!("sha256:{:x}", Sha256::digest(&original));
    // The exact opaque ref the trusted provider descriptor carries.
    let artifact = artifact_entry("med_job1365", &original);
    let original_ref = artifact["ref"].as_str().unwrap().to_owned();
    let original_bytes = original.clone();
    let (base, _seen, worker) = media_door(move |request| {
        match (request.method.as_str(), request.url.as_str()) {
            ("POST", "/v1/runtime/media/image") => json_response(
                201,
                json!({"ok":true,"data":{"job":job_view("med_job1365","succeeded",json!([artifact.clone()]),false,31_500)}}),
            ),
            ("GET", _) => artifact_response(&original_bytes),
            _ => panic!(
                "unexpected media request {} {}",
                request.method, request.url
            ),
        }
    });
    let adapter = image_adapter(&base);
    let proof = image_proof(media_quote());
    let output = adapter
        .execute_app_capability(b"test-token", &proof, &json!({}), "app-call-1365")
        .unwrap();
    worker.join().unwrap();

    let result = &output.result;
    // Legacy fields stay DERIVATIVE, exactly as before.
    let asset = output.asset.expect("retained image asset");
    assert_eq!(asset.media_type, "image/jpeg");
    assert!(asset.bytes.len() <= ASSET_LIMIT, "derivative fits custody");
    assert_eq!(
        result["asset_sha256"],
        format!("sha256:{:x}", Sha256::digest(&asset.bytes)),
        "legacy digest covers the retained derivative"
    );
    assert_eq!(result["asset_media_type"], "image/jpeg");
    // The derivative is provably not the original.
    assert!(asset.bytes.len() < original_len as usize);
    assert_ne!(result["asset_sha256"], original_sha256);

    // Frozen contract: the verified ORIGINAL descriptor rides the receipt.
    let original_artifact = &result["original_artifact"];
    assert_eq!(original_artifact["schema"], 1);
    assert_eq!(original_artifact["ref"], original_ref);
    assert_eq!(original_artifact["sha256"], original_sha256);
    assert_eq!(original_artifact["bytes"], original_len);
    // Original MIME, validated from bytes BEFORE the JPEG re-encode.
    assert_eq!(original_artifact["media_type"], "image/png");
    // Receipt shape only: no URL, credential or private key material, and
    // no extra fields beyond the frozen four-plus-schema shape.
    let fields = original_artifact.as_object().expect("descriptor object");
    let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["bytes", "media_type", "ref", "schema", "sha256"],
        "original_artifact carries only the frozen provenance fields"
    );
}

/// A descriptor digest that does not match the downloaded bytes must still
/// be refused by the existing guard; no original receipt may describe bytes
/// that failed verification. Reuses the real ArtifactMismatch guard.
#[test]
fn cad1365_original_digest_mismatch_is_refused_and_no_receipt_describes_it() {
    let original = noisy_square_png();
    // Same byte count, wrong digest: the descriptor lies about the content.
    let mut forged = artifact_entry("med_job1365x", &original);
    let forged_ref = format!(
        "med_job1365x.{:x}",
        Sha256::digest(b"not the original bytes")
    );
    forged["ref"] = json!(forged_ref);
    forged["digest"] = json!(format!("{:x}", Sha256::digest(b"not the original bytes")));
    let served = original.clone();
    let (base, _seen, worker) = media_door(move |request| {
        match (request.method.as_str(), request.url.as_str()) {
            ("POST", "/v1/runtime/media/image") => json_response(
                201,
                json!({"ok":true,"data":{"job":job_view("med_job1365x","succeeded",json!([forged.clone()]),false,31_500)}}),
            ),
            ("GET", _) => artifact_response(&served),
            _ => panic!(
                "unexpected media request {} {}",
                request.method, request.url
            ),
        }
    });
    let adapter = image_adapter(&base);
    let proof = image_proof(media_quote());
    let error = adapter
        .execute_app_capability(b"test-token", &proof, &json!({}), "app-call-1365")
        .err()
        .expect("a descriptor/content mismatch must be refused");
    assert!(error.contains("differs from its receipt"), "{error}");
    worker.join().unwrap();
}
