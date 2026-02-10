//! Common test utilities and fixtures
//!
//! This module provides shared infrastructure for integration tests,
//! inspired by libtransmission's test-fixtures.h

pub mod fixtures;
pub mod helpers;
pub mod mocks;
pub mod sandbox;

pub use fixtures::{SessionTest, TorrentTest};
pub use helpers::{wait_for, wait_for_async, wait_for_torrent_state};
pub use sandbox::SandboxedTest;
