use crate::session::CLIENT_ID;
use bittorrent_common::types::InfoHash;
use bytes::BytesMut;
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use peer_protocol::{
    MessageCodec,
    peer::extension::{ExtendedHandshake, ExtendedMessage},
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
    remote_pid: Id,
    info_hash: InfoHash,
    addr: SocketAddr,
}

impl PeerInfo {
    pub fn new(remote_pid: Id, info_hash: InfoHash, addr: SocketAddr) -> Self {
        Self {
            info_hash,
            addr,
            remote_pid,
        }
    }

    /// Retrieve the peer address.
    pub fn addr(&self) -> &SocketAddr {
        &self.addr
    }

    /// Retrieve the peer info hash.
    pub fn info_hash(&self) -> &InfoHash {
        &self.info_hash
    }
}

#[derive(Debug)]
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
    incoming_request: HashSet<BlockInfo>,

    last_recv_msg: Instant,
    supports_extended: bool,

    max_reqq_size: usize,
}

const DEFAULT_REQQ_SIZE: usize = 250; // The default in in libtorrent is 250.

impl Connected {
    pub fn new(stream: TcpStream, supports_extended: bool) -> Self {
        let framed = Framed::new(stream, MessageCodec {});
        let (sink, stream) = framed.split();
        Self {
            sink,
            stream,
            peer_state: PeerState::default(),
            download_queue: None,
            outbound_requests: HashSet::new(),
            incoming_request: HashSet::new(),
            last_recv_msg: Instant::now(),
            supports_extended,
            max_reqq_size: DEFAULT_REQQ_SIZE,
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
            Duration::from_secs(15),
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
        tracing::debug!(
            "Initiating handshake with peer at {}",
            self.peer_info.addr()
        );

        let handshake = Handshake::new(*CLIENT_ID, *self.peer_info.info_hash());

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
            tracing::warn!("Handshake failed: info hash mismatch");
            return Err(PeerError::InvalidHandshake);
        }

        // TODO: Register client id

        tracing::debug!(
            "Handshake successful with peer at {}",
            self.peer_info.addr()
        );

        let supports_extended = remote_handshake.support_extended_message();

        Ok(Peer {
            state: Connected::new(self.state.stream, supports_extended),
            peer_info: self.peer_info,
            manager_tx: self.manager_tx,
            cmd_rx: self.cmd_rx,
        })
    }
}

impl Peer<Connected> {
    pub fn from_incoming(
        stream: TcpStream,
        supports_extended: bool,
        peer_info: PeerInfo,
        manager_tx: mpsc::Sender<PeerEvent>,
        cmd_rx: mpsc::Receiver<PeerCommand>,
    ) -> Self {
        Self {
            state: Connected::new(stream, supports_extended),
            peer_info,
            manager_tx,
            cmd_rx,
        }
    }

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

        if self.state.supports_extended {
            self.send_extended_handshake().await?;
        }

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
                            match &msg{
                                Message::Interested => {
                                    self.state.peer_state.am_interested=true;
                                }
                                Message::NotInterested => {
                                    self.state.peer_state.am_interested = false;
                                }
                                Message::Choke => {
                                    self.state.peer_state.am_choking = true;
                                }
                                Message::Unchoke => {
                                    self.state.peer_state.am_choking=false;
                                }
                                Message::Piece(block) => {
                                    let block_info = BlockInfo{
                                        index:block.index,
                                        begin:block.begin,
                                        length:block.data.len() as u32,
                                    };
                                    self.state.incoming_request.remove(&block_info);
                                }
                                _ => {}
                            };

                            self.state.sink.send(msg).await.map_err(PeerError::IoError)?;
                        }
                        Some(PeerCommand::RejectRequest(block_info)) => {
                            if !self.state.incoming_request.is_empty(){
                                //maybe waiting for this
                                //response the peer canceled the request resulting in us clearing
                                //the incoming_request set
                                self.state.incoming_request.remove(&block_info);
                            }
                        }
                        Some(PeerCommand::AvailableTask(tasks)) => {
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

    async fn send_extended_handshake(&mut self) -> Result<(), PeerError> {
        let extended_handshake = ExtendedHandshake::default()
            .with_client_version("rust.torrent dev")
            .with_yourip(self.peer_info.addr.ip());

        self.state
            .sink
            .send(Message::Extended(ExtendedMessage::Handshake(
                extended_handshake,
            )))
            .await
            .map_err(PeerError::IoError)
    }

    async fn try_request_blocks(&mut self) -> Result<(), PeerError> {
        // tracing::debug!("trying to request block to peer {}", self.peer_info.addr);

        let am_interested = self.state.peer_state.am_interested;
        let peer_not_choking = !self.state.peer_state.peer_choking;
        let has_queue = self.state.download_queue.is_some();

        // tracing::debug!(am_interested, peer_not_choking, has_queue);
        if am_interested && peer_not_choking && has_queue {
            // tracing::debug!("requesting block to peer {}", self.peer_info.addr);
            self.request_blocks().await?
        }

        Ok(())
    }

    async fn request_blocks(&mut self) -> Result<(), PeerError> {
        // Only request up to the pipeline limit
        while self.state.outbound_requests.len() < self.state.max_reqq_size {
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
                self.refill_download_queue().await;
                // we need a refill
                break;
            }
        }
        Ok(())
    }

    async fn refill_download_queue(&self) {
        if self.state.outbound_requests.is_empty() {
            let _ = self
                .manager_tx
                .send(PeerEvent::NeedTask(self.peer_info.remote_pid))
                .await;
        }
    }

    fn pop_block(&mut self) -> Option<BlockInfo> {
        self.state.download_queue.as_mut().and_then(|q| q.pop())
    }

    // Clear the outbound_request, cancels the pending blocks, and notify manager that piece is
    // canceld, so it is partial now
    async fn delete_all_requests(&mut self) {
        if self.state.outbound_requests.is_empty() {
            return;
        }

        let pending_requests = self.state.outbound_requests.drain();
        for req in pending_requests {
            self.state.sink.send(Message::Cancel(req)).await.unwrap()
        }

        //  WARN: Right now we dont have a mechanism to update block level state in the piece
        //  picker, so canceling a piece without make it available to other peer connection to
        //  request will result in an unifished download
    }

    // helper function to map protocol message recv from remote peer  to PeerEvent so manager reacts to them
    async fn handle_msg(&mut self, msg: protocol::Message) {
        use protocol::Message::*;
        match msg {
            KeepAlive => {
                tracing::debug!("recv keepalive");
            }
            // Can we request related
            Choke => {
                tracing::debug!("peer choked us");
                self.delete_all_requests().await;
                self.state.peer_state.peer_choking = true;
                // todo:
                // halt requests
                // outgoing_request + download_queue = not_received_blocks
                // update piece status by:
                //  1. marking that piece as partial
                //  2. make those block available to other peer
                //
            }
            Unchoke => {
                if !self.state.peer_state.peer_choking {
                    tracing::debug!("received unchoke when already unchoked");
                }
                self.state.peer_state.peer_choking = false;
                if let Err(e) = self.try_request_blocks().await {
                    tracing::warn!(?e);
                }
            }
            // Choke related
            Interested => {
                tracing::info!("peer is interested in us");
                let _ = self
                    .manager_tx
                    .send(PeerEvent::Interest(self.peer_info.remote_pid))
                    .await;
                self.state.peer_state.peer_interested = true;
            }
            NotInterested => {
                if self.state.peer_state.peer_interested {
                    tracing::info!("peer is not interested in us anyomore");
                    let _ = self
                        .manager_tx
                        .send(PeerEvent::NotInterest(self.peer_info.remote_pid))
                        .await;
                    self.state.peer_state.peer_interested = false;
                }

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
                // maybe validate this here: so peer only update stats in piece picker
                .manager_tx
                .send(PeerEvent::Bitfield(self.peer_info.remote_pid, payload))
                .await
                .unwrap(),
            // Piece Related
            Request(block_info) => {
                if self.state.peer_state.am_choking {
                    return;
                }
                if self.state.incoming_request.contains(&block_info) {
                    return;
                }

                self.state.incoming_request.insert(block_info);
                let _ = self
                    .manager_tx
                    .send(PeerEvent::BlockRequest(
                        self.peer_info.remote_pid,
                        block_info,
                    ))
                    .await;
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
                    let _ = self
                        .manager_tx
                        .send(PeerEvent::AddBlock(self.peer_info.remote_pid, block))
                        .await;
                    // We freed a pipeline slot; try to request more blocks from the queue
                    if let Err(e) = self.try_request_blocks().await {
                        tracing::warn!(?e);
                    }
                } else {
                    tracing::warn!(
                        "got unrequested piece {}:{}->{}",
                        block_info.index,
                        block_info.begin,
                        block_info.length
                    );
                }
            }
            Cancel(_block_info) => {
                tracing::info!("recv cancel");
            }
            Extended(ExtendedMessage::Handshake(ext_handshake)) => {
                tracing::info!(?ext_handshake);

                if let Some(reqq) = ext_handshake.reqq {
                    self.state.max_reqq_size = (reqq as usize).max(self.state.max_reqq_size);
                }
            }
        }
    }
}

pub fn spawn_outgoing_peer(
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

pub fn spawn_incoming_peer(
    info: PeerInfo,
    manager_tx: mpsc::Sender<PeerEvent>,
    stream: TcpStream,
    supports_extended: bool,
) -> mpsc::Sender<PeerCommand> {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);

    tokio::spawn(async move {
        // attach context to all logs from this task

        let result = async {
            let peer_conn =
                Peer::from_incoming(stream, supports_extended, info, manager_tx.clone(), cmd_rx);
            peer_conn.run().await
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
