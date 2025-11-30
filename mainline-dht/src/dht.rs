use std::net::SocketAddr;

use crate::node_id::NodeId;

// ---- SERVER ----

#[derive(Debug, Clone, Copy)]
pub struct Server {
    node_id: NodeId,
    listen_addr: SocketAddr,
}

pub struct Node {}

impl Server {
    pub fn new(listen_addr: SocketAddr) -> Self {
        let mut node_id = NodeId::generate_random();
        node_id.secure_node_id(&listen_addr.ip());

        Self {
            node_id,
            listen_addr,
        }
    }
}
