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

pub fn incoming_connections() {
    counter!("net.incoming_connections").increment(1);
}

pub fn on_read_counter() {
    counter!("net.on_read_counter").increment(1);
}

pub fn on_write_counter() {
    counter!("net.on_write_counter").increment(1);
}

//traffic
pub fn inc_sent_payload(bytes: u64) {
    counter!("traffic.sent_payload_bytes").increment(bytes);
}
pub fn inc_recv_payload(bytes: u64) {
    counter!("traffic.recv_payload_bytes").increment(bytes);
}
pub fn inc_sent_total(bytes: u64) {
    counter!("traffic.sent_bytes").increment(bytes);
}
pub fn inc_recv_total(bytes: u64) {
    counter!("traffic.recv_bytes").increment(bytes);
}
