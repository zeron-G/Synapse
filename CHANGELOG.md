# Changelog

All notable changes to Synapse are documented here.  
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Unreleased]

### Planned (Phase 2)
- Zero-copy typed channels connected to IDL types
- Latest-Value Slots (seqlock-based) for AI async inference
- Adaptive wait strategy: spin → yield → futex / WaitOnAddress
- `synapse compile` CLI tool for `.bridge` → Rust/Python/C++
- Multi-channel support (channel registry in control block)
- Hot-reload schema with version negotiation
- Cross-process benchmarking suite (latency histogram, throughput)
- Graceful shutdown and peer-death detection protocol

---

## [0.1.5] — 2026-02-28

### Added — Phase 1.5: IDL Schema System
- `.bridge` IDL format: `namespace`, `struct`, `enum`, `channel`, fixed-size arrays `[T; N]`
- Lexer with line/column tracking and `//` line comments
- Recursive-descent parser → `Schema` AST
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
- `ControlBlock`: magic number, version, random session token (u128), state machine (`Init → Ready → Closing → Dead`), PID tracking, heartbeat fields
- `host()` / `connect()` lifecycle in Rust (`synapse-core` crate)
- PyO3 Python bindings (`synapse` native module)
- Pure-mmap Python bridge — no native module required, matches Rust wire format exactly
- C++ header-only client (`bindings/cpp/include/synapse.h`)
- End-to-end demo: Python AI host ↔ C++ game loop connector
- 12 tests passing: 7 unit + 4 integration + 1 doc-test
- Validated on Linux and Windows
