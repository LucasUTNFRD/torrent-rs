use std::net::{Ipv4Addr, SocketAddr};

use bittorrent_common::{metainfo::parse_torrent_from_file, types::PeerID};
use tokio::net::UdpSocket;
use tracker_client::{AnnounceParams, Events, TrackerClient, UdpTrackerClient, Url};

// TODO: Test Re-Announces
// TODO: Test time-outs : This means it doesn't retransmit lost packets itself. The application is responsible for this. If a response is not received after 15 * 2 ^ n seconds, the client should retransmit the request, where n starts at 0 and is increased up to 8 (3840 seconds) after every retransmission. Note that it is necessary to rerequest a connection ID when it has expired.
// TODO: Test expired connections_id

async fn spawn_mock_udp_tracker() -> (Url, tokio::task::JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let url = Url::parse(&format!("udp://{}:{}/announce", addr.ip(), addr.port())).unwrap();

    let handle = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        let connection_id: i64 = 0x0102030405060708;

        loop {
            let (len, src) = socket.recv_from(&mut buf).await.unwrap();
            if len < 16 {
                continue;
            }

            // Connect request: [0..8]=protocol id, [8..12]=action=0, [12..16]=tx_id
            let action = i32::from_be_bytes(buf[8..12].try_into().unwrap());
            if action == 0 {
                let tx_id = i32::from_be_bytes(buf[12..16].try_into().unwrap());
                let mut resp = [0u8; 16];
                resp[0..4].copy_from_slice(&0i32.to_be_bytes()); // action=0 (connect)
                resp[4..8].copy_from_slice(&tx_id.to_be_bytes());
                resp[8..16].copy_from_slice(&connection_id.to_be_bytes());
                socket.send_to(&resp, src).await.unwrap();
                continue;
            }

            // Announce request:
            // [0..8]=connection_id, [8..12]=action=1, [12..16]=tx_id, ...
            if action == 1 {
                let tx_id = i32::from_be_bytes(buf[12..16].try_into().unwrap());
                let interval = 1800i32;
                let leechers = 4i32;
                let seeders = 2i32;

                // compact peers (two entries)
                let mut peers = Vec::new();
                peers.extend_from_slice(&[1, 2, 3, 4]);
                peers.extend_from_slice(&6881u16.to_be_bytes());
                peers.extend_from_slice(&[5, 6, 7, 8]);
                peers.extend_from_slice(&51413u16.to_be_bytes());

                let mut resp = Vec::with_capacity(20 + peers.len());
                resp.extend_from_slice(&1i32.to_be_bytes()); // action=1 (announce)
                resp.extend_from_slice(&tx_id.to_be_bytes());
                resp.extend_from_slice(&interval.to_be_bytes());
                resp.extend_from_slice(&leechers.to_be_bytes());
                resp.extend_from_slice(&seeders.to_be_bytes());
                resp.extend_from_slice(&peers);

                socket.send_to(&resp, src).await.unwrap();
                break;
            }
        }
    });

    (url, handle)
}

#[tokio::test]
async fn udp_integration_basic_announce() {
    let (url, _handle) = spawn_mock_udp_tracker().await;

    // Use local sample torrent to obtain a valid info_hash/left (deterministic)
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

    let client = UdpTrackerClient::new().await.unwrap();
    let resp = client.announce(&params, url).await.unwrap();

    assert_eq!(resp.interval, 1800);
    assert_eq!(resp.seeders, 2);
    assert_eq!(resp.leechers, 4);
    assert_eq!(resp.peers.len(), 2);
    assert_eq!(
        resp.peers[0],
        SocketAddr::new(Ipv4Addr::new(1, 2, 3, 4).into(), 6881)
    );
    assert_eq!(
        resp.peers[1],
        SocketAddr::new(Ipv4Addr::new(5, 6, 7, 8).into(), 51413)
    );
}
