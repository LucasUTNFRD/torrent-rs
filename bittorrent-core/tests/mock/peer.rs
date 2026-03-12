use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bittorrent_common::types::InfoHash;
use bytes::BytesMut;
use futures::{SinkExt, StreamExt};
use peer_protocol::protocol::{Handshake, Message, MessageCodec};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;
use tokio_util::codec::Framed;

use turmoil::net::TcpStream;

/// Shared structure to collect unchoke durations from all mock peers.
pub type UnchokeTiming = Arc<Mutex<HashMap<String, Duration>>>;

/// Run a mock peer as a turmoil host task.
///
/// Connects to `seeder_addr`, performs handshake with `info_hash`,
/// sends `Interested`, then tracks Choke/Unchoke timing.
/// Sends KeepAlive every 30s to avoid timeout disconnections.
///
/// Timing data is stored in the shared map **on every Choke transition**
/// (not just at exit), since turmoil may drop futures when the sim ends.
pub async fn run_mock_peer(
    peer_name: String,
    seeder_addr: std::net::SocketAddr,
    info_hash: InfoHash,
    timing: UnchokeTiming,
) -> Result<(), Box<dyn std::error::Error>> {
    // Generate a unique peer_id for this mock peer
    let mut peer_id = [0u8; 20];
    let name_bytes = peer_name.as_bytes();
    let copy_len = name_bytes.len().min(20);
    peer_id[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

    tracing::info!("[{}] Connecting to seeder at {}", peer_name, seeder_addr);

    // Connect to the seeder's TCP listener
    let mut stream = TcpStream::connect(&seeder_addr).await?;

    tracing::info!("[{}] Connected, sending handshake", peer_name);

    // -- Handshake --
    // Seeder's listener reads handshake first, so we send ours first
    let handshake = Handshake::new(peer_id.into(), info_hash);
    stream.write_all(&handshake.to_bytes()).await?;

    // Then read seeder's handshake response
    let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);
    stream.read_exact(&mut buf).await?;
    let _remote_handshake = Handshake::from_bytes(&buf).expect("invalid handshake from seeder");

    tracing::info!("[{}] Handshake complete, sending Interested", peer_name);

    // -- Framed protocol --
    let framed = Framed::new(stream, MessageCodec {});
    let (mut sink, mut stream) = framed.split();

    // Send Interested to trigger the choker
    sink.send(Message::Interested).await?;

    // Track unchoke timing
    let mut total_unchoke_duration = Duration::ZERO;
    let mut last_unchoke: Option<Instant> = None;
    let mut keepalive_interval = tokio::time::interval(Duration::from_secs(30));
    keepalive_interval.tick().await; // consume immediate first tick

    loop {
        tokio::select! {
            maybe_msg = stream.next() => {
                match maybe_msg {
                    Some(Ok(msg)) => {
                        match msg {
                            Message::Unchoke => {
                                tracing::debug!("[{}] UNCHOKED", peer_name);
                                if last_unchoke.is_none() {
                                    last_unchoke = Some(Instant::now());
                                }
                            }
                            Message::Choke => {
                                tracing::debug!("[{}] CHOKED", peer_name);
                                if let Some(start) = last_unchoke.take() {
                                    total_unchoke_duration += start.elapsed();
                                    // Store timing on every choke transition,
                                    // since turmoil may drop this future at sim end
                                    timing
                                        .lock()
                                        .unwrap()
                                        .insert(peer_name.clone(), total_unchoke_duration);
                                }
                            }
                            Message::KeepAlive => {}
                            _ => {
                                tracing::trace!("[{}] Received {:?}", peer_name, msg);
                            }
                        }
                    }
                    Some(Err(e)) => {
                        tracing::warn!("[{}] Stream error: {}", peer_name, e);
                        break;
                    }
                    None => {
                        tracing::info!("[{}] Stream closed", peer_name);
                        break;
                    }
                }
            }
            _ = keepalive_interval.tick() => {
                if sink.send(Message::KeepAlive).await.is_err() {
                    tracing::warn!("[{}] Failed to send keepalive", peer_name);
                    break;
                }
            }
        }
    }

    // Final update: account for last unchoke period if still unchoked
    if let Some(start) = last_unchoke.take() {
        total_unchoke_duration += start.elapsed();
    }

    // Store final result
    timing
        .lock()
        .unwrap()
        .insert(peer_name, total_unchoke_duration);

    Ok(())
}
