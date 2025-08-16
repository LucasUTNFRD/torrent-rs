### **Peer task responsibilities**

- Owns the network I/O for a single peer (read/write messages).
    
- Tracks timing for keep-alive and peer inactivity.
    
- Reports events back to the manager (`PeerAdded`, `ReceivedMessage`, `PeerError`, `PeerDisconnect`).
    

The peer task **does not decide what pieces to request, when to choke, or which peer to favor**. It just faithfully executes commands and reports what happens.

---

### **Peer manager responsibilities**

- Maintains the global view: all peers, active pieces, and current downloads.
    
- Implements policies:
    
    1. **Piece selection**
        
        - Decides which piece to request from which peer.
            
        - Sends `SendMessage(RequestPiece(piece_id))` to the corresponding peer task.
            
    2. **Choking/unchoking**
        
        - Monitors peer download/upload rates.
            
        - Sends `SendMessage(Choke/Unchoke)` commands to peers.
            
        - Can implement optimistic unchoking by periodically choosing a random peer to unchoke.
            
    3. **Work-stealing / rarest-first**
        
        - Chooses peers to request rare pieces from.
            
        - Can coordinate multiple peer tasks to avoid duplicate requests unnecessarily.
            

---

### **Why this separation helps**

1. **Centralized policy**
    
    - Manager sees all peers; can enforce global rules.
        
    - Peer tasks are dumb executors; no state duplication or race conditions.
        
2. **Isolation**
    
    - If a peer misbehaves (disconnects, sends bad data), only that task fails. Manager can reassign work safely.
        
3. **Simpler scheduling**
    
    - Manager can queue commands to peers without worrying about socket readiness.
        
    - Peer tasks consume the commands in a natural async loop.
        
4. **Scalable algorithms**
    
    - You can implement complex choking strategies (e.g., proportional download rate, optimistic unchoking) without touching the network I/O logic.
        

---

### **Analogy**

- **Peer task**: a “worker” that faithfully executes instructions.
    
- **Peer manager**: the “scheduler/brain” that decides what work to assign, in what order, and to which workers.
    

By decoupling execution (I/O) from policy (scheduling), the manager can implement **advanced algorithms safely and efficiently**, which would be messy if every peer tried to decide on its own.
