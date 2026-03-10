# Rust BitTorrent Simulation Suite: Porting Plan

This document outlines the architecture and implementation strategy for porting the `libtorrent` deterministic simulation environment to a Rust-based BitTorrent client using the Actor model and the Tokio runtime.

## 1. Vision & Goals
Create a **deterministic, discrete-event simulation** where time and network behavior are fully controlled. This allows for reproducible testing of complex swarm behaviors, protocol edge cases, and DHT routing without real-world network variability.

**Constraints:**
- Protocol: BitTorrent v1 (No uTP, No v2).
- Runtime: Tokio-based Actor model.
- Abstraction: Virtualize at the `tokio::net` layer.

## 2. Architectural Mapping

| libtorrent (C++ / libsimulator) | Rust Port Equivalent | Responsibility |
| :--- | :--- | :--- |
| `sim::simulation` | `SimulationEngine` | Central event loop and global virtual clock. |
| `sim::asio::io_context` | `VirtualNode` | Container for one Peer Actor with its own Virtual IP. |
| `sim::asio::ip::tcp::socket` | `SimTcpStream` | Implements `AsyncRead`/`AsyncWrite`, routing to the `Router`. |
| `sim::sink` / `sim::route` | `NetworkLink` | Models latency, packet loss, and bandwidth limits. |
| `setup_swarm()` | `SwarmBuilder` | DSL/Fluent API to bootstrap N actors with specific settings. |

## 3. Core Architecture Components

### A. Deterministic Runtime (`SimulationHost`)
A custom wrapper around the task scheduler that:
- **Virtual Clock**: Utilizes `tokio::time::pause()` and `advance()`. Time only advances when all actors are "quiescent" (waiting on I/O or timers).
- **Event Queue**: A priority queue of `(Instant, Event)` where `Event` is "Timer Fired" or "Packet Delivered".
- **Reproducibility**: Uses a fixed seed for all internal PRNGs (randomness), including PeerID generation and Piece Picking.

### B. Virtual Networking Layer
Virtualized at the `tokio::net` level using a provider pattern:
- **`NetworkProvider` Trait**: Defines `connect`, `listen`, `bind`.
- **`RealNetwork`**: Standard implementation using `tokio::net::TcpStream`.
- **`SimNetwork`**: Simulation implementation using `SimTcpStream` which talks to a central `Router` actor.
- **`Router`**: Calculates delivery times based on a `TopologyMap` (e.g., DSL vs. Fiber links). It captures writes and "delivers" them to the reader's buffer after the calculated latency.

### C. Actor Integration
Each BitTorrent peer is an independent Actor spawned within a `VirtualNode`.
1. The `SimulationHost` initializes N `VirtualNodes`.
2. Each node is assigned a `VirtualIP`.
3. The client's `NetworkProvider` is injected with the node's `SimNetwork` handle.

## 4. Scope of Protocol Correctness
... (rest of section 4) ...

## 5. Implementation Roadmap

### Phase 0: Abstraction Prep (Week 0)
- [ ] **Trait-ify Networking**: Refactor `Peer<S>` to be generic over `T: AsyncRead + AsyncWrite + Unpin + Send + 'static`.
- [ ] **Clock Abstraction**: Replace `Instant::now()` calls with a `Clock` trait to allow mocking.
- [ ] **Seeded Entropy**: Inject `Rng` instances into actors instead of using `thread_rng()`.

### Phase 1: Core Engine (Week 1)
- [ ] Implement `SimulationEngine` priority queue and virtual clock management.
- [ ] Create mockable `Instant` and `Sleep` abstractions compatible with Tokio's `pause/advance`.
- [ ] Implement basic `Router` with fixed latency.

### Phase 2: Virtual Network (Week 2)
- [ ] Define `NetworkProvider` trait for the client.
- [ ] Implement `SimTcpStream` and `SimTcpListener` (In-memory byte channels).
- [ ] Implement basic topology modeling (latency/jitter).

### Phase 3: Client Adaptation (Week 3)
- [ ] Refactor existing Client Actors to use the `NetworkProvider`.
- [ ] Build `SwarmBuilder` to automate multi-actor setup.
- [ ] Implement deterministic PRNG for the client when in "Sim Mode".

### Phase 4: Porting Scenarios (Week 4+)
- [ ] Port `test_transfer.cpp` -> `tests/sim_transfer.rs`.
- [ ] Port `test_swarm.cpp` -> `tests/sim_swarm.rs`.
- [ ] Port `test_dht.cpp` -> `tests/sim_dht.rs`.
