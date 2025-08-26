use bittorrent_common::types::{InfoHash, PeerID};
use bytes::BytesMut;
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use peer_protocol::{
    MessageCodec,
    protocol::{self, BlockInfo, Handshake, Message},
};
use std::{collections::HashSet, fmt::Debug, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    time::{Instant, interval_at, timeout},
};
use tokio_util::codec::Framed;
use tracing::instrument;

use super::{
    error::PeerError,
    manager::{Id, PeerCommand, PeerEvent},
};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct PeerInfo {
    local_pid: PeerID,
    remote_pid: Id,
    info_hash: InfoHash,
    addr: SocketAddr,
}

impl PeerInfo {
    pub fn new(local_pid: PeerID, remote_pid: Id, info_hash: InfoHash, addr: SocketAddr) -> Self {
        Self {
            local_pid,
            info_hash,
            addr,
            remote_pid,
        }
    }

    /// Retrieve the peer address.
    pub fn addr(&self) -> &SocketAddr {
        &self.addr
    }

    /// Retrieve the peer id.
    pub fn local_peer_id(&self) -> &PeerID {
        &self.local_pid
    }

    /// Retrieve the peer info hash.
    pub fn info_hash(&self) -> &InfoHash {
        &self.info_hash
    }
}

#[derive(Debug)]
// TODO: Idk how to rename this, because peer state is an enum, this is also a peer state
struct PeerState {
    ///this client is choking the peer
    pub am_choking: bool,
    ///this client is interested in the peer
    pub am_interested: bool,
    ///  peer is choking this client
    pub peer_choking: bool,
    ///  peer is interested in this client
    pub peer_interested: bool,
}

impl Default for PeerState {
    /// Client connections start out as "choked" and "not interested".
    fn default() -> Self {
        Self {
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
        }
    }
}

pub trait State: Debug {}

#[derive(Debug)]
pub struct New {}
impl State for New {}

#[derive(Debug)]
pub struct Handshaking {
    stream: TcpStream,
}
impl State for Handshaking {}

#[derive(Debug)]
pub struct Connected {
    sink: SplitSink<Framed<TcpStream, MessageCodec>, Message>,
    stream: SplitStream<Framed<TcpStream, MessageCodec>>,
    peer_state: PeerState,
    download_queue: Option<Vec<BlockInfo>>,
    outbound_requests: HashSet<BlockInfo>,
    last_recv_msg: Instant,
    // Track of pieces we requested and we are awaiting
    // outbound_requests:
    // track of pieces peer requested
    // inbound_requests
    // inflight_blocks:
}

impl Connected {
    pub fn new(stream: TcpStream) -> Self {
        let framed = Framed::new(stream, MessageCodec {});
        let (sink, stream) = framed.split();
        Self {
            sink,
            stream,
            peer_state: PeerState::default(),
            download_queue: None,
            outbound_requests: HashSet::new(),
            last_recv_msg: Instant::now(),
        }
    }
}
impl State for Connected {}

pub struct Peer<S: State> {
    state: S,
    peer_info: PeerInfo,
    manager_tx: mpsc::Sender<PeerEvent>,
    cmd_rx: mpsc::Receiver<PeerCommand>,
}

impl Peer<New> {
    pub fn new(
        peer_info: PeerInfo,
        manager_tx: mpsc::Sender<PeerEvent>,
        cmd_rx: mpsc::Receiver<PeerCommand>,
    ) -> Self {
        Self {
            state: New {},
            peer_info,
            manager_tx,
            cmd_rx,
        }
    }

    pub async fn connect(self) -> Result<Peer<Handshaking>, PeerError> {
        let stream = timeout(
            Duration::from_secs(10),
            TcpStream::connect(self.peer_info.addr()),
        )
        .await
        .map_err(|_| PeerError::Timeout)?
        .map_err(PeerError::IoError)?;

        Ok(Peer {
            state: Handshaking { stream },
            peer_info: self.peer_info,
            manager_tx: self.manager_tx,
            cmd_rx: self.cmd_rx,
        })
    }
}

impl Peer<Handshaking> {
    pub async fn handshake(mut self) -> Result<Peer<Connected>, PeerError> {
        tracing::info!(
            "Initiating handshake with peer at {}",
            self.peer_info.addr()
        );
        let handshake =
            Handshake::new(*self.peer_info.local_peer_id(), *self.peer_info.info_hash());

        tracing::debug!("Sending handshake message to peer");
        self.state
            .stream
            .write_all(&handshake.to_bytes())
            .await
            .map_err(PeerError::IoError)?;
        tracing::debug!("Handshake sent successfully");

        tracing::debug!("Waiting to receive handshake from peer");
        let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);
        self.state
            .stream
            .read_exact(&mut buf)
            .await
            .map_err(PeerError::IoError)?;
        tracing::debug!("Received handshake from peer");

        let remote_handshake = Handshake::from_bytes(&buf).ok_or(PeerError::InvalidHandshake)?;
        tracing::debug!("Validating remote handshake");

        if remote_handshake.info_hash != *self.peer_info.info_hash() {
            tracing::error!("Handshake failed: info hash mismatch");
            return Err(PeerError::InvalidHandshake);
        }

        tracing::info!(
            "Handshake successful with peer at {}",
            self.peer_info.addr()
        );

        Ok(Peer {
            state: Connected::new(self.state.stream),
            peer_info: self.peer_info,
            manager_tx: self.manager_tx,
            cmd_rx: self.cmd_rx,
        })
    }
}

impl Peer<Connected> {
    const CONNECTION_TIMEOUT: Duration = Duration::from_secs(120); // 2 minutes
    #[instrument(
    name = "peer",
    skip_all,
    fields(
        pid = ?self.peer_info.remote_pid,
        addr = %self.peer_info.addr,
        )
    )]
    pub async fn run(mut self) -> Result<(), PeerError> {
        // send message to keep the connection alive
        let mut heartbeat = interval_at(
            tokio::time::Instant::now() + Duration::from_secs(60),
            Duration::from_secs(60),
        );

        let mut timeout_check = interval_at(
            Instant::now() + Duration::from_secs(30),
            Duration::from_secs(30),
        );

        loop {
            tokio::select! {
                maybe_msg = self.state.stream.next() => {
                    match maybe_msg {
                        Some(Ok(msg)) => {
                            self.state.last_recv_msg = Instant::now();
                            self.handle_msg(msg).await;
                        }
                        Some(Err(e)) => return Err(PeerError::IoError(e)),
                        None => return Err(PeerError::Disconnected),
                    }
                }
                maybe_cmd= self.cmd_rx.recv() => {
                    match maybe_cmd{
                        Some(PeerCommand::SendMessage(msg)) => {
                            // before sending process state
                            tracing::debug!("Sending {msg:?}");
                            match msg{
                                Message::Interested => {
                                    self.state.peer_state.am_interested=true;
                                }
                                Message::NotInterested => {
                                    self.state.peer_state.am_interested = false;
                                }
                                _ => {}
                            };

                            self.state.sink.send(msg).await.map_err(PeerError::IoError)?;
                        }
                        Some(PeerCommand::AvailableTask(tasks)) => {
                            tracing::debug!("rec some download_queue");
                            self.state.download_queue = Some(tasks);
                            if let Err(e)  = self.try_request_blocks().await{
                                tracing::warn!(?e);
                            }
                        }
                        Some(PeerCommand::Disconnect) => break,
                        None => break,
                    }
                }
                _ = heartbeat.tick() => {
                    self.state.sink.send(Message::KeepAlive).await.map_err(PeerError::IoError)?
                }
                _ = timeout_check.tick() => {
                    // Check if we've received any message within the timeout window
                    if self.state.last_recv_msg.elapsed() > Self::CONNECTION_TIMEOUT {
                        tracing::warn!(
                            "Peer {} connection timeout: no message received for {:?}",
                            self.peer_info.addr(),
                            self.state.last_recv_msg.elapsed()
                        );
                        return Err(PeerError::Timeout);
                    }
                }
            }
        }

        Ok(())
    }

    async fn try_request_blocks(&mut self) -> Result<(), PeerError> {
        tracing::debug!("trying to request block to peer {}", self.peer_info.addr);

        let am_interested = self.state.peer_state.am_interested;
        let peer_not_choking = !self.state.peer_state.peer_choking;
        let has_queue = self.state.download_queue.is_some();

        tracing::debug!(am_interested, peer_not_choking, has_queue);
        if am_interested && peer_not_choking && has_queue {
            tracing::debug!("requesting block to peer {}", self.peer_info.addr);
            self.request_blocks().await?
        }

        Ok(())
    }

    // TODO Dynamically adjust pipeline by upload rate of peer
    const MAX_PIPELINE: usize = 5;
    async fn request_blocks(&mut self) -> Result<(), PeerError> {
        // Only request up to the pipeline limit
        while self.state.outbound_requests.len() < Self::MAX_PIPELINE {
            if let Some(block) = self.pop_block() {
                self.state.outbound_requests.insert(block);

                tracing::debug!("Requesting block: {:?}", block);
                self.state
                    .sink
                    .send(Message::Request(block))
                    .await
                    .map_err(PeerError::IoError)?;
            } else {
                tracing::debug!("No more blocks available in queue");
                break;
            }
        }
        Ok(())
    }

    fn pop_block(&mut self) -> Option<BlockInfo> {
        self.state.download_queue.as_mut().and_then(|q| q.pop())
    }

    // helper function to map protocol message recv from remote peer  to PeerEvent so manager reacts to them
    async fn handle_msg(&mut self, msg: protocol::Message) {
        use protocol::Message::*;
        match msg {
            KeepAlive => {
                tracing::debug!("recv keepalive");
            }
            // Can we request related
            Choke => self.state.peer_state.peer_choking = true,
            Unchoke => {
                self.state.peer_state.peer_choking = false;
                if let Err(e) = self.try_request_blocks().await {
                    tracing::warn!(?e);
                }
            }
            // Choke related
            Interested => {
                tracing::info!("peer is interested in us");
            }
            NotInterested => {
                tracing::info!("peer is not interested in us");
            }
            // Swarm info related
            Have { piece_index } => self
                .manager_tx
                .send(PeerEvent::Have {
                    pid: self.peer_info.remote_pid,
                    piece_idx: piece_index,
                })
                .await
                .unwrap(),
            Bitfield(payload) => self
                .manager_tx
                .send(PeerEvent::Bitfield(self.peer_info.remote_pid, payload))
                .await
                .unwrap(),
            // Piece Related
            Request(block_info) => {
                tracing::info!("peer request block : {:?}", block_info);
            }
            Piece(block) => {
                // check if we requested this block
                let block_info = BlockInfo {
                    index: block.index,
                    begin: block.begin,
                    length: block.data.len() as u32,
                };

                if self.state.outbound_requests.remove(&block_info) {
                    tracing::debug!("Received block");
                    let _ = self.manager_tx.send(PeerEvent::AddBlock(block)).await;
                    // We freed a pipeline slot; try to request more blocks from the queue
                    if let Err(e) = self.try_request_blocks().await {
                        tracing::warn!(?e);
                    }
                }
            }
            Cancel(_block_info) => {
                tracing::info!("recv cancel");
            }
        }
    }
}

// IDEA: Use an Arc<SharedState> wit atomics to share download stats,

pub fn spawn_peer(
    info: PeerInfo,
    manager_tx: mpsc::Sender<PeerEvent>,
) -> mpsc::Sender<PeerCommand> {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);

    tokio::spawn(async move {
        // attach context to all logs from this task

        let result = async {
            let p0 = Peer::new(info, manager_tx.clone(), cmd_rx);
            // Establish connection to remote peer
            let p1 = p0.connect().await?;
            // Handshake remote peer
            let p2 = p1.handshake().await?;
            p2.run().await
        }
        .await;

        if let Err(err) = result {
            tracing::warn!(?err, "peer task exited with error");
            let _ = manager_tx
                .send(PeerEvent::PeerError(info.remote_pid, err))
                .await;
        }
    });

    cmd_tx
}
