use std::io::Cursor;

use cadence_image::{prepare, validate_native, Format};
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageReader};

fn png_with_orientation(orientation: u16) -> Vec<u8> {
    // An asymmetric 32x16 image: red on the left, blue on the right.
    let mut pixels = Vec::new();
    for _ in 0..16 {
        for x in 0..32 {
            pixels.extend_from_slice(if x < 16 { &[255, 0, 0] } else { &[0, 0, 255] });
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, 32, 16, ExtendedColorType::Rgb8)
        .expect("encode PNG fixture");

    // Little-endian TIFF with one SHORT Orientation entry and no next IFD.
    // A PNG eXIf chunk contains the TIFF bytes, not the JPEG Exif prefix.
    let mut tiff = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, // TIFF header and IFD offset
        1, 0, // one directory entry
        0x12, 0x01, 3, 0, // Orientation tag, SHORT type
        1, 0, 0, 0, // one value
    ];
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&[0, 0]); // padding for the inline SHORT
    tiff.extend_from_slice(&[0, 0, 0, 0]); // no next IFD

    let mut chunk = Vec::new();
    chunk.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"eXIf");
    chunk.extend_from_slice(&tiff);
    let crc = crc32fast::hash(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    // The PNG signature and IHDR occupy the first 33 bytes.
    png.splice(33..33, chunk);
    png
}

fn assert_upright(bytes: &[u8], label: &str) {
    let mut decoder = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .expect("identify returned image")
        .into_decoder()
        .expect("open returned image decoder");
    let orientation = decoder.orientation().expect("read returned orientation");
    let mut image = DynamicImage::from_decoder(decoder).expect("decode returned pixels");
    image.apply_orientation(orientation);
    let rgb = image.to_rgb8();
    assert_eq!(rgb.dimensions(), (16, 32), "{label}: upright dimensions");
    let top = rgb.get_pixel(8, 8).0;
    let bottom = rgb.get_pixel(8, 24).0;
    assert!(
        u16::from(top[0]) > u16::from(top[2]) + 60,
        "{label}: upright top must be red, got {top:?}"
    );
    assert!(
        u16::from(bottom[2]) > u16::from(bottom[0]) + 60,
        "{label}: upright bottom must be blue, got {bottom:?}"
    );
}

#[test]
fn png_exif_orientation_is_preserved_or_applied() {
    let upright = png_with_orientation(6);
    assert_upright(&upright, "input control");

    let native = validate_native(&upright, "image/png").expect("prepare native PNG");
    assert_upright(native.as_ref(), "native returned image");

    let hosted = prepare(&upright, Format::Png).expect("prepare hosted PNG");
    assert!(matches!(hosted.format, Format::Png | Format::Jpeg));
    assert_upright(&hosted.bytes, "hosted returned image");

    let malformed = png_with_orientation(9);
    let native_refused = validate_native(&malformed, "image/png").is_err();
    let hosted_refused = prepare(&malformed, Format::Png).is_err();
    assert!(
        native_refused && hosted_refused,
        "invalid orientation: native_refused={native_refused}, hosted_refused={hosted_refused}"
    );
}

#[test]
fn native_oriented_png_preserves_sixteen_bit_samples() {
    // A 2x3 RGB16 raster in row-major order. The first row has adjacent
    // low-order-distinct samples; all other red values distinguish position.
    let red_samples = [0x1000u16, 0x1001, 0x3000, 0x4000, 0x5000, 0x6000];
    let mut pixels = Vec::new();
    for red in red_samples {
        pixels.extend_from_slice(&red.to_ne_bytes());
        pixels.extend_from_slice(&0u16.to_ne_bytes());
        pixels.extend_from_slice(&0u16.to_ne_bytes());
    }

    // Little-endian TIFF with Orientation=6 and no next IFD.
    let tiff = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, // TIFF header and IFD offset
        1, 0, // one entry
        0x12, 0x01, 3, 0, // Orientation tag, SHORT type
        1, 0, 0, 0, // one value
        6, 0, 0, 0, // Orientation=6 and inline padding
        0, 0, 0, 0, // no next IFD
    ];
    let mut png = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut png);
    encoder.set_exif_metadata(tiff).expect("set PNG eXIf");
    encoder
        .write_image(&pixels, 2, 3, ExtendedColorType::Rgb16)
        .expect("encode RGB16 PNG");
    assert!(png.len() < 4 * 1024 * 1024);

    let check = |bytes: &[u8], label: &str| {
        let mut decoder = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .expect("identify PNG")
            .into_decoder()
            .expect("open PNG decoder");
        let orientation = decoder.orientation().expect("read orientation");
        let mut image = DynamicImage::from_decoder(decoder).expect("decode RGB16 pixels");
        assert!(
            matches!(
                image.color(),
                image::ColorType::Rgb16 | image::ColorType::Rgba16
            ),
            "{label}: returned pixels must remain 16-bit"
        );
        image.apply_orientation(orientation);
        let rgb = image.to_rgb16();
        assert_eq!(rgb.dimensions(), (3, 2), "{label}: upright dimensions");
        let expected = [0x5000u16, 0x3000, 0x1000, 0x6000, 0x4000, 0x1001];
        for (pixel, red) in rgb.pixels().zip(expected) {
            assert_eq!(pixel.0, [red, 0, 0], "{label}: upright 16-bit sample");
        }
        assert_ne!(rgb.get_pixel(2, 0).0[0], rgb.get_pixel(2, 1).0[0]);
    };

    check(&png, "input control");
    let native = validate_native(&png, "image/png").expect("prepare native RGB16 PNG");
    check(native.as_ref(), "native returned image");
}
