//! UTC timestamps for diagnostics, session records and catalog manifests.

use std::fmt::Write;

/// Format Unix seconds as an RFC 3339 UTC timestamp.
pub fn format_unix_seconds(seconds: u64) -> String {
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let day_seconds = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3_600,
        (day_seconds % 3_600) / 60,
        day_seconds % 60
    )
}

/// Format Unix milliseconds as an RFC 3339 UTC timestamp with fixed precision.
pub fn format_unix_millis(millis: u64) -> String {
    let mut timestamp = format_unix_seconds(millis / 1000);
    timestamp.pop(); // Insert the fractional seconds before the UTC suffix.
    write!(timestamp, ".{:03}Z", millis % 1000).expect("writing to a String cannot fail");
    timestamp
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
pub(super) fn civil_from_days(days_since_epoch: u64) -> (i64, u32, u32) {
    let z = days_since_epoch as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_last_day_of_a_gregorian_cycle() {
        assert_eq!(format_unix_seconds(13_574_649_599), "2400-02-29T23:59:59Z");
        assert_eq!(
            format_unix_millis(13_574_649_600_042),
            "2400-03-01T00:00:00.042Z"
        );
    }
}
