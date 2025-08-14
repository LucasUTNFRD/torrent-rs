use std::{collections::BTreeMap, fs::File, io::Read, path::PathBuf};

use bencode::bencode::{Bencode, BencodeError, Encode};
use sha1::{Digest, Sha1};

use crate::types::InfoHash;

/// Main metainfo structure representing a .torrent file
#[derive(Debug, Clone)]
pub struct TorrentInfo {
    /// Dictionary describing the file(s) of the torrent
    pub info: Info,

    /// The announce URL of the tracker
    pub announce: String,

    /// CHECK [BEP_0012](https://www.bittorrent.org/beps/bep_0012.html)
    pub announce_list: Option<Vec<Vec<String>>>,

    pub creation_date: Option<i64>,

    pub comment: Option<String>,

    pub created_by: Option<String>,

    pub encoding: Option<String>,

    pub info_hash: InfoHash,
}

/// Info dictionary containing torrent file information
#[derive(Debug, Clone)]
pub struct Info {
    /// Number of bytes in each piece
    pub piece_length: i64,

    /// Concatenation of all 20-byte SHA1 hash values
    pub pieces: Vec<[u8; 20]>,

    /// Optional: Private tracker flag (1 = private, 0 or absent = public)
    pub private: Option<i64>,

    /// File mode - either single file or multiple files
    pub mode: FileMode,
}

/// Represents either single-file or multi-file torrent mode
#[derive(Debug, Clone)]
pub enum FileMode {
    /// Single file torrent
    SingleFile {
        /// Filename (advisory)
        name: String,
        /// File length in bytes
        length: i64,
        /// Optional: MD5 sum of the file
        md5sum: Option<String>,
    },
    /// Multi-file torrent
    MultiFile {
        /// Directory name to store all files (advisory)
        name: String,
        /// List of files in the torrent
        files: Vec<FileInfo>,
    },
}

impl FileMode {
    /// Check if this is a single-file torrent
    pub fn is_single_file(&self) -> bool {
        matches!(self, FileMode::SingleFile { .. })
    }

    /// Check if this is a multi-file torrent
    pub fn is_multi_file(&self) -> bool {
        matches!(self, FileMode::MultiFile { .. })
    }

    pub fn length(&self) -> i64 {
        match &self {
            FileMode::SingleFile { length, .. } => *length,
            FileMode::MultiFile { files, .. } => files.iter().map(|f| f.length).sum(),
        }
    }

    /// Get the name of the torrent (filename for single-file, directory name for multi-file)
    pub fn name(&self) -> &str {
        match &self {
            FileMode::SingleFile { name, .. } => name,
            FileMode::MultiFile { name, .. } => name,
        }
    }
}

/// Information about a single file in a multi-file torrent
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// File length in bytes
    pub length: i64,

    /// Optional: MD5 sum of the file
    pub md5sum: Option<String>,

    /// Path components representing the file path
    /// We use a custom deserializer to convert Vec<String> to PathBuf
    pub path: PathBuf,
}

impl TorrentInfo {
    /// Get the total size of all files in the torrent
    pub fn total_size(&self) -> i64 {
        self.info.mode.length()
    }

    /// Get the number of pieces in the torrent
    pub fn num_pieces(&self) -> usize {
        (self.total_size() as f64 / self.info.piece_length as f64).ceil() as usize
    }

    /// Get all tracker URLs (primary + announce-list)
    pub fn all_trackers(&self) -> Vec<String> {
        let mut trackers = vec![self.announce.clone()];

        if let Some(announce_list) = &self.announce_list {
            for tier in announce_list {
                trackers.extend(tier.clone());
            }
        }

        // Remove duplicates while preserving order
        let mut seen = std::collections::HashSet::new();
        trackers.retain(|url| seen.insert(url.clone()));

        trackers
    }

    /// Check if this is a private torrent
    pub fn is_private(&self) -> bool {
        self.info.private == Some(1)
    }
}

impl FileInfo {
    /// Get the filename (last component of the path)
    pub fn filename(&self) -> Option<&str> {
        self.path.file_name().and_then(|s| s.to_str())
    }

    /// Get the directory path (parent directory)
    pub fn directory_path(&self) -> Option<&std::path::Path> {
        self.path.parent()
    }

    /// Get the file extension
    pub fn extension(&self) -> Option<&str> {
        self.path.extension().and_then(|s| s.to_str())
    }

    /// Get the full path as a string
    pub fn path_str(&self) -> Option<&str> {
        self.path.to_str()
    }

    /// Get the path components as a vector of strings (for bencode serialization)
    pub fn path_components(&self) -> Vec<String> {
        self.path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect()
    }
}

// Constants for bencode dictionary keys
const KEY_INFO: &[u8] = b"info";
const KEY_ANNOUNCE: &[u8] = b"announce";
const KEY_ANNOUNCE_LIST: &[u8] = b"announce-list";
const KEY_CREATION_DATE: &[u8] = b"creation date";
const KEY_COMMENT: &[u8] = b"comment";
const KEY_CREATED_BY: &[u8] = b"created by";
const KEY_ENCODING: &[u8] = b"encoding";

// Info dictionary keys
const KEY_PIECE_LENGTH: &[u8] = b"piece length";
const KEY_PIECES: &[u8] = b"pieces";
const KEY_PRIVATE: &[u8] = b"private";
const KEY_NAME: &[u8] = b"name";
const KEY_LENGTH: &[u8] = b"length";
const KEY_MD5SUM: &[u8] = b"md5sum";
const KEY_FILES: &[u8] = b"files";
const KEY_PATH: &[u8] = b"path";

#[derive(Debug, thiserror::Error)]
pub enum TorrentParseError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Bencode error: {0}")]
    BencodeError(#[from] BencodeError),
    #[error("Missing required field: {0}")]
    MissingField(String),
    #[error("Invalid field type: {0}")]
    InvalidFieldType(String),
    #[error("Invalid UTF-8 string")]
    InvalidUtf8,
}

pub fn parse_torrent_from_file(path: &str) -> Result<TorrentInfo, TorrentParseError> {
    let mut file = File::open(path)?;
    let mut byte_buf: Vec<u8> = Vec::new();
    file.read_to_end(&mut byte_buf)?;

    let bencode = Bencode::decode(&byte_buf)?;
    parse_torrent_from_bencode(&bencode)
}

fn parse_torrent_from_bencode(bencode: &Bencode) -> Result<TorrentInfo, TorrentParseError> {
    let dict = match bencode {
        Bencode::Dict(dict) => dict,
        _ => {
            return Err(TorrentParseError::InvalidFieldType(
                "Root must be a dictionary".to_string(),
            ));
        }
    };

    // Parse required fields
    let announce = get_string_from_dict(dict, KEY_ANNOUNCE)?;
    let info_bencode = dict
        .get(KEY_INFO)
        .ok_or_else(|| TorrentParseError::MissingField("info".to_string()))?;

    // Parse info dictionary
    let info = parse_info_dict(info_bencode)?;
    let info_hash = calculate_info_hash(&info)?;

    // Parse optional fields
    let announce_list = parse_announce_list(dict)?;
    let creation_date = get_optional_int_from_dict(dict, KEY_CREATION_DATE);
    let comment = get_optional_string_from_dict(dict, KEY_COMMENT);
    let created_by = get_optional_string_from_dict(dict, KEY_CREATED_BY);
    let encoding = get_optional_string_from_dict(dict, KEY_ENCODING);

    Ok(TorrentInfo {
        info,
        announce,
        announce_list,
        creation_date,
        comment,
        created_by,
        encoding,
        info_hash: info_hash.into(),
    })
}

fn parse_info_dict(info_bencode: &Bencode) -> Result<Info, TorrentParseError> {
    let dict = match info_bencode {
        Bencode::Dict(dict) => dict,
        _ => {
            return Err(TorrentParseError::InvalidFieldType(
                "Info must be a dictionary".to_string(),
            ));
        }
    };

    // Parse required fields
    let piece_length = get_int_from_dict(dict, KEY_PIECE_LENGTH)?;
    let pieces_bytes = get_bytes_from_dict(dict, KEY_PIECES)?;
    let pieces = parse_pieces(&pieces_bytes)?;
    let private = get_optional_int_from_dict(dict, KEY_PRIVATE);

    // Determine file mode
    let mode = if dict.contains_key(KEY_FILES) {
        // Multi-file mode
        let name = get_string_from_dict(dict, KEY_NAME)?;
        let files = parse_files_list(dict)?;
        FileMode::MultiFile { name, files }
    } else {
        // Single-file mode
        let name = get_string_from_dict(dict, KEY_NAME)?;
        let length = get_int_from_dict(dict, KEY_LENGTH)?;
        let md5sum = get_optional_string_from_dict(dict, KEY_MD5SUM);
        FileMode::SingleFile {
            name,
            length,
            md5sum,
        }
    };

    Ok(Info {
        piece_length,
        pieces,
        private,
        mode,
    })
}

fn parse_pieces(pieces_bytes: &[u8]) -> Result<Vec<[u8; 20]>, TorrentParseError> {
    if pieces_bytes.len() % 20 != 0 {
        return Err(TorrentParseError::InvalidFieldType(
            "Pieces length must be multiple of 20".to_string(),
        ));
    }

    let pieces = pieces_bytes
        .chunks_exact(20)
        .map(|chunk| {
            let mut piece = [0u8; 20];
            piece.copy_from_slice(chunk);
            piece
        })
        .collect();

    Ok(pieces)
}

fn parse_files_list(
    dict: &std::collections::BTreeMap<Vec<u8>, Bencode>,
) -> Result<Vec<FileInfo>, TorrentParseError> {
    let files_bencode = dict
        .get(KEY_FILES)
        .ok_or_else(|| TorrentParseError::MissingField("files".to_string()))?;

    let files_list = match files_bencode {
        Bencode::List(list) => list,
        _ => {
            return Err(TorrentParseError::InvalidFieldType(
                "Files must be a list".to_string(),
            ));
        }
    };

    let mut files = Vec::new();
    for file_bencode in files_list {
        let file_dict = match file_bencode {
            Bencode::Dict(dict) => dict,
            _ => {
                return Err(TorrentParseError::InvalidFieldType(
                    "File entry must be a dictionary".to_string(),
                ));
            }
        };

        let length = get_int_from_dict(file_dict, KEY_LENGTH)?;
        let md5sum = get_optional_string_from_dict(file_dict, KEY_MD5SUM);
        let path = parse_file_path(file_dict)?;

        files.push(FileInfo {
            length,
            md5sum,
            path,
        });
    }

    Ok(files)
}

fn parse_file_path(
    dict: &std::collections::BTreeMap<Vec<u8>, Bencode>,
) -> Result<PathBuf, TorrentParseError> {
    let path_bencode = dict
        .get(KEY_PATH)
        .ok_or_else(|| TorrentParseError::MissingField("path".to_string()))?;

    let path_list = match path_bencode {
        Bencode::List(list) => list,
        _ => {
            return Err(TorrentParseError::InvalidFieldType(
                "Path must be a list".to_string(),
            ));
        }
    };

    let mut path = PathBuf::new();
    for component_bencode in path_list {
        let component_bytes = match component_bencode {
            Bencode::Bytes(bytes) => bytes,
            _ => {
                return Err(TorrentParseError::InvalidFieldType(
                    "Path component must be bytes".to_string(),
                ));
            }
        };

        let component_str =
            std::str::from_utf8(component_bytes).map_err(|_| TorrentParseError::InvalidUtf8)?;
        path.push(component_str);
    }

    Ok(path)
}

fn parse_announce_list(
    dict: &std::collections::BTreeMap<Vec<u8>, Bencode>,
) -> Result<Option<Vec<Vec<String>>>, TorrentParseError> {
    let announce_list_bencode = match dict.get(KEY_ANNOUNCE_LIST) {
        Some(bencode) => bencode,
        None => return Ok(None),
    };

    let announce_list = match announce_list_bencode {
        Bencode::List(list) => list,
        _ => {
            return Err(TorrentParseError::InvalidFieldType(
                "Announce-list must be a list".to_string(),
            ));
        }
    };

    let mut result = Vec::new();
    for tier_bencode in announce_list {
        let tier_list = match tier_bencode {
            Bencode::List(list) => list,
            _ => {
                return Err(TorrentParseError::InvalidFieldType(
                    "Announce-list tier must be a list".to_string(),
                ));
            }
        };

        let mut tier = Vec::new();
        for url_bencode in tier_list {
            let url_bytes = match url_bencode {
                Bencode::Bytes(bytes) => bytes,
                _ => {
                    return Err(TorrentParseError::InvalidFieldType(
                        "Announce URL must be bytes".to_string(),
                    ));
                }
            };

            let url = std::str::from_utf8(url_bytes).map_err(|_| TorrentParseError::InvalidUtf8)?;
            tier.push(url.to_string());
        }
        result.push(tier);
    }

    Ok(Some(result))
}

fn calculate_info_hash(info: &Info) -> Result<[u8; 20], TorrentParseError> {
    let bencoded_info = Bencode::encode(info);

    let hash_generic_array = Sha1::digest(&bencoded_info);
    let hash_array: [u8; 20] = hash_generic_array.into();

    Ok(hash_array)
}

impl Encode for Info {
    fn to_bencode(&self) -> Bencode {
        let mut dict = BTreeMap::new();
        dict.insert(KEY_LENGTH.to_vec(), Bencode::Int(self.mode.length()));
        dict.insert(
            KEY_NAME.to_vec(),
            Bencode::Bytes(self.mode.name().as_bytes().to_vec()),
        );
        dict.insert(KEY_PIECE_LENGTH.to_vec(), Bencode::Int(self.piece_length));
        let concatendated_hashes: Vec<u8> = self
            .pieces
            .iter()
            .flat_map(|hash| hash.iter())
            .copied()
            .collect();
        dict.insert(KEY_PIECES.to_vec(), Bencode::Bytes(concatendated_hashes));
        Bencode::Dict(dict)
    }
}

// Helper functions for extracting values from bencode dictionaries
fn get_string_from_dict(
    dict: &std::collections::BTreeMap<Vec<u8>, Bencode>,
    key: &[u8],
) -> Result<String, TorrentParseError> {
    let bytes = get_bytes_from_dict(dict, key)?;
    std::str::from_utf8(&bytes)
        .map(|s| s.to_string())
        .map_err(|_| TorrentParseError::InvalidUtf8)
}

fn get_optional_string_from_dict(
    dict: &std::collections::BTreeMap<Vec<u8>, Bencode>,
    key: &[u8],
) -> Option<String> {
    dict.get(key).and_then(|bencode| match bencode {
        Bencode::Bytes(bytes) => std::str::from_utf8(bytes).ok().map(|s| s.to_string()),
        _ => None,
    })
}

fn get_bytes_from_dict(
    dict: &std::collections::BTreeMap<Vec<u8>, Bencode>,
    key: &[u8],
) -> Result<Vec<u8>, TorrentParseError> {
    match dict.get(key) {
        Some(Bencode::Bytes(bytes)) => Ok(bytes.clone()),
        Some(_) => Err(TorrentParseError::InvalidFieldType(format!(
            "Expected bytes for key {:?}",
            std::str::from_utf8(key)
        ))),
        None => Err(TorrentParseError::MissingField(format!(
            "Key {:?} not found",
            std::str::from_utf8(key)
        ))),
    }
}

fn get_int_from_dict(
    dict: &std::collections::BTreeMap<Vec<u8>, Bencode>,
    key: &[u8],
) -> Result<i64, TorrentParseError> {
    match dict.get(key) {
        Some(Bencode::Int(value)) => Ok(*value),
        Some(_) => Err(TorrentParseError::InvalidFieldType(format!(
            "Expected integer for key {:?}",
            std::str::from_utf8(key)
        ))),
        None => Err(TorrentParseError::MissingField(format!(
            "Key {:?} not found",
            std::str::from_utf8(key)
        ))),
    }
}

fn get_optional_int_from_dict(
    dict: &std::collections::BTreeMap<Vec<u8>, Bencode>,
    key: &[u8],
) -> Option<i64> {
    dict.get(key).and_then(|bencode| match bencode {
        Bencode::Int(value) => Some(*value),
        _ => None,
    })
}
