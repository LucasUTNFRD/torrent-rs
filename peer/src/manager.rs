use std::collections::HashMap;

use peer_protocol::protocol::Message;
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot},
};

use crate::{
    PeerError,
    connection::{PeerConnection, PeerInfo},
};

#[derive(Debug)]
pub enum ManagerCommand {
    AddPeer {
        peer_info: PeerInfo,
        stream: TcpStream,
    },
    RemovePeer(PeerInfo),
}

#[derive(Debug)]
pub enum PeerEvent {
    SendMessage(Message),
    Disconnect,
}

struct PeerManager {
    peers: HashMap<PeerInfo, mpsc::Sender<PeerEvent>>,
    manager_rx: mpsc::UnboundedReceiver<ManagerCommand>,
}

struct PeerConnectionConfig {}

//  A handle is an object that other pieces of code can use to talk to the actor, and is also what keeps the actor alive.
#[derive(Debug, Clone)]
pub struct PeerManagerHandle {
    manager_tx: mpsc::UnboundedSender<ManagerCommand>,
}

impl PeerManagerHandle {
    // TODO: Implement a manager builder to pass a config
    pub fn new() -> Self {
        let (manager_tx, manager_rx) = mpsc::unbounded_channel();

        let manager = PeerManager::new(manager_rx);
        tokio::spawn(manager.run());

        Self { manager_tx }
    }

    pub async fn add_peer(&self, info: PeerInfo, stream: TcpStream) -> Result<(), PeerError> {
        self.manager_tx
            .send(ManagerCommand::AddPeer {
                peer_info: info,
                stream,
            })
            .map_err(|_| PeerError::Disconnected)?;

        Ok(())
    }
}

impl PeerManagerHandle {}

impl PeerManager {
    pub fn new(manager_rx: mpsc::UnboundedReceiver<ManagerCommand>) -> Self {
        Self {
            peers: HashMap::new(),
            manager_rx,
        }
    }

    pub async fn run(mut self) {
        while let Some(cmd) = self.manager_rx.recv().await {
            use ManagerCommand::*;
            match cmd {
                AddPeer { peer_info, stream } => {
                    let (event_tx, event_rx) = mpsc::channel(32);
                    self.peers.insert(peer_info, event_tx);
                    let conn = PeerConnection::new(peer_info, event_rx);

                    tokio::spawn(conn.run(stream));
                }
                _ => unimplemented!(),
            }
        }
    }
}
