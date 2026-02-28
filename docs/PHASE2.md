# Synapse Phase 2 Design

Phase 2 elevates Synapse from a raw byte transport into a schema-driven, AI-optimized inter-process bridge. The IDL compiler from Phase 1.5 feeds directly into typed channels, latest-value slots, and a CLI toolchain.

---

## Table of Contents

1. [Schema-Driven Zero-Copy Channels](#1-schema-driven-zero-copy-channels)
2. [Latest-Value Slots](#2-latest-value-slots)
3. [Adaptive Wait Strategy](#3-adaptive-wait-strategy)
4. [CLI Tool: synapse compile](#4-cli-tool-synapse-compile)
5. [Multi-Channel Support](#5-multi-channel-support)
6. [Hot-Reload Schema](#6-hot-reload-schema)
7. [Benchmarking Suite](#7-benchmarking-suite)
8. [Error Recovery and Graceful Shutdown](#8-error-recovery-and-graceful-shutdown)

---

## 1. Schema-Driven Zero-Copy Channels

### Goal

Connect IDL-defined types directly to ring buffer slots so that both sides read and write typed structs without any serialization. A `GameState` pushed by C++ is directly readable from Python as a ctypes struct mapped into the ring slot — no copy, no decode.

### Design

The ring buffer slot format currently is:

```
[length: u32][payload: u8 * (slot_size - 4)]
```

Phase 2 replaces the opaque `u8` payload with a typed view. The slot size is determined by the IDL type's computed C ABI size:

```
slot_size = align_up(4 + sizeof(T), cacheline_size)
```

A `TypedRing<T>` wrapper provides type-safe push/pop that reinterprets the slot pointer directly as `*mut T`:

```
Ring slot layout for struct T (size S, align A):
  [length: u32 = S][_padding to align A][T bytes...][trailing bytes to slot_size]
```

The `channel` declaration in `.bridge` binds a named direction to a type. The compiler emits a `ChannelDescriptor` (name hash, type ID, slot_size) stored in the control block's channel registry.

### API Sketch

**Rust**:
```rust
// Generated from game.bridge
use game_bridge::GameStateChannel;

let ch: TypedChannel<GameState> = bridge.channel::<GameState>("host_to_client")?;
ch.send(&GameState { position: Vec3f { x: 1.0, y: 0.0, z: 0.0 }, .. })?;

// Connector side
let state: Option<GameState> = ch.recv()?;
```

**Python** (using generated ctypes class):
```python
from game_bridge_generated import GameState, GameStateChannel

ch = bridge.channel("host_to_client", GameState)
state = ch.recv()  # returns GameState ctypes instance mapped into shm, zero-copy
if state:
    print(f"pos: {state.position.x:.2f}")
```

**C++**:
```cpp
auto ch = bridge.channel<GameState>("host_to_client");
auto state = ch.recv();  // std::optional<GameState*> — pointer into shm
if (state) {
    std::cout << state->position.x << "\n";
}
```

### Implementation Notes

- `TypedRing<T>` is a zero-sized wrapper over `Ring` with compile-time size checks (`assert_eq!(size_of::<T>(), slot.len - 4)`).
- Python zero-copy: use `ctypes.from_address(slot_ptr + 4)` — no memcpy, the ctypes struct is a view into the mmap.
- C++ zero-copy: `reinterpret_cast<T*>(slot_ptr + offset)` — same direct view.
- The slot pointer remains valid as long as the consumer holds the ring's tail reservation. Phase 2 introduces a "hold" primitive: `ring.peek()` returns a `SlotGuard` that advances tail only on drop.

---

## 2. Latest-Value Slots

### Goal

AI inference pipelines produce results asynchronously — a game may request a pathfinding decision and the AI agent writes the result whenever it's ready. Instead of queuing stale frames in a ring, Latest-Value Slots keep only the most recent value of each type, overwriting previous ones.

This is the canonical pattern for: game state → AI, AI decision → game.

### Design

A Latest-Value Slot (LVS) is a fixed location in the shared memory region after the rings. Each slot holds one instance of a schema type plus a seqlock for lock-free reads:

```
LVS slot layout:
  [seq: u64 (seqlock counter, cache-line aligned)]
  [data: T bytes (size from IDL layout)]
  [_padding to cache-line boundary]
```

**Seqlock protocol**:
- Writer: increment seq (odd = writing), write data, increment seq (even = done).
- Reader: read seq (must be even = no writer), read data, re-read seq (must equal first). Retry on mismatch.

This gives readers wait-free reads in the common case (no concurrent writer), with bounded retries during a write (~nanoseconds).

LVS declarations are added to `.bridge` channel blocks:

```bridge
channel ai_bridge {
    // Streaming ring (high-frequency, ordered)
    input_frames: InputSnapshot,

    // Latest-value slot (async, unordered — only latest matters)
    slot ai_decision: Command,
    slot game_state: GameState,
}
```

### API Sketch

**Rust**:
```rust
// Write (AI agent side)
bridge.slot_write("ai_decision", &Command {
    tag: command_variants::MOVE_TO,
    payload: ...,
})?;

// Read (game side) — wait-free seqlock
let cmd: Option<Command> = bridge.slot_read("ai_decision")?;
```

**Python**:
```python
# Write
bridge.slot_write("ai_decision", cmd)  # cmd is a Command ctypes instance

# Read
cmd = bridge.slot_read("ai_decision", Command)  # returns Command or None
```

**C++**:
```cpp
bridge.slot_write("ai_decision", cmd);
auto cmd = bridge.slot_read<Command>("ai_decision");  // std::optional<Command>
```

### Implementation Notes

- Slots are allocated sequentially after the ring regions. The control block's slot registry maps slot name hash → (offset, size, type_id).
- Seqlock counter must be cache-line-padded to prevent false sharing between the counter and data.
- A memory fence (Release on write, Acquire on read) is required on all platforms. Use `std::sync::atomic::fence(Ordering::AcqRel)`.
- Python implementation uses `ctypes.from_address` + manual seq reads via `struct.unpack_from`.

---

## 3. Adaptive Wait Strategy

### Goal

Eliminate unnecessary CPU waste when the ring or LVS has no data, while keeping latency minimal when data arrives. The naive `spin { recv() }` loop wastes a core; `sleep(1ms)` adds latency.

### Design

Three-phase adaptive wait, configurable per channel:

```
Phase 1 — Spin:   busy-poll for N iterations (default: 200)
             └─ For sub-microsecond latency on hot paths
Phase 2 — Yield:  std::hint::spin_loop() for M iterations (default: 500)
             └─ Yields the hyperthread, keeps core hot
Phase 3 — Park:   futex_wait / WaitOnAddress with timeout T (default: 1ms)
             └─ OS-level sleep, woken by sender
```

The sender signals the OS waiter via `futex_wake` / `WakeByAddressSingle` after each push. The wake address is the ring's head pointer (already cache-line aligned).

```
Sender:                          Receiver (blocked in phase 3):
  ring.push(data)                  WaitOnAddress(head, old_head, 1ms)
  futex_wake(head_ptr, 1)     →    wakes up, retries pop
```

### API Sketch

```rust
// Blocking receive with adaptive wait
let data: Vec<u8> = bridge.recv_wait(WaitConfig {
    spin_iters: 200,
    yield_iters: 500,
    park_timeout: Duration::from_millis(1),
})?;

// Or use the default wait config
let data = bridge.recv_blocking()?;

// Named channel typed version
let state: GameState = ch.recv_wait(WaitConfig::default())?;
```

**Python**:
```python
# Blocking recv with default adaptive wait
data = bridge.recv_blocking(timeout_ms=100)

# LVS slot blocking read
cmd = bridge.slot_read_blocking("ai_decision", Command, timeout_ms=50)
```

### Implementation Notes

- Phase 1 and 2 are pure Rust (`std::hint::spin_loop()`).
- Phase 3 on Linux: `syscall(SYS_futex, head_ptr, FUTEX_WAIT, old_val, timeout, NULL, 0)`.
- Phase 3 on Windows: `WaitOnAddress(head_ptr, &old_val, 8, timeout_ms)` + `WakeByAddressSingle(head_ptr)`.
- The wait address (ring head) must be naturally aligned (already true: cacheline-aligned u64).
- `WaitConfig` is passed by value, stored in `TypedChannel` for blocking variants.
- Python exposes this via a `timeout_ms` kwarg on `recv_blocking`; the actual futex is called in Rust through the PyO3 binding (GIL released during park phase).

---

## 4. CLI Tool: synapse compile

### Goal

A standalone `synapse` binary that compiles `.bridge` schema files into source code for all three targets, runnable from build scripts, `build.rs`, and CI pipelines.

### Design

```
synapse compile [OPTIONS] <FILE.bridge>

Options:
  --lang rust|python|cpp|all   Target language(s) [default: all]
  --out <DIR>                  Output directory [default: same as input]
  --check                      Validate only, no output (for CI)
  --print                      Print to stdout instead of files

Output files (with --lang all):
  {name}_bridge.rs             Rust bindings
  {name}_bridge.py             Python ctypes bindings
  {name}_bridge.hpp            C++ bindings
```

**Example**:
```bash
synapse compile game.bridge --out src/generated/

# In Cargo build.rs:
fn build() {
    synapse_idl::build_script::compile("schemas/game.bridge", "src/generated/");
    println!("cargo:rerun-if-changed=schemas/game.bridge");
}
```

### API Sketch

The `synapse-idl` crate exposes a `build_script` module:

```rust
// build.rs
fn main() {
    synapse_idl::build_script::compile_all(
        "schemas/",          // input directory
        "src/generated/",    // output directory
    ).unwrap();
}
```

### Implementation Notes

- The CLI binary lives in a new `cli/` crate with `synapse-idl` as a dependency.
- Error messages include file name, line, and column: `game.bridge:12:5: unknown type 'Vec4f'`.
- `--check` mode returns exit code 1 on parse/layout errors, enabling `pre-commit` hooks.
- `build_script::compile_all` walks a directory for `*.bridge` files and compiles each.
- Generated files include a checksum comment so the build can skip unchanged schemas:
  `// source-hash: sha256:abc123... — regenerate with: synapse compile game.bridge`

---

## 5. Multi-Channel Support

### Goal

A single shared memory region hosts multiple independent typed channels, each with its own ring pair. This avoids creating separate shm regions for each data stream and allows a single bridge to carry game state, AI decisions, debug telemetry, etc.

### Design

The control block's channel registry (currently a stub `channel_count: u32`) becomes a proper table stored immediately after the ControlBlock:

```
ControlBlock (256 bytes)
  channel_count: u32 = N
  └──→ ChannelRegistry (N * sizeof(ChannelDescriptor) bytes)

ChannelDescriptor (64 bytes, cache-line aligned):
  name_hash:   u64      — FNV-1a hash of channel name
  type_id:     u64      — hash of IDL type name
  offset:      u64      — byte offset of ring_ab from region base
  slot_size:   u32      — bytes per slot
  capacity:    u32      — ring capacity
  flags:       u32      — bit 0 = LVS slot, bit 1 = adaptive wait enabled
  _pad:        [28 bytes]
```

Lookup is O(N) linear scan over the registry — acceptable for N < 64 channels. For larger N, a hash table in the registry would replace the linear scan.

### API Sketch

**Bridge creation with multiple channels**:
```rust
let bridge = host_with_channels("game", &[
    ChannelSpec::ring("game_state",   size_of::<GameState>(),  1024),
    ChannelSpec::ring("commands",     size_of::<Command>(),    256),
    ChannelSpec::slot("ai_decision",  size_of::<Command>()),
])?;

let state_ch  = bridge.channel::<GameState>("game_state")?;
let cmd_ch    = bridge.channel::<Command>("commands")?;
```

**Dynamic discovery** (connector side):
```rust
let bridge = connect("game")?;
for desc in bridge.channels() {
    println!("channel '{}': {} slots × {} bytes", desc.name, desc.capacity, desc.slot_size);
}
```

### Implementation Notes

- Total region size is computed upfront from the channel list; the registry is written during `host()` before setting state to `Ready`.
- Connector validates each channel's `type_id` against its expected type. A mismatch returns `SynapseError::TypeMismatch { channel, expected, found }`.
- Adding channels after bridge creation requires version negotiation (see section 6).
- Channel names are limited to 63 bytes (null-terminated, stored in a separate string table at the end of the registry).

---

## 6. Hot-Reload Schema

### Goal

Allow updating `.bridge` schemas without restarting both sides. An AI agent deployed in a long-running process can receive schema updates from the game engine, renegotiate types, and continue operating.

### Design

Version negotiation occurs at connect time and optionally at runtime:

**Control block additions**:
```
schema_version:  u32   — incremented on each schema change
schema_hash:     u64   — FNV-1a of the .bridge source
schema_offset:   u64   — offset of embedded schema text (optional)
schema_size:     u32   — byte length of embedded schema
```

**Negotiation protocol** (at connect time):
1. Connector reads `schema_hash` from ControlBlock.
2. Connector compares to its compiled-in hash.
3. If equal: proceed normally.
4. If different: connector reads embedded schema text from `schema_offset`, recompiles in-process using `synapse_idl::compile()`, validates layout compatibility.
5. Layout compatible (same field offsets, same sizes): proceed with runtime-generated type accessors.
6. Incompatible (field removed, size changed): error `SynapseError::SchemaIncompatible`.

**Runtime hot-reload** (after initial connect):
- Host increments `schema_version` and writes new schema text to `schema_offset`.
- Connector's background thread polls `schema_version` (one atomic u32 load per tick).
- On version change: re-run negotiation protocol above.
- During renegotiation: existing channels drain; new channels use new schema.

### API Sketch

**Embedding a schema in the bridge**:
```rust
let bridge = host_with_schema("game",
    include_str!("schemas/game.bridge"),
    channels,
)?;
```

**Reload callback** (connector side):
```rust
bridge.on_schema_reload(|new_schema| {
    eprintln!("Schema reloaded: v{}", new_schema.version);
    // Re-bind channels
});
```

### Implementation Notes

- Schema text is stored in a dedicated region after the channel registry, written once at host creation.
- Hot-reload is disabled by default; opt-in via `BridgeConfig::enable_hot_reload(true)`.
- The Rust IDL compiler (`synapse_idl::compile`) is fast enough for in-process use: a 1KB schema compiles in < 1ms.
- Python and C++ connectors that don't support dynamic recompile receive `SchemaIncompatible` and must restart.

---

## 7. Benchmarking Suite

### Goal

Provide reproducible, CI-friendly benchmarks measuring the two critical metrics: round-trip latency (P50/P99/P999) and unidirectional throughput (messages/second, GB/s).

### Design

The benchmark suite lives in `benches/` and uses Criterion.rs for Rust microbenchmarks, plus a standalone `bench_roundtrip` binary for cross-process latency.

**Metrics**:

| Benchmark | Description | Target |
|-----------|-------------|--------|
| `ring_push_pop` | Single-process SPSC push+pop | < 50ns |
| `roundtrip_same_host` | Host↔Connector on same machine | < 500ns P99 |
| `throughput_1kb` | Sustained 1KB message throughput | > 2 GB/s |
| `lvs_write_read` | Latest-Value Slot seqlock cycle | < 20ns |
| `compile_schema` | IDL compile + layout for 1KB schema | < 1ms |

**Latency histogram output**:
```
Ring roundtrip latency (N=100000, payload=64 bytes):
  P50:   142 ns
  P90:   198 ns
  P99:   387 ns
  P999: 1240 ns
  Max:  4821 ns
```

### API Sketch

```bash
# Run all Criterion benchmarks
cd core && cargo bench

# Cross-process roundtrip benchmark (two-process, uses OS timer)
cargo run --bin bench_roundtrip -- --iters 100000 --size 64

# Throughput benchmark
cargo run --bin bench_throughput -- --duration 5s --size 1024
```

**Criterion integration**:
```rust
// benches/ring.rs
fn bench_roundtrip(c: &mut Criterion) {
    let h = host("bench").unwrap();
    let conn = connect("bench").unwrap();

    c.bench_function("ring_roundtrip_64b", |b| {
        b.iter(|| {
            h.send(black_box(b"x" as &[u8] * 64)).unwrap();
            conn.recv().unwrap()
        })
    });
}
```

### Implementation Notes

- True cross-process latency requires `CLOCK_MONOTONIC_RAW` (Linux) / `QueryPerformanceCounter` (Windows) timestamps embedded in the message payload.
- The benchmark binary spawns a child process as the connector; parent acts as host, measures RTT.
- Histogram bins: 50 logarithmically-spaced bins from 50ns to 10ms. Output as text table and as a simple CSV for charting.
- The CI pipeline runs `bench_roundtrip` on every commit and fails if P99 regresses > 20% vs. the stored baseline.

---

## 8. Error Recovery and Graceful Shutdown

### Goal

Handle process crashes, network partitions (future remote transport), and schema mismatches without hanging the surviving side. Both host and connector should detect peer death and either recover or report a clean error.

### Design

**Heartbeat mechanism**:
- Host writes `creator_heartbeat` (ControlBlock offset 56) every 100ms.
- Connector writes `connector_heartbeat` (offset 64) every 100ms.
- Each side checks the peer's heartbeat on every `recv()` call (or on a timer).
- Stale heartbeat (> 500ms without update) → `SynapseError::PeerDead`.

**State machine**:
```
         host calls host()             connector calls connect()
              │                                  │
         State::Init ──────────────────────→ State::Ready
              │  (host sets after init)
              │
         State::Ready  (normal operation)
              │
         State::Closing  (either side signals shutdown)
              │
         State::Dead     (both sides have exited cleanly, or crash detected)
```

**Graceful shutdown sequence**:
1. Either side sets `State::Closing`.
2. Both sides drain their outbound rings (finish in-flight sends).
3. Both sides read until rings are empty.
4. Both sides set `State::Dead`.
5. Host frees the shared memory region.

**Crash recovery**:
- On Linux: the shm file at `/dev/shm/{name}` persists if the host crashes. A new host calling `host()` with the same name detects the stale magic + old PID (via `creator_pid`) and re-initializes the region.
- On Windows: the kernel object is reference-counted and automatically destroyed when all handles are closed (process death closes all handles).

### API Sketch

```rust
// Graceful shutdown
bridge.shutdown(ShutdownMode::Graceful)?;  // drain + State::Dead
bridge.shutdown(ShutdownMode::Immediate)?; // State::Dead immediately

// Error handling
match bridge.recv() {
    Some(data) => process(data),
    None => {}  // empty ring
}

// Heartbeat check (call periodically or on recv)
match bridge.check_peer() {
    Ok(()) => {}
    Err(SynapseError::PeerDead) => { reconnect(); }
    Err(SynapseError::PeerStale { last_seen_ms }) => {
        eprintln!("peer stale for {}ms", last_seen_ms);
    }
}
```

**Python**:
```python
bridge.shutdown()  # sends State::Closing, waits for drain

try:
    data = bridge.recv_blocking(timeout_ms=500)
except synapse.PeerDead:
    print("Peer process died — reconnecting")
    bridge = SynapseBridge(CHANNEL_NAME, create=False)
```

### Implementation Notes

- Heartbeats are written using `AtomicU64::store(Ordering::Release)` and read with `Ordering::Acquire`.
- The heartbeat check on `recv()` adds one atomic load per call — negligible cost.
- For the crash recovery path on Linux, compare `creator_pid` to `/proc/{pid}/status` existence to confirm the creator is actually dead before re-initializing.
- Windows process death detection: `OpenProcess(SYNCHRONIZE, pid)` + `WaitForSingleObject(0ms)` returns `WAIT_OBJECT_0` if the process has exited.
- A `WatchdogThread` background thread (optional, off by default) handles heartbeat writes and peer-death detection without requiring the user to call `check_peer()` manually.
