use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use std::{collections::VecDeque, sync::Arc};

use anyhow::Result;
use bittorrent_core::types::{SessionStats, TorrentDetails, TorrentSummary};
use ratatui::widgets::ListState;

use crate::torrent::DaemonClient;

#[derive(PartialEq)]
pub enum Mode {
    Main,
    AddMagnet,
    FileBrowser,
}

pub struct FileBrowser {
    pub current_dir: PathBuf,
    pub entries: Vec<fs::DirEntry>,
    pub state: ListState,
}

impl FileBrowser {
    pub fn new(dir: PathBuf) -> Self {
        let mut fb = Self {
            current_dir: dir,
            entries: Vec::new(),
            state: ListState::default(),
        };
        fb.refresh();
        fb
    }

    pub fn refresh(&mut self) {
        if let Ok(read_dir) = fs::read_dir(&self.current_dir) {
            self.entries = read_dir.filter_map(|e| e.ok()).collect();
            // Sort: directories first, then files
            self.entries.sort_by(|a, b| {
                let a_is_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let b_is_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if a_is_dir && !b_is_dir {
                    std::cmp::Ordering::Less
                } else if !a_is_dir && b_is_dir {
                    std::cmp::Ordering::Greater
                } else {
                    a.file_name().cmp(&b.file_name())
                }
            });
        }
        self.state.select(Some(0));
    }

    pub fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.entries.len().saturating_sub(1) {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.entries.len().saturating_sub(1)
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn enter(&mut self) -> Option<PathBuf> {
        if let Some(i) = self.state.selected() {
            if let Some(entry) = self.entries.get(i) {
                let path = entry.path();
                if path.is_dir() {
                    self.current_dir = path;
                    self.refresh();
                    return None;
                } else if path
                    .extension()
                    .map(|ext| ext == "torrent")
                    .unwrap_or(false)
                {
                    return Some(path);
                }
            }
        }
        None
    }

    pub fn parent(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            self.current_dir = parent.to_path_buf();
            self.refresh();
        }
    }
}

#[derive(PartialEq)]
pub enum Tab {
    Torrents,
    Details,
}

pub struct App {
    pub client: Arc<DaemonClient>,
    pub torrents: Vec<TorrentSummary>,
    pub selected_torrent: Option<TorrentDetails>,
    pub list_state: ListState,
    pub stats: SessionStats,
    pub active_tab: Tab,
    pub details_tab: usize, // 0: Peers, 1: Trackers, 2: Files
    pub download_history: VecDeque<u64>,
    pub upload_history: VecDeque<u64>,
    pub should_quit: bool,

    pub mode: Mode,
    pub input: String,
    pub file_browser: FileBrowser,
    pub status_message: Option<(String, Instant)>,
}

impl App {
    pub fn new(client: DaemonClient) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self {
            client: Arc::new(client),
            torrents: Vec::new(),
            selected_torrent: None,
            list_state,
            stats: SessionStats::default(),
            active_tab: Tab::Torrents,
            details_tab: 0,
            download_history: VecDeque::with_capacity(60),
            upload_history: VecDeque::with_capacity(60),
            should_quit: false,
            mode: Mode::Main,
            input: String::new(),
            file_browser: FileBrowser::new(current_dir),
            status_message: None,
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    pub async fn refresh(&mut self) -> Result<()> {
        self.torrents = self.client.get_torrents().await.unwrap_or_default();
        self.stats = self.client.get_stats().await.unwrap_or_default();

        if let Some(index) = self.list_state.selected() {
            if let Some(summary) = self.torrents.get(index) {
                let id_hex = summary.id.to_hex();
                self.selected_torrent = self.client.get_torrent_details(&id_hex).await.ok();
            } else {
                self.selected_torrent = None;
            }
        } else {
            self.selected_torrent = None;
        }

        self.download_history
            .push_back(self.stats.total_download_rate);
        if self.download_history.len() > 60 {
            self.download_history.pop_front();
        }
        self.upload_history.push_back(self.stats.total_upload_rate);
        if self.upload_history.len() > 60 {
            self.upload_history.pop_front();
        }

        Ok(())
    }

    pub fn next(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= self.torrents.len().saturating_sub(1) {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.torrents.len().saturating_sub(1)
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn first(&mut self) {
        self.list_state.select(Some(0));
    }

    pub fn last(&mut self) {
        if !self.torrents.is_empty() {
            self.list_state.select(Some(self.torrents.len() - 1));
        }
    }
}
