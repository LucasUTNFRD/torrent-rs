//! Session management integration tests
//!
//! Tests for the bittorrent-core Session covering:
//! - Session lifecycle (create, shutdown)
//! - Torrent management (add, remove, list)
//! - Settings and configuration
//! - Statistics and monitoring

use common::fixtures::*;

mod common;

/// Test session creation and shutdown
#[tokio::test]
async fn session_create_shutdown() {
    let fixture = SessionTest::new().await.expect("Failed to create session");
    
    // Verify session is running
    let stats = fixture.session.get_stats().await;
    // Should have default stats with 0 torrents
    
    // Clean shutdown
    fixture.shutdown().await.expect("Failed to shutdown");
}

/// Test adding torrent from file
#[tokio::test]
async fn session_add_torrent_from_file() {
    // TODO: Create test torrent file
    // TODO: Add to session
    // TODO: Verify torrent is added and returns ID
}

/// Test adding torrent from magnet link
#[tokio::test]
async fn session_add_magnet_link() {
    // TODO: Add magnet link to session
    // TODO: Verify torrent enters metadata fetching state
}

/// Test removing torrent
#[tokio::test]
async fn session_remove_torrent() {
    // TODO: Add torrent
    // TODO: Remove by ID
    // TODO: Verify removed from list
}

/// Test listing torrents
#[tokio::test]
async fn session_list_torrents() {
    // TODO: Add multiple torrents
    // TODO: List all torrents
    // TODO: Verify all are returned
}

/// Test getting torrent by ID
#[tokio::test]
async fn session_get_torrent() {
    // TODO: Add torrent
    // TODO: Get by ID
    // TODO: Verify correct torrent returned
}

/// Test session statistics
#[tokio::test]
async fn session_statistics() {
    // TODO: Add torrents in different states
    // TODO: Get stats
    // TODO: Verify counts are correct
}

/// Test session with DHT enabled
#[tokio::test]
async fn session_with_dht() {
    let fixture = SessionTest::with_dht().await.expect("Failed to create session");
    
    // TODO: Verify DHT is bootstrapping
    // TODO: Verify DHT stats in session stats
    
    fixture.shutdown().await.expect("Failed to shutdown");
}

/// Test session configuration
#[tokio::test]
async fn session_configuration() {
    // TODO: Create session with custom config
    // TODO: Verify settings applied
}

/// Test error handling for duplicate torrent
#[tokio::test]
async fn session_rejects_duplicate_torrent() {
    // TODO: Add torrent
    // TODO: Try to add same torrent again
    // TODO: Verify error
}

/// Test session shutdown waits for operations
#[tokio::test]
async fn session_graceful_shutdown() {
    // TODO: Start some operations
    // TODO: Shutdown
    // TODO: Verify graceful completion
}
