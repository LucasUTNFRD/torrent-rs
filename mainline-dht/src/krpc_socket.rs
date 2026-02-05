//! KRPC Socket Layer - UDP socket handling actor
//!
//! This module provides a dedicated actor for UDP socket I/O,
//! separating network concerns from DHT logic.

use std::net::SocketAddr;

use tokio::{
    net::UdpSocket,
    sync::mpsc::{self, Receiver, Sender},
};

use crate::message::KrpcMessage;

/// Commands sent to the KrpcSocket actor
pub enum SocketCommand {
    /// Send a KRPC message to a specific address
    Send {
        message: KrpcMessage,
        to: SocketAddr,
    },
    /// Shutdown the socket actor
    Shutdown,
}

/// Events received from the KrpcSocket actor
#[derive(Debug)]
pub enum SocketEvent {
    /// A KRPC message was received
    MessageReceived {
        message: KrpcMessage,
        from: SocketAddr,
    },
    /// Failed to parse a received packet
    ParseError {
        data: Vec<u8>,
        from: SocketAddr,
        error: String,
    },
    /// Failed to send a packet
    SendError {
        message: KrpcMessage,
        to: SocketAddr,
        error: std::io::Error,
    },
}

/// Handle to communicate with the KrpcSocket actor
#[derive(Clone)]
pub struct SocketHandle {
    command_tx: Sender<SocketCommand>,
}

impl SocketHandle {
    /// Send raw bytes to the specified address
    pub async fn send_to(&self, data: &[u8], to: SocketAddr) -> Result<(), std::io::Error> {
        // Convert bytes back to KrpcMessage - this is a bit inefficient but maintains API
        // In a real implementation, we'd have a separate command for raw bytes
        let message = KrpcMessage::from_bytes(data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        self.send(message, to).await
    }

    /// Send a KRPC message to the specified address
    pub async fn send(&self, message: KrpcMessage, to: SocketAddr) -> Result<(), std::io::Error> {
        self.command_tx
            .send(SocketCommand::Send { message, to })
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "socket actor closed"))
    }

    /// Shutdown the socket actor
    pub async fn shutdown(&self) -> Result<(), std::io::Error> {
        self.command_tx
            .send(SocketCommand::Shutdown)
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "socket actor closed"))
    }
}

// Re-export for backwards compatibility
pub type KrpcSocketHandle = SocketHandle;

/// KrpcSocket actor - handles all UDP socket I/O
pub struct KrpcSocket {
    socket: UdpSocket,
    command_rx: Receiver<SocketCommand>,
    event_tx: Sender<SocketEvent>,
}

impl KrpcSocket {
    /// Create a new KrpcSocket actor bound to the given address
    pub async fn new(
        bind_addr: SocketAddr,
    ) -> Result<(Self, SocketHandle, Receiver<SocketEvent>), std::io::Error> {
        let socket = UdpSocket::bind(bind_addr).await?;
        let (command_tx, command_rx) = mpsc::channel(128);
        let (event_tx, event_rx) = mpsc::channel(128);

        let actor = Self {
            socket,
            command_rx,
            event_tx,
        };

        let handle = SocketHandle { command_tx };

        Ok((actor, handle, event_rx))
    }

    /// Run the socket actor
    pub async fn run(mut self) {
        let mut buf = vec![0u8; 4096];

        loop {
            tokio::select! {
                // Handle incoming UDP packets
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, from)) => {
                            let data = &buf[..len];
                            match KrpcMessage::from_bytes(data) {
                                Ok(message) => {
                                    let event = SocketEvent::MessageReceived { message, from };
                                    if self.event_tx.send(event).await.is_err() {
                                        tracing::debug!("DHT actor dropped, shutting down socket");
                                        return;
                                    }
                                }
                                Err(e) => {
                                    let event = SocketEvent::ParseError {
                                        data: data.to_vec(),
                                        from,
                                        error: e.to_string(),
                                    };
                                    if self.event_tx.send(event).await.is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("UDP recv error: {}", e);
                        }
                    }
                }

                // Handle commands from DHT actor
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        SocketCommand::Send { message, to } => {
                            let bytes = message.to_bytes();
                            if let Err(e) = self.socket.send_to(&bytes, to).await {
                                let event = SocketEvent::SendError {
                                    message,
                                    to,
                                    error: e,
                                };
                                if self.event_tx.send(event).await.is_err() {
                                    return;
                                }
                            }
                        }
                        SocketCommand::Shutdown => {
                            tracing::debug!("KrpcSocket shutting down");
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::KrpcMessage;
    use crate::node_id::NodeId;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[tokio::test]
    async fn test_socket_roundtrip() {
        // Create two sockets
        let addr1 = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let addr2 = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));

        let (socket1, handle1, mut _events1) = KrpcSocket::new(addr1).await.unwrap();
        let (socket2, handle2, mut events2) = KrpcSocket::new(addr2).await.unwrap();

        // Get actual bound addresses
        let actual_addr1 = socket1.socket.local_addr().unwrap();
        let actual_addr2 = socket2.socket.local_addr().unwrap();

        // Spawn both actors
        tokio::spawn(socket1.run());
        tokio::spawn(socket2.run());

        // Create a ping message
        let node_id = NodeId::generate_random();

        // Send from socket1 to socket2
        let msg = KrpcMessage::ping_query(42, node_id);
        handle1.send(msg, actual_addr2).await.unwrap();

        // Wait for socket2 to receive it
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events2.recv())
            .await
            .unwrap()
            .unwrap();

        match event {
            SocketEvent::MessageReceived { message, from } => {
                assert_eq!(from, actual_addr1);
                // Verify it's a ping query
                assert!(matches!(message.body, crate::message::MessageBody::Query(crate::message::Query::Ping { .. })));
            }
            _ => panic!("Expected MessageReceived, got {:?}", event),
        }
    }
}
