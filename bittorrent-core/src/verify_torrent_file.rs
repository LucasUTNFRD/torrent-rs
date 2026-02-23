use std::{fs::File, io, os::unix::fs::FileExt, path::Path};

use bittorrent_common::metainfo::{FileInfo, FileMode, TorrentInfo};
use sha1::{Digest, Sha1};

use crate::torrent::TorrentError;

pub fn verify_content(src: &Path, metainfo: &TorrentInfo) -> Result<bool, TorrentError> {
    let files = metainfo.files();
    let piece_length = metainfo.info.piece_length as usize;
    let total_size = metainfo.total_size() as usize;
    let num_pieces = metainfo.num_pieces();

    let content_root = match &metainfo.info.mode {
        FileMode::SingleFile { .. } => src.to_path_buf(),
        FileMode::MultiFile { name, .. } => src.join(name),
    };

    for piece_index in 0..num_pieces {
        let piece_len = calculate_piece_length(piece_index, piece_length, total_size);
        let data = read_piece(&content_root, &files, piece_index, piece_length, piece_len)?;

        let mut hasher = Sha1::new();
        hasher.update(&data);
        let calculated: [u8; 20] = hasher.finalize().into();

        let expected = metainfo
            .get_piece_hash(piece_index)
            .expect("piece hash must exist for valid piece index");

        if calculated != *expected {
            return Ok(false);
        }
    }

    Ok(true)
}

fn calculate_piece_length(piece_index: usize, piece_length: usize, total_size: usize) -> usize {
    let piece_start = piece_index * piece_length;
    let remaining = total_size.saturating_sub(piece_start);
    piece_length.min(remaining)
}

fn read_piece(
    content_root: &Path,
    files: &[FileInfo],
    piece_index: usize,
    piece_length: usize,
    piece_len: usize,
) -> io::Result<Vec<u8>> {
    let mut data = vec![0u8; piece_len];
    let mut global_offset = piece_index * piece_length;
    let mut bytes_read = 0;

    for file_info in files {
        let file_len = file_info.length as usize;

        if global_offset >= file_len {
            global_offset -= file_len;
            continue;
        }

        let file_path = content_root.join(&file_info.path);
        let file = File::open(&file_path)?;

        let start_in_file = global_offset;
        let available_in_file = file_len - start_in_file;
        let remaining_to_read = piece_len - bytes_read;
        let read_len = available_in_file.min(remaining_to_read);

        file.read_exact_at(
            &mut data[bytes_read..bytes_read + read_len],
            start_in_file as u64,
        )?;

        bytes_read += read_len;
        global_offset = 0;

        if bytes_read >= piece_len {
            break;
        }
    }

    Ok(data)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::Write,
        path::PathBuf,
        sync::Arc,
    };

    use bittorrent_common::{metainfo::Info, types::InfoHash};

    use super::*;

    fn compute_piece_hashes(data: &[u8], piece_length: i64) -> Vec<[u8; 20]> {
        let mut hashes = Vec::new();
        for chunk in data.chunks(piece_length as usize) {
            let mut hasher = Sha1::new();
            hasher.update(chunk);
            hashes.push(hasher.finalize().into());
        }
        hashes
    }

    fn create_test_torrent_info_single(name: &str, data: &[u8], piece_length: i64) -> TorrentInfo {
        let pieces = compute_piece_hashes(data, piece_length);

        TorrentInfo {
            info: Arc::new(Info {
                piece_length,
                pieces,
                private: None,
                mode: FileMode::SingleFile {
                    name: name.to_string(),
                    length: data.len() as i64,
                    md5sum: None,
                },
            }),
            announce: "http://tracker.example.com/announce".to_string(),
            announce_list: None,
            creation_date: None,
            comment: None,
            created_by: None,
            encoding: None,
            info_hash: InfoHash::new([0u8; 20]),
        }
    }

    fn create_test_torrent_info_multi(
        name: &str,
        files: Vec<(&str, &[u8])>,
        piece_length: i64,
    ) -> (TorrentInfo, Vec<u8>) {
        let mut concatenated = Vec::new();
        let mut file_infos = Vec::new();

        for (path, data) in &files {
            concatenated.extend_from_slice(data);
            file_infos.push(FileInfo {
                length: data.len() as i64,
                md5sum: None,
                path: PathBuf::from(path),
            });
        }

        let pieces = compute_piece_hashes(&concatenated, piece_length);

        let torrent_info = TorrentInfo {
            info: Arc::new(Info {
                piece_length,
                pieces,
                private: None,
                mode: FileMode::MultiFile {
                    name: name.to_string(),
                    files: file_infos,
                },
            }),
            announce: "http://tracker.example.com/announce".to_string(),
            announce_list: None,
            creation_date: None,
            comment: None,
            created_by: None,
            encoding: None,
            info_hash: InfoHash::new([0u8; 20]),
        };

        (torrent_info, concatenated)
    }

    #[test]
    fn test_verify_single_file_valid() {
        let temp_dir = tempfile::tempdir().unwrap();
        let piece_length = 512i64;

        let data: Vec<u8> = (0..1024).map(|i| i as u8).collect();
        let file_path = temp_dir.path().join("test.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        let metainfo = create_test_torrent_info_single("test.bin", &data, piece_length);

        let result = verify_content(temp_dir.path(), &metainfo).unwrap();
        assert!(result);
    }

    #[test]
    fn test_verify_single_file_invalid() {
        let temp_dir = tempfile::tempdir().unwrap();
        let piece_length = 512i64;

        let original_data: Vec<u8> = (0..1024).map(|i| i as u8).collect();
        let metainfo = create_test_torrent_info_single("test.bin", &original_data, piece_length);

        let corrupted_data: Vec<u8> = (0..1024).map(|i| (i + 1) as u8).collect();
        let file_path = temp_dir.path().join("test.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(&corrupted_data).unwrap();
        drop(file);

        let result = verify_content(temp_dir.path(), &metainfo).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_verify_multi_file_valid() {
        let temp_dir = tempfile::tempdir().unwrap();
        let piece_length = 512i64;

        let file1_data: Vec<u8> = (0..300).map(|i| i as u8).collect();
        let file2_data: Vec<u8> = (0..400).map(|i| (i + 100) as u8).collect();
        let file3_data: Vec<u8> = (0..200).map(|i| (i + 200) as u8).collect();

        let files = vec![
            ("file1.bin", file1_data.as_slice()),
            ("subdir/file2.bin", file2_data.as_slice()),
            ("file3.bin", file3_data.as_slice()),
        ];

        let (metainfo, _) =
            create_test_torrent_info_multi("test_multi", files.clone(), piece_length);

        let torrent_dir = temp_dir.path().join("test_multi");
        fs::create_dir_all(torrent_dir.join("subdir")).unwrap();

        for (path, data) in &files {
            let file_path = torrent_dir.join(path);
            let mut file = File::create(&file_path).unwrap();
            file.write_all(data).unwrap();
        }

        let result = verify_content(temp_dir.path(), &metainfo).unwrap();
        assert!(result);
    }

    #[test]
    fn test_verify_multi_file_piece_spans_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let piece_length = 256i64;

        let file1_data: Vec<u8> = (0..200).map(|i| i as u8).collect();
        let file2_data: Vec<u8> = (0..300).map(|i| (i + 100) as u8).collect();

        let files = vec![
            ("file1.bin", file1_data.as_slice()),
            ("file2.bin", file2_data.as_slice()),
        ];

        let (metainfo, _) =
            create_test_torrent_info_multi("test_span", files.clone(), piece_length);

        let torrent_dir = temp_dir.path().join("test_span");
        fs::create_dir_all(&torrent_dir).unwrap();

        for (path, data) in &files {
            let file_path = torrent_dir.join(path);
            let mut file = File::create(&file_path).unwrap();
            file.write_all(data).unwrap();
        }

        let result = verify_content(temp_dir.path(), &metainfo).unwrap();
        assert!(result);
    }

    #[test]
    fn test_verify_multi_file_invalid() {
        let temp_dir = tempfile::tempdir().unwrap();
        let piece_length = 512i64;

        let file1_data: Vec<u8> = (0..300).map(|i| i as u8).collect();
        let file2_data: Vec<u8> = (0..400).map(|i| (i + 100) as u8).collect();

        let files = vec![
            ("file1.bin", file1_data.as_slice()),
            ("file2.bin", file2_data.as_slice()),
        ];

        let (metainfo, _) =
            create_test_torrent_info_multi("test_invalid", files.clone(), piece_length);

        let torrent_dir = temp_dir.path().join("test_invalid");
        fs::create_dir_all(&torrent_dir).unwrap();

        let corrupted_file2: Vec<u8> = (0..400).map(|i| (i + 200) as u8).collect();
        let file_path = torrent_dir.join("file2.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(&corrupted_file2).unwrap();

        let file_path = torrent_dir.join("file1.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(&file1_data).unwrap();

        let result = verify_content(temp_dir.path(), &metainfo).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_verify_last_piece_smaller() {
        let temp_dir = tempfile::tempdir().unwrap();
        let piece_length = 512i64;

        let data: Vec<u8> = (0..700).map(|i| i as u8).collect();
        let file_path = temp_dir.path().join("test.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        let metainfo = create_test_torrent_info_single("test.bin", &data, piece_length);

        let result = verify_content(temp_dir.path(), &metainfo).unwrap();
        assert!(result);
        assert_eq!(metainfo.num_pieces(), 2);
    }
}
