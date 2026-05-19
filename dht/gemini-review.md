# DHT Implementation Audit Report (BEP 0005 & BEP 0032)

This report assesses the correctness and accuracy of the DHT implementation against BitTorrent Enhancement Proposals (BEPs) 0005 (Mainline DHT) and 0032 (IPv6 Extension).

## Executive Summary
The implementation establishes a solid foundation for the KRPC protocol and dual-stack DHT support. However, it contains several critical discrepancies and incomplete features that affect its compliance with the specifications and its reliability in a real-world DHT network. Most notably, iterative search logic is flawed, routing table maintenance is incomplete, and IPv6 message encoding lacks the required field separation.

---

## 1. Node ID and Distance Metric (BEP 0005)
**Status: ✅ CORRECT**

- **Node ID**: Correctly implemented as a 160-bit identifier (`src/node_id.rs`).
- **XOR Metric**: The distance calculation and the `distance_exp` mapping for k-bucket indexing are correctly implemented.
- **Dual-Stack**: Correctly uses the same Node ID for both IPv4 and IPv6 instances, as recommended in BEP 0032.

## 2. Routing Table and K-Buckets (BEP 0005)
**Status: ⚠️ INCOMPLETE / PARTIAL DISCREPANCIES**

- **Bucket Splitting**: The implementation uses a fixed array of 160 buckets (`src/routing_table.rs`). BEP 0005 specifies a dynamic splitting logic starting from a single bucket covering the entire range. While a fixed-bucket approach covers the space, it deviates from the standard procedural rules.
- **Node Health Tracking**:
    - The `Good`, `Questionable`, and `Bad` states are defined but not fully implemented.
    - Missing 15-minute rules for "Good" node status.
    - **CRITICAL**: Query timeouts in `src/dht.rs` are not reported back to the `RoutingTable`, meaning `consecutive_failures` never increment. This prevents "Bad" nodes from ever being removed or replaced.
- **Bucket Refresh**: There is no background task to refresh buckets that haven't seen activity in 15 minutes, as required by the specification.
- **Replacement Cache**: There is a `replacement_candidates` mechanism, but the logic to promote them when a bucket node becomes "Bad" is incomplete.

## 3. KRPC Messages and Bencoding (BEP 0005 & BEP 0032)
**Status: ❌ CRITICAL DISCREPANCIES**

- **IPv6 Field Separation (BEP 0032)**:
    - **MAJOR BUG**: The implementation fails to use the `nodes6` and `values6` fields for IPv6 node/peer contact information. It attempts to pack them into the standard `nodes` and `values` fields. This will cause decoding failures in other compliant clients.
    - **Compact Formats**: While IPv4 compact info (26 bytes for nodes, 6 bytes for peers) is correct, the 38-byte IPv6 node info and 18-byte peer info are not correctly handled during serialization.
- **The `want` Parameter**: Correctly implemented as a list of strings (`n4`, `n6`).
- **MTU Limits**: The implementation does not enforce the 1024-octet MTU limit for IPv6 UDP payloads recommended by BEP 0032.
- **Transaction IDs**: Correctly handled as opaque strings.

## 4. Token Management (BEP 0005)
**Status: ⚠️ MINOR DISCREPANCIES**

- **Secret Rotation**:
    - The rotation interval is 15 minutes, whereas BEP 0005 recommends 5 minutes (with tokens valid for 10).
    - **Logic Flaw**: `check_rotation` is only triggered on `AnnouncePeer` queries. If a node only receives `GetPeers` queries, its secrets will never rotate, potentially allowing tokens to remain valid indefinitely.
- **IP Binding**: Tokens are correctly hashed with the sender's IP address and infohash, preventing announcement spoofing.

## 5. Iterative Search and Protocol Flow (BEP 0005)
**Status: ❌ CRITICAL BUG**

- **AnnouncePeer Sender ID**: In `src/dht.rs` (`tasks::Search::run`), the `AnnouncePeer` query incorrectly uses the *target* node's ID as the `id` field of the query. It must use `self.our_node_id`. This will cause remote nodes to reject the announcement or associate it with the wrong ID.
- **Peer Storage**: `src/peer_store.rs` lacks an expiration mechanism. Peers should expire after ~30 minutes. The current implementation will grow indefinitely in memory.
- **Recursive Logic**: The iterative search for `find_node` and `get_peers` follows the general "find the K closest nodes" approach but could be more robust in handling late-arriving responses.

---

## Conclusion and Recommendations

To achieve full specification correctness, the following actions are required:

- [x]  **Fix IPv6 Encoding**: Update `src/message.rs` to use `nodes6` and `values6` for IPv6 addresses and implement the 38-byte compact format.
- [x]  **Fix AnnouncePeer ID**: Update `tasks::Search::run` in `src/dht.rs` to use the correct sender Node ID.
- [ ]  **Implement Health Propagation**: Ensure query timeouts are reported to the `RoutingTable` to allow removal of dead nodes.
- [ ]  **Enforce MTU**: Add checks to ensure outgoing UDP packets stay within the 1024-octet limit for IPv6.
- [ ]  **Add Peer Expiration**: Implement a background cleanup task for `PeerStore`.
- [ ]  **Align Token Rotation**: Change the rotation interval to 5 minutes and trigger it on both `GetPeers` and `AnnouncePeer` requests.
