use std::{
    collections::HashMap,
    fs::File,
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use crate::storage::{StorageError, StorageMessage, open_file};

use bittorrent_common::{
    metainfo::{FileInfo, Info},
    types::InfoHash,
};
use bytes::BytesMut;
use peer_protocol::protocol::Block;
use sha1::{Digest, Sha1};
use tokio::sync::mpsc;

pub struct StorageManager {
    storage: Arc<StorageState>,
    pub rx: tokio::sync::mpsc::Receiver<StorageMessage>,
}

pub(crate) struct StorageState {
    pub download_dir: PathBuf,
    pub torrents: RwLock<HashMap<InfoHash, Arc<TorrentCache>>>,
}

pub struct TorrentCache {
    pub metainfo: Arc<Info>,
    pub name: String,
    pub files: Arc<[FileInfo]>,
    // Because pread()/pwrite() (used by FileExt::read_exact_at / write_all_at) are atomic -- they don't modify the file descriptor's internal seek position. Multiple threads can safely do positional I/O on the same File concurrently. Arc<File> lets us hand out a clone of the file handle to a spawn_blocking closure without holding the Mutex during the actual I/O.
    pub file_handles: Mutex<HashMap<PathBuf, Arc<File>>>,
}

impl StorageState {
    pub fn new(download_dir: PathBuf) -> Self {
        Self {
            download_dir,
            torrents: RwLock::new(HashMap::default()),
        }
    }

    /// Look up a torrent by info hash, returning a cloned `Arc` so the caller
    /// keeps the `TorrentCache` alive even if the entry is concurrently removed
    /// from the map.
    pub fn get_torrent(&self, info_hash: &InfoHash) -> Option<Arc<TorrentCache>> {
        self.torrents.read().unwrap().get(info_hash).map(Arc::clone)
    }

    pub fn write(&self, cache: &TorrentCache, piece_index: u32, data: &[u8]) -> io::Result<()> {
        let files = Arc::clone(&cache.files);
        let piece_length = cache.metainfo.piece_length as usize;

        // Calculate the global byte offset for this piece
        let mut global_offset = piece_index as usize * piece_length;
        let mut remaining_data = data;

        // Iterate through files to find where to start writing
        for file_info in files.iter() {
            let file_len = file_info.length as usize;

            // If this entire file is before our starting offset, skip it
            if global_offset >= file_len {
                global_offset -= file_len;
                continue;
            }

            // Get or open the file handle
            let file = self.get_or_open_file(cache, &file_info.path)?;

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

    pub fn read(
        &self,
        cache: &TorrentCache,
        piece_index: u32,
        begin: u32,
        length: u32,
    ) -> io::Result<Block> {
        let files = Arc::clone(&cache.files);
        let piece_length = cache.metainfo.piece_length as usize;

        // Calculate global byte offset for this read
        let mut global_offset = (piece_index as usize * piece_length) + begin as usize;

        let mut data = BytesMut::zeroed(length as usize);
        let mut bytes_read = 0;

        // Iterate through files to find where to start reading
        for file_info in files.iter() {
            let file_len = file_info.length as usize;

            // If this entire file is before our starting offset, skip it
            if global_offset >= file_len {
                global_offset -= file_len;
                continue;
            }

            // Get or open the file handle
            let file = self.get_or_open_file(cache, &file_info.path)?;

            // Calculate how much we can read from this file
            let start_in_file = global_offset;
            let available_in_file = file_len - start_in_file;
            let remaining_to_read = data.len() - bytes_read;
            let read_len = available_in_file.min(remaining_to_read);

            // Read from this file
            file.read_exact_at(
                &mut data[bytes_read..bytes_read + read_len],
                start_in_file as u64,
            )?;

            // Update counters and reset offset for next file
            bytes_read += read_len;
            global_offset = 0;

            // If we've read all requested data, we're done
            if bytes_read >= data.len() {
                break;
            }
        }

        if bytes_read != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Could not read all requested bytes",
            ));
        }

        Ok(Block {
            index: piece_index,
            begin,
            data: data.freeze(),
        })
    }

    fn get_or_open_file(&self, cache: &TorrentCache, file_path: &Path) -> io::Result<Arc<File>> {
        // Lock the per-torrent file handle cache.  This is the only
        // Mutex -- its critical section is tiny (HashMap lookup + possible insert).
        // The actual I/O happens AFTER we release this lock, on the Arc<File>.
        let mut handles = cache.file_handles.lock().unwrap();

        if let Some(file) = handles.get(file_path) {
            return Ok(Arc::clone(file));
        }

        let torrent_root = self.download_dir.join(&cache.name);

        // First file opened for this torrent: ensure parent dirs exist.
        if handles.is_empty() {
            for fi in cache.files.iter() {
                let full_path = torrent_root.join(&fi.path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
            }
        }

        let full_path = torrent_root.join(file_path);
        let file = Arc::new(open_file(&full_path)?);
        handles.insert(file_path.to_path_buf(), Arc::clone(&file));
        Ok(file)
    }

    pub fn add_torrent(&self, info_hash: InfoHash, meta: Arc<Info>) {
        let name = meta.mode.name().to_string();
        let files = meta.files();

        for fi in &files {
            let full_path = self.download_dir.join(&name).join(&fi.path);

            if let Some(parent) = full_path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                eprintln!(
                    "storage: failed to create directory {:}: {e}",
                    parent.display()
                );
            }
        }

        self.torrents.write().unwrap().insert(
            info_hash,
            Arc::new(TorrentCache {
                metainfo: meta,
                name,
                files: files.into_boxed_slice().into(),
                file_handles: Mutex::new(HashMap::new()),
            }),
        );
    }
}

impl StorageManager {
    pub fn new(download_dir: PathBuf, rx: mpsc::Receiver<StorageMessage>) -> Self {
        Self {
            rx,
            storage: Arc::new(StorageState::new(download_dir)),
        }
    }

    pub async fn start(mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                StorageMessage::AddTorrent {
                    info_hash,
                    meta,
                    result_tx,
                } => {
                    self.storage.add_torrent(info_hash, meta);
                    let _ = result_tx.send(Ok(()));
                }
                StorageMessage::RemoveTorrent { id, result_tx } => {
                    // Run inline -- the write lock is O(1) and does not perform
                    // I/O, so it won't block the event loop appreciably.
                    // Running inline guarantees removal is ordered with respect
                    // to subsequent messages so we don't need to coordinate with
                    // in-flight spawned tasks.
                    self.storage.torrents.write().unwrap().remove(&id);
                    let _ = result_tx.send(Ok(()));
                }
                StorageMessage::Read {
                    id,
                    piece_index,
                    begin,
                    length,
                    result_tx,
                } => {
                    // Snapshot the Arc<TorrentCache> *before* spawning so the
                    // cache stays alive even if RemoveTorrent runs before the
                    // blocking task acquires the lock.
                    let Some(cache) = self.storage.get_torrent(&id) else {
                        let _ = result_tx.send(Err(StorageError::TorrentNotFound(id)));
                        continue;
                    };
                    let storage = self.storage.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = storage
                            .read(&cache, piece_index, begin, length)
                            .map_err(StorageError::from);
                        let _ = result_tx.send(result);
                    });
                }
                StorageMessage::Write {
                    info_hash,
                    piece,
                    data,
                    result_tx,
                } => {
                    let Some(cache) = self.storage.get_torrent(&info_hash) else {
                        let _ = result_tx.send(Err(StorageError::TorrentNotFound(info_hash)));
                        continue;
                    };
                    let storage = self.storage.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = storage
                            .write(&cache, piece, &data)
                            .map_err(StorageError::from);
                        let _ = result_tx.send(result);
                    });
                }
                StorageMessage::Verify {
                    info_hash,
                    piece,
                    data,
                    result_tx,
                } => {
                    let Some(cache) = self.storage.get_torrent(&info_hash) else {
                        let _ = result_tx.send(Err(StorageError::TorrentNotFound(info_hash)));
                        continue;
                    };

                    let expected_piece_hash =
                        cache.metainfo.get_piece_hash(piece as usize).copied();

                    // Move CPU-intensive SHA1 computation to spawn_blocking
                    tokio::task::spawn_blocking(move || {
                        let result = expected_piece_hash.map_or(
                            Err(StorageError::PieceNotFound(piece)),
                            |expected| {
                                let mut hasher = Sha1::new();
                                hasher.update(data);
                                let piece_hash: [u8; 20] = hasher.finalize().into();
                                Ok(piece_hash == expected)
                            },
                        );

                        let _ = result_tx.send(result);
                    });
                }
            }
        }
    }
}
