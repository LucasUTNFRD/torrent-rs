use bittorrent_common::types::InfoHash;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

#[derive(Default)]
pub struct PeerStore {
    inner: HashMap<InfoHash, HashSet<SocketAddr>>,
}

impl PeerStore {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn add_peer(&mut self, info_hash: InfoHash, addr: SocketAddr) {
        self.inner.entry(info_hash).or_default().insert(addr);
    }

    pub fn add_peers(&mut self, info_hash: InfoHash, addrs: &[SocketAddr]) {
        let set = self.inner.entry(info_hash).or_default();
        set.extend(addrs.iter().copied());
    }

    pub fn get_peers(&self, info_hash: &InfoHash) -> Option<Box<[SocketAddr]>> {
        self.inner
            .get(info_hash)
            .map(|set| set.iter().copied().collect::<Vec<_>>().into_boxed_slice())
    }
}
