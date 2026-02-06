//! Test fixtures for integration tests
//!
//! Provides pre-configured test environments similar to libtransmission's
//! TransmissionTest → SandboxedTest → SessionTest hierarchy.

use std::path::PathBuf;
use std::time::Duration;

use bittorrent_common::metainfo::TorrentInfo;
use bittorrent_core::{
    Session, SessionConfig, SessionError, TorrentId, TorrentState, TorrentSummary,
};

use crate::common::sandbox::SandboxedTest;

/// A test fixture that provides a configured Session with sandboxed storage
///
/// Similar to libtransmission's SessionTest class.
///
/// # Example
///
/// ```rust
/// use common::fixtures::SessionTest;
///
/// #[tokio::test]
/// async fn test_torrent_add() {
///     let fixture = SessionTest::new().await;
///     // Use fixture.session and fixture.sandbox...
/// }
/// ```
pub struct SessionTest {
    /// The sandboxed test environment
    pub sandbox: SandboxedTest,
    /// The configured session
    pub session: Session,
    /// The session configuration used
    pub config: SessionConfig,
}

impl SessionTest {
    /// Create a new SessionTest with default configuration
    pub async fn new() -> Result<Self, SessionError> {
        let sandbox = SandboxedTest::new();
        let save_path = sandbox.path().to_path_buf();
        
        let config = SessionConfig {
            port: 0, // Let OS assign ephemeral port
            save_path,
            enable_dht: false, // Disable DHT for tests
        };
        
        let session = Session::new(config.clone()).await?;
        
        Ok(Self {
            sandbox,
            session,
            config,
        })
    }

    /// Create a new SessionTest with DHT enabled
    pub async fn with_dht() -> Result<Self, SessionError> {
        let sandbox = SandboxedTest::new();
        let save_path = sandbox.path().to_path_buf();
        
        let config = SessionConfig {
            port: 0,
            save_path,
            enable_dht: true,
        };
        
        let session = Session::new(config.clone()).await?;
        
        Ok(Self {
            sandbox,
            session,
            config,
        })
    }

    /// Create a test torrent file in the sandbox
    pub fn create_test_file(&self, filename: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        self.sandbox.create_file(filename, contents)
    }

    /// Get the path where torrents are downloaded
    pub fn download_path(&self) -> &PathBuf {
        &self.config.save_path
    }

    /// Shutdown the session gracefully
    pub async fn shutdown(self) -> Result<(), SessionError> {
        self.session.shutdown().await
    }
}

/// A test fixture specifically for torrent testing
///
/// Extends SessionTest with helper methods for creating and managing torrents.
pub struct TorrentTest {
    pub session_test: SessionTest,
}

impl TorrentTest {
    /// Create a new TorrentTest fixture
    pub async fn new() -> Result<Self, SessionError> {
        let session_test = SessionTest::new().await?;
        Ok(Self { session_test })
    }

    /// Add a torrent to the session
    pub async fn add_torrent(
        &self,
        info: TorrentInfo,
    ) -> Result<TorrentId, SessionError> {
        self.session_test.session.add_torrent(info).await
    }

    /// Add a magnet link to the session
    pub async fn add_magnet(
        &self,
        magnet_uri: &str,
    ) -> Result<TorrentId, SessionError> {
        self.session_test.session.add_magnet(magnet_uri).await
    }

    /// Get torrent summary
    pub async fn get_torrent(&self, id: TorrentId) -> Option<TorrentSummary> {
        self.session_test.session.get_torrent(id).await
    }

    /// Wait for a torrent to reach a specific state
    pub async fn wait_for_state(
        &self,
        id: TorrentId,
        state: TorrentState,
        timeout: Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        
        loop {
            if let Some(summary) = self.get_torrent(id).await {
                if summary.state == state {
                    return true;
                }
            }
            
            if std::time::Instant::now() > deadline {
                return false;
            }
            
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Remove a torrent from the session
    pub async fn remove_torrent(&self, id: TorrentId) -> Result<(), SessionError> {
        self.session_test.session.remove_torrent(id).await
    }
}

/// Helper to create a simple torrent info for testing
pub fn create_test_torrent_info(
    name: &str,
    piece_length: usize,
    pieces_count: usize,
) -> TorrentInfo {
    // This is a placeholder - in a real implementation you'd create
    // a valid TorrentInfo with proper hashes
    // For now, this serves as a template for test authors
    unimplemented!("create_test_torrent_info needs to be implemented based on TorrentInfo structure")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_test_creates_session() {
        let fixture = SessionTest::new().await.expect("Failed to create session");
        
        // Verify session is running
        let stats = fixture.session.get_stats().await;
        // Should have default stats
        
        // Clean up
        fixture.shutdown().await.expect("Failed to shutdown");
    }

    #[tokio::test]
    async fn session_test_creates_files() {
        let fixture = SessionTest::new().await.expect("Failed to create session");
        
        let test_file = fixture.create_test_file("test.txt", b"Hello, World!");
        assert!(test_file.exists());
        
        fixture.shutdown().await.expect("Failed to shutdown");
    }
}
