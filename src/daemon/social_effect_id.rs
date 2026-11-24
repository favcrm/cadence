// Social effect identifiers encode the owning installation.
pub fn social_effect_install(id: &str) -> Option<String> {
    let mut p = id.split('_');
    if p.next() != Some("sfx") {
        return None;
    };
    let hex = p.next()?;
    let nonce = p.next()?;
    if p.next().is_some()
        || hex.is_empty()
        || hex.len() % 2 != 0
        || nonce.len() != 32
        || !hex
            .bytes()
            .chain(nonce.bytes())
            .all(|b| b.is_ascii_hexdigit())
    {
        return None;
    };
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}
