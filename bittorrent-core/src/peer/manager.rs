// #![allow(dead_code)]
// use std::{
//     collections::{HashMap, HashSet},
//     net::SocketAddr,
//     sync::{
//         Arc,
//         atomic::{AtomicUsize, Ordering},
//     },
//     time::Duration,
// };
//
// use super::Bitfield;
//
// use super::piece_picker::Picker;
// use bytes::Bytes;
// use peer_protocol::protocol::{Block, BlockInfo, Message};
// use piece_cache::PieceCache;
// use tokio::{
//     sync::{mpsc, oneshot},
//     time::interval,
// };
//
// use crate::{
//     peer::{
//         choker::Choker,
//         connection::{PeerInfo, spawn_incoming_peer, spawn_outgoing_peer},
//     },
//     storage::Storage,
//     torrent_refactor::{Metadata, PeerSource},
// };
//
// use super::error::PeerError;
//
// pub enum ManagerCommand {
//     AddPeer(PeerSource),
// }
//
// // #[derive(Debug)]
// // pub enum PeerEvent {
// //     Have { pid: Id, piece_idx: u32 },
// //     Bitfield(Id, Bytes),
// //     PeerError(Id, PeerError),
// //     AddBlock(Id, Block),
// //     BlockRequest(Id, BlockInfo),
// //     Interest(Id),
// //     NotInterest(Id),
// //     NeedTask(Id),
// // }
// //
// // #[derive(Debug)]
// // pub enum PeerCommand {
// //     SendMessage(Message),
// //     RejectRequest(BlockInfo),
// //     Disconnect,
// //     AvailableTask(Vec<BlockInfo>),
// // }
//
// #[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
// pub struct Id(pub usize);
// static PEER_COUNTER: AtomicUsize = AtomicUsize::new(0);
//
// enum ManagerState {
//     WaitingForMetadata,
//     Active {
//         /// Bitfield tracking our downloaded pieces
//         bitfield: Bitfield,
//         /// Piece selection strategy
//         picker: Picker,
//         /// Piece caching for incomplete pieces
//         cache: PieceCache,
//     },
// }
//
// pub struct PeerManager {
//     metadata: Arc<Metadata>,
//     peers: HashMap<Id, PeerState>,
//
//     manager_rx: mpsc::UnboundedReceiver<ManagerCommand>,
//
//     peer_event_tx: mpsc::Sender<PeerEvent>,
//     peer_event_rx: mpsc::Receiver<PeerEvent>,
//
//     connected_addrs: HashSet<SocketAddr>,
//
//     state: ManagerState,
//     choker: Choker,
//     storage: Arc<Storage>,
// }
//
// #[derive(Debug, Clone)]
// struct PeerState {
//     pub pid: Id,
//     pub addr: SocketAddr,
//     pub sender: mpsc::Sender<PeerCommand>,
// }
//
// impl PeerManager {
//     pub fn new(
//         metadata: Arc<Metadata>,
//         storage: Arc<Storage>,
//     ) -> (Self, mpsc::UnboundedSender<ManagerCommand>) {
//         let (manager_tx, manager_rx) = mpsc::unbounded_channel();
//         let (peer_event_tx, peer_event_rx) = mpsc::channel(64);
//
//         let state = if let Metadata::TorrentFile(torrent_info) = metadata.as_ref() {
//             ManagerState::Active {
//                 bitfield: Bitfield::new(torrent_info.num_pieces()),
//                 picker: Picker::new(torrent_info.clone()),
//                 cache: PieceCache::new(torrent_info.clone()),
//             }
//         } else {
//             ManagerState::WaitingForMetadata
//         };
//
//         let manager = Self {
//             peers: HashMap::new(),
//             manager_rx,
//             peer_event_tx,
//             peer_event_rx,
//             metadata,
//             storage,
//             state,
//             connected_addrs: HashSet::new(),
//             choker: Choker::new(),
//         };
//
//         (manager, manager_tx)
//     }
//
//     pub async fn run(mut self) {
//         let mut choke_interval = interval(Duration::from_secs(10));
//         loop {
//             tokio::select! {
//                 maybe_cmd= self.manager_rx.recv()=>{
//                     match maybe_cmd {
//                         Some(cmd) => self.handle_cmd(cmd).await,
//                         None => break,
//                     }
//                 },
//                 maybe_peer_cmd= self.peer_event_rx.recv()=>{
//                     match maybe_peer_cmd {
//                         Some(event) => self.handle_peer_event(event).await,
//                         None => tracing::warn!("peer_event channel closed"),
//                     }
//                 },
//                 _ = choke_interval.tick()  => {
//                     self.run_choke_algorithm().await;
//                 }
//             }
//         }
//     }
//
//     // Add this method to run the choke algorithm
//     async fn run_choke_algorithm(&mut self) {}
//
//     async fn send_choke_unchoke_messages(&self, to_choke: Vec<Id>, to_unchoke: Vec<Id>) {
//         // // Send choke messages
//         // for peer_id in to_choke {
//         //     if let Some(peer) = self.peers.get(&peer_id) {
//         //         let _ = peer
//         //             .sender
//         //             .send(PeerCommand::SendMessage(Message::Choke))
//         //             .await;
//         //         tracing::debug!("Sent choke to peer {}", peer_id.0);
//         //     }
//         // }
//         //
//         // // Send unchoke messages
//         // for peer_id in to_unchoke {
//         //     if let Some(peer) = self.peers.get(&peer_id) {
//         //         let _ = peer
//         //             .sender
//         //             .send(PeerCommand::SendMessage(Message::Unchoke))
//         //             .await;
//         //         tracing::info!("Sent unchoke to peer {}", peer_id.0);
//         //     }
//         // }
//     }
//
//     fn add_peer(&mut self, peer: PeerSource) {
//         let addr = peer.get_addr();
//
//         let id = PEER_COUNTER.fetch_add(1, Ordering::Relaxed);
//         let id = Id(id);
//
//         if self.connected_addrs.contains(&addr) {
//             return;
//         }
//
//         let peer_tx = match peer {
//             PeerSource::Inbound {
//                 stream,
//                 remote_addr,
//                 supports_ext,
//             } => {
//                 let peer_info = PeerInfo::new(id, self.metadata.info_hash(), remote_addr);
//                 spawn_incoming_peer(peer_info, self.peer_event_tx.clone(), stream, supports_ext)
//             }
//             PeerSource::Outbound(remote_addr) => {
//                 let peer_info = PeerInfo::new(id, self.metadata.info_hash(), remote_addr);
//                 spawn_outgoing_peer(peer_info, self.peer_event_tx.clone())
//             }
//         };
//
//         self.peers.insert(
//             id,
//             PeerState {
//                 pid: id,
//                 addr,
//                 sender: peer_tx,
//             },
//         );
//     }
//
//     async fn handle_cmd(&mut self, cmd: ManagerCommand) {
//         use ManagerCommand::*;
//         match cmd {
//             AddPeer(peer) => {
//                 self.add_peer(peer);
//             }
//         }
//     }
//
//     async fn handle_interest(&mut self, pid: Id) {
//         // let peer = match self.peers.get_mut(&pid) {
//         //     Some(p) => p,
//         //     None => return,
//         // };
//         //
//         // if peer.am_interested {
//         //     return;
//         // }
//         //
//         // let should_send_interest = true;
//         //
//         // if should_send_interest {
//         //     peer.am_interested = true;
//         //     let _ = peer
//         //         .sender
//         //         .send(PeerCommand::SendMessage(Message::Interested))
//         //         .await;
//         //
//         //     self.assign_piece(pid).await;
//         // }
//     }
//
//     async fn handle_peer_event(&mut self, cmd: PeerEvent) {
//         use PeerEvent::*;
//         match cmd {
//             Have { pid, piece_idx } => {
//                 let peer = match self.peers.get_mut(&pid) {
//                     Some(peer) => peer,
//                     None => return,
//                 };
//
//                 // if let Err(e) = peer.bitfield.set(piece_idx as usize) {
//                 //     tracing::warn!("set operation failed {e}");
//                 //     return;
//                 // }
//
//                 // self.picker
//                 //     .as_mut()
//                 //     .unwrap()
//                 //     .increment_availability(AvailabilityUpdate::Index(piece_idx));
//
//                 // are we interested?
//                 self.handle_interest(pid).await;
//             }
//             Bitfield(pid, payload) => {
//                 // A bitfield of the wrong length is considered an error.
//                 // Clients should drop the connection if they receive bitfields that are not of the correct size, or if the bitfield has any of the spare bits set.
//                 let peer = match self.peers.get_mut(&pid) {
//                     Some(peer) => peer,
//                     None => return,
//                 };
//
//                 // match bitfield::Bitfield::try_from((payload, self.torrent.num_pieces())) {
//                 //     Ok(bitfield) => {
//                 //         peer.bitfield = bitfield;
//                 //
//                 //         self.picker
//                 //             .as_ref()
//                 //             .unwrap()
//                 //             .increment_availability(AvailabilityUpdate::Bitfield(&peer.bitfield));
//                 //
//                 //         // Send our bitfield if we have at least one piece
//                 //         if !self.bitfield.is_empty() {
//                 //             let _ = peer
//                 //                 .sender
//                 //                 .send(PeerCommand::SendMessage(Message::Bitfield(
//                 //                     self.bitfield.as_bytes(),
//                 //                 )))
//                 //                 .await;
//                 //         }
//                 //
//                 //         self.handle_interest(pid).await;
//                 //     }
//                 //     Err(e) => {
//                 //         tracing::warn!("dropping peer connection - reason {e}");
//                 //         let _ = peer.sender.send(PeerCommand::Disconnect).await;
//                 //         self.clean_up_peer(pid);
//                 //     }
//                 // };
//             }
//             AddBlock(id, block) => {
//                 // // cache the block
//                 // let Some(peer) = self.peers.get_mut(&id) else {
//                 //     return;
//                 // };
//                 //
//                 // // Peer metrics
//                 // peer.stats.last_block = Instant::now();
//                 // peer.stats.downloaded += block.data.len();
//                 //
//                 // if let Some(completed_piece) = self.cache.insert_block(&block) {
//                 //     peer.assigned_piece_idx = None;
//                 //
//                 //     let piece_index = block.index;
//                 //     let torrent_id = self.torrent.info_hash;
//                 //
//                 //     // Mark the piece as writing
//                 //     self.picker
//                 //         .mark_piece_as(piece_index as usize, PieceState::Writing);
//                 //
//                 //     let (verification_tx, verification_rx) = oneshot::channel();
//                 //     self.storage.verify_piece(
//                 //         torrent_id,
//                 //         piece_index,
//                 //         completed_piece.clone(),
//                 //         verification_tx,
//                 //     );
//                 //
//                 //     let valid = verification_rx
//                 //         .await
//                 //         .expect("failed to receive piece validation");
//                 //
//                 //     if valid {
//                 //         self.storage
//                 //             .write_piece(torrent_id, piece_index, completed_piece.clone());
//                 //         self.bitfield.set(piece_index as usize).unwrap();
//                 //         self.picker
//                 //             .mark_piece_as(piece_index as usize, PieceState::Finished);
//                 //         self.broadcast_have(piece_index);
//                 //     } else {
//                 //         //  marking the piece as none results in downloading from zero the piece
//                 //         tracing::warn!("piece {} was invalid", piece_index);
//                 //         self.picker
//                 //             .mark_piece_as(piece_index as usize, PieceState::None);
//                 //         self.cache.drop_piece(piece_index as usize);
//                 //     }
//                 // }
//             }
//             NeedTask(id) => {
//                 // let peer_has_assignment = self
//                 //     .peers
//                 //     .get(&id)
//                 //     .map(|p| p.assigned_piece_idx.is_some())
//                 //     .unwrap_or(false);
//                 //
//                 // if !peer_has_assignment {
//                 //     self.assign_piece(id).await;
//                 // }
//             }
//             PeerError(id, err) => {
//                 tracing::warn!(?err, ?id);
//                 self.clean_up_peer(id);
//             }
//             BlockRequest(id, block_info) => {
//                 // if !self.bitfield.has(block_info.index as usize) {
//                 //     tracing::error!("peer request an index which we dont have");
//                 //     if let Some(peer) = self.peers.get_mut(&id) {
//                 //         let _ = peer
//                 //             .sender
//                 //             .send(PeerCommand::RejectRequest(block_info))
//                 //             .await;
//                 //     }
//                 //
//                 //     return;
//                 // }
//                 //
//                 // if !self.choker.is_unchoked(&id) {
//                 //     tracing::error!("peer requested without prior unchoke message");
//                 //     return;
//                 // }
//                 //
//                 // //TODO: improve this to avoid blocking the main loop
//                 // let (block_tx, block_rx) = oneshot::channel();
//                 // self.storage
//                 //     .read_block(self.torrent.info_hash, block_info, block_tx);
//                 //
//                 // let block = match block_rx.await {
//                 //     Ok(Ok(block)) => block,
//                 //     Ok(Err(e)) => {
//                 //         tracing::error!(?e, "storage error");
//                 //         return;
//                 //     }
//                 //     Err(e) => {
//                 //         tracing::error!(?e, "oneshot dropped");
//                 //         return;
//                 //     }
//                 // };
//                 //
//                 // if let Some(peer) = self.peers.get_mut(&id) {
//                 //     let _ = peer
//                 //         .sender
//                 //         .send(PeerCommand::SendMessage(Message::Piece(block)))
//                 //         .await;
//                 // }
//             }
//             Interest(id) => {
//                 // Register interest
//                 // self.choker.set_peer_interest(id, true);
//
//                 // Run choke algorithm when interest changes
//                 self.run_choke_algorithm().await;
//             }
//             NotInterest(id) => {
//                 // self.choker.set_peer_interest(id, false);
//
//                 // Run choke algorithm when interest changes
//                 self.run_choke_algorithm().await;
//             }
//         }
//     }
//
//     async fn assign_piece(&mut self, id: Id) {
//         // let peer = match self.peers.get_mut(&id) {
//         //     Some(peer) => peer,
//         //     None => return,
//         // };
//         //
//         // // Only assign when we are interested and the peer has no piece assigned
//         // if !peer.am_interested {
//         //     tracing::debug!(?id, "skipping assignment: not interested");
//         //     return;
//         // }
//         //
//         // if peer.assigned_piece_idx.is_some() {
//         //     tracing::debug!(?id, "skipping assignment: already has an assigned piece");
//         //     return;
//         // }
//         //
//         // if let Some((idx, piece)) = self.picker.pick_piece(&peer.bitfield) {
//         //     // Verify piece is actually in None state
//         //     peer.assigned_piece_idx = Some(idx);
//         //     self.picker.mark_piece_as(idx, PieceState::Requested);
//         //     let _ = peer.sender.send(PeerCommand::AvailableTask(piece)).await;
//         //     tracing::debug!("Assigned piece {} to peer {}", idx, id.0);
//         // } else {
//         //     let (none, requested, writing, finished) = self.picker.summary();
//         //     tracing::debug!(
//         //         peer = %id.0,
//         //         none = none,
//         //         requested = requested,
//         //         writing = writing,
//         //         finished = finished,
//         //         "no pieces available for peer"
//         //     );
//         //     tracing::debug!(?peer.bitfield);
//         // }
//     }
//
//     // todo: broadcast_have  to peer that only sent pieces recently and are not seeders
//     fn broadcast_have(&self, piece_index: u32) {
//         for state in self.peers.values() {
//             let sender = state.sender.clone();
//             tokio::spawn(async move {
//                 let _ = sender
//                     .send(PeerCommand::SendMessage(Message::Have { piece_index }))
//                     .await;
//             });
//         }
//     }
//
//     //Removes disconnected peer from the peers HashMap
//     // Decrements piece availability for all pieces the peer had
//     fn clean_up_peer(&mut self, id: Id) {
//         // if let Some(removed_peer) = self.peers.remove(&id) {
//         //     self.picker
//         //         .decrement_availability(AvailabilityUpdate::Bitfield(&removed_peer.bitfield));
//         //
//         //     self.connected_addrs.remove(&removed_peer.addr);
//         //
//         //     if let Some(piece_idx) = removed_peer.assigned_piece_idx {
//         //         self.picker.mark_piece_as(piece_idx, PieceState::None);
//         //         self.cache.drop_piece(piece_idx);
//         //     }
//         //     // Remove peer from choker and run algorithm
//         //     self.choker.remove_peer(&id);
//         //
//         //     let peer_stats = self.get_peer_stats();
//         //     self.choker.run_choke_algorithm(&peer_stats);
//         // }
//     }
// }
//
// mod piece_cache {
//     use std::{collections::HashMap, sync::Arc};
//
//     use bittorrent_common::metainfo::TorrentInfo;
//     use peer_protocol::protocol::Block;
//
//     pub struct PieceCache {
//         pieces: HashMap<usize, PieceMetadata>,
//     }
//
//     struct PieceMetadata {
//         buffer: Box<[u8]>,
//         piece_length: usize,
//         downloaded: usize,
//     }
//
//     impl PieceCache {
//         pub fn new(torrent: Arc<TorrentInfo>) -> Self {
//             let mut pieces = HashMap::with_capacity(torrent.num_pieces());
//             for i in 0..torrent.num_pieces() {
//                 let piece_length = torrent.get_piece_len(i) as usize;
//                 let buffer = vec![0; piece_length].into_boxed_slice();
//                 let piece_metadata = PieceMetadata {
//                     buffer,
//                     piece_length,
//                     downloaded: 0,
//                 };
//                 pieces.insert(i, piece_metadata);
//             }
//             Self { pieces }
//         }
//
//         pub fn insert_block(&mut self, block: &Block) -> Option<Arc<[u8]>> {
//             let index = block.index as usize;
//
//             let piece = self.pieces.get_mut(&index)?;
//             let offset = block.begin as usize;
//             piece.buffer[offset..offset + block.data.len()].copy_from_slice(&block.data);
//             // BUG: WE overcount if presence of duplicates
//             piece.downloaded += block.data.len();
//             if piece.downloaded >= piece.piece_length {
//                 // Remove the piece from the cache
//                 let piece = self.pieces.remove(&index).unwrap();
//                 return Some(piece.buffer.into());
//             }
//             None
//         }
//
//         pub fn drop_piece(&mut self, piece_idx: usize) {
//             self.pieces.remove(&piece_idx);
//         }
//     }
// }
