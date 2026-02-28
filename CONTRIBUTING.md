# Contributing to Synapse

Thanks for your interest! Synapse is a focused low-latency library — contributions that keep it fast, clean, and cross-platform are most welcome.

---

## Getting Started

```bash
git clone https://github.com/zeron-G/Synapse
cd Synapse

# Build & test everything
cd core && cargo test
cd ../idl && cargo test
```

---

## Project Layout

| Directory | Purpose |
|-----------|---------|
| `core/` | Rust runtime: shared memory, ring buffers, control block |
| `idl/` | `.bridge` IDL compiler: lexer → parser → layout → codegen |
| `bindings/python/` | PyO3 native extension |
| `bindings/cpp/include/` | Header-only C++ client |
| `examples/` | End-to-end demos (Python ↔ C++) |
| `docs/` | Phase designs and research notes |
| `benchmarks/` | Latency and throughput benchmarks |

See the Architecture section in [README.md](README.md) for how the pieces fit together.

---

## Development Workflow

### Before opening a PR

1. **Format**: `cargo fmt` in each crate you touched
2. **Lint**: `cargo clippy -- -D warnings` (no new warnings)
3. **Test**: `cargo test` — all tests must pass on Linux and Windows
4. **Cross-platform**: if your change touches shm or ring code, consider both platforms

### Commit style

Use short, imperative subject lines:

```
feat: add seqlock Latest-Value Slot
fix: correct slot_ptr offset calculation
docs: update Phase 2 design
chore: bump Cargo.lock
```

Prefix: `feat` / `fix` / `docs` / `chore` / `refactor` / `test` / `ci`

---

## What to Work On

Check the [issues](https://github.com/zeron-G/Synapse/issues) tab. Good starting points are labeled **`good first issue`**.

Phase 2 items (from [docs/PHASE2.md](docs/PHASE2.md)):
- `synapse compile` CLI tool
- Adaptive wait strategy (spin → yield → futex)
- Latest-Value Slots (seqlock-based)
- Cross-process benchmark suite

---

## Design Principles

- **Zero-copy first** — avoid allocations on the hot path
- **Lock-free** — prefer atomics over mutexes in shared memory
- **C ABI** — IDL-generated types must be byte-identical across all three languages
- **Cross-platform** — Linux + Windows are both first-class
- **No hidden complexity** — keep the ring buffer and control block simple and auditable

---

## Reporting Issues

Use the issue templates:
- **Bug report** — unexpected behavior, panics, or incorrect output
- **Feature request** — new capability or improvement proposal

Please include OS, Rust version, and a minimal reproduction snippet.

---

## License

By contributing, you agree your work will be licensed under the [MIT License](LICENSE).
