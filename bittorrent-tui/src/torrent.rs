use anyhow::Result;
use bittorrent_core::types::{SessionStats, TorrentDetails, TorrentSummary};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct AddTorrentRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

pub struct DaemonClient {
    pub client: Client,
    pub base_url: String,
}

impl DaemonClient {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
        }
    }

    pub async fn get_torrents(&self) -> Result<Vec<TorrentSummary>> {
        let url = format!("{}/torrents", self.base_url);
        let res = self.client.get(url).send().await?.json().await?;
        Ok(res)
    }

    pub async fn get_torrent_details(&self, id: &str) -> Result<TorrentDetails> {
        let url = format!("{}/torrents/{}", self.base_url, id);
        let res = self.client.get(url).send().await?.json().await?;
        Ok(res)
    }

    pub async fn get_stats(&self) -> Result<SessionStats> {
        let url = format!("{}/stats", self.base_url);
        let res = self.client.get(url).send().await?.json().await?;
        Ok(res)
    }

    pub async fn add_torrent(&self, path: Option<String>, uri: Option<String>) -> Result<()> {
        let url = format!("{}/torrents", self.base_url);
        let req = AddTorrentRequest { path, uri };
        let res = self.client.post(url).json(&req).send().await?;
        if res.status().is_success() {
            Ok(())
        } else {
            let err: serde_json::Value = res.json().await?;
            anyhow::bail!("API Error: {}", err["error"])
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;

    if bytes < KB {
        format!("{} B", bytes)
    } else if bytes < MB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes < TB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    }
}

pub fn format_bytes_per_second(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}
