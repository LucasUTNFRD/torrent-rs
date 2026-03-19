use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use crate::{
    metrics::counters::inc_connected,
    protocol::{
        extension::{
            DATA_ID, ExtendedHandshake, ExtendedMessage, MetadataMessage, REJECT_ID, REQUEST_ID,
            RawExtendedMessage,
        },
        peer_wire::{Block, BlockInfo, Handshake, Message, MessageCodec},
    },
};
use bittorrent_common::{
    metainfo::Info,
    types::{InfoHash, PeerID},
};
use bytes::{Bytes, BytesMut};
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
    time::{Instant, interval},
};
use tokio_util::codec::Framed;
use tracing::debug;

use crate::{
    bitfield::{Bitfield, BitfieldError},
    events::peer::Direction,
    metrics::counters,
    net::{ConnectTimeout, TcpStream},
    peer::{PeerMessage, metrics::PeerMetrics},
    session::CLIENT_ID,
    torrent::{Pid, TorrentMessage},
};

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

    #[error("Self connection detected")]
    SelfConnection,
}

#[derive(Debug, Clone)]
pub(crate) enum BitfieldState {
    NotReceived,
    Received(Bitfield),
    Validated(Bitfield),
}

impl BitfieldState {
    const fn new() -> Self {
        Self::NotReceived
    }

    pub const fn is_received(&self) -> bool {
        !matches!(self, Self::NotReceived)
    }

    pub const fn get_bitfield_mut(&mut self) -> Option<&mut Bitfield> {
        match self {
            Self::NotReceived => None,
            Self::Received(bf) | Self::Validated(bf) => Some(bf),
        }
    }

    pub fn set_received(&mut self, bitfield: Bitfield) {
        *self = Self::Received(bitfield);
    }

    pub fn validate(&mut self, num_pieces: usize) -> Result<(), BitfieldError> {
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

    pub fn ensure_capacity(&mut self, required_pieces: usize) {
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

pub const EXTENSION_NAME_METADATA: &str = "ut_metadata";
pub const EXTENSION_NAME_PEX: &str = "ut_pex";

const UT_METADATA_ID: u8 = 1;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct PeerInfo {
    pub peer_id: PeerID,
    pub source: Direction,

    // Remote state (what the remote peer told us)
    pub remote_choking: bool,    // they are choking us
    pub remote_interested: bool, // they are interested in us

    // Local state (what we are doing to them)
    pub am_choking: bool,    // we are choking them
    pub am_interested: bool, // we are interested in them

    pub snubbed: bool, // no piece in >60s
}

impl PeerInfo {
    fn new(peer_id: PeerID, source: Direction) -> Self {
        Self {
            peer_id,
            source,
            remote_choking: true,
            remote_interested: false,
            am_choking: true,
            am_interested: false,
            snubbed: false,
        }
    }
}

struct PeerConnection {
    pid: Pid,
    peer_addr: SocketAddr,

    // Shared with the handle held by the Torrent/Choker
    peer_info: Arc<RwLock<PeerInfo>>,

    torrent_tx: mpsc::Sender<TorrentMessage>,
    cmd_rx: mpsc::Receiver<PeerMessage>,

    sink: SplitSink<Framed<TcpStream, MessageCodec>, Message>,
    stream: SplitStream<Framed<TcpStream, MessageCodec>>,

    remote_extensions: HashMap<String, i64>,
    supports_extended: bool,

    // remote peer bitfield
    bitfield: BitfieldState,

    // Metadata
    metadata: Option<Arc<Info>>,
    metadata_size: usize,

    // Request pipeline
    request_queue: Vec<BlockInfo>,
    outgoing_requests: HashSet<BlockInfo>,
    target_request_queue: usize,
    max_outgoing_request: usize,

    // Congestion control
    metrics: PeerMetrics,
    slow_start: bool,
    prev_download_rate: f64,

    last_recv_msg: Instant,
    last_block_request: Instant,
}

impl PeerConnection {
    fn new(
        pid: Pid,
        stream: TcpStream,
        peer_addr: SocketAddr,
        peer_info: Arc<RwLock<PeerInfo>>,
        torrent_tx: mpsc::Sender<TorrentMessage>,
        cmd_rx: mpsc::Receiver<PeerMessage>,
        supports_extended: bool,
    ) -> Self {
        let framed = Framed::new(stream, MessageCodec {});
        let (sink, stream) = framed.split();
        Self {
            pid,
            peer_addr,
            peer_info,
            torrent_tx,
            cmd_rx,
            sink,
            stream,
            supports_extended,
            remote_extensions: HashMap::new(),
            bitfield: BitfieldState::new(),
            metadata: None,
            metadata_size: 0,
            request_queue: Vec::new(),
            outgoing_requests: HashSet::new(),
            target_request_queue: 4,
            max_outgoing_request: 250,
            slow_start: true,
            prev_download_rate: 0.0,
            last_recv_msg: Instant::now(),
            last_block_request: Instant::now(),
            metrics: PeerMetrics::new(),
        }
    }

    async fn handshake(
        stream: &mut TcpStream,
        local_peer_id: PeerID,
        info_hash: InfoHash,
    ) -> Result<(PeerID, bool), ConnectionError> {
        let handshake = Handshake::new(local_peer_id, info_hash);
        stream.write_all(&handshake.to_bytes()).await?;

        let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);
        stream.read_exact(&mut buf).await?;

        let remote = Handshake::from_bytes(&buf).ok_or(ConnectionError::InvalidHandshake)?;

        if remote.info_hash != info_hash {
            return Err(ConnectionError::InvalidHandshake);
        }
        if remote.peer_id == local_peer_id {
            return Err(ConnectionError::SelfConnection);
        }

        Ok((remote.peer_id, remote.support_extended_message()))
    }

    // ── Event loop ────────────────────────────────────────────────────────────
    async fn run(mut self) -> Result<(), ConnectionError> {
        self.have_valid_metadata().await;

        if self.supports_extended {
            self.send_extended_handshake().await?;
        }

        let mut heartbeat = interval(Duration::from_secs(60));
        let mut metric_tick = interval(Duration::from_millis(500));
        heartbeat.tick().await;
        metric_tick.tick().await;

        loop {
            self.maybe_request_metadata().await?;

            tokio::select! {
                maybe_msg = self.stream.next() => match maybe_msg {
                    Some(Ok(msg)) => {
                        counters::on_read_counter();
                        self.handle_msg(msg).await?
                    },
                    _ => break,
                },
                maybe_cmd = self.cmd_rx.recv() => match maybe_cmd {
                    Some(cmd) => self.handle_cmd(cmd).await?,
                    None => break,
                },
                _ = heartbeat.tick() => {
                    let elapsed_request = self.last_block_request.elapsed();
                    let elapsed_recv = self.last_recv_msg.elapsed();
                    let am_interested = self.peer_info.read().unwrap().am_interested;

                    if am_interested && elapsed_request > Duration::from_secs(30)
                        || elapsed_recv > Duration::from_secs(45)
                    {
                        break;
                    }
                    self.sink.send(Message::KeepAlive).await?;
                },
                _ = metric_tick.tick() => self.metric_update(),
            }
        }

        let bitfield = match self.bitfield {
            BitfieldState::Validated(bf) => Some(bf),
            _ => None,
        };
        let _ = self
            .torrent_tx
            .send(TorrentMessage::PeerDisconnected(self.pid, bitfield))
            .await;

        Ok(())
    }

    // ── Info write helpers ────────────────────────────────────────────────────
    //
    // Centralise RwLock write access. Each helper documents what it mutates.

    fn set_remote_choking(&self, val: bool) {
        self.peer_info.write().unwrap().remote_choking = val;
    }

    fn set_remote_interested(&self, val: bool) {
        self.peer_info.write().unwrap().remote_interested = val;
    }

    fn set_am_choking(&self, val: bool) {
        self.peer_info.write().unwrap().am_choking = val;
    }

    fn set_am_interested(&self, val: bool) {
        self.peer_info.write().unwrap().am_interested = val;
    }

    // ── Metric update (1Hz) ───────────────────────────────────────────────────

    fn metric_update(&mut self) {
        self.metrics.update_rates();
        let current_rate = self.metrics.download_rate_f64();

        if self.slow_start {
            let remote_choking = self.peer_info.read().unwrap().remote_choking;
            let plateaued =
                self.prev_download_rate > 0.0 && current_rate + 5000.0 >= self.prev_download_rate;

            if !remote_choking && current_rate > 0.0 && plateaued {
                self.slow_start = false;
                tracing::info!(
                    peer = %self.peer_addr,
                    current_rate,
                    prev_rate = self.prev_download_rate,
                    target_queue = self.target_request_queue,
                    "slow_start exit: rate plateaued"
                );
            } else {
                tracing::debug!(
                    peer = %self.peer_addr,
                    current_rate,
                    prev_rate = self.prev_download_rate,
                    remote_choking,
                    target_queue = self.target_request_queue,
                    "metric_update: still in slow_start"
                );
            }
        } else {
            const BLOCK_SIZE: f64 = 16384.0;
            const QUEUE_TIME: f64 = 3.0;
            let raw_target = (QUEUE_TIME * current_rate / BLOCK_SIZE) as usize;
            let new_target = raw_target.max(4).min(self.max_outgoing_request);
            self.target_request_queue = new_target;
            tracing::debug!(
                peer = %self.peer_addr,
                current_rate,
                raw_target,
                new_target,
                in_flight = self.outgoing_requests.len(),
                "metric_update: steady-state target recalculated"
            );
        }

        self.prev_download_rate = current_rate;
    }

    // ── Message dispatch ──────────────────────────────────────────────────────

    async fn handle_msg(&mut self, msg: Message) -> Result<(), ConnectionError> {
        self.last_recv_msg = Instant::now();
        match msg {
            Message::KeepAlive => {}
            Message::Choke => self.on_choke().await,
            Message::Unchoke => self.on_unchoke().await?,
            Message::Interested => self.on_interested().await?,
            Message::NotInterested => self.on_not_interested().await?,
            Message::Have { piece_index } => self.on_have(piece_index).await?,
            Message::Bitfield(payload) => self.on_bitfield(payload).await?,
            Message::Request(block_info) => self.on_request(block_info).await?,
            Message::Piece(block) => self.on_piece(block).await?,
            Message::Cancel(_) => { /* TODO */ }
            Message::Port { port } => {
                let node_addr = SocketAddr::new(self.peer_addr.ip(), port);
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::DhtAddNode { node_addr })
                    .await;
            }
            Message::Extended(ext) => match ext {
                ExtendedMessage::Handshake(hs) => self.on_extended_handshake(hs).await?,
                ExtendedMessage::ExtensionMessage(raw) => self.on_extension(raw).await?,
            },
        }
        Ok(())
    }

    // ── Command dispatch ──────────────────────────────────────────────────────

    async fn handle_cmd(&mut self, cmd: PeerMessage) -> Result<(), ConnectionError> {
        match cmd {
            PeerMessage::SendHave { piece_index } => {
                if let BitfieldState::Validated(ref bf) = self.bitfield
                    && bf.has(piece_index as usize)
                {
                    self.sink.send(Message::Have { piece_index }).await?;
                }
            }
            PeerMessage::SendBitfield { bitfield } => {
                if !bitfield.as_bytes().is_empty() {
                    self.sink
                        .send(Message::Bitfield(Bytes::copy_from_slice(
                            bitfield.as_bytes(),
                        )))
                        .await?;
                }
            }
            PeerMessage::SendChoke => {
                self.metrics.reset_since_unchoked();
                self.set_am_choking(true);
                self.sink.send(Message::Choke).await?;
            }
            PeerMessage::SendUnchoke => {
                self.set_am_choking(false);
                self.sink.send(Message::Unchoke).await?;
            }
            PeerMessage::HaveMetadata(metadata) => {
                let num_pieces = metadata.num_pieces();
                self.metadata = Some(metadata);
                self.bitfield.validate(num_pieces)?;
                self.update_interest().await?;
            }
            PeerMessage::SendMessage(msg) => {
                self.sink.send(msg).await?;
            }
            PeerMessage::Disconnect => { /* TODO: graceful shutdown */ }
        }
        Ok(())
    }

    // ── Protocol handlers ─────────────────────────────────────────────────────

    async fn on_choke(&mut self) {
        self.set_remote_choking(true);

        // clear the requests that haven't been sent yet
        self.request_queue.clear();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::ClearPeerRequest { pid: self.pid })
            .await;
    }

    async fn on_unchoke(&mut self) -> Result<(), ConnectionError> {
        self.set_remote_choking(false);
        let am_interested = self.peer_info.read().unwrap().am_interested;
        if am_interested {
            self.request_block().await?;
            self.send_block_requests().await?;
        } else {
            self.update_interest().await?;
        }
        Ok(())
    }

    async fn on_interested(&mut self) -> Result<(), ConnectionError> {
        self.set_remote_interested(true);
        let _ = self
            .torrent_tx
            .send(TorrentMessage::Interest(self.pid))
            .await;
        Ok(())
    }

    async fn on_not_interested(&mut self) -> Result<(), ConnectionError> {
        self.set_remote_interested(false);
        let _ = self
            .torrent_tx
            .send(TorrentMessage::NotInterest(self.pid))
            .await;
        Ok(())
    }

    async fn on_have(&mut self, piece_index: u32) -> Result<(), ConnectionError> {
        if !self.bitfield.is_received() {
            self.bitfield
                .set_received(Bitfield::with_size((piece_index + 1) as usize));
        }
        if self.metadata.is_none() {
            self.bitfield.ensure_capacity((piece_index + 1) as usize);
        }

        if let Some(ref meta) = self.metadata {
            let n = meta.pieces.len();
            if piece_index as usize >= n {
                return Err(ConnectionError::Protocol(format!(
                    "HAVE piece index {piece_index} >= num_pieces {n}"
                )));
            }
        } else if piece_index >= 65536 {
            return Err(ConnectionError::Protocol(format!(
                "HAVE piece index {piece_index} exceeds pre-metadata limit"
            )));
        }

        if let Some(bf) = self.bitfield.get_bitfield_mut() {
            if bf.num_pieces == 0 || piece_index as usize >= bf.num_pieces {
                return Ok(());
            }
            if bf.has(piece_index as usize) {
                return Ok(());
            }
            bf.set(piece_index as usize);
            if self.metadata.is_some() {
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::Have {
                        pid: self.pid,
                        piece_idx: piece_index,
                    })
                    .await;
            }
        }

        self.update_interest().await
    }

    async fn on_bitfield(&mut self, payload: Bytes) -> Result<(), ConnectionError> {
        if self.bitfield.is_received() {
            return Err(ConnectionError::Protocol("Duplicate bitfield".into()));
        }
        if let Some(ref meta) = self.metadata {
            let bf = Bitfield::from_bytes_checked(payload, meta.pieces.len())
                .map_err(ConnectionError::BitfieldError)?;
            self.bitfield = BitfieldState::Validated(bf);
        } else {
            self.bitfield
                .set_received(Bitfield::from_bytes_unchecked(payload));
        }
        self.update_interest().await
    }

    async fn on_request(&mut self, block_info: BlockInfo) -> Result<(), ConnectionError> {
        let am_choking = self.peer_info.read().unwrap().am_choking;
        if am_choking {
            return Ok(());
        }
        let _ = self
            .torrent_tx
            .send(TorrentMessage::RemoteBlockRequest {
                pid: self.pid,
                block_info,
            })
            .await;
        Ok(())
    }

    async fn on_piece(&mut self, block: Block) -> Result<(), ConnectionError> {
        self.metrics.record_download(block.data.len() as u64);

        let block_info = BlockInfo {
            index: block.index,
            begin: block.begin,
            length: u32::try_from(block.data.len()).expect("block len > u32::MAX"),
        };

        if !self.outgoing_requests.remove(&block_info) {
            debug!("Received unrequested block: {block_info:?}");
            return Ok(());
        }

        let _ = self
            .torrent_tx
            .send(TorrentMessage::ReceiveBlock(self.pid, block))
            .await;

        self.adjust_pipeline_on_piece();
        self.request_block().await?;
        self.send_block_requests().await?;
        Ok(())
    }

    fn adjust_pipeline_on_piece(&mut self) {
        if self.slow_start {
            self.target_request_queue =
                (self.target_request_queue + 1).min(self.max_outgoing_request);
        }
    }

    // ── Block request pipeline ────────────────────────────────────────────────

    async fn request_block(&mut self) -> Result<(), ConnectionError> {
        if self.metadata.is_none() {
            return Ok(());
        }
        if !self.request_queue.is_empty() {
            tracing::trace!(
                peer = %self.peer_addr,
                pending_in_queue = self.request_queue.len(),
                "request_block: request_queue non-empty, skipping refill"
            );
            return Ok(());
        }

        let in_flight = self.outgoing_requests.len();
        if in_flight >= self.target_request_queue {
            tracing::debug!(
                peer = %self.peer_addr,
                in_flight,
                target_queue = self.target_request_queue,
                slow_start = self.slow_start,
                "request_block: pipeline full, not requesting more"
            );
            return Ok(());
        }

        let slots = self.target_request_queue - in_flight;
        tracing::debug!(
            peer = %self.peer_addr,
            in_flight,
            target_queue = self.target_request_queue,
            slots,
            slow_start = self.slow_start,
            "request_block: fetching blocks from session"
        );

        let BitfieldState::Validated(bitfield) = self.bitfield.clone() else {
            return Ok(());
        };

        let (tx, rx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::RequestBlock {
                pid: self.pid,
                max_requests: slots,
                bitfield,
                block_tx: tx,
            })
            .await;

        let blocks = rx.await.expect("TorrentSession dropped oneshot sender");
        let received = blocks.len();
        self.request_queue.extend(blocks);
        tracing::debug!(
            peer = %self.peer_addr,
            slots_requested = slots,
            blocks_received = received,
            "request_block: session returned blocks"
        );
        Ok(())
    }

    async fn send_block_requests(&mut self) -> Result<(), ConnectionError> {
        let before = self.outgoing_requests.len();
        while let Some(block) = self.request_queue.pop() {
            if self.outgoing_requests.len() > self.max_outgoing_request {
                break;
            }
            self.outgoing_requests.insert(block);
            self.last_block_request = Instant::now();
            self.sink.feed(Message::Request(block)).await?;
        }
        self.sink.flush().await?;
        let after = self.outgoing_requests.len();
        if after != before {
            tracing::debug!(
                peer = %self.peer_addr,
                sent = after - before,
                in_flight = after,
                target_queue = self.target_request_queue,
                remaining_in_local_queue = self.request_queue.len(),
                "send_block_requests: sent wire requests"
            );
        }
        Ok(())
    }

    // ── Interest ──────────────────────────────────────────────────────────────

    async fn update_interest(&mut self) -> Result<(), ConnectionError> {
        if self.peer_info.read().unwrap().am_interested {
            return Ok(());
        }
        let BitfieldState::Validated(ref bf) = self.bitfield else {
            return Ok(());
        };

        let (tx, rx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::ShouldBeInterested {
                pid: self.pid,
                bitfield: bf.clone(),
                resp_tx: tx,
            })
            .await;

        let interested = rx.await.expect("sender dropped");
        self.set_am_interested(interested);

        if interested {
            self.request_block().await?;
            self.send_block_requests().await?;
        }
        Ok(())
    }

    // ── Extended / metadata ───────────────────────────────────────────────────

    async fn send_extended_handshake(&mut self) -> Result<(), ConnectionError> {
        let mut extensions = BTreeMap::new();
        extensions.insert(EXTENSION_NAME_METADATA.to_string(), UT_METADATA_ID.into());

        let hs = ExtendedHandshake::new()
            .with_extensions(extensions)
            .with_client_version("torrent-rs 0.1.0")
            .with_request_queue_size(250);

        self.sink
            .send(Message::Extended(ExtendedMessage::Handshake(hs)))
            .await?;

        Ok(())
    }

    async fn on_extended_handshake(
        &mut self,
        hs: ExtendedHandshake,
    ) -> Result<(), ConnectionError> {
        if let Some(m) = hs.m {
            if let Some(&id) = m.get(EXTENSION_NAME_METADATA) {
                self.remote_extensions
                    .insert(EXTENSION_NAME_METADATA.to_string(), id);
            }
            if let Some(sz) = hs.metadata_size.filter(|&s| s > 0) {
                self.metadata_size = sz as usize;
            }
            if self.metadata.is_none() {
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::PeerWithMetadata {
                        pid: self.pid,
                        metadata_size: self.metadata_size,
                    })
                    .await;
                self.maybe_request_metadata().await?;
            }
        }
        if let Some(reqq) = hs.reqq.filter(|&r| r > 0) {
            self.max_outgoing_request = self.max_outgoing_request.max(reqq as usize);
        }
        // if let Some(v) = hs.v {
        // TODO: v from extendd is consider source of truth for getting client name
        // }
        Ok(())
    }

    async fn on_extension(&mut self, raw: RawExtendedMessage) -> Result<(), ConnectionError> {
        if raw.id == UT_METADATA_ID {
            self.handle_metadata_message(raw.payload).await?;
        }
        Ok(())
    }

    async fn maybe_request_metadata(&mut self) -> Result<(), ConnectionError> {
        if self.metadata.is_some() {
            return Ok(());
        }
        let supported = self
            .remote_extensions
            .get(EXTENSION_NAME_METADATA)
            .is_some_and(|&id| id != 0);
        if supported {
            self.request_metadata().await?;
        }
        Ok(())
    }

    async fn request_metadata(&mut self) -> Result<(), ConnectionError> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::FillMetadataRequest {
                pid: self.pid,
                metadata_piece: tx,
            })
            .await;
        let Ok(piece) = rx.await else {
            return Ok(());
        };

        let ext_id = self.remote_extensions[EXTENSION_NAME_METADATA] as u8;
        let payload = bencode::Bencode::encode(&MetadataMessage::Request { piece }).into();
        self.sink
            .send(Message::Extended(ExtendedMessage::ExtensionMessage(
                RawExtendedMessage {
                    id: ext_id,
                    payload,
                },
            )))
            .await?;
        Ok(())
    }

    async fn handle_metadata_message(&mut self, payload: Bytes) -> Result<(), ConnectionError> {
        use bencode::{Bencode, BencodeDict};

        let decoded = Bencode::decode(&payload)
            .map_err(|e| ConnectionError::Protocol(format!("bad metadata bencode: {e}")))?;
        let Bencode::Dict(ref dict) = decoded else {
            return Err(ConnectionError::Protocol(
                "metadata message not a dict".into(),
            ));
        };

        let msg_type = dict
            .get_i64(b"msg_type")
            .ok_or_else(|| ConnectionError::Protocol("missing msg_type".into()))?;
        let piece = dict
            .get_i64(b"piece")
            .ok_or_else(|| ConnectionError::Protocol("missing piece".into()))?
            as u32;

        match msg_type {
            REQUEST_ID => {
                let ext_id = self.remote_extensions[EXTENSION_NAME_METADATA] as u8;
                let payload = bencode::Bencode::encode(&MetadataMessage::Reject { piece }).into();
                self.sink
                    .send(Message::Extended(ExtendedMessage::ExtensionMessage(
                        RawExtendedMessage {
                            id: ext_id,
                            payload,
                        },
                    )))
                    .await?;
            }
            DATA_ID => {
                let _total_size = dict
                    .get_i64(b"total_size")
                    .ok_or_else(|| ConnectionError::Protocol("missing total_size".into()))?
                    as usize;
                let data_start = bencode::Bencode::encoder(&decoded).len();
                if payload.len() <= data_start {
                    return Err(ConnectionError::Protocol(
                        "no data after bencode header".into(),
                    ));
                }
                let _ = self
                    .torrent_tx
                    .send(TorrentMessage::ReceiveMetadata {
                        pid: self.pid,
                        piece_idx: piece,
                        metadata: payload.slice(data_start..),
                    })
                    .await;
            }
            REJECT_ID => {
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
                    "unknown metadata msg_type: {msg_type}"
                )));
            }
        }
        Ok(())
    }

    async fn have_valid_metadata(&mut self) {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::ValidMetadata { resp: tx })
            .await;
        self.metadata = rx.await.unwrap();
    }
}

// ── PeerHandle (what TorrentSession and the global choker hold) ──────────────
//
// Clone-able. The info Arc lets the choker read state without a message round
// trip. The tx channel is for sending commands into the peer task.
#[derive(Debug, Clone)]
pub struct PeerHandle {
    pub pid: Pid,
    pub peer_addr: SocketAddr,
    pub info: Arc<RwLock<PeerInfo>>,
    pub tx: mpsc::Sender<PeerMessage>,
}

// ── Public spawn API ──────────────────────────────────────────────────────────
//
// Two functions, one per direction. Both return PeerHandle immediately;
// the actual work runs in a detached task. Errors are reported via
// TorrentMessage::PeerError.

/// Spawn an outbound connection. Connects, handshakes, then runs the event loop.
pub fn spawn_outbound(
    pid: Pid,
    addr: SocketAddr,
    info_hash: InfoHash,
    local_peer_id: PeerID,
    torrent_tx: mpsc::Sender<TorrentMessage>,
) -> PeerHandle {
    let (tx, rx) = mpsc::channel(256);
    // PeerInfo initialised with a placeholder peer_id; updated after handshake
    let info = Arc::new(RwLock::new(PeerInfo::new(*CLIENT_ID, Direction::Outbound)));

    let handle = PeerHandle {
        pid,
        peer_addr: addr,
        info: Arc::clone(&info),
        tx,
    };

    let tx_err = torrent_tx.clone();
    tokio::spawn(async move {
        let result: Result<(), ConnectionError> = async {
            let mut stream = TcpStream::connect_timeout(&addr, CONNECTION_TIMEOUT).await?;
            let (remote_peer_id, supports_extended) =
                PeerConnection::handshake(&mut stream, local_peer_id, info_hash).await?;
            inc_connected();

            // Update real peer_id now that handshake is done
            info.write().unwrap().peer_id = remote_peer_id;

            PeerConnection::new(
                pid,
                stream,
                addr,
                Arc::clone(&info),
                torrent_tx,
                rx,
                supports_extended,
            )
            .run()
            .await
        }
        .await;

        if let Err(e) = result {
            debug!(peer = %addr, error = %e, "Outbound peer failed");
            let _ = tx_err.send(TorrentMessage::PeerError(pid, e, None)).await;
        }
    });

    handle
}

/// Spawn an inbound connection. The caller has already performed the TCP handshake
/// (done in the listener/session layer). We take the raw stream and skip directly
/// to the BitTorrent handshake.
pub fn spawn_inbound(
    pid: Pid,
    addr: SocketAddr,
    stream: TcpStream,
    // info_hash: InfoHash,
    torrent_tx: mpsc::Sender<TorrentMessage>,
    remote_peer_id: PeerID,
    supports_ext: bool,
) -> PeerHandle {
    let (tx, rx) = mpsc::channel(256);
    let info = Arc::new(RwLock::new(PeerInfo::new(*CLIENT_ID, Direction::Inbound)));

    let handle = PeerHandle {
        pid,
        peer_addr: addr,
        info: Arc::clone(&info),
        tx,
    };

    let tx_err = torrent_tx.clone();
    tokio::spawn(async move {
        let result: Result<(), ConnectionError> = async {
            // let (remote_peer_id, supports_extended) =
            //     PeerConnection::handshake(&mut stream, local_peer_id, info_hash).await?;

            inc_connected();
            info.write().unwrap().peer_id = remote_peer_id;

            PeerConnection::new(
                pid,
                stream,
                addr,
                Arc::clone(&info),
                torrent_tx,
                rx,
                supports_ext,
            )
            .run()
            .await
        }
        .await;

        if let Err(e) = result {
            debug!(peer = %addr, error = %e, "Inbound peer failed");
            let _ = tx_err.send(TorrentMessage::PeerError(pid, e, None)).await;
        }
    });

    handle
}
