use std::{
    env,
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use bittorrent_common::{metainfo::Info, types::InfoHash};
use peer_protocol::protocol::{Block, BlockInfo};
use sha1::Digest;
use tokio::sync::{mpsc, oneshot};

use crate::storage::storage_manager::StorageManager;

mod storage_manager;

pub enum StorageMessage {
    AddTorrent {
        info_hash: InfoHash,
        meta: Arc<Info>,
    },
    RemoveTorrent {
        id: InfoHash,
    },
    Read {
        id: InfoHash,
        piece_index: u32,
        begin: u32,
        length: u32,
        block_rx: oneshot::Sender<Result<Block, io::Error>>,
    },
    Write {
        info_hash: InfoHash,
        piece: u32,
        data: Arc<[u8]>,
    },
    Verify {
        info_hash: InfoHash,
        piece: u32,
        data: Arc<[u8]>,
        verification_tx: oneshot::Sender<bool>,
    },
}

pub struct Storage {
    tx: mpsc::Sender<StorageMessage>,
}

fn get_download_dir() -> PathBuf {
    env::var_os("HOME").map_or_else(
        || panic!("$HOME not set"),
        |home| {
            let mut p = PathBuf::from(home);
            p.push("Downloads");
            p.push("Torrents");
            p
        },
    )
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage {
    #[must_use]
    pub fn new() -> Self {
        Self::with_download_dir(get_download_dir())
    }

    pub fn with_download_dir(download_dir: PathBuf) -> Self {
        // TODO(step 6): proper backpressure -- callers use try_send() which
        // silently drops messages when the channel is full.  For now, size the
        // buffer large enough that we don't hit this in normal operation.
        let (tx, rx) = mpsc::channel(1024);

        let manager = StorageManager::new(download_dir, rx);
        tokio::spawn(manager.start());

        Self { tx }
    }

    pub fn add_torrent(&self, info_hash: InfoHash, torrent: Arc<Info>) {
        let _ = self.tx.try_send(StorageMessage::AddTorrent {
            info_hash,
            meta: torrent,
        });
    }

    #[allow(dead_code)]
    pub fn remove_torrent(&self, torrent_id: InfoHash) {
        let _ = self
            .tx
            .try_send(StorageMessage::RemoveTorrent { id: torrent_id });
    }

    pub fn verify_piece(
        &self,
        info_hash: InfoHash,
        piece_index: u32,
        piece_data: Arc<[u8]>,
        verification_tx: oneshot::Sender<bool>,
    ) {
        let _ = self.tx.try_send(StorageMessage::Verify {
            info_hash,
            piece: piece_index,
            data: piece_data,
            verification_tx,
        });
    }

    pub fn write_piece(&self, info_hash: InfoHash, piece_index: u32, piece_data: Arc<[u8]>) {
        let _ = self.tx.try_send(StorageMessage::Write {
            info_hash,
            piece: piece_index,
            data: piece_data,
        });
    }

    pub fn read_block(
        &self,
        torrent_id: InfoHash,
        block_info: BlockInfo,
        block_rx: oneshot::Sender<Result<Block, io::Error>>,
    ) {
        let _ = self.tx.try_send(StorageMessage::Read {
            id: torrent_id,
            piece_index: block_info.index,
            begin: block_info.begin,
            length: block_info.length,
            block_rx,
        });
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
        }
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
        env, fs,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use bittorrent_common::{
        metainfo::{self, FileInfo, Info, TorrentInfo},
        types::InfoHash,
    };

    use crate::storage::storage_manager::StorageState;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{}_{}", prefix, now))
    }

    #[test]
    fn test_read_and_write_single_file_mode() {
        let download_dir = unique_temp_dir("bt_storage_single");
        fs::create_dir_all(&download_dir).unwrap();

        let piece_length = 512i64;
        let total_length = 1024i64;
        let num_pieces = (total_length as f64 / piece_length as f64).ceil() as usize;
        let pieces = vec![[0u8; 20]; num_pieces];

        let info = Arc::new(Info {
            piece_length,
            pieces,
            private: None,
            mode: metainfo::FileMode::SingleFile {
                name: "test_single.bin".to_string(),
                length: total_length,
                md5sum: None,
            },
        });

        let info_hash = InfoHash::new([0u8; 20]);

        let state = StorageState::new(download_dir.clone());
        state.add_torrent(info_hash, info);
        let cache = state.get_torrent(&info_hash).unwrap();

        let piece0_data: Vec<u8> = (0..piece_length).map(|i| i as u8).collect();
        let piece1_data: Vec<u8> = (0..piece_length).map(|i| (i + 128) as u8).collect();

        state
            .write(&cache, 0, &piece0_data)
            .expect("write piece 0 failed");
        state
            .write(&cache, 1, &piece1_data)
            .expect("write piece 1 failed");

        let read_piece0 = state
            .read(&cache, 0, 0, piece_length as u32)
            .expect("read piece0 failed");
        assert_eq!(read_piece0.data, piece0_data);

        let read_piece1 = state
            .read(&cache, 1, 0, piece_length as u32)
            .expect("read piece1 failed");
        assert_eq!(read_piece1.data, piece1_data);

        let block = state
            .read(&cache, 0, 256, 256)
            .expect("read block failed");
        assert_eq!(block.data, &piece0_data[256..512]);

        fs::remove_dir_all(&download_dir).unwrap();
    }

    #[test]
    fn test_read_and_write_multi_file_mode() {
        let download_dir = unique_temp_dir("bt_storage_multi");
        fs::create_dir_all(&download_dir).unwrap();

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

        let info = Arc::new(Info {
            piece_length,
            pieces,
            private: None,
            mode: metainfo::FileMode::MultiFile {
                name: "test_multi".to_string(),
                files: vec![f1.clone(), f2.clone()],
            },
        });

        let id = InfoHash::new([1u8; 20]);

        let state = StorageState::new(download_dir.clone());
        state.add_torrent(id, info);
        let cache = state.get_torrent(&id).unwrap();

        let piece0_data: Vec<u8> = (0..512).map(|i| i as u8).collect();
        let piece1_data: Vec<u8> = (0..512).map(|i| (i + 100) as u8).collect();
        let piece2_data: Vec<u8> = (0..76).map(|i| (i + 200) as u8).collect();

        state
            .write(&cache, 0, &piece0_data)
            .expect("write piece 0 failed");
        state
            .write(&cache, 1, &piece1_data)
            .expect("write piece 1 failed");
        state
            .write(&cache, 2, &piece2_data)
            .expect("write piece 2 failed");

        let read_p0 = state.read(&cache, 0, 0, 512).expect("read p0 failed");
        assert_eq!(read_p0.data, piece0_data);

        let read_p1 = state.read(&cache, 1, 0, 512).expect("read p1 failed");
        assert_eq!(read_p1.data, piece1_data);

        let read_p2 = state.read(&cache, 2, 0, 76).expect("read p2 failed");
        assert_eq!(read_p2.data, piece2_data);

        let spanning_block = state.read(&cache, 1, 88, 100).expect("spanning read failed");
        assert_eq!(spanning_block.data.len(), 100);

        fs::remove_dir_all(&download_dir).unwrap();
    }

    #[test]
    fn test_file_handle_caching() {
        let download_dir = unique_temp_dir("bt_storage_cache");
        fs::create_dir_all(&download_dir).unwrap();

        let info = Arc::new(Info {
            piece_length: 512,
            pieces: vec![[0u8; 20]; 2],
            private: None,
            mode: metainfo::FileMode::SingleFile {
                name: "cache_test.bin".to_string(),
                length: 1024,
                md5sum: None,
            },
        });

        let id = InfoHash::new([4u8; 20]);

        let state = StorageState::new(download_dir.clone());
        state.add_torrent(id, info);
        let cache = state.get_torrent(&id).unwrap();

        // Initial state: no file handles cached
        assert_eq!(
            state
                .torrents
                .read()
                .unwrap()
                .get(&id)
                .unwrap()
                .file_handles
                .lock()
                .unwrap()
                .len(),
            0
        );

        // First write should open and cache the file handle
        let data = vec![42u8; 512];
        state.write(&cache, 0, &data).expect("write failed");
        assert_eq!(
            state
                .torrents
                .read()
                .unwrap()
                .get(&id)
                .unwrap()
                .file_handles
                .lock()
                .unwrap()
                .len(),
            1
        );

        // Subsequent operations should reuse the cached handle
        state.write(&cache, 1, &data).expect("second write failed");
        assert_eq!(
            state
                .torrents
                .read()
                .unwrap()
                .get(&id)
                .unwrap()
                .file_handles
                .lock()
                .unwrap()
                .len(),
            1
        );

        let _read_data = state.read(&cache, 0, 0, 256).expect("read failed");
        assert_eq!(
            state
                .torrents
                .read()
                .unwrap()
                .get(&id)
                .unwrap()
                .file_handles
                .lock()
                .unwrap()
                .len(),
            1
        );

        fs::remove_dir_all(&download_dir).unwrap();
    }
}
