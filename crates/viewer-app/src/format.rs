//! 상태 표시줄, 찾기 막대, 파일 구조가 함께 쓰는 수와 크기의 표시 형식.

/// 세 자리마다 쉼표를 넣는다.
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

/// 1024 단위로 줄여 소수 첫째 자리까지 표시한다.
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

/// 압축 전 크기가 압축 후 크기의 몇 배인지. 압축 후 크기가 0이면 `-`다.
pub fn format_ratio(uncompressed: u64, compressed: u64) -> String {
    if compressed == 0 {
        return "-".into();
    }
    format!("{:.1}배", uncompressed as f64 / compressed as f64)
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
        assert_eq!(format_ratio(300, 100), "3.0배");
        assert_eq!(format_ratio(10, 0), "-");
    }
}
