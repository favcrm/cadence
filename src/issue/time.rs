//! Minimal UTC clock helpers — enough for ISO `created`/`at` fields and
//! the `YYYYMMDDTHHMMSSZ` comment filenames, without a date dependency.

use std::time::{SystemTime, UNIX_EPOCH};

/// Days-from-civil inverse (Howard Hinnant's algorithm): epoch days →
/// (year, month, day) in the proleptic Gregorian calendar.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `(year, month, day, hour, minute, second)` for an epoch second.
pub fn utc_parts(epoch: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (
        y,
        m,
        d,
        (secs / 3600) as u32,
        ((secs % 3600) / 60) as u32,
        (secs % 60) as u32,
    )
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// `2026-09-17T17:24:00Z`
pub fn iso(epoch: i64) -> String {
    let (y, m, d, hh, mm, ss) = utc_parts(epoch);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// `20260917T172400Z` — the UTC-basic comment filename prefix.
pub fn basic(epoch: i64) -> String {
    let (y, m, d, hh, mm, ss) = utc_parts(epoch);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z")
}

/// (year, month, day) → days since epoch — the forward half of
/// `civil_from_days` (same Howard Hinnant algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = ((m + 9) % 12) as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// `iso`'s inverse — `YYYY-MM-DDTHH:MM:SSZ` → epoch seconds. Returns
/// `None` for anything that is not exactly that shape; a retro reports
/// a missing timing as `unknown`, never a misparsed one.
pub fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |from: usize, to: usize| s[from..to].parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || !(0..=23).contains(&hh)
        || !(0..=59).contains(&mm)
        || !(0..=60).contains(&ss)
    {
        return None;
    }
    let epoch = days_from_civil(y, mo as u32, d as u32) * 86_400 + hh * 3600 + mm * 60 + ss;
    // Round-trip through `iso` rejects non-existent dates — 2026-02-31
    // would otherwise normalise forward into March.
    (iso(epoch) == s).then_some(epoch)
}

/// Parse a note filename's leading `YYYYMMDD-HHMMSS` into ISO UTC —
/// the chain sort key for notes and the `at` shown in activity.
pub fn note_name_to_iso(name: &str) -> Option<String> {
    if name.len() < 16 || name.as_bytes()[8] != b'-' {
        return None;
    }
    let (date, time) = name.split_at(8);
    let time = &time[1..];
    if !(date.bytes().all(|b| b.is_ascii_digit())
        && time.len() >= 6
        && time[..6].bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}Z",
        &date[..4],
        &date[4..6],
        &date[6..8],
        &time[..2],
        &time[2..4],
        &time[4..6]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_to_iso() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        // 2026-09-17T17:24:00Z == epoch 1789665840
        assert_eq!(iso(1_789_665_840), "2026-09-17T17:24:00Z");
        assert_eq!(basic(1_789_665_840), "20260917T172400Z");
        // Leap year boundary: 2024-02-29T23:59:59Z == 1709251199
        assert_eq!(iso(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(iso(1_709_251_200), "2024-03-01T00:00:00Z");
    }

    #[test]
    fn parse_iso_round_trip() {
        // `iso`'s exact inverse, including leap-year boundaries.
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2026-09-17T17:24:00Z"), Some(1_789_665_840));
        assert_eq!(parse_iso("2024-02-29T23:59:59Z"), Some(1_709_251_199));
        // Malformed shapes.
        for bad in [
            "",
            "2026-09-17",
            "2026-09-17T17:24:00",  // no Z
            "2026-09-17 17:24:00Z", // space not T
            "2026-9-17T17:24:00Z",  // not zero-padded
            "2026-13-01T00:00:00Z", // month 13
            "2026-02-31T00:00:00Z", // non-existent date
            "2026-09-17T24:00:00Z", // hour 24
            "2026-09-17T17:24:60Z", // leap second rejected
            "abcd-09-17T17:24:00Z",
        ] {
            assert_eq!(parse_iso(bad), None, "{bad}");
        }
    }

    #[test]
    fn note_name_parse() {
        assert_eq!(
            note_name_to_iso("20260917-172400-60747-cadence-board-i1-kickoff.md"),
            Some("2026-09-17T17:24:00Z".to_string())
        );
        assert_eq!(note_name_to_iso("random.md"), None);
        assert_eq!(note_name_to_iso("2026-09-17-x.md"), None);
    }
}
