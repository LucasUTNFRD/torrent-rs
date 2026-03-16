use metrics::{counter, gauge, histogram};

// disk metrics
pub fn disk_bytes_written(n: u64) {
    counter!("disk.bytes_written").increment(n);
}

pub fn disky_bytes_read(n: u64) {
    counter!("disk.blocks_read").increment(n);
}

pub fn disk_read_time_ms(n: u64) {
    histogram!("disk.read_time_ms").record(n as f64);
}

pub fn disk_write_time_ms(n: u64) {
    histogram!("disk.write_time_ms").record(n as f64);
}

// bt protocol
pub fn piece_requests() {
    counter!("bt.piece_requests").increment(1);
}

pub fn piece_passed() {
    counter!("bt.piece_passed").increment(1);
}

pub fn piece_failed() {
    counter!("bt.piece_failed").increment(1);
}

// net
pub fn peers_connected(n: f64) {
    gauge!("net.peers_connected").set(n);
}

pub fn connection_attempts() {
    counter!("net.connection_attempts").increment(1);
}
