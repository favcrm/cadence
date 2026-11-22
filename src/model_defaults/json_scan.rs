//! Strict JSON scanner and settings document parser for `model_defaults`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::{BTreeMap, HashSet};

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::Deserialize;

use crate::error::{Error, Result};

use super::{
    validate_config, ModelDefaults, SettingsWrite, MAX_CONFIG_BYTES, MAX_HTTP_BODY_BYTES,
    MAX_JSON_DEPTH,
};

pub(super) fn unique_string_map<'de, D, T>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct MapVisitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for MapVisitor<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an object with unique string keys")
        }
        fn visit_map<A>(self, mut access: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut map = BTreeMap::new();
            while let Some(key) = access.next_key::<String>()? {
                if map.contains_key(&key) {
                    return Err(de::Error::custom(format!("duplicate key '{key}'")));
                }
                map.insert(key, access.next_value()?);
            }
            Ok(map)
        }
    }
    deserializer.deserialize_map(MapVisitor(std::marker::PhantomData))
}

/// Walk JSON and reject duplicate object keys before serde's last-key
/// map can hide them. The walk is also the size and integer-revision gate
/// for a settings body. Nesting deeper than [`MAX_JSON_DEPTH`] is rejected
/// so the scan cannot overflow the daemon stack.
struct JsonScan<'a> {
    bytes: &'a [u8],
    i: usize,
}

impl<'a> JsonScan<'a> {
    fn new(raw: &'a str) -> Self {
        Self {
            bytes: raw.as_bytes(),
            i: 0,
        }
    }

    fn fail(message: impl Into<String>) -> Error {
        Error::invalid("invalid_config", message)
    }

    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.i), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Result<u8> {
        self.bytes
            .get(self.i)
            .copied()
            .ok_or_else(|| Self::fail("unexpected end of JSON"))
    }

    fn bump(&mut self) -> Result<u8> {
        let byte = self.peek()?;
        self.i += 1;
        Ok(byte)
    }

    fn expect(&mut self, want: u8) -> Result<()> {
        let got = self.bump()?;
        if got == want {
            Ok(())
        } else {
            Err(Self::fail(format!(
                "expected '{}', found '{}'",
                want as char, got as char
            )))
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.bump()?;
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(Self::fail("invalid unicode escape")),
            };
            value = (value << 4) | u32::from(digit);
        }
        Ok(value)
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self.bump()?;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escaped = self.bump()?;
                    match escaped {
                        b'"' | b'\\' | b'/' => out.push(escaped as char),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let unit = self.hex4()?;
                            if (0xD800..=0xDBFF).contains(&unit) {
                                if self.bump()? != b'\\' || self.bump()? != b'u' {
                                    return Err(Self::fail("lone unicode surrogate"));
                                }
                                let low = self.hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err(Self::fail("invalid unicode surrogate pair"));
                                }
                                let point = 0x10000 + (((unit - 0xD800) << 10) | (low - 0xDC00));
                                out.push(
                                    char::from_u32(point)
                                        .ok_or_else(|| Self::fail("invalid unicode scalar"))?,
                                );
                            } else if (0xDC00..=0xDFFF).contains(&unit) {
                                return Err(Self::fail("lone unicode surrogate"));
                            } else {
                                out.push(
                                    char::from_u32(unit)
                                        .ok_or_else(|| Self::fail("invalid unicode scalar"))?,
                                );
                            }
                        }
                        _ => return Err(Self::fail("invalid string escape")),
                    }
                }
                0x00..=0x1F => return Err(Self::fail("unescaped control character in string")),
                byte if byte < 0x80 => out.push(byte as char),
                byte => {
                    let width = match byte {
                        0xC2..=0xDF => 2,
                        0xE0..=0xEF => 3,
                        0xF0..=0xF4 => 4,
                        _ => return Err(Self::fail("invalid UTF-8 in string")),
                    };
                    let start = self.i - 1;
                    let end = start + width;
                    if end > self.bytes.len() {
                        return Err(Self::fail("truncated UTF-8 in string"));
                    }
                    let text = std::str::from_utf8(&self.bytes[start..end])
                        .map_err(|_| Self::fail("invalid UTF-8 in string"))?;
                    out.push_str(text);
                    self.i = end;
                }
            }
        }
    }

    fn parse_number(&mut self) -> Result<()> {
        if self.peek()? == b'-' {
            self.i += 1;
        }
        match self.peek()? {
            b'0' => self.i += 1,
            b'1'..=b'9' => {
                while self.bytes.get(self.i).is_some_and(|b| b.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            _ => return Err(Self::fail("invalid number")),
        }
        if self.bytes.get(self.i) == Some(&b'.') {
            self.i += 1;
            let start = self.i;
            while self.bytes.get(self.i).is_some_and(|b| b.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == start {
                return Err(Self::fail("invalid number"));
            }
        }
        if matches!(self.bytes.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.bytes.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let start = self.i;
            while self.bytes.get(self.i).is_some_and(|b| b.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == start {
                return Err(Self::fail("invalid number"));
            }
        }
        Ok(())
    }

    fn parse_literal(&mut self, literal: &[u8]) -> Result<()> {
        if self.bytes[self.i..].starts_with(literal) {
            self.i += literal.len();
            Ok(())
        } else {
            Err(Self::fail("invalid literal"))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<()> {
        if depth >= MAX_JSON_DEPTH {
            return Err(Self::fail(format!("JSON nesting exceeds {MAX_JSON_DEPTH}")));
        }
        self.skip_ws();
        match self.peek()? {
            b'{' => self.parse_object(None, depth),
            b'[' => self.parse_array(depth),
            b'"' => {
                self.parse_string()?;
                Ok(())
            }
            b't' => self.parse_literal(b"true"),
            b'f' => self.parse_literal(b"false"),
            b'n' => self.parse_literal(b"null"),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => Err(Self::fail("invalid JSON value")),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<()> {
        self.expect(b'[')?;
        self.skip_ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Ok(());
        }
        loop {
            self.parse_value(depth + 1)?;
            self.skip_ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Ok(());
                }
                _ => return Err(Self::fail("expected ',' or ']'")),
            }
        }
    }

    fn parse_object(
        &mut self,
        mut capture: Option<&mut EnvelopeCapture>,
        depth: usize,
    ) -> Result<()> {
        if depth >= MAX_JSON_DEPTH {
            return Err(Self::fail(format!("JSON nesting exceeds {MAX_JSON_DEPTH}")));
        }
        self.expect(b'{')?;
        self.skip_ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Ok(());
        }
        let mut keys = HashSet::new();
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            if !keys.insert(key.clone()) {
                return Err(Self::fail(format!("duplicate key '{key}'")));
            }
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let start = self.i;
            if capture.is_some() {
                match key.as_str() {
                    "expected_revision" | "config" => {}
                    other => {
                        return Err(Error::invalid(
                            "invalid_request",
                            format!("unknown settings field '{other}'"),
                        ))
                    }
                }
            }
            self.parse_value(depth + 1)?;
            if let Some(captured) = capture.as_mut() {
                let lexeme = std::str::from_utf8(&self.bytes[start..self.i])
                    .map_err(|_| Self::fail("settings field was not UTF-8"))?
                    .to_string();
                match key.as_str() {
                    "expected_revision" => captured.revision_lexeme = Some(lexeme),
                    "config" => captured.config_len = Some(lexeme.len()),
                    _ => {}
                }
            }
            self.skip_ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Ok(());
                }
                _ => return Err(Self::fail("expected ',' or '}'")),
            }
        }
    }
}

struct EnvelopeCapture {
    revision_lexeme: Option<String>,
    config_len: Option<usize>,
}

fn revision_from_lexeme(lexeme: &str) -> Result<i64> {
    if lexeme.is_empty() || !lexeme.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::invalid(
            "invalid_request",
            "expected_revision must be a nonnegative integer",
        ));
    }
    if lexeme.len() > 1 && lexeme.starts_with('0') {
        return Err(Error::invalid(
            "invalid_request",
            "expected_revision must be a nonnegative integer",
        ));
    }
    let value: u64 = lexeme.parse().map_err(|_| {
        Error::invalid(
            "invalid_request",
            "expected_revision must be a nonnegative integer",
        )
    })?;
    i64::try_from(value).map_err(|_| {
        Error::invalid(
            "invalid_request",
            "expected_revision is outside the stored integer range",
        )
    })
}

/// Parse a settings save body. Duplicate keys are rejected from the
/// original text, and the config value itself must be at most 16 KiB.
pub fn parse_settings_document(raw: &str) -> Result<SettingsWrite> {
    if raw.len() > MAX_HTTP_BODY_BYTES {
        return Err(Error::invalid(
            "invalid_request",
            format!("settings body exceeds {MAX_HTTP_BODY_BYTES} bytes"),
        ));
    }
    let mut scan = JsonScan::new(raw);
    scan.skip_ws();
    if scan.peek()? != b'{' {
        return Err(Error::invalid(
            "invalid_request",
            "settings body must be a JSON object",
        ));
    }
    let mut captured = EnvelopeCapture {
        revision_lexeme: None,
        config_len: None,
    };
    scan.parse_object(Some(&mut captured), 0)?;
    scan.skip_ws();
    if scan.i != scan.bytes.len() {
        return Err(Error::invalid(
            "invalid_request",
            "trailing data after settings JSON",
        ));
    }
    let Some(lexeme) = captured.revision_lexeme else {
        return Err(Error::invalid(
            "invalid_request",
            "settings body requires expected_revision",
        ));
    };
    let Some(config_len) = captured.config_len else {
        return Err(Error::invalid(
            "invalid_request",
            "settings body requires config",
        ));
    };
    if config_len > MAX_CONFIG_BYTES {
        return Err(Error::invalid(
            "invalid_config",
            format!("model defaults document exceeds {MAX_CONFIG_BYTES} bytes"),
        ));
    }
    let expected_revision = revision_from_lexeme(&lexeme)?;
    let mut body: SettingsBody = serde_json::from_str(raw).map_err(|err| {
        Error::invalid(
            "invalid_config",
            format!("model defaults document was rejected: {err}"),
        )
    })?;
    validate_config(&mut body.config)?;
    Ok(SettingsWrite {
        expected_revision,
        config: body.config,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsBody {
    #[allow(dead_code)]
    expected_revision: u64,
    config: ModelDefaults,
}
