use std::time::Duration;

/// Decimal units, matching Finder: three significant digits, so "146 GB", "53.6 GB", "8.28 GB".
pub fn bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return if bytes == 1 {
            "1 byte".into()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 999.5 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    let decimals = if value >= 99.95 {
        0
    } else if value >= 9.995 {
        1
    } else {
        2
    };
    format!("{value:.decimals$} {}", UNITS[unit])
}

/// `bytes`, prefixed with "≥" while the total can still grow.
pub fn size(bytes_: u64, settled: bool) -> String {
    if settled {
        bytes(bytes_)
    } else {
        format!("≥ {}", bytes(bytes_))
    }
}

/// Thousands separated with commas: "4,450,390".
pub fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

pub fn items(value: u64) -> String {
    if value == 1 {
        "1 item".into()
    } else {
        format!("{} items", count(value))
    }
}

pub fn duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 60.0 {
        format!("{seconds:.1} s")
    } else {
        format!(
            "{}:{:02} min",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_with_three_significant_digits() {
        assert_eq!(bytes(0), "0 bytes");
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(999), "999 bytes");
        assert_eq!(bytes(1_000), "1.00 KB");
        assert_eq!(bytes(8_280_000_000), "8.28 GB");
        assert_eq!(bytes(53_630_000_000), "53.6 GB");
        assert_eq!(bytes(146_790_000_000), "147 GB");
        assert_eq!(bytes(999_600_000), "1.00 GB");
        assert_eq!(bytes(2_500_000_000_000), "2.50 TB");
    }

    #[test]
    fn marks_partial_sizes() {
        assert_eq!(size(12_400_000_000, false), "≥ 12.4 GB");
        assert_eq!(size(12_400_000_000, true), "12.4 GB");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(4_450_390), "4,450,390");
        assert_eq!(items(1), "1 item");
        assert_eq!(items(12_345), "12,345 items");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(Duration::from_millis(22_640)), "22.6 s");
        assert_eq!(duration(Duration::from_secs(95)), "1:35 min");
    }
}
