# BitTorrent Mainline DHT - Design Analysis & Rust Port Planning

## Executive Summary

This document provides a comprehensive analysis of the BitTorrent Mainline DHT implementation for the purpose of porting it to Rust using an **Actor Model** architecture with **Stream-Based APIs**. This approach offers superior composability, type safety, and integration with modern async Rust applications compared to the original C callback-based design.

---

## Repository Overview

### What is this?

This is a **production-quality C implementation** of the Kademlia Distributed Hash Table (DHT) used in the BitTorrent network, authored by Juliusz Chroboczek. It implements the "mainline" DHT variant used by major BitTorrent clients.

### Key Specifications

- **Protocol**: Kademlia DHT (BitTorrent "mainline" variant per BEP-5)
- **Language**: C99
- **License**: MIT
- **Design**: Single-threaded, event-driven, callback-based
- **Network**: Dual-stack IPv4/IPv6 support
- **Platform**: Cross-platform (Unix/Linux, Windows)

### Core Files

| File | Purpose | Lines |
|------|---------|-------|
| `dht.c` | Main implementation | ~3000 |
| `dht.h` | Public API header | ~70 |
| `dht-example.c` | Example application | ~526 |
| `README` | API documentation | ~206 |

---

## Architecture Deep Dive

### Original C Design Philosophy

The C implementation uses **cooperative multitasking** with a poll-based API:

```
Application Event Loop
         │
         ▼
┌─────────────────┐
│  select/poll    │◄───── Wait for network I/O or timeout
└────────┬────────┘
         │
    ┌────┴────┐
    ▼         ▼
Packet    Timeout
│         │
▼         ▼
dht_periodic(buf, NULL, &tosleep, callback, closure)
         │
         ▼
    Callback invoked
    (DHT_EVENT_VALUES,
     DHT_EVENT_SEARCH_DONE)
```

**Key characteristics:**
- **No internal threading** - caller drives execution
- **Returns `tosleep`** - indicates next wake time
- **Callback-driven** - async events via function pointers
- **Socket abstraction** - `dht_sendto()` callback for I/O

### Core Data Structures

#### 1. Node (Peer Information)

```c
struct node {
    unsigned char id[20];           // 160-bit node ID (SHA1-like)
    struct sockaddr_storage ss;     // Network address (IPv4/IPv6)
    int sslen;                      // Address length
    time_t time;                    // Last message received (15 min timeout)
    time_t reply_time;              // Last correct reply (2 hour timeout)
    time_t pinged_time;             // Last request sent
    int pinged;                     // Unanswered ping count (evict at 3+)
    struct node *next;              // Linked list
};
```

**Node Lifecycle:**
```
Discovered ──▶ Questionable ──▶ Verified ──▶ Bad
   │              │              │            │
   ▼              ▼              ▼            ▼
Added on     1-2 unanswered   pinged=0    pinged>=3
receipt      pings            reply_time  eviction
                              recent      candidate
```

**Goodness Criteria:**
```c
node_is_good(node) = 
    node->pinged <= 2 &&                          // Max 2 unanswered
    node->reply_time >= now - 7200 &&             // Reply within 2h
    node->time >= now - 900                       // Activity within 15m
```

#### 2. K-Bucket (Routing Table Entry)

```c
struct bucket {
    int af;                         // Address family
    unsigned char first[20];        // Lower bound of ID range
    int count;                      // Current nodes
    int max_count;                  // Max (adaptive, min 8)
    time_t time;                    // Last reply in bucket
    struct node *nodes;             // Linked list
    struct sockaddr_storage cached; // Replacement candidate
    struct bucket *next;
};
```

**Bucket Splitting Algorithm:**
```
Bucket covers ID range [first, next->first)
When full AND contains own ID:
    1. Calculate middle ID
    2. Create new bucket at middle
    3. Redistribute nodes
    4. Adjust max_count (parent/2, min 8)
```

**Routing Table Structure:**
```
160-bit ID Space
├─ Bucket 0: [0x0000...0000, 0x4000...0000)  ← Far nodes
├─ Bucket 1: [0x4000...0000, 0x6000...0000)
│   ...
├─ Bucket N: [0xFFFF...FF80, 0xFFFF...FFFF]  ← Close nodes
│   └─ Split when full
└─ Separate trees for IPv4 and IPv6
```

#### 3. Search State

```c
struct search {
    unsigned short tid;             // Transaction ID
    int af;                         // Address family
    time_t step_time;               // Last progress
    unsigned char id[20];           // Target infohash
    unsigned short port;            // Port to announce (0=lookup)
    int done;                       // Completion flag
    struct search_node nodes[14];   // Candidate nodes
    int numnodes;
};

struct search_node {
    unsigned char id[20];
    struct sockaddr_storage ss;
    time_t request_time;            // Query sent
    time_t reply_time;              // Reply received
    int pinged;                     // Attempts
    unsigned char token[40];        // For announce
    int replied;                    // Got reply?
    int acked;                      // Announce acked?
};
```

**Search State Machine:**
```
                    ┌──────────────────┐
                    │      INIT        │
                    └────────┬─────────┘
                             │
                    Seed with closest
                    from routing table
                             │
                             ▼
                    ┌──────────────────┐
                    │    QUERYING      │
                    └────────┬─────────┘
                             │
              Send get_peers to closest 8
              (max 4 concurrent)
                             │
                             ▼
                    ┌──────────────────┐
              ┌────▶│  GOT RESPONSE    │
              │     └────────┬─────────┘
              │              │
              │     Add returned nodes
              │     Continue querying
              │              │
              │              ▼
              │     ┌──────────────────┐
              └─────│  First 8 replied?│─NO──┐
                    └────────┬─────────┘     │
                           YES              │
                             │               │
              ┌──────────────┴───────────────┘
              │
              ▼
     ┌──────────────────┐
     │  port == 0 ?     │
     └────────┬─────────┘
           YES│    NO│
              ▼      ▼
     ┌─────────┐  ┌──────────────┐
     │  DONE   │  │ ANNOUNCING  │
     └─────────┘  └──────┬───────┘
                         │
              Send announces
              to closest 8
                         │
                         ▼
                ┌──────────────┐
                │    DONE      │
                └──────────────┘
```

---

## Protocol Messages (Bencode)

### Message Format

All DHT messages use BitTorrent bencoding:

```
Strings:    <length>:<content>          e.g., "4:ping"
Integers:   i<number>e                  e.g., "i42e"
Lists:      l<content>e                  e.g., "li1ei2ee"
Dictionaries: d<key><value>...e
```

### Message Types

#### Ping (Connectivity Check)

**Query:**
```bencode
d
  1:a d 2:id 20:<20-byte sender id> e
  1:q 4:ping
  1:t 2:<tid>
  1:y 1:q
e
```

**Response:**
```bencode
d
  1:r d 2:id 20:<20-byte responder id> e
  1:t 2:<tid>
  1:y 1:r
e
```

#### Find Node (Routing Table Query)

**Query:**
```bencode
d
  1:a d
    2:id 20:<sender id>
    6:target 20:<target id>
    4:want l 2:n4 2:n6 e  (optional)
  e
  1:q 9:find_node
  1:t 2:<tid>
  1:y 1:q
e
```

**Response:**
```bencode
d
  1:r d
    2:id 20:<responder id>
    5:nodes <26*N bytes>  (20-byte ID + 4-byte IP + 2-byte port)
    6:nodes6 <38*N bytes> (20-byte ID + 16-byte IP + 2-byte port)
  e
  1:t 2:<tid>
  1:y 1:r
e
```

#### Get Peers (Torrent Peer Discovery)

**Query:**
```bencode
d
  1:a d
    2:id 20:<sender id>
    9:info_hash 20:<torrent infohash>
  e
  1:q 9:get_peers
  1:t 2:<tid>
  1:y 1:q
e
```

**Response (peers found):**
```bencode
d
  1:r d
    2:id 20:<responder id>
    6:values l <6-byte peer>* e  (IPv4: 4-byte IP + 2-byte port)
    7:values6 l <18-byte peer>* e (IPv6: 16-byte IP + 2-byte port)
    5:token 8:<token>
  e
  1:t 2:<tid>
  1:y 1:r
e
```

**Response (no peers):** Returns closest nodes like find_node + token

#### Announce Peer (Register as Seeder)

**Query:**
```bencode
d
  1:a d
    2:id 20:<sender id>
    9:info_hash 20:<torrent infohash>
    4:port i<port>e
    5:token 8:<token from get_peers>
  e
  1:q 13:announce_peer
  1:t 2:<tid>
  1:y 1:q
e
```

### Transaction IDs

```c
// 4-byte structure
// Bytes 0-1: Message type ("pn", "fn", "gp", "ap")
// Bytes 2-3: Sequence number (host order)

// Examples:
make_tid(tid, "pn", seqno)  // ping
make_tid(tid, "fn", seqno)  // find_node  
make_tid(tid, "gp", seqno)  // get_peers
make_tid(tid, "ap", seqno)  // announce_peer
```

---

## Security & Rate Limiting

### Token-Based Security

Prevents announce spoofing:

```c
token = HMAC(secret, IP || port)[0:8]

verify_token(token, address):
    return token == make_token(address, current_secret) ||
           token == make_token(address, old_secret)
```

**Properties:**
- IP+port specific
- Secret rotates every 15-45 minutes
- Old tokens valid ~1 hour

### Rate Limiting (Token Bucket)

```c
token_bucket():
    if tokens == 0:
        elapsed = now - last_refill
        tokens = min(400, 100 * elapsed)
    
    if tokens > 0:
        tokens--
        return ALLOW
    return DENY
```

**Parameters:**
- Max burst: 400 requests
- Refill: 100 tokens/second
- Only applies to queries (not replies)

### Address Filtering

```c
is_martian(address):
    IPv4: port==0 || 0.x.x.x || 127.x.x.x || multicast
    IPv6: port==0 || multicast || link-local || loopback || IPv4-mapped
```

---

## Timer Management

### Timer Types

| Timer | Purpose | Frequency |
|-------|---------|-----------|
| `rotate_secrets_time` | Token rotation | 15-45 min |
| `expire_stuff_time` | Cleanup stale data | 2-4 min |
| `confirm_nodes_time` | Bucket maintenance | Adaptive (5s-3min) |
| `search_time` | Search progress | Per-search (10s) |
| `token_bucket_time` | Rate limit refill | On demand |

### Adaptive Timing

```c
if maintenance_performed:
    confirm_nodes_time = now + 5-15 seconds   // Check soon
else:
    confirm_nodes_time = now + 60-180 seconds // Check later
```

---

## Rust Port: Actor Model Architecture

### Why Actor Model?

The Actor Model is ideal for this DHT implementation because:

1. **Single-writer principle** - One actor owns routing table state
2. **Message passing** - Natural fit for network protocols
3. **Isolation** - Fault tolerance (supervisor can restart)
4. **Composability** - Easy to add/remove features
5. **No locks** - Eliminates deadlock/race conditions
6. **Backpressure** - Bounded channels prevent overload

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Application Layer                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────────┐  │
│  │ Search API  │  │  Info API   │  │    Bootstrap/Management     │  │
│  └──────┬──────┘  └──────┬──────┘  └─────────────┬───────────────┘  │
│         │                │                       │                   │
│         ▼                ▼                       ▼                   │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                    DhtActor (Main Actor)                      │  │
│  │  ┌────────────────────────────────────────────────────────┐  │  │
│  │  │              Actor State (Single-Threaded)              │  │  │
│  │  │  ┌──────────────┐ ┌─────────────┐ ┌──────────────────┐  │  │  │
│  │  │  │ RoutingTable │ │  Searches   │ │     Storage      │  │  │  │
│  │  │  │ (k-buckets)  │ │  (HashMap)  │ │  (HashMap)       │  │  │  │
│  │  │  └──────────────┘ └─────────────┘ └──────────────────┘  │  │  │
│  │  └────────────────────────────────────────────────────────┘  │  │
│  │                           │                                   │  │
│  └───────────────────────────┼───────────────────────────────────┘  │
│                              │                                       │
│                   ┌──────────┴──────────┐                            │
│                   ▼                     ▼                            │
│         ┌─────────────────┐   ┌──────────────────┐                   │
│         │   NetworkActor  │   │  TimerActor      │                   │
│         │  (UDP Socket)   │   │ (Timer dispatch) │                   │
│         └─────────────────┘   └──────────────────┘                   │
└─────────────────────────────────────────────────────────────────────┘
```

### Actor Communication

```
┌──────────────┐    Command (mpsc)     ┌──────────────┐
│  Application │ ─────────────────────▶ │   DhtActor   │
└──────────────┘                        └──────────────┘
                                               │
                                               │ NetworkEvent
                                               │ (mpsc)
                                               ▼
                                        ┌──────────────┐
                                        │ NetworkActor │
                                        └──────────────┘
                                               │
                                               │ UdpPacket
                                               │ (UDP socket)
                                               ▼
                                          [Internet]
```

### Core Actor: DhtActor

```rust
/// Main DHT actor that owns all mutable state
pub struct DhtActor {
    /// Receive commands from application
    command_rx: mpsc::Receiver<DhtCommand>,
    
    /// Receive network events
    network_rx: mpsc::Receiver<NetworkEvent>,
    
    /// Receive timer events  
    timer_rx: mpsc::Receiver<TimerEvent>,
    
    /// Routing table (IPv4 and IPv6)
    routing_table: RoutingTable,
    
    /// Active searches
    searches: HashMap<SearchId, Search>,
    
    /// Stored peer announcements
    storage: HashMap<InfoHash, Vec<PeerInfo>>,
    
    /// Network actor handle
    network: mpsc::Sender<NetworkCommand>,
    
    /// Configuration
    config: DhtConfig,
    
    /// Our node ID
    node_id: NodeId,
    
    /// Token secrets (current and old)
    secrets: (Secret, Secret),
}

impl DhtActor {
    pub async fn run(mut self) -> Result<(), DhtError> {
        loop {
            tokio::select! {
                // Handle application commands
                cmd = self.command_rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd).await?,
                        None => break, // Channel closed
                    }
                }
                
                // Handle network packets
                event = self.network_rx.recv() => {
                    if let Some(event) = event {
                        self.handle_network_event(event).await?;
                    }
                }
                
                // Handle timer events
                event = self.timer_rx.recv() => {
                    if let Some(event) = event {
                        self.handle_timer_event(event).await?;
                    }
                }
            }
        }
        Ok(())
    }
}
```

### Message Types

```rust
/// Commands sent TO the DHT actor
pub enum DhtCommand {
    /// Start a search for peers
    Search {
        infohash: InfoHash,
        /// Port to announce (None = lookup only)
        announce_port: Option<u16>,
        /// Address family preference
        family: AddressFamily,
        /// Channel to send results
        result_tx: mpsc::Sender<SearchEvent>,
    },
    
    /// Bootstrap with known node
    AddNode {
        node_id: NodeId,
        addr: SocketAddr,
    },
    
    /// Ping a potential node
    PingNode {
        addr: SocketAddr,
    },
    
    /// Get routing table statistics
    GetStats {
        respond_to: oneshot::Sender<DhtStats>,
    },
    
    /// Shutdown the DHT
    Shutdown,
}

/// Events sent FROM the DHT actor (search results)
pub enum SearchEvent {
    /// Found peers
    Peers(Vec<PeerInfo>),
    /// Search completed (success or timeout)
    Complete,
    /// Error occurred
    Error(DhtError),
}

/// Network events FROM NetworkActor
pub enum NetworkEvent {
    PacketReceived {
        data: Bytes,
        from: SocketAddr,
    },
    SendError {
        addr: SocketAddr,
        error: std::io::Error,
    },
}

/// Network commands TO NetworkActor
pub enum NetworkCommand {
    SendPacket {
        data: Bytes,
        to: SocketAddr,
    },
    BindAddress {
        addr: SocketAddr,
    },
}
```

### Supporting Actors

#### NetworkActor

```rust
/// Handles all UDP socket I/O
pub struct NetworkActor {
    socket_v4: Option<UdpSocket>,
    socket_v6: Option<UdpSocket>,
    command_rx: mpsc::Receiver<NetworkCommand>,
    dht_tx: mpsc::Sender<NetworkEvent>,
}

impl NetworkActor {
    pub async fn run(mut self) -> Result<(), NetworkError> {
        let mut buf = [0u8; 4096];
        
        loop {
            tokio::select! {
                // Receive commands
                cmd = self.command_rx.recv() => {
                    match cmd {
                        Some(NetworkCommand::SendPacket { data, to }) => {
                            self.send_packet(&data, to).await?;
                        }
                        Some(NetworkCommand::BindAddress { addr }) => {
                            self.bind_address(addr).await?;
                        }
                        None => break,
                    }
                }
                
                // Receive IPv4 packets
                result = self.recv_v4(&mut buf), if self.socket_v4.is_some() => {
                    if let Ok((len, addr)) = result {
                        self.dht_tx.send(NetworkEvent::PacketReceived {
                            data: Bytes::copy_from_slice(&buf[..len]),
                            from: addr,
                        }).await.ok();
                    }
                }
                
                // Receive IPv6 packets
                result = self.recv_v6(&mut buf), if self.socket_v6.is_some() => {
                    if let Ok((len, addr)) = result {
                        self.dht_tx.send(NetworkEvent::PacketReceived {
                            data: Bytes::copy_from_slice(&buf[..len]),
                            from: addr,
                        }).await.ok();
                    }
                }
            }
        }
        Ok(())
    }
}
```

#### TimerActor

```rust
/// Manages all timer scheduling
pub struct TimerActor {
    command_rx: mpsc::Receiver<TimerCommand>,
    dht_tx: mpsc::Sender<TimerEvent>,
    timers: BinaryHeap<Timer>,
}

struct Timer {
    deadline: Instant,
    event: TimerEvent,
}

impl Ord for Timer {
    fn cmp(&self, other: &Self) -> Ordering {
        other.deadline.cmp(&self.deadline) // Reverse for min-heap
    }
}

impl TimerActor {
    pub async fn run(mut self) {
        loop {
            let next_deadline = self.timers.peek().map(|t| t.deadline);
            
            tokio::select! {
                // New timer requested
                cmd = self.command_rx.recv() => {
                    match cmd {
                        Some(TimerCommand::Schedule { deadline, event }) => {
                            self.timers.push(Timer { deadline, event });
                        }
                        Some(TimerCommand::Cancel { timer_id }) => {
                            // Remove specific timer
                        }
                        None => break,
                    }
                }
                
                // Wait until next timer deadline
                _ = sleep_until(next_deadline), if next_deadline.is_some() => {
                    if let Some(timer) = self.timers.pop() {
                        self.dht_tx.send(timer.event).await.ok();
                    }
                }
            }
        }
    }
}
```

---

## Stream-Based API Design

### Why Streams?

Streams provide a **pull-based, composable** alternative to callbacks:

1. **Composability** - Chain with `filter`, `map`, `take`, `timeout`
2. **Backpressure** - Consumer controls rate via polling
3. **Type Safety** - Return types encode success/failure
4. **Cancellation** - Drop stream to stop search
5. **Integration** - Works with `tokio::select!`, `futures::join`

### Public API

```rust
/// Main DHT client handle (cloneable)
#[derive(Clone)]
pub struct Dht {
    command_tx: mpsc::Sender<DhtCommand>,
}

impl Dht {
    /// Create and start DHT
    pub async fn new(config: DhtConfig) -> Result<Self, DhtError> {
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        
        // Spawn actors
        let dht_actor = DhtActor::new(config, cmd_rx).await?;
        tokio::spawn(dht_actor.run());
        
        Ok(Self { command_tx: cmd_tx })
    }
    
    /// Search for peers - returns a Stream of peer discoveries
    /// 
    /// # Example
    /// ```rust
    /// let mut peers = dht.search(infohash, AddressFamily::Both).await?;
    /// 
    /// while let Some(peer) = peers.next().await {
    ///     println!("Found peer: {}", peer.addr);
    /// }
    /// ```
    pub async fn search(
        &self,
        infohash: InfoHash,
        family: AddressFamily,
    ) -> Result<impl Stream<Item = PeerInfo>, DhtError> {
        let (result_tx, result_rx) = mpsc::channel(32);
        
        self.command_tx.send(DhtCommand::Search {
            infohash,
            announce_port: None,
            family,
            result_tx,
        }).await.map_err(|_| DhtError::Shutdown)?;
        
        // Convert channel to stream
        Ok(ReceiverStream::new(result_rx)
            .filter_map(|event| async move {
                match event {
                    SearchEvent::Peers(peers) => Some(stream::iter(peers)),
                    SearchEvent::Complete | SearchEvent::Error(_) => None,
                }
            })
            .flatten())
    }
    
    /// Search and announce - streams peers while also announcing
    pub async fn search_and_announce(
        &self,
        infohash: InfoHash,
        port: u16,
        family: AddressFamily,
    ) -> Result<impl Stream<Item = PeerInfo>, DhtError> {
        let (result_tx, result_rx) = mpsc::channel(32);
        
        self.command_tx.send(DhtCommand::Search {
            infohash,
            announce_port: Some(port),
            family,
            result_tx,
        }).await.map_err(|_| DhtError::Shutdown)?;
        
        Ok(ReceiverStream::new(result_rx)
            .take_while(|event| future::ready(!matches!(event, SearchEvent::Complete)))
            .filter_map(|event| async move {
                match event {
                    SearchEvent::Peers(peers) => Some(stream::iter(peers)),
                    _ => None,
                }
            })
            .flatten())
    }
    
    /// Bootstrap with known node
    pub async fn add_node(&self, node_id: NodeId, addr: SocketAddr) -> Result<(), DhtError> {
        self.command_tx.send(DhtCommand::AddNode { node_id, addr })
            .await
            .map_err(|_| DhtError::Shutdown)
    }
    
    /// Get routing table statistics
    pub async fn stats(&self) -> Result<DhtStats, DhtError> {
        let (tx, rx) = oneshot::channel();
        
        self.command_tx.send(DhtCommand::GetStats { respond_to: tx })
            .await
            .map_err(|_| DhtError::Shutdown)?;
        
        rx.await.map_err(|_| DhtError::Shutdown)
    }
    
    /// Shutdown the DHT gracefully
    pub async fn shutdown(self) -> Result<(), DhtError> {
        self.command_tx.send(DhtCommand::Shutdown)
            .await
            .map_err(|_| DhtError::Shutdown)
    }
}
```

### Advanced Stream Patterns

```rust
use futures::{StreamExt, TryStreamExt};

// Example 1: Search with timeout
let peers = dht.search(infohash, AddressFamily::Ipv4).await?
    .timeout(Duration::from_secs(30))
    .filter_map(|result| future::ready(result.ok())); // Ignore timeouts

// Example 2: Collect first 50 peers
let peers: Vec<PeerInfo> = dht.search(infohash, AddressFamily::Both).await?
    .take(50)
    .collect()
    .await;

// Example 3: Search multiple torrents concurrently
let searches = vec![hash1, hash2, hash3];
let all_peers = futures::stream::select_all(
    searches.into_iter()
        .map(|h| dht.search(h, AddressFamily::Both))
        .collect::<Result<Vec<_>, _>>()?
);

// Example 4: Process peers as they arrive with backpressure
while let Some(peer) = peers.next().await {
    // Process peer
    process_peer(peer).await;
    // If processing is slow, the stream will backpressure
    // and the DHT won't send more events until we poll again
}
```

---

## Internal Implementation Details

### Routing Table Module

```rust
/// Thread-safe routing table (owned by DhtActor)
pub struct RoutingTable {
    ipv4: KBucketTree,
    ipv6: KBucketTree,
    node_id: NodeId,
}

struct KBucketTree {
    buckets: Vec<KBucket>,
    /// Cache for replacement candidates
    pending: Vec<NodeInfo>,
}

struct KBucket {
    range: IdRange,
    nodes: Vec<Node>,
    max_size: usize,
    last_updated: Instant,
    /// Candidate for replacement
    cached: Option<NodeInfo>,
}

impl RoutingTable {
    /// Find bucket for ID
    fn find_bucket(&self, id: &NodeId, family: AddressFamily) -> Option<&KBucket> {
        // Binary search through sorted buckets
    }
    
    /// Insert or update node
    fn insert(&mut self, node: NodeInfo, confirmed: Confirmation) -> InsertResult {
        // 1. Check if node exists - update timestamps
        // 2. If bucket has space - insert
        // 3. If bucket full and contains our ID - split
        // 4. If can't split - try to evict bad node
        // 5. Otherwise - cache for later
    }
    
    /// Find closest nodes to target
    fn closest(&self, target: &NodeId, family: AddressFamily, n: usize) -> Vec<Node> {
        // XOR distance sort, return closest N
    }
}
```

### Search Module

```rust
/// State machine for iterative lookup
pub struct Search {
    id: SearchId,
    target: InfoHash,
    announce_port: Option<u16>,
    family: AddressFamily,
    /// Candidates sorted by XOR distance
    candidates: BTreeMap<XorDistance, SearchNode>,
    /// Active in-flight queries
    in_flight: HashSet<NodeId>,
    /// State
    state: SearchState,
    /// Result channel
    result_tx: mpsc::Sender<SearchEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SearchState {
    Seeding,
    Querying { pending: usize },
    Announcing { remaining: usize },
    Complete,
}

impl Search {
    /// Process a step - called when timer fires or response received
    fn step(&mut self) -> Vec<Action> {
        match &self.state {
            SearchState::Seeding => {
                // Send initial queries
                self.start_queries()
            }
            SearchState::Querying { pending } => {
                if *pending == 0 {
                    // All queries responded
                    if self.announce_port.is_some() {
                        self.start_announces()
                    } else {
                        self.complete()
                    }
                } else {
                    // Send more queries if slots available
                    self.fill_query_slots()
                }
            }
            SearchState::Announcing { remaining } => {
                if *remaining == 0 {
                    self.complete()
                } else {
                    vec![] // Wait for acks
                }
            }
            SearchState::Complete => vec![],
        }
    }
    
    fn handle_response(&mut self, from: &NodeId, nodes: Vec<NodeInfo>, peers: Vec<PeerInfo>) {
        // Update candidate state
        // Add new nodes to candidates
        // Send peers to result channel
    }
}

/// Actions the search wants the actor to take
pub enum Action {
    SendQuery { to: NodeInfo, query: Query },
    ScheduleTimer { delay: Duration, event: SearchEvent },
    SendResults { peers: Vec<PeerInfo> },
    Complete,
}
```

### Protocol Module

```rust
/// Zero-copy bencode parser
pub struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn parse_message(&mut self) -> Result<Message<'a>, ParseError> {
        // Parse bencode dictionary
        // Extract fields by name (memmem-like search)
        // Validate lengths and formats
    }
}

/// Message types
#[derive(Debug, Clone)]
pub enum Message<'a> {
    Query(Query<'a>),
    Response(Response<'a>),
    Error(ErrorResponse<'a>),
}

#[derive(Debug, Clone)]
pub enum Query<'a> {
    Ping { tid: &'a [u8], id: NodeId },
    FindNode { tid: &'a [u8], id: NodeId, target: NodeId, want: Want },
    GetPeers { tid: &'a [u8], id: NodeId, infohash: InfoHash, want: Want },
    AnnouncePeer { 
        tid: &'a [u8], 
        id: NodeId, 
        infohash: InfoHash, 
        port: u16,
        implied_port: Option<u16>,
        token: &'a [u8] 
    },
}

/// Serializer
pub struct Serializer {
    buf: Vec<u8>,
}

impl Serializer {
    pub fn ping_response(&mut self, tid: &[u8], id: NodeId) -> &[u8] {
        // Write bencode
        // d1:r d 2:id 20:<id> e 1:t <tid> 1:y 1:r e
    }
    
    pub fn get_peers_response(
        &mut self,
        tid: &[u8],
        id: NodeId,
        token: &[u8],
        nodes: &[CompactNode],
        peers: &[PeerInfo],
    ) -> &[u8] {
        // Write bencode with nodes and/or peers
    }
}
```

---

## Error Handling Strategy

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DhtError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Invalid node ID: {0}")]
    InvalidNodeId(String),
    
    #[error("Parse error: {0}")]
    ParseError(String),
    
    #[error("Search limit exceeded")]
    SearchLimitExceeded,
    
    #[error("DHT is shutting down")]
    Shutdown,
    
    #[error("Timeout")]
    Timeout,
}

/// Result type for DHT operations
pub type Result<T> = std::result::Result<T, DhtError>;

/// Error recovery strategies
/// 
/// 1. Parse errors: Log and drop packet
/// 2. Network errors: Retry with backoff
/// 3. Search errors: Send error event, clean up
/// 4. Actor panic: Supervisor restarts
```

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_xor_distance() {
        let id1 = NodeId::from_hex("0000000000000000000000000000000000000000");
        let id2 = NodeId::from_hex("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF");
        let target = NodeId::from_hex("0000000000000000000000000000000000000001");
        
        // id1 is closer to target
        assert!(id1.distance(&target) < id2.distance(&target));
    }
    
    #[test]
    fn test_bucket_splitting() {
        let mut table = RoutingTable::new(NodeId::random());
        
        // Fill root bucket
        for i in 0..128 {
            let node = NodeInfo {
                id: NodeId::random(),
                addr: "192.168.1.1:6881".parse().unwrap(),
            };
            table.insert(node, Confirmation::Direct);
        }
        
        // Should have split
        assert!(table.buckets().len() > 1);
    }
    
    #[test]
    fn test_bencode_roundtrip() {
        let msg = Message::Query(Query::Ping {
            tid: b"pn01",
            id: NodeId::random(),
        });
        
        let mut serializer = Serializer::new();
        let encoded = serializer.serialize(&msg);
        
        let mut parser = Parser::new(encoded);
        let decoded = parser.parse_message().unwrap();
        
        assert_eq!(msg, decoded);
    }
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_two_node_communication() {
    let node1 = Dht::new(DhtConfig::default()).await.unwrap();
    let node2 = Dht::new(DhtConfig::default()).await.unwrap();
    
    // Bootstrap node2 with node1
    let node1_info = node1.local_addr().await.unwrap();
    node2.add_node(node1.node_id(), node1_info).await.unwrap();
    
    // Wait for convergence
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // Node2 should find node1 in routing table
    let stats = node2.stats().await.unwrap();
    assert!(stats.good_nodes > 0);
}

#[tokio::test]
async fn test_search_convergence() {
    // Create small DHT network
    let bootstrap = create_test_network(10).await;
    
    // Create searching node
    let searcher = Dht::new(DhtConfig {
        bootstrap_nodes: vec![bootstrap],
        ..Default::default()
    }).await.unwrap();
    
    // Search for infohash
    let target = InfoHash::random();
    let peers: Vec<_> = searcher.search(target, AddressFamily::Both)
        .await
        .unwrap()
        .timeout(Duration::from_secs(30))
        .collect()
        .await;
    
    // Should find some peers (from network announcements)
    assert!(!peers.is_empty());
}
```

### Property-Based Tests

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_id_distance_symmetry(id1 in any::<NodeId>(), id2 in any::<NodeId>()) {
        assert_eq!(id1.distance(&id2), id2.distance(&id1));
    }
    
    #[test]
    fn test_id_distance_triangle_inequality(
        a in any::<NodeId>(),
        b in any::<NodeId>(),
        c in any::<NodeId>()
    ) {
        // In XOR metric: d(a,c) <= d(a,b) XOR d(b,c)
        let d_ac = a.distance(&c);
        let d_ab = a.distance(&b);
        let d_bc = b.distance(&c);
        
        assert!(d_ac <= d_ab ^ d_bc);
    }
}
```

---

## Performance Considerations

### Memory Layout

```rust
/// Optimize for cache locality
#[repr(C, packed)]
struct Node {
    id: NodeId,              // 20 bytes
    addr: SocketAddr,        // 32 bytes (SocketAddrV6)
    time: u32,               // 4 bytes (epoch seconds)
    reply_time: u32,         // 4 bytes
    pinged_time: u32,        // 4 bytes
    pinged: u8,              // 1 byte
    // Total: 65 bytes, ~1.7 cache lines
}

/// Use SmallVec for small collections
use smallvec::SmallVec;

type NodeList = SmallVec<[Node; 8]>; // Most buckets have 8 nodes
```

### Zero-Copy Parsing

```rust
use bytes::Bytes;

/// Parse without copying data
fn parse_packet(data: Bytes) -> Result<Message, ParseError> {
    // Use Bytes for reference-counted slices
    // Parse in-place without allocating
}

/// Reuse buffers
pub struct PacketBuffer {
    buf: BytesMut,
}

impl PacketBuffer {
    pub fn recv(&mut self, socket: &UdpSocket) -> Result<Bytes, Error> {
        self.buf.resize(4096, 0);
        let n = socket.recv(&mut self.buf)?;
        self.buf.truncate(n);
        Ok(self.buf.split().freeze())
    }
}
```

### Lock-Free Channels

```rust
use crossbeam::channel::{bounded, Sender, Receiver};

/// For high-throughput scenarios
pub struct HighPerfDht {
    command_tx: Sender<DhtCommand>,
    // ...
}
```

---

## Crate Structure

```
dht/
├── Cargo.toml
├── src/
│   ├── lib.rs           # Public API, re-exports
│   ├── actor.rs         # DhtActor implementation
│   ├── client.rs        # Dht handle (cloneable client)
│   ├── routing/
│   │   ├── mod.rs
│   │   ├── table.rs     # RoutingTable
│   │   ├── bucket.rs    # KBucket
│   │   └── node.rs      # NodeInfo
│   ├── search/
│   │   ├── mod.rs
│   │   ├── manager.rs   # Search coordination
│   │   └── state.rs     # Search state machine
│   ├── protocol/
│   │   ├── mod.rs
│   │   ├── message.rs   # Message types
│   │   ├── encode.rs    # Bencode serialization
│   │   └── decode.rs    # Bencode parsing
│   ├── network/
│   │   ├── mod.rs
│   │   └── actor.rs     # NetworkActor
│   ├── storage/
│   │   ├── mod.rs
│   │   └── peer_store.rs # InfoHash -> Peers mapping
│   ├── security/
│   │   ├── mod.rs
│   │   ├── token.rs     # Token generation/verification
│   │   └── rate_limit.rs # Token bucket
│   ├── types/
│   │   ├── mod.rs
│   │   ├── id.rs        # NodeId, InfoHash
│   │   └── addr.rs      # Address types
│   ├── error.rs         # Error types
│   └── config.rs        # DhtConfig
├── tests/
│   ├── integration_tests.rs
│   └── prop_tests.rs
└── benches/
    └── routing_table.rs
```

---

## Dependencies

```toml
[dependencies]
# Async runtime
tokio = { version = "1", features = ["net", "rt", "time", "sync", "macros"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_bytes = "0.11"

# Bencode (or custom implementation)
bendy = "0.3"  # Or implement manually for zero-copy

# Bytes manipulation
bytes = "1"
bytesize = "1"

# Crypto (token generation)
sha-1 = "0.10"
hmac = "0.12"
rand = "0.8"

# Error handling
thiserror = "1"

# Logging/tracing
tracing = "0.1"

# Utilities
futures = "0.3"
tokio-stream = "0.1"
smallvec = "1"
slotmap = "1"  # For stable IDs

[dev-dependencies]
tokio-test = "0.4"
proptest = "1"
criterion = "0.5"
```

---

## Migration Guide from C

### API Translation

| C API | Rust API | Notes |
|-------|----------|-------|
| `dht_init()` | `Dht::new(config).await` | Async constructor |
| `dht_periodic()` | Actor event loop | Internal, not exposed |
| `dht_search()` | `dht.search(hash).await` | Returns Stream |
| Callback | `mpsc::channel` | Type-safe message passing |
| `dht_sendto()` | `UdpSocket` | Direct async I/O |
| `dht_nodes()` | `dht.stats().await` | Async query |
| `dht_uninit()` | Drop handle | Automatic cleanup |

### Behavior Changes

1. **No global state** - All state owned by Actor
2. **Explicit types** - No void* closures
3. **Result types** - Errors encoded in types, not errno
4. **Cancellation** - Drop stream to cancel search
5. **Composability** - Chain stream operations

---

## Conclusion

This design provides:

1. **Type Safety** - Leverage Rust's type system
2. **Composability** - Stream-based APIs
3. **Concurrency** - Actor model without locks
4. **Performance** - Zero-copy, cache-friendly
5. **Testability** - Clear separation of concerns
6. **Maintainability** - Modern Rust patterns

The Actor Model with Stream-Based API offers significant advantages over the original C callback design while maintaining full protocol compatibility with the BitTorrent DHT network.
