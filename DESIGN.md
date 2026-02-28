# Synapse Design Specification (Phase 1)

## Overview
Synapse is a cross-language runtime bridge using shared memory + lock-free ring buffers.
Target: Python ↔ C++/Rust with <1μs latency, zero-copy data access.

## Architecture

```
Python Process ←→ [Shared Memory Region] ←→ C++/Rust Process
                  Synapse Runtime (Rust)

Shared Memory Layout:
┌──────────────────────────────────────┐
│ Control Block (256 bytes)            │
│   magic: u64 = 0x53594E4150534500    │
│   version: u32                       │
│   session_token: u128 (random UUID)  │
│   channel_count: u32                 │
│   heartbeat_a/b: u64                 │
│   state: u32 (init/ready/shutdown)   │
├──────────────────────────────────────┤
│ Channel Registry (variable)          │
├──────────────────────────────────────┤
│ Ring Buffer A→B (SPSC, lock-free)    │
│   head: u64 (cacheline aligned)      │
│   tail: u64 (cacheline aligned)      │
│   slots[capacity]: [len:u32][data]   │
├──────────────────────────────────────┤
│ Ring Buffer B→A (SPSC)               │
├──────────────────────────────────────┤
│ Data Slots (schema-driven structs)   │
│   Latest-Value slots for AI Agent    │
└──────────────────────────────────────┘
```

## Phase 1 Scope (MVP)

### Must Have
1. Rust core library (synapse-core crate)
   - Cross-platform shared memory (Linux mmap + Windows CreateFileMapping)
   - Lock-free SPSC ring buffer (power-of-2 cap, cacheline-aligned)
   - Control block with magic + session token
   - Host/connect lifecycle

2. Python bindings (PyO3)
   - synapse.host(name) / synapse.connect(name)
   - bridge.send(bytes) / bridge.recv() -> bytes
   - GIL released during shm ops

3. C++ header-only client (synapse.h)
   - Same API pattern
   - No deps beyond OS headers

4. Working demo: Python AI ↔ C++ game loop

### Key Decisions
- SPSC not MPMC (simpler, faster, use N channels for N endpoints)
- Rust core → PyO3 + cbindgen
- Session token (u128) prevents cross-attach
- Adaptive wait: spin → yield → futex

## Bug Fixes from Review
- slot_ptr must use index * slot_size offset (not always slot[0])
- Python recv must read ring_ba (B→A), not ring_ab
