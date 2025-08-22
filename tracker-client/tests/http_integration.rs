use std::net::Ipv4Addr;

use bittorrent_common::{metainfo::parse_torrent_from_file, types::PeerID};
use tracker_client::{AnnounceParams, Events, HttpTrackerClient, TrackerClient};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn be_string(s: &str) -> String {
    format!("{}:{}", s.len(), s)
}

fn be_bytes_len(len: usize) -> String {
    format!("{}:", len)
}

fn bencode_http_compact_response(
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

#[tokio::test]
async fn http_integration_compact_peers() {
    // Start mock HTTP server
    let server = MockServer::start().await;

    let peers = &[
        (Ipv4Addr::new(1, 2, 3, 4), 6881),
        (Ipv4Addr::new(5, 6, 7, 8), 51413),
    ];
    let body = bencode_http_compact_response(peers, 1800, 2, 4);

    Mock::given(method("GET"))
        .and(path("/announce"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(&server)
        .await;

    // Build params
    let torrent =
        parse_torrent_from_file("../sample_torrents/sample.torrent").expect("parse torrent");
    let params = AnnounceParams {
        info_hash: torrent.info_hash,
        peer_id: PeerID::new([0u8; 20]),
        port: 6881,
        uploaded: 0,
        downloaded: 0,
        left: torrent.total_size(),
        event: Events::Started,
    };

    let url = Url::parse(&format!("{}/announce", &server.uri())).unwrap();
    let client = HttpTrackerClient::new().unwrap();

    let resp = client.announce(&params, url).await.unwrap();
    assert_eq!(resp.interval, 1800);
    assert_eq!(resp.seeders, 2);
    assert_eq!(resp.leechers, 4);
    assert_eq!(resp.peers.len(), 2);
}
