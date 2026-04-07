pub fn format_speed(bytes_per_second: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes_per_second < KB {
        format!("{} B/s", bytes_per_second)
    } else if bytes_per_second < MB {
        format!("{:.1} KiB/s", bytes_per_second as f64 / KB as f64)
    } else if bytes_per_second < GB {
        format!("{:.2} MiB/s", bytes_per_second as f64 / MB as f64)
    } else {
        format!("{:.2} GiB/s", bytes_per_second as f64 / GB as f64)
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
