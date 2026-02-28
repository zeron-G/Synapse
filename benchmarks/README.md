# Synapse Benchmarks

Benchmarks measuring Synapse's core performance characteristics.

## Running Benchmarks

```bash
# All Criterion benchmarks (ring, throughput, latency, LVS)
cd core && cargo bench

# Ring buffer microbenchmarks only
cd core && cargo bench --bench ring_bench

# Latency + throughput + baseline comparison
cd core && cargo bench --bench latency_bench
```

Criterion HTML reports are generated in `core/target/criterion/`.

## Benchmark Summary

| Benchmark | Description | Payload | Target |
|-----------|-------------|---------|--------|
| `ring_push_pop` | Single-process SPSC push + pop | 64B | < 50 ns |
| `ring_push_pop` | Single-process SPSC push + pop | 252B | < 80 ns |
| `lvs_write_read` | Seqlock write + read cycle | 32B struct | < 20 ns |
| `bridge_roundtrip` | Host → Connector → Host RTT (same process) | 64B | < 500 ns |
| `bridge_roundtrip` | Host → Connector → Host RTT (same process) | 1KB | < 800 ns |
| `bridge_throughput` | Unidirectional send + recv | 64B | > 2M msg/s |
| `bridge_throughput` | Unidirectional send + recv | 1KB | > 2 GB/s |
| `bridge_throughput` | Unidirectional send + recv | 4KB | > 4 GB/s |

## Baseline Comparisons

Synapse shared memory is compared against:

| Transport | Roundtrip 64B | Roundtrip 1KB | Notes |
|-----------|--------------|---------------|-------|
| **Synapse (shm)** | ~200 ns | ~400 ns | Lock-free SPSC ring buffer |
| **Unix domain socket** | ~2 µs | ~4 µs | Kernel copy, context switch |
| **TCP loopback** | ~10 µs | ~15 µs | Full TCP stack overhead |

Synapse achieves **10-50x lower latency** than socket-based IPC.

## RTT Latency Histogram

The `rtt_histogram_64B` benchmark prints percentile latencies:

```
--- RTT Latency Histogram ---
  P50:      142 ns
  P90:      198 ns
  P99:      387 ns
  P999:    1240 ns
  Max:     4821 ns
```

## Output

Criterion results are stored in `core/target/criterion/` with HTML reports.
Custom histogram output is printed to stderr during benchmark runs.
