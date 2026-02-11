use std::{
    collections::HashMap,
    fs::File,
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use crate::storage::open_file;

use super::StorageMessage;

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
    pub torrents: RwLock<HashMap<InfoHash, TorrentCache>>,
}

pub struct TorrentCache {
    pub metainfo: Arc<Info>,
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

    pub fn write(&self, info_hash: InfoHash, piece_index: u32, data: &[u8]) -> io::Result<()> {
        let (files, piece_length) = {
            let torrent_map = self.torrents.read().unwrap();
            let cache = torrent_map.get(&info_hash).unwrap();

            (
                Arc::clone(&cache.files),
                cache.metainfo.piece_length as usize,
            )
        };

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

    pub fn read(
        &self,
        info_hash: InfoHash,
        piece_index: u32,
        begin: u32,
        length: u32,
    ) -> io::Result<Block> {
        // First, collect the file information we need
        let (files, piece_length) = {
            let torrent_map = self.torrents.read().unwrap();

            let cache = torrent_map.get(&info_hash).unwrap();
            (cache.files.clone(), cache.metainfo.piece_length as usize)
        };

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
            let file = self.get_or_open_file(info_hash, &file_info.path)?;

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

    fn get_or_open_file(&self, info_hash: InfoHash, file_path: &Path) -> io::Result<Arc<File>> {
        // Read lock on the torrent map -- we're looking up an existing entry,
        // not inserting/removing a torrent.  Multiple threads can do this
        // concurrently for different (or the same) torrent.
        let torrents = self.torrents.read().unwrap();
        let cache = torrents
            .get(&info_hash)
            .expect("Torrent registered by storage");

        // Now lock the per-torrent file handle cache.  This is the only
        // Mutex -- its critical section is tiny (HashMap lookup + possible insert).
        // The actual I/O happens AFTER we release this lock, on the Arc<File>.
        let mut handles = cache.file_handles.lock().unwrap();

        if let Some(file) = handles.get(file_path) {
            return Ok(Arc::clone(file));
        }

        // First file opened for this torrent: ensure parent dirs exist.
        if handles.is_empty() {
            for fi in cache.files.iter() {
                let full_path = self.download_dir.join(&fi.path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
            }
        }

        let full_path = self.download_dir.join(file_path);
        let file = Arc::new(open_file(&full_path)?);
        handles.insert(file_path.to_path_buf(), Arc::clone(&file));
        Ok(file)
    }

    pub fn add_torrent(&self, info_hash: InfoHash, meta: Arc<Info>) {
        let files = meta.files();

        for fi in &files {
            let full_path = self.download_dir.join(&fi.path);

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
            TorrentCache {
                metainfo: meta,
                files: files.into_boxed_slice().into(),
                file_handles: Mutex::new(HashMap::new()),
            },
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
                StorageMessage::AddTorrent { info_hash, meta } => {
                    self.storage.add_torrent(info_hash, meta);
                }
                StorageMessage::RemoveTorrent { id } => {
                    self.storage.torrents.write().unwrap().remove(&id);
                }
                StorageMessage::Read {
                    id,
                    piece_index,
                    begin,
                    length,
                    block_rx,
                } => {
                    let data = self.storage.read(id, piece_index, begin, length);
                    let _ = block_rx.send(data);
                }
                StorageMessage::Write {
                    info_hash,
                    piece,
                    data,
                } => {
                    // TODO: return error to session
                    if let Err(e) = self.storage.write(info_hash, piece, &data) {
                        tracing::error!(?e);
                    }
                }
                StorageMessage::Verify {
                    info_hash,
                    piece,
                    data,
                    verification_tx,
                } => {
                    let expected_piece_hash = self
                        .storage
                        .torrents
                        .read()
                        .unwrap()
                        .get(&info_hash)
                        .expect("piece verification of a non-registered torrent")
                        .metainfo
                        .get_piece_hash(piece as usize)
                        .copied();

                    if let Some(expected) = expected_piece_hash {
                        let mut hasher = Sha1::new();
                        hasher.update(data);
                        let piece_hash: [u8; 20] = hasher.finalize().into();

                        let matches = piece_hash == expected;
                        if verification_tx.send(matches).is_err() {
                            tracing::error!("failed to send verification to peer");
                        }
                    } else {
                        let _ = verification_tx.send(false);
                    }
                }
            }
        }
    }
}
