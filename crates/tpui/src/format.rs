//! Number and size formats shared by the status bar, find bar and file structure view.

/// Inserts a comma every three digits.
pub fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

/// Count with comma grouping and the singular or plural noun, e.g. `1 row`, `3,224 rows`.
pub fn format_quantity(count: u64, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{} {noun}", format_count(count))
}

/// Scales by 1024 and shows one decimal.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// How many times larger the uncompressed size is than the compressed size. `-` if the compressed size is 0.
pub fn format_ratio(uncompressed: u64, compressed: u64) -> String {
    if compressed == 0 {
        return "-".into();
    }
    format!("{:.1}×", uncompressed as f64 / compressed as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_counts_with_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn formats_byte_sizes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn formats_compression_ratio() {
        assert_eq!(format_ratio(300, 100), "3.0×");
        assert_eq!(format_quantity(1, "row", "rows"), "1 row");
        assert_eq!(format_quantity(3224, "row", "rows"), "3,224 rows");
        assert_eq!(format_ratio(10, 0), "-");
    }
}
