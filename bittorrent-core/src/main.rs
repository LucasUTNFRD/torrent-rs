use bittorrent_core::torrent::metainfo::parse_torrent_from_file;

fn main() {
    let file = "../sample_torrents/big-buck-bunny.torrent";
    println!("reading from {file}");
    let torrent = parse_torrent_from_file(file).unwrap();

    println!("torrent file: {torrent:?}");
    println!("info_hash:{:?}", hex::encode(torrent.info_hash));

    println!("Hello World!");
}
