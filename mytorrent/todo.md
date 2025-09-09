 mytorrent git:(feat/listen-incoming-connection) ✗ transmission-cli -w $(pwd) test.torrent // runs the tranmssion as a seeder
 cargo run -- --log-level=debug mytorrent/test.torrent // runs our client to leech the whole file

// with this we could experiment we more detailed swarm
// bittorrent-tracker --http --udp --port 8000 -- this for starting a local tracker
