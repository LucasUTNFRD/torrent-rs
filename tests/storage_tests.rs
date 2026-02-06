//! Storage integration tests
//!
//! Tests for bittorrent-core storage covering:
//! - Piece read/write
//! - File creation and allocation
//! - Sparse files
//! - Verification

use common::fixtures::*;

mod common;

/// Test writing and reading a piece
#[tokio::test]
async fn storage_write_read_piece() {
    // TODO: Create storage
    // TODO: Write piece data
    // TODO: Read back and verify
}

/// Test piece verification
#[tokio::test]
async fn storage_piece_verification() {
    // TODO: Create storage with known data
    // TODO: Verify piece hash
    // TODO: Verify corruption detection
}

/// Test sparse file support
#[tokio::test]
async fn storage_sparse_files() {
    // TODO: Create large torrent
    // TODO: Verify sparse allocation (if supported)
}

/// Test multi-file torrent storage
#[tokio::test]
async fn storage_multi_file() {
    // TODO: Create multi-file torrent
    // TODO: Write pieces spanning file boundaries
    // TODO: Verify correct file placement
}

/// Test storage error handling
#[tokio::test]
async fn storage_error_handling() {
    // TODO: Test disk full
    // TODO: Test permission denied
    // TODO: Verify proper error propagation
}
