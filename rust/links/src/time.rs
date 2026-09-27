//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The handful of date and duration formats the registry's converters speak (README §4:
//! `iso8601_ms`, `seconds_ms`, `rfc3339`, `unix_s`). Hand-written because they are tiny and the
//! crate has no other use for a date library.

/// A parsed RFC 3339 date-time: the normalized text (`T` separator, upper-case `Z`) and the
/// instant it names, in milliseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DateTime {
    pub text: String,
    pub unix_ms: i64,
}

fn digits(s: &[u8]) -> Option<i64> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    s.iter().try_fold(0i64, |acc, d| {
        acc.checked_mul(10)?.checked_add(i64::from(d - b'0'))
    })
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`; a space is accepted for `T`. A time without an offset
/// is not RFC 3339 and is rejected rather than guessed.
pub(crate) fn parse_rfc3339(input: &str) -> Option<DateTime> {
    let s = input.trim().as_bytes();
    if s.len() < 20 || s[4] != b'-' || s[7] != b'-' || s[13] != b':' || s[16] != b':' {
        return None;
    }
    if !matches!(s[10], b'T' | b't' | b' ') {
        return None;
    }
    let (y, mo, d) = (digits(&s[0..4])?, digits(&s[5..7])?, digits(&s[8..10])?);
    let (h, mi, sec) = (
        digits(&s[11..13])?,
        digits(&s[14..16])?,
        digits(&s[17..19])?,
    );
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 {
        return None;
    }
    if sec > 60 {
        return None; // 60 is a leap second
    }
    let mut i = 19;
    let mut millis = 0i64;
    if s.get(i) == Some(&b'.') {
        let start = i + 1;
        let mut end = start;
        while end < s.len() && s[end].is_ascii_digit() {
            end += 1;
        }
        if end == start {
            return None;
        }
        let frac = &s[start..end];
        let mut ms = [b'0'; 3];
        for (slot, digit) in ms.iter_mut().zip(frac) {
            *slot = *digit;
        }
        millis = digits(&ms)?;
        i = end;
    }
    let offset_minutes = match s.get(i) {
        Some(b'Z' | b'z') if i + 1 == s.len() => 0,
        Some(sign @ (b'+' | b'-')) if i + 6 == s.len() && s[i + 3] == b':' => {
            let oh = digits(&s[i + 1..i + 3])?;
            let om = digits(&s[i + 4..i + 6])?;
            if oh > 23 || om > 59 {
                return None;
            }
            let total = oh * 60 + om;
            if *sign == b'-' { -total } else { total }
        }
        _ => return None,
    };
    let seconds = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec.min(59);
    let unix_ms = (seconds - offset_minutes * 60) * 1000 + millis;
    let mut text = String::from_utf8(s.to_vec()).ok()?;
    text.replace_range(10..11, "T");
    if text.ends_with('z') {
        text.pop();
        text.push('Z');
    }
    Some(DateTime { text, unix_ms })
}

/// Unix seconds → `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn unix_seconds_to_rfc3339(seconds: i64) -> Option<String> {
    // Keep to four-digit years so the output stays RFC 3339.
    if !(0..=253_402_300_799).contains(&seconds) {
        return None;
    }
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}

/// ISO 8601 duration → milliseconds: `P[nD]T[nH][nM][n[.n]S]`, e.g. Bilibili's `PT00H08M19S`.
/// Years, months and weeks have no fixed length and are rejected.
pub(crate) fn iso8601_duration_ms(input: &str) -> Option<u64> {
    let s = input.trim();
    let rest = s.strip_prefix('P').or_else(|| s.strip_prefix('p'))?;
    let (date, time) = match rest.find(['T', 't']) {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    let mut total: f64 = 0.0;
    let mut any = false;
    let mut take = |part: &str, units: &[(char, f64)]| -> Option<()> {
        let mut number = String::new();
        let mut last_unit = 0;
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' || c == ',' {
                number.push(if c == ',' { '.' } else { c });
                continue;
            }
            let unit = c.to_ascii_uppercase();
            let idx = units.iter().position(|(u, _)| *u == unit)?;
            if idx < last_unit || number.is_empty() {
                return None;
            }
            last_unit = idx + 1;
            let value: f64 = number.parse().ok()?;
            total += value * units[idx].1;
            any = true;
            number.clear();
        }
        number.is_empty().then_some(())
    };
    take(date, &[('D', 86_400_000.0)])?;
    if let Some(time) = time {
        if time.is_empty() {
            return None;
        }
        take(time, &[('H', 3_600_000.0), ('M', 60_000.0), ('S', 1000.0)])?;
    }
    if !any || !total.is_finite() || total < 0.0 || total > 1e15 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(total.round() as u64) // bounded above
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_bilibili_and_offsets() {
        let dt = parse_rfc3339("2026-09-22T09:25:33.000Z").expect("valid");
        assert_eq!(dt.unix_ms, 1_790_069_133_000);
        assert_eq!(dt.text, "2026-09-22T09:25:33.000Z");
        let local = parse_rfc3339("2026-09-22 17:25:33+08:00").expect("valid");
        assert_eq!(local.unix_ms, dt.unix_ms);
        assert_eq!(local.text, "2026-09-22T17:25:33+08:00");
        assert!(parse_rfc3339("2026-09-22T09:25:33").is_none(), "no offset");
        assert!(parse_rfc3339("2026-02-30T00:00:00Z").is_none());
        assert!(parse_rfc3339("yesterday").is_none());
    }

    #[test]
    fn unix_seconds() {
        assert_eq!(
            unix_seconds_to_rfc3339(1_790_069_133).as_deref(),
            Some("2026-09-22T09:25:33Z")
        );
        assert_eq!(
            unix_seconds_to_rfc3339(0).as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert!(unix_seconds_to_rfc3339(-1).is_none());
    }

    #[test]
    fn durations() {
        assert_eq!(iso8601_duration_ms("PT00H08M19S"), Some(499_000));
        assert_eq!(iso8601_duration_ms("PT3M"), Some(180_000));
        assert_eq!(iso8601_duration_ms("P1DT1S"), Some(86_401_000));
        assert_eq!(iso8601_duration_ms("PT1.5S"), Some(1500));
        assert_eq!(
            iso8601_duration_ms("P1M"),
            None,
            "months have no fixed length"
        );
        assert_eq!(iso8601_duration_ms("PT"), None);
        assert_eq!(iso8601_duration_ms("PT5M3H"), None, "units out of order");
        assert_eq!(iso8601_duration_ms("8:19"), None);
    }
}
