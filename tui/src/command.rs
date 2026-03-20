use bittorrent_core::types::TorrentId;

pub struct AddTorrentCommand {
    pub value: String,
}

pub struct RemoveTorrentCommand {
    pub id: TorrentId,
}

pub struct OpenAddTorrentCommand;

pub struct OpenRemoveTorrentCommand {
    pub id: TorrentId,
}
