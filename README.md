# Synapse

Cross-language runtime bridge via shared memory + lock-free ring buffers.
Zero-copy, sub-microsecond latency communication between Python, C++, and Rust.

## Why Synapse?

Python dominates AI/ML. Game engines and performance-critical systems use C++/Rust. Existing bridges are either too slow (gRPC ~100us+), too coupled (pybind11 requires the same process), or too complex (raw shared memory with hand-rolled protocols).

Synapse provides:
- **~100ns latency** via shared memory — 100x faster than localhost sockets
- **Lock-free SPSC ring buffers** — bidirectional streaming, zero mutexes
- **Schema-driven type layout** via `.bridge` IDL — zero-copy struct access, no serialization
- **Typed channels** — `TypedChannel<T>` for schema-driven zero-copy reads and writes
- **Latest-Value Slots** — seqlock-based async inference results, always-fresh values
- **Adaptive wait** — Spin -> Yield -> Park (futex/WaitOnAddress) with configurable thresholds
- **Graceful shutdown** — heartbeat watchdog, peer death detection, clean resource cleanup
- **Cross-platform** — Linux (POSIX shm) and Windows (CreateFileMapping)
- **Three language targets** — Rust core, Python (PyO3 + pure-mmap), C++ header-only

---

## Architecture

```
+---------------------------------------------------------------------+
|                          Synapse Bridge                               |
|                                                                       |
|   Python Process (Host)           C++/Rust Process (Connector)        |
|   ---------------------           ----------------------------        |
|   bridge = host("game")           bridge = connect("game")            |
|   bridge.send(b"frame")    -->    msg = bridge.recv()                 |
|   ack = bridge.recv()      <--    bridge.send("ACK")                  |
|                                                                       |
|          +------------------------------------------+                 |
|          |       Shared Memory Region                |                 |
|          +------------------------------------------+                 |
|          |  ControlBlock (256 bytes)                 |                 |
|          |    magic, version, session_token           |                 |
|          |    state (Init/Ready/Closing/Dead)         |                 |
|          |    creator_pid, connector_pid              |                 |
|          |    heartbeat_a, heartbeat_b                |                 |
|          +------------------------------------------+                 |
|          |  Channel Registry (Phase 2)               |                 |
|          |    name -> (offset, capacity, slot_size)   |                 |
|          +------------------------------------------+                 |
|          |  Ring A->B (SPSC, lock-free)              |                 |
|          |    head (cacheline-aligned)                |                 |
|          |    tail (cacheline-aligned)                |                 |
|          |    slots[N]: [len:u32][payload]            |                 |
|          +------------------------------------------+                 |
|          |  Ring B->A (SPSC, lock-free)              |                 |
|          +------------------------------------------+                 |
|          |  Latest-Value Slots (seqlock-based)       |                 |
|          |    AI inference results, game state        |                 |
|          +------------------------------------------+                 |
+---------------------------------------------------------------------+

IDL Pipeline (.bridge -> multi-language bindings):
  game.bridge -> synapse-idl -> Rust structs / Python ctypes / C++ structs
                  lexer -> parser -> AST -> layout (C ABI) -> codegen
```

---

## Project Structure

```
Synapse/
├── core/                   # Rust crate: synapse-core
│   └── src/
│       ├── lib.rs          # host(), connect() — public Bridge API
│       ├── control.rs      # ControlBlock — state machine, session token
│       ├── ring.rs         # SPSC ring buffer — RingHeader, Ring
│       ├── shm.rs          # SharedRegion — cross-platform mmap
│       ├── error.rs        # SynapseError, Result
│       ├── typed_channel.rs # TypedChannel<T> + ChannelRegistry (Phase 2)
│       ├── latest_slot.rs  # LatestSlot<T> — seqlock-based (Phase 2)
│       ├── wait.rs         # Adaptive wait: Spin -> Yield -> Park (Phase 2)
│       └── shutdown.rs     # Watchdog + ShutdownProtocol (Phase 2)
│   └── benches/
│       ├── ring_bench.rs   # Criterion: push/pop, throughput, LVS
│       └── latency_bench.rs # Criterion: RTT, baseline comparisons
│
├── idl/                    # Rust crate: synapse-idl
│   └── src/
│       ├── lib.rs          # parse(), compile(), generate_rust/python/cpp()
│       ├── ast.rs          # Schema AST — Struct, Enum, Channel, Type
│       ├── lexer.rs        # Tokenizer for .bridge source files
│       ├── parser.rs       # Recursive-descent parser
│       ├── layout.rs       # C ABI alignment + struct sizing
│       ├── bin/synapse.rs  # synapse compile CLI tool
│       └── codegen/        # Language-specific code generators
│           ├── rust.rs     # -> #[repr(C)] Rust structs
│           ├── python.rs   # -> ctypes Python classes
│           └── cpp.rs      # -> C++ structs with static_assert
│
├── bindings/
│   ├── python/             # PyO3 extension (native synapse module)
│   └── cpp/include/
│       └── synapse.h       # Header-only C++ client — no build step
│
├── examples/
│   ├── python_sender.py    # Python host: pure-mmap, sends frames
│   └── cpp_receiver.cpp    # C++ connector: receives frames, sends ACK
│
├── benchmarks/             # Benchmark docs and results
│   ├── README.md           # Benchmark methodology and summary table
│   └── results/            # Output storage
│
├── docs/
│   ├── PHASE2.md           # Phase 2 detailed design
│   └── research/           # Architecture research notes
│
├── DESIGN.md               # Full design specification
├── CONTRIBUTING.md         # How to contribute
└── CHANGELOG.md            # Release history
```

---

## Phase 2 API Overview

### Typed Channels

Zero-copy typed reads and writes over ring buffers:

```rust
use synapse_core::typed_channel::TypedChannel;
use synapse_core::ring::RingHeader;

#[repr(C)]
#[derive(Clone, Copy)]
struct GameState { x: f32, y: f32, z: f32, frame_id: u64 }

// Create a typed channel
let slot_size = ((4 + std::mem::size_of::<GameState>() + 7) & !7) as u64;
unsafe {
    RingHeader::init(ring_ptr, 1024, slot_size);
    let ch = TypedChannel::<GameState>::from_ring_ptr(ring_ptr).unwrap();

    ch.write(&GameState { x: 1.0, y: 2.0, z: 3.0, frame_id: 0 }).unwrap();
    let state = ch.read().unwrap();
}
```

### Channel Registry

Multiple named channels in a single shared memory region:

```rust
use synapse_core::typed_channel::{ChannelRegistry, REGISTRY_SIZE};

unsafe {
    ChannelRegistry::init(base_ptr);
    let reg = ChannelRegistry::from_ptr(base_ptr);
    reg.register("game_state", offset, 1024, slot_size).unwrap();
    let desc = reg.lookup("game_state").unwrap();
}
```

### Latest-Value Slots (Seqlock)

Single-writer, multi-reader slots for async inference results:

```rust
use synapse_core::latest_slot::LatestSlot;

#[repr(C)]
#[derive(Clone, Copy)]
struct Decision { action: u32, confidence: f32 }

unsafe {
    LatestSlot::<Decision>::init(slot_ptr);
    let slot = LatestSlot::<Decision>::from_ptr(slot_ptr);

    // Writer (AI agent)
    slot.write(&Decision { action: 1, confidence: 0.95 });

    // Reader (game loop) — wait-free
    if let Some(decision) = slot.read() {
        println!("action={}, conf={}", decision.action, decision.confidence);
    }
}
```

### Adaptive Wait

Configurable spin -> yield -> park strategy:

```rust
use synapse_core::wait::{WaitStrategy, Waiter, wake_one};
use std::sync::atomic::AtomicU32;
use std::time::Duration;

let waiter = Waiter::new(WaitStrategy::Adaptive {
    spin_count: 100,
    yield_count: 10,
});

let flag = AtomicU32::new(0);
let result = waiter.wait_until(
    &flag, 0,
    || flag.load(std::sync::atomic::Ordering::Acquire) != 0,
    Duration::from_secs(1),
);
```

### Graceful Shutdown

Heartbeat watchdog and shutdown protocol:

```rust
use synapse_core::shutdown::{Watchdog, ShutdownProtocol, ShutdownMode, PeerStatus};

unsafe {
    let mut wd = Watchdog::new(cb_ptr, true);
    wd.beat(); // Call periodically

    match wd.check_peer() {
        PeerStatus::Alive => { /* ok */ }
        PeerStatus::Dead => { /* reconnect */ }
        _ => {}
    }

    let proto = ShutdownProtocol::new(cb_ptr as *mut _, true);
    proto.initiate(ShutdownMode::Graceful); // Closing -> drain -> Dead
    proto.complete();
}
```

---

## Shared Memory Layout

| Zone | Size | Purpose |
|------|------|---------|
| ControlBlock | 256 bytes | Magic number, version, session token, state, PIDs |
| Channel Registry | ~4.7 KB | Name -> (offset, capacity, slot_size) for up to 64 channels |
| Ring A->B | `192 + cap * slot_size` bytes | Host -> Connector SPSC ring |
| Ring B->A | same | Connector -> Host SPSC ring |
| Latest-Value Slots | `16 + sizeof(T)` per slot | Seqlock-based async data |

**Defaults**: 1024 slots, 256 bytes/slot (252 bytes max payload + 4-byte length prefix).

---

## IDL Schema System (.bridge)

The `.bridge` IDL defines cross-language data types. The compiler produces **byte-for-byte identical C ABI layouts** in Rust, Python (ctypes), and C++.

### Syntax

```
namespace game;

struct Vec3f { x: f32, y: f32, z: f32, }

struct GameState {
    position: Vec3f,
    velocity: Vec3f,
    health:   f32,
    frame_id: u64,
}

enum Command {
    MoveTo  { target: Vec3f },
    Attack  { target_id: u32, weapon_id: u32 },
    Idle,
}

channel game_bridge {
    host_to_client: GameState,
    client_to_host: Command,
}
```

### CLI Compiler

```bash
synapse compile game.bridge --lang rust python cpp --output src/generated/
```

---

## Build Instructions

### Prerequisites

- Rust 1.75+ (`rustup update stable`)
- C++17 compiler for examples (GCC / Clang / MSVC)
- Python 3.8+ for Python examples

### Core & IDL

```bash
cd core && cargo build && cargo test
cd idl  && cargo build && cargo test
```

### Run Benchmarks

```bash
cd core && cargo bench
```

### C++ header

Header-only — no build step needed. Just include:

```bash
g++ -std=c++17 -O2 -Ibindings/cpp/include \
    -o examples/cpp_receiver examples/cpp_receiver.cpp -lrt
```

### Run the demo

```bash
# Terminal 1
python examples/python_sender.py

# Terminal 2
./examples/cpp_receiver
```

---

## Performance

| Transport | Roundtrip 64B | Roundtrip 1KB |
|-----------|--------------|---------------|
| **Synapse (shm)** | ~200 ns | ~400 ns |
| Unix domain socket | ~2 us | ~4 us |
| TCP loopback | ~10 us | ~15 us |

Synapse achieves **10-50x lower latency** than socket-based IPC.

See [benchmarks/README.md](benchmarks/README.md) for the full benchmark suite.

---

## Status & Roadmap

### Phase 1 — Core Runtime (Complete)
Cross-platform shared memory, lock-free SPSC rings, control block state machine, Rust/Python/C++ bindings.

### Phase 1.5 — IDL Schema System (Complete)
`.bridge` format, lexer/parser/layout engine, Rust + Python + C++ codegen with identical C ABI layouts.

### Phase 2 — Schema-Driven Channels (Complete)
- Zero-copy typed channels with channel registry
- Latest-Value Slots (seqlock) for AI async results
- Adaptive wait strategy: spin -> yield -> futex/WaitOnAddress
- `synapse compile` CLI tool
- Graceful shutdown protocol with heartbeat watchdog
- Criterion benchmark suite with baseline comparisons
- Full pipeline integration tests

See [docs/PHASE2.md](docs/PHASE2.md) for the full Phase 2 design.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT
