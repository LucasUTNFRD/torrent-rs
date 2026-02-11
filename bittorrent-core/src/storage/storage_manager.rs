use std::{
    collections::HashMap,
    fs::File,
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
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

pub struct StorageManager {
    pub download_dir: PathBuf,
    pub torrents: HashMap<InfoHash, Cache>,
    pub rx: mpsc::Receiver<StorageMessage>,
}

pub struct Cache {
    pub metainfo: Arc<Info>,
    pub files: Vec<FileInfo>,
    pub file_handles: HashMap<PathBuf, File>,
}

impl StorageManager {
    pub fn new(download_dir: PathBuf, rx: mpsc::Receiver<StorageMessage>) -> Self {
        Self {
            download_dir,
            rx,
            torrents: HashMap::new(),
        }
    }

    pub fn handle_add_torrent(&mut self, info_hash: InfoHash, meta: Arc<Info>) {
        let files = meta.files();

        for fi in &files {
            let full_path = self.download_dir.join(&fi.path);

            if let Some(parent) = full_path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                eprintln!("storage: failed to create directory {parent:#?}: {e}");
            }
        }

        self.torrents.insert(
            info_hash,
            Cache {
                metainfo: meta,
                files,
                file_handles: HashMap::new(),
            },
        );
    }

    pub fn start(mut self) {
        while let Ok(msg) = self.rx.recv() {
            match msg {
                StorageMessage::AddTorrent { info_hash, meta } => {
                    self.handle_add_torrent(info_hash, meta);
                }
                StorageMessage::RemoveTorrent { id } => {
                    self.torrents.remove(&id);
                }
                StorageMessage::Read {
                    id,
                    piece_index,
                    begin,
                    length,
                    block_rx,
                } => {
                    let data = self.read(id, piece_index, begin, length);
                    let _ = block_rx.send(data);
                }
                StorageMessage::Write {
                    info_hash,
                    piece,
                    data,
                } => {
                    // TODO: return error to peer
                    if let Err(e) = self.write(info_hash, piece, &data) {
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
                        .torrents
                        .get(&info_hash)
                        .expect("peice verification of a non registered torrent")
                        .metainfo
                        .get_piece_hash(piece as usize);

                    match expected_piece_hash {
                        Some(expected_piece_hash) => {
                            let mut hasher = Sha1::new();
                            hasher.update(data);
                            let piece_hash: [u8; 20] = hasher.finalize().into();

                            let matches = piece_hash == *expected_piece_hash;
                            if verification_tx.send(matches).is_err() {
                                tracing::error!("failed to send verification to peer");
                            };
                        }
                        None => {
                            if verification_tx.send(false).is_err() {
                                tracing::error!("failed to send verification to peer");
                            };
                        }
                    }
                }
            }
        }
    }

    pub fn write(&mut self, info_hash: InfoHash, piece_index: u32, data: &[u8]) -> io::Result<()> {
        // First, collect the file information we need
        let (files, piece_length) = {
            let cache = self.torrents.get(&info_hash).unwrap();
            (
                // TODO: Remove this clone
                cache.files.clone(),
                cache.metainfo.piece_length as usize,
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

    pub fn read(
        &mut self,
        info_hash: InfoHash,
        piece_index: u32,
        begin: u32,
        length: u32,
    ) -> io::Result<Block> {
        // First, collect the file information we need
        let (files, piece_length) = {
            let cache = self.torrents.get(&info_hash).unwrap();
            (cache.files.clone(), cache.metainfo.piece_length as usize)
        };

        // Calculate global byte offset for this read
        let mut global_offset = (piece_index as usize * piece_length) + begin as usize;

        let mut data = BytesMut::zeroed(length as usize);
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

    fn get_or_open_file(&mut self, info_hash: InfoHash, file_path: &Path) -> io::Result<&mut File> {
        let cache = self.torrents.get_mut(&info_hash).unwrap();

        // On first access for this torrent, ensure parents exist for all files.
        // This is more efficient than creating directories on every write/read,
        // and it also covers tests that insert Cache directly (bypassing AddTorrent).
        if cache.file_handles.is_empty() {
            for fi in &cache.files {
                let full_path = self.download_dir.join(&fi.path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
            }
        }

        if !cache.file_handles.contains_key(file_path) {
            let full_path = self.download_dir.join(file_path);
            let file = open_file(&full_path)?;
            cache.file_handles.insert(file_path.to_path_buf(), file);
        }

        Ok(cache.file_handles.get_mut(file_path).unwrap())
    }
}
