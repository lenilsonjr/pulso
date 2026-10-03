//! The `end` timestamps `/latest` compares.

/// Microseconds since the Unix epoch for `YYYY-MM-DD` + `T`, `t` or a space +
/// `HH:MM:SS[.f+]` + a zone (`Z`, `±HH:MM`, `±HHMM` or `±HH`), or `None`.
///
/// Instants are compared in absolute time because the offsets in the archive
/// differ. A timestamp without a zone has no absolute time and is rejected.
pub fn parse_instant(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let year = number(b, 0, 4)?;
    let month = after(b, 4, b'-', |b| number(b, 5, 2))?;
    let day = after(b, 7, b'-', |b| number(b, 8, 2))?;
    if !matches!(b.get(10), Some(b'T' | b't' | b' ')) {
        return None;
    }
    let hour = number(b, 11, 2)?;
    let minute = after(b, 13, b':', |b| number(b, 14, 2))?;
    let second = after(b, 16, b':', |b| number(b, 17, 2))?;

    let mut at = 19;
    let mut micros = 0;
    if b.get(at) == Some(&b'.') {
        let digits = b[at + 1..]
            .iter()
            .take_while(|d| d.is_ascii_digit())
            .count();
        if digits == 0 {
            return None;
        }
        let kept = &b[at + 1..at + 1 + digits.min(6)];
        micros = kept.iter().fold(0, |acc, d| acc * 10 + i64::from(d - b'0'));
        micros *= 10_i64.pow(6 - kept.len() as u32);
        at += 1 + digits;
    }
    let offset = zone(&b[at..])?;

    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    Some((seconds - offset) * 1_000_000 + micros)
}

fn number(b: &[u8], at: usize, len: usize) -> Option<i64> {
    b.get(at..at + len)?.iter().try_fold(0, |acc, d| {
        d.is_ascii_digit().then(|| acc * 10 + i64::from(d - b'0'))
    })
}

fn after(
    b: &[u8],
    at: usize,
    separator: u8,
    then: impl FnOnce(&[u8]) -> Option<i64>,
) -> Option<i64> {
    if b.get(at) == Some(&separator) {
        then(b)
    } else {
        None
    }
}

fn zone(z: &[u8]) -> Option<i64> {
    let (sign, rest) = match z {
        [b'Z'] => return Some(0),
        [b'+', rest @ ..] => (1, rest),
        [b'-', rest @ ..] => (-1, rest),
        _ => return None,
    };
    let (hours, minutes) = match rest {
        [_, _] => (number(rest, 0, 2)?, 0),
        [_, _, _, _] => (number(rest, 0, 2)?, number(rest, 2, 2)?),
        [_, _, b':', _, _] => (number(rest, 0, 2)?, number(rest, 3, 2)?),
        _ => return None,
    };
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 3600 + minutes * 60))
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the given proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(text: &str) -> i64 {
        parse_instant(text).unwrap_or_else(|| panic!("{text} should parse")) / 1_000_000
    }

    #[test]
    fn the_epoch_is_zero() {
        assert_eq!(secs("1970-01-01T00:00:00Z"), 0);
        assert_eq!(secs("1970-01-01T01:00:00+01:00"), 0);
        assert_eq!(secs("1969-12-31T19:00:00-05:00"), 0);
    }

    #[test]
    fn known_instants() {
        assert_eq!(secs("2026-07-06T09:00:00Z"), 1_783_328_400);
        assert_eq!(secs("2000-02-29T12:00:00Z"), 951_825_600);
        assert_eq!(secs("1900-03-01T00:00:00Z"), -2_203_891_200);
        assert_eq!(secs("0001-01-01T00:00:00Z"), -62_135_596_800);
        assert_eq!(secs("9999-12-31T23:59:59Z"), 253_402_300_799);
    }

    #[test]
    fn instants_compare_in_absolute_time_not_as_text() {
        // 05:00-04:00 is 09:00Z, later than 09:03+01:00 (08:03Z), though it sorts first as text.
        let a = parse_instant("2026-07-06T05:00:00-04:00").unwrap();
        let b = parse_instant("2026-07-06T09:03:00+01:00").unwrap();
        assert!("2026-07-06T05:00:00-04:00" < "2026-07-06T09:03:00+01:00");
        assert!(a > b);
        // 10:00Z beats 05:00-04:00 (09:00Z).
        assert!(parse_instant("2026-07-06T10:00:00Z").unwrap() > a);
    }

    #[test]
    fn the_same_instant_in_other_spellings_is_equal() {
        let z = parse_instant("2026-07-06T09:00:00Z").unwrap();
        for same in [
            "2026-07-06T10:00:00+01:00",
            "2026-07-06T10:00:00+0100",
            "2026-07-06T10:00:00+01",
            "2026-07-06 09:00:00Z",
            "2026-07-06t09:00:00Z",
            "2026-07-06T09:00:00.000Z",
            "2026-07-06T09:00:00.0000000009Z",
            "2026-07-06T04:30:00-04:30",
        ] {
            assert_eq!(parse_instant(same), Some(z), "{same}");
        }
    }

    #[test]
    fn fractions_keep_microseconds_and_drop_the_rest() {
        let base = parse_instant("2026-07-06T09:00:00Z").unwrap();
        assert_eq!(
            parse_instant("2026-07-06T09:00:00.5Z"),
            Some(base + 500_000)
        );
        assert_eq!(
            parse_instant("2026-07-06T09:00:00.123Z"),
            Some(base + 123_000)
        );
        assert_eq!(
            parse_instant("2026-07-06T09:00:00.123456Z"),
            Some(base + 123_456)
        );
        assert_eq!(
            parse_instant("2026-07-06T09:00:00.1234569Z"),
            Some(base + 123_456)
        );
    }

    #[test]
    fn leap_days_are_checked() {
        assert!(parse_instant("2024-02-29T00:00:00Z").is_some());
        assert!(parse_instant("2000-02-29T00:00:00Z").is_some());
        assert!(parse_instant("2026-02-29T00:00:00Z").is_none());
        assert!(parse_instant("1900-02-29T00:00:00Z").is_none());
        assert!(parse_instant("2026-04-31T00:00:00Z").is_none());
    }

    #[test]
    fn rejects_what_has_no_absolute_time_or_is_not_a_time() {
        for bad in [
            "",
            "2026-07-06",
            "2026-07-06T09:00:00",
            "2026-07-06T09:00",
            "2026-07-06T09:00:00z",
            "2026-13-06T09:00:00Z",
            "2026-00-06T09:00:00Z",
            "2026-07-00T09:00:00Z",
            "2026-07-32T09:00:00Z",
            "0000-07-06T09:00:00Z",
            "2026-07-06T24:00:00Z",
            "2026-07-06T09:60:00Z",
            "2026-07-06T09:00:60Z",
            "2026-07-06T09:00:00+24:00",
            "2026-07-06T09:00:00+01:60",
            "2026-07-06T09:00:00+1:00",
            "2026-07-06T09:00:00+01:0",
            "2026-07-06T09:00:00.Z",
            "2026-07-06T09:00:00Z ",
            "2026-07-06X09:00:00Z",
            "20260706T090000Z",
            "2026-7-6T09:00:00Z",
            "+026-07-06T09:00:00Z",
            "２０２６-07-06T09:00:00Z",
        ] {
            assert_eq!(parse_instant(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn does_not_panic_on_arbitrary_text() {
        for text in [
            "é",
            "2026-07-06T09:00:00é",
            "2026-07-06T09:00:00.é",
            "日本語日本語日本語日本語日本語",
            "\u{0}",
        ] {
            assert_eq!(parse_instant(text), None);
        }
    }
}
