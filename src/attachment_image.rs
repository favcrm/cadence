//! CAD-2162: bounded image inspection and safe SVG rasterization for
//! retained chat attachments.
//!
//! Two responsibilities, kept out of `store::chat_files` so the custody
//! module stays about provenance:
//!
//! - **Raster validation.** PNG/JPEG/WebP bytes are decoded under hard
//!   dimension, pixel and allocation limits. The compressed size is
//!   bounded separately ([`IMAGE_MAX_BYTES`]) by the native prompt bound
//!   the Pi adapter enforces, so an image accepted here can actually be
//!   delivered as native image pixels instead of failing later.
//! - **SVG source + rasterization.** SVG source is retained and read as
//!   inert text. Visual delivery parses it with the linked `roxmltree`
//!   parser (document types disabled) and refuses any document that carries
//!   script, embedded markup, animation, an event handler or a reference
//!   outside a same-document fragment; [`rasterize_svg`] then renders it
//!   with rsvg-convert resolved from PATH, root-owned and non-writable,
//!   under a Landlock ruleset built before fork, RLIMIT_CPU/AS/FSIZE/CORE/
//!   NOFILE/NPROC and a process-group deadline.
//!
//! Neither function mutates custody. They answer "may this be delivered
//! as visual input?" for the exact bytes the caller already digest-
//! checked. A rasterized SVG is a derived turn image; the retained
//! source stays the original attachment.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};

/// The per-image ceiling the native image prompt accepts. The Pi adapter
/// refuses anything larger before writing a prompt; refusing at upload
/// keeps that promise honest instead of retaining an image no turn can
/// deliver.
pub const IMAGE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Aggregate native image bytes one prompt may carry (a library pin plus
/// the attachment set). Base64 rides above it; refusing here keeps the
/// provider request bounded instead of failing at the provider with an
/// opaque error.
pub const TURN_IMAGE_TOTAL_MAX: usize = 16 * 1024 * 1024;

/// Longest decoded side. Independent of the compressed byte count: a
/// tiny file can expand enormously.
const IMAGE_SIDE_LIMIT: u32 = 4096;
const IMAGE_PIXEL_LIMIT: u64 = 4096 * 4096;
const IMAGE_DECODE_ALLOC_LIMIT: u64 = 96 * 1024 * 1024;

/// Rasterized SVG output bound. The renderer is asked to fit within
/// this square; the decoded PNG is re-checked before delivery.
const SVG_RASTER_SIDE: u32 = 2048;
/// The raster output bound the parent accepts. A 2048-square RGBA
/// intermediate can approach 16 MiB before PNG compression, so this is
/// deliberately above the 4 MiB delivery bound: an over-bound PNG is
/// re-encoded, not truncated or rejected while it is still decodable.
const SVG_RASTER_FSIZE: u64 = 24 * 1024 * 1024;
/// The child's per-file cap. Higher than the accepted output because
/// fontconfig may write its own cache in the private temp dir; the
/// accepted output is still checked against [`SVG_RASTER_FSIZE`].
const SVG_CHILD_FSIZE: u64 = 64 * 1024 * 1024;
const SVG_TIMEOUT: Duration = Duration::from_secs(15);
const SVG_STDERR_BOUND: usize = 64 * 1024;

const CONVERTER: &str = "rsvg-convert";

/// `true` for the MIME kinds delivered as native image pixels.
pub fn visual_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/webp" | "image/svg+xml"
    )
}

/// `true` for the raster kinds validated by [`validate_raster`].
pub fn raster_mime(mime: &str) -> bool {
    matches!(mime, "image/png" | "image/jpeg" | "image/webp")
}

/// The `'static` MIME for a retained visual row, as `TurnImage` needs.
/// SVG is absent: its delivered type is decided by the rasterizer.
pub fn static_visual_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

fn image_format(mime: &str) -> Option<image::ImageFormat> {
    match mime {
        "image/png" => Some(image::ImageFormat::Png),
        "image/jpeg" => Some(image::ImageFormat::Jpeg),
        "image/webp" => Some(image::ImageFormat::WebP),
        _ => None,
    }
}

/// The byte-sniffed raster MIME, independent of any caller claim.
pub fn sniff_raster(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(IMAGE_SIDE_LIMIT);
    limits.max_image_height = Some(IMAGE_SIDE_LIMIT);
    limits.max_alloc = Some(IMAGE_DECODE_ALLOC_LIMIT);
    limits
}

fn decode_raster(bytes: &[u8], mime: &str) -> Result<image::DynamicImage> {
    let format = image_format(mime)
        .ok_or_else(|| Error::rejected("the attachment does not declare a raster image type"))?;
    let limits = decode_limits();
    let mut probe = image::ImageReader::with_format(Cursor::new(bytes), format);
    probe.limits(limits.clone());
    let (width, height) = probe
        .into_dimensions()
        .map_err(|_| Error::rejected("the image dimensions are invalid or exceed the bound"))?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > IMAGE_PIXEL_LIMIT {
        return Err(Error::rejected(format!(
            "the image is {width}x{height} — over the {IMAGE_SIDE_LIMIT}-pixel-side \
             delivery bound"
        )));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| Error::rejected("the image cannot be decoded within the delivery bound"))?;
    if decoded.width() != width || decoded.height() != height {
        return Err(Error::rejected(
            "the image dimensions changed during decode",
        ));
    }
    Ok(decoded)
}

/// Prove raster bytes are exactly their declared kind and decode within
/// the delivery bound. The caller passes bytes whose size and digest it
/// already checked; this never opens a path.
pub fn validate_raster(bytes: &[u8], mime: &str) -> Result<()> {
    if bytes.len() > IMAGE_MAX_BYTES {
        return Err(Error::rejected(format!(
            "the image is {} bytes — over the {IMAGE_MAX_BYTES}-byte native image \
             prompt bound",
            bytes.len()
        )));
    }
    let sniffed = sniff_raster(bytes)
        .ok_or_else(|| Error::rejected("the bytes carry no supported image signature"))?;
    if sniffed != mime {
        return Err(Error::rejected(format!(
            "the image declares '{mime}' but its bytes sniff as '{sniffed}'"
        )));
    }
    decode_raster(bytes, mime).map(|_| ())
}

/// The most nodes one document may decode into. A source small enough to
/// upload can still expand into a very large tree, and the renderer runs in
/// the daemon's own process tree.
const SVG_NODE_LIMIT: u32 = 100_000;

/// Screen SVG source with a real XML parser before a renderer or a reader
/// sees it, then walk the decoded tree. A document that trips any rule is
/// refused whole: the renderer never needs the network or a local file,
/// never runs script, and never expands an entity.
///
/// The tree comes from `roxmltree` with document-type parsing disabled, so
/// the rules judge what the parser actually decoded — character references,
/// namespace prefixes and CDATA included — rather than a second, weaker
/// reading of the same bytes.
pub fn validate_svg_source(source: &str) -> Result<()> {
    if source.trim().is_empty() {
        return Err(Error::rejected("the SVG source is empty"));
    }
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: SVG_NODE_LIMIT,
        entity_resolver: None,
    };
    let document = roxmltree::Document::parse_with_options(source, options).map_err(|error| {
        Error::rejected(match error {
            roxmltree::Error::DtdDetected => {
                "the SVG was refused: an SVG may not declare a document type"
            }
            roxmltree::Error::NodesLimitReached => {
                "the SVG was refused: it expands into more elements than the render bound allows"
            }
            _ => "the SVG was refused: it is not a well-formed XML document",
        })
    })?;
    if !document
        .root_element()
        .tag_name()
        .name()
        .eq_ignore_ascii_case("svg")
    {
        return Err(Error::rejected(
            "the SVG was refused: the root element is not <svg>",
        ));
    }
    for node in document.descendants() {
        match node.node_type() {
            roxmltree::NodeType::PI => {
                return Err(Error::rejected(
                    "the SVG was refused: an SVG may not carry a processing instruction",
                ));
            }
            roxmltree::NodeType::Element => {
                let name = node.tag_name().name().to_ascii_lowercase();
                if let Some(why) = refused_element(&name) {
                    return Err(Error::rejected(format!("the SVG was refused: {why}")));
                }
                for attribute in node.attributes() {
                    // The parser reports the local name, so a namespace
                    // prefix cannot hide an event handler (`foo:onload`).
                    let attribute_name = attribute.name().to_ascii_lowercase();
                    let is_event = attribute_name.len() > 2
                        && attribute_name.starts_with("on")
                        && attribute_name.as_bytes()[2].is_ascii_alphabetic();
                    if is_event {
                        return Err(Error::rejected(format!(
                            "the SVG was refused: the '{attribute_name}' event handler is not \
                             allowed"
                        )));
                    }
                    if attribute_name == "href" && !safe_svg_reference(attribute.value()) {
                        return Err(Error::rejected(
                            "the SVG was refused: href may reference only a same-document \
                             fragment or an embedded PNG/JPEG/WebP data URI",
                        ));
                    }
                    check_svg_value(attribute.value())?;
                }
                // `<style>` content is CSS the renderer interprets; every
                // other character data is inert text.
                if name == "style" {
                    for child in node.children().filter(|child| child.is_text()) {
                        check_svg_value(child.text().unwrap_or_default())?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Elements refused outright: script, embedded markup, and anything that
/// mutates the document during a render.
fn refused_element(local: &str) -> Option<&'static str> {
    match local {
        "script" => Some("script is not allowed"),
        "foreignobject" => Some("foreign markup is not allowed"),
        "iframe" => Some("an embedded frame is not allowed"),
        "embed" => Some("embedded external content is not allowed"),
        "object" => Some("an embedded object is not allowed"),
        "set" | "animate" | "animatemotion" | "animatetransform" | "animatecolor" | "discard" => {
            Some("animation and attribute mutation are not allowed")
        }
        "handler" | "listener" => Some("event handler elements are not allowed"),
        _ => None,
    }
}

/// Refuse a value that could reach the network, a local file or a second
/// grammar. Applied to every attribute value and to `<style>` content,
/// because an SVG carries CSS in both.
fn check_svg_value(value: &str) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    // `to_ascii_lowercase` preserves byte length, so the indices below map
    // onto `value`.
    let lower = value.to_ascii_lowercase();
    for (needle, why) in [
        ("javascript:", "a javascript: reference is not allowed"),
        ("@import", "importing a stylesheet is not allowed"),
        ("@font-face", "loading an external font is not allowed"),
        ("expression(", "a CSS expression is not allowed"),
        ("/*", "CSS comments are not allowed"),
        // CSS functions that can carry a URL outside url(). librsvg may not
        // implement all of them; refusing the token costs only that document.
        ("image(", "a CSS image() function is not allowed"),
        ("image-set(", "a CSS image-set() function is not allowed"),
        ("cross-fade(", "a CSS cross-fade() function is not allowed"),
        ("\\", "CSS escape sequences are not allowed"),
    ] {
        if lower.contains(needle) {
            return Err(Error::rejected(format!("the SVG was refused: {why}")));
        }
    }
    // Every `url(...)` target must be a same-document fragment. This covers
    // paint servers, filters, clip paths, cursors and style blocks without
    // needing a CSS parser.
    let mut at = 0;
    while let Some(relative) = lower[at..].find("url(") {
        let start = at + relative + 4;
        let Some(end) = value[start..].find(')') else {
            return Err(Error::rejected(
                "the SVG was refused: an unterminated url() reference",
            ));
        };
        let target = value[start..start + end]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .trim();
        if !target.starts_with('#') {
            return Err(Error::rejected(
                "the SVG was refused: url() may reference only a same-document fragment",
            ));
        }
        at = start + end + 1;
    }
    Ok(())
}

fn safe_svg_reference(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || value.starts_with('#')
        || [
            "data:image/png;base64,",
            "data:image/jpeg;base64,",
            "data:image/webp;base64,",
        ]
        .iter()
        .any(|prefix| {
            value
                .get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
}

/// Rasterize SVG source to a bounded PNG (or JPEG when the PNG would
/// exceed the native prompt bound).
///
/// Returns the delivered MIME and bytes. Every refusal names the actual
/// obstacle: missing converter, no Landlock, unsafe source, timeout, or
/// output outside the bound. Never falls back to sending the source as
/// if it were pixels.
pub fn rasterize_svg(source: &[u8]) -> Result<(&'static str, Vec<u8>)> {
    let text =
        std::str::from_utf8(source).map_err(|_| Error::rejected("the SVG source is not UTF-8"))?;
    validate_svg_source(text)?;
    let program = resolve_converter()?;
    let dir = tempfile::Builder::new()
        .prefix("cadence-svg-")
        .tempdir()
        .map_err(|e| Error::rejected(format!("staging the SVG render failed: {e}")))?;
    let input = dir.path().join("source.svg");
    let output = dir.path().join("raster.png");
    std::fs::write(&input, source)
        .map_err(|e| Error::rejected(format!("staging the SVG render failed: {e}")))?;

    let policy = raster_policy(&program, dir.path());
    let prepared = crate::confine::PreparedRuleset::prepare(&policy).map_err(|e| {
        Error::rejected(format!(
            "the SVG could not be rasterized safely: this host cannot confine the \
             renderer ({e}); the SVG source was not converted"
        ))
    })?;

    let side = SVG_RASTER_SIDE.to_string();
    let mut cmd = std::process::Command::new(&program);
    cmd.arg(&input)
        .args([
            "-f",
            "png",
            "-a",
            "-w",
            side.as_str(),
            "-h",
            side.as_str(),
            "-o",
        ])
        .arg(&output)
        .current_dir(dir.path())
        .env_clear()
        .env("HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path().join("cache"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("LC_ALL", "C.UTF-8")
        .env("LANG", "C.UTF-8")
        .stdin(std::process::Stdio::null());
    // The child applies the ruleset with syscalls only; the ruleset and
    // its path rules were built above, before fork.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(move || {
            apply_render_limits()?;
            prepared.restrict_after_fork()
        });
    }
    let (result, _) = crate::proc::run_bounded_limited(&mut cmd, SVG_TIMEOUT, SVG_STDERR_BOUND)
        .map_err(|e| match e {
            crate::proc::BoundedError::Spawn(ref io)
                if io.kind() == std::io::ErrorKind::NotFound =>
            {
                Error::rejected(converter_missing())
            }
            crate::proc::BoundedError::TimedOut { .. } => {
                Error::rejected("the SVG could not be rasterized within the 15-second limit")
            }
            e => Error::rejected(format!("the SVG could not be rasterized: {e}")),
        })?;
    if !result.status.success() {
        return Err(Error::rejected(
            "the SVG converter rejected the source — it is not a well-formed, \
             supported SVG",
        ));
    }
    let meta = std::fs::metadata(&output)
        .map_err(|_| Error::rejected("the SVG converter produced no output"))?;
    if !meta.is_file() || meta.len() == 0 {
        return Err(Error::rejected("the SVG converter produced no output"));
    }
    if meta.len() > SVG_RASTER_FSIZE {
        return Err(Error::rejected(
            "the rasterized SVG exceeds the render size bound",
        ));
    }
    let png = std::fs::read(&output)
        .map_err(|e| Error::rejected(format!("reading the rasterized SVG failed: {e}")))?;
    // `rsvg-convert -f png` always emits a PNG; prove it and its bounds
    // rather than trusting the tool's own claim. An over-bound PNG is
    // re-encoded, not silently accepted or truncated, so the byte cap
    // applies to the delivered bytes rather than the intermediate.
    if png.len() <= IMAGE_MAX_BYTES {
        validate_raster(&png, "image/png")
            .map_err(|e| Error::rejected(format!("the rasterized SVG is unusable: {e}")))?;
        return Ok(("image/png", png));
    }
    let jpeg = shrink_to_prompt_bound(&png)?;
    Ok(("image/jpeg", jpeg))
}

/// Re-encode an over-bound raster as JPEG so a valid source image still
/// reaches the model. Refuses if even the re-encode exceeds the native
/// prompt bound.
fn shrink_to_prompt_bound(png: &[u8]) -> Result<Vec<u8>> {
    let image = decode_raster(png, "image/png")?;
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
        .encode_image(&rgb)
        .map_err(|_| Error::rejected("the rasterized SVG could not be re-encoded"))?;
    if out.len() > IMAGE_MAX_BYTES {
        return Err(Error::rejected(format!(
            "the rasterized SVG is {} bytes after re-encoding — over the \
             {IMAGE_MAX_BYTES}-byte native image prompt bound",
            out.len()
        )));
    }
    validate_raster(&out, "image/jpeg")?;
    Ok(out)
}

fn converter_missing() -> String {
    format!(
        "SVG rasterization is unavailable on this host because `{CONVERTER}` is not \
         installed (install librsvg2-bin); the SVG source is still readable as text, \
         but it was not sent as visual input"
    )
}

/// Resolve `rsvg-convert` from `PATH` and require a root-owned,
/// non-group/world-writable regular executable. A user-writable binary
/// could be swapped between resolution and exec; Landlock would still
/// confine it, but running a binary the daemon does not trust is not
/// worth the risk.
fn resolve_converter() -> Result<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Err(Error::rejected(converter_missing()));
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(CONVERTER);
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        let canonical = candidate.canonicalize().map_err(|_| {
            Error::rejected(format!(
                "the resolved `{CONVERTER}` is not a usable executable"
            ))
        })?;
        let canonical_meta = std::fs::metadata(&canonical).map_err(|_| {
            Error::rejected(format!(
                "the resolved `{CONVERTER}` is not a usable executable"
            ))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if canonical_meta.uid() != 0 || canonical_meta.permissions().mode() & 0o022 != 0 {
                return Err(Error::rejected(format!(
                    "the resolved `{CONVERTER}` at {} is not root-owned and \
                     non-writable; refusing to execute an untrusted renderer",
                    canonical.display()
                )));
            }
        }
        return Ok(canonical);
    }
    Err(Error::rejected(converter_missing()))
}

/// The renderer's filesystem policy: the binary and system libraries/
/// fonts readable and executable, the private temp dir readable and
/// writable, nothing else.
fn raster_policy(program: &Path, temp: &Path) -> crate::confine::Policy {
    let mut read = vec![
        program.to_path_buf(),
        temp.to_path_buf(),
        PathBuf::from("/usr"),
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/lib64"),
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/share/fontconfig"),
        PathBuf::from("/usr/share/locale"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/etc/fonts"),
        PathBuf::from("/etc/ld.so.cache"),
        PathBuf::from("/etc/ld.so.conf"),
        PathBuf::from("/etc/ld.so.conf.d"),
        // glib/fontconfig may resolve the current account and locale;
        // these are non-secret system lookups, not user data.
        PathBuf::from("/etc/passwd"),
        PathBuf::from("/etc/group"),
        PathBuf::from("/etc/nsswitch.conf"),
        PathBuf::from("/etc/localtime"),
    ];
    read.retain(|p| p.exists());
    crate::confine::Policy {
        read,
        write: vec![temp.to_path_buf()],
    }
}

/// Child-only resource caps. Called from `pre_exec`, so syscalls only.
fn apply_render_limits() -> std::io::Result<()> {
    // A macro keeps each limit's constant and syscall argument the same
    // inferred type across platforms (the resource type differs between
    // libc targets) and needs no post-fork closure inference.
    macro_rules! set_limit {
        ($resource:expr, $soft:expr, $hard:expr) => {{
            let limits = libc::rlimit {
                rlim_cur: $soft as libc::rlim_t,
                rlim_max: $hard as libc::rlim_t,
            };
            // SAFETY: plain setrlimit with a live struct.
            if unsafe { libc::setrlimit($resource, &limits) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }};
    }
    set_limit!(libc::RLIMIT_CPU, 10, 10);
    set_limit!(libc::RLIMIT_AS, 768u64 * 1024 * 1024, 768u64 * 1024 * 1024);
    set_limit!(libc::RLIMIT_FSIZE, SVG_CHILD_FSIZE, SVG_CHILD_FSIZE);
    set_limit!(libc::RLIMIT_CORE, 0, 0);
    set_limit!(libc::RLIMIT_NOFILE, 64, 64);
    // The renderer needs no children. Threads count against this on Linux;
    // 64 leaves room for the loader while bounding a fork/thread bomb.
    set_limit!(libc::RLIMIT_NPROC, 64, 64);
    Ok(())
}

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
