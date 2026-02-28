# Synapse Design Specification

## Overview
Synapse is a cross-language runtime bridge using shared memory + lock-free ring buffers.
Target: Python ↔ C++/Rust with <1μs latency, zero-copy data access.

---

## Phase 1: Core Runtime

### Architecture

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

### Scope (Complete)

**Must Have**
1. Rust core library (synapse-core crate)
   - Cross-platform shared memory (Linux mmap + Windows CreateFileMapping)
   - Lock-free SPSC ring buffer (power-of-2 cap, cacheline-aligned)
   - Control block with magic + session token
   - Host/connect lifecycle

2. Python bindings (PyO3)
   - synapse.host(name) / synapse.connect(name)
   - bridge.send(bytes) / bridge.recv() -> bytes
   - GIL released during shm ops

3. Python pure-mmap bridge (no native module required)
   - SynapseBridge class using raw mmap
   - Matches Rust wire format exactly

4. C++ header-only client (synapse.h)
   - Same API pattern
   - No deps beyond OS headers

5. Working demo: Python AI ↔ C++ game loop

### Key Decisions
- SPSC not MPMC (simpler, faster, use N channels for N endpoints)
- Rust core → PyO3 + header-only C++
- Session token (u128) prevents cross-attach
- Adaptive wait: spin → yield → futex (planned Phase 2)

### Status: Complete
- 12 tests passing (7 unit + 4 integration + 1 doc-test)
- Windows and Linux validated

### Bug Fixes (from review)
- slot_ptr must use index * slot_size offset (not always slot[0])
- Python recv must read ring_ba (B→A), not ring_ab

---

## Phase 1.5: IDL Schema System

### Overview

The `.bridge` IDL provides a compact schema language for defining cross-language data types. The compiler (`synapse-idl` crate) produces byte-for-byte identical C ABI layouts in Rust, Python (ctypes), and C++.

### Scope (Complete)

1. **Lexer** (`idl/src/lexer.rs`)
   - Tokens: keywords (`namespace`, `struct`, `enum`, `channel`), identifiers, integer literals, punctuation (`{}[],:;`)
   - Line comments (`//`)
   - Line/column tracking for error messages

2. **Parser** (`idl/src/parser.rs`)
   - Recursive-descent
   - Produces `Schema { namespace, items: Vec<Item> }`
   - `Item` = `Struct(StructDef)` | `Enum(EnumDef)` | `Channel(ChannelDef)`
   - Field types: primitives, named types, fixed-size arrays `[T; N]`

3. **Layout Engine** (`idl/src/layout.rs`)
   - C ABI alignment rules (natural alignment, no `#[packed]`)
   - Struct: fields aligned to their natural alignment; struct size padded to max field alignment
   - Enum: `[tag: u32][padding to payload align][payload: max variant size]`
   - Arrays: element alignment, total size = element_size × count
   - Nested structs: resolved in declaration order (no forward references within same file)

4. **Code Generators** (`idl/src/codegen/`)
   - **Rust** (`rust.rs`): `#[repr(C)] #[derive(Debug, Clone, Copy)]` structs; enum tag constant modules; payload structs per variant; tagged union struct
   - **Python** (`python.rs`): `ctypes.Structure` subclasses with `_fields_`; enum tag integer constants
   - **C++** (`cpp.rs`): bare structs with `static_assert` size and alignment checks; variant constant namespaces

5. **Public API** (`idl/src/lib.rs`)
   - `parse(src) -> Result<Schema>`
   - `compile(src) -> Result<(Schema, SchemaLayout)>`
   - `generate_rust(src) -> Result<String>`
   - `generate_python(src) -> Result<String>`
   - `generate_cpp(src) -> Result<String>`

### .bridge Grammar

```
schema    ::= namespace? item*
namespace ::= "namespace" IDENT ";"
item      ::= struct | enum | channel
struct    ::= "struct" IDENT "{" (field ","?)* "}"
enum      ::= "enum" IDENT "{" (variant ","?)* "}"
channel   ::= "channel" IDENT "{" (entry ","?)* "}"
field     ::= IDENT ":" type
variant   ::= IDENT ("{" (field ","?)* "}")?
entry     ::= IDENT ":" IDENT
type      ::= "[" type ";" INT "]"    // array
            | "u8" | "u16" | "u32" | "u64"
            | "i8" | "i16" | "i32" | "i64"
            | "f32" | "f64" | "bool"
            | IDENT                    // named type
```

### Example

```bridge
namespace game;

struct Vec3f { x: f32, y: f32, z: f32, }

struct GameState {
    position: Vec3f,
    velocity: Vec3f,
    health:   f32,
    frame_id: u64,
}

enum Command {
    MoveTo { target: Vec3f },
    Attack { target_id: u32, weapon_id: u32 },
    Idle,
}

channel game_bridge {
    host_to_client: GameState,
    client_to_host: Command,
}
```

### Layout Rules (summary)

| Type | Size | Align |
|------|------|-------|
| u8, i8, bool | 1 | 1 |
| u16, i16 | 2 | 2 |
| u32, i32, f32 | 4 | 4 |
| u64, i64, f64 | 8 | 8 |
| [T; N] | size(T) × N | align(T) |
| struct S | Σ fields + padding | max field align |
| enum E | 4 + pad + max payload | max(4, max payload align) |

### Status: Complete

---

## Phase 2: Schema-Driven Channels

See [docs/PHASE2.md](docs/PHASE2.md) for the full design.

### Summary of Phase 2 Goals

| Feature | Description |
|---------|-------------|
| Typed channels | Connect IDL types to ring slots; zero-copy struct access |
| Latest-Value Slots | Seqlock-based; always-fresh value for AI async results |
| Adaptive wait | Spin → yield → futex/WaitOnAddress with configurable thresholds |
| CLI tool | `synapse compile game.bridge` → Rust/Python/C++ files |
| Multi-channel | Channel registry in control block; N rings per bridge |
| Hot-reload | Schema version negotiation; in-process IDL recompile |
| Benchmarking | Criterion + cross-process RTT histogram |
| Error recovery | Heartbeats, peer-death detection, graceful shutdown protocol |
