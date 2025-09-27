// #![allow(dead_code)]
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::Instant,
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
    protocol::{BlockInfo, Handshake, Message},
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};

use tokio_util::codec::Framed;
use tracing::{debug, info, warn};

use crate::{
    bitfield::{Bitfield, BitfieldError},
    peer::{PeerMessage, metrics::PeerMetrics},
    piece_picker::PiecePicker,
    session::CLIENT_ID,
    torrent_refactor::{Pid, TorrentMessage},
};

///The metadata is handled in blocks of 16KiB (16384 Bytes).
const METADATA_BLOCK_SIZE: usize = 1 << 14;

const UT_METADATA_ID: u8 = 1;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("Network error: {0}")]
    Network(#[from] tokio::io::Error),

    #[error("Handshake failed: {0}")]
    Handshake(String),

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
        let stream = TcpStream::connect(self.addr).await?;

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

    // Download related
    //
    // the blocks we have reserved in the piece
    // picker and will request from this peer.
    // std::vector<pending_block> m_request_queue;

    // the queue of blocks we have requested
    // from this peer
    // std::vector<pending_block> m_download_queue;
    outbound_requests: HashSet<BlockInfo>,
    incoming_request: HashSet<BlockInfo>,

    supports_extended: bool,
    remote_extensions: HashMap<String, i64>,

    last_recv_msg: Instant,

    // Remote peer state
    peer_interested: bool,
    peer_choked: bool,

    // LOCAL STATE
    interesting: bool,
    choked: bool,

    // TODO: merge this two into None or Option<Arc<Info>>
    have_metadata: bool,
    metadata: Option<Arc<Info>>,

    metadata_size: usize,
    max_outgoing_request: usize,

    bitfield: Bitfield,
    bitfield_received: bool,
}

impl ConnectionState for Connected {}
impl Connected {
    pub fn new(stream: TcpStream, supports_extended: bool) -> Self {
        let framed = Framed::new(stream, MessageCodec {});
        let (sink, stream) = framed.split();

        Self {
            sink,
            stream,
            outbound_requests: HashSet::new(),
            incoming_request: HashSet::new(),
            supports_extended,
            last_recv_msg: Instant::now(),
            peer_interested: false,
            peer_choked: true,
            interesting: false,
            choked: true,
            have_metadata: false,
            remote_extensions: HashMap::new(),
            metadata_size: 0,
            max_outgoing_request: 0,
            bitfield: Bitfield::new(),
            bitfield_received: false,
            metadata: None,
        }
    }
}

impl Peer<Connected> {
    pub fn start_from_incoming() -> Result<(), ConnectionError> {
        todo!()
    }

    pub async fn start(mut self) -> Result<(), ConnectionError> {
        self.state.have_metadata = self.have_valid_metadata().await;

        if self.state.have_metadata {
            debug!("HAVE METADATA");
        }

        if self.state.supports_extended {
            self.send_extended_handshake().await?
        }

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
            }
        }

        Ok(())
    }

    async fn handle_msg(&mut self, msg: Message) -> Result<(), ConnectionError> {
        match msg {
            Message::KeepAlive => self.on_keepalive().await?,
            Message::Choke => self.on_choke().await?,
            Message::Unchoke => self.on_unchoke().await?,
            Message::Interested => self.on_interested().await?,
            Message::NotInterested => self.on_not_interested().await?,
            Message::Have { piece_index } => self.on_have(piece_index).await?,
            Message::Bitfield(payload) => self.on_bitfield(payload).await?,
            Message::Request(block) => self.on_request().await?,
            Message::Piece(info) => self.on_piece().await?,
            Message::Cancel(block) => self.on_cancel().await?,
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
            PeerMessage::SendHave { piece_index } => todo!(),
            PeerMessage::SendBitfield { bitfield } => todo!(),
            PeerMessage::SendChoke => todo!(),
            PeerMessage::SendUnchoke => todo!(),
            PeerMessage::Disconnect => todo!(),
            PeerMessage::SendMessage(protocol_msg) => todo!(),
            PeerMessage::HaveMetadata(metadata) => {
                debug!("Received metadata from torrent");

                self.state.have_metadata = true;
                self.state.metadata = Some(metadata);

                // Validate bitfield if we received one before having metadata
                if self.state.bitfield_received && !self.state.bitfield.is_empty() {
                    let num_pieces = self
                        .state
                        .metadata
                        .as_ref()
                        .expect("Bitfiled received and was not empty")
                        .pieces
                        .len();
                    self.state.bitfield.validate(num_pieces)?;
                }

                // Update interest status now that we have metadata
                self.update_interest_status().await?;
            }
        }
        Ok(())
    }

    async fn update_interest_status(&mut self) -> Result<(), ConnectionError> {
        if !self.state.bitfield_received {
            debug!("we did not received the bitfield yet");
            return Ok(());
        }

        if self.state.interesting {
            return Ok(());
        }

        // if we have the bitfield we we dont have the metadata yet, it mean it was not vaildated
        if !self.state.have_metadata {
            debug!("the bitfield is not validated");
            return Ok(());
        }

        debug!("checking interest ----------------");

        // ask torrent manager if we should be interested in this peer
        let (resp_tx, resp_rx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::ShouldBeInterested {
                pid: self.pid,
                bitfield: self.state.bitfield.clone(),
                resp_tx,
            })
            .await;

        self.state.interesting = resp_rx.await.expect("sender dropped");

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
        // todo!()
        debug!("RECV CHOKE");
        Ok(())
    }
    async fn on_unchoke(&mut self) -> Result<(), ConnectionError> {
        // 		INVARIANT_CHECK;
        //
        // 		boost::shared_ptr<torrent> t = m_torrent.lock();
        // 		TORRENT_ASSERT(t);
        //
        // #ifndef TORRENT_DISABLE_EXTENSIONS
        // 		for (extension_list_t::iterator i = m_extensions.begin()
        // 			, end(m_extensions.end()); i != end; ++i)
        // 		{
        // 			if ((*i)->on_unchoke()) return;
        // 		}
        // #endif
        //
        // #ifdef TORRENT_VERBOSE_LOGGING
        // 		peer_log("<== UNCHOKE");
        // #endif
        // 		m_peer_choked = false;
        // 		m_last_unchoked = time_now();
        // 		if (is_disconnecting()) return;
        //
        // 		if (is_interesting())
        // 		{
        // 			request_a_block(*t, *this);
        // 			send_block_requests();
        // 		}
        debug!("UNCHOKE Received");
        self.state.peer_choked = false;
        // todo: update last unchoked interval
        if self.state.interesting {
            self.request_block()?;
            // request block for this torrent
            // send blocks
        }

        Ok(())
    }

    // fn send

    //
    fn request_block(&self) -> Result<(), ConnectionError> {
        if !self.state.have_metadata {
            return Ok(());
        }

        let info = self.state.metadata.as_ref().expect("have metadata").clone();
        let bitfield = &self.state.bitfield;

        // self.torrent_tx.send(TorrentMessage::NeedTask(self.pid));

        let mut p = PiecePicker::new(info);
        let dq_size = self.download_queue_size();

        p.pick_piece(bitfield, dq_size);

        Ok(())
    }

    fn download_queue_size(&self) -> usize {
        let metrics = PeerMetrics::new();
        let download_rate = metrics.get_download_rate(); //metric in bytes per second

        // assumes the latency of the link to be 1 second.
        // https://blog.libtorrent.org/2011/11/requesting-pieces/
        let m_desired_queue_size = download_rate / METADATA_BLOCK_SIZE as u64;
        m_desired_queue_size as usize
    }

    fn send_block_request() {}

    async fn on_interested(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }

    async fn on_not_interested(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }

    // Updates piece availability - Affects which pieces the torrent tries to download next
    // Enables requests - May make this peer interesting if they have pieces we want
    async fn on_have(&mut self, have_idx: u32) -> Result<(), ConnectionError> {
        debug!("Received HAVE message for piece {}", have_idx);

        // Step 1: Bitfield Initialization - If no bitfield received yet, treat peer as having none
        if !self.state.bitfield_received && self.state.bitfield.is_empty() {
            debug!("First HAVE message without bitfield - initializing empty bitfield");
            // Initialize with minimal size, will be resized as needed
            self.state.bitfield = Bitfield::with_size(1);
        }

        // Step 2: Dynamic Resizing - If no metadata yet, resize bitfield to accommodate new piece index
        if !self.state.have_metadata {
            let required_pieces = (have_idx + 1) as usize;
            if required_pieces > self.state.bitfield.size() {
                debug!(
                    "Resizing bitfield from {} to {} pieces",
                    self.state.bitfield.size(),
                    required_pieces
                );
                self.state.bitfield.resize(required_pieces);
            }
        }

        // Step 3: Index Validation - Ensure piece index is within valid range
        if self.state.have_metadata {
            let num_pieces = self
                .state
                .metadata
                .as_ref()
                .expect("Have metadata")
                .pieces
                .len();

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
            // Pre-metadata validation - cap at 64k pieces - based on libtorrent codebase
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

        // Step 4: Update Peer State
        // - Set bit in peer's bitfield and increment piece count
        let had_piece_before = self.state.bitfield.has(have_idx as usize);
        if !had_piece_before {
            self.state.bitfield.set(have_idx as usize);

            // Notify torrent about piece availability
            let _ = self
                .torrent_tx
                .send(TorrentMessage::Have {
                    pid: self.pid,
                    piece_idx: have_idx,
                })
                .await;
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
        if self.state.bitfield_received {
            warn!("Received duplicate bitfield from peer");
            return Err(ConnectionError::Protocol(
                "Duplicate bitfield message".to_string(),
            ));
        }

        // Step 1: Reception Flag - Mark that bitfield has been received
        self.state.bitfield_received = true;

        if self.state.have_metadata {
            // Step 2: Size Validation - Verify bitfield size matches expected piece count
            let num_pieces = self
                .state
                .metadata
                .as_ref()
                .expect("have metatada flag without Some(metadata)")
                .pieces
                .len();

            match Bitfield::from_bytes_checked(bitfield_payload, num_pieces) {
                Ok(bitfield) => {
                    self.state.bitfield = bitfield;
                }
                Err(e) => {
                    warn!("Invalid bitfield received: {}", e);
                    return Err(ConnectionError::BitfieldError(e));
                }
            }
        } else {
            // Step 5: Pre-Metadata Handling - Just store bitfield and detect potential seeds
            debug!("Storing bitfield for later validation (no metadata yet)");

            // Store bitfield for torrent even without validation
            self.state.bitfield = Bitfield::from_bytes_unchecked(bitfield_payload.clone());
        }

        // Step 6: Interest Calculation - Determine if peer is interesting to us
        self.update_interest_status().await?;

        debug!("Bitfield processed successfully");
        Ok(())
    }

    async fn on_request(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }
    async fn on_piece(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }
    async fn on_cancel(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }

    async fn maybe_request_medata(&mut self) -> Result<(), ConnectionError> {
        if self.state.have_metadata {
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

            if dbg!(!self.state.have_metadata) {
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

        // if let Some(v) = handshake.v {}
        // if let Some(reqq) = handshake.reqq
        //     && reqq > 0
        // {
        //     self.state.max_outgoing_request = reqq as usize;
        // }
        // if let Some(p) = handshake.p {
        //     // ingore this for now
        // }
        // if let Some(metadata_size) = handshake.metadata_size
        //     && metadata_size > 0
        // {
        //     self.state.metadata_size = metadata_size as usize;
        // }

        Ok(())
    }

    /// Ask torrent actor if we have a valid metada file
    async fn have_valid_metadata(&self) -> bool {
        let (otx, orx) = oneshot::channel();
        let _ = self
            .torrent_tx
            .send(TorrentMessage::ValidMetadata { resp: otx })
            .await;

        orx.await.unwrap_or(false)
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

    // Metadata
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

        if let Err(err) = result {
            warn!(?err);
            let _ = torrent_tx.send(TorrentMessage::PeerError(pid, err)).await;
        } else {
            warn!("disconnected");
            let _ = torrent_tx.send(TorrentMessage::PeerDisconnected(pid)).await;
        }
    });
}
