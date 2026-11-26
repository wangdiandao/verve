//! Pure formatting helpers for byte counts, throughput rates, and durations.

const SIZE_UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// Formats a byte count using binary (`1024`) units (e.g. `"0 B"`, `"512 B"`, `"1.5 KB"`, `"24.8 MB"`, `"3.20 GB"`).
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit_index = 0usize;
    while value >= 1024.0 && unit_index + 1 < SIZE_UNITS.len() {
        value /= 1024.0;
        unit_index += 1;
    }

    let unit = SIZE_UNITS[unit_index];
    if unit_index <= 2 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.2} {unit}")
    }
}

/// Formats a throughput rate in bytes per second (e.g. `"0 B/s"`, `"12.4 KB/s"`, `"8.50 GB/s"`).
pub fn format_throughput_bps(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// Formats an elapsed duration in seconds as `HH:MM:SS` (e.g. `"00:04:12"`, `"12:30:05"`).
pub fn format_duration_hms(secs: u64) -> String {
    let hours = secs / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// Formats a Unix timestamp in seconds as a UTC calendar date (`YYYY-MM-DD`).
pub fn format_unix_date(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Formats a Unix timestamp in seconds as a UTC date and time (`YYYY-MM-DD HH:MM`).
pub fn format_unix_datetime(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Howard Hinnant's Euclidean civil-date algorithm converting days since `1970-01-01`
/// into `(year, month, day)` in the proleptic Gregorian calendar.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_byte_counts_across_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(26_004_685), "24.8 MB");
        assert_eq!(format_bytes(3_435_973_837), "3.20 GB");
        assert_eq!(format_bytes(2 * 1024_u64.pow(4)), "2.00 TB");
    }

    #[test]
    fn formats_throughput_rates() {
        assert_eq!(format_throughput_bps(0), "0 B/s");
        assert_eq!(format_throughput_bps(768), "768 B/s");
        assert_eq!(format_throughput_bps(12_698), "12.4 KB/s");
        assert_eq!(format_throughput_bps(8_912_896), "8.5 MB/s");
        assert_eq!(format_throughput_bps(2_147_483_648), "2.00 GB/s");
    }

    #[test]
    fn formats_durations_hms() {
        assert_eq!(format_duration_hms(0), "00:00:00");
        assert_eq!(format_duration_hms(59), "00:00:59");
        assert_eq!(format_duration_hms(252), "00:04:12");
        assert_eq!(format_duration_hms(45_005), "12:30:05");
        assert_eq!(format_duration_hms(360_000), "100:00:00");
    }

    #[test]
    fn formats_unix_dates_and_datetimes_utc() {
        assert_eq!(format_unix_date(0), "1970-01-01");
        assert_eq!(format_unix_datetime(0), "1970-01-01 00:00");

        // 2000-02-29 12:34:56 UTC (century leap year)
        assert_eq!(format_unix_date(951_827_696), "2000-02-29");
        assert_eq!(format_unix_datetime(951_827_696), "2000-02-29 12:34");

        // 2024-02-29 23:59:59 UTC (modern leap day)
        assert_eq!(format_unix_date(1_709_251_199), "2024-02-29");
        assert_eq!(format_unix_datetime(1_709_251_199), "2024-02-29 23:59");

        // 2026-11-26 08:05:00 UTC
        assert_eq!(format_unix_date(1_795_680_300), "2026-11-26");
        assert_eq!(format_unix_datetime(1_795_680_300), "2026-11-26 08:05");
    }
}
