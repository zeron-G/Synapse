# Synapse

Cross-language runtime bridge via shared memory + lock-free ring buffers.  
Zero-copy, sub-microsecond latency communication between Python, C++, and Rust.

## Why Synapse?

Python dominates AI/ML. Game engines and performance-critical systems use C++/Rust. Existing bridges are either too slow (gRPC ~100μs+), too coupled (pybind11 requires the same process), or too complex (raw shared memory with hand-rolled protocols).

Synapse provides:
- **~100ns latency** via shared memory — 100× faster than localhost sockets
- **Lock-free SPSC ring buffers** — bidirectional streaming, zero mutexes
- **Schema-driven type layout** via `.bridge` IDL — zero-copy struct access, no serialization
- **AI Agent optimized** — Latest-Value Slots for async inference results
- **Cross-platform** — Linux (POSIX shm) and Windows (CreateFileMapping)
- **Three language targets** — Rust core, Python (PyO3 + pure-mmap), C++ header-only

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                          Synapse Bridge                             │
│                                                                     │
│   Python Process (Host)           C++/Rust Process (Connector)      │
│   ─────────────────────           ──────────────────────────────    │
│   bridge = host("game")           bridge = connect("game")          │
│   bridge.send(b"frame")    ──→    msg = bridge.recv()               │
│   ack = bridge.recv()      ←──    bridge.send("ACK")                │
│                                                                     │
│          ┌──────────────────────────────────────┐                   │
│          │       Shared Memory Region            │                   │
│          ├──────────────────────────────────────┤                   │
│          │  ControlBlock (256 bytes)             │                   │
│          │    magic, version, session_token      │                   │
│          │    state (Init/Ready/Closing/Dead)    │                   │
│          │    creator_pid, connector_pid         │                   │
│          │    heartbeat_a, heartbeat_b           │                   │
│          ├──────────────────────────────────────┤                   │
│          │  Ring A→B (SPSC, lock-free)           │                   │
│          │    head (cacheline-aligned)           │                   │
│          │    tail (cacheline-aligned)           │                   │
│          │    slots[N]: [len:u32][payload]       │                   │
│          ├──────────────────────────────────────┤                   │
│          │  Ring B→A (SPSC, lock-free)           │                   │
│          ├──────────────────────────────────────┤                   │
│          │  Data Slots (Phase 2: schema-typed)   │                   │
│          │    Latest-Value slots for AI agents   │                   │
│          └──────────────────────────────────────┘                   │
└─────────────────────────────────────────────────────────────────────┘

IDL Pipeline (.bridge → multi-language bindings):
  game.bridge → synapse-idl → Rust structs / Python ctypes / C++ structs
                  lexer → parser → AST → layout (C ABI) → codegen
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
│       └── error.rs        # SynapseError, Result
│
├── idl/                    # Rust crate: synapse-idl
│   └── src/
│       ├── lib.rs          # parse(), compile(), generate_rust/python/cpp()
│       ├── ast.rs          # Schema AST — Struct, Enum, Channel, Type
│       ├── lexer.rs        # Tokenizer for .bridge source files
│       ├── parser.rs       # Recursive-descent parser
│       ├── layout.rs       # C ABI alignment + struct sizing
│       └── codegen/        # Language-specific code generators
│           ├── rust.rs     # → #[repr(C)] Rust structs
│           ├── python.rs   # → ctypes Python classes
│           └── cpp.rs      # → C++ structs with static_assert
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
├── docs/
│   ├── PHASE2.md           # Phase 2 detailed design
│   └── research/           # Architecture research notes
│
├── DESIGN.md               # Full design specification
├── CONTRIBUTING.md         # How to contribute
└── CHANGELOG.md            # Release history
```

---

## Shared Memory Layout

The bridge is a single named shared memory region divided into three zones:

| Zone | Size | Purpose |
|------|------|---------|
| ControlBlock | 256 bytes | Magic number, version, session token, state, PIDs |
| Ring A→B | `192 + cap × slot_size` bytes | Host → Connector SPSC ring |
| Ring B→A | same | Connector → Host SPSC ring |

**Defaults**: 1024 slots, 256 bytes/slot (252 bytes max payload + 4-byte length prefix).

### ControlBlock

The control block occupies the first 256 bytes of the shared region:

- **magic** (`u64`) — `0x53594E4150534500` (`"SYNAPSE\0"`) — prevents attaching to unrelated shm
- **version** (`u32`) — wire format version, checked on `connect()`
- **session_token** (`u128`, random) — prevents cross-attach between unrelated processes
- **state** (`atomic u32`) — `Init → Ready → Closing → Dead` state machine
- **creator_pid / connector_pid** (`u64`) — process identity tracking
- **heartbeat_a / heartbeat_b** (`atomic u64`) — liveness signals (Phase 2 watchdog)

### Ring Buffer

Each ring is a lock-free SPSC (single producer, single consumer) queue:

- **Power-of-2 capacity** — index masking instead of modulo
- **Monotonic head/tail counters** — never wrap; empty when `head == tail`, full when `head - tail >= capacity`
- **Cacheline isolation** — head and tail live on separate cache lines to prevent false sharing
- **Slot format** — each slot is `[length: u32][payload: u8 × (slot_size - 4)]`

### Naming Convention

| Platform | Kernel object |
|----------|--------------|
| Linux | `/dev/shm/{name}` |
| Windows | `Local\synapse_{name}` |

---

## IDL Schema System (.bridge)

The `.bridge` IDL defines cross-language data types. The compiler produces **byte-for-byte identical C ABI layouts** in Rust, Python (ctypes), and C++.

### Syntax

```
namespace game;

struct Vec3f {
    x: f32,
    y: f32,
    z: f32,
}

struct GameState {
    position: Vec3f,
    velocity: Vec3f,
    health:   f32,
    frame_id: u64,
}

// Tagged enum — [tag: u32][padding][payload: max variant size]
enum Command {
    MoveTo  { target: Vec3f },
    Attack  { target_id: u32, weapon_id: u32 },
    Idle,
}

// Channel — binds named directional streams to types
channel game_bridge {
    host_to_client: GameState,
    client_to_host: Command,
}
```

**Primitives**: `u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 bool`  
**Composite**: `struct`, `enum` (tagged union), fixed-size arrays `[T; N]`

### Compiler Pipeline

```
.bridge source
    │
    ▼ Lexer       — keywords, identifiers, literals, punctuation, // comments
    ▼ Parser      — recursive-descent → Schema AST
    ▼ Layout      — C ABI offsets, sizes, alignment (natural alignment rules)
    ▼ Codegen     — Rust (#[repr(C)]) / Python (ctypes) / C++ (static_assert)
```

### Layout Rules

| Type | Size | Align |
|------|------|-------|
| `u8`, `i8`, `bool` | 1 | 1 |
| `u16`, `i16` | 2 | 2 |
| `u32`, `i32`, `f32` | 4 | 4 |
| `u64`, `i64`, `f64` | 8 | 8 |
| `[T; N]` | `size(T) × N` | `align(T)` |
| `struct S` | Σ fields + trailing padding | max field align |
| `enum E` | `4 + pad + max payload` | `max(4, max payload align)` |

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

### Python bindings (PyO3)

```bash
pip install maturin
cd bindings/python
maturin develop
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

## Status & Roadmap

### ✅ Phase 1 — Core Runtime
Cross-platform shared memory, lock-free SPSC rings, control block state machine, Rust/Python/C++ bindings, 12 tests passing.

### ✅ Phase 1.5 — IDL Schema System
`.bridge` format, lexer/parser/layout engine, Rust + Python + C++ codegen with identical C ABI layouts.

### 🔲 Phase 2 — Schema-Driven Channels
Zero-copy typed channels, Latest-Value Slots (seqlock), adaptive wait (spin → futex), `synapse compile` CLI, multi-channel registry, hot-reload, benchmarking suite, graceful shutdown protocol.

See [docs/PHASE2.md](docs/PHASE2.md) for the full Phase 2 design.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT
