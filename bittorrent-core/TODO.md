
- [x] On peer Have we are not updating the piece manager (torrent.rs) 
- [ ] WE need to check that we send our bitfield. In case of a newly stared download, the bitfield is sent lazy, given that we dont have any client assume this and via sending Have message each time we write a piec to disk, we are notifying that we have this new piece. And for incoming connection after connection we must send our bitfield. 
- [ ] Impl Choker
  - [ ] Peer Connection should send to torrent TorrentMessage::Interes or TorrentMessage::NotInterested
  - [ ] We need to track per Peer the Upload rate for this on each block we send in handle_peer_cmd realte to SendMessasge and the message is MEssage::Piece


