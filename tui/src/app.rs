use bittorrent_core::{
    FileInfo, PeerSnapshot, Session, TorrentMeta, TorrentProgress, TrackerStatus, types::TorrentId,
};
use ratatui::widgets::TableState;
use std::{collections::HashMap, fmt::Display};
use tokio::sync::watch;

pub enum Status {
    None,
    Info(String),
    Error(String),
}

pub enum View {
    List,
    Detail { id: TorrentId, tab: DetailTab },
}

#[derive(Clone, Copy, PartialEq)]
pub enum DetailTab {
    Overview = 0,
    Files = 1,
    Peers = 2,
    Trackers = 3,
}

impl DetailTab {
    pub fn next(self) -> Self {
        match self {
            Self::Overview => Self::Files,
            Self::Files => Self::Peers,
            Self::Peers => Self::Trackers,
            Self::Trackers => Self::Overview,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Self::Overview => Self::Trackers,
            Self::Files => Self::Overview,
            Self::Peers => Self::Files,
            Self::Trackers => Self::Peers,
        }
    }
    pub fn titles() -> Vec<&'static str> {
        vec!["Overview", "Files", "Peers", "Trackers"]
    }
}

pub struct DetailCache {
    pub meta: Option<TorrentMeta>,
    pub files: Vec<FileInfo>,
    pub peers: Vec<PeerSnapshot>,
    pub trackers: Vec<TrackerStatus>,
    pub files_table_state: std::cell::Cell<TableState>,
    pub peers_table_state: std::cell::Cell<TableState>,
}

impl Default for DetailCache {
    fn default() -> Self {
        Self {
            meta: None,
            files: Vec::new(),
            peers: Vec::new(),
            trackers: Vec::new(),
            files_table_state: std::cell::Cell::new(TableState::default()),
            peers_table_state: std::cell::Cell::new(TableState::default()),
        }
    }
}

pub struct App {
    pub session: Session,
    receivers: HashMap<TorrentId, watch::Receiver<TorrentProgress>>,
    pub progress: HashMap<TorrentId, TorrentProgress>,
    pub torrent_order: Vec<TorrentId>,
    pub selected_index: usize,
    pub should_quit: bool,
    pub status: Status,
    pub view: View,
    pub detail: DetailCache,
    pub detail_tick: u64,
}

impl App {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            receivers: HashMap::new(),
            progress: HashMap::new(),
            torrent_order: Vec::new(),
            selected_index: 0,
            should_quit: false,
            status: Status::None,
            view: View::List,
            detail: DetailCache::default(),
            detail_tick: 0,
        }
    }

    pub async fn tick(&mut self) {
        for (id, rx) in &mut self.receivers {
            if rx.has_changed().unwrap_or(false)
                && let Some(snap) = self.progress.get_mut(id)
            {
                *snap = rx.borrow_and_update().clone();
            }
        }

        // TODO: Refresh peer/tracker data periodically when those APIs are implemented
        if let View::Detail { id, tab } = self.view {
            self.detail_tick += 1;
            if self.detail_tick % 5 == 0 {
                match tab {
                    DetailTab::Peers => {
                        if let Ok(p) = self.session.get_peers(id).await {
                            self.detail.peers = p;
                        }
                    }
                    DetailTab::Trackers => {
                        if let Ok(t) = self.session.get_trackers(id).await {
                            self.detail.trackers = t;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub const fn quit(&mut self) {
        self.should_quit = true;
    }

    pub async fn add_torrent_to_state(&mut self, id: TorrentId) -> anyhow::Result<()> {
        if !self.receivers.contains_key(&id) {
            let rx = self.session.subscribe_torrent(id).await?;
            self.progress.insert(id, rx.borrow().clone());
            self.receivers.insert(id, rx);
            self.torrent_order.push(id);
        }
        Ok(())
    }

    pub fn remove_torrent_from_state(&mut self, id: TorrentId) {
        self.receivers.remove(&id);
        self.progress.remove(&id);
        self.torrent_order.retain(|&x| x != id);
        self.selected_index = self
            .selected_index
            .min(self.torrent_order.len().saturating_sub(1));
    }

    pub fn selected_id(&self) -> Option<TorrentId> {
        self.torrent_order.get(self.selected_index).copied()
    }

    pub fn set_error(&mut self, e: impl Display) {
        self.status = Status::Error(e.to_string());
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Status::Info(msg.into());
    }

    pub fn clear_status(&mut self) {
        self.status = Status::None;
    }

    pub async fn open_detail(&mut self, id: TorrentId) -> anyhow::Result<()> {
        let detail = self.session.get_torrent_meta(id).await?;
        self.detail = DetailCache {
            meta: Some(detail.meta),
            files: detail.files,
            peers: detail.peers,
            trackers: detail.trackers,
            files_table_state: std::cell::Cell::new(TableState::default()),
            peers_table_state: std::cell::Cell::new(TableState::default()),
        };
        self.view = View::Detail {
            id,
            tab: DetailTab::Overview,
        };
        Ok(())
    }

    pub fn close_detail(&mut self) {
        self.view = View::List;
        self.detail = DetailCache::default();
    }
}
