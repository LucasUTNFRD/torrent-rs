#![allow(dead_code)]
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    time::Instant,
};

use bencode::Bencode;
use bittorrent_common::types::InfoHash;
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
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};
use tokio_util::codec::Framed;
use tracing::{debug, field::debug, info, warn};

use crate::{
    session::CLIENT_ID,
    torrent_refactor::{PeerMessage, Pid, TorrentMessage},
};

///The metadata is handled in blocks of 16KiB (16384 Bytes).
const METADATA_BLOCK_SIZE: usize = 1 << 14;

#[derive(Debug)]
pub enum ConnectionError {
    Network(tokio::io::Error),
    Handshake(String),
    Protocol(String),
    Timeout,
    InvalidHandshake,
}

impl From<tokio::io::Error> for ConnectionError {
    fn from(e: tokio::io::Error) -> Self {
        ConnectionError::Network(e)
    }
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

    have_metadata: bool,
    metadata_size: usize,
    max_outgoing_request: usize,
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

    async fn handle_peer_cmd(&mut self, peer_cmd: PeerMessage) -> Result<(), ConnectionError> {
        match peer_cmd {
            PeerMessage::SendHave { piece_index } => todo!(),
            PeerMessage::SendBitfield { bitfield } => todo!(),
            PeerMessage::SendChoke => todo!(),
            PeerMessage::SendUnchoke => todo!(),
            PeerMessage::Disconnect => todo!(),
            PeerMessage::SendMessage(protocol_msg) => todo!(),
            PeerMessage::HaveMetadata => {
                debug!("HAVE METADATA");
                self.state.have_metadata = true;
            }
        }
        Ok(())
    }

    async fn send_extended_handshake(&mut self) -> Result<(), ConnectionError> {
        let mut extensions = BTreeMap::new();
        extensions.insert(EXTENSION_NAME_METADATA.to_string(), 1);
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
        debug("recv keepalive");
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
            // request block for this torrent
            // send blocks
        }
        Ok(())
    }

    fn request_block(&self) -> Result<(), ConnectionError> {
        // check that the metadata is valid
        todo!()
    }

    async fn on_interested(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }
    async fn on_not_interested(&mut self) -> Result<(), ConnectionError> {
        todo!()
    }

    // Updates piece availability - Affects which pieces the torrent tries to download next
    // Enables requests - May make this peer interesting if they have pieces we want
    // Peer classification - May identify peer as a seed
    async fn on_have(&mut self) -> Result<(), ConnectionError> {
        debug!("recv have");
        Ok(())
    }

    // we can only verify bitfield if we have first the whole metadata
    //
    // Establishes peer capabilities - We know exactly what the peer has
    // Enables piece selection - Piece picker can make informed decisions
    // Interest determination - Determines if we want to download from this peer
    // Connection management - May disconnect redundant connections
    // Performance optimization - Avoids individual HAVE messages for initial state
    async fn on_bitfield(&mut self) -> Result<(), ConnectionError> {
        debug!("recv bitfield");
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
        // If extension map to id 0, the peer removed in runtime the extension support
        let client_support_metadata_yet = self
            .state
            .remote_extensions
            .get(EXTENSION_NAME_METADATA)
            .map(|ext| *ext != 0)
            .unwrap_or(false);

        if dbg!(client_support_metadata_yet && !self.state.have_metadata) {
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

            debug!(?raw_message, "requesting metadata");

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
                debug("SHOULD REQUEST METADATA");
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

    const UT_METADATA_ID: u8 = 1;
    async fn on_extension(&mut self, raw_msg: RawExtendedMessage) -> Result<(), ConnectionError> {
        debug!("recv raw extended message");
        if raw_msg.id == Self::UT_METADATA_ID {
            self.handle_metadata_message(raw_msg.payload).await?;
        } else {
            tracing::debug!("Received unknown extension message with id: {}", raw_msg.id);
        }

        Ok(())
    }

    // Metadata
    async fn handle_metadata_message(&mut self, payload: Bytes) -> Result<(), ConnectionError> {
        use bencode::{Bencode, BencodeDict};

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

    async fn handle_msg(&mut self, msg: Message) -> Result<(), ConnectionError> {
        match msg {
            Message::KeepAlive => self.on_keepalive().await?,
            Message::Choke => self.on_choke().await?,
            Message::Unchoke => self.on_unchoke().await?,
            Message::Interested => self.on_interested().await?,
            Message::NotInterested => self.on_not_interested().await?,
            Message::Have { piece_index } => self.on_have().await?,
            Message::Bitfield(bit) => self.on_bitfield().await?,
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
            let p0 = Peer::new(pid, addr, info_hash, torrent_tx, cmd_rx);
            // Establish connection to remote peer
            let p1 = p0.connect().await?;
            // Handshake remote peer
            let p2 = p1.handshake().await?;
            p2.start().await?;
            Ok(())
        }
        .await;

        if let Err(err) = result {
            debug!(?err);
        }
    });
}
