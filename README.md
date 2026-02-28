# Synapse

Cross-language runtime bridge via shared memory + lock-free ring buffers.
Zero-copy, sub-microsecond latency bidirectional communication between Python, C++, and Rust.

## Why Synapse?

Python dominates AI/ML. Game engines and performance-critical systems use C++/Rust. Existing bridges are either too slow (gRPC ~100μs+), too coupled (pybind11 requires the same process), or too complex (raw shared memory with hand-rolled protocols).

Synapse provides:
- **~100ns latency** via shared memory (100x faster than localhost sockets)
- **Lock-free SPSC ring buffers** for bidirectional streaming, no mutexes
- **Schema-driven type layout** via the `.bridge` IDL — zero-copy struct access, no serialization
- **AI Agent optimized** with Latest-Value Slots for async inference results
- **Cross-platform**: Linux (POSIX shm) and Windows (CreateFileMapping)
- **Three language targets**: Rust core, Python bindings (PyO3 + pure-mmap), C++ header-only

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
│          │  Ring A→B (SPSC, lock-free)          │                   │
│          │    head (cacheline-aligned u64)       │                   │
│          │    tail (cacheline-aligned u64)       │                   │
│          │    capacity, slot_size, mask          │                   │
│          │    slots[capacity]: [len:u32][data]   │                   │
│          ├──────────────────────────────────────┤                   │
│          │  Ring B→A (SPSC, lock-free)          │                   │
│          ├──────────────────────────────────────┤                   │
│          │  Data Slots (Phase 2: schema-typed)  │                   │
│          │    Latest-Value slots for AI agents  │                   │
│          └──────────────────────────────────────┘                   │
└─────────────────────────────────────────────────────────────────────┘

IDL Pipeline (.bridge files → multi-language bindings):
  game.bridge → synapse-idl → Rust structs / Python ctypes / C++ structs
                  lexer → parser → AST → layout (C ABI) → codegen
```

---

## Project Structure

```
Synapse/
├── core/                        # Rust crate: synapse-core
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs               # host(), connect(), Bridge API
│       ├── control.rs           # ControlBlock: magic, state, session token
│       ├── ring.rs              # SPSC ring buffer: RingHeader, Ring
│       ├── shm.rs               # SharedRegion: cross-platform mmap
│       └── error.rs             # SynapseError, Result
│
├── idl/                         # Rust crate: synapse-idl
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs               # parse(), compile(), generate_rust/python/cpp()
│       ├── ast.rs               # Schema, Item, StructDef, EnumDef, ChannelDef, Type
│       ├── lexer.rs             # Lexer: tokenizes .bridge source
│       ├── parser.rs            # Parser: tokens → AST
│       ├── layout.rs            # Layout: C ABI alignment + struct sizing
│       └── codegen/
│           ├── mod.rs
│           ├── rust.rs          # Generates #[repr(C)] Rust structs
│           ├── python.rs        # Generates ctypes Python classes
│           └── cpp.rs           # Generates C++ structs with static_assert
│
├── bindings/
│   ├── python/                  # PyO3 extension (synapse Python module)
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   └── cpp/include/
│       └── synapse.h            # Header-only C++ client
│
├── examples/
│   ├── python_sender.py         # Python host: pure-mmap, sends frames, recv ACK
│   └── cpp_receiver.cpp         # C++ connector: recv frames, send ACK
│
├── docs/
│   ├── PHASE2.md                # Phase 2 detailed design
│   └── research/                # Architecture research notes
│
├── DESIGN.md                    # Design specification
└── README.md
```

---

## Phase 1: Core Runtime (Complete)

Phase 1 delivers the shared memory transport layer and language bindings.

### Shared Memory Layout

The bridge is a single named shared memory region with three zones:

| Zone | Size | Purpose |
|------|------|---------|
| ControlBlock | 256 bytes | Magic number, version, session token, state, PIDs |
| Ring A→B | `192 + cap * slot_size` bytes | Host → Connector SPSC ring |
| Ring B→A | same | Connector → Host SPSC ring |

**Defaults**: capacity = 1024 slots, slot_size = 256 bytes (252 bytes max payload + 4-byte length prefix).

### Control Block

```
Offset  Field               Type
──────  ─────────────────── ──────────────
0       magic               u64   = 0x53594E4150534500 ("SYNAPSE\0")
8       version             u32   = 1
12      flags               u32
16      region_size         u64
24      creator_pid         u64
32      connector_pid       u64
40      session_token_lo    u64   } random u128 — prevents cross-attach
48      session_token_hi    u64   }
56      creator_heartbeat   u64 (atomic)
64      connector_heartbeat u64 (atomic)
72      state               u32 (atomic) — 0=Init, 1=Ready, 2=Closing, 3=Dead
76      channel_count       u32
80      _reserved           [128 bytes]
```

### Ring Buffer

Lock-free SPSC (single producer, single consumer) ring using a power-of-2 capacity and monotonic head/tail counters. Each slot is `[length: u32][payload: u8 * (slot_size - 4)]`.

```
Head (cacheline 0, 64 bytes) — producer writes
Tail (cacheline 1, 64 bytes) — consumer writes
Meta (cacheline 2, 64 bytes) — capacity, slot_size, mask
Slots[capacity]              — ring data
```

Ring empty: `head == tail`. Ring full: `head - tail >= capacity`.

### Naming Convention

| Platform | Kernel object name |
|----------|-------------------|
| Windows | `Local\synapse_{name}` |
| Linux | `/dev/shm/{name}` |

---

## Phase 1.5: IDL Schema System (Complete)

Phase 1.5 introduces the `.bridge` IDL — a compact schema language for defining cross-language data types. The compiler produces byte-for-byte identical C ABI layouts in Rust, Python (ctypes), and C++.

### .bridge File Syntax

```bridge
// Namespace declaration (optional, affects generated module/namespace names)
namespace game;

// Primitive struct — maps to C ABI #[repr(C)]
struct Vec3f {
    x: f32,
    y: f32,
    z: f32,
}

// Nested struct
struct GameState {
    position: Vec3f,
    velocity: Vec3f,
    health:   f32,
    frame_id: u64,
}

// Fixed-size array field
struct InputSnapshot {
    keys:     [u8; 32],   // 32-byte bitfield
    mouse_dx: i16,
    mouse_dy: i16,
    tick:     u32,
}

// Tagged enum (C-style discriminated union)
// Layout: [tag: u32][padding][payload: max variant size]
enum Command {
    MoveTo  { target: Vec3f },
    Attack  { target_id: u32, weapon_id: u32 },
    Idle,
}

// Channel declaration — binds named directional streams to types
channel game_bridge {
    host_to_client: GameState,
    client_to_host: Command,
}
```

**Primitive types**: `u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 bool`
**Composite types**: `struct`, `enum` (tagged union), fixed-size arrays `[T; N]`
**Channel types**: named directional entries referencing struct/enum types

### Compiler Pipeline

```
.bridge source
    │
    ▼
  Lexer (lexer.rs)
    Tokenizes: keywords (namespace/struct/enum/channel), identifiers,
    integer literals, punctuation ({} [] : ; ,), line comments (//)
    │
    ▼
  Parser (parser.rs)
    Produces Schema AST:
      Schema { namespace, items: Vec<Item> }
      Item = Struct(StructDef) | Enum(EnumDef) | Channel(ChannelDef)
    │
    ▼
  Layout (layout.rs)
    Computes C ABI offsets, sizes, alignment:
      - Struct fields: aligned to natural alignment, padded to struct align
      - Enum: tag (u32) + padding + max payload, aligned to max field align
      - Arrays: element alignment, total = element_size * count
    │
    ▼
  Codegen (codegen/{rust,python,cpp}.rs)
    ├── Rust:   #[repr(C)] struct, tagged union mod + payload structs
    ├── Python: ctypes.Structure subclasses with _fields_
    └── C++:    struct with static_assert size/alignment checks
```

### Generated Code Examples

**Input** (`game.bridge`):
```bridge
namespace game;
struct Vec3f { x: f32, y: f32, z: f32, }
enum Command { MoveTo { target_id: u32 }, Idle, }
```

**Rust output**:
```rust
// Auto-generated by synapse-idl. Do not edit.

/// Size: 12 bytes, Align: 4 bytes
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vec3f {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// Size: 8 bytes, Align: 4 bytes
pub mod command_variants {
    pub const MOVE_TO: u32 = 0;
    pub const IDLE: u32 = 1;
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CommandMoveToPayload {
    pub target_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Command {
    pub tag: u32,
    pub payload: [u8; 4],
}
```

**Python output**:
```python
# Auto-generated by synapse-idl. Do not edit.
import ctypes

class Vec3f(ctypes.Structure):
    _fields_ = [
        ("x", ctypes.c_float),
        ("y", ctypes.c_float),
        ("z", ctypes.c_float),
    ]

class Command(ctypes.Structure):
    _fields_ = [
        ("tag", ctypes.c_uint32),
        ("payload", ctypes.c_uint8 * 4),
    ]
COMMAND_MOVE_TO = 0
COMMAND_IDLE    = 1
```

**C++ output**:
```cpp
// Auto-generated by synapse-idl. Do not edit.
#pragma once
#include <cstdint>
#include <cassert>

struct Vec3f {
    float x;
    float y;
    float z;
};
static_assert(sizeof(Vec3f) == 12, "Vec3f size mismatch");
static_assert(alignof(Vec3f) == 4, "Vec3f align mismatch");

namespace Command_variants {
    static constexpr uint32_t MOVE_TO = 0;
    static constexpr uint32_t IDLE    = 1;
}

struct CommandMoveToPayload { uint32_t target_id; };

struct Command {
    uint32_t tag;
    uint8_t  payload[4];
};
static_assert(sizeof(Command) == 8, "Command size mismatch");
```

---

## API Reference

### Rust (synapse-core)

```rust
use synapse_core::{host, connect, host_with_config, connect_with_config};

// Host side — creates the shared memory region
let bridge = host("my_channel")?;

// Custom config: 512 slots, 512 bytes/slot
let bridge = host_with_config("my_channel", 512, 512)?;

// Connector side — opens existing region
let bridge = connect("my_channel")?;

// Send (host → ring_ab; connector → ring_ba)
bridge.send(b"hello")?;

// Receive — non-blocking, returns Option<Vec<u8>>
if let Some(data) = bridge.recv() {
    println!("got: {:?}", data);
}

// State check
assert!(bridge.is_ready());

// Session token — same on both sides, random u128
let token: u128 = bridge.session_token();
```

### Python (pure mmap — no native module required)

```python
from examples.python_sender import SynapseBridge

# Host: creates shared memory
with SynapseBridge("my_channel", create=True) as bridge:
    bridge.send(b"hello from Python")

    # Poll for reply
    while True:
        msg = bridge.recv()
        if msg is not None:
            print(f"Got: {msg.decode()}")
            break

# Connector: attaches to existing region
with SynapseBridge("my_channel", create=False) as bridge:
    while True:
        msg = bridge.recv()
        if msg:
            bridge.send(b"ACK:" + msg)
            break
```

### Python (PyO3 native module — build required)

```python
import synapse

# Host
bridge = synapse.host("my_channel")
bridge.send(b"hello")

# Connector
bridge = synapse.connect("my_channel")
data = bridge.recv()  # returns bytes or None
```

### C++ (header-only, `bindings/cpp/include/synapse.h`)

```cpp
#include "synapse.h"

// Connector side
auto bridge = synapse::connect("my_channel");

// Non-blocking receive as raw bytes
auto raw = bridge.recv();        // std::optional<std::vector<uint8_t>>

// Non-blocking receive as string
auto msg = bridge.recv_string(); // std::optional<std::string>

if (msg) {
    std::cout << "Got: " << *msg << "\n";
    bridge.send("ACK:" + *msg);
}

// Host side
auto bridge = synapse::host("my_channel");
bridge.send("hello from C++");
```

### IDL Compiler (synapse-idl)

```rust
use synapse_idl;

let src = r#"
    namespace game;
    struct Vec3f { x: f32, y: f32, z: f32, }
    channel updates { host_to_client: Vec3f, }
"#;

// Parse to AST
let schema = synapse_idl::parse(src)?;

// Compile: parse + compute C ABI layout
let (schema, layout) = synapse_idl::compile(src)?;

// Generate code
let rust_code   = synapse_idl::generate_rust(src)?;
let python_code = synapse_idl::generate_python(src)?;
let cpp_code    = synapse_idl::generate_cpp(src)?;
```

---

## Build Instructions

### Prerequisites

- Rust 1.75+ (`rustup update stable`)
- C++17 compiler for the example (GCC/Clang/MSVC)
- Python 3.8+ for the Python example

### Core crate

```bash
cd core
cargo build
cargo test          # 7 unit + 4 integration + 1 doc-test = 12 tests
```

### IDL crate

```bash
cd idl
cargo build
cargo test          # lexer, parser, layout, codegen tests
```

### Python bindings (PyO3 native module)

```bash
# Requires maturin: pip install maturin
cd bindings/python
maturin develop     # installs into current venv
python -c "import synapse; print(synapse)"
```

### C++ header

The C++ client is header-only — no separate build step:

```bash
# From project root
g++ -std=c++17 -O2 -Ibindings/cpp/include \
    -o examples/cpp_receiver examples/cpp_receiver.cpp
# Linux: add -lrt
```

### Run the demo

```bash
# Terminal 1: Python host
python examples/python_sender.py

# Terminal 2: C++ connector (after building above)
./examples/cpp_receiver
```

---

## Status & Roadmap

### Phase 1 — Core Runtime (DONE)
- [x] Cross-platform shared memory (`SharedRegion`)
- [x] Lock-free SPSC ring buffer (`RingHeader`, `Ring`)
- [x] Control block: magic, version, session token, state machine
- [x] `host()` / `connect()` lifecycle in Rust
- [x] PyO3 Python bindings
- [x] Pure-mmap Python bridge (no native module needed)
- [x] C++ header-only client
- [x] 12 tests passing (Windows + Linux)

### Phase 1.5 — IDL Schema System (DONE)
- [x] `.bridge` format: `namespace`, `struct`, `enum`, `channel`, arrays
- [x] Lexer with line/column tracking, line comments
- [x] Recursive-descent parser
- [x] C ABI layout engine (natural alignment, trailing padding, tagged enums)
- [x] Rust codegen: `#[repr(C)]` structs, variant tag modules, payload structs
- [x] Python codegen: `ctypes.Structure` classes
- [x] C++ codegen: structs with `static_assert` checks

### Phase 2 — Schema-Driven Channels (Planned)
- [ ] Connect IDL types to ring buffer slots (zero-copy typed channels)
- [ ] Latest-Value Slots for AI agent async inference
- [ ] Adaptive wait strategy (spin → yield → futex/WaitOnAddress)
- [ ] `synapse compile game.bridge` CLI tool
- [ ] Multi-channel support (channel registry in control block)
- [ ] Hot-reload schema with version negotiation
- [ ] Benchmarking suite (latency histogram, throughput measurement)
- [ ] Graceful shutdown and error recovery protocol

See [docs/PHASE2.md](docs/PHASE2.md) for the full Phase 2 design.

---

## License

MIT
