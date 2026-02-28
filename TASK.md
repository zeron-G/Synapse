You are working on the Synapse project - a cross-language runtime bridge via shared memory + lock-free ring buffers.

Phase 2 continues. subtask_01-05 are DONE. Now implement these 4 remaining tasks:

## 1. Benchmark Suite (subtask_06)
- Add Criterion benchmarks in benchmarks/ directory
- Measure cross-process RTT latency (round-trip time histogram)
- Measure throughput (messages/sec at various payload sizes: 64B, 1KB, 64KB, 1MB)
- Compare with baseline: Unix domain socket, TCP loopback
- Add `cargo bench` integration, output results to benchmarks/results/
- Include a markdown summary table in benchmarks/README.md

## 2. Graceful Shutdown Protocol (subtask_07)
- Implement heartbeat watchdog to detect peer death (missed N heartbeats -> peer_dead)
- Graceful shutdown sequence: signal intent -> drain buffers -> cleanup shm
- Resource cleanup: unmap shm, close file descriptors, remove shm files
- Error recovery: detect corrupted state, reset to known-good state
- Add ShutdownProtocol and Watchdog structs in core/src/
- Tests: graceful shutdown, peer death detection, recovery after crash

## 3. Full Pipeline Integration Tests (subtask_08)
- End-to-end tests: Python sender -> Rust bridge -> C++ receiver (and reverse)
- Test all new Phase 2 features together: TypedChannels + LatestSlots + AdaptiveWait + Shutdown
- Update CI workflow to run full integration suite
- Update test-local.sh with all new test steps
- Stress test: concurrent channels, multiple readers/writers, sustained throughput

## 4. Documentation (subtask_09)
- Update README.md with Phase 2 API overview and examples
- Update DESIGN.md with new architecture (channels, slots, wait strategies, shutdown)
- Create/update PHASE2.md marking all features complete with status table
- Generate API docs with cargo doc --no-deps
- Update CHANGELOG.md with all Phase 2 changes
- Add usage examples for each new feature

Rules:
- All Rust tests must pass on both Linux and Windows (use cfg for platform-specific code)
- Use cargo fmt after writing code
- Use cargo clippy -- -D warnings to check
- Run all tests after writing to verify they pass
- Make separate commits for each subtask with descriptive messages

When completely finished with ALL FOUR, run:
openclaw system event --text "Done: subtask_06-09 complete - Benchmark Suite, Graceful Shutdown, Integration Tests, Documentation. TASK-001 COMPLETE." --mode now
