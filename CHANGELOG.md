# Changelog

All notable changes to Synapse are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [0.2.0] — 2026-02-28

### Added — Phase 2: Schema-Driven Channels

#### Typed Channels (`core/src/typed_channel.rs`)
- `TypedChannel<T>` — zero-copy typed reads/writes over ring buffers
- `ChannelRegistry` — maps channel names to ring offsets (up to 64 channels)
- `ChannelDescriptor` — runtime channel metadata for dynamic discovery
- `compute_multi_channel_size()` / `compute_channel_offsets()` — layout helpers

#### Latest-Value Slots (`core/src/latest_slot.rs`)
- `LatestSlot<T>` — seqlock-based single-writer, multi-reader slot
- Wait-free reads with bounded retries (1000 max)
- Designed for AI async inference results where only latest value matters

#### Adaptive Wait Strategy (`core/src/wait.rs`)
- `WaitStrategy` enum: `Spin`, `Yield`, `Park`, `Adaptive { spin_count, yield_count }`
- `Waiter` — configurable wait with timeout support
- Platform-specific parking: Linux futex, Windows WaitOnAddress, macOS fallback
- `wake_one()` — wake a single blocked waiter

#### Graceful Shutdown Protocol (`core/src/shutdown.rs`)
- `Watchdog` — peer heartbeat monitoring with configurable missed-beat threshold
- `ShutdownProtocol` — graceful shutdown: signal intent (Closing) -> drain -> cleanup (Dead)
- `is_process_alive()` — cross-platform PID liveness detection
- `can_reclaim_stale_region()` — detect and reclaim abandoned shm regions

#### Benchmark Suite (`core/benches/`)
- Criterion benchmarks: ring push/pop, burst throughput, LVS seqlock cycle
- Bridge round-trip latency at multiple payload sizes (64B, 1KB)
- Unidirectional throughput measurement (64B, 1KB, 4KB)
- Baseline comparisons: Unix domain socket and TCP loopback round-trip
- RTT percentile histogram output (P50/P90/P99/P999)

#### CLI Tool
- `synapse compile` — compile `.bridge` schemas to Rust/Python/C++ code

#### Comprehensive Test Suite
- Cross-process bidirectional message passing tests
- Error path tests: magic mismatch, version mismatch, ring full, data too large
- Python bridge end-to-end tests (pure mmap, wire format validation)
- C++ header compilation and runtime tests
- Phase 2 integration tests: all features working together
- Stress tests: concurrent channels, sustained throughput, LVS contention
- 130+ tests across core and idl crates

---

## [0.1.5] — 2026-02-28

### Added — Phase 1.5: IDL Schema System
- `.bridge` IDL format: `namespace`, `struct`, `enum`, `channel`, fixed-size arrays `[T; N]`
- Lexer with line/column tracking and `//` line comments
- Recursive-descent parser -> `Schema` AST
- C ABI layout engine: natural alignment, struct trailing padding, tagged enum layout
- Rust codegen: `#[repr(C)]` structs, variant tag constants, payload structs
- Python codegen: `ctypes.Structure` subclasses with `_fields_`
- C++ codegen: structs with `static_assert` size and alignment checks
- Public API: `parse()`, `compile()`, `generate_rust/python/cpp()`

---

## [0.1.0] — 2026-02-27

### Added — Phase 1: Core Runtime
- Cross-platform shared memory (`SharedRegion`): Linux POSIX shm + Windows `CreateFileMapping`
- Lock-free SPSC ring buffer (`RingHeader`, `Ring`): power-of-2 capacity, cacheline-aligned head/tail
- `ControlBlock`: magic number, version, random session token (u128), state machine (`Init -> Ready -> Closing -> Dead`), PID tracking, heartbeat fields
- `host()` / `connect()` lifecycle in Rust (`synapse-core` crate)
- PyO3 Python bindings (`synapse` native module)
- Pure-mmap Python bridge — no native module required, matches Rust wire format exactly
- C++ header-only client (`bindings/cpp/include/synapse.h`)
- End-to-end demo: Python AI host <-> C++ game loop connector
- 12 tests passing: 7 unit + 4 integration + 1 doc-test
- Validated on Linux and Windows
