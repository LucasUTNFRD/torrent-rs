//! Sandboxed test environment
//!
//! Provides temporary directories that are automatically cleaned up,
//! similar to libtransmission's Sandbox class.

use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A test sandbox that provides a temporary directory
///
/// The directory is created when the struct is instantiated and
/// automatically deleted when it goes out of scope.
///
/// # Example
///
/// ```
/// use common::sandbox::SandboxedTest;
///
/// let sandbox = SandboxedTest::new();
/// let path = sandbox.path();
/// // use path for test files...
/// // directory is cleaned up when sandbox is dropped
/// ```
pub struct SandboxedTest {
    temp_dir: TempDir,
}

impl SandboxedTest {
    /// Create a new sandboxed test environment
    pub fn new() -> Self {
        Self {
            temp_dir: TempDir::new().expect("Failed to create temp directory"),
        }
    }

    /// Create a new sandboxed test environment with a specific prefix
    pub fn with_prefix(prefix: &str) -> Self {
        Self {
            temp_dir: TempDir::with_prefix(prefix).expect("Failed to create temp directory"),
        }
    }

    /// Get the path to the sandbox directory
    pub fn path(&self) -> &Path {
        self.temp_dir.path()
    }

    /// Join a path component to the sandbox directory
    pub fn join<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        self.temp_dir.path().join(path)
    }

    /// Create a subdirectory within the sandbox
    pub fn create_dir<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        let full_path = self.join(&path);
        std::fs::create_dir_all(&full_path).expect("Failed to create directory");
        full_path
    }

    /// Create a file with contents in the sandbox
    pub fn create_file<P: AsRef<Path>>(&self, path: P, contents: impl AsRef<[u8]>) -> PathBuf {
        let full_path = self.join(&path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        std::fs::write(&full_path, contents).expect("Failed to write file");
        full_path
    }
}

impl Default for SandboxedTest {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_creates_temp_directory() {
        let sandbox = SandboxedTest::new();
        let path = sandbox.path();
        assert!(path.exists());
        assert!(path.is_dir());
    }

    #[test]
    fn sandbox_creates_files() {
        let sandbox = SandboxedTest::new();
        let file_path = sandbox.create_file("test.txt", b"Hello, World!");

        assert!(file_path.exists());
        assert_eq!(std::fs::read_to_string(file_path).unwrap(), "Hello, World!");
    }

    #[test]
    fn sandbox_creates_directories() {
        let sandbox = SandboxedTest::new();
        let dir_path = sandbox.create_dir("downloads/torrents");

        assert!(dir_path.exists());
        assert!(dir_path.is_dir());
    }
}
