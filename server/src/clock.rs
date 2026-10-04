use std::fmt::Write;

/// The server's local time in the form `receivedAt` has always had:
/// `YYYY-MM-DDTHH:MM:SS+HH:MM`.
pub fn local_timestamp() -> String {
    // SAFETY: `time` accepts a null pointer. `localtime_r` and `gmtime_r` only
    // write into the `tm` handed to them and take no other mutable state.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            libc::gmtime_r(&now, &mut tm);
            tm.tm_gmtoff = 0;
        }
        tm
    };
    format_stamp(
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        (tm.tm_hour, tm.tm_min, tm.tm_sec),
        #[allow(clippy::useless_conversion)] // `c_long` is 32 bits on some targets
        i64::from(tm.tm_gmtoff),
    )
}

fn format_stamp(
    year: i32,
    month: i32,
    day: i32,
    time: (i32, i32, i32),
    offset_secs: i64,
) -> String {
    let (hour, minute, second) = time;
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let offset = offset_secs.abs();
    let mut text = format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{sign}{:02}:{:02}",
        offset / 3600,
        offset % 3600 / 60
    );
    if offset % 60 != 0 {
        write!(text, ":{:02}", offset % 60).expect("writing to a String cannot fail");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_python_isoformat() {
        assert_eq!(
            format_stamp(2026, 10, 3, (22, 53, 10), 0),
            "2026-10-03T22:53:10+00:00"
        );
        assert_eq!(
            format_stamp(2026, 7, 6, (9, 5, 3), 3600),
            "2026-07-06T09:05:03+01:00"
        );
        assert_eq!(
            format_stamp(2026, 7, 6, (9, 5, 3), -(4 * 3600 + 1800)),
            "2026-07-06T09:05:03-04:30"
        );
        assert_eq!(
            format_stamp(2026, 7, 6, (9, 5, 3), 36 * 60 + 45),
            "2026-07-06T09:05:03+00:36:45"
        );
    }

    #[test]
    fn the_local_timestamp_is_an_iso_stamp_with_an_offset() {
        let stamp = local_timestamp();
        assert_eq!(stamp.len(), 25, "{stamp}");
        assert!(crate::isotime::parse_instant(&stamp).is_some(), "{stamp}");
    }
}
