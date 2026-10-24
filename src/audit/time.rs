//! Time.

use super::*;

/// `24h`, `7d`, `2w`, `YYYY-MM-DD`, or a unix epoch → epoch seconds.
pub(super) fn parse_since(s: &str) -> Result<f64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix('h') {
        (n, 3600.0)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86400.0)
    } else if let Some(n) = s.strip_suffix('w') {
        (n, 604800.0)
    } else {
        (s, 0.0)
    };
    if mult > 0.0 {
        let n: f64 = num
            .parse()
            .map_err(|_| Error::rejected(format!("--since '{s}': bad duration")))?;
        return Ok(crate::issue::time::now_epoch() as f64 - n * mult);
    }
    if let Ok(epoch) = s.parse::<f64>() {
        return Ok(epoch);
    }
    parse_iso(s).map(|t| t as f64).ok_or_else(|| {
        Error::rejected(format!(
            "--since '{s}': expected 24h, 7d, YYYY-MM-DD or epoch"
        ))
    })
}

/// `YYYY-MM-DD` or RFC-3339 (`2026-09-20T10:30:27Z`) → epoch seconds.
pub(super) fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T').unwrap_or((s, "00:00:00"));
    let mut dp = date.split('-');
    let (y, m, d): (i64, i64, i64) = (
        dp.next()?.parse().ok()?,
        dp.next()?.parse().ok()?,
        dp.next()?.parse().ok()?,
    );
    let mut tp = time.split(':');
    let (hh, mm, ss): (i64, i64, i64) = (
        tp.next().unwrap_or("0").parse().ok()?,
        tp.next().unwrap_or("0").parse().ok()?,
        tp.next().unwrap_or("0").split('.').next()?.parse().ok()?,
    );
    // Days since epoch (Howard Hinnant's civil-from-days inverse).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}
