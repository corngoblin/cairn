//! Small formatting helpers, shared by the CLI and the window so both describe the
//! same numbers the same way.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `1.4 GB`, `212.9 MB`, `940 B`. Powers of 1024, because that is what the user's
/// file manager will show for the same folder.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `3m 12s`, `48s`, `1h 04m`.
pub fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// A UTC `YYYY-MM-DD HH:MM` for a unix timestamp.
///
/// Civil-from-days rather than a date crate: the app needs exactly one date format
/// in exactly two places, and a dependency that has to be present in the flatpak
/// build for that is not a trade worth making.
pub fn format_unix(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs_of_day = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a calendar date.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_file_manager_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(940), "940 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(45 * 1024 * 1024 * 1024), "45.0 GB");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format_duration(9), "9s");
        assert_eq!(format_duration(192), "3m 12s");
        assert_eq!(format_duration(3840), "1h 04m");
    }

    #[test]
    fn unix_times_format_as_utc_dates() {
        assert_eq!(format_unix(0), "1970-01-01 00:00");
        assert_eq!(format_unix(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(format_unix(1_767_225_600), "2026-01-01 00:00");
    }
}
