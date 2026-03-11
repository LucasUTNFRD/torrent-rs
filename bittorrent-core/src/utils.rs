pub fn format_speed(bits_per_second: u64) -> String {
    if bits_per_second < 1_000 {
        format!("{} bps", bits_per_second)
    } else if bits_per_second < 1_000_000 {
        format!("{:.1} Kbps", bits_per_second as f64 / 1_000.0)
    } else if bits_per_second < 1_000_000_000 {
        format!("{:.2} Mbps", bits_per_second as f64 / 1_000_000.0)
    } else {
        format!("{:.2} Gbps", bits_per_second as f64 / 1_000_000_000.0)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;

    if bytes < KB {
        format!("{} B", bytes)
    } else if bytes < MB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes < TB {
        format!("{:.2} GB", bytes as f64 / TB as f64)
    } else {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    }
}
