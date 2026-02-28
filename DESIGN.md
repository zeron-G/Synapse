# Synapse Design Specification

## Overview
Synapse is a cross-language runtime bridge using shared memory + lock-free ring buffers.
Target: Python <-> C++/Rust with <1us latency, zero-copy data access.

---

## Phase 1: Core Runtime

### Architecture

```
Python Process <-> [Shared Memory Region] <-> C++/Rust Process
                  Synapse Runtime (Rust)

Shared Memory Layout:
+--------------------------------------+
| Control Block (256 bytes)            |
|   magic: u64 = 0x53594E4150534500    |
|   version: u32                       |
|   session_token: u128 (random UUID)  |
|   channel_count: u32                 |
|   heartbeat_a/b: u64                 |
|   state: u32 (init/ready/shutdown)   |
+--------------------------------------+
| Channel Registry (variable)          |
+--------------------------------------+
| Ring Buffer A->B (SPSC, lock-free)   |
|   head: u64 (cacheline aligned)      |
|   tail: u64 (cacheline aligned)      |
|   slots[capacity]: [len:u32][data]   |
+--------------------------------------+
| Ring Buffer B->A (SPSC)              |
+--------------------------------------+
| Latest-Value Slots (seqlock)         |
+--------------------------------------+
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

5. Working demo: Python AI <-> C++ game loop

### Key Decisions
- SPSC not MPMC (simpler, faster, use N channels for N endpoints)
- Rust core -> PyO3 + header-only C++
- Session token (u128) prevents cross-attach
- Adaptive wait: spin -> yield -> futex (Phase 2)

### Status: Complete
- 12 tests passing (7 unit + 4 integration + 1 doc-test)
- Windows and Linux validated

---

## Phase 1.5: IDL Schema System

### Overview

The `.bridge` IDL provides a compact schema language for defining cross-language data types. The compiler (`synapse-idl` crate) produces byte-for-byte identical C ABI layouts in Rust, Python (ctypes), and C++.

### Scope (Complete)

1. **Lexer** (`idl/src/lexer.rs`)
   - Tokens: keywords, identifiers, integer literals, punctuation
   - Line comments (`//`)
   - Line/column tracking for error messages

2. **Parser** (`idl/src/parser.rs`)
   - Recursive-descent -> Schema AST
   - Field types: primitives, named types, fixed-size arrays `[T; N]`

3. **Layout Engine** (`idl/src/layout.rs`)
   - C ABI alignment rules (natural alignment)
   - Struct, enum (tagged union), array layout computation

4. **Code Generators** (`idl/src/codegen/`)
   - Rust: `#[repr(C)]` structs with derive macros
   - Python: `ctypes.Structure` subclasses
   - C++: structs with `static_assert` checks

5. **CLI Tool** (`idl/src/bin/synapse.rs`)
   - `synapse compile game.bridge --lang rust python cpp --output dir/`

### Status: Complete

---

## Phase 2: Schema-Driven Channels

### Overview

Phase 2 elevates Synapse from a raw byte transport into a schema-driven, AI-optimized inter-process bridge.

### Features (All Complete)

#### 1. Typed Channels (`core/src/typed_channel.rs`)
- `TypedChannel<T>` binds `#[repr(C)]` types to ring buffer slots
- `ChannelRegistry` maps channel names to ring offsets (up to 64 channels)
- Zero-copy: values written directly into ring slots as raw bytes
- `compute_multi_channel_size()` and `compute_channel_offsets()` for layout

#### 2. Latest-Value Slots (`core/src/latest_slot.rs`)
- `LatestSlot<T>` — single-writer, multi-reader seqlock slot
- Writer: increment seq (odd), write data, increment seq (even)
- Reader: wait-free reads with bounded retries
- For AI async inference where only the latest result matters

#### 3. Adaptive Wait Strategy (`core/src/wait.rs`)
- `WaitStrategy::Spin` — pure spin loop (lowest latency)
- `WaitStrategy::Yield` — yield to OS scheduler
- `WaitStrategy::Park` — OS futex/WaitOnAddress immediately
- `WaitStrategy::Adaptive { spin_count, yield_count }` — three-phase progression
- Platform support: Linux futex, Windows WaitOnAddress, macOS fallback

#### 4. Graceful Shutdown (`core/src/shutdown.rs`)
- `Watchdog` — monitors peer heartbeats, detects death after N missed beats
- `ShutdownProtocol` — graceful shutdown: Closing -> drain -> Dead
- `is_process_alive()` — cross-platform PID liveness check
- `can_reclaim_stale_region()` — detect and reclaim abandoned shm

#### 5. Benchmark Suite (`core/benches/`)
- Criterion benchmarks: ring push/pop, throughput, LVS write/read
- Cross-process RTT latency with percentile histogram
- Baseline comparisons: Unix domain socket, TCP loopback
- `cargo bench` integration

#### 6. Integration Tests (`core/tests/phase2_integration_test.rs`)
- All Phase 2 features tested together
- Concurrent typed channels with multiple readers/writers
- LVS under sustained write pressure with multi-reader contention
- Full lifecycle: create -> use -> shutdown

### Layout Rules

| Type | Size | Align |
|------|------|-------|
| u8, i8, bool | 1 | 1 |
| u16, i16 | 2 | 2 |
| u32, i32, f32 | 4 | 4 |
| u64, i64, f64 | 8 | 8 |
| [T; N] | size(T) * N | align(T) |
| struct S | sum of fields + padding | max field align |
| enum E | 4 + pad + max payload | max(4, max payload align) |

### Status: Complete

See [docs/PHASE2.md](docs/PHASE2.md) for the full Phase 2 design document.
