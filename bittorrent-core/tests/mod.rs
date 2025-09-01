use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{net::SocketAddr, sync::Arc};

use bittorrent_common::{
    metainfo::{FileMode, Info, TorrentInfo},
    types::InfoHash,
};
use bittorrent_core::{Bitfield, PeerManagerHandle, Storage};
use bytes::{Bytes, BytesMut};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use peer_protocol::MessageCodec;
use peer_protocol::protocol::{Block, BlockInfo, Handshake, Message};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use bittorrent_common::types::PeerID;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

const BLOCK_SIZE: u32 = 1 << 14; // 16KB

#[derive(Debug)]
// Mock Seeder Peer
struct MockPeer {
    addr: SocketAddr,
    info_hash: InfoHash,
    blocks: Arc<HashMap<BlockInfo, Block>>,
    client_id: PeerID,
}

impl MockPeer {
    pub fn new(
        addr: SocketAddr,
        info_hash: InfoHash,
        client_id: PeerID,
        blocks: Arc<HashMap<BlockInfo, Block>>,
    ) -> Self {
        Self {
            addr,
            info_hash,
            client_id,
            blocks,
        }
    }

    /// Start listening and accepting connections
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind(self.addr).await?;
        let actual_addr = listener.local_addr()?;

        tracing::info!("Mock peer listening on {}", actual_addr);

        // Accept connections in a loop
        while let Ok((stream, peer_addr)) = listener.accept().await {
            tracing::info!(
                "Mock peer at {} accepted connection from {}",
                actual_addr,
                peer_addr
            );

            let info_hash = self.info_hash;
            let client_id = self.client_id;
            let blocks = self.blocks.clone();

            // Spawn a task to handle this specific connection
            tokio::spawn(async move {
                if let Err(e) =
                    Self::handle_connection(stream, info_hash, client_id, blocks.clone()).await
                {
                    tracing::warn!("Connection handling failed: {}", e);
                }
            });
        }

        Ok(())
    }

    /// Handle a single peer connection through its entire lifecycle
    async fn handle_connection(
        mut stream: TcpStream,
        info_hash: InfoHash,
        client_id: PeerID,
        blocks: Arc<HashMap<BlockInfo, Block>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Phase 1: Handshake
        tracing::debug!("Awaiting handshake from peer");

        let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);
        stream.read_exact(&mut buf).await?;

        let remote_handshake = Handshake::from_bytes(&buf).ok_or("Invalid handshake received")?;

        // Validate info hash
        if remote_handshake.info_hash != info_hash {
            return Err("Info hash mismatch".into());
        }

        tracing::debug!("Valid handshake received, responding");

        // Send our handshake response
        let our_handshake = Handshake::new(client_id, info_hash);
        stream.write_all(&our_handshake.to_bytes()).await?;

        tracing::info!("Handshake completed successfully");

        // Phase 2: Switch to message protocol
        let framed = Framed::new(stream, MessageCodec {});
        let (mut sink, mut stream) = framed.split();

        // Create and send bitfield
        let num_pieces = blocks.keys().map(|b| b.index as usize).max().unwrap_or(0) + 1;

        let mut bitfield = Bitfield::new(num_pieces);
        for block_info in blocks.keys() {
            let _ = bitfield.set(block_info.index as usize);
        }

        sink.send(Message::Bitfield(bitfield.as_bytes())).await?;

        // Phase 3: Message handling loop
        Self::serve_peer(&mut sink, &mut stream, blocks.clone()).await
    }

    /// Handle the main peer protocol messages
    async fn serve_peer(
        sink: &mut SplitSink<Framed<TcpStream, MessageCodec>, Message>,
        stream: &mut SplitStream<Framed<TcpStream, MessageCodec>>,
        blocks: Arc<HashMap<BlockInfo, Block>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        while let Some(msg_result) = stream.next().await {
            let msg = msg_result?;
            tracing::debug!("Mock peer received message: {:?}", msg);

            match msg {
                Message::KeepAlive => {
                    // Acknowledge keep alive
                }
                Message::Choke => {
                    tracing::debug!("Peer choked us");
                }
                Message::Unchoke => {
                    tracing::debug!("Peer unchoked us");
                }
                Message::Interested => {
                    tracing::debug!("Peer is interested, unchoking");
                    sink.send(Message::Unchoke).await?;
                }
                Message::NotInterested => {
                    tracing::debug!("Peer is not interested");
                }
                Message::Have { piece_index } => {
                    tracing::debug!("Peer has piece {}", piece_index);
                }
                Message::Bitfield(_) => {
                    tracing::debug!("Received peer bitfield");
                }
                Message::Request(block_info) => {
                    if let Some(block) = blocks.get(&block_info) {
                        tracing::debug!("Serving block: {:?}", block_info);
                        sink.send(Message::Piece(block.clone())).await?;
                    } else {
                        tracing::warn!("Requested block not available: {:?}", block_info);
                    }
                }
                Message::Piece(_block) => {
                    tracing::debug!("Received piece (as leecher)");
                }
                Message::Cancel(_) => {
                    tracing::debug!("Peer cancelled request");
                }
            }
        }

        Ok(())
    }
}

pub struct MockSwarm {
    peers: Vec<SocketAddr>,
    _handles: Vec<tokio::task::JoinHandle<()>>,
}

impl MockSwarm {
    pub async fn new(
        swarm_size: usize,
        torrent: Arc<TorrentInfo>,
        blocks_map: Arc<HashMap<BlockInfo, Block>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut peers = Vec::new();
        let mut handles = Vec::new();
        let base_port = 9000;

        // Spawn peers sequentially with incremented addresses
        for i in 0..swarm_size {
            let port = base_port + i;
            let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;

            tracing::info!("Creating mock peer {} at {}", i, addr);

            // All peers are seeders for now - they get all blocks
            let peer_blocks = blocks_map.clone();
            let client_id = PeerID::generate();

            let mock_peer = MockPeer::new(addr, torrent.info_hash, client_id, peer_blocks);

            // Spawn the peer and store its handle
            let handle = tokio::spawn(async move {
                if let Err(e) = mock_peer.run().await {
                    tracing::error!("Mock peer at {} failed: {}", addr, e);
                }
            });

            peers.push(addr);
            handles.push(handle);
        }

        // Give peers a moment to start listening
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Ok(Self {
            peers,
            _handles: handles,
        })
    }

    pub fn get_peer_addrs(&self) -> &[SocketAddr] {
        &self.peers
    }

    pub fn seeder_addrs(&self) -> impl Iterator<Item = &SocketAddr> {
        // Since all peers are seeders for now, return all peers
        self.peers.iter()
    }
}

fn create_deterministic_torrent_with_blocks(
    file_size: usize,
    piece_length: i64,
    file_name: &str,
    seed: u64,
) -> (TorrentInfo, HashMap<BlockInfo, Block>) {
    // Generate deterministic but varied test data
    let mut file_data = Vec::with_capacity(file_size);
    for i in 0..file_size {
        let value = ((i as u64 + seed) * 1103515245 + 12345) % 256;
        file_data.push(value as u8);
    }

    let mut pieces = Vec::new();
    let mut blocks_map = HashMap::new();
    let mut file_offset = 0;

    while file_offset < file_data.len() {
        let piece_end = std::cmp::min(file_offset + piece_length as usize, file_data.len());
        let piece_data = &file_data[file_offset..piece_end];
        let piece_index = pieces.len() as u32;

        // Calculate piece hash
        let mut hasher = Sha1::new();
        hasher.update(piece_data);
        let hash: [u8; 20] = hasher.finalize().into();
        pieces.push(hash);

        // Break piece into blocks
        let mut piece_offset = 0;
        while piece_offset < piece_data.len() {
            let block_end = std::cmp::min(piece_offset + BLOCK_SIZE as usize, piece_data.len());
            let block_data = &piece_data[piece_offset..block_end];

            let block_info = BlockInfo {
                index: piece_index,
                begin: piece_offset as u32,
                length: block_data.len() as u32,
            };

            let block = Block {
                index: piece_index,
                begin: piece_offset as u32,
                data: Bytes::copy_from_slice(block_data),
            };

            blocks_map.insert(block_info, block);
            piece_offset = block_end;
        }

        file_offset = piece_end;
    }

    let info = Info {
        piece_length,
        pieces,
        private: None,
        mode: FileMode::SingleFile {
            name: file_name.to_string(),
            length: file_size as i64,
            md5sum: None,
        },
    };

    let mut hasher = Sha1::new();
    hasher.update(format!("test_{}_{}_{}", file_name, file_size, seed));
    let info_hash_bytes: [u8; 20] = hasher.finalize().into();

    let torrent = TorrentInfo {
        info,
        announce: "http://mock-tracker.local/announce".to_string(),
        announce_list: None,
        creation_date: Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        ),
        comment: Some("End-to-end test torrent".to_string()),
        created_by: Some("rust-torrent-e2e-test".to_string()),
        encoding: None,
        info_hash: InfoHash::new(info_hash_bytes),
    };

    (torrent, blocks_map)
}

#[tokio::test]
async fn test_complete_download_flow() {
    // Initialize tracing for better debugging
    let _ = tracing_subscriber::fmt::try_init();

    // Test configuration
    const FILE_SIZE: usize = 65 * 1024 * 1024; // 65 mb
    const PIECE_LENGTH: i64 = 16 * 1024; // 16KB pieces
    const FILE_NAME: &str = "complete_test.bin";
    const SEED: u64 = 42;
    const SWARM_SIZE: usize = 3;

    let (torrent_file, blocks_map) =
        create_deterministic_torrent_with_blocks(FILE_SIZE, PIECE_LENGTH, FILE_NAME, SEED);

    let torrent_file = Arc::new(torrent_file);
    let blocks_map = Arc::new(blocks_map);
    tracing::info!(
        "Created torrent with {} pieces and {} blocks",
        torrent_file.num_pieces(),
        blocks_map.len()
    );

    // Create mock swarm - ALL PEERS ARE SEEDERS FOR NOW
    let swarm = MockSwarm::new(SWARM_SIZE, torrent_file.clone(), blocks_map)
        .await
        .expect("Failed to create mock swarm");

    tracing::info!(
        "Created mock swarm with {} peers",
        swarm.get_peer_addrs().len()
    );

    // Create storage for testing
    let storage = Arc::new(Storage::new());
    storage.add_torrent(torrent_file.clone());

    // Create peer manager
    let (peer_manager, completion_rx) = PeerManagerHandle::new(torrent_file.clone(), storage);

    // Add all seeder peers to manager
    let client_id = PeerID::generate();
    for &seeder_addr in swarm.get_peer_addrs() {
        tracing::info!("Adding peer: {}", seeder_addr);
        peer_manager.add_peer(seeder_addr, client_id);
    }

    if let Err(e) = completion_rx.await {
        tracing::error!(?e);
    }

    tracing::info!(
        "torrent {} finished downloading",
        torrent_file.info.mode.name()
    );

    // Shutdown
    peer_manager.shutdown();
}
