use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

use bencode::bencode::Bencode;

use super::{TrackerClient, TrackerResponse, error::TrackerError};

use url::Url;

pub struct QueryParamsBuilder<'a> {
    url: Url,
    params: &'a super::AnnounceParams,
}

impl<'a> QueryParamsBuilder<'a> {
    pub fn new(url: Url, params: &'a super::AnnounceParams) -> Self {
        Self { url, params }
    }

    pub fn build(mut self) -> Url {
        let mut query_pairs = vec![
            ("info_hash", self.params.info_hash.as_bytes().to_vec()),
            ("peer_id", self.params.peer_id.as_bytes().to_vec()),
            ("port", self.params.port.to_string().into_bytes()),
            ("uploaded", self.params.uploaded.to_string().into_bytes()),
            (
                "downloaded",
                self.params.downloaded.to_string().into_bytes(),
            ),
            ("left", self.params.left.to_string().into_bytes()),
            ("compact", b"1".to_vec()),
        ];

        if let Some(event_str) = self.params.event.to_string() {
            query_pairs.push(("event", event_str.into_bytes()));
        }

        // Build the query string manually
        let mut query_string = String::new();
        for (key, value) in &query_pairs {
            if !query_string.is_empty() {
                query_string.push('&');
            }
            query_string.push_str(key);
            query_string.push('=');

            if *key == "info_hash" || *key == "peer_id" {
                for &byte in value {
                    query_string.push('%');
                    query_string.push_str(&format!("{:02X}", byte));
                }
            } else {
                query_string
                    .push_str(&url::form_urlencoded::byte_serialize(value).collect::<String>());
            }
        }

        // Apply the encoded query string
        self.url.set_query(Some(&query_string));
        self.url
    }
}

#[derive(Clone)]
pub struct HttpTrackerClient {
    client: reqwest::Client,
}

#[async_trait::async_trait]
impl TrackerClient for HttpTrackerClient {
    async fn announce(
        &self,
        params: &super::AnnounceParams,
        tracker_url: url::Url,
    ) -> Result<TrackerResponse, TrackerError> {
        let tracker_url = QueryParamsBuilder::new(tracker_url, params).build();
        println!("{tracker_url}");
        let response = self.client.get(tracker_url).send().await?;
        let bytes = response.bytes().await?;
        self.parse_announce_response(&bytes).await
    }
}

impl HttpTrackerClient {
    pub fn new() -> Result<Self, TrackerError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("Failed to create HTTP client"),
        })
    }

    async fn parse_announce_response(&self, data: &[u8]) -> Result<TrackerResponse, TrackerError> {
        let response_bencode = Bencode::decode(data).map_err(TrackerError::BencodeError)?;
        let dict = match response_bencode {
            Bencode::Dict(dict) => dict,
            _ => {
                return Err(TrackerError::InvalidResponse(
                    "Info must be a dictionary".to_string(),
                ));
            }
        };

        //Error Checking
        if let Some(failure_reason) = dict.get_str(KEY_FAILURE_REASON) {
            return Err(TrackerError::TrackerError(failure_reason.to_string()));
        }
        if let Some(warning_message) = dict.get_str(KEY_WARNING_MESSAGE) {
            tracing::warn!(warning_message)
        }

        // Extract interval
        const DEFAULT_ANNOUNCE_INTERVAL: i64 = 20;
        let interval = dict
            .get_i64(b"interval")
            .unwrap_or(DEFAULT_ANNOUNCE_INTERVAL) as i32;

        // // Extract min interval (optional)
        // let min_interval = get_optional_string_from_dict(&dict, KEY_MIN_INTERVAL);
        //
        // // Extract tracker id (optional)
        // let tracker_id = get_optional_string_from_dict(&dict, KEY_TRACKER_ID);
        //
        // // Extract complete and incomplete peers
        // let complete: u32 =
        //     get_optional_int_from_dict(&dict, b"complete").unwrap_or_default() as u32;
        //
        // let incomplete =
        //     get_optional_int_from_dict(&dict, b"incomplete").unwrap_or_default() as u32;

        // Parse peers - could be dictionary model or binary model
        let peers = parse_peers(&dict)?;

        Ok(TrackerResponse {
            peers,
            interval,
            leechers: 0,
            seeders: 0,
        })
    }
}

fn parse_peers(dict: &BTreeMap<Vec<u8>, Bencode>) -> Result<Vec<SocketAddr>, TrackerError> {
    match dict.get(KEY_PEERS) {
        Some(Bencode::Bytes(bytes)) => {
            if bytes.len() % 6 != 0 {
                return Err(TrackerError::InvalidResponse(
                    "Invalid compact peers format".to_string(),
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
        Some(Bencode::List(peer_list)) => peer_list
            .iter()
            .map(|item| {
                let dict = match item {
                    Bencode::Dict(d) => d,
                    _ => {
                        return Err(TrackerError::InvalidResponse(
                            "Expected dict in peer list".into(),
                        ));
                    }
                };

                let ip = dict
                    .get_str(KEY_IP)
                    .ok_or_else(|| TrackerError::InvalidResponse("Missing or invalid IP".into()))?
                    .parse()
                    .map_err(|_| TrackerError::InvalidResponse("Invalid IP format".into()))?;

                let port: u16 = dict.get_i64(KEY_PORT).ok_or_else(|| {
                    TrackerError::InvalidResponse("Missing or invalid port".into())
                })? as u16;

                Ok(SocketAddr::new(ip, port))
            })
            .collect(),

        _ => Err(TrackerError::InvalidResponse(
            "missing peers field".to_string(),
        )),
    }
}

pub trait BencodeDictExt {
    fn get_bytes(&self, key: &[u8]) -> Option<&[u8]>;
    fn get_str(&self, key: &[u8]) -> Option<&str>;
    fn get_i64(&self, key: &[u8]) -> Option<i64>;
    fn get_list(&self, key: &[u8]) -> Option<&[Bencode]>;
    fn get_dict(&self, key: &[u8]) -> Option<&BTreeMap<Vec<u8>, Bencode>>;
}

impl BencodeDictExt for BTreeMap<Vec<u8>, Bencode> {
    fn get_bytes(&self, key: &[u8]) -> Option<&[u8]> {
        match self.get(key) {
            Some(Bencode::Bytes(b)) => Some(b.as_slice()),
            _ => None,
        }
    }

    fn get_str(&self, key: &[u8]) -> Option<&str> {
        self.get_bytes(key)
            .and_then(|b| std::str::from_utf8(b).ok())
    }

    fn get_i64(&self, key: &[u8]) -> Option<i64> {
        match self.get(key) {
            Some(Bencode::Int(i)) => Some(*i),
            _ => None,
        }
    }

    fn get_list(&self, key: &[u8]) -> Option<&[Bencode]> {
        match self.get(key) {
            Some(Bencode::List(l)) => Some(l.as_slice()),
            _ => None,
        }
    }

    fn get_dict(&self, key: &[u8]) -> Option<&BTreeMap<Vec<u8>, Bencode>> {
        match self.get(key) {
            Some(Bencode::Dict(d)) => Some(d),
            _ => None,
        }
    }
}

const KEY_FAILURE_REASON: &[u8] = b"failure reason";
const KEY_WARNING_MESSAGE: &[u8] = b"warning message";
const KEY_PORT: &[u8] = b"port";
const KEY_IP: &[u8] = b"IP";
const KEY_PEERS: &[u8] = b"peers";
const KEY_MIN_INTERVAL: &[u8] = b"min interval";
const KEY_TRACKER_ID: &[u8] = b"tracker id";
