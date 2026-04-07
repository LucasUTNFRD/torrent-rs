use std::{collections::BTreeMap, net::SocketAddr};

use bencode::{Bencode, BencodeDict};
use url::Url;

use super::{AnnounceData, AnnounceResponse, TrackerError};

#[derive(Clone)]
pub struct HttpClient(reqwest::Client);

impl HttpClient {
    pub fn new() -> Self {
        Self(
            reqwest::Client::builder()
                .gzip(true)
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("Failed to create HTTP client"),
        )
    }

    pub async fn announce(
        &self,
        url: &Url,
        data: AnnounceData,
    ) -> Result<AnnounceResponse, TrackerError> {
        let mut url = url.clone();

        {
            let mut query = url.query_pairs_mut();
            query.append_pair(
                "info_hash",
                &percent_encode_bytes(data.info_hash.as_slice()),
            );
            query.append_pair("peer_id", &percent_encode_bytes(data.peer_id.as_slice()));
            query.append_pair("port", &data.port.to_string());
            query.append_pair("uploaded", &data.uploaded.to_string());
            query.append_pair("downloaded", &data.downloaded.to_string());
            query.append_pair("left", &data.left.to_string());
            query.append_pair("compact", "1");

            if let Some(event) = data.event.as_str() {
                query.append_pair("event", event);
            }
        }

        tracing::debug!("Announcing to tracker URL: {}", url);
        let response = self.0.get(url).send().await?;
        let bytes = response.bytes().await?;
        self.parse_response(&bytes)
    }

    fn parse_response(&self, data: &[u8]) -> Result<AnnounceResponse, TrackerError> {
        let bencode = Bencode::decode(data).map_err(TrackerError::BencodeError)?;
        let dict = match bencode {
            Bencode::Dict(d) => d,
            _ => return Err(TrackerError::InvalidResponse("not a dict".into())),
        };

        if let Some(failure) = dict.get_str(b"failure reason") {
            return Err(TrackerError::TrackerError(failure.to_string()));
        }

        if let Some(warning) = dict.get_str(b"warning message") {
            tracing::warn!("Tracker warning: {}", warning);
        }

        let interval = dict.get_i64(b"interval").unwrap_or(20) as i32;
        let seeders = dict.get_i64(b"complete").unwrap_or(0) as i32;
        let leechers = dict.get_i64(b"incomplete").unwrap_or(0) as i32;
        let peers = parse_peers(&dict)?;

        Ok(AnnounceResponse {
            peers,
            interval,
            leechers,
            seeders,
        })
    }
}

fn percent_encode_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("%{:02X}", b)).collect()
}

fn parse_peers(dict: &BTreeMap<Vec<u8>, Bencode>) -> Result<Vec<SocketAddr>, TrackerError> {
    match dict.get(&b"peers"[..].to_vec()) {
        Some(Bencode::Bytes(bytes)) => parse_compact_peers(bytes),
        Some(Bencode::List(list)) => parse_dict_peers(list),
        _ => Err(TrackerError::InvalidResponse("missing peers".into())),
    }
}

fn parse_compact_peers(bytes: &[u8]) -> Result<Vec<SocketAddr>, TrackerError> {
    if !bytes.len().is_multiple_of(6) {
        return Err(TrackerError::InvalidResponse(
            "Invalid compact peers format".into(),
        ));
    }

    let mut peers = Vec::with_capacity(bytes.len() / 6);
    for chunk in bytes.chunks_exact(6) {
        let ip = std::net::Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
        let port = u16::from_be_bytes([chunk[4], chunk[5]]);
        peers.push(SocketAddr::new(std::net::IpAddr::V4(ip), port));
    }

    Ok(peers)
}

fn parse_dict_peers(list: &[Bencode]) -> Result<Vec<SocketAddr>, TrackerError> {
    list.iter()
        .map(|item| {
            let dict = match item {
                Bencode::Dict(d) => d,
                _ => return Err(TrackerError::InvalidResponse("peer not a dict".into())),
            };

            let ip = dict
                .get_str(b"ip")
                .ok_or_else(|| TrackerError::InvalidResponse("missing IP".into()))?
                .parse()
                .map_err(|_| TrackerError::InvalidResponse("invalid IP".into()))?;

            let port = dict
                .get_i64(b"port")
                .ok_or_else(|| TrackerError::InvalidResponse("missing port".into()))?
                as u16;

            Ok(SocketAddr::new(ip, port))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bittorrent_common::types::InfoHash;
    use std::net::Ipv4Addr;

    fn be_string(s: &str) -> String {
        format!("{}:{}", s.len(), s)
    }

    fn be_bytes_len(len: usize) -> String {
        format!("{}:", len)
    }

    fn make_compact_response(
        peers: &[(Ipv4Addr, u16)],
        interval: i64,
        complete: i64,
        incomplete: i64,
    ) -> Vec<u8> {
        let mut compact = Vec::with_capacity(peers.len() * 6);
        for (ip, port) in peers {
            let o = ip.octets();
            compact.extend_from_slice(&[o[0], o[1], o[2], o[3]]);
            compact.extend_from_slice(&port.to_be_bytes());
        }

        let header = format!(
            "d{}i{}e{}i{}e{}i{}e{}",
            be_string("complete"),
            complete,
            be_string("incomplete"),
            incomplete,
            be_string("interval"),
            interval,
            be_string("peers"),
        );

        let mut out = Vec::new();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(be_bytes_len(compact.len()).as_bytes());
        out.extend_from_slice(&compact);
        out.extend_from_slice(b"e");
        out
    }

    fn make_dict_response(
        peers: &[(Ipv4Addr, u16)],
        interval: i64,
        complete: i64,
        incomplete: i64,
    ) -> Vec<u8> {
        let mut s = format!(
            "d{}i{}e{}i{}e{}i{}e{}l",
            be_string("complete"),
            complete,
            be_string("incomplete"),
            incomplete,
            be_string("interval"),
            interval,
            be_string("peers")
        );

        for (ip, port) in peers {
            let ip_str = ip.to_string();
            s.push_str(&format!(
                "d{}{}{}i{}ee",
                be_string("ip"),
                be_string(&ip_str),
                be_string("port"),
                port
            ));
        }

        s.push_str("ee");
        s.into_bytes()
    }

    #[tokio::test]
    async fn test_compact_peers() {
        let client = HttpClient::new();
        let peers = &[
            (Ipv4Addr::new(1, 2, 3, 4), 6881),
            (Ipv4Addr::new(10, 20, 30, 40), 51413),
        ];
        let data = make_compact_response(peers, 1800, 5, 10);

        let resp = client.parse_response(&data).unwrap();
        assert_eq!(resp.interval, 1800);
        assert_eq!(resp.seeders, 5);
        assert_eq!(resp.leechers, 10);
        assert_eq!(resp.peers.len(), 2);
        assert_eq!(
            resp.peers[0],
            SocketAddr::new(Ipv4Addr::new(1, 2, 3, 4).into(), 6881)
        );
    }

    #[tokio::test]
    async fn test_dict_peers() {
        let client = HttpClient::new();
        let peers = &[(Ipv4Addr::new(192, 168, 1, 1), 6881)];
        let data = make_dict_response(peers, 900, 2, 3);

        let resp = client.parse_response(&data).unwrap();
        assert_eq!(resp.interval, 900);
        assert_eq!(resp.peers.len(), 1);
        assert_eq!(
            resp.peers[0],
            SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 6881)
        );
    }

    #[test]
    fn test_percent_encode_bytes() {
        let hash = InfoHash::new([
            0x12, 0x34, 0xAB, 0xCD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        let encoded = percent_encode_bytes(hash.as_slice());
        assert_eq!(
            encoded,
            "%12%34%AB%CD%00%00%00%00%00%00%00%00%00%00%00%00%00%00%00%00"
        );
    }
}
