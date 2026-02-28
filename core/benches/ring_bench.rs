//! Criterion benchmarks for ring buffer operations and typed channels.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use synapse_core::ring::{Ring, RingHeader};

fn alloc_ring(capacity: u64, slot_size: u64) -> (Vec<u8>, Ring) {
    let size = RingHeader::region_size(capacity, slot_size);
    let mut region = vec![0u8; size];
    unsafe {
        RingHeader::init(region.as_mut_ptr(), capacity, slot_size);
        let ring = Ring::from_ptr(region.as_mut_ptr());
        (region, ring)
    }
}

fn bench_ring_push_pop(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_push_pop");

    for &payload_size in &[64usize, 128, 252] {
        let slot_size = 256u64;
        let (_mem, ring) = alloc_ring(1024, slot_size);
        let data = vec![0xABu8; payload_size];
        let mut buf = vec![0u8; slot_size as usize];

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("push_pop", payload_size),
            &payload_size,
            |b, _| {
                b.iter(|| {
                    ring.try_push(black_box(&data)).unwrap();
                    ring.try_pop(black_box(&mut buf)).unwrap();
                });
            },
        );
    }

    group.finish();
}

fn bench_ring_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_throughput");

    for &payload_size in &[64usize, 1024, 4096] {
        let slot_size = ((payload_size + 4 + 7) & !7) as u64;
        let capacity = 1024u64;
        let (_mem, ring) = alloc_ring(capacity, slot_size);
        let data = vec![0xCDu8; payload_size];
        let mut buf = vec![0u8; slot_size as usize];

        group.throughput(Throughput::Bytes(payload_size as u64));
        group.bench_with_input(
            BenchmarkId::new("throughput", format!("{}B", payload_size)),
            &payload_size,
            |b, _| {
                b.iter(|| {
                    ring.try_push(black_box(&data)).unwrap();
                    ring.try_pop(black_box(&mut buf)).unwrap();
                });
            },
        );
    }

    group.finish();
}

fn bench_ring_burst(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_burst");
    let capacity = 1024u64;
    let slot_size = 256u64;
    let (_mem, ring) = alloc_ring(capacity, slot_size);
    let data = vec![0xEFu8; 64];
    let mut buf = vec![0u8; slot_size as usize];

    // Burst: push N then pop N
    for &burst_size in &[16u64, 64, 256] {
        group.throughput(Throughput::Elements(burst_size));
        group.bench_with_input(
            BenchmarkId::new("burst", burst_size),
            &burst_size,
            |b, &n| {
                b.iter(|| {
                    for _ in 0..n {
                        ring.try_push(black_box(&data)).unwrap();
                    }
                    for _ in 0..n {
                        ring.try_pop(black_box(&mut buf)).unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_lvs_write_read(c: &mut Criterion) {
    use synapse_core::latest_slot::LatestSlot;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TestData {
        x: f64,
        y: f64,
        z: f64,
        id: u64,
    }

    let size = LatestSlot::<TestData>::required_size();
    let mut mem = vec![0u8; size];
    unsafe {
        LatestSlot::<TestData>::init(mem.as_mut_ptr());
    }
    let slot = unsafe { LatestSlot::<TestData>::from_ptr(mem.as_mut_ptr()) };

    let data = TestData {
        x: 1.0,
        y: 2.0,
        z: 3.0,
        id: 42,
    };

    c.bench_function("lvs_write_read", |b| {
        b.iter(|| {
            slot.write(black_box(&data));
            black_box(slot.read())
        });
    });
}

criterion_group!(
    benches,
    bench_ring_push_pop,
    bench_ring_throughput,
    bench_ring_burst,
    bench_lvs_write_read,
);
criterion_main!(benches);
