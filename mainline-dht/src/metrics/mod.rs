use metrics::{counter, gauge};

pub fn inc_msg_in() {
    counter!("dht.messages_in_total").increment(1);
}
pub fn inc_msg_out() {
    counter!("dht.messages_out_total").increment(1);
}
pub fn inc_msg_dropped() {
    counter!("dht.messages_dropped_total").increment(1);
}
pub fn inc_msg_out_dropped() {
    counter!("dht.messages_out_dropped_total").increment(1);
}

pub fn inc_bytes_in(n: usize) {
    counter!("dht.bytes_in_total").increment(n as u64);
}
pub fn inc_bytes_out(n: usize) {
    counter!("dht.bytes_out_total").increment(n as u64);
}

pub fn set_nodes(n: usize) {
    gauge!("dht.nodes").set(n as f64);
}
pub fn set_torrents(n: usize) {
    gauge!("dht.torrents").set(n as f64);
}
pub fn set_peers(n: usize) {
    gauge!("dht.peers").set(n as f64);
}
