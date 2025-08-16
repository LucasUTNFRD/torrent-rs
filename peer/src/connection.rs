use std::net::SocketAddr;

use bittorrent_core::types::{InfoHash, PeerID};
use bytes::BytesMut;
use futures::{SinkExt, StreamExt};
use peer_protocol::{
    MessageDecoder,
    protocol::{self, Handshake},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};
use tokio_util::codec::Framed;

use crate::{PeerError, manager::PeerEvent};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct PeerInfo {
    pid: PeerID,
    info_hash: InfoHash,
    addr: SocketAddr,
}

impl PeerInfo {
    pub fn new(pid: PeerID, info_hash: InfoHash, addr: SocketAddr) -> Self {
        Self {
            pid,
            info_hash,
            addr,
        }
    }

    /// Retrieve the peer address.
    pub fn addr(&self) -> &SocketAddr {
        &self.addr
    }

    /// Retrieve the peer id.
    pub fn peer_id(&self) -> &PeerID {
        &self.pid
    }

    /// Retrieve the peer info hash.
    pub fn info_hash(&self) -> &InfoHash {
        &self.info_hash
    }
}

#[derive(Debug)]
pub struct PeerConnection {
    peer_info: PeerInfo,
    cmd_rx: mpsc::Receiver<PeerEvent>,
    peer_state: PeerState,
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

impl PeerConnection {
    pub fn new(peer_info: PeerInfo, cmd_rx: mpsc::Receiver<PeerEvent>) -> Self {
        Self {
            peer_info,
            cmd_rx,
            peer_state: PeerState::default(),
        }
    }

    // async fn handshake_peer(&self, stream: &mut TcpStream) -> Result<(), PeerError> {
    //     let handshake = Handshake::new(*self.peer_info.peer_id(), *self.peer_info.info_hash());
    //     stream
    //         .write_all(&handshake.to_bytes())
    //         .await
    //         .map_err(PeerError::IoError)?;
    //
    //     let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);
    //     stream
    //         .read_exact(&mut buf)
    //         .await
    //         .map_err(PeerError::IoError)?;
    //
    //     let remote_handshake = Handshake::from_bytes(&buf).ok_or(PeerError::InvalidHandshake)?;
    //
    //     if remote_handshake.info_hash != *self.peer_info.info_hash() {
    //         return Err(PeerError::InvalidHandshake);
    //     }
    //     Ok(())
    // }

    async fn handshake_peer(&self, stream: &mut TcpStream) -> Result<(), PeerError> {
        tracing::info!(
            "Initiating handshake with peer at {}",
            self.peer_info.addr()
        );
        let handshake = Handshake::new(*self.peer_info.peer_id(), *self.peer_info.info_hash());

        tracing::debug!("Sending handshake message to peer");
        stream
            .write_all(&handshake.to_bytes())
            .await
            .map_err(PeerError::IoError)?;
        tracing::debug!("Handshake sent successfully");

        tracing::debug!("Waiting to receive handshake from peer");
        let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);
        stream
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
        Ok(())
    }

    pub async fn run(mut self, mut stream: TcpStream) -> Result<(), PeerError> {
        //The handshake is a required message and must be the first message transmitted by the client.
        self.handshake_peer(&mut stream).await?;

        let framed = Framed::new(stream, MessageDecoder {});
        let (mut sink, mut stream) = framed.split();

        loop {
            tokio::select! {
                maybe_msg = stream.next() => {
                    match maybe_msg {
                        Some(Ok(msg)) => self.handle_msg(&msg),
                        Some(Err(e)) => return Err(PeerError::IoError(e)),
                        None => return Err(PeerError::Disconnected),
                    }
                }
                maybe_event= self.cmd_rx.recv() => {
                    match maybe_event {
                        Some(PeerEvent::SendMessage(msg)) => {
                            sink.send(msg).await.map_err(PeerError::IoError)?;
                        }
                        Some(PeerEvent::Disconnect) => {
                            // return Err(PeerError::Disconnect);
                        }
                        None => break,
                    }
                }
            }
        }

        Ok(())
    }

    fn handle_msg(&mut self, msg: &protocol::Message) {
        use protocol::Message::*;
        match msg {
            KeepAlive => todo!(),
            Choke => todo!(),
            Unchoke => todo!(),
            Interested => todo!(),
            NotInterested => todo!(),
            Have { piece_index: u32 } => todo!(),
            Bitfield(Bytes) => todo!(),
            Request(BlockInfo) => todo!(),
            Piece(Block) => todo!(),
            Cancel(BlockInfo) => todo!(),
        }
    }
}

// impl Drop for PeerConnection {
//     fn drop(&mut self) {
//         // self.manager_tx.send(TrackerMessage::Disconnect)
//     }
// }
