# 🧠 Synapse

**Cross-language runtime bridge via shared memory + event bus.**

Zero-copy, sub-microsecond latency bidirectional communication between Python and C++/Rust/C#/Go.

## Why Synapse?

Python dominates AI/ML. Game engines and performance-critical systems use C++/Rust/C#. Existing bridges are either too slow (gRPC ~100μs), too coupled (pybind11 same process), or too complex (raw shared memory).

Synapse provides:
- 🚀 **~100ns latency** via shared memory (100x faster than sockets)
- 🔄 **Lock-free SPSC ring buffers** for bidirectional event streaming
- 📐 **Schema-driven zero-copy** type mapping (no serialization overhead)
- 🎮 **AI Agent optimized** with Latest-Value Slots for async inference
- 🌍 **Cross-platform** (Linux/macOS/Windows)

## Quick Start

```python
import synapse

# Python side: connect to game engine
bridge = synapse.connect("my_game")
bridge.send(b"walk_to_market")
response = bridge.recv()
```

```cpp
#include <synapse.h>

// C++ side: host the bridge
auto bridge = synapse::host("my_game");
auto msg = bridge.recv();
bridge.send("ACK:" + msg);
```

## Architecture

```
Python Process ←→ [Shared Memory: Control Block + SPSC Rings + Data Slots] ←→ C++/Rust Process
                              Synapse Runtime (Rust core)
```

## Status

🚧 **Phase 1: Core** — Under active development

## License

MIT
