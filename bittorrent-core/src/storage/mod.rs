use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
};

use bittorrent_common::{
    metainfo::{FileInfo, TorrentInfo},
    types::InfoHash,
};
use sha1::{Digest, Sha1};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {}

struct StorageHandler {}

struct StorageManager {
    download_dir: PathBuf,
    torrents: HashMap<InfoHash, Cache>,
    rx: mpsc::Receiver<StorageMessage>,
}

struct Cache {
    metainfo: Arc<TorrentInfo>,
    files: Vec<FileInfo>,
    // Cache file handles to avoid reopening files constantly
    file_handles: HashMap<PathBuf, File>,
}

pub enum StorageMessage {
    AddTorrent {
        id: InfoHash,
        meta: Arc<TorrentInfo>,
    },
    RemoveTorrent {
        id: InfoHash,
    },
    Read {
        id: InfoHash,
        piece_index: u32,
        begin: u32,
        length: u32,
        // peer: Sender<PeerCommand>,
    },
    Write {
        id: InfoHash,
        piece: u32,
        data: Box<[u8]>,
    },
    Verify {
        id: InfoHash,
        piece: u32,
        data: Box<[u8]>,
    },
}

pub struct Storage {
    tx: mpsc::Sender<StorageMessage>,
    join_handle: JoinHandle<()>,
}

impl Storage {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        const BASE_DIR: &str = "$HOME/Downloads/Torrents/";
        let mananger = StorageManager::new(PathBuf::from(BASE_DIR), rx);
        let builder = thread::Builder::new().name("Storage handler".to_string());
        let handle = builder
            .spawn(|| mananger.start())
            .expect("Failed to spawn from thread builder");

        Self {
            join_handle: handle,
            tx,
        }
    }
}

impl StorageManager {
    pub fn new(download_dir: PathBuf, rx: mpsc::Receiver<StorageMessage>) -> Self {
        Self {
            download_dir,
            rx,
            torrents: HashMap::new(),
        }
    }

    pub fn start(mut self) {
        use StorageMessage::*;
        while let Ok(msg) = self.rx.recv() {
            match msg {
                AddTorrent { id, meta } => {
                    let files = meta.files();
                    self.torrents.insert(
                        id,
                        Cache {
                            metainfo: meta,
                            files,
                            file_handles: HashMap::new(),
                        },
                    );
                }
                RemoveTorrent { id } => {
                    self.torrents.remove(&id);
                }
                Read {
                    id,
                    piece_index,
                    begin,
                    length,
                } => {
                    let _ = self.read(id, piece_index, begin, length);
                }
                Write { id, piece, data } => {
                    let _ = self.write(id, piece, &data);
                }
                Verify { id, piece, data } => {
                    let expected_piece_hash =
                        self.torrents.get(&id).unwrap().metainfo.info.pieces[piece as usize];

                    let mut hasher = Sha1::new();
                    hasher.update(data);
                    let piece_hash: [u8; 20] = hasher.finalize().into();

                    let matches = piece_hash == expected_piece_hash;
                    // Here we should notify of the result
                }
            }
        }
    }

    fn write(&mut self, info_hash: InfoHash, piece_index: u32, data: &[u8]) -> io::Result<()> {
        // First, collect the file information we need
        let (files, piece_length) = {
            let cache = self.torrents.get(&info_hash).unwrap();
            (
                cache.files.clone(),
                cache.metainfo.info.piece_length as usize,
            )
        };

        // Calculate the global byte offset for this piece
        let mut global_offset = piece_index as usize * piece_length;
        let mut remaining_data = data;

        // Iterate through files to find where to start writing
        for file_info in &files {
            let file_len = file_info.length as usize;

            // If this entire file is before our starting offset, skip it
            if global_offset >= file_len {
                global_offset -= file_len;
                continue;
            }

            // Get or open the file handle
            let file = self.get_or_open_file(info_hash, &file_info.path)?;

            // Calculate how much we can write to this file
            let start_in_file = global_offset;
            let available_in_file = file_len - start_in_file;
            let write_len = available_in_file.min(remaining_data.len());

            // Write to this file
            file.write_all_at(&remaining_data[..write_len], start_in_file as u64)?;

            // Update remaining data and reset offset for next file
            remaining_data = &remaining_data[write_len..];
            global_offset = 0;

            // If we've written all data, we're done
            if remaining_data.is_empty() {
                break;
            }
        }

        if !remaining_data.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Could not write all piece data - insufficient file space",
            ));
        }

        Ok(())
    }

    fn read(
        &mut self,
        info_hash: InfoHash,
        piece_index: u32,
        begin: u32,
        length: u32,
    ) -> io::Result<Vec<u8>> {
        // First, collect the file information we need
        let (files, piece_length) = {
            let cache = self.torrents.get(&info_hash).unwrap();
            (
                cache.files.clone(),
                cache.metainfo.info.piece_length as usize,
            )
        };

        // Calculate global byte offset for this read
        let mut global_offset = (piece_index as usize * piece_length) + begin as usize;

        let mut result = vec![0u8; length as usize];
        let mut bytes_read = 0;

        // Iterate through files to find where to start reading
        for file_info in &files {
            let file_len = file_info.length as usize;

            // If this entire file is before our starting offset, skip it
            if global_offset >= file_len {
                global_offset -= file_len;
                continue;
            }

            // Get or open the file handle
            let file = self.get_or_open_file(info_hash, &file_info.path)?;

            // Calculate how much we can read from this file
            let start_in_file = global_offset;
            let available_in_file = file_len - start_in_file;
            let remaining_to_read = result.len() - bytes_read;
            let read_len = available_in_file.min(remaining_to_read);

            // Read from this file
            file.read_exact_at(
                &mut result[bytes_read..bytes_read + read_len],
                start_in_file as u64,
            )?;

            // Update counters and reset offset for next file
            bytes_read += read_len;
            global_offset = 0;

            // If we've read all requested data, we're done
            if bytes_read >= result.len() {
                break;
            }
        }

        if bytes_read != result.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Could not read all requested bytes",
            ));
        }

        Ok(result)
    }

    // Helper function to get or open a file handle
    fn get_or_open_file(&mut self, info_hash: InfoHash, file_path: &Path) -> io::Result<&mut File> {
        let cache = self.torrents.get_mut(&info_hash).unwrap();

        if !cache.file_handles.contains_key(file_path) {
            let full_path = self.download_dir.join(file_path);
            let file = open_file(&full_path)?;
            cache.file_handles.insert(file_path.to_path_buf(), file);
        }

        Ok(cache.file_handles.get_mut(file_path).unwrap())
    }
}

// TODO: reduce usage of many blocking syscalls

fn open_file(path: &Path) -> io::Result<File> {
    if !path.exists() {
        let create_dir = if path.is_dir() {
            Some(path)
        } else {
            path.parent()
        };

        if let Some(dir) = create_dir {
            std::fs::create_dir_all(dir)?;
        };
    }
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .read(true)
        .open(path)
}

#[cfg(test)]
mod test {
    use std::{
        collections::HashMap,
        env, fs,
        path::PathBuf,
        sync::{Arc, mpsc},
        time::{SystemTime, UNIX_EPOCH},
    };

    use bittorrent_common::{
        metainfo::{self, FileInfo, Info, TorrentInfo},
        types::InfoHash,
    };

    use crate::storage::{Cache, StorageManager};

    // use super::*;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{}_{}", prefix, now))
    }

    #[test]
    fn test_read_and_write_single_file_mode() {
        // Setup a temporary download directory
        let download_dir = unique_temp_dir("bt_storage_single");
        fs::create_dir_all(&download_dir).unwrap();

        // Construct a simple single-file TorrentInfo
        let piece_length = 512i64;
        let total_length = 1024i64; // two pieces
        let num_pieces = (total_length as f64 / piece_length as f64).ceil() as usize;
        let pieces = vec![[0u8; 20]; num_pieces];

        let info = Info {
            piece_length,
            pieces,
            private: None,
            mode: metainfo::FileMode::SingleFile {
                name: "test_single.bin".to_string(),
                length: total_length,
                md5sum: None,
            },
        };

        let torrent = TorrentInfo {
            info,
            announce: "http://localhost".to_string(),
            announce_list: None,
            creation_date: None,
            comment: None,
            created_by: None,
            encoding: None,
            info_hash: InfoHash::new([0u8; 20]),
        };

        let id = torrent.info_hash;
        let meta = Arc::new(torrent.clone());

        // Prepare StorageManager (rx never used in test)
        let (_tx, rx) = mpsc::channel();
        let mut manager = StorageManager::new(download_dir.clone(), rx);

        // Insert torrent into manager cache
        manager.torrents.insert(
            id,
            Cache {
                metainfo: meta.clone(),
                files: meta.files(),
                file_handles: HashMap::new(),
            },
        );

        // Prepare test data for each piece
        let piece0_data: Vec<u8> = (0..piece_length).map(|i| i as u8).collect();
        let piece1_data: Vec<u8> = (0..piece_length).map(|i| (i + 128) as u8).collect();

        // Write pieces individually (this is the correct usage)
        manager
            .write(id, 0, &piece0_data)
            .expect("write piece 0 failed");

        manager
            .write(id, 1, &piece1_data)
            .expect("write piece 1 failed");

        // Read back pieces
        let read_piece0 = manager
            .read(id, 0, 0, piece_length as u32)
            .expect("read piece0 failed");
        assert_eq!(read_piece0, piece0_data);

        let read_piece1 = manager
            .read(id, 1, 0, piece_length as u32)
            .expect("read piece1 failed");
        assert_eq!(read_piece1, piece1_data);

        // Test partial reads (blocks)
        let block = manager.read(id, 0, 256, 256).expect("read block failed");
        assert_eq!(block, &piece0_data[256..512]);

        // Cleanup
        fs::remove_dir_all(&download_dir).unwrap();
    }

    #[test]
    fn test_read_and_write_multi_file_mode() {
        // Setup a temporary download directory
        let download_dir = unique_temp_dir("bt_storage_multi");
        fs::create_dir_all(&download_dir).unwrap();

        // Multi-file torrent: two files, lengths 600 and 500 => total 1100
        let f1 = FileInfo {
            length: 600,
            md5sum: None,
            path: PathBuf::from("file1.bin"),
        };
        let f2 = FileInfo {
            length: 500,
            md5sum: None,
            path: PathBuf::from("subdir").join("file2.bin"),
        };

        let total_length = f1.length + f2.length;
        let piece_length = 512i64;
        let num_pieces = (total_length as f64 / piece_length as f64).ceil() as usize;
        let pieces = vec![[0u8; 20]; num_pieces];

        let info = Info {
            piece_length,
            pieces,
            private: None,
            mode: metainfo::FileMode::MultiFile {
                name: "test_multi".to_string(),
                files: vec![f1.clone(), f2.clone()],
            },
        };

        let torrent = TorrentInfo {
            info,
            announce: "http://localhost".to_string(),
            announce_list: None,
            creation_date: None,
            comment: None,
            created_by: None,
            encoding: None,
            info_hash: InfoHash::new([1u8; 20]),
        };

        let id = torrent.info_hash;
        let meta = Arc::new(torrent.clone());

        // Prepare StorageManager (rx never used in test)
        let (_tx, rx) = mpsc::channel();
        let mut manager = StorageManager::new(download_dir.clone(), rx);

        // Insert torrent into manager cache
        manager.torrents.insert(
            id,
            Cache {
                metainfo: meta.clone(),
                files: meta.files(),
                file_handles: HashMap::new(),
            },
        );

        // Write pieces individually
        let piece0_data: Vec<u8> = (0..512).map(|i| i as u8).collect();
        let piece1_data: Vec<u8> = (0..512).map(|i| (i + 100) as u8).collect();
        let piece2_data: Vec<u8> = (0..76).map(|i| (i + 200) as u8).collect(); // Last piece is partial

        manager
            .write(id, 0, &piece0_data)
            .expect("write piece 0 failed");
        manager
            .write(id, 1, &piece1_data)
            .expect("write piece 1 failed");
        manager
            .write(id, 2, &piece2_data)
            .expect("write piece 2 failed");

        // Read back and verify
        let read_p0 = manager.read(id, 0, 0, 512).expect("read p0 failed");
        assert_eq!(read_p0, piece0_data);

        let read_p1 = manager.read(id, 1, 0, 512).expect("read p1 failed");
        assert_eq!(read_p1, piece1_data);

        let read_p2 = manager.read(id, 2, 0, 76).expect("read p2 failed");
        assert_eq!(read_p2, piece2_data);

        // Test reading a block that spans file boundary
        // Piece 1 starts at global offset 512, so reading from begin=88 with length=100
        // should span from file1 (which ends at offset 600) into file2
        let spanning_block = manager.read(id, 1, 88, 100).expect("spanning read failed");

        // This should read bytes from offset 600 in the torrent (end of file1)
        // and beginning of file2
        let mut expected = Vec::new();
        expected.extend_from_slice(&piece1_data[88..]); // Rest of piece1 data
        assert_eq!(spanning_block.len(), 100);

        // Cleanup
        fs::remove_dir_all(&download_dir).unwrap();
    }

    #[test]
    fn test_file_handle_caching() {
        // Test that file handles are properly cached and reused
        let download_dir = unique_temp_dir("bt_storage_cache");
        fs::create_dir_all(&download_dir).unwrap();

        let info = Info {
            piece_length: 512,
            pieces: vec![[0u8; 20]; 2],
            private: None,
            mode: metainfo::FileMode::SingleFile {
                name: "cache_test.bin".to_string(),
                length: 1024,
                md5sum: None,
            },
        };

        let torrent = TorrentInfo {
            info,
            announce: "http://localhost".to_string(),
            announce_list: None,
            creation_date: None,
            comment: None,
            created_by: None,
            encoding: None,
            info_hash: InfoHash::new([4u8; 20]),
        };

        let id = torrent.info_hash;
        let meta = Arc::new(torrent.clone());

        let (_tx, rx) = mpsc::channel();
        let mut manager = StorageManager::new(download_dir.clone(), rx);

        manager.torrents.insert(
            id,
            Cache {
                metainfo: meta.clone(),
                files: meta.files(),
                file_handles: HashMap::new(),
            },
        );

        // Initial state: no file handles cached
        assert_eq!(manager.torrents.get(&id).unwrap().file_handles.len(), 0);

        // First write should open and cache the file handle
        let data = vec![42u8; 512];
        manager.write(id, 0, &data).expect("write failed");
        assert_eq!(manager.torrents.get(&id).unwrap().file_handles.len(), 1);

        // Subsequent operations should reuse the cached handle
        manager.write(id, 1, &data).expect("second write failed");
        assert_eq!(manager.torrents.get(&id).unwrap().file_handles.len(), 1);

        let _read_data = manager.read(id, 0, 0, 256).expect("read failed");
        assert_eq!(manager.torrents.get(&id).unwrap().file_handles.len(), 1);

        fs::remove_dir_all(&download_dir).unwrap();
    }

    #[test]
    fn test_nested_directory_creation() {
        // Test that nested directories are created properly
        let download_dir = unique_temp_dir("bt_storage_nested");
        fs::create_dir_all(&download_dir).unwrap();

        let files = vec![
            FileInfo {
                length: 512,
                md5sum: None,
                path: PathBuf::from("level1").join("level2").join("deep_file.bin"),
            },
            FileInfo {
                length: 256,
                md5sum: None,
                path: PathBuf::from("another").join("path").join("file.dat"),
            },
        ];

        let info = Info {
            piece_length: 512,
            pieces: vec![[0u8; 20]; 2],
            private: None,
            mode: metainfo::FileMode::MultiFile {
                name: "test_nested".to_string(),
                files,
            },
        };

        let torrent = TorrentInfo {
            info,
            announce: "http://localhost".to_string(),
            announce_list: None,
            creation_date: None,
            comment: None,
            created_by: None,
            encoding: None,
            info_hash: InfoHash::new([5u8; 20]),
        };

        let id = torrent.info_hash;
        let meta = Arc::new(torrent.clone());

        let (_tx, rx) = mpsc::channel();
        let mut manager = StorageManager::new(download_dir.clone(), rx);

        manager.torrents.insert(
            id,
            Cache {
                metainfo: meta.clone(),
                files: meta.files(),
                file_handles: HashMap::new(),
            },
        );

        // Write data - this should create the nested directories
        let piece_data = vec![123u8; 512];
        manager
            .write(id, 0, &piece_data)
            .expect("write with nested dirs failed");

        // Verify directories were created
        assert!(download_dir.join("level1").join("level2").exists());
        assert!(download_dir.join("another").join("path").exists());

        // Verify files exist and have correct content
        assert!(
            download_dir
                .join("level1")
                .join("level2")
                .join("deep_file.bin")
                .exists()
        );
        assert!(
            download_dir
                .join("another")
                .join("path")
                .join("file.dat")
                .exists()
        );

        // Read back and verify
        let read_data = manager.read(id, 0, 0, 512).expect("read failed");
        assert_eq!(read_data, piece_data);

        fs::remove_dir_all(&download_dir).unwrap();
    }
}
