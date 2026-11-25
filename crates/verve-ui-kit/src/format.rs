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
}
