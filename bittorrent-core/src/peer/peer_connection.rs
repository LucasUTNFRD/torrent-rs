use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use bencode::Bencode;
use bittorrent_common::{metainfo::Info, types::InfoHash};
use bytes::{Bytes, BytesMut};
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use peer_protocol::{
    MessageCodec,
    peer::extension::{
        DATA_ID, EXTENSION_NAME_METADATA, ExtendedHandshake, ExtendedMessage, MetadataMessage,
        REJECT_ID, REQUEST_ID, RawExtendedMessage,
    },
    protocol::{Block, BlockInfo, Handshake, Message},
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::interval,
};

use tokio_util::codec::Framed;
use tracing::{debug, info, instrument, warn};

use crate::{
    bitfield::{Bitfield, BitfieldError},
    peer::{ConnectTimeout, PeerMessage, metrics::PeerMetrics},
    // piece_picker::DownloadTask,
    session::CLIENT_ID,
    torrent::{Pid, TorrentMessage},
};

///The metadata is handled in blocks of 16KiB (16384 Bytes).
const UT_METADATA_ID: u8 = 1;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("Network error: {0}")]
    Network(#[from] tokio::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Connection timeout")]
    Timeout,

    #[error("Invalid handshake")]
    InvalidHandshake,

    #[error("Bitfield error")]
    BitfieldError(#[from] BitfieldError),
}

pub trait ConnectionState {}
pub struct Peer<S: ConnectionState> {
    state: S,
    addr: SocketAddr,
    torrent_tx: mpsc::Sender<TorrentMessage>,
    cmd_rx: mpsc::Receiver<PeerMessage>,
    info_hash: InfoHash,
    pid: Pid,
}

pub struct New;
impl ConnectionState for New {}

pub struct Handshaking {
    stream: TcpStream,
}

impl Peer<New> {
    pub fn new(
        pid: Pid,
        addr: SocketAddr,
        info_hash: InfoHash, //maybe move this into a new_outbound
        torrent_tx: mpsc::Sender<TorrentMessage>,
        cmd_rx: mpsc::Receiver<PeerMessage>,
    ) -> Self {
        Self {
            addr,
            info_hash,
            torrent_tx,
            cmd_rx,
            pid,
            state: New {},
        }
    }

    pub async fn connect(self) -> Result<Peer<Handshaking>, ConnectionError> {
        let stream = TcpStream::connect_timeout(&self.addr, CONNECTION_TIMEOUT).await?;

        Ok(Peer {
            state: Handshaking { stream },
            addr: self.addr,
            torrent_tx: self.torrent_tx,
            cmd_rx: self.cmd_rx,
            info_hash: self.info_hash,
            pid: self.pid,
        })
    }
}

impl ConnectionState for Handshaking {}

impl Peer<Handshaking> {
    pub async fn handshake(mut self) -> Result<Peer<Connected>, ConnectionError> {
        debug!("Initiating handshake with peer at {}", self.addr);

        let handshake = Handshake::new(*CLIENT_ID, self.info_hash);

        debug!("Sending handshake message to peer");
        self.state.stream.write_all(&handshake.to_bytes()).await?;
        tracing::debug!("Handshake sent successfully");

        tracing::debug!("Waiting to receive handshake from peer");
        let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);
        self.state.stream.read_exact(&mut buf).await?;
        tracing::debug!("Received handshake from peer");

        let remote_handshake =
            Handshake::from_bytes(&buf).ok_or(ConnectionError::InvalidHandshake)?;
        tracing::debug!("Validating remote handshake");

        if remote_handshake.info_hash != self.info_hash {
            warn!("Handshake failed: info hash mismatch");
            return Err(ConnectionError::InvalidHandshake);
        }

        let supports_extended = remote_handshake.support_extended_message();

        Ok(Peer {
            state: Connected::new(self.state.stream, supports_extended),
            cmd_rx: self.cmd_rx,
            addr: self.addr,
            torrent_tx: self.torrent_tx,
            info_hash: self.info_hash,
            pid: self.pid,
        })
    }
}

pub struct Connected {
    // Socket Layer
    sink: SplitSink<Framed<TcpStream, MessageCodec>, Message>,
    stream: SplitStream<Framed<TcpStream, MessageCodec>>,
    last_recv_msg: Instant,

    request_queue: Vec<BlockInfo>,
    outgoing_request: HashSet<BlockInfo>,
    last_block_request: Instant,

    target_request_queue: usize,
    max_outgoing_request: usize,
    metrics: Arc<PeerMetrics>,

    slow_start: bool,
    prev_download_rate: u64,

    supports_extended: bool,
    remote_extensions: HashMap<String, i64>,

    // Remote peer state
    peer_interested: bool,
    peer_choked: bool,

    // LOCAL STATE
    interesting: bool,
    choked: bool,

    metadata: Option<Arc<Info>>,

    metadata_size: usize,

    bitfield: BitfieldState,
}

#[derive(Debug, Clone)]
enum BitfieldState {
    NotReceived,
    Received(Bitfield),
    Validated(Bitfield),
}

impl BitfieldState {
    fn new() -> Self {
        Self::NotReceived
    }

    fn is_received(&self) -> bool {
        !matches!(self, Self::NotReceived)
    }

    fn get_bitfield_mut(&mut self) -> Option<&mut Bitfield> {
        match self {
            Self::NotReceived => None,
            Self::Received(bf) | Self::Validated(bf) => Some(bf),
        }
    }

    fn set_received(&mut self, bitfield: Bitfield) {
        *self = Self::Received(bitfield);
    }

    fn validate(&mut self, num_pieces: usize) -> Result<(), BitfieldError> {
        match self {
            Self::NotReceived => {
                // Initialize with empty validated bitfield
                *self = Self::Validated(Bitfield::with_size(num_pieces));
                Ok(())
            }
            Self::Received(bitfield) => {
                bitfield.validate(num_pieces)?;
                *self = Self::Validated(bitfield.clone());
                Ok(())
            }
            Self::Validated(_) => Ok(()), // Already validated
        }
    }

    fn ensure_capacity(&mut self, required_pieces: usize) {
        match self {
            Self::NotReceived => {
                *self = Self::Received(Bitfield::with_size(required_pieces));
            }
            Self::Received(bitfield) | Self::Validated(bitfield) => {
                if required_pieces > bitfield.size() {
                    bitfield.resize(required_pieces);
                }
            }
        }
    }
}

impl ConnectionState for Connected {}
impl Connected {
    pub fn new(stream: TcpStream, supports_extended: bool) -> Self {
        let framed = Framed::new(stream, MessageCodec {});
        let (sink, stream) = framed.split();

        Self {
            sink,
            stream,
            outgoing_request: HashSet::new(),
            supports_extended,
            last_recv_msg: Instant::now(),
            peer_interested: false,
            peer_choked: true,
            interesting: false,
            choked: true,
            remote_extensions: HashMap::new(),
            metadata_size: 0,
            max_outgoing_request: 250,
            bitfield: BitfieldState::new(),
            metadata: None,
            request_queue: Vec::new(),
            metrics: Arc::new(PeerMetrics::new()),
            last_block_request: Instant::now(),
            target_request_queue: 2,
            prev_download_rate: 0,
            slow_start: true,
        }
    }
}

// TODO: Add slow start mechanisim

impl Peer<Connected> {
    #[instrument(
    skip(self),
    name = "peer",
    target = "torrent::peer",
    fields(
        pid = %self.pid,
        addr = %self.addr,
    )
    )]
    pub async fn start(mut self) -> Result<(), ConnectionError> {
        info!("connection established");

        self.have_valid_metadata().await;

        if self.state.supports_extended {
            self.send_extended_handshake().await?
        }

        let mut heartbeat_ticker = interval(Duration::from_secs(60));
        let mut metric_update = interval(Duration::from_secs(1));
        heartbeat_ticker.tick().await;
        metric_update.tick().await;

        loop {
            self.maybe_request_medata().await?;

            tokio::select! {
                maybe_msg = self.state.stream.next() => {
                    match maybe_msg{
                        // TODO: Not all error should return
                        Some(Ok(msg)) => self.handle_msg(msg).await?,
                        Some(Err(_e)) => break,
                        None => break,
                    }
                }
                maybe_cmd = self.cmd_rx.recv() => {
                    match maybe_cmd{
                        Some(cmd) => {self.handle_peer_cmd(cmd).await?},
                        None => break,
                    }
                }
                _=heartbeat_ticker.tick()=>{

                    let time_since_last_request = self.state.last_block_request.elapsed();
                    if self.state.interesting && time_since_last_request > Duration::from_secs(30) || self.state.last_recv_msg.elapsed() > Duration::from_secs(60) {
                        warn!(
                            "Disconnecting peer: interested but no block request activity for {} seconds",
                            time_since_last_request.as_secs()
                        );
                        break;
                    }

                    self.state.sink.send(Message::KeepAlive).await?;
                }
                _ = metric_update.tick() => {
                    self.metric_update();
                }
            }
        }

        // For proper cleanup
        let bitifeld = if let BitfieldState::Validated(bitifeld) = self.state.bitfield {
            Some(bitifeld)
        } else {
            None
        };

        // I did not figure out a easier way of doing this cleaup
        // maybe implemet Drop trait for Peer<State>
        let _ = self
            .torrent_tx
            .send(TorrentMessage::PeerDisconnected(self.pid, bitifeld))
            .await;

        Ok(())
    }

    fn metric_update(&mut self) {
        self.state.metrics.update_rates();

        info!(
            "dr : {} | choked: {} | interested: {} | max_queue_size : {}, outgoing_requests: {} | slow_start : {}",
            self.state.metrics.get_download_rate_human(),
            self.state.choked,
            self.state.interesting,
            self.state.target_request_queue,
            self.state.outgoing_request.len(),
            self.state.slow_start
        );

        let current_rate = self.state.metrics.get_download_rate();
        if self.state.slow_start {
            let rate_increase = current_rate.saturating_sub(self.state.prev_download_rate);

            // 2025 Logic: Require at least 5% growth to stay in slow start.
            // On high speed networks, 10kB/s is just noise.
            // We use a minimum threshold of 10kB/s to avoid division issues at very low speeds.
            let threshold = std::cmp::max(current_rate / 20, 10_240);

            if current_rate > 0 && rate_increase < threshold {
                info!(
                    "Leaving slow start. Rate: {} B/s, Increase: {}",
                    current_rate, rate_increase
                );
                self.state.slow_start = false;
            }
        } else {
            const BLOCK_SIZE: f64 = 16384.0;
            const ASSUMED_LATENCY: f64 = 0.5;

            // BDP
            let target = (current_rate as f64 * ASSUMED_LATENCY / BLOCK_SIZE) as usize;

            // Ensure we request at least 2 items (pipelining), but do not exceed the peer's explicit limit
            self.state.target_request_queue = target.max(2).min(self.state.max_outgoing_request);
        }

        // Update previous rate for the next tick comparison
        self.state.prev_download_rate = current_rate;
    }

    // debug message with
    // download speed
    // pipeline utilization
    fn debug_peer_dowload() {}

    async fn handle_msg(&mut self, msg: Message) -> Result<(), ConnectionError> {
        self.state.last_recv_msg = Instant::now();

        match msg {
            Message::KeepAlive => self.on_keepalive().await?,
            Message::Choke => self.on_choke().await?,
            Message::Unchoke => self.on_unchoke().await?,
            Message::Interested => self.on_interested().await?,
            Message::NotInterested => self.on_not_interested().await?,
            Message::Have { piece_index } => self.on_have(piece_index).await?,
            Message::Bitfield(payload) => self.on_bitfield(payload).await?,
            Message::Request(block) => self.on_request().await?,
            Message::Piece(block) => self.on_incoming_piece(block).await?,
            Message::Cancel(block) => self.on_cancel().await?,
            Message::Port { port } => {
                // Peers that receive this message should attempt to ping the node on the received port and IP address of the remote peer.
                let node_addr: SocketAddr = SocketAddr::new(self.addr.ip(), port);
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::DhtAddNode { node_addr })
                    .await;
            }
            Message::Extended(extended) => match extended {
                ExtendedMessage::Handshake(handshake) => {
                    self.on_extended_handshake(handshake).await?
                }
                ExtendedMessage::ExtensionMessage(raw_extended_msg) => {
                    self.on_extension(raw_extended_msg).await?;
                }
            },
        }

        Ok(())
    }

    async fn handle_peer_cmd(&mut self, peer_cmd: PeerMessage) -> Result<(), ConnectionError> {
        match peer_cmd {
            PeerMessage::SendHave { piece_index } => {
                if let BitfieldState::Validated(ref mut bitifled) = self.state.bitfield
                    && bitifled.has(piece_index as usize)
                {
                    self.state.sink.send(Message::Have { piece_index }).await?
                }
            }
            PeerMessage::SendBitfield { bitfield } => todo!(),
            PeerMessage::SendChoke => todo!(),
            PeerMessage::SendUnchoke => todo!(),
            PeerMessage::Disconnect => todo!(),
            PeerMessage::SendMessage(protocol_msg) => self.state.sink.send(protocol_msg).await?,
            PeerMessage::HaveMetadata(metadata) => {
                debug!("Received metadata from torrent");

                let num_pieces = metadata.num_pieces();
                self.state.metadata = Some(metadata);

                self.state.bitfield.validate(num_pieces)?;

                // Update interest status now that we have metadata
                self.update_interest_status().await?;
            }
        }
        Ok(())
    }

    async fn update_interest_status(&mut self) -> Result<(), ConnectionError> {
        if self.state.interesting {
            return Ok(());
        }

        let BitfieldState::Validated(ref bitfield) = self.state.bitfield else {
            return Ok(());
        };

        // ask torrent manager if we should be interested in this peer
        let (resp_tx, resp_rx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::ShouldBeInterested {
                pid: self.pid,
                bitfield: bitfield.clone(),
                resp_tx,
            })
            .await;

        self.state.interesting = resp_rx.await.expect("sender dropped");

        self.request_block().await?;
        self.send_block_request().await?;

        Ok(())
    }

    async fn send_extended_handshake(&mut self) -> Result<(), ConnectionError> {
        let mut extensions = BTreeMap::new();
        extensions.insert(EXTENSION_NAME_METADATA.to_string(), UT_METADATA_ID.into());
        // TODO: extensions.insert(EXTENSION_NAME_PEX.to_string(), 2);

        let handshake = ExtendedHandshake::new()
            .with_extensions(extensions)
            .with_client_version("torrent-rs 0.1.0")
            .with_request_queue_size(250); //for now we ignore this

        self.state
            .sink
            .send(Message::Extended(ExtendedMessage::Handshake(handshake)))
            .await?;

        Ok(())
    }

    // Message Handlers
    async fn on_keepalive(&mut self) -> Result<(), ConnectionError> {
        debug!("recv keepalive");
        Ok(())
    }
    async fn on_choke(&mut self) -> Result<(), ConnectionError> {
        // todo!(
        debug!("RECV CHOKE");
        Ok(())
    }
    async fn on_unchoke(&mut self) -> Result<(), ConnectionError> {
        debug!("UNCHOKE Received");
        self.state.peer_choked = false;
        if self.state.interesting {
            self.request_block().await?;
            self.send_block_request().await?;
            // request block for this torrent
            // send blocks
        } else {
            self.update_interest_status().await?;
        }

        Ok(())
    }

    /// Request blocks to TorrentSupervisor
    async fn request_block(&mut self) -> Result<(), ConnectionError> {
        if self.state.metadata.is_none() {
            return Ok(());
        }

        if !self.state.request_queue.is_empty() {
            debug!("There are remaing piece to request in the queue");
            return Ok(());
        }

        let BitfieldState::Validated(bitfield) = self.state.bitfield.clone() else {
            return Ok(());
        };

        // // Calculate how many blocks we can request
        // // max_outgoing_request is your pipeline size (e.g., 10-50 blocks)
        // let available_slots = self
        //     .state
        //     .max_outgoing_request
        //     .saturating_sub(self.state.outgoing_request.len());
        //
        // if available_slots == 0 {
        //     return Ok(());
        // }
        let current_in_flight = self.state.outgoing_request.len();

        // If we have more in flight than our target, back off
        if current_in_flight >= self.state.target_request_queue {
            return Ok(());
        }

        // Calculate slots to fill the window
        let available_slots = self.state.target_request_queue - current_in_flight;

        if available_slots == 0 {
            return Ok(());
        }

        let (block_tx, block_rx) = oneshot::channel();

        let _ = self
            .torrent_tx
            .send(TorrentMessage::RequestBlock {
                pid: self.pid,
                max_requests: available_slots,
                bitfield,
                block_tx,
            })
            .await;

        let tasks = block_rx.await.expect("channel not dropped");

        self.state.request_queue.extend(tasks);

        Ok(())
    }

    async fn send_block_request(&mut self) -> Result<(), ConnectionError> {
        debug!("SENDING BLOCK REQUEST");
        while let Some(block) = self.state.request_queue.pop() {
            if self.state.outgoing_request.len() > self.state.max_outgoing_request {
                debug!("pipeline is empty, should request more pieces");
                break;
            }

            self.state.outgoing_request.insert(block);
            self.state.last_block_request = Instant::now();
            self.state.sink.send(Message::Request(block)).await?;
            debug!("SENT");
        }

        Ok(())
    }

    async fn on_interested(&mut self) -> Result<(), ConnectionError> {
        todo!("Received interested message")
    }

    async fn on_not_interested(&mut self) -> Result<(), ConnectionError> {
        todo!("Received not interested message")
    }

    // Updates piece availability - Affects which pieces the torrent tries to download next
    // Enables requests - May make this peer interesting if they have pieces we want
    async fn on_have(&mut self, have_idx: u32) -> Result<(), ConnectionError> {
        debug!("Received HAVE message for piece {}", have_idx);

        // Step 1: Bitfield Initialization - If no bitfield received yet, initialize one
        if !self.state.bitfield.is_received() {
            debug!("First HAVE message without bitfield - initializing empty bitfield");
            self.state
                .bitfield
                .set_received(Bitfield::with_size((have_idx + 1) as usize));
        }

        // Step 2: Dynamic Resizing - If no metadata yet, resize bitfield to accommodate new piece index
        if self.state.metadata.is_none() {
            let required_pieces = (have_idx + 1) as usize;
            self.state.bitfield.ensure_capacity(required_pieces);
        }

        // Step 3: Index Validation - Ensure piece index is within valid range
        if let Some(metadata) = &self.state.metadata {
            let num_pieces = metadata.pieces.len();

            if have_idx as usize >= num_pieces {
                warn!(
                    "Received HAVE for invalid piece index {} (max: {})",
                    have_idx,
                    num_pieces - 1
                );
                return Err(ConnectionError::Protocol(format!(
                    "Invalid piece index: {} >= {}",
                    have_idx, num_pieces
                )));
            }
        } else {
            // Pre-metadata validation - cap at 64k pieces
            if have_idx >= 65536 {
                warn!(
                    "Received HAVE for piece index {} exceeding 64k limit",
                    have_idx
                );
                return Err(ConnectionError::Protocol(format!(
                    "Piece index {} exceeds maximum",
                    have_idx
                )));
            }
        }

        // Update the bitfield if we don't already have this piece
        if let Some(bitfield) = self.state.bitfield.get_bitfield_mut() {
            if bitfield.has(have_idx as usize) {
                return Ok(());
            }

            bitfield.set(have_idx as usize);

            // Notify torrent about piece availability
            if self.state.metadata.is_some() {
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::Have {
                        pid: self.pid,
                        piece_idx: have_idx,
                    })
                    .await;
            }
        }

        // Step 6: Determine if peer became interesting to us
        self.update_interest_status().await?;

        Ok(())
    }

    async fn on_bitfield(&mut self, bitfield_payload: Bytes) -> Result<(), ConnectionError> {
        debug!(
            "Received BITFIELD message ({} bytes)",
            bitfield_payload.len()
        );

        // Check if we already received a bitfield
        if self.state.bitfield.is_received() {
            warn!("Received duplicate bitfield from peer");
            return Err(ConnectionError::Protocol(
                "Duplicate bitfield message".to_string(),
            ));
        }

        if let Some(metadata) = &self.state.metadata {
            let num_pieces = metadata.pieces.len();

            match Bitfield::from_bytes_checked(bitfield_payload, num_pieces) {
                Ok(bitfield) => {
                    self.state.bitfield = BitfieldState::Validated(bitfield);
                }
                Err(e) => {
                    warn!("Invalid bitfield received: {}", e);
                    return Err(ConnectionError::BitfieldError(e));
                }
            }
        } else {
            debug!("Storing bitfield for later validation (no metadata yet)");
            let bitfield = Bitfield::from_bytes_unchecked(bitfield_payload);
            self.state.bitfield.set_received(bitfield);
        }

        self.update_interest_status().await?;

        debug!("Bitfield processed successfully");

        Ok(())
    }

    async fn on_request(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }
    async fn on_incoming_piece(&mut self, block: Block) -> Result<(), ConnectionError> {
        self.state.metrics.record_download(block.data.len() as u64);
        let block_info = BlockInfo {
            index: block.index,
            begin: block.begin,
            length: block.data.len() as u32,
        };

        if self.state.outgoing_request.remove(&block_info) {
            let _ = self
                .torrent_tx
                .send(TorrentMessage::ReceiveBlock(self.pid, block))
                .await;

            if self.state.slow_start {
                // BUFFERBLOAT PROTECTION
                // Calculate how many seconds of data are currently queued
                let pending_bytes = self.state.outgoing_request.len() as u64 * 16384;
                let current_rate = self.state.metrics.get_download_rate().max(1); // avoid div/0

                let queue_duration_secs = pending_bytes / current_rate;

                // If we have more than 5 seconds of data queued, STOP increasing.
                // We are requesting faster than the peer can send.
                if queue_duration_secs < 5 {
                    self.state.target_request_queue += 1;

                    // Cap it at max_outgoing_request
                    if self.state.target_request_queue > self.state.max_outgoing_request {
                        self.state.target_request_queue = self.state.max_outgoing_request;
                    }
                } else {
                    warn!(
                        "Bufferbloat detected ({}s queue). Exiting slow start.",
                        queue_duration_secs
                    );
                    self.state.slow_start = false;
                    // Note: We don't reset target_request_queue here immediately;
                    // the metric_update tick (every 1s) will snap it back to BDP.
                    // But request_block() will stop sending because in_flight (huge) > target (will be BDP).
                }
            }
            // Calculate how many seconds of data are currently queued
            let pending_bytes = self.state.outgoing_request.len() as u64 * 16384;
            let current_rate = self.state.metrics.get_download_rate().max(1);

            let queue_duration_secs = pending_bytes / current_rate;

            // If we have more than 5 seconds of data queued, STOP increasing.
            // We are requesting faster than the peer can send.
            if queue_duration_secs < 5 {
                self.state.target_request_queue += 1;

                // Cap it at max_outgoing_request
                if self.state.target_request_queue > self.state.max_outgoing_request {
                    self.state.target_request_queue = self.state.max_outgoing_request;
                }
            } else {
                warn!(
                    "Bufferbloat detected ({}s queue). Exiting slow start.",
                    queue_duration_secs
                );
                self.state.slow_start = false;
                // Note: We don't reset target_request_queue here immediately;
                // the metric_update tick (every 1s) will snap it back to BDP.
                // But request_block() will stop sending because in_flight (huge) > target (will be BDP).
            }

            self.request_block().await?;
            self.send_block_request().await?;
        } else {
            debug!("Recv a non-requested block: {block_info:?}");
        }

        Ok(())
    }
    async fn on_cancel(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }

    async fn maybe_request_medata(&mut self) -> Result<(), ConnectionError> {
        if self.state.metadata.is_some() {
            return Ok(());
        }
        // If extension map to id 0, the peer removed in runtime the extension support
        let client_support_metadata_yet = self
            .state
            .remote_extensions
            .get(EXTENSION_NAME_METADATA)
            .map(|ext| *ext != 0)
            .unwrap_or(false);

        if client_support_metadata_yet {
            self.request_metadata().await?
        }

        Ok(())
    }

    async fn request_metadata(&mut self) -> Result<(), ConnectionError> {
        let (otx, orx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::FillMetadataRequest {
                pid: self.pid,
                metadata_piece: otx,
            })
            .await;

        if let Ok(piece) = orx.await {
            let extension_id = self.state.remote_extensions[EXTENSION_NAME_METADATA];
            let payload = Bencode::encode(&MetadataMessage::Request { piece }).into();
            let raw_message = RawExtendedMessage {
                id: extension_id as u8,
                payload,
            };

            debug!(?raw_message, "---------- requesting metadata piece {piece}");

            self.state
                .sink
                .send(Message::Extended(ExtendedMessage::ExtensionMessage(
                    raw_message,
                )))
                .await?;
        }

        Ok(())
    }

    // Register Extensions that peer supports
    async fn on_extended_handshake(
        &mut self,
        handshake: ExtendedHandshake,
    ) -> Result<(), ConnectionError> {
        debug!(?handshake);

        if let Some(m) = handshake.m {
            // METATADA EXTENSION HANDLING
            if let Some(id) = m.get(EXTENSION_NAME_METADATA) {
                self.state
                    .remote_extensions
                    .insert(EXTENSION_NAME_METADATA.to_string(), *id);
            }
            if let Some(metadata_size) = handshake.metadata_size
                && metadata_size > 0
            {
                self.state.metadata_size = metadata_size as usize;
            }

            if self.state.metadata.is_none() {
                debug!("SHOULD REQUEST METADATA");
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::PeerWithMetadata {
                        pid: self.pid,
                        metadata_size: self.state.metadata_size,
                    })
                    .await;

                self.maybe_request_medata().await?;
            }
        }

        if let Some(reqq) = handshake.reqq
            && reqq > 0
        {
            self.state.max_outgoing_request = self.state.max_outgoing_request.max(reqq as usize);
        }

        Ok(())
    }

    // TODO: Improve this
    async fn have_valid_metadata(&mut self) {
        let (otx, orx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::ValidMetadata { resp: otx })
            .await;

        let metadata = orx.await.unwrap();
        self.state.metadata = metadata;
    }

    async fn on_extension(&mut self, raw_msg: RawExtendedMessage) -> Result<(), ConnectionError> {
        debug!("recv raw extended message");
        if raw_msg.id == UT_METADATA_ID {
            self.handle_metadata_message(raw_msg.payload).await?;
        } else {
            tracing::debug!("Received unknown extension message with id: {}", raw_msg.id);
        }

        Ok(())
    }

    async fn handle_metadata_message(&mut self, payload: Bytes) -> Result<(), ConnectionError> {
        use bencode::{Bencode, BencodeDict};

        //  TODO: Move this to protocol

        // Parse the bencode part to get the message info
        let decoded = Bencode::decode(&payload).map_err(|e| {
            ConnectionError::Protocol(format!("Failed to decode metadata message: {}", e))
        })?;

        let dict = match &decoded {
            Bencode::Dict(dict) => dict,
            _ => {
                return Err(ConnectionError::Protocol(
                    "Metadata message is not a dictionary".to_string(),
                ));
            }
        };

        let msg_type = dict.get_i64(b"msg_type").ok_or_else(|| {
            ConnectionError::Protocol("Missing msg_type in metadata message".to_string())
        })?;

        let piece = dict.get_i64(b"piece").ok_or_else(|| {
            ConnectionError::Protocol("Missing piece in metadata message".to_string())
        })? as u32;

        match msg_type {
            REQUEST_ID => {
                // Request - peer is asking us for metadata (we don't handle this yet)
                tracing::debug!("Received metadata request for piece {}", piece);

                let extension_id = self.state.remote_extensions[EXTENSION_NAME_METADATA];
                let reject_metadata_msg = MetadataMessage::Reject { piece };
                let payload = Bencode::encode(&reject_metadata_msg).into();

                let raw_message = RawExtendedMessage {
                    id: extension_id as u8,
                    payload,
                };

                self.state
                    .sink
                    .send(Message::Extended(ExtendedMessage::ExtensionMessage(
                        raw_message,
                    )))
                    .await?
            }
            DATA_ID => {
                // Data - peer is sending us metadata
                let _total_size = dict.get_i64(b"total_size").ok_or_else(|| {
                    ConnectionError::Protocol(
                        "Missing total_size in metadata data message".to_string(),
                    )
                })? as usize;

                // Calculate where the actual data starts after the bencode dictionary
                let bencode_data = Bencode::encoder(&decoded);
                let data_start = bencode_data.len();

                if payload.len() <= data_start {
                    return Err(ConnectionError::Protocol(
                        "No metadata data found after bencode header".to_string(),
                    ));
                }

                // Slice the payload to get just the metadata data part
                let data = payload.slice(data_start..);

                tracing::debug!(
                    "Received metadata piece {}, data size: {}",
                    piece,
                    data.len()
                );

                // Send the metadata piece to the torrent handler using ReceiveMetadata
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::ReceiveMetadata {
                        pid: self.pid,
                        piece_idx: piece,
                        metadata: data,
                    })
                    .await;
            }
            REJECT_ID => {
                // Reject - peer rejected our request
                tracing::debug!("Metadata request for piece {} was rejected", piece);
                // Cancel the metadata request
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::RejectedMetadataRequest {
                        pid: self.pid,
                        rejected_piece: piece,
                    })
                    .await;
            }
            _ => {
                return Err(ConnectionError::Protocol(format!(
                    "Unknown metadata message type: {}",
                    msg_type
                )));
            }
        }

        Ok(())
    }
}

pub fn spawn_outgoing_peer(
    pid: Pid,
    addr: SocketAddr,
    info_hash: InfoHash,
    torrent_tx: mpsc::Sender<TorrentMessage>,
    cmd_rx: mpsc::Receiver<PeerMessage>,
) {
    tokio::spawn(async move {
        let result: Result<(), ConnectionError> = async {
            let p0 = Peer::new(pid, addr, info_hash, torrent_tx.clone(), cmd_rx);
            // Establish connection to remote peer
            let p1 = p0.connect().await?;
            // Handshake remote peer
            let p2 = p1.handshake().await?;
            p2.start().await?;
            Ok(())
        }
        .await;

        if let Err(e) = result {
            warn!(?e);
        }
    });
}
