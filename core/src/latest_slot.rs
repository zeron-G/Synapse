//! Seqlock-based latest-value slots for lock-free async data sharing.
//!
//! `LatestSlot<T>` provides a single-writer, multi-reader slot where the writer
//! can update without blocking and readers always get the latest consistent value.
//!
//! Uses a sequence counter for lock-free consistency:
//! - Even sequence = stable (safe to read)
//! - Odd sequence = write in progress (reader must retry)
//!
//! Designed for AI Agent async inference results where the latest value matters
//! more than every intermediate value.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

/// Header for a latest-value slot in shared memory.
///
/// Layout:
///   [0..8)    sequence counter (AtomicU64)
///   [8..16)   data_size (u64, size of T in bytes)
///   [16..)    data payload
const SLOT_HEADER_SIZE: usize = 16;

/// Maximum number of read retries before giving up.
const MAX_READ_RETRIES: usize = 1000;

/// A seqlock-based latest-value slot.
///
/// The writer updates the value atomically (from the reader's perspective)
/// using a sequence counter protocol. Readers spin-retry if they observe
/// a write in progress.
pub struct LatestSlot<T: Copy> {
    base: *mut u8,
    _marker: PhantomData<T>,
}

unsafe impl<T: Copy> Send for LatestSlot<T> {}
unsafe impl<T: Copy> Sync for LatestSlot<T> {}

impl<T: Copy> LatestSlot<T> {
    /// Initialize a latest-value slot at the given pointer.
    ///
    /// # Safety
    /// `base` must point to at least `Self::required_size()` bytes of writable, zeroed memory.
    pub unsafe fn init(base: *mut u8) {
        let seq = &*(base as *const AtomicU64);
        seq.store(0, Ordering::Release);
        let data_size_ptr = base.add(8) as *mut u64;
        *data_size_ptr = std::mem::size_of::<T>() as u64;
    }

    /// Create a LatestSlot view over existing shared memory.
    ///
    /// # Safety
    /// `base` must point to a previously initialized LatestSlot region.
    pub unsafe fn from_ptr(base: *mut u8) -> Self {
        Self {
            base,
            _marker: PhantomData,
        }
    }

    /// Total bytes needed for this slot (header + sizeof(T)).
    pub fn required_size() -> usize {
        SLOT_HEADER_SIZE + std::mem::size_of::<T>()
    }

    fn seq_atomic(&self) -> &AtomicU64 {
        unsafe { &*(self.base as *const AtomicU64) }
    }

    fn data_ptr(&self) -> *mut u8 {
        unsafe { self.base.add(SLOT_HEADER_SIZE) }
    }

    /// Write a new value to the slot.
    ///
    /// This is a single-writer operation — only one thread/process should call `write`.
    pub fn write(&self, value: &T) {
        let seq = self.seq_atomic();

        // Step 1: Increment to odd (signals write-in-progress)
        let current = seq.load(Ordering::Relaxed);
        seq.store(current.wrapping_add(1), Ordering::Release);

        // Step 2: Write the data
        // Compiler fence to prevent reordering of data write before sequence increment
        std::sync::atomic::fence(Ordering::AcqRel);
        unsafe {
            std::ptr::copy_nonoverlapping(
                value as *const T as *const u8,
                self.data_ptr(),
                std::mem::size_of::<T>(),
            );
        }

        // Step 3: Increment to even (signals write complete)
        std::sync::atomic::fence(Ordering::AcqRel);
        seq.store(current.wrapping_add(2), Ordering::Release);
    }

    /// Read the latest value from the slot.
    ///
    /// Returns `None` if no value has been written yet (sequence == 0) or if
    /// the read could not be completed consistently within MAX_READ_RETRIES.
    pub fn read(&self) -> Option<T> {
        let seq = self.seq_atomic();

        for _ in 0..MAX_READ_RETRIES {
            let s1 = seq.load(Ordering::Acquire);

            // No value written yet
            if s1 == 0 {
                return None;
            }

            // Write in progress (odd sequence) — retry
            if s1 & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }

            // Read the data
            std::sync::atomic::fence(Ordering::Acquire);
            let value = unsafe {
                let mut val = std::mem::MaybeUninit::<T>::uninit();
                std::ptr::copy_nonoverlapping(
                    self.data_ptr(),
                    val.as_mut_ptr() as *mut u8,
                    std::mem::size_of::<T>(),
                );
                val.assume_init()
            };

            // Verify sequence hasn't changed (no concurrent write)
            std::sync::atomic::fence(Ordering::Acquire);
            let s2 = seq.load(Ordering::Acquire);
            if s1 == s2 {
                return Some(value);
            }

            // Sequence changed — a write happened during our read, retry
            std::hint::spin_loop();
        }

        None // Could not get consistent read
    }

    /// Get the current sequence number.
    pub fn sequence(&self) -> u64 {
        self.seq_atomic().load(Ordering::Acquire)
    }

    /// Check if a value has been written (sequence > 0 and even).
    pub fn has_value(&self) -> bool {
        let s = self.sequence();
        s > 0 && s & 1 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct TestData {
        x: f64,
        y: f64,
        z: f64,
        id: u64,
    }

    fn alloc_slot<T: Copy>() -> (Vec<u8>, LatestSlot<T>) {
        let size = LatestSlot::<T>::required_size();
        let mut mem = vec![0u8; size];
        unsafe {
            LatestSlot::<T>::init(mem.as_mut_ptr());
            let slot = LatestSlot::<T>::from_ptr(mem.as_mut_ptr());
            (mem, slot)
        }
    }

    #[test]
    fn test_write_read_basic() {
        let (_mem, slot) = alloc_slot::<TestData>();

        // No value yet
        assert!(slot.read().is_none());
        assert!(!slot.has_value());

        let data = TestData {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            id: 42,
        };
        slot.write(&data);

        assert!(slot.has_value());
        let got = slot.read().unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn test_overwrite_gets_latest() {
        let (_mem, slot) = alloc_slot::<TestData>();

        for i in 0..10u64 {
            let data = TestData {
                x: i as f64,
                y: 0.0,
                z: 0.0,
                id: i,
            };
            slot.write(&data);
        }

        let got = slot.read().unwrap();
        assert_eq!(got.id, 9);
        assert_eq!(got.x, 9.0);
    }

    #[test]
    fn test_sequence_increments() {
        let (_mem, slot) = alloc_slot::<u64>();

        assert_eq!(slot.sequence(), 0);

        slot.write(&100u64);
        assert_eq!(slot.sequence(), 2);

        slot.write(&200u64);
        assert_eq!(slot.sequence(), 4);

        slot.write(&300u64);
        assert_eq!(slot.sequence(), 6);
    }

    #[test]
    fn test_concurrent_read_write() {
        // Allocate shared memory (using a pinned allocation to ensure stable address)
        let size = LatestSlot::<TestData>::required_size();
        let mem = Arc::new(vec![0u8; size]);

        // Initialize
        unsafe {
            LatestSlot::<TestData>::init(mem.as_ptr() as *mut u8);
        }

        let mem_writer = Arc::clone(&mem);
        let mem_reader = Arc::clone(&mem);

        let iterations = 10_000u64;

        let writer = thread::spawn(move || {
            let slot = unsafe { LatestSlot::<TestData>::from_ptr(mem_writer.as_ptr() as *mut u8) };
            for i in 0..iterations {
                let data = TestData {
                    x: i as f64,
                    y: i as f64 * 2.0,
                    z: i as f64 * 3.0,
                    id: i,
                };
                slot.write(&data);
            }
        });

        let reader = thread::spawn(move || {
            let slot = unsafe { LatestSlot::<TestData>::from_ptr(mem_reader.as_ptr() as *mut u8) };
            // Wait until the writer has published at least one value before starting
            // to read. Without this, the reader may exhaust its loop before the writer
            // has written anything, causing a spurious assertion failure on slow CI.
            while !slot.has_value() {
                std::hint::spin_loop();
            }
            let mut last_id = 0u64;
            let mut reads = 0u64;

            for _ in 0..iterations * 2 {
                if let Some(data) = slot.read() {
                    // Values must be self-consistent
                    assert_eq!(data.y, data.x * 2.0, "inconsistent y for id={}", data.id);
                    assert_eq!(data.z, data.x * 3.0, "inconsistent z for id={}", data.id);
                    assert_eq!(data.x, data.id as f64, "x != id for id={}", data.id);

                    // Monotonically increasing (we may skip values but never go backward)
                    assert!(
                        data.id >= last_id,
                        "went backward: {} < {}",
                        data.id,
                        last_id
                    );
                    last_id = data.id;
                    reads += 1;
                }
                std::hint::spin_loop();
            }

            assert!(reads > 0, "reader should have gotten at least one value");
            reads
        });

        writer.join().unwrap();
        let reads = reader.join().unwrap();
        assert!(reads > 0);
    }

    #[test]
    fn test_consistency_under_contention() {
        // Multiple readers, one writer — all readers must see consistent data
        let size = LatestSlot::<TestData>::required_size();
        let mem = Arc::new(vec![0u8; size]);

        unsafe {
            LatestSlot::<TestData>::init(mem.as_ptr() as *mut u8);
        }

        let iterations = 5_000u64;
        let num_readers = 4;

        let mem_writer = Arc::clone(&mem);
        let writer = thread::spawn(move || {
            let slot = unsafe { LatestSlot::<TestData>::from_ptr(mem_writer.as_ptr() as *mut u8) };
            for i in 0..iterations {
                slot.write(&TestData {
                    x: i as f64,
                    y: i as f64 * 10.0,
                    z: i as f64 * 100.0,
                    id: i,
                });
            }
        });

        let readers: Vec<_> = (0..num_readers)
            .map(|_| {
                let mem_r = Arc::clone(&mem);
                thread::spawn(move || {
                    let slot =
                        unsafe { LatestSlot::<TestData>::from_ptr(mem_r.as_ptr() as *mut u8) };
                    // Wait until the writer has published at least one value.
                    // This prevents a race on macOS (and other platforms) where thread
                    // scheduling can allow readers to finish their loop before the writer
                    // has written anything, resulting in 0 successful reads.
                    while !slot.has_value() {
                        std::hint::spin_loop();
                    }
                    let mut consistent_reads = 0u64;
                    for _ in 0..iterations {
                        if let Some(data) = slot.read() {
                            assert_eq!(data.y, data.x * 10.0);
                            assert_eq!(data.z, data.x * 100.0);
                            consistent_reads += 1;
                        }
                    }
                    consistent_reads
                })
            })
            .collect();

        writer.join().unwrap();
        for r in readers {
            let reads = r.join().unwrap();
            assert!(reads > 0);
        }
    }

    #[test]
    fn test_primitive_types() {
        // Test with simple u64
        let (_mem, slot) = alloc_slot::<u64>();
        slot.write(&12345u64);
        assert_eq!(slot.read().unwrap(), 12345u64);

        // Test with f32
        let (_mem, slot) = alloc_slot::<f32>();
        slot.write(&3.14f32);
        assert!((slot.read().unwrap() - 3.14f32).abs() < f32::EPSILON);
    }
}
