the optimistic_unchoke simulation in libtorrent is a highly specialized test designed to verify the
  fairness and rotation of the unchoking algorithm.


  In a real BitTorrent swarm, a node has a limited number of "unchoke slots" (peers it sends data to).
  Most slots go to peers that provide the best download rates (tit-for-tat), but one or more slots are
  reserved for Optimistic Unchoking—randomly picking a peer to see if they can provide better rates.


  1. Test Configuration
  The test sets up a very specific environment to isolate the optimistic unchoke behavior:
   * Slots: unchoke_slots_limit is set to 1 and num_optimistic_unchoke_slots is set to 1. This means
     there is only an optimistic slot and no regular tit-for-tat slots.
   * Swarm: 20 simulated peers are connected to one central downloader.
   * Peer State: All 20 peers are in idle mode. They don't send any data, so the downloader has no
     "best" peers to prefer. This forces the downloader to rely entirely on the optimistic rotation
     logic.
   * Duration: The test runs for a long period (num_nodes * 90 seconds), ensuring every peer has
     multiple opportunities to be picked.


  2. How the Simulation Works
  The test uses a custom peer_conn (from bittorrent_peer.hpp) to act as a "fake" peer.
   * Event Monitoring: The test maintains a choke_state for every peer. It tracks when a peer receives a
     choke (msg 0) or unchoke (msg 1) message from the library.
   * Timing: When a peer is unchoked, the test records the last_unchoke timestamp. When it is choked
     again, it adds the elapsed time to unchoke_duration.
   * Networking: Each of the 20 peers is assigned a unique virtual IP address (e.g., 50.0.0.1, 50.0.0.2,
     etc.) using the simulator's virtual network stack.


  3. Success Criteria (Verification)
  The core of the test is a statistical check on the "fairness" of the rotation:
   * Expected Time: If the rotation is perfectly fair, each of the 20 peers should spend exactly 1/20th
     of the total test duration being unchoked.
   1     std::int64_t const average_unchoke_time = duration_ms / num_nodes;
   * Tolerance: The test loops through every peer and checks if their actual unchoke_duration is close
     to the average_unchoke_time.
   1     TEST_CHECK(std::abs(unchoke_duration - average_unchoke_time) < 1500);
      The 1500ms tolerance allows for minor timing jitter in the simulation's event loop but ensures
  that no peer was ignored or given unfair preference.


  Summary of the Simulation "Trick"
  The brilliance of this test is that it removes the "noise" of the network. By making all peers "bad"
  (idle), it forces the library's unchoking algorithm into its fallback state: pure, fair rotation. If
  the library's logic is correct, the resulting distribution of unchoke time across the 20 peers will be
  uniform.


  Replicating this in Turmoil
  To do this in Turmoil:
   1. Mock Peer: Create a Turmoil host that implements a minimal BitTorrent protocol. It should just
      listen for Choke/Unchoke messages and record the time.
   2. Simulation Controller: Use turmoil::Sim to run 20 of these hosts.
   3. Deterministic Clock: Because Turmoil is deterministic, you can use tokio::time::Instant and
      Duration to calculate the unchoke times. They will be perfectly consistent across test runs.
   4. Assertion: At the end of the sim.run(), collect the timing data from all 20 hosts and assert that
      they are within a small standard deviation of each other.

Consideration on MockPeer: there is in peer-protocol a Codec implementation to wrap a TcpStream and send bittorent messages. 
