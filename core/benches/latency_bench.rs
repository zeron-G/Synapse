//! Cross-process RTT latency and throughput benchmarks.
//!
//! Measures round-trip time for host → connector → host message passing,
//! and compares with baseline Unix domain socket and TCP loopback transports.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::time::Instant;

use synapse_core::{connect, host};

fn unique_name(prefix: &str) -> String {
    format!(
        "{}_{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000_000
    )
}

fn cleanup_shm(name: &str) {
    let _ = std::fs::remove_file(format!("/dev/shm/{name}"));
}

fn bench_bridge_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("bridge_roundtrip");

    for &payload_size in &[64usize, 1024] {
        let slot_size = ((payload_size + 4 + 63) & !63) as u64;
        let name = unique_name("bench_rt");
        cleanup_shm(&name);

        let h = synapse_core::host_with_config(&name, 1024, slot_size).unwrap();
        let conn = synapse_core::connect_with_config(&name, 1024, slot_size).unwrap();

        let data = vec![0xABu8; payload_size];
        let reply = vec![0xCDu8; 8];

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("shm", format!("{}B", payload_size)),
            &payload_size,
            |b, _| {
                b.iter(|| {
                    h.send(black_box(&data)).unwrap();
                    let _ = conn.recv().unwrap();
                    conn.send(black_box(&reply)).unwrap();
                    let _ = h.recv().unwrap();
                });
            },
        );

        drop(h);
        drop(conn);
        cleanup_shm(&name);
    }

    group.finish();
}

fn bench_bridge_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("bridge_throughput");

    for &payload_size in &[64usize, 1024, 4096] {
        let slot_size = ((payload_size + 4 + 63) & !63) as u64;
        let name = unique_name("bench_tp");
        cleanup_shm(&name);

        let h = synapse_core::host_with_config(&name, 1024, slot_size).unwrap();
        let conn = synapse_core::connect_with_config(&name, 1024, slot_size).unwrap();

        let data = vec![0xABu8; payload_size];

        group.throughput(Throughput::Bytes(payload_size as u64));
        group.bench_with_input(
            BenchmarkId::new("unidirectional", format!("{}B", payload_size)),
            &payload_size,
            |b, _| {
                b.iter(|| {
                    h.send(black_box(&data)).unwrap();
                    black_box(conn.recv().unwrap());
                });
            },
        );

        drop(h);
        drop(conn);
        cleanup_shm(&name);
    }

    group.finish();
}

/// Baseline: Unix domain socket round-trip
#[cfg(unix)]
fn bench_uds_roundtrip(c: &mut Criterion) {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let mut group = c.benchmark_group("baseline_uds");

    for &payload_size in &[64usize, 1024] {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.set_nonblocking(false).unwrap();
        b.set_nonblocking(false).unwrap();

        let data = vec![0xABu8; payload_size];
        let mut recv_buf = vec![0u8; payload_size];

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("roundtrip", format!("{}B", payload_size)),
            &payload_size,
            |bench, _| {
                bench.iter(|| {
                    a.write_all(black_box(&data)).unwrap();
                    b.read_exact(&mut recv_buf).unwrap();
                    b.write_all(&recv_buf[..8]).unwrap();
                    a.read_exact(&mut recv_buf[..8]).unwrap();
                });
            },
        );
    }

    group.finish();
}

/// Baseline: TCP loopback round-trip
fn bench_tcp_roundtrip(c: &mut Criterion) {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    let mut group = c.benchmark_group("baseline_tcp");

    for &payload_size in &[64usize, 1024] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        let (mut server, _) = listener.accept().unwrap();

        client.set_nodelay(true).unwrap();
        server.set_nodelay(true).unwrap();

        let data = vec![0xABu8; payload_size];
        let mut recv_buf = vec![0u8; payload_size];

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("roundtrip", format!("{}B", payload_size)),
            &payload_size,
            |bench, _| {
                bench.iter(|| {
                    client.write_all(black_box(&data)).unwrap();
                    server.read_exact(&mut recv_buf).unwrap();
                    server.write_all(&recv_buf[..8]).unwrap();
                    client.read_exact(&mut recv_buf[..8]).unwrap();
                });
            },
        );
    }

    group.finish();
}

/// Standalone RTT histogram — run outside Criterion for percentile output.
fn rtt_histogram(c: &mut Criterion) {
    let name = unique_name("bench_hist");
    cleanup_shm(&name);

    let h = host(&name).unwrap();
    let conn = connect(&name).unwrap();

    let iterations = 10_000usize;
    let payload = vec![0u8; 64];
    let reply = vec![1u8; 8];

    // Warmup
    for _ in 0..100 {
        h.send(&payload).unwrap();
        let _ = conn.recv().unwrap();
        conn.send(&reply).unwrap();
        let _ = h.recv().unwrap();
    }

    let mut latencies = Vec::with_capacity(iterations);

    c.bench_function("rtt_histogram_64B", |b| {
        b.iter(|| {
            let start = Instant::now();
            h.send(black_box(&payload)).unwrap();
            let _ = conn.recv().unwrap();
            conn.send(black_box(&reply)).unwrap();
            let _ = h.recv().unwrap();
            let elapsed = start.elapsed();
            latencies.push(elapsed.as_nanos() as u64);
        });
    });

    // Print percentile summary
    if latencies.len() > 100 {
        latencies.sort();
        let p = |pct: f64| -> u64 {
            let idx = ((pct / 100.0) * latencies.len() as f64) as usize;
            latencies[idx.min(latencies.len() - 1)]
        };
        eprintln!(
            "\n--- RTT Latency Histogram ({} samples) ---",
            latencies.len()
        );
        eprintln!("  P50:  {:>8} ns", p(50.0));
        eprintln!("  P90:  {:>8} ns", p(90.0));
        eprintln!("  P99:  {:>8} ns", p(99.0));
        eprintln!("  P999: {:>8} ns", p(99.9));
        eprintln!("  Max:  {:>8} ns", latencies.last().unwrap());
        eprintln!("-------------------------------------------\n");
    }

    drop(h);
    drop(conn);
    cleanup_shm(&name);
}

#[cfg(unix)]
criterion_group!(
    benches,
    bench_bridge_roundtrip,
    bench_bridge_throughput,
    bench_uds_roundtrip,
    bench_tcp_roundtrip,
    rtt_histogram,
);

#[cfg(not(unix))]
criterion_group!(
    benches,
    bench_bridge_roundtrip,
    bench_bridge_throughput,
    bench_tcp_roundtrip,
    rtt_histogram,
);

criterion_main!(benches);
