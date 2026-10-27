//! Strict scalars for the v1 protected-Connection contract.
//!
//! Every wire representation is a canonical string or a checked safe integer;
//! no `f64`, no `usize`, no JS-Number-sized revision. Validation happens at
//! deserialization, so a value that reached a typed struct is already canonical.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;

/// Largest value any Counter/UsdMicros/Revision may hold (`i64::MAX`).
pub const I64_MAX_U64: u64 = i64::MAX as u64;
/// Largest integer that survives a JS `Number` round trip.
pub const MAX_SAFE_MS: u64 = 9_007_199_254_740_991;
pub const MAX_IDENTIFIER_BYTES: usize = 128;
pub const MAX_MODEL_BYTES: usize = 200;
pub const MAX_HTTPS_URL_BYTES: usize = 2_048;
pub const PICO_PER_USD: u128 = 1_000_000_000_000;
pub const PICO_PER_MICRO_USD: u128 = 1_000_000;
/// Smallest known-secret fragment the projection screen refuses to emit.
pub const SECRET_FRAGMENT_BYTES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueError(String);

impl ValueError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ValueError {}

fn canonical_u64(value: &str, positive: bool) -> Result<u64, ValueError> {
    // `0|[1-9][0-9]*` — a sign, padding, exponent or non-ASCII digit refuses.
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 19 {
        return Err(ValueError::new("not a canonical bounded decimal"));
    }
    if bytes[0] == b'0' && bytes.len() != 1 {
        return Err(ValueError::new("leading zero in canonical decimal"));
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return Err(ValueError::new("canonical decimal is not ASCII digits"));
    }
    let parsed: u64 = value
        .parse()
        .map_err(|_| ValueError::new("canonical decimal exceeds u64"))?;
    if parsed > I64_MAX_U64 {
        return Err(ValueError::new("canonical decimal exceeds i64"));
    }
    if positive && parsed == 0 {
        return Err(ValueError::new("revision must be positive"));
    }
    Ok(parsed)
}

macro_rules! decimal_newtype {
    ($name:ident, $doc:literal, $positive:expr) => {
        #[doc = $doc]
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema,
        )]
        #[serde(try_from = "String")]
        #[schemars(with = "String")]
        pub struct $name(u64);

        impl $name {
            pub fn get(self) -> u64 {
                self.0
            }
            /// Trusted in-process construction; the canonical bound still holds.
            pub fn new(value: u64) -> Result<Self, ValueError> {
                if value > I64_MAX_U64 {
                    return Err(ValueError::new("decimal exceeds i64"));
                }
                if $positive && value == 0 {
                    return Err(ValueError::new("revision must be positive"));
                }
                Ok(Self(value))
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValueError;
            fn try_from(value: String) -> Result<Self, ValueError> {
                canonical_u64(&value, $positive).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0.to_string())
            }
        }
    };
}

decimal_newtype!(
    Counter,
    "Nonnegative canonical decimal counter (`0|[1-9][0-9]*`, <= i64::MAX).",
    false
);
decimal_newtype!(
    UsdMicros,
    "Nonnegative exact USD micro-dollars as a canonical decimal string.",
    false
);
decimal_newtype!(
    Revision,
    "Positive canonical decimal revision (0 refuses).",
    true
);

impl Counter {
    pub fn zero() -> Self {
        Counter(0)
    }
    /// Checked increment; exhaustion refuses rather than wrapping.
    pub fn checked_next(self) -> Result<Self, ValueError> {
        let next = self
            .0
            .checked_add(1)
            .ok_or_else(|| ValueError::new("counter exhausted"))?;
        Self::new(next)
    }
}

impl UsdMicros {
    pub fn zero() -> Self {
        UsdMicros(0)
    }
    /// Checked sum; i64 exhaustion refuses.
    pub fn checked_add(self, other: Self) -> Result<Self, ValueError> {
        let sum = self
            .0
            .checked_add(other.0)
            .ok_or_else(|| ValueError::new("money exhausted"))?;
        Self::new(sum)
    }
}

/// Exact USD micro-dollars from a checked `u64`; the canonical bound refuses.
impl TryFrom<u64> for UsdMicros {
    type Error = ValueError;
    fn try_from(value: u64) -> Result<Self, ValueError> {
        Self::new(value)
    }
}

/// Pico-dollar rate (`1e-12 USD`) as a canonical decimal string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(with = "String")]
pub struct PicoUsd(u128);

impl PicoUsd {
    pub fn get(self) -> u128 {
        self.0
    }
    pub fn new(value: u128) -> Self {
        Self(value)
    }
    pub fn zero() -> Self {
        Self(0)
    }
}

impl TryFrom<String> for PicoUsd {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, ValueError> {
        let bytes = value.as_bytes();
        if bytes.is_empty() || bytes.len() > 39 {
            return Err(ValueError::new("pico-dollar rate is out of bounds"));
        }
        if bytes[0] == b'0' && bytes.len() != 1 {
            return Err(ValueError::new("leading zero in pico-dollar rate"));
        }
        if !bytes.iter().all(u8::is_ascii_digit) {
            return Err(ValueError::new("pico-dollar rate is not ASCII digits"));
        }
        value
            .parse::<u128>()
            .map(PicoUsd)
            .map_err(|_| ValueError::new("pico-dollar rate exceeds u128"))
    }
}

impl fmt::Display for PicoUsd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for PicoUsd {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

/// Trusted epoch milliseconds: a checked safe JSON integer, never a float.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "u64")]
#[schemars(with = "u64")]
pub struct EpochMs(u64);

impl EpochMs {
    pub fn get(self) -> u64 {
        self.0
    }
    pub fn new(value: u64) -> Result<Self, ValueError> {
        if value > MAX_SAFE_MS {
            return Err(ValueError::new(
                "epoch milliseconds exceed the safe integer range",
            ));
        }
        Ok(Self(value))
    }
    pub fn checked_add(self, millis: u64) -> Result<Self, ValueError> {
        let sum = self
            .0
            .checked_add(millis)
            .ok_or_else(|| ValueError::new("epoch milliseconds exhausted"))?;
        Self::new(sum)
    }
    /// The durable nondecreasing clock floor: a lower `now` refuses.
    pub fn at_or_after_floor(self, floor: EpochMs) -> bool {
        self.0 >= floor.0
    }
    pub fn saturating_sub(self, other: EpochMs) -> u64 {
        self.0.saturating_sub(other.0)
    }
}

impl TryFrom<u64> for EpochMs {
    type Error = ValueError;
    fn try_from(value: u64) -> Result<Self, ValueError> {
        Self::new(value)
    }
}

impl fmt::Display for EpochMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for EpochMs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0)
    }
}

/// `sha256:` plus 64 lowercase hex digits.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(with = "String")]
pub struct Digest(String);

impl Digest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Digest {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, ValueError> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(ValueError::new("digest lacks the sha256: prefix"));
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ValueError::new("digest is not 64 lowercase hex digits"));
        }
        Ok(Self(value))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// Bounded printable ASCII identifier (`0x21..=0x7e`, 1..=128 bytes).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(with = "String")]
pub struct Ident(String);

impl Ident {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Ident {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, ValueError> {
        if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
            return Err(ValueError::new("identifier is outside 1..=128 bytes"));
        }
        if !value.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(ValueError::new("identifier is not printable ASCII"));
        }
        Ok(Self(value))
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Ident {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// Canonical lowercase UUID selector (`8-4-4-4-12`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(with = "String")]
pub struct UuidV1(String);

impl UuidV1 {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for UuidV1 {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, ValueError> {
        let bytes = value.as_bytes();
        let shape = [8usize, 4, 4, 4, 12];
        let mut index = 0usize;
        for (part, want) in shape.iter().enumerate() {
            if part > 0 {
                if bytes.get(index) != Some(&b'-') {
                    return Err(ValueError::new("uuid is not canonical lowercase"));
                }
                index += 1;
            }
            for _ in 0..*want {
                match bytes.get(index) {
                    Some(b) if b.is_ascii_digit() || (b'a'..=b'f').contains(b) => index += 1,
                    _ => return Err(ValueError::new("uuid is not canonical lowercase")),
                }
            }
        }
        if index != bytes.len() {
            return Err(ValueError::new("uuid has trailing bytes"));
        }
        if value == "00000000-0000-0000-0000-000000000000" {
            return Err(ValueError::new("the nil uuid is not a selector"));
        }
        Ok(Self(value))
    }
}

impl fmt::Display for UuidV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for UuidV1 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// Stable hosted agent UID: `agt-` plus the 32 lowercase hex UUID digits.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(with = "String")]
pub struct AgentUid(String);

impl AgentUid {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AgentUid {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, ValueError> {
        let Some(hex) = value.strip_prefix("agt-") else {
            return Err(ValueError::new("agent uid lacks the agt- prefix"));
        };
        if hex.len() != 32
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ValueError::new("agent uid is not 32 lowercase hex digits"));
        }
        Ok(Self(value))
    }
}

impl fmt::Display for AgentUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AgentUid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// An explicitly named model id; whitespace and control bytes refuse.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(with = "String")]
pub struct ModelId(String);

impl ModelId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// `provider/id` when the id names a provider namespace.
    pub fn provider(&self) -> Option<&str> {
        self.0.split_once('/').map(|(provider, _)| provider)
    }
}

impl TryFrom<String> for ModelId {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, ValueError> {
        if value.is_empty() || value.len() > MAX_MODEL_BYTES {
            return Err(ValueError::new("model id is outside 1..=200 bytes"));
        }
        if !value.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(ValueError::new("model id is not printable ASCII"));
        }
        Ok(Self(value))
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ModelId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// A fixed-https destination: scheme, host, optional absolute path; no query,
/// fragment, userinfo, whitespace or non-https scheme can be represented.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(with = "String")]
pub struct HttpsUrl(String);

impl HttpsUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HttpsUrl {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, ValueError> {
        if value.is_empty() || value.len() > MAX_HTTPS_URL_BYTES {
            return Err(ValueError::new("https url is outside 1..=2048 bytes"));
        }
        if !value.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(ValueError::new("https url is not printable ASCII"));
        }
        let Some(rest) = value.strip_prefix("https://") else {
            return Err(ValueError::new("url must use https"));
        };
        if rest.contains('?') || rest.contains('#') || rest.contains('@') || rest.contains('\\') {
            return Err(ValueError::new(
                "url carries a query, fragment, userinfo or backslash",
            ));
        }
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, Some(path)),
            None => (rest, None),
        };
        if authority.is_empty() {
            return Err(ValueError::new("url has no host"));
        }
        if authority.starts_with('.') || authority.ends_with('.') || authority.contains("..") {
            return Err(ValueError::new("url host is not canonical"));
        }
        if let Some(path) = path {
            if path.contains("//") {
                return Err(ValueError::new("url path is not canonical"));
            }
        }
        Ok(Self(value))
    }
}

impl fmt::Display for HttpsUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for HttpsUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// `16..=4096` output-token cap; omitted, zero, negative and float refuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "u32")]
#[schemars(with = "u32")]
pub struct MaxOutputTokens(u32);

impl MaxOutputTokens {
    pub fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for MaxOutputTokens {
    type Error = ValueError;
    fn try_from(value: u32) -> Result<Self, ValueError> {
        if !(16..=4096).contains(&value) {
            return Err(ValueError::new("max_output_tokens is outside 16..=4096"));
        }
        Ok(Self(value))
    }
}

impl Serialize for MaxOutputTokens {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PriceError {
    Grammar,
    Overflow,
}

impl fmt::Display for PriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PriceError::Grammar => f.write_str("price is not an exact unsigned decimal"),
            PriceError::Overflow => f.write_str("price arithmetic overflowed its checked bound"),
        }
    }
}
impl std::error::Error for PriceError {}

/// Exact `USD -> picodollars` conversion, one rounding step, upward only.
///
/// Grammar is `0|[1-9][0-9]*` optionally followed by `.` and at least one
/// digit. At most 12 fractional digits are exact; any nonzero digit beyond
/// them rounds the result up by one picodollar. No float, no exponent.
pub fn parse_usd_decimal_to_pico(value: &str) -> Result<PicoUsd, PriceError> {
    if value.is_empty() || value.len() > 64 {
        return Err(PriceError::Grammar);
    }
    let (integer, fraction) = match value.split_once('.') {
        Some((integer, fraction)) => {
            if fraction.is_empty() {
                return Err(PriceError::Grammar);
            }
            (integer, fraction)
        }
        None => (value, ""),
    };
    let integer_bytes = integer.as_bytes();
    if integer_bytes.is_empty() || !integer_bytes.iter().all(u8::is_ascii_digit) {
        return Err(PriceError::Grammar);
    }
    if integer_bytes[0] == b'0' && integer_bytes.len() != 1 {
        return Err(PriceError::Grammar);
    }
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PriceError::Grammar);
    }
    let whole: u128 = integer.parse().map_err(|_| PriceError::Overflow)?;
    let mut pico = whole
        .checked_mul(PICO_PER_USD)
        .ok_or(PriceError::Overflow)?;
    let mut fractional = 0u128;
    let mut place = 100_000_000_000u128;
    let mut round_up = false;
    for (index, digit) in fraction.bytes().enumerate() {
        let digit = u128::from(digit - b'0');
        if index < 12 {
            fractional = fractional
                .checked_add(digit.checked_mul(place).ok_or(PriceError::Overflow)?)
                .ok_or(PriceError::Overflow)?;
            place /= 10;
        } else if digit != 0 {
            round_up = true;
        }
    }
    pico = pico.checked_add(fractional).ok_or(PriceError::Overflow)?;
    if round_up {
        pico = pico.checked_add(1).ok_or(PriceError::Overflow)?;
    }
    Ok(PicoUsd::new(pico))
}

/// Convert an advertised USD-per-million-tokens rate (OpenRouter `max_price`
/// shape) into exact pico-dollars per token, rounding up so the reservation
/// is never under-stated. The conversion happens exactly once.
pub fn parse_usd_per_million_to_pico_per_token(value: &str) -> Result<PicoUsd, PriceError> {
    let per_million = parse_usd_decimal_to_pico(value)?;
    Ok(PicoUsd::new(per_million.get().div_ceil(1_000_000)))
}

/// Reserve the full input bound plus the enforced output cap:
/// `ceil((input*prompt + output*completion + request) / 1e6)` USD micros.
pub fn reserve_usd_micros(
    input_bound: u64,
    prompt_pico_per_token: PicoUsd,
    output_bound: u64,
    completion_pico_per_token: PicoUsd,
    request_pico: PicoUsd,
) -> Result<UsdMicros, PriceError> {
    let input = (input_bound as u128)
        .checked_mul(prompt_pico_per_token.get())
        .ok_or(PriceError::Overflow)?;
    let output = (output_bound as u128)
        .checked_mul(completion_pico_per_token.get())
        .ok_or(PriceError::Overflow)?;
    let total = input
        .checked_add(output)
        .and_then(|total| total.checked_add(request_pico.get()))
        .ok_or(PriceError::Overflow)?;
    let micros = total.div_ceil(PICO_PER_MICRO_USD);
    let micros = u64::try_from(micros).map_err(|_| PriceError::Overflow)?;
    UsdMicros::new(micros).map_err(|_| PriceError::Overflow)
}
